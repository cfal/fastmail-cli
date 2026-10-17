use super::*;
use std::sync::Mutex;
use wiremock::matchers::body_string_contains;

fn draft_email() -> Value {
    json!({
        "id":"draft0", "keywords":{"$draft":true, "custom":true},
        "mailboxIds":{"drafts":true, "other":true},
        "from":[{"name":"Per-message Name", "email":"alias@example.com"}],
        "to":[{"email":"to@example.com"}], "cc":[{"email":"cc@example.com"}],
        "bcc":[{"email":"hidden@example.com"}], "subject":"Saved subject",
        "headers":[
            {"name":"From","value":"Per-message Name <alias@example.com>"},
            {"name":"To","value":"to@example.com"},
            {"name":"Cc","value":"cc@example.com"},
            {"name":"Bcc","value":"hidden@example.com"}
        ],
        "textBody":[{"partId":"text", "type":"text/plain"}],
        "htmlBody":[{"partId":"html", "type":"text/html"}],
        "bodyValues":{"text":{"value":"Plain version"}, "html":{"value":"<p>Different HTML</p>"}},
        "hasAttachment":true,
        "attachments":[{"partId":"file", "blobId":"kept", "name":"kept.pdf", "type":"application/pdf", "size":123}]
    })
}

async fn draft_server() -> (MockServer, Arc<Mutex<Value>>) {
    let server = MockServer::start().await;
    let email = Arc::new(Mutex::new(draft_email()));
    let served = email.clone();
    Mock::given(method("POST")).and(path("/jmap"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let call = &body["methodCalls"][0];
            let account = &call[1]["accountId"];
            let payload = match call[0].as_str().unwrap() {
                "Email/get" => json!({"accountId":account,"state":"snapshot", "list":[served.lock().unwrap().clone()], "notFound":[]}),
                "Identity/get" => json!({"list":[{"id":"domain", "name":"Identity Name", "email":"*@example.com"}]}),
                "Mailbox/get" => json!({"list":[
                    {"id":"drafts","name":"Drafts","role":"drafts"},
                    {"id":"sent","name":"Sent","role":"sent"},
                    {"id":"trash","name":"Trash","role":"trash"}
                ]}),
                "Email/set" => {
                    served.lock().unwrap()["mailboxIds"] = call[1]["update"]["draft0"]["mailboxIds"].clone();
                    json!({"accountId":account,"updated":{"draft0":null}})
                }
                "EmailSubmission/set" => {
                    return ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[
                        ["EmailSubmission/set",{"accountId":account,"created":{"submission":{"id":"sub1"}}},call[2]],
                        ["Email/set",{"accountId":account,"updated":{"draft0":null}},call[2]]
                    ]}));
                }
                other => panic!("Unexpected call {other}"),
            };
            ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[[call[0],payload,call[2]]]}))
        }).with_priority(2).mount(&server).await;
    (server, email)
}

async fn send(
    schema: &super::super::FastmailSchema,
    client: SharedClient,
    id: &str,
    action: &str,
    token: Option<&str>,
) -> Value {
    let query = format!(
        "mutation {{ sendDraft(emailId:{},action:{action},confirmationToken:{}) {{success emailId submissionId preview confirmationToken filingWarning error}} }}",
        json!(id),
        json!(token)
    );
    let response = schema
        .execute(request(&query, client, CardDavCreds::default()))
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    response.data.into_json().unwrap()["sendDraft"].clone()
}

async fn writes(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter_map(|req| serde_json::from_slice::<Value>(&req.body).ok())
        .flat_map(|body| body["methodCalls"].as_array().cloned().unwrap_or_default())
        .filter(|call| matches!(call[0].as_str(), Some("Email/set" | "EmailSubmission/set")))
        .collect()
}

