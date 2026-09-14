use super::*;

#[tokio::test]
async fn mark_read_patches_only_seen_even_after_a_concurrent_keyword_change() {
    use std::sync::Mutex;
    let server = mock_server(1).await;
    let keywords = Arc::new(Mutex::new(json!({"$flagged":true, "custom":true})));
    let updated = keywords.clone();
    Mock::given(method("POST"))
        .and(wiremock::matchers::body_string_contains("Email/set"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let patch = &body["methodCalls"][0][1]["update"]["e0"];
            assert_eq!(patch.as_object().unwrap().len(), 1);
            let seen = patch.get("keywords/$seen").unwrap();
            let mut keywords = updated.lock().unwrap();
            if seen.is_null() {
                keywords.as_object_mut().unwrap().remove("$seen");
            } else {
                keywords["$seen"] = seen.clone();
            }
            ResponseTemplate::new(200).set_body_json(
                json!({"methodResponses":[["Email/set", {"updated":{"e0":null}}, "k0"]]}),
            )
        })
        .with_priority(1)
        .mount(&server)
        .await;
    for read in [true, false] {
        let response = run(
            &server,
            &format!("mutation {{ markAsRead(emailId: \"e0\", read: {read}) {{ success }} }}"),
        )
        .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap()["markAsRead"]["success"],
            true
        );
        let keywords = keywords.lock().unwrap();
        assert_eq!(keywords["$flagged"], true);
        assert_eq!(keywords["custom"], true);
        assert_eq!(keywords.get("$seen").is_some(), read);
    }
}

async fn spam(
    schema: &super::super::FastmailSchema,
    client: SharedClient,
    id: &str,
    action: &str,
    token: Option<&str>,
) -> Value {
    let query = format!(
        "mutation {{ markAsSpam(emailId:{}, action:{action}, confirmationToken:{}) {{ success error confirmationToken }} }}",
        json!(id),
        json!(token)
    );
    let response = schema
        .execute(request(&query, client, CardDavCreds::default()))
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    response.data.into_json().unwrap()["markAsSpam"].clone()
}

#[tokio::test]
async fn spam_confirmation_requires_an_unexpired_one_shot_preview_for_the_same_email() {
    use wiremock::matchers::body_string_contains;
    let server = mock_server(2).await;
    Mock::given(body_string_contains("Mailbox/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[["Mailbox/get", {"list":[{"id":"junk", "name":"Junk", "role":"junk"}]}, "m0"]]})))
        .with_priority(1).mount(&server).await;
    Mock::given(body_string_contains("Email/set"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"methodResponses":[["Email/set", {"updated":{"e0":null}}, "u0"]]}),
        ))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let schema = build_schema();
    let client = client_for(&server);
    assert_eq!(
        spam(&schema, client.clone(), "e0", "CONFIRM", None).await["success"],
        false
    );
    let preview = spam(&schema, client.clone(), "e0", "PREVIEW", None).await;
    let token = preview["confirmationToken"].as_str().unwrap();
    assert_eq!(
        spam(&schema, client.clone(), "e1", "CONFIRM", Some(token)).await["success"],
        false
    );

    let preview = spam(&schema, client.clone(), "e0", "PREVIEW", None).await;
    let token = preview["confirmationToken"].as_str().unwrap();
    schema
        .data::<super::super::types::NonceStore>()
        .unwrap()
        .lock()
        .await
        .get_mut(token)
        .unwrap()
        .issued_at = std::time::Instant::now() - std::time::Duration::from_secs(16 * 60);
    let expired = spam(&schema, client.clone(), "e0", "CONFIRM", Some(token)).await;
    assert!(expired["error"].as_str().unwrap().contains("expired"));

    let preview = spam(&schema, client.clone(), "e0", "PREVIEW", None).await;
    let token = preview["confirmationToken"].as_str().unwrap();
    assert_eq!(
        spam(&schema, client.clone(), "e0", "CONFIRM", Some(token)).await["success"],
        true
    );
    assert_eq!(
        spam(&schema, client, "e0", "CONFIRM", Some(token)).await["success"],
        false
    );
}

