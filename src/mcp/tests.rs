//! MCP and HTTP transport contract tests.

use super::*;
use axum::body::{Body, Bytes};
use http::{Request, StatusCode};
use tower::ServiceExt;

fn in_process_router(
    surfaces: HttpSurfaces,
    auth: Option<http_security::BasicAuth>,
) -> axum::Router {
    let policy =
        http_security::HttpSecurity::new("127.0.0.1:8080".parse().unwrap(), auth, vec![]).unwrap();
    http_router(FastmailMcp::build(None), surfaces, policy).unwrap()
}

fn request(method: &str, path: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:8080")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn json_routes_limit_declared_and_streamed_bodies_to_two_mib() {
    use async_graphql::futures_util::stream;
    const LIMIT: usize = 2 * 1024 * 1024;
    let router = in_process_router(
        HttpSurfaces {
            graphql: true,
            graphiql: false,
            browser: false,
        },
        None,
    );
    let chunk = Bytes::from(vec![b' '; 8192]);
    for path in [
        "/mcp",
        "/graphql",
        "/graphql/stream",
        "/cli/v1/jmap",
        "/cli/v1/contacts",
    ] {
        for declared in [true, false] {
            let chunks = std::iter::repeat_n(chunk.clone(), LIMIT / chunk.len())
                .map(Ok::<_, std::io::Error>)
                .chain([Ok(Bytes::from_static(b" "))]);
            let mut req = request("POST", path, Body::from_stream(stream::iter(chunks)));
            if declared {
                req.headers_mut()
                    .insert("content-length", (LIMIT + 1).to_string().parse().unwrap());
            }
            let response = router.clone().oneshot(req).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "{path}, declared={declared}"
            );
            assert_eq!(response.headers()["cache-control"], "no-store");
            if path == "/mcp" {
                let body = axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap();
                assert_eq!(body, "Cannot read request within the body limit");
            }
        }
    }

    let broken = stream::iter([
        Ok(Bytes::from_static(b"{")),
        Err(std::io::Error::other("interrupted body")),
    ]);
    let response = router
        .clone()
        .oneshot(request("POST", "/mcp", Body::from_stream(broken)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(body, "Cannot read request within the body limit");

    let initialize = serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
            "protocolVersion":"2025-11-25", "capabilities":{},
            "clientInfo":{"name":"test", "version":"1"}
        }
    });
    let mut body = initialize.to_string().into_bytes();
    body.resize(LIMIT, b' ');
    let response = router
        .oneshot(request("POST", "/mcp", Body::from(body)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("mcp-session-id"));
}

#[tokio::test]
async fn cli_jmap_batch_limits_are_checked_before_authentication() {
    let router = in_process_router(
        HttpSurfaces {
            graphql: false,
            graphiql: false,
            browser: false,
        },
        None,
    );
    for (count, expected) in [
        (
            64,
            "Config error: Configure Fastmail credentials on the server",
        ),
        (65, "Config error: At most 64 JMAP method calls per request"),
    ] {
        let body = serde_json::json!({"methodCalls": vec![serde_json::json!(["Mailbox/get", {}, "0"]); count]});
        let response = router
            .clone()
            .oneshot(request(
                "POST",
                "/cli/v1/jmap",
                Body::from(body.to_string()),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"error":expected})
        );
    }
}

#[tokio::test]
async fn http_policy_rejects_requests_without_polling_their_bodies() {
    use async_graphql::futures_util::stream;
    let mut file = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut file, b"[users]\nalice = 'password'").unwrap();
    let router = in_process_router(
        HttpSurfaces {
            graphql: true,
            graphiql: false,
            browser: false,
        },
        Some(http_security::BasicAuth::load(file.path()).unwrap()),
    );
    for (host, status) in [
        ("localhost:8080", StatusCode::UNAUTHORIZED),
        ("rebind.example", StatusCode::FORBIDDEN),
    ] {
        let unreadable = stream::poll_fn(
            |_| -> std::task::Poll<Option<Result<Bytes, std::io::Error>>> {
                panic!("Rejected requests must not poll their bodies")
            },
        );
        let mut req = request("POST", "/mcp", Body::from_stream(unreadable));
        req.headers_mut().insert("host", host.parse().unwrap());
        assert_eq!(router.clone().oneshot(req).await.unwrap().status(), status);
    }
}