#[tokio::test]
async fn saved_draft_preview_then_send_pins_content_and_identity_without_recreating_it() {
    let (server, _) = draft_server().await;
    let schema = build_schema();
    let client = client_for(&server);
    let preview = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
    assert_eq!(preview["success"], true, "{preview}");
    let text = preview["preview"].as_str().unwrap();
    for expected in [
        "Per-message Name",
        "alias@example.com",
        "Plain version",
        "<p>Different HTML</p>",
        "kept.pdf",
        "application/pdf",
        "123",
        "hidden@example.com",
    ] {
        assert!(text.contains(expected), "Missing {expected}: {text}");
    }
    assert!(writes(&server).await.is_empty());
    let result = send(
        &schema,
        client.clone(),
        "draft0",
        "CONFIRM",
        preview["confirmationToken"].as_str(),
    )
    .await;
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(result["emailId"], "draft0");
    assert_eq!(result["submissionId"], "sub1");
    assert!(result["filingWarning"].is_null());
    let calls = writes(&server).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0][0], "EmailSubmission/set");
    assert_eq!(
        calls[0][1]["create"]["submission"],
        json!({"emailId":"draft0","identityId":"domain"})
    );
    assert_eq!(
        calls[0][1]["onSuccessUpdateEmail"]["#submission"],
        json!({
            "mailboxIds/drafts":null,"mailboxIds/sent":true,"keywords/$draft":null,"keywords/$seen":true
        })
    );
    assert_eq!(
        super::calls(&server)
            .await
            .iter()
            .filter(|call| call.method == "Identity/get")
            .count(),
        2
    );
    assert_eq!(
        send(
            &schema,
            client,
            "draft0",
            "CONFIRM",
            preview["confirmationToken"].as_str()
        )
        .await["success"],
        false
    );
    assert_eq!(writes(&server).await.len(), 1);
    assert!(downloads(&server).await.is_empty());
}

#[tokio::test]
async fn saved_draft_refuses_missing_approval_and_draft_action() {
    let (server, _) = draft_server().await;
    let schema = build_schema();
    for action in ["CONFIRM", "DRAFT"] {
        assert_eq!(
            send(&schema, client_for(&server), "draft0", action, None).await["success"],
            false
        );
    }
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn saved_draft_rejects_missing_or_duplicate_routing_headers() {
    let (server, email) = draft_server().await;
    for name in ["From", "sEnDeR", "TO", "cC", "Bcc", "Reply-To", "Subject"] {
        let mut invalid = draft_email();
        let headers = invalid["headers"].as_array_mut().unwrap();
        headers.retain(|header| !header["name"].as_str().unwrap().eq_ignore_ascii_case(name));
        headers.push(json!({"name":name,"value":"hidden@example.net"}));
        headers.push(json!({"name":name.to_lowercase(),"value":"visible@example.com"}));
        *email.lock().unwrap() = invalid;
        let result = send(
            &build_schema(),
            client_for(&server),
            "draft0",
            "PREVIEW",
            None,
        )
        .await;
        assert_eq!(result["success"], false, "{name}: {result}");
        assert!(result["confirmationToken"].is_null());
    }
    email
        .lock()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("headers");
    assert_eq!(
        send(
            &build_schema(),
            client_for(&server),
            "draft0",
            "PREVIEW",
            None
        )
        .await["success"],
        false
    );
    let mut missing_from = draft_email();
    missing_from["headers"]
        .as_array_mut()
        .unwrap()
        .retain(|header| header["name"] != "From");
    *email.lock().unwrap() = missing_from;
    assert_eq!(
        send(
            &build_schema(),
            client_for(&server),
            "draft0",
            "PREVIEW",
            None
        )
        .await["success"],
        false
    );
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn discarded_drafts_cannot_use_an_outstanding_send_approval() {
    let (server, _) = draft_server().await;
    let schema = build_schema();
    let client = client_for(&server);
    let preview = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
    assert_eq!(preview["success"], true);
    let discard = schema
        .execute(request(
            r#"mutation {deleteDraft(emailId:"draft0") {success}}"#,
            client.clone(),
            CardDavCreds::default(),
        ))
        .await;
    assert_eq!(
        discard.data.into_json().unwrap()["deleteDraft"]["success"],
        true
    );
    let result = send(
        &schema,
        client.clone(),
        "draft0",
        "CONFIRM",
        preview["confirmationToken"].as_str(),
    )
    .await;
    assert_eq!(result["success"], false, "{result}");
    assert!(result["error"].as_str().unwrap().contains("Trash"));
    assert_eq!(
        send(&schema, client, "draft0", "PREVIEW", None).await["success"],
        false
    );
    assert!(
        writes(&server)
            .await
            .iter()
            .all(|call| call[0] == "Email/set")
    );
}

#[tokio::test]
async fn draft_operations_use_roles_despite_same_named_folders() {
    let (server, email) = draft_server().await;
    email.lock().unwrap()["mailboxIds"] = json!({"real-drafts":true,"other":true});
    Mock::given(body_string_contains("Mailbox/get")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[
        ["Mailbox/get",{"list":[
            {"id":"decoy-drafts","name":"Drafts"}, {"id":"decoy-sent","name":"Sent"}, {"id":"decoy-trash","name":"Trash"},
            {"id":"real-drafts","name":"Entwuerfe","role":"drafts"}, {"id":"real-sent","name":"Gesendet","role":"sent"}, {"id":"real-trash","name":"Papierkorb","role":"trash"}
        ]},"m0"]
    ]}))).with_priority(1).mount(&server).await;
    let schema = build_schema();
    let client = client_for(&server);
    let preview = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
    let result = send(
        &schema,
        client,
        "draft0",
        "CONFIRM",
        preview["confirmationToken"].as_str(),
    )
    .await;
    assert_eq!(result["success"], true, "{result}");
    let requests = writes(&server).await;
    assert_eq!(
        requests[0][1]["onSuccessUpdateEmail"]["#submission"],
        json!({
            "mailboxIds/real-drafts":null,"mailboxIds/real-sent":true,"keywords/$draft":null,"keywords/$seen":true
        })
    );
    let discard = run(
        &server,
        r#"mutation {deleteDraft(emailId:"draft0") {success}}"#,
    )
    .await;
    assert_eq!(
        discard.data.into_json().unwrap()["deleteDraft"]["success"],
        true
    );
    assert_eq!(
        writes(&server).await[1][1]["update"]["draft0"]["mailboxIds"],
        json!({"real-trash":true})
    );
}

