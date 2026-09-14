use super::*;

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
