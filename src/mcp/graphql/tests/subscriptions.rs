use super::*;

#[tokio::test]
async fn subscription_reports_expired_history_and_ends() {
    use async_graphql::futures_util::StreamExt;
    use wiremock::matchers::body_string_contains;
    let server = mock_server(0).await;
    Mock::given(body_string_contains(r#""ids":[]"#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[["Email/get",{"accountId":"acct1","state":"present","list":[],"notFound":[]},"s0"]]})))
        .with_priority(1).expect(2).mount(&server).await;
    Mock::given(body_string_contains("Email/changes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"methodResponses":[["error",{"type":"cannotCalculateChanges"},"c0"]]}),
        ))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let schema = build_schema();
    let mut stream = schema.execute_stream(request(
        "subscription { emails(pollSeconds: 1) { id } }",
        client_for(&server),
        CardDavCreds::default(),
    ));
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap();
    assert!(
        response
            .errors
            .iter()
            .any(|error| error.message.contains("resync required"))
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn subscription_rejects_zero_poll_without_network_access() {
    use async_graphql::futures_util::StreamExt;
    let server = mock_server(0).await;
    let schema = build_schema();
    let req = request(
        "subscription { emails(pollSeconds: 0) { id } }",
        client_for(&server),
        CardDavCreds::default(),
    );
    let response = schema.execute_stream(req).next().await.unwrap();
    assert!(
        response
            .errors
            .iter()
            .any(|e| e.message.contains("at least one second"))
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn subscription_does_not_retain_lazy_records_between_events() {
    use async_graphql::futures_util::StreamExt;
    use wiremock::matchers::body_string_contains;
    let server = mock_server(1).await;
    Mock::given(body_string_contains(r#""ids":[]"#))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"methodResponses":[["Email/get", {"accountId":"acct1","state":"s0", "list":[],"notFound":[]}, "s0"]]}),
        ))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(body_string_contains("Email/changes"))
        .respond_with(|request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let previous = body["methodCalls"][0][1]["sinceState"].as_str().unwrap();
            ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[["Email/changes", {
                "accountId":"acct1","oldState":previous,"newState":format!("{previous}-next"),
                "created":["e0"], "updated":[], "destroyed":[], "hasMoreChanges":false
            }, "c0"]]}))
        })
        .with_priority(1)
        .mount(&server)
        .await;
    let schema = build_schema();
    let req = request(
        "subscription { emails(pollSeconds: 1) { id textBody readableBody { format content } attachments { nodes { base64 } } } }",
        client_for(&server),
        CardDavCreds::default(),
    ).data(super::super::attachment_scope::AttachmentScopes::with_limits(
        ATTACHMENT_BYTES.len(), super::super::attachment_scope::base64_size(ATTACHMENT_BYTES.len()), 1,
    ));
    let mut stream = schema.execute_stream(req);
    for _ in 0..3 {
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap();
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        assert_eq!(data["emails"]["textBody"], "Body of e0");
        assert_eq!(
            data["emails"]["readableBody"],
            json!({"format":"text","content":"Body of e0"})
        );
    }
    let detail_gets = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get" && c.properties.contains(&"textBody".into()))
        .count();
    assert_eq!(detail_gets, 3, "each event must release its lazy records");
    assert_eq!(
        downloads(&server).await.len(),
        3,
        "each event receives a fresh blob scope"
    );
}