#[tokio::test]
async fn missing_or_duplicate_system_roles_do_not_issue_approvals() {
    for roles in [
        json!([
            {"id":"drafts","name":"Drafts"}, {"id":"sent","name":"Sent"}, {"id":"trash","name":"Trash"}
        ]),
        json!([
            {"id":"drafts","name":"Drafts","role":"drafts"}, {"id":"sent","name":"Sent","role":"sent"},
            {"id":"sent2","name":"Another Sent","role":"sent"}, {"id":"trash","name":"Trash","role":"trash"},
            {"id":"trash2","name":"Another Trash","role":"trash"}
        ]),
    ] {
        let (server, _) = draft_server().await;
        Mock::given(body_string_contains("Mailbox/get"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"methodResponses":[["Mailbox/get",{"list":roles},"m0"]]}),
                ),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        let preview = send(
            &build_schema(),
            client_for(&server),
            "draft0",
            "PREVIEW",
            None,
        )
        .await;
        assert_eq!(preview["success"], false);
        assert!(preview["confirmationToken"].is_null());
        let discard = run(
            &server,
            r#"mutation {deleteDraft(emailId:"draft0") {success}}"#,
        )
        .await;
        assert_eq!(
            discard.data.into_json().unwrap()["deleteDraft"]["success"],
            false
        );
        assert!(writes(&server).await.is_empty());
    }
}

#[tokio::test]
async fn saved_draft_rejects_incomplete_bodies_or_unsupported_senders_without_approval() {
    let (server, email) = draft_server().await;
    let mut cases = Vec::new();
    for field in ["textBody", "htmlBody", "attachments", "bodyValues", "from"] {
        let mut missing = draft_email();
        missing.as_object_mut().unwrap().remove(field);
        cases.push(missing);
    }
    for flag in ["isTruncated", "isEncodingProblem"] {
        let mut invalid = draft_email();
        invalid["bodyValues"]["html"][flag] = json!(true);
        cases.push(invalid);
    }
    let mut invalid = draft_email();
    invalid["sender"] = json!([{"email":"different@example.com"}]);
    cases.push(invalid);
    let mut invalid = draft_email();
    invalid["sender"] = json!([{"email":"alias@example.com"},{"email":"alias@example.com"}]);
    cases.push(invalid);
    let mut invalid = draft_email();
    invalid["from"] = json!([{"email":"one@example.com"},{"email":"two@example.com"}]);
    cases.push(invalid);
    for value in [json!({}), json!({"$draft":false})] {
        let mut invalid = draft_email();
        invalid["keywords"] = value;
        cases.push(invalid);
    }
    for invalid in cases {
        *email.lock().unwrap() = invalid;
        let result = send(
            &build_schema(),
            client_for(&server),
            "draft0",
            "PREVIEW",
            None,
        )
        .await;
        assert_eq!(result["success"], false, "{result}");
        assert!(result["confirmationToken"].is_null());
    }
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn saved_draft_rechecks_classification_before_submission() {
    let (server, email) = draft_server().await;
    let schema = build_schema();
    let client = client_for(&server);
    let preview = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
    email.lock().unwrap()["keywords"] = json!({"$seen":true});
    assert_eq!(
        send(
            &schema,
            client,
            "draft0",
            "CONFIRM",
            preview["confirmationToken"].as_str()
        )
        .await["success"],
        false
    );
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn saved_draft_approval_binds_resolved_identity_and_account() {
    for change in ["identity", "identity_fields", "account"] {
        let (server, _) = draft_server().await;
        let schema = build_schema();
        let client = client_for(&server);
        let preview = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
        if change != "account" {
            let identity = if change == "identity" {
                json!({"id":"changed","name":"Identity Name","email":"*@example.com"})
            } else {
                json!({"id":"domain","name":"Changed Name","email":"*@example.com"})
            };
            Mock::given(body_string_contains("Identity/get"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[
                        ["Identity/get",{"list":[identity]},"i0"]
                    ]})),
                )
                .with_priority(1)
                .mount(&server)
                .await;
        } else {
            let mut session = session_json(&server.uri());
            session["capabilities"]["urn:ietf:params:jmap:submission"] = json!({});
            session["primaryAccounts"]["urn:ietf:params:jmap:mail"] = json!("acct2");
            Mock::given(method("GET"))
                .and(path("/jmap/session"))
                .respond_with(ResponseTemplate::new(200).set_body_json(session))
                .mount(&server)
                .await;
            client.lock().await.authenticate().await.unwrap();
        }
        let result = send(
            &schema,
            client,
            "draft0",
            "CONFIRM",
            preview["confirmationToken"].as_str(),
        )
        .await;
        assert_eq!(result["success"], false, "{result}");
        assert!(
            result["error"].as_str().unwrap().contains("Params changed"),
            "{result}"
        );
        assert!(writes(&server).await.is_empty());
    }
}

