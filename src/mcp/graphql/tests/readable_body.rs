use super::*;

#[tokio::test]
async fn readable_body_loader_shares_bodies_across_waiters_and_cache_hits() {
    use async_graphql::futures_util::future::join_all;
    let server = mock_server(2).await;
    let loader = super::super::loaders::shared_emails(client_for(&server));
    let loaded = join_all((0..18).map(|_| loader.load_one("e0".to_owned()))).await;
    let emails: Vec<_> = loaded.into_iter().map(|e| e.unwrap().unwrap()).collect();
    assert!(emails.iter().all(|e| Arc::ptr_eq(e, &emails[0])));
    assert_eq!(emails[0].text_content().as_deref(), Some("Body of e0"));
    let cached = loader.load_one("e0".to_owned()).await.unwrap().unwrap();
    assert!(Arc::ptr_eq(&cached, &emails[0]));
    assert_eq!(calls(&server).await.len(), 1);
}

#[tokio::test]
async fn readable_body_supports_legacy_public_loader_injection() {
    let server = mock_server(1).await;
    let client = client_for(&server);
    let loaders = super::super::loaders::Loaders::new(client.clone());
    let original: crate::models::Email = loaders
        .email
        .load_one("e0".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.id, "e0");
    let response = build_schema()
        .execute(
            async_graphql::Request::new(
                r#"{ email(id:"e0") { readableBody { content } textBody } }"#,
            )
            .data(client)
            .data(loaders.email),
        )
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(
        response.data.into_json().unwrap()["email"],
        json!({
            "readableBody":{"content":"Body of e0"}, "textBody":"Body of e0"
        })
    );
    assert_eq!(calls(&server).await.len(), 1);
}

#[tokio::test]
async fn readable_bodies_are_lazy_batched_and_preserve_raw_graphql_fields() {
    use wiremock::matchers::body_string_contains;
    let server = mock_server(3).await;
    let html = include_str!("../../../../tests/fixtures/html-email.html");
    Mock::given(method("POST"))
        .and(body_string_contains("bodyValues"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let call = &body["methodCalls"][0];
            let list: Vec<_> = call[1]["ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| {
                    json!({"id":id, "textBody":[{"partId":"1","type":"text/html"}],
                    "htmlBody":[{"partId":"1","type":"text/html"}],
                    "bodyValues":{"1":{"value":html,"isTruncated":true,"isEncodingProblem":true}}})
                })
                .collect();
            ResponseTemplate::new(200)
                .set_body_json(json!({"methodResponses":[["Email/get",{"list":list},call[2]]]}))
        })
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let response = run(&server, "{ emails(first: 3) { nodes { id } } }").await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(
        calls(&server)
            .await
            .iter()
            .filter(|c| c.method == "Email/get")
            .count(),
        1
    );

    let response = run(&server, "{ emails(first: 3) { nodes {
        id textBody htmlBody
        readableBody { format content isTruncated isEncodingProblem warnings sourceParts { partId contentType } }
        plain: readableBody(format: TEXT) { format content }
        rich: readableBody(format: MARKDOWN) { format content }
    } } }").await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let data = response.data.into_json().unwrap();
    for email in data["emails"]["nodes"].as_array().unwrap() {
        assert_eq!(email["textBody"], html);
        assert_eq!(email["htmlBody"], html);
        let body = &email["readableBody"];
        assert_eq!(body["format"], "markdown");
        assert_eq!(body["content"], email["rich"]["content"]);
        assert!(
            body["content"]
                .as_str()
                .unwrap()
                .contains("Cancellation deadline")
        );
        assert_eq!(
            body["sourceParts"],
            json!([{"partId":"1","contentType":"text/html"}])
        );
        assert_eq!(body["isTruncated"], true);
        assert_eq!(body["isEncodingProblem"], true);
        assert_eq!(email["plain"]["format"], "text");
        assert!(
            email["plain"]["content"]
                .as_str()
                .unwrap()
                .contains("Invoice & receipt")
        );
        assert!(
            !email["plain"]["content"]
                .as_str()
                .unwrap()
                .contains("pixel.example")
        );
    }
    let calls = calls(&server).await;
    let detail: Vec<_> = calls
        .iter()
        .filter(|c| c.properties.contains(&"bodyValues".to_owned()))
        .collect();
    assert_eq!(detail.len(), 1);
    assert_eq!(detail[0].ids.len(), 3);
    assert!(downloads(&server).await.is_empty());
}

#[tokio::test]
async fn readable_body_is_available_on_individual_messages_and_threads() {
    let server = mock_server(2).await;
    let response = run(
        &server,
        r#"{
        email(id: "e0") { readableBody { format content warnings } }
        thread(emailId: "e0") { emails { nodes { readableBody { format content } } } }
    }"#,
    )
    .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let data = response.data.into_json().unwrap();
    assert_eq!(
        data["email"]["readableBody"],
        json!({"format":"text","content":"Body of e0","warnings":[]})
    );
    assert!(
        data["thread"]["emails"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node["readableBody"]["format"] == "text")
    );
    assert!(downloads(&server).await.is_empty());
}

#[tokio::test]
async fn readable_body_null_and_conversion_failures_do_not_replace_raw_bodies() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/jmap"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let call = &body["methodCalls"][0];
            let list: Vec<_> = call[1]["ids"].as_array().unwrap().iter().map(|id| {
                if id == "empty" { json!({"id":id}) } else {
                    json!({"id":id,"htmlBody":[{"partId":"1","type":"text/html"}],"bodyValues":{"1":{"value":"%PDF-private"}}})
                }
            }).collect();
            ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[["Email/get",{"list":list},call[2]]]}))
        }).expect(1).mount(&server).await;
    let response = run(
        &server,
        r#"{
        empty: email(id:"empty") { readableBody { content } }
        failed: email(id:"failed") { htmlBody readableBody { content warnings } }
    }"#,
    )
    .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let data = response.data.into_json().unwrap();
    assert!(data["empty"]["readableBody"].is_null());
    assert_eq!(data["failed"]["htmlBody"], "%PDF-private");
    assert_eq!(
        data["failed"]["readableBody"]["content"],
        "[HTML conversion unavailable]"
    );
    assert_eq!(
        data["failed"]["readableBody"]["warnings"],
        json!(["HTML conversion failed; inspect the original body values."])
    );
}
