use std::collections::HashMap;
use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, bail};
use axum::{
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use http::{HeaderMap, StatusCode, header, uri::Authority};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub(crate) const MAX_JSON_BODY_BYTES: usize = 2 * 1024 * 1024;

async fn bounded_body(request: Request, limit: usize) -> Result<Request, StatusCode> {
    use std::error::Error as _;
    let (parts, body) = request.into_parts();
    match axum::body::to_bytes(body, limit).await {
        Ok(bytes) => Ok(Request::from_parts(parts, axum::body::Body::from(bytes))),
        Err(error) => {
            let status = if error
                .source()
                .is_some_and(|e| e.is::<http_body_util::LengthLimitError>())
            {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            };
            Err(status)
        }
    }
}

pub(super) async fn limit_mcp_body(request: Request, next: Next) -> Response {
    // rmcp collects raw bodies and does not use Axum's limited JSON extractor.
    match bounded_body(request, MAX_JSON_BODY_BYTES).await {
        Ok(request) => next.run(request).await,
        Err(status) => (status, "Cannot read request within the body limit").into_response(),
    }
}

#[derive(Clone, Default)]
pub struct BasicAuth {
    users: HashMap<String, [u8; 32]>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthFile {
    users: HashMap<String, String>,
}

impl BasicAuth {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut text = String::new();
        std::fs::File::open(path)
            .with_context(|| format!("Cannot open auth file {}", path.display()))?
            .take(1024 * 1024 + 1)
            .read_to_string(&mut text)?;
        if text.len() > 1024 * 1024 {
            bail!("Auth file exceeds 1 MiB");
        }
        Self::parse(&text)
    }

    fn parse(text: &str) -> anyhow::Result<Self> {
        let file: AuthFile = toml::from_str(text).map_err(|_| {
            anyhow::anyhow!(
                "Invalid auth file: expected a TOML [users] table of username/password strings"
            )
        })?;
        if file.users.is_empty() {
            bail!("Auth file contains no users");
        }
        let mut users = HashMap::new();
        for (username, password) in file.users {
            if username.is_empty()
                || username.contains(':')
                || username.chars().any(char::is_control)
                || password.is_empty()
                || password.chars().any(char::is_control)
            {
                bail!("Auth file contains an invalid username or password");
            }
            users.insert(username, Sha256::digest(password.as_bytes()).into());
        }
        Ok(Self { users })
    }

    fn accepts(&self, headers: &HeaderMap) -> bool {
        if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
            return false;
        }
        let Some(value) = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
        else {
            return false;
        };
        let Some((scheme, encoded)) = value.split_once(' ') else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("basic") {
            return false;
        }
        let Ok(decoded) = STANDARD.decode(encoded) else {
            return false;
        };
        let Ok(credentials) = std::str::from_utf8(&decoded) else {
            return false;
        };
        let Some((username, password)) = credentials.split_once(':') else {
            return false;
        };
        let digest: [u8; 32] = Sha256::digest(password.as_bytes()).into();
        let expected = self.users.get(username);
        bool::from(digest.ct_eq(expected.unwrap_or(&[0; 32]))) && expected.is_some()
    }
}

#[derive(Clone)]
pub struct HttpSecurity {
    auth: Option<BasicAuth>,
    allowed_hosts: Vec<String>,
}

impl HttpSecurity {
    pub fn new(
        addr: SocketAddr,
        auth: Option<BasicAuth>,
        mut allowed_hosts: Vec<String>,
    ) -> anyhow::Result<Self> {
        if allowed_hosts.is_empty() && addr.ip().is_loopback() {
            allowed_hosts = vec![
                "localhost".into(),
                "127.0.0.1".into(),
                "[::1]".into(),
                if addr.is_ipv6() {
                    format!("[{}]", addr.ip())
                } else {
                    addr.ip().to_string()
                },
            ];
        }
        for host in &mut allowed_hosts {
            let authority: Authority = host.parse().context("Invalid allowed host")?;
            if authority.port().is_some() || authority.as_str().contains('@') {
                bail!("Allowed hosts must be hostnames without ports");
            }
            *host = authority.host().to_ascii_lowercase();
        }
        Ok(Self {
            auth,
            allowed_hosts,
        })
    }

    fn check_browser(&self, headers: &HeaderMap) -> bool {
        if headers.get_all(header::HOST).iter().count() != 1 {
            return false;
        }
        let Some(host) = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.parse::<Authority>().ok())
        else {
            return false;
        };
        if host.as_str().contains('@') {
            return false;
        }
        if !self.allowed_hosts.is_empty()
            && !self
                .allowed_hosts
                .iter()
                .any(|h| h.eq_ignore_ascii_case(host.host()))
        {
            return false;
        }
        if headers
            .get("sec-fetch-site")
            .is_some_and(|v| v == "cross-site")
        {
            return false;
        }
        if headers.get_all(header::ORIGIN).iter().count() > 1 {
            return false;
        }
        if let Some(origin) = headers.get(header::ORIGIN) {
            let Some(origin) = origin
                .to_str()
                .ok()
                .and_then(|v| reqwest::Url::parse(v).ok())
            else {
                return false;
            };
            if !matches!(origin.scheme(), "http" | "https")
                || !origin.username().is_empty()
                || origin.password().is_some()
                || origin.path() != "/"
                || origin.query().is_some()
                || origin.fragment().is_some()
            {
                return false;
            }
            let Ok(expected) = reqwest::Url::parse(&format!("{}://{}", origin.scheme(), host))
            else {
                return false;
            };
            if origin.origin() != expected.origin() {
                return false;
            }
        }
        true
    }
}

