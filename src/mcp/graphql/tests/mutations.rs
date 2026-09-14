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

async fn compose_server() -> MockServer {
    let server = mock_server(1).await;
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
                        json!({"list": [
                            {"id":"id1", "name":"Me", "email":"me@example.com"},
                            {"id":"domain", "name":"Saved Name", "email":"*@example.com"}
                        ]})
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
    .with_priority(2)
    .mount(&server)
    .await;
    server
}

#[tokio::test]
async fn compose_pins_the_reviewed_default_when_identity_order_changes() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    for (field, other_email) in ["sendEmail", "replyToEmail", "forwardEmail"]
        .into_iter()
        .flat_map(|field| ["me@example.com", "other@example.com"].map(|email| (field, email)))
    {
        let server = compose_server().await;
        let lookups = Arc::new(AtomicUsize::new(0));
        let counter = lookups.clone();
        Mock::given(wiremock::matchers::body_string_contains("Identity/get"))
            .respond_with(move |_: &wiremock::Request| {
                let mut identities = vec![
                    json!({"id":"id1", "name":"Me", "email":"me@example.com"}),
                    json!({"id":"other", "name":"Other", "email":other_email}),
                ];
                if counter.fetch_add(1, Ordering::SeqCst) >= 2 {
                    identities.reverse();
                }
                ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[[
                    "Identity/get", {"list":identities}, "i0"
                ]]}))
            })
            .with_priority(1)
            .expect(2)
            .mount(&server)
            .await;
        let schema = build_schema();
        let client = client_for(&server);
        let mut params = json!({"body":"Body"});
        if field == "sendEmail" {
            params["subject"] = json!("Subject");
        } else {
            params["emailId"] = json!("e0");
        }
        if field != "replyToEmail" {
            params["to"] = json!("to@example.com");
        }
        let preview = compose(&schema, client.clone(), field, "PREVIEW", &params, None).await;
        assert!(
            preview["preview"]
                .as_str()
                .unwrap()
                .contains("From: Me <me@example.com>")
        );
        let result = compose(
            &schema,
            client,
            field,
            "CONFIRM",
            &params,
            preview["confirmationToken"].as_str(),
        )
        .await;
        assert_eq!(result["success"], true, "{result}");
        assert_eq!(lookups.load(Ordering::SeqCst), 2);
        let mut creates = 0;
        for req in server.received_requests().await.unwrap() {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            for call in body["methodCalls"].as_array().into_iter().flatten() {
                if call[0] == "Email/set" {
                    creates += 1;
                    assert_eq!(
                        call[1]["create"]["email"]["from"],
                        json!([{"email":"me@example.com", "name":"Me"}])
                    );
                }
                if call[0] == "EmailSubmission/set" {
                    assert_eq!(call[1]["create"]["submission"]["identityId"], "id1");
                }
            }
        }
        assert_eq!(creates, 1);
    }
}

#[tokio::test]
async fn compose_confirmation_rejects_changed_identity_fields() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    for (field, changed_key) in ["sendEmail", "replyToEmail", "forwardEmail"]
        .into_iter()
        .flat_map(|field| ["id", "name", "email"].map(|key| (field, key)))
    {
        let server = compose_server().await;
        let lookups = Arc::new(AtomicUsize::new(0));
        let counter = lookups.clone();
        Mock::given(wiremock::matchers::body_string_contains("Identity/get"))
            .respond_with(move |_: &wiremock::Request| {
                let mut identity = json!({"id":"id1", "name":"Me", "email":"me@example.com"});
                if counter.fetch_add(1, Ordering::SeqCst) > 0 {
                    identity[changed_key] = json!("other@example.com");
                }
                ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[[
                    "Identity/get", {"list":[identity]}, "i0"
                ]]}))
            })
            .with_priority(1)
            .expect(2)
            .mount(&server)
            .await;
        let schema = build_schema();
        let client = client_for(&server);
        let mut params = json!({"body":"Body"});
        if field == "sendEmail" {
            params["subject"] = json!("Subject");
        } else {
            params["emailId"] = json!("e0");
        }
        if field != "replyToEmail" {
            params["to"] = json!("to@example.com");
        }
        let preview = compose(&schema, client.clone(), field, "PREVIEW", &params, None).await;
        let result = compose(
            &schema,
            client,
            field,
            "CONFIRM",
            &params,
            preview["confirmationToken"].as_str(),
        )
        .await;
        assert_eq!(result["success"], false, "{field}.{changed_key}: {result}");
        assert!(result["error"].as_str().unwrap().contains("Params changed"));
        assert_eq!(lookups.load(Ordering::SeqCst), 2);
        assert!(
            !calls(&server)
                .await
                .iter()
                .any(|call| call.method.contains("/set"))
        );
    }
}

