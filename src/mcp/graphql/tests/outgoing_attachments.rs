use super::*;

async fn compose(
    schema: &super::super::FastmailSchema,
    client: SharedClient,
    field: &str,
    action: &str,
    attachments: Value,
    token: Option<&str>,
) -> Value {
    let args = match field {
        "sendEmail" => "to: \"to@example.com\", subject: \"Subject\",",
        "replyToEmail" => "emailId: \"e0\",",
        "forwardEmail" => "emailId: \"e0\", to: \"to@example.com\",",
        _ => panic!("Unknown compose mutation"),
    };
    let query = format!("mutation($attachments: [AttachmentInput!], $action: SendAction!, $token: String) {{
        {field}({args} body: \"Body\", attachments: $attachments, action: $action, confirmationToken: $token)
        {{ success emailId preview confirmationToken error }} }}");
    let response = schema
        .execute(request(&query, client, CardDavCreds::default()).variables(
            async_graphql::Variables::from_json(json!({
                "attachments":attachments, "action":action, "token":token
            })),
        ))
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    response.data.into_json().unwrap()[field].clone()
}

async fn uploads(server: &MockServer) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| {
            request.method == wiremock::http::Method::POST && request.url.path() == "/jmap/upload"
        })
        .collect()
}

async fn email_sets(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter_map(|request| serde_json::from_slice::<Value>(&request.body).ok())
        .flat_map(|body| body["methodCalls"].as_array().cloned().unwrap_or_default())
        .filter(|call| call[0] == "Email/set")
        .map(|call| call[1].clone())
        .collect()
}

async fn mock_upload(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/jmap/upload"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"blobId":"uploaded"})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn attachments_are_reviewed_then_sent_or_saved_in_order_for_every_compose_path() {
    for field in ["sendEmail", "replyToEmail", "forwardEmail"] {
        for action in ["CONFIRM", "DRAFT"] {
            let server = mutations::compose_server().await;
            mock_upload(&server).await;
            let schema = build_schema();
            let client = client_for(&server);
            let attachments = json!([
                {"name":"photo.PNG", "base64":"YWJj"},
                {"name":"kept.txt", "blobId":"existing"},
                {"name":"empty.txt", "base64":""}
            ]);
            let preview = compose(
                &schema,
                client.clone(),
                field,
                "PREVIEW",
                attachments.clone(),
                None,
            )
            .await;
            assert_eq!(preview["success"], true, "{preview}");
            let text = preview["preview"].as_str().unwrap();
            assert!(
                text.contains("photo.PNG (image/png, 3 bytes, sha256:ba7816bf"),
                "{text}"
            );
            assert!(
                text.contains("kept.txt (text/plain, 18 bytes, blobId:existing)"),
                "{text}"
            );
            assert!(uploads(&server).await.is_empty());
            assert!(email_sets(&server).await.is_empty());
            assert_eq!(downloads(&server).await.len(), 1);

            let result = compose(
                &schema,
                client.clone(),
                field,
                action,
                attachments.clone(),
                preview["confirmationToken"].as_str(),
            )
            .await;
            assert_eq!(result["success"], true, "{field} {action}: {result}");
            let uploaded = uploads(&server).await;
            assert_eq!(uploaded.len(), 2);
            assert_eq!(uploaded[0].body, b"abc");
            assert_eq!(uploaded[0].headers["content-type"], "image/png");
            assert_eq!(uploaded[1].body, b"");
            assert_eq!(
                downloads(&server).await.len(),
                1,
                "CONFIRM must not remeasure blobs"
            );
            let sets = email_sets(&server).await;
            assert_eq!(sets.len(), 1);
            assert_eq!(
                sets[0]["create"]["email"]["bodyStructure"],
                json!({
                    "type":"multipart/mixed", "subParts":[
                        {"partId":"textBody", "type":"text/plain"},
                        {"blobId":"uploaded", "name":"photo.PNG", "type":"image/png", "disposition":"attachment"},
                        {"blobId":"existing", "name":"kept.txt", "type":"text/plain", "disposition":"attachment"},
                        {"blobId":"uploaded", "name":"empty.txt", "type":"text/plain", "disposition":"attachment"}
                    ]
                })
            );
            let submissions = calls(&server)
                .await
                .into_iter()
                .filter(|call| call.method == "EmailSubmission/set")
                .count();
            assert_eq!(submissions, usize::from(action == "CONFIRM"));
            let replay = compose(
                &schema,
                client,
                field,
                action,
                attachments,
                preview["confirmationToken"].as_str(),
            )
            .await;
            assert_eq!(replay["success"], false);
            assert_eq!(uploads(&server).await.len(), 2);
            assert_eq!(email_sets(&server).await.len(), 1);
        }
    }
}

