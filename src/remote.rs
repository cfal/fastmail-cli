//! CLI transport configuration. Server credentials never substitute for Fastmail credentials.

use crate::error::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, RequestBuilder, Url};

tokio::task_local! {
    static SERVER: Option<HttpServer>;
}

#[derive(Clone)]
pub struct HttpServer {
    base: Url,
    authorization: Option<http::HeaderValue>,
    client: Client,
}

impl HttpServer {
    pub fn new(url: &str, username: Option<&str>, password: Option<&str>) -> Result<Self> {
        let invalid = || {
            Error::Config(
                "Server must be an HTTP(S) URL without credentials, query or fragment".into(),
            )
        };
        let mut base = Url::parse(url).map_err(|_| invalid())?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(invalid());
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let authorization = match (username, password) {
            (None, None) => None,
            (Some(user), Some(password))
                if !user.is_empty()
                    && !password.is_empty()
                    && !user.contains(':')
                    && !user.chars().any(char::is_control)
                    && !password.chars().any(char::is_control) =>
            {
                let mut value = http::HeaderValue::from_str(&format!(
                    "Basic {}",
                    STANDARD.encode(format!("{user}:{password}"))
                ))
                .map_err(|_| Error::Config("Invalid HTTP login".into()))?;
                value.set_sensitive(true);
                Some(value)
            }
            _ => {
                return Err(Error::Config(
                    "Set --server-user and FASTMAIL_SERVER_PASSWORD together".into(),
                ));
            }
        };
        Ok(Self {
            base,
            authorization,
            client: crate::util::http_client()?,
        })
    }

    pub async fn scope<F: std::future::Future>(server: Option<Self>, operation: F) -> F::Output {
        SERVER.scope(server, operation).await
    }

    pub(crate) fn current() -> Option<Self> {
        SERVER.try_with(Clone::clone).ok().flatten()
    }

    pub(crate) fn url(&self, endpoint: &str) -> Url {
        self.base
            .join(&format!("cli/v1/{endpoint}"))
            .expect("Static endpoint")
    }

    pub(crate) fn authorize(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.authorization {
            Some(auth) => request.header(http::header::AUTHORIZATION, auth.clone()),
            None => request,
        }
    }

    pub(crate) async fn contacts<T: serde::de::DeserializeOwned>(
        &self,
        request: serde_json::Value,
    ) -> Result<T> {
        let response = self
            .authorize(self.client.post(self.url("contacts")))
            .json(&request)
            .send()
            .await?;
        let response = check_response(response).await?;
        let bytes =
            crate::util::read_bounded_response(response, crate::util::MAX_ATTACHMENT_BYTES).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

pub(crate) async fn check_response(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let fallback = || Error::Server(format!("HTTP server request failed ({status})"));
    let bytes = crate::util::read_bounded_response(response, 16 * 1024)
        .await
        .map_err(|_| fallback())?;
    #[derive(serde::Deserialize)]
    struct ProxyError {
        error: String,
    }
    let error = serde_json::from_slice::<ProxyError>(&bytes).map_err(|_| fallback())?;
    if error.error.is_empty() {
        return Err(fallback());
    }
    Err(Error::Server(format!(
        "HTTP server request failed ({status}): {}",
        error.error
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn carddav_proxy_errors_preserve_details_but_ignore_non_json_bodies() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        for (body, expected) in [
            (
                r#"{"error":"Contact changed concurrently"}"#,
                "Contact changed concurrently",
            ),
            ("not JSON", "HTTP server request failed"),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(409).set_body_string(body))
                .mount(&server)
                .await;
            let client = HttpServer::new(&server.uri(), None, None).unwrap();
            let error = client
                .contacts::<()>(serde_json::json!({"operation":"delete", "id":"contact"}))
                .await
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn server_url_validation_does_not_echo_credentials() {
        for value in [
            "file:///mail",
            "http://user:secret@example.com",
            "https://example.com/?token=secret",
            "https://example.com/#secret",
        ] {
            let error = HttpServer::new(value, None, None)
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("secret"));
        }
        let server = HttpServer::new("https://example.com/mail", None, None).unwrap();
        assert!(
            !server
                .authorize(server.client.get(server.url("session")))
                .build()
                .unwrap()
                .headers()
                .contains_key(http::header::AUTHORIZATION)
        );
        assert_eq!(
            server.url("session").as_str(),
            "https://example.com/mail/cli/v1/session"
        );
        assert!(HttpServer::new("http://localhost", Some("user"), None).is_err());
    }
}