#[tokio::test]
async fn compose_drafts_retain_failed_identity_resolution_without_retrying() {
    for field in ["sendEmail", "replyToEmail", "forwardEmail"] {
        for status in [200, 503] {
            for from in [None, Some("me@example.com")] {
                let server = compose_server().await;
                Mock::given(wiremock::matchers::body_string_contains("Identity/get"))
                    .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                        "methodResponses":[["Identity/get", {"list":[]}, "i0"]]
                    })))
                    .with_priority(1)
                    .expect(2)
                    .mount(&server)
                    .await;
                let schema = build_schema();
                let client = client_for(&server);
                let mut params = json!({"body":"Body"});
                if field == "sendEmail" {
                    params["subject"] = json!("Subject");
                } else {
                    params["emailId"] = json!("e0");
                }
                if field != "replyToEmail" {
                    params["to"] = json!("to@example.com");
                }
                if let Some(from) = from {
                    params["from"] = json!(from);
                }
                let preview =
                    compose(&schema, client.clone(), field, "PREVIEW", &params, None).await;
                assert!(
                    preview["preview"]
                        .as_str()
                        .unwrap()
                        .contains("From: (unresolved)")
                );
                let result = compose(
                    &schema,
                    client,
                    field,
                    "DRAFT",
                    &params,
                    preview["confirmationToken"].as_str(),
                )
                .await;
                assert_eq!(
                    result["success"],
                    from.is_none(),
                    "{field}/{status}: {result}"
                );
                let mut creates = 0;
                for req in server.received_requests().await.unwrap() {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    for call in body["methodCalls"].as_array().into_iter().flatten() {
                        assert_ne!(call[0], "EmailSubmission/set");
                        if call[0] == "Email/set" {
                            creates += 1;
                            let email = &call[1]["create"]["email"];
                            assert!(email.get("from").is_none());
                            assert_eq!(email["mailboxIds"], json!({"drafts":true}));
                            assert_eq!(email["keywords"]["$draft"], true);
                        }
                    }
                }
                assert_eq!(creates, usize::from(from.is_none()));
            }
        }
    }
}

#[tokio::test]
async fn unchanged_compose_confirmation_sends_reviewed_payload_once() {
    let server = compose_server().await;
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
    let mut reviewed_senders = Vec::new();
    let mut expected_identities = Vec::new();
    for field in ["sendEmail", "replyToEmail", "forwardEmail"] {
        for from in [None, Some("Fastmail-CLI Tester <new@example.com>")] {
            let schema = build_schema();
            let client = client_for(&server);
            let mut params = json!({"body":"approved body", "cc":"cc@example.com", "bcc":"bcc@example.com", "htmlBody":"<p>approved</p>"});
            if let Some(from) = from {
                params["from"] = json!(from);
            }
            if field != "replyToEmail" {
                params["to"] = json!("to@example.com");
            }
            if field == "sendEmail" {
                params["subject"] = json!("approved subject");
            } else {
                params["emailId"] = json!("e0");
            }
            if from.is_some() {
                let preview =
                    compose(&schema, client.clone(), field, "PREVIEW", &params, None).await;
                let mut changed = params.clone();
                changed["from"] = json!("Different Name <new@example.com>");
                let rejected = compose(
                    &schema,
                    client.clone(),
                    field,
                    "CONFIRM",
                    &changed,
                    preview["confirmationToken"].as_str(),
                )
                .await;
                assert_eq!(rejected["success"], false);
                assert!(
                    rejected["error"]
                        .as_str()
                        .unwrap()
                        .contains("Params changed")
                );
            }
            let preview = compose(&schema, client.clone(), field, "PREVIEW", &params, None).await;
            reviewed_subjects.push(subject_of(&preview));
            let expected_sender = from.unwrap_or("Me <me@example.com>");
            assert!(
                preview["preview"]
                    .as_str()
                    .unwrap()
                    .lines()
                    .any(|line| line == format!("From: {expected_sender}"))
            );
            reviewed_senders.push(expected_sender.to_string());
            expected_identities.push(if from.is_some() { "domain" } else { "id1" });
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
            reviewed_senders.push(expected_sender.to_string());
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
    }
    let mut submissions = 0;
    let mut creates = 0;
    let mut submitted_subjects = Vec::new();
    let mut submitted_senders = Vec::new();
    let mut submitted_identities = Vec::new();
    for req in server.received_requests().await.unwrap() {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
        for call in body["methodCalls"].as_array().into_iter().flatten() {
            if call[0] == "EmailSubmission/set" {
                submitted_identities.push(
                    call[1]["create"]["submission"]["identityId"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                );
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
            let from: Vec<crate::models::EmailAddress> =
                serde_json::from_value(email["from"].clone()).unwrap();
            assert_eq!(from.len(), 1);
            submitted_senders.push(from[0].to_string());
            assert!(
                email["bodyValues"]["textBody"]["value"]
                    .as_str()
                    .unwrap()
                    .contains("approved body")
            );
            assert_eq!(email["bodyValues"]["htmlBody"]["value"], "<p>approved</p>");
        }
    }
    assert_eq!(creates, 12);
    assert_eq!(submissions, 6);
    assert_eq!(submitted_subjects, reviewed_subjects);
    assert_eq!(submitted_senders, reviewed_senders);
    assert_eq!(submitted_identities, expected_identities);
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