#[tokio::test]
async fn saved_draft_approvals_do_not_cross_ids_operations_or_expiration() {
    let (server, email) = draft_server().await;
    let schema = build_schema();
    let client = client_for(&server);
    let preview = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
    email.lock().unwrap()["id"] = json!("draft1");
    let result = send(
        &schema,
        client.clone(),
        "draft1",
        "CONFIRM",
        preview["confirmationToken"].as_str(),
    )
    .await;
    assert_eq!(result["success"], false);
    assert!(result["error"].as_str().unwrap().contains("Params changed"));
    *email.lock().unwrap() = draft_email();
    let compose_preview = schema.execute(request(
        r#"mutation {sendEmail(action:PREVIEW,to:"to@example.com",subject:"Saved subject",body:"Plain version",from:"alias@example.com") {confirmationToken}}"#,
        client.clone(),CardDavCreds::default()
    )).await;
    let compose_preview = compose_preview.data.into_json().unwrap();
    let result = send(
        &schema,
        client.clone(),
        "draft0",
        "CONFIRM",
        compose_preview["sendEmail"]["confirmationToken"].as_str(),
    )
    .await;
    assert_eq!(result["success"], false);
    assert!(result["error"].as_str().unwrap().contains("Params changed"));

    let preview = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
    let token = preview["confirmationToken"].as_str().unwrap();
    schema
        .data::<super::super::types::NonceStore>()
        .unwrap()
        .lock()
        .await
        .get_mut(token)
        .unwrap()
        .issued_at = std::time::Instant::now() - std::time::Duration::from_secs(16 * 60);
    let result = send(&schema, client, "draft0", "CONFIRM", Some(token)).await;
    assert_eq!(result["success"], false);
    assert!(result["error"].as_str().unwrap().contains("expired"));
    assert!(writes(&server).await.is_empty());
}

#[tokio::test]
async fn saved_draft_html_only_and_empty_bodies_remain_sendable() {
    let (server, email) = draft_server().await;
    for html_only in [false, true] {
        let mut draft = draft_email();
        if html_only {
            draft["textBody"] = draft["htmlBody"].clone();
            draft["bodyValues"].as_object_mut().unwrap().remove("text");
        } else {
            draft["textBody"] = json!([]);
            draft["htmlBody"] = json!([]);
            draft["bodyValues"] = json!({});
        }
        *email.lock().unwrap() = draft;
        let result = send(
            &build_schema(),
            client_for(&server),
            "draft0",
            "PREVIEW",
            None,
        )
        .await;
        assert_eq!(result["success"], true, "{result}");
        if html_only {
            assert!(result["preview"].as_str().unwrap().contains("text/html"));
            assert!(!result["preview"].as_str().unwrap().contains("text/plain"));
        }
    }
    assert!(writes(&server).await.is_empty());
}

#[test]
fn saved_draft_preview_limits_are_explicit() {
    let identity =
        serde_json::from_value(json!({"id":"id", "name":"Me", "email":"me@example.com"})).unwrap();
    let mut draft = draft_email();
    draft["attachments"] = json!(vec![draft["attachments"][0].clone(); 129]);
    let email = serde_json::from_value(draft).unwrap();
    assert!(
        super::super::drafts::preview(&email, &identity)
            .unwrap_err()
            .contains("128-part")
    );
    let mut draft = draft_email();
    draft["bodyValues"]["text"]["value"] = json!("x".repeat(1024 * 1024));
    let email = serde_json::from_value(draft).unwrap();
    assert!(
        super::super::drafts::preview(&email, &identity)
            .unwrap_err()
            .contains("1 MiB")
    );
}