async fn compose(
    schema: &super::super::FastmailSchema,
    client: SharedClient,
    field: &str,
    action: &str,
    params: &Value,
    token: Option<&str>,
) -> Value {
    let mut args = params
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| format!("{key}: {value}"))
        .collect::<Vec<_>>();
    args.push(format!("action: {action}"));
    if let Some(token) = token {
        args.push(format!("confirmationToken: {}", json!(token)));
    }
    let query = format!(
        "mutation {{ {field}({}) {{ success preview confirmationToken error }} }}",
        args.join(", ")
    );
    let response = schema
        .execute(request(&query, client, CardDavCreds::default()))
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    response.data.into_json().unwrap()[field].clone()
}

#[tokio::test]
async fn unchanged_compose_confirmation_sends_reviewed_payload_once() {
    let server = mock_server(1).await;
    let subject_of = |preview: &Value| {
        preview["preview"]
            .as_str()
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("Subject: "))
            .unwrap()
            .to_string()
    };
    let mut reviewed_subjects = Vec::new();
    Mock::given(|req: &wiremock::Request| {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
        body["methodCalls"].as_array().is_some_and(|calls| {
            calls.iter().any(|c| {
                matches!(
                    c[0].as_str(),
                    Some("Mailbox/get" | "Identity/get" | "Email/set")
                )
            })
        })
    })
    .respond_with(|req: &wiremock::Request| {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let responses: Vec<_> = body["methodCalls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let payload = match c[0].as_str().unwrap() {
                    "Mailbox/get" => json!({"list": [
                        {"id":"sent", "name":"Sent", "role":"sent"},
                        {"id":"drafts", "name":"Drafts", "role":"drafts"}
                    ]}),
                    "Identity/get" => {
                        json!({"list": [{"id":"id1", "name":"Me", "email":"me@example.com"}]})
                    }
                    "Email/set" => json!({"created":{"email":{"id":"created"}}}),
                    "EmailSubmission/set" => json!({"created":{"submission":{"id":"submitted"}}}),
                    _ => panic!("Unexpected method: {}", c[0]),
                };
                json!([c[0], payload, c[2]])
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({"methodResponses":responses}))
    })
    .with_priority(1)
    .mount(&server)
    .await;
    for field in ["sendEmail", "replyToEmail", "forwardEmail"] {
        let schema = build_schema();
        let client = client_for(&server);
        let mut params = json!({"body":"approved body", "cc":"cc@example.com", "bcc":"bcc@example.com", "htmlBody":"<p>approved</p>"});
        if field != "replyToEmail" {
            params["to"] = json!("to@example.com");
        }
        if field == "sendEmail" {
            params["subject"] = json!("approved subject");
        } else {
            params["emailId"] = json!("e0");
        }
        let preview = compose(&schema, client.clone(), field, "PREVIEW", &params, None).await;
        reviewed_subjects.push(subject_of(&preview));
        assert!(
            preview["preview"]
                .as_str()
                .unwrap()
                .contains("me@example.com")
        );
        let token = preview["confirmationToken"].as_str().unwrap();
        let sent = compose(
            &schema,
            client.clone(),
            field,
            "CONFIRM",
            &params,
            Some(token),
        )
        .await;
        assert_eq!(sent["success"], true, "{sent}");
        let replay = compose(
            &schema,
            client.clone(),
            field,
            "CONFIRM",
            &params,
            Some(token),
        )
        .await;
        assert_eq!(replay["success"], false);
        let preview = compose(&schema, client.clone(), field, "PREVIEW", &params, None).await;
        reviewed_subjects.push(subject_of(&preview));
        let draft = compose(
            &schema,
            client,
            field,
            "DRAFT",
            &params,
            preview["confirmationToken"].as_str(),
        )
        .await;
        assert_eq!(draft["success"], true, "{draft}");
    }
    let mut submissions = 0;
    let mut creates = 0;
    let mut submitted_subjects = Vec::new();
    for req in server.received_requests().await.unwrap() {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
        for call in body["methodCalls"].as_array().into_iter().flatten() {
            if call[0] == "EmailSubmission/set" {
                assert_eq!(
                    call[1]["onSuccessUpdateEmail"]["#submission"]["mailboxIds"],
                    json!({"sent":true})
                );
                assert_eq!(
                    call[1]["onSuccessUpdateEmail"]["#submission"]["keywords/$draft"],
                    Value::Null
                );
                submissions += 1;
            }
            if call[0] != "Email/set" {
                continue;
            }
            creates += 1;
            let email = &call[1]["create"]["email"];
            submitted_subjects.push(email["subject"].as_str().unwrap().to_string());
            assert_eq!(email["mailboxIds"], json!({"drafts":true}));
            assert_eq!(email["keywords"]["$draft"], true);
            assert_eq!(email["cc"][0]["email"], "cc@example.com");
            assert_eq!(email["bcc"][0]["email"], "bcc@example.com");
            assert_eq!(email["from"][0]["email"], "me@example.com");
            assert!(
                email["bodyValues"]["textBody"]["value"]
                    .as_str()
                    .unwrap()
                    .contains("approved body")
            );
            assert_eq!(email["bodyValues"]["htmlBody"]["value"], "<p>approved</p>");
        }
    }
    assert_eq!(creates, 6);
    assert_eq!(submissions, 3);
    assert_eq!(submitted_subjects, reviewed_subjects);
}