#[tokio::test]
async fn changed_attachment_content_metadata_order_or_source_is_not_approved() {
    for field in ["sendEmail", "replyToEmail", "forwardEmail"] {
        let server = mutations::compose_server().await;
        let schema = build_schema();
        let client = client_for(&server);
        let shown =
            json!([{"name":"a.txt", "base64":"YWJj"}, {"name":"b.txt", "blobId":"existing"}]);
        for changed in [
            json!([]),
            json!([{"name":"a.txt", "base64":"YWJk"}, {"name":"b.txt", "blobId":"existing"}]),
            json!([{"name":"renamed.txt", "base64":"YWJj"}, {"name":"b.txt", "blobId":"existing"}]),
            json!([{"name":"a.txt", "contentType":"text/html", "base64":"YWJj"}, {"name":"b.txt", "blobId":"existing"}]),
            json!([{"name":"a.txt", "base64":"YWJj"}, {"name":"b.txt", "blobId":"different"}]),
            json!([{"name":"a.txt", "blobId":"existing"}, {"name":"b.txt", "blobId":"existing"}]),
            json!([{"name":"b.txt", "blobId":"existing"}, {"name":"a.txt", "base64":"YWJj"}]),
        ] {
            let preview = compose(
                &schema,
                client.clone(),
                field,
                "PREVIEW",
                shown.clone(),
                None,
            )
            .await;
            let result = compose(
                &schema,
                client.clone(),
                field,
                "CONFIRM",
                changed,
                preview["confirmationToken"].as_str(),
            )
            .await;
            assert_eq!(result["success"], false, "{result}");
            assert!(
                result["error"].as_str().unwrap().contains("Params changed"),
                "{result}"
            );
        }
        assert!(uploads(&server).await.is_empty());
        assert!(email_sets(&server).await.is_empty());
    }
}

#[tokio::test]
async fn malformed_or_excessive_attachment_inputs_make_no_requests() {
    let server = mutations::compose_server().await;
    let schema = build_schema();
    let client = client_for(&server);
    let mut cases = vec![
        json!([{ "name":"x.txt" }]),
        json!([{ "name":"x.txt", "base64":"", "blobId":"existing" }]),
        json!([{ "name":"x.txt", "blobId":"../session" }]),
        json!([{ "name":"x.txt", "blobId":"" }]),
        json!([{ "name":"x.txt", "base64":"!!!!" }]),
        json!([{ "name":" ", "base64":"" }]),
        json!([{ "name":"x.txt\n", "base64":"" }]),
        json!([{ "name":"x.txt", "contentType":"\rtext/plain", "base64":"" }]),
        json!([{ "name":"x.txt", "contentType":"t\u{e9}xt/plain", "base64":"" }]),
        json!([{ "name":"x.txt", "contentType":"foo", "base64":"" }]),
        json!([{ "name":"x.txt", "contentType":"text/", "base64":"" }]),
        json!([{ "name":"x.txt", "contentType":"text/plain;", "base64":"" }]),
        json!([{ "name":"x.txt", "contentType":"a b", "base64":"" }]),
        json!([{ "name":"x.txt", "contentType":"text/csv; charset=utf-8", "base64":"" }]),
        json!([{ "name":"x.txt", "contentType":"multipart/mixed", "base64":"" }]),
        json!([{ "name":"x.txt", "contentType":"text/*", "base64":"" }]),
        json!([{ "name":"x.txt", "contentType":"*/plain", "base64":"" }]),
        json!([{ "name":"x".repeat(256), "base64":"" }]),
        json!([{ "name":"x.txt", "blobId":"x".repeat(256) }]),
    ];
    cases.push(json!(vec![json!({"name":"empty.txt", "base64":""}); 129]));
    for attachments in cases {
        let result = compose(
            &schema,
            client.clone(),
            "sendEmail",
            "PREVIEW",
            attachments,
            None,
        )
        .await;
        assert_eq!(result["success"], false, "{result}");
        assert!(result["confirmationToken"].is_null());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn missing_tokens_never_upload_or_create_mail() {
    let server = mutations::compose_server().await;
    let schema = build_schema();
    for field in ["sendEmail", "replyToEmail", "forwardEmail"] {
        for action in ["CONFIRM", "DRAFT"] {
            let result = compose(
                &schema,
                client_for(&server),
                field,
                action,
                json!([{ "name":"a.txt", "base64":"YWJj" }, {"name":"b.txt", "blobId":"existing"}]),
                None,
            )
            .await;
            assert_eq!(result["success"], false);
        }
    }
    assert!(downloads(&server).await.is_empty());
    assert!(uploads(&server).await.is_empty());
    assert!(email_sets(&server).await.is_empty());
}

#[tokio::test]
async fn upload_attachment_returns_blob_metadata_without_creating_mail() {
    let server = mutations::compose_server().await;
    mock_upload(&server).await;
    let response = run(
        &server,
        r#"mutation {
        uploadAttachment(name:"photo.png", base64:"YWJj") {blobId name contentType size}
    }"#,
    )
    .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(
        response.data.into_json().unwrap(),
        json!({"uploadAttachment": {
            "blobId":"uploaded", "name":"photo.png", "contentType":"image/png", "size":3
        }})
    );
    let requests = uploads(&server).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body, b"abc");
    assert_eq!(requests[0].headers["content-type"], "image/png");
    assert!(calls(&server).await.is_empty());
}

