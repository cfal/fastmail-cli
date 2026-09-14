use super::*;

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
            json!({"methodResponses":[["Email/get", {"state":"s0", "list":[]}, "s0"]]}),
        ))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(body_string_contains("Email/changes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[["Email/changes", {"newState":"s1", "created":["e0"], "updated":[], "destroyed":[], "hasMoreChanges":false}, "c0"]]})))
        .with_priority(1).mount(&server).await;
    let schema = build_schema();
    let req = request(
        "subscription { emails(pollSeconds: 1) { id textBody } }",
        client_for(&server),
        CardDavCreds::default(),
    );
    let mut stream = schema.execute_stream(req);
    for _ in 0..3 {
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap();
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap()["emails"]["textBody"],
            "Body of e0"
        );
    }
    let detail_gets = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get" && c.properties.contains(&"textBody".into()))
        .count();
    assert_eq!(detail_gets, 3, "each event must release its lazy records");
}