#[tokio::test]
async fn saved_draft_filing_failure_retains_submission_success_without_retry() {
    let (server, _) = draft_server().await;
    Mock::given(body_string_contains("EmailSubmission/set")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[
        ["EmailSubmission/set",{"accountId":"acct1","created":{"submission":{"id":"accepted"}}},"submitDraft"],
        ["Email/set",{"accountId":"acct1","notUpdated":{"draft0":{"type":"forbidden"}}},"submitDraft"]
    ]}))).with_priority(1).expect(1).mount(&server).await;
    let schema = build_schema();
    let client = client_for(&server);
    let preview = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
    let sibling = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
    let result = send(
        &schema,
        client.clone(),
        "draft0",
        "CONFIRM",
        preview["confirmationToken"].as_str(),
    )
    .await;
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(result["submissionId"], "accepted");
    assert_eq!(result["emailId"], "draft0");
    assert!(
        result["filingWarning"]
            .as_str()
            .unwrap()
            .contains("Do not resend")
    );
    let repeated = send(
        &schema,
        client,
        "draft0",
        "CONFIRM",
        sibling["confirmationToken"].as_str(),
    )
    .await;
    assert_eq!(repeated["success"], false, "{repeated}");
    assert!(repeated["error"].as_str().unwrap().contains("already-used"));
    assert_eq!(writes(&server).await.len(), 1);
}

#[tokio::test]
async fn ambiguous_draft_submission_failures_never_retry_or_reuse_approvals() {
    for response in [
        ResponseTemplate::new(503),
        ResponseTemplate::new(200).set_body_string("invalid json"),
        ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[]})),
    ] {
        let (server, _) = draft_server().await;
        Mock::given(body_string_contains("EmailSubmission/set"))
            .respond_with(response)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        let schema = build_schema();
        let client = client_for(&server);
        let first = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
        let sibling = send(&schema, client.clone(), "draft0", "PREVIEW", None).await;
        let result = send(
            &schema,
            client.clone(),
            "draft0",
            "CONFIRM",
            first["confirmationToken"].as_str(),
        )
        .await;
        assert_eq!(result["success"], false, "{result}");
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("outcome is unknown; inspect Sent before retrying")
        );
        for token in [
            first["confirmationToken"].as_str(),
            sibling["confirmationToken"].as_str(),
        ] {
            let replay = send(&schema, client.clone(), "draft0", "CONFIRM", token).await;
            assert_eq!(replay["success"], false);
            assert!(replay["error"].as_str().unwrap().contains("already-used"));
        }
        assert_eq!(writes(&server).await.len(), 1);
    }
}

#[tokio::test]
async fn delete_draft_is_a_state_guarded_trash_move_not_destruction() {
    let (server, _) = draft_server().await;
    let result = run(
        &server,
        r#"mutation {deleteDraft(emailId:"draft0") {success error}}"#,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(
        result.data.into_json().unwrap()["deleteDraft"]["success"],
        true
    );
    let writes = writes(&server).await;
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0],
        json!(["Email/set",{
        "accountId":"acct1","ifInState":"snapshot","update":{"draft0":{"mailboxIds":{"trash":true}}}
    },"discardDraft"])
    );
}

#[tokio::test]
async fn delete_draft_refuses_nondrafts_and_reports_conflicts_without_retrying() {
    let (server, email) = draft_server().await;
    email.lock().unwrap()["keywords"] = json!({});
    let query = r#"mutation {deleteDraft(emailId:"draft0") {success error}}"#;
    let result = run(&server, query).await;
    assert_eq!(
        result.data.into_json().unwrap()["deleteDraft"]["success"],
        false
    );
    assert!(writes(&server).await.is_empty());
    *email.lock().unwrap() = draft_email();
    Mock::given(body_string_contains("Email/set"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[
                ["error",{"type":"stateMismatch"},"discardDraft"]
            ]})),
        )
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let result = run(&server, query).await;
    let result = result.data.into_json().unwrap();
    assert_eq!(result["deleteDraft"]["success"], false);
    assert!(
        result["deleteDraft"]["error"]
            .as_str()
            .unwrap()
            .contains("stateMismatch")
    );
    assert_eq!(writes(&server).await.len(), 1);
}