pub async fn guard(
    axum::extract::State(policy): axum::extract::State<HttpSecurity>,
    request: Request,
    next: Next,
) -> Response {
    if !policy.check_browser(request.headers()) {
        return (StatusCode::FORBIDDEN, "Host or Origin not allowed").into_response();
    }
    if policy
        .auth
        .as_ref()
        .is_some_and(|auth| !auth.accepts(request.headers()))
    {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                "Basic realm=\"fastmail\", charset=\"UTF-8\"",
            )],
            "Authentication required",
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,
        "default-src 'self'; script-src 'self'; worker-src 'self'; connect-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'".parse().unwrap());
    response
        .headers_mut()
        .insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn body_limit_covers_sized_and_streamed_input() {
        use axum::body::{Body, Bytes};
        let sized = Request::builder()
            .header("Content-Length", "17")
            .body(Body::from("abcdefghijklmnopq"))
            .unwrap();
        assert_eq!(
            bounded_body(sized, 16).await.unwrap_err(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let chunks = async_graphql::futures_util::stream::iter([
            Ok::<_, std::io::Error>(Bytes::from_static(b"abcdefgh")),
            Ok(Bytes::from_static(b"ijklmnop")),
            Ok(Bytes::from_static(b"q")),
        ]);
        let streamed = Request::new(Body::from_stream(chunks));
        assert_eq!(
            bounded_body(streamed, 16).await.unwrap_err(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let accepted = bounded_body(Request::new(Body::from("abcdefghijklmnop")), 16)
            .await
            .unwrap();
        assert_eq!(
            axum::body::to_bytes(accepted.into_body(), 16)
                .await
                .unwrap(),
            "abcdefghijklmnop"
        );
    }

    #[test]
    fn ipv6_loopback_is_an_allowed_host() {
        let policy = HttpSecurity::new("[::1]:8080".parse().unwrap(), None, vec![]).unwrap();
        assert!(policy.check_browser(&headers("[::1]:8080", Some("http://[::1]:8080"))));
        assert!(!policy.check_browser(&headers("[::2]:8080", None)));
    }

    fn headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, host.parse().unwrap());
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, origin.parse().unwrap());
        }
        headers
    }

    #[test]
    fn basic_auth_accepts_only_configured_credentials() {
        let auth = BasicAuth::parse("[users]\nalice = 'a:password'\nbob = 'different'").unwrap();
        for (credentials, accepted) in [
            ("alice:a:password", true),
            ("bob:different", true),
            ("alice:different", false),
            ("unknown:a:password", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::AUTHORIZATION,
                format!("Basic {}", STANDARD.encode(credentials))
                    .parse()
                    .unwrap(),
            );
            assert_eq!(auth.accepts(&headers), accepted);
        }
        assert!(!auth.accepts(&HeaderMap::new()));
        for value in ["Basic ???", "Bearer value", "Basic YWxpY2U="] {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, value.parse().unwrap());
            assert!(!auth.accepts(&headers));
        }
    }

    #[test]
    fn malformed_auth_files_fail_closed_without_echoing_secrets() {
        for input in [
            "[users]",
            "[users]\nalice = ''",
            "users = 'secret'",
            "[users]\nalice='secret' garbage",
            "[users]\nalice='first'\nalice='second'",
        ] {
            let error = BasicAuth::parse(input).err().unwrap().to_string();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn browser_checks_do_not_require_authentication() {
        let local = HttpSecurity::new("127.0.0.1:8080".parse().unwrap(), None, vec![]).unwrap();
        assert!(local.check_browser(&headers("localhost:8080", None)));
        assert!(local.check_browser(&headers("localhost:8080", Some("http://localhost:8080"))));
        assert!(!local.check_browser(&headers("rebind.example:8080", None)));
        assert!(!local.check_browser(&headers("localhost:8080", Some("https://evil.example"))));
        assert!(!local.check_browser(&headers("localhost:8080", Some("http://localhost:9999"))));
        assert!(!local.check_browser(&headers("localhost:8080", Some("null"))));
        let remote = HttpSecurity::new("0.0.0.0:8080".parse().unwrap(), None, vec![]).unwrap();
        assert!(remote.check_browser(&headers("mail.example", Some("https://mail.example"))));
        let restricted = HttpSecurity::new(
            "0.0.0.0:8080".parse().unwrap(),
            None,
            vec!["mail.example".into()],
        )
        .unwrap();
        assert!(!restricted.check_browser(&headers("wrong.example", None)));
    }
}