#[tokio::test]
async fn compose_confirmation_binds_every_argument() {
    let server = mock_server(1).await;
    let client = client_for(&server);
    for field in ["sendEmail", "replyToEmail", "forwardEmail"] {
        let schema = build_schema();
        let mut params = json!({"body": "body", "cc": "cc@example.com", "bcc": "bcc@example.com", "from": "sender@example.com", "htmlBody": "<p>review me</p>"});
        if field != "replyToEmail" {
            params["to"] = json!("to@example.com");
        }
        if field == "sendEmail" {
            params["subject"] = json!("subject");
        } else {
            params["emailId"] = json!("e0");
        }
        if field == "replyToEmail" {
            params["all"] = json!(false);
        }
        for key in params.as_object().unwrap().keys() {
            let preview = compose(&schema, client.clone(), field, "PREVIEW", &params, None).await;
            assert!(
                preview["preview"]
                    .as_str()
                    .unwrap()
                    .contains("<p>review me</p>")
            );
            let token = preview["confirmationToken"].as_str().unwrap();
            let mut changed = params.clone();
            changed[key] = if key == "all" {
                json!(true)
            } else {
                json!("changed")
            };
            let confirmed = compose(
                &schema,
                client.clone(),
                field,
                "CONFIRM",
                &changed,
                Some(token),
            )
            .await;
            assert_eq!(confirmed["success"], false, "{field}.{key}");
            assert!(
                confirmed["error"]
                    .as_str()
                    .unwrap()
                    .contains("Params changed"),
                "{field}.{key}: {confirmed}"
            );
        }
    }
    assert!(
        !calls(&server)
            .await
            .iter()
            .any(|call| call.method.contains("/set"))
    );
}

#[tokio::test]
async fn compose_confirmation_cannot_cross_operations_or_accounts() {
    let server = mock_server(1).await;
    let schema = build_schema();
    let params = json!({"to": "to@example.com", "subject": "e0", "body": "body"});
    let preview = compose(
        &schema,
        client_for(&server),
        "sendEmail",
        "PREVIEW",
        &params,
        None,
    )
    .await;
    let token = preview["confirmationToken"].as_str().unwrap();
    let response = compose(
        &schema,
        client_for(&server),
        "forwardEmail",
        "CONFIRM",
        &json!({"emailId": "e0", "to": "to@example.com", "body": "body"}),
        Some(token),
    )
    .await;
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("Params changed")
    );

    let preview = compose(
        &schema,
        client_for(&server),
        "sendEmail",
        "PREVIEW",
        &params,
        None,
    )
    .await;
    let token = preview["confirmationToken"].as_str().unwrap();
    let mut second_session = session_json(&server.uri());
    second_session["username"] = json!("second@example.com");
    second_session["primaryAccounts"]["urn:ietf:params:jmap:mail"] = json!("acct2");
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(second_session))
        .with_priority(1)
        .mount(&server)
        .await;
    let second = client_for(&server);
    second.lock().await.authenticate().await.unwrap();
    let response = compose(
        &schema,
        second,
        "sendEmail",
        "CONFIRM",
        &params,
        Some(token),
    )
    .await;
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("Params changed")
    );
}