#[tokio::test]
async fn upload_attachment_rejects_invalid_inputs_before_upload() {
    let server = mutations::compose_server().await;
    for arguments in [
        r#"name:"a.txt", contentType:"text/", base64:"YWJj""#,
        r#"name:"a.txt", contentType:"text/plain;", base64:"YWJj""#,
        r#"name:"a.txt", contentType:"text/plain; charset=utf-8", base64:"YWJj""#,
        r#"name:"a.txt", contentType:"multipart/mixed", base64:"YWJj""#,
        r#"name:"a.txt", base64:"!!!!""#,
        r#"name:"", base64:"YWJj""#,
    ] {
        let response = run(
            &server,
            &format!("mutation {{ uploadAttachment({arguments}) {{blobId}} }}"),
        )
        .await;
        assert!(!response.errors.is_empty());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn rejected_blob_download_does_not_issue_a_confirmation() {
    let server = mutations::compose_server().await;
    Mock::given(method("GET"))
        .and(path("/jmap/download/denied"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .with_priority(1)
        .mount(&server)
        .await;
    let result = compose(
        &build_schema(),
        client_for(&server),
        "sendEmail",
        "PREVIEW",
        json!([{ "name":"x.txt", "blobId":"denied" }]),
        None,
    )
    .await;
    assert_eq!(result["success"], false, "{result}");
    assert!(result["error"].as_str().unwrap().contains("403"));
    assert!(result["confirmationToken"].is_null());
    assert!(uploads(&server).await.is_empty());
    assert!(email_sets(&server).await.is_empty());
}

#[tokio::test]
async fn attachment_approvals_cannot_cross_operations_or_accounts() {
    let server = mutations::compose_server().await;
    let schema = build_schema();
    let client = client_for(&server);
    let attachments = json!([{"name":"a.txt", "blobId":"existing"}]);
    let preview = compose(
        &schema,
        client.clone(),
        "sendEmail",
        "PREVIEW",
        attachments.clone(),
        None,
    )
    .await;
    let result = compose(
        &schema,
        client.clone(),
        "forwardEmail",
        "CONFIRM",
        attachments.clone(),
        preview["confirmationToken"].as_str(),
    )
    .await;
    assert_eq!(result["success"], false);

    let preview = compose(
        &schema,
        client,
        "sendEmail",
        "PREVIEW",
        attachments.clone(),
        None,
    )
    .await;
    let mut session = session_json(&server.uri());
    session["username"] = json!("other@example.com");
    session["primaryAccounts"]["urn:ietf:params:jmap:mail"] = json!("acct2");
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session))
        .with_priority(1)
        .mount(&server)
        .await;
    let other = client_for(&server);
    other.lock().await.authenticate().await.unwrap();
    let result = compose(
        &schema,
        other,
        "sendEmail",
        "CONFIRM",
        attachments,
        preview["confirmationToken"].as_str(),
    )
    .await;
    assert_eq!(result["success"], false);
    assert!(
        result["error"].as_str().unwrap().contains("Params changed"),
        "{result}"
    );
    assert!(uploads(&server).await.is_empty());
    assert!(email_sets(&server).await.is_empty());
}

#[tokio::test]
async fn invalid_sender_is_rejected_before_attachment_upload() {
    let server = mutations::compose_server().await;
    let schema = build_schema();
    let client = client_for(&server);
    let query = r#"mutation($action: SendAction!, $token: String) {
        sendEmail(action:$action, confirmationToken:$token, from:"invalid sender",
            to:"to@example.com", subject:"Subject", body:"Body",
            attachments:[{name:"a.txt", base64:"YWJj"}]) {success confirmationToken error}
    }"#;
    let mut token = Value::Null;
    for action in ["PREVIEW", "CONFIRM"] {
        let response = schema
            .execute(
                request(query, client.clone(), CardDavCreds::default()).variables(
                    async_graphql::Variables::from_json(json!({"action":action, "token":token})),
                ),
            )
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap()["sendEmail"].clone();
        assert_eq!(data["success"], action == "PREVIEW", "{data}");
        token = data["confirmationToken"].clone();
    }
    assert!(uploads(&server).await.is_empty());
    assert!(email_sets(&server).await.is_empty());
}