#[tokio::test]
async fn optional_http_surfaces_and_security_headers_match_configuration() {
    for (graphql, graphiql) in [(false, false), (true, false), (false, true)] {
        let router = in_process_router(
            HttpSurfaces {
                graphql,
                graphiql,
                browser: false,
            },
            None,
        );
        for (method, path, enabled) in [
            ("POST", "/graphql", graphql || graphiql),
            ("GET", "/", graphiql),
            ("GET", "/assets/main.js", graphiql),
            ("GET", "/assets/missing.js", false),
        ] {
            let response = router
                .clone()
                .oneshot(request(
                    method,
                    path,
                    Body::from(r#"{"query":"{ __typename }"}"#),
                ))
                .await
                .unwrap();
            let expected = if enabled {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            };
            assert_eq!(
                response.status(),
                expected,
                "graphql={graphql}, graphiql={graphiql}, {path}"
            );
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert_eq!(response.headers()["referrer-policy"], "no-referrer");
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("frame-ancestors 'none'")
            );
            if enabled && path == "/assets/main.js" {
                assert_eq!(response.headers()["content-encoding"], "gzip");
                assert_eq!(response.headers()["content-type"], "text/javascript");
            }
        }
    }
}

#[tokio::test]
async fn mcp_graphql_variables_require_an_object_and_reach_the_schema() {
    let mcp = FastmailMcp::build(Some("test-token".into()));
    for (variables, message) in [
        ("[]", "Variables must be a JSON object"),
        ("null", "Variables must be a JSON object"),
        ("\"text\"", "Variables must be a JSON object"),
        ("{", "Invalid variables JSON:"),
    ] {
        let result = mcp
            .graphql(Parameters(GraphqlRequest {
                query: "{ __typename }".into(),
                variables: Some(variables.into()),
            }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        assert!(text_of(result).starts_with(message));
    }
    let result = mcp
        .graphql(Parameters(GraphqlRequest {
            query: "query($name: String!) { __type(name: $name) { name } }".into(),
            variables: Some(r#"{"name":"QueryRoot"}"#.into()),
        }))
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&text_of(result)).unwrap();
    assert_eq!(response["data"]["__type"]["name"], "QueryRoot");
    assert!(response.get("errors").is_none());
}

#[tokio::test]
async fn graphql_transports_check_syntax_before_authentication() {
    let mcp = FastmailMcp::build(None);
    let query = "[".repeat(33);
    let error = build_http_request(
        &mcp,
        HttpGraphqlRequest {
            query: query.clone(),
            variables: None,
            operation_name: None,
        },
    )
    .await
    .err()
    .unwrap();
    assert!(error.contains("syntax nesting"));
    let result = mcp
        .graphql(Parameters(GraphqlRequest {
            query,
            variables: None,
        }))
        .await
        .unwrap();
    assert!(text_of(result).contains("syntax nesting"));
}

#[tokio::test]
async fn http_auth_and_browser_policy_cover_every_surface() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut file, b"[users]\nalice = 'password'\nbob = 'second'").unwrap();
    let auth = http_security::BasicAuth::load(file.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let policy = http_security::HttpSecurity::new(
        format!("127.0.0.1:{port}").parse().unwrap(),
        Some(auth),
        vec![],
    )
    .unwrap();
    let router = http_router(
        FastmailMcp::build(None),
        HttpSurfaces {
            graphql: true,
            graphiql: true,
            browser: false,
        },
        policy,
    )
    .unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    for path in [
        "/",
        "/mcp",
        "/graphql",
        "/graphql/stream",
        "/assets/main.js",
        "/cli/v1/session",
        "/cli/v1/jmap",
        "/cli/v1/upload",
        "/cli/v1/download",
        "/cli/v1/contacts",
        "/cli/v1/events",
    ] {
        let response = client.post(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.status(), http::StatusCode::UNAUTHORIZED, "{path}");
        assert!(
            response
                .headers()
                .contains_key(http::header::WWW_AUTHENTICATE)
        );
    }
    for (user, password, status) in [
        ("alice", "password", 200),
        ("bob", "second", 200),
        ("alice", "second", 401),
    ] {
        let response = client
            .post(format!("{base}/graphql"))
            .basic_auth(user, Some(password))
            .json(&serde_json::json!({"query": "{ __typename }"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
    }
    for (name, value) in [
        ("host", "rebind.example"),
        ("origin", "https://evil.example"),
    ] {
        let response = client
            .post(format!("{base}/graphql"))
            .basic_auth("alice", Some("password"))
            .header(name, value)
            .json(&serde_json::json!({"query": "{ __typename }"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::FORBIDDEN);
    }
    let response = client
        .post(format!("{base}/graphql"))
        .basic_auth("alice", Some("password"))
        .header("x-fastmail-token", "must-not-be-used")
        .json(&serde_json::json!({"query": "{ session { status } }"}))
        .send()
        .await
        .unwrap();
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("No Fastmail token available")
    );
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn cli_http_roundtrip_keeps_fastmail_credentials_on_server() {
    use serde_json::{Value, json};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let upstream = MockServer::start().await;
    let origin = upstream.uri();
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "capabilities":{"urn:ietf:params:jmap:core":{},"urn:ietf:params:jmap:mail":{}},
            "accounts":{}, "primaryAccounts":{"urn:ietf:params:jmap:mail":"acct1"},
            "username":"server@example.com", "apiUrl":format!("{origin}/jmap"),
            "downloadUrl":format!("{origin}/download?blob={{blobId}}"),
            "uploadUrl":format!("{origin}/upload"),
            "eventSourceUrl":format!("{origin}/events?ping={{ping}}")
        })))
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/jmap"))
        .respond_with(|req: &wiremock::Request| {
            let value: Value = serde_json::from_slice(&req.body).unwrap();
            ResponseTemplate::new(200)
                .set_body_json(json!({"methodResponses":value["methodCalls"]}))
        })
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path("/download"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"attachment"))
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"blobId":"uploaded"})))
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path("/events"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("id: event2\ndata: {}\n\n", "text/event-stream"),
        )
        .mount(&upstream)
        .await;
    let mcp = FastmailMcp::build(Some("server-only".into()));
    let local = JmapClient::with_test_session(&format!("{origin}/jmap"));
    mcp.clients
        .lock()
        .await
        .insert("server-only".into(), Arc::new(Mutex::new(local)));
    let mut file = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut file, b"[users]\ncli = 'login'").unwrap();
    let auth = http_security::BasicAuth::load(file.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let policy = http_security::HttpSecurity::new(addr, Some(auth), vec![]).unwrap();
    let router = http_router(
        mcp,
        HttpSurfaces {
            graphql: false,
            graphiql: false,
            browser: false,
        },
        policy,
    )
    .unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let remote = crate::remote::HttpServer::new(
        &format!("http://127.0.0.1:{}", addr.port()),
        Some("cli"),
        Some("login"),
    )
    .unwrap();
    crate::remote::HttpServer::scope(Some(remote), async {
        let client = crate::jmap::authenticated_client().await.unwrap();
        assert_eq!(client.session().unwrap().username, "server@example.com");
        let calls = vec![json!(["Mailbox/get", {"accountId":"acct1"}, "0"])];
        assert_eq!(client.request(calls.clone()).await.unwrap(), calls);
        assert_eq!(
            client.download_blob("blob/a?b&c").await.unwrap(),
            b"attachment"
        );
        assert_eq!(
            client
                .upload_blob(b"upload".to_vec(), "text/plain")
                .await
                .unwrap(),
            "uploaded"
        );
        let response = client.open_event_stream(30, Some("event1")).await.unwrap();
        assert!(response.text().await.unwrap().contains("id: event2"));
    })
    .await;
    for request in upstream.received_requests().await.unwrap() {
        assert_eq!(request.headers["authorization"], "Bearer test-token");
        assert!(!request.headers.contains_key("x-fastmail-token"));
        if request.url.path() == "/download" {
            assert_eq!(
                request.url.query_pairs().collect::<Vec<_>>(),
                vec![("blob".into(), "blob/a?b&c".into())]
            );
        }
        if request.url.path() == "/events" {
            assert_eq!(request.headers["last-event-id"], "event1");
        }
        if request.url.path() == "/upload" {
            assert_eq!(request.body, b"upload");
        }
    }
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn http_auth_is_optional_even_on_a_non_loopback_listener() {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let policy = http_security::HttpSecurity::new(addr, None, vec![]).unwrap();
    let router = http_router(
        FastmailMcp::build(None),
        HttpSurfaces {
            graphql: true,
            graphiql: false,
            browser: false,
        },
        policy,
    )
    .unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/graphql", addr.port()))
        .json(&serde_json::json!({"query": "{ __typename }"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    assert!(response.text().await.unwrap().contains("QueryRoot"));
    task.abort();
    let _ = task.await;
}

fn text_of(result: CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect()
}

#[tokio::test]
async fn schema_sdl_absent_null_and_empty_types_return_the_full_schema() {
    let mcp = FastmailMcp::build(None);
    let full = mcp.schema.sdl();
    for input in ["{}", r#"{"types":null}"#, r#"{"types":[]}"#] {
        let request: SchemaRequest = serde_json::from_str(input).unwrap();
        let sdl = text_of(mcp.schema_sdl(Parameters(request)).await.unwrap());
        assert_eq!(sdl, full, "{input}");
    }
}

#[tokio::test]
async fn schema_sdl_with_types_returns_only_those() {
    let mcp = FastmailMcp::http();
    let sliced = text_of(
        mcp.schema_sdl(Parameters(SchemaRequest {
            types: Some(vec!["Session".into()]),
        }))
        .await
        .unwrap(),
    );

    assert!(sliced.contains("type Session {"));
    assert!(!sliced.contains("input EmailFilter {"));

    let full = text_of(
        mcp.schema_sdl(Parameters(SchemaRequest::default()))
            .await
            .unwrap(),
    );
    assert!(
        sliced.len() * 10 < full.len(),
        "{} of {} is not a saving worth the argument",
        sliced.len(),
        full.len()
    );
}

#[test]
fn introspection_needs_no_token() {
    // What GraphiQL sends on load, plus the shapes around it.
    assert!(is_introspection_only("{ __schema { queryType { name } } }"));
    assert!(is_introspection_only(
        "query IntrospectionQuery { __schema { types { name } } }"
    ));
    assert!(is_introspection_only(
        "{ __type(name: \"Email\") { name } }"
    ));
    assert!(is_introspection_only("{ __typename }"));
    assert!(is_introspection_only(
        "{ ...F } fragment F on QueryRoot { __typename }"
    ));
}

#[test]
fn real_fields_still_need_a_token() {
    assert!(!is_introspection_only("{ mailboxes { name } }"));
    // Mixed with introspection, and nested below it, still count as real.
    assert!(!is_introspection_only("{ __typename mailboxes { name } }"));
    assert!(!is_introspection_only(
        "mutation { sendEmail(action: PREVIEW) { preview } }"
    ));
    // Classify fragment contents as well as direct selections.
    assert!(!is_introspection_only(
        "{ ...F } fragment F on QueryRoot { mailboxes { name } }"
    ));
    assert!(!is_introspection_only("{ this is not graphql"));
}

#[tokio::test]
async fn cold_health_failures_are_structured_through_http_and_mcp() {
    use serde_json::json;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
    for (status, expected) in [
        (401, "INVALID_CREDENTIALS"),
        (503, "UNREACHABLE"),
        (0, "UNREACHABLE"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status.max(400)))
            .mount(&server)
            .await;
        let endpoint = if status == 0 {
            "http://127.0.0.1:1".to_owned()
        } else {
            server.uri()
        };
        for http in [true, false] {
            let mcp = FastmailMcp::build(Some("test-token".into()));
            // Inject only the endpoint, never an authenticated session.
            let client = JmapClient::with_test_session_endpoint(&endpoint);
            assert!(client.session().is_err());
            mcp.clients
                .lock()
                .await
                .insert("test-token".into(), Arc::new(Mutex::new(client)));
            let query =
                "query Health { ...Probe } fragment Probe on QueryRoot { session { status } }";
            let response = if http {
                let result = graphql_endpoint(
                    axum::extract::State(mcp),
                    axum::Json(HttpGraphqlRequest {
                        query: query.into(),
                        variables: None,
                        operation_name: Some("Health".into()),
                    }),
                )
                .await;
                serde_json::to_value(result.0).unwrap()
            } else {
                serde_json::from_str(&text_of(
                    mcp.graphql(Parameters(GraphqlRequest {
                        query: query.into(),
                        variables: None,
                    }))
                    .await
                    .unwrap(),
                ))
                .unwrap()
            };
            assert_eq!(
                response["data"]["session"]["status"],
                json!(expected),
                "{response}"
            );
        }
    }
}

#[tokio::test]
async fn graphql_stream_emits_next_and_complete_protocol_events() {
    let mcp = FastmailMcp::build(Some("test-token".into()));
    let client = JmapClient::with_test_session("http://127.0.0.1:1");
    mcp.clients
        .lock()
        .await
        .insert("test-token".into(), Arc::new(Mutex::new(client)));
    let response = graphql_stream_endpoint(
        axum::extract::State(mcp),
        axum::Json(HttpGraphqlRequest {
            query: "subscription { emails(pollSeconds: 0) { id } }".into(),
            variables: None,
            operation_name: None,
        }),
    )
    .await;
    assert_eq!(
        response.headers()[http::header::CONTENT_TYPE],
        "text/event-stream"
    );
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(text.contains("event: next\n"), "{text}");
    assert!(text.contains("event: complete\n"), "{text}");
    assert!(text.contains("at least one second"));
}

#[test]
fn only_selected_health_queries_defer_authentication() {
    let document = "query Health { session { status } } query Mail { emails { nodes { id } } }";
    assert!(is_local_query(document, true, Some("Health")));
    assert!(!is_local_query(document, true, Some("Mail")));
    assert!(!is_local_query(
        "{ session { status } mailboxes { name } }",
        true,
        None
    ));
}
