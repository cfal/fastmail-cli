//! CLI transport configuration. Server credentials never substitute for Fastmail credentials.

use crate::error::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use percent_encoding::percent_decode_str;
use reqwest::{Client, RequestBuilder};
use std::cell::Cell;
use url::{SyntaxViolation, Url};

tokio::task_local! {
    static SERVER: Option<HttpServer>;
}

/// HTTP proxy client with a runtime-bound connection pool.
///
/// Keep a client and all its clones on the Tokio runtime where it was constructed.
/// A client constructed outside a runtime must be used on only one runtime.
/// For multiple runtimes, construct separate clients inside each runtime.
#[derive(Clone)]
pub struct HttpServer {
    base: Url,
    authorization: Option<http::HeaderValue>,
    client: Client,
}

impl HttpServer {
    /// Use each client within one Tokio runtime.
    /// Basic credentials may be supplied in the URL or as a separate pair, not both.
    /// URL credentials are percent-decoded and removed from all request URLs.
    pub fn new(url: &str, username: Option<&str>, password: Option<&str>) -> Result<Self> {
        // URL parsing strips raw tabs/newlines and trims leading/trailing controls.
        if url.chars().any(char::is_control) {
            return Err(Error::Config(
                "Server URL must not contain control characters".into(),
            ));
        }
        let invalid =
            || Error::Config("Server must be an HTTP(S) URL without query or fragment".into());
        // Parsing normalizes empty userinfo away, including both `@` and `:@`.
        let has_url_login = Cell::new(false);
        let mut base = Url::options()
            .syntax_violation_callback(Some(&|violation| {
                if violation == SyntaxViolation::EmbeddedCredentials {
                    has_url_login.set(true);
                }
            }))
            .parse(url)
            .map_err(|_| invalid())?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(invalid());
        }

        let has_url_login = has_url_login.get();
        let invalid_login = || {
            Error::Config(
                if has_url_login {
                    "Invalid HTTP login in server URL"
                } else {
                    "Set --server-user and FASTMAIL_SERVER_PASSWORD together"
                }
                .into(),
            )
        };
        let url_login = if has_url_login {
            if username.is_some() || password.is_some() {
                return Err(Error::Config(
                    "Use either server URL credentials or --server-user and FASTMAIL_SERVER_PASSWORD, not both".into(),
                ));
            }
            let decode = |value: &str| {
                percent_decode_str(value)
                    .decode_utf8()
                    .map(|value| value.into_owned())
                    .map_err(|_| invalid_login())
            };
            Some((
                decode(base.username())?,
                decode(base.password().ok_or_else(invalid_login)?)?,
            ))
        } else {
            None
        };
        base.set_username("").map_err(|_| invalid())?;
        base.set_password(None).map_err(|_| invalid())?;
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let (username, password) = match &url_login {
            Some((user, password)) => (Some(user.as_str()), Some(password.as_str())),
            None => (username, password),
        };
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
            _ => return Err(invalid_login()),
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
    match status.as_u16() {
        401 => return Err(Error::InvalidToken("HTTP server login rejected (401)")),
        429 => return Err(Error::RateLimited),
        _ => {}
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

    #[test]
    fn server_url_credentials_are_decoded_and_removed_from_requests() {
        for (url, clean, user, password) in [
            (
                "http://user:plain-secret@localhost:8080",
                "http://localhost:8080/cli/v1/session",
                "user",
                "plain-secret",
            ),
            (
                "https://alice%40example.com:p%40ss%3Aword%2F%25%2B%3F@example.com/mail%20box",
                "https://example.com/mail%20box/cli/v1/session",
                "alice@example.com",
                "p@ss:word/%+?",
            ),
            (
                "https://user+name:p+ass%2540@example.com/",
                "https://example.com/cli/v1/session",
                "user+name",
                "p+ass%40",
            ),
            (
                "https://user:pa%2541ss@example.com/",
                "https://example.com/cli/v1/session",
                "user",
                "pa%41ss",
            ),
            (
                "https://%C3%BCser:p%C3%A4ss@example.com/",
                "https://example.com/cli/v1/session",
                "\u{fc}ser",
                "p\u{e4}ss",
            ),
        ] {
            let server = HttpServer::new(url, None, None).unwrap();
            assert!(server.base.username().is_empty());
            assert!(server.base.password().is_none());
            let request = server
                .authorize(server.client.get(server.url("session")))
                .build()
                .unwrap();
            assert_eq!(request.url().as_str(), clean);
            let authorization = &request.headers()[http::header::AUTHORIZATION];
            assert_eq!(
                authorization,
                &format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
            );
            assert!(authorization.is_sensitive());
            assert!(!format!("{request:?}").contains(password));
            assert!(!format!("{request:?}").contains(authorization.to_str().unwrap()));
        }
    }

    #[tokio::test]
    async fn transport_errors_do_not_expose_url_credentials() {
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = HttpServer::new(
            &format!(
                "http://private-user:private-password%40@{}",
                listener.local_addr().unwrap()
            ),
            None,
            None,
        )
        .unwrap();
        let disconnect = async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.read_exact(&mut [0]).await.unwrap();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(
                client.contacts::<()>(serde_json::json!({"operation":"list"})),
                disconnect
            )
        })
        .await
        .expect("Request did not fail after the server disconnected");
        let Error::Http(error) = result.unwrap_err() else {
            panic!("Expected a transport error");
        };
        assert_eq!(error.url(), Some(&client.url("contacts")));
        assert!(!format!("{error} {error:?}").contains("private-"));
    }

    #[test]
    fn invalid_url_logins_fail_without_echoing_credentials() {
        for login in [
            "",
            ":",
            "private-secret",
            "private-secret:",
            ":private-secret",
            "user%3Aname:private-secret",
            "user:private-secret%0D%0A",
            "user%00:private-secret",
            "user:private-secret%7F",
            "user:private-secret%FF",
            "%FF:private-secret",
        ] {
            let error = HttpServer::new(&format!("https://{login}@example.com"), None, None)
                .err()
                .unwrap();
            assert!(
                matches!(error, Error::Config(ref message) if message == "Invalid HTTP login in server URL")
            );
            assert!(!format!("{error:?}").contains("private-secret"));
        }
    }

    #[test]
    fn server_urls_reject_raw_controls_for_every_login_source() {
        let assert_rejected = |url: &str, username: Option<&str>, password: Option<&str>| {
            let error = HttpServer::new(url, username, password).err().unwrap();
            assert!(matches!(error, Error::Config(ref message) if message ==
                "Server URL must not contain control characters"));
            assert!(!format!("{error:?}").contains("private-secret"));
        };
        for (login, username, password) in [
            ("", None, None),
            ("", Some("user"), Some("private-secret")),
            ("user:private-secret@", None, None),
        ] {
            for control in ['\0', '\t', '\n', '\r', '\u{7f}', '\u{85}'] {
                for url in [
                    format!("{control}https://{login}example.com"),
                    format!("https://{login}trusted.example{control}.evil.test"),
                    format!("https://{login}example.com/mail{control}box"),
                    format!("https://{login}example.com{control}"),
                ] {
                    assert_rejected(&url, username, password);
                }
            }
        }
        for login in ["user:private-\nsecret", "us\ter:private-secret"] {
            assert_rejected(&format!("https://{login}@example.com"), None, None);
        }
    }

    #[test]
    fn url_and_separate_logins_cannot_be_combined() {
        for (username, password) in [
            (Some("other"), None),
            (None, Some("private-password")),
            (Some("other"), Some("private-password")),
            (Some(""), None),
            (None, Some("")),
            (Some(""), Some("")),
        ] {
            let error = HttpServer::new(
                "https://user:private-secret@example.com",
                username,
                password,
            )
            .err()
            .unwrap();
            assert!(matches!(error, Error::Config(ref message) if message ==
                "Use either server URL credentials or --server-user and FASTMAIL_SERVER_PASSWORD, not both"));
        }
    }

    #[test]
    fn separate_logins_keep_literal_values_and_validation() {
        let server = HttpServer::new(
            "https://example.com",
            Some("user%40name"),
            Some("p%3Ass+word"),
        )
        .unwrap();
        assert_eq!(
            server.authorization.as_ref().unwrap(),
            &format!("Basic {}", STANDARD.encode("user%40name:p%3Ass+word"))
        );
        assert!(server.authorization.as_ref().unwrap().is_sensitive());
        for (user, password) in [
            (Some("user"), None),
            (None, Some("secret")),
            (Some(""), Some("secret")),
            (Some("user"), Some("")),
            (Some("user:name"), Some("secret")),
            (Some("user\n"), Some("secret")),
            (Some("user"), Some("secret\0")),
        ] {
            let error = HttpServer::new("https://example.com", user, password)
                .err()
                .unwrap();
            assert!(matches!(error, Error::Config(ref message) if message ==
                "Set --server-user and FASTMAIL_SERVER_PASSWORD together"));
        }
    }

    #[tokio::test]
    async fn proxy_authentication_and_rate_limits_keep_their_classification() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        for status in [401, 429] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_string("not JSON"))
                .mount(&server)
                .await;
            let client = HttpServer::new(&server.uri(), None, None).unwrap();
            let error = client
                .contacts::<()>(serde_json::json!({"operation":"list"}))
                .await
                .unwrap_err();
            match status {
                401 => assert!(matches!(error, Error::InvalidToken(_)), "{error:?}"),
                429 => assert!(matches!(error, Error::RateLimited), "{error:?}"),
                _ => unreachable!(),
            }
        }
    }

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
            "https://user:secret@example.com:invalid",
            "https://user:secret@example.com/?token=secret",
            "https://example.com/?token=secret",
            "https://example.com/#secret",
        ] {
            let error = HttpServer::new(value, None, None)
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("secret"));
        }
        for path in ["mail", "mail@example.com", "mail%40example.com"] {
            let server =
                HttpServer::new(&format!("https://example.com/{path}"), None, None).unwrap();
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
                format!("https://example.com/{path}/cli/v1/session")
            );
        }
        assert!(HttpServer::new("http://localhost", Some("user"), None).is_err());
    }
}
