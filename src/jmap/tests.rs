use super::*;

#[test]
fn subject_prefixes_preserve_case_whitespace_and_missing_subjects() {
    for (subject, reply, forward) in [
        (None, "Re: ", "Fwd: "),
        (Some(""), "Re: ", "Fwd: "),
        (Some("Subject"), "Re: Subject", "Fwd: Subject"),
        (Some("Re:"), "Re:", "Fwd: Re:"),
        (Some("rE: Subject"), "rE: Subject", "Fwd: rE: Subject"),
        (Some("fWd: Subject"), "Re: fWd: Subject", "fWd: Subject"),
        (
            Some(" Re: Subject"),
            "Re:  Re: Subject",
            "Fwd:  Re: Subject",
        ),
        (Some("rE: \u{130}"), "rE: \u{130}", "Fwd: rE: \u{130}"),
    ] {
        assert_eq!(prefixed_subject(subject, "Re:"), reply);
        assert_eq!(prefixed_subject(subject, "Fwd:"), forward);
    }
}

#[test]
fn http_status_classification_keeps_errors_and_endpoint_fallbacks_distinct() {
    for message in ["Authentication failed", "Token expired or invalid"] {
        for code in [200, 204, 302, 400, 401, 404, 413, 429, 500, 503, 599] {
            let status = reqwest::StatusCode::from_u16(code).unwrap();
            let result = check_jmap_status(status, message);
            match code {
                401 => {
                    assert!(matches!(result, Err(Error::InvalidToken(value)) if value == message))
                }
                429 => assert!(matches!(result, Err(Error::RateLimited))),
                500..=599 => assert!(
                    matches!(result, Err(Error::Server(value)) if value == format!("Server error: {status}"))
                ),
                _ => assert!(result.is_ok()),
            }
        }
    }
}

#[test]
fn method_errors_preserve_upstream_fields_and_fallbacks() {
    for (payload, expected_type, expected_description) in [
        (
            json!({"type":"forbidden", "description":"Not allowed"}),
            "forbidden",
            "Not allowed",
        ),
        (json!({"type":"", "description":""}), "", ""),
        (
            json!({"type":42, "description":false}),
            "unknown",
            "Fallback",
        ),
        (Value::Null, "unknown", "Fallback"),
    ] {
        let error = method_error("Email/set", &payload, "Fallback");
        assert_eq!(
            error.to_string(),
            format!("JMAP error: Email/set failed - {expected_type}: {expected_description}")
        );
    }
}

#[test]
fn address_serialization_preserves_explicit_null_names_and_key_order() {
    let addresses = vec![
        EmailAddress {
            name: None,
            email: "a@example.test".into(),
        },
        EmailAddress {
            name: Some("A Name".into()),
            email: "b@example.test".into(),
        },
    ];
    assert_eq!(
        addresses_json(&addresses).to_string(),
        r#"[{"email":"a@example.test","name":null},{"email":"b@example.test","name":"A Name"}]"#,
    );
}

#[test]
fn cli_search_mapping_preserves_every_field_and_omits_unset_flags() {
    let filter = SearchFilter {
        text: Some("".into()),
        from: Some("from".into()),
        to: Some("to".into()),
        cc: Some("cc".into()),
        bcc: Some("bcc".into()),
        subject: Some("subject".into()),
        body: Some("body".into()),
        mailbox: Some("not-an-id".into()),
        has_attachment: true,
        min_size: Some(0),
        max_size: Some(99),
        before: Some("2026-01-01".into()),
        after: Some("2025-01-01T12:00:00Z".into()),
        unread: true,
        flagged: true,
    };
    assert_eq!(
        search_filter_to_jmap(&filter, Some("mb1")),
        json!({
            "text":"", "from":"from", "to":"to", "cc":"cc", "bcc":"bcc", "subject":"subject",
            "body":"body", "inMailbox":"mb1", "hasAttachment":true, "minSize":0, "maxSize":99,
            "before":"2026-01-01T00:00:00Z", "after":"2025-01-01T12:00:00Z",
            "notKeyword":"$seen", "hasKeyword":"$flagged",
        })
    );
    assert_eq!(
        search_filter_to_jmap(&SearchFilter::default(), None),
        json!({})
    );
}

fn create_test_session(capabilities: Vec<&str>) -> Session {
    let mut caps = HashMap::new();
    for cap in capabilities {
        caps.insert(cap.to_string(), serde_json::json!({}));
    }

    let mut primary_accounts = HashMap::new();
    primary_accounts.insert(
        "urn:ietf:params:jmap:mail".to_string(),
        "test-account".to_string(),
    );

    Session {
        capabilities: caps,
        accounts: HashMap::new(),
        primary_accounts,
        username: "test@example.com".to_string(),
        api_url: "https://api.example.com/jmap".to_string(),
        download_url: "https://api.example.com/download".to_string(),
        upload_url: "https://api.example.com/upload".to_string(),
        event_source_url: None,
        state: None,
    }
}

#[test]
fn test_apply_url_template_basic() {
    let result = apply_url_template(
        "https://api.example.com/{a}/{b}",
        &[("a", "hello"), ("b", "world")],
    );
    assert_eq!(result, "https://api.example.com/hello/world");
}

#[test]
fn test_apply_url_template_no_cascade() {
    // A value that contains another template marker must not be re-substituted.
    let result = apply_url_template("https://x/{a}/{b}", &[("a", "{b}"), ("b", "LEAKED")]);
    assert_eq!(result, "https://x/%7Bb%7D/LEAKED");
}

#[test]
fn test_apply_url_template_unknown_placeholder_preserved() {
    let result = apply_url_template("/{known}/{other}", &[("known", "X")]);
    assert_eq!(result, "/X/{other}");
}

#[test]
fn test_apply_url_template_no_placeholders() {
    let result = apply_url_template("https://api.example.com/v1", &[]);
    assert_eq!(result, "https://api.example.com/v1");
}

#[test]
fn test_apply_url_template_unterminated_brace() {
    let result = apply_url_template("/path/{unterminated", &[]);
    assert_eq!(result, "/path/{unterminated");
}

#[test]
fn test_require_capability_succeeds_when_present() {
    let mut client = JmapClient::try_new("test-token".to_string()).unwrap();
    client.session = Some(create_test_session(vec![
        "urn:ietf:params:jmap:core",
        "urn:ietf:params:jmap:mail",
        "urn:ietf:params:jmap:submission",
    ]));

    assert!(
        client
            .require_capability("urn:ietf:params:jmap:submission", "Email sending")
            .is_ok()
    );
}

#[test]
fn test_require_capability_fails_when_missing() {
    let mut client = JmapClient::try_new("test-token".to_string()).unwrap();
    client.session = Some(create_test_session(vec![
        "urn:ietf:params:jmap:core",
        "urn:ietf:params:jmap:mail",
    ]));

    let result = client.require_capability("urn:ietf:params:jmap:submission", "Email sending");
    assert!(result.is_err());

    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("urn:ietf:params:jmap:submission"));
    assert!(err_msg.contains("read-only"));
}

#[test]
fn test_require_capability_fails_when_no_session() {
    let client = JmapClient::try_new("test-token".to_string()).unwrap();

    let result = client.require_capability("urn:ietf:params:jmap:submission", "Email sending");
    assert!(result.is_err());

    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Authentication required"));
    assert!(!err_msg.contains("read-only"));
}

#[test]
fn test_require_capability_works_for_masked_email() {
    let mut client = JmapClient::try_new("test-token".to_string()).unwrap();
    client.session = Some(create_test_session(vec![
        "urn:ietf:params:jmap:core",
        "urn:ietf:params:jmap:mail",
    ]));

    let result =
        client.require_capability("https://www.fastmail.com/dev/maskedemail", "Masked email");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("maskedemail"));
}

fn test_identity(id: &str, email: &str, name: &str) -> Identity {
    Identity {
        id: id.to_string(),
        name: name.to_string(),
        email: email.to_string(),
        reply_to: None,
        bcc: None,
        html_signature: None,
        text_signature: None,
        may_delete: true,
    }
}

fn addr(email: &str) -> EmailAddress {
    EmailAddress {
        name: None,
        email: email.to_string(),
    }
}

fn reply_fixture(from: Vec<&str>, to: Vec<&str>, cc: Vec<&str>) -> Email {
    let mut email = Email {
        id: "test".into(),
        ..Default::default()
    };
    email.from = Some(from.iter().map(|e| addr(e)).collect());
    email.to = Some(to.iter().map(|e| addr(e)).collect());
    email.cc = Some(cc.iter().map(|e| addr(e)).collect());
    email
}

fn emails(addrs: &[EmailAddress]) -> Vec<String> {
    addrs.iter().map(|a| a.email.clone()).collect()
}

#[test]
fn test_expand_reply_plain_does_not_expand() {
    let original = reply_fixture(
        vec!["sender@x"],
        vec!["recip1@x", "recip2@x"],
        vec!["cc1@x"],
    );
    let (to, cc) = expand_reply_recipients(&original, false, Some("me@x"), vec![addr("user@x")]);
    assert_eq!(emails(&to), vec!["sender@x"]);
    assert_eq!(emails(&cc), vec!["user@x"]);
}

#[test]
fn test_expand_reply_prefers_reply_to_over_from() {
    // The shape that bounces in the wild: a branded From on a domain with
    // no MX record, and the real inbox in Reply-To.
    let mut original = reply_fixture(vec!["noreply@branded.invalid"], vec![], vec![]);
    original.reply_to = Some(vec![addr("support@real")]);

    let (to, _) = expand_reply_recipients(&original, false, Some("me@x"), vec![]);
    assert_eq!(emails(&to), vec!["support@real"]);
}

#[test]
fn test_expand_reply_all_uses_reply_to_as_the_sender() {
    let mut original = reply_fixture(
        vec!["noreply@branded.invalid"],
        vec!["recip1@x", "me@x"],
        vec!["cc1@x"],
    );
    original.reply_to = Some(vec![addr("support@real")]);

    let (to, cc) = expand_reply_recipients(&original, true, Some("me@x"), vec![]);
    // Reply-To replaces From, it does not join it: the branded address is
    // still undeliverable on a reply-all.
    assert_eq!(emails(&to), vec!["support@real", "recip1@x"]);
    assert_eq!(emails(&cc), vec!["cc1@x"]);
}

#[test]
fn test_expand_reply_falls_back_to_from_when_reply_to_is_absent_or_empty() {
    let original = reply_fixture(vec!["sender@x"], vec![], vec![]);
    let (to, _) = expand_reply_recipients(&original, false, None, vec![]);
    assert_eq!(emails(&to), vec!["sender@x"]);

    // An empty list is not a Reply-To. Treating it as one would reply to
    // nobody.
    let mut empty = reply_fixture(vec!["sender@x"], vec![], vec![]);
    empty.reply_to = Some(vec![]);
    let (to, _) = expand_reply_recipients(&empty, false, None, vec![]);
    assert_eq!(emails(&to), vec!["sender@x"]);
}

#[test]
fn test_expand_reply_all_adds_original_recipients() {
    let original = reply_fixture(
        vec!["sender@x"],
        vec!["recip1@x", "recip2@x"],
        vec!["cc1@x"],
    );
    let (to, cc) = expand_reply_recipients(&original, true, Some("me@x"), vec![]);
    assert_eq!(emails(&to), vec!["sender@x", "recip1@x", "recip2@x"]);
    assert_eq!(emails(&cc), vec!["cc1@x"]);
}

#[test]
fn test_expand_reply_all_filters_me_from_to() {
    let original = reply_fixture(
        vec!["sender@x"],
        vec!["recip1@x", "me@x", "recip2@x"],
        vec![],
    );
    let (to, _) = expand_reply_recipients(&original, true, Some("me@x"), vec![]);
    assert_eq!(emails(&to), vec!["sender@x", "recip1@x", "recip2@x"]);
}

#[test]
fn test_expand_reply_all_filters_me_from_cc() {
    let original = reply_fixture(vec!["sender@x"], vec![], vec!["cc1@x", "me@x", "cc2@x"]);
    let (_, cc) = expand_reply_recipients(&original, true, Some("me@x"), vec![]);
    assert_eq!(emails(&cc), vec!["cc1@x", "cc2@x"]);
}

#[test]
fn test_expand_reply_all_case_insensitive_me() {
    let original = reply_fixture(vec!["sender@x"], vec!["ME@X"], vec!["me@X"]);
    let (to, cc) = expand_reply_recipients(&original, true, Some("me@x"), vec![]);
    assert_eq!(emails(&to), vec!["sender@x"]);
    assert_eq!(emails(&cc), Vec::<String>::new());
}

#[test]
fn test_expand_reply_dedupes_overlapping_user_cc_and_reply_all_to() {
    // The exact duplicate-send scenario from the bug report: user notices
    // preview is missing recipients, adds them as cc to "fix" the preview;
    // send path expands reply-all into To AND those addresses appear in CC.
    let original = reply_fixture(
        vec!["paul@x"],
        vec!["sher@x", "dylan@x", "anne@x", "leon@x"],
        vec![],
    );
    let user_cc = vec![addr("sher@x"), addr("anne@x"), addr("leon@x")];
    let (to, cc) = expand_reply_recipients(&original, true, Some("dylan@x"), user_cc);
    // Dylan filtered out; rest in To.
    assert_eq!(emails(&to), vec!["paul@x", "sher@x", "anne@x", "leon@x"]);
    // Nothing in CC — all user-supplied addresses were already in To.
    assert_eq!(emails(&cc), Vec::<String>::new());
}

#[test]
fn test_expand_reply_dedupes_duplicates_in_original() {
    // Unusual but possible: original.from address also appears in
    // original.to (e.g. sender CC'd themselves).
    let original = reply_fixture(vec!["x@x"], vec!["x@x", "y@x"], vec![]);
    let (to, _) = expand_reply_recipients(&original, true, None, vec![]);
    assert_eq!(emails(&to), vec!["x@x", "y@x"]);
}

#[test]
fn test_expand_reply_without_my_email_still_dedupes() {
    // Preview path when identity resolution fails: no "me" filter, but
    // dedup should still run.
    let original = reply_fixture(vec!["sender@x"], vec!["a@x", "a@x"], vec![]);
    let (to, _) = expand_reply_recipients(&original, true, None, vec![]);
    assert_eq!(emails(&to), vec!["sender@x", "a@x"]);
}

#[test]
fn test_expand_reply_preserves_to_order() {
    let original = reply_fixture(
        vec!["first@x"],
        vec!["second@x", "third@x"],
        vec!["fourth@x", "fifth@x"],
    );
    let (to, cc) = expand_reply_recipients(&original, true, None, vec![]);
    assert_eq!(emails(&to), vec!["first@x", "second@x", "third@x"]);
    assert_eq!(emails(&cc), vec!["fourth@x", "fifth@x"]);
}

#[test]
fn test_pick_identity_none_returns_first() {
    let identities = vec![
        test_identity("id1", "alice@example.com", "Alice"),
        test_identity("id2", "bob@example.com", "Bob"),
    ];
    let result = pick_identity(identities, None).unwrap();
    assert_eq!(result.email, "alice@example.com");
}

#[test]
fn test_pick_identity_none_empty_list() {
    let result = pick_identity(vec![], None);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Identity not found")
    );
}

#[test]
fn test_pick_identity_matches_exact() {
    let identities = vec![
        test_identity("id1", "alice@example.com", "Alice"),
        test_identity("id2", "bob@example.com", "Bob"),
    ];
    let result = pick_identity(identities, Some("bob@example.com")).unwrap();
    assert_eq!(result.id, "id2");
}

#[test]
fn test_pick_identity_case_insensitive() {
    let identities = vec![test_identity("id1", "Alice@Example.COM", "Alice")];
    let result = pick_identity(identities, Some("alice@example.com")).unwrap();
    assert_eq!(result.id, "id1");
}

#[test]
fn test_pick_identity_not_found() {
    let identities = vec![test_identity("id1", "alice@example.com", "Alice")];
    let result = pick_identity(identities, Some("nobody@example.com"));
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("nobody@example.com"));
    assert!(err.contains("list identities"));
}

// ============ Body structure tests ============

#[test]
fn test_body_structure_plain_text_only() {
    let mut email = HashMap::new();
    apply_body_structure(&mut email, "Hello world", None, &[]);

    // Should have textBody array, no htmlBody, no bodyStructure
    assert!(email.contains_key("textBody"));
    assert!(!email.contains_key("htmlBody"));
    assert!(!email.contains_key("bodyStructure"));

    let text_body = &email["textBody"];
    assert_eq!(text_body[0]["partId"], "textBody");
    assert_eq!(text_body[0]["type"], "text/plain");

    let body_values = &email["bodyValues"];
    assert_eq!(body_values["textBody"]["value"], "Hello world");
    assert_eq!(body_values["textBody"]["charset"], "utf-8");
}

#[test]
fn test_body_structure_text_plus_html() {
    let mut email = HashMap::new();
    apply_body_structure(&mut email, "fallback", Some("<h1>Rich</h1>"), &[]);

    // Should have both textBody and htmlBody arrays, no bodyStructure
    assert!(email.contains_key("textBody"));
    assert!(email.contains_key("htmlBody"));
    assert!(!email.contains_key("bodyStructure"));

    assert_eq!(email["textBody"][0]["partId"], "textBody");
    assert_eq!(email["htmlBody"][0]["partId"], "htmlBody");
    assert_eq!(email["htmlBody"][0]["type"], "text/html");

    let body_values = &email["bodyValues"];
    assert_eq!(body_values["textBody"]["value"], "fallback");
    assert_eq!(body_values["htmlBody"]["value"], "<h1>Rich</h1>");
}

#[test]
fn test_body_structure_text_with_attachment() {
    let mut email = HashMap::new();
    let attachments = vec![UploadedAttachment {
        blob_id: "Gblob123".into(),
        filename: "report.pdf".into(),
        content_type: "application/pdf".into(),
    }];
    apply_body_structure(&mut email, "See attached", None, &attachments);

    // Must use bodyStructure, NOT textBody/htmlBody
    assert!(email.contains_key("bodyStructure"));
    assert!(!email.contains_key("textBody"));
    assert!(!email.contains_key("htmlBody"));

    let structure = &email["bodyStructure"];
    assert_eq!(structure["type"], "multipart/mixed");

    let parts = structure["subParts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);

    // First part: plain text
    assert_eq!(parts[0]["partId"], "textBody");
    assert_eq!(parts[0]["type"], "text/plain");

    // Second part: attachment
    assert_eq!(parts[1]["blobId"], "Gblob123");
    assert_eq!(parts[1]["name"], "report.pdf");
    assert_eq!(parts[1]["type"], "application/pdf");
    assert_eq!(parts[1]["disposition"], "attachment");
}

#[test]
fn test_body_structure_html_with_attachment() {
    let mut email = HashMap::new();
    let attachments = vec![UploadedAttachment {
        blob_id: "Gblob456".into(),
        filename: "_DSF1117.jpg".into(),
        content_type: "image/jpeg".into(),
    }];
    apply_body_structure(
        &mut email,
        "Fallback text",
        Some("<h1>Photo</h1>"),
        &attachments,
    );

    assert!(email.contains_key("bodyStructure"));
    assert!(!email.contains_key("textBody"));
    assert!(!email.contains_key("htmlBody"));

    let structure = &email["bodyStructure"];
    assert_eq!(structure["type"], "multipart/mixed");

    let parts = structure["subParts"].as_array().unwrap();
    assert_eq!(parts.len(), 2);

    // First part: multipart/alternative with text + html
    assert_eq!(parts[0]["type"], "multipart/alternative");
    let alt_parts = parts[0]["subParts"].as_array().unwrap();
    assert_eq!(alt_parts.len(), 2);
    assert_eq!(alt_parts[0]["partId"], "textBody");
    assert_eq!(alt_parts[1]["partId"], "htmlBody");

    // Second part: attachment
    assert_eq!(parts[1]["blobId"], "Gblob456");
    assert_eq!(parts[1]["name"], "_DSF1117.jpg");

    // bodyValues should have both text and html
    let bv = &email["bodyValues"];
    assert_eq!(bv["textBody"]["value"], "Fallback text");
    assert_eq!(bv["htmlBody"]["value"], "<h1>Photo</h1>");
}

#[test]
fn test_body_structure_multiple_attachments() {
    let mut email = HashMap::new();
    let attachments = vec![
        UploadedAttachment {
            blob_id: "Ga".into(),
            filename: "a.pdf".into(),
            content_type: "application/pdf".into(),
        },
        UploadedAttachment {
            blob_id: "Gb".into(),
            filename: "b.xlsx".into(),
            content_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                .into(),
        },
    ];
    apply_body_structure(&mut email, "docs attached", None, &attachments);

    let parts = email["bodyStructure"]["subParts"].as_array().unwrap();
    assert_eq!(parts.len(), 3); // text + 2 attachments
    assert_eq!(parts[1]["blobId"], "Ga");
    assert_eq!(parts[2]["blobId"], "Gb");
}

// ============ upload_blob mock test ============

/// Build a client pointed at a mock JMAP server.
pub(super) fn mock_client(uri: &str) -> JmapClient {
    let mut client = JmapClient::try_new("test-token".to_string()).unwrap();
    let mut session = create_test_session(vec![
        "urn:ietf:params:jmap:core",
        "urn:ietf:params:jmap:mail",
    ]);
    session.api_url = format!("{uri}/jmap");
    client.available_capabilities = session.capabilities.keys().cloned().collect();
    client.session = Some(session);
    client
}

/// One `methodResponses` envelope around a single method result.
pub(super) fn jmap_response(method: &str, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "methodResponses": [[method, result, "c0"]] })
}

#[tokio::test]
async fn email_thread_and_query_fetches_respect_the_session_object_limit() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(|request: &wiremock::Request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let calls = body["methodCalls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        let result = if call[0] == "Email/query" {
            json!({"ids": ["e0", "e1", "e2", "e3", "e4"], "position": 0, "queryState":"q1"})
        } else {
            let ids = call[1]["ids"].as_array().unwrap();
            assert!(ids.len() <= 2);
            json!({"list": ids.iter().map(|id| json!({"id":id, "threadId":"t0", "emailIds":["e0", "e1", "e2", "e3", "e4"]})).collect::<Vec<_>>(), "notFound": []})
        };
        ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[[call[0], result, call[2]]]}))
    }).mount(&server).await;
    let mut client = mock_client(&server.uri());
    client.session.as_mut().unwrap().capabilities.insert(
        "urn:ietf:params:jmap:core".into(),
        json!({"maxObjectsInGet":2}),
    );
    let ids = (0..5).map(|n| format!("e{n}")).collect::<Vec<_>>();
    assert_eq!(client.get_emails(&ids).await.unwrap().len(), 5);
    assert_eq!(client.get_email_summaries(&ids).await.unwrap().len(), 5);
    assert_eq!(client.thread_email_ids(&ids).await.unwrap().len(), 5);
    assert_eq!(client.get_thread("e0").await.unwrap().len(), 5);
    let page = client.query_emails(EmailQuery::first(5)).await.unwrap();
    assert_eq!(
        page.emails.iter().map(|e| &e.id).collect::<Vec<_>>(),
        ids.iter().collect::<Vec<_>>()
    );
}

#[test]
fn missing_or_invalid_object_limit_uses_a_nonzero_fallback() {
    let mut client = mock_client("https://example.test");
    for value in [Value::Null, json!(0), json!("invalid")] {
        client.session.as_mut().unwrap().capabilities.insert(
            "urn:ietf:params:jmap:core".into(),
            json!({"maxObjectsInGet":value}),
        );
        assert_eq!(client.max_objects_in_get(), 100);
    }
}

#[tokio::test]
async fn mailbox_resolution_refreshes_names_and_roles() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
            "Mailbox/get",
            json!({"list":[{"id":"new", "name":"Renamed", "role":"sent"}]}),
        )))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
            "Mailbox/get",
            json!({"list":[{"id":"old", "name":"Original", "role":"sent"}]}),
        )))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    let mut client = mock_client(&server.uri());
    assert_eq!(client.find_mailbox("sent").await.unwrap().id, "old");
    assert_eq!(client.find_mailbox("sent").await.unwrap().id, "new");
    assert_eq!(client.find_mailbox("renamed").await.unwrap().id, "new");
}

#[tokio::test]
async fn failed_submission_leaves_a_draft_not_a_sent_message() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(|request: &wiremock::Request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let calls = body["methodCalls"].as_array().unwrap();
        let response = match calls[0][0].as_str().unwrap() {
            "Mailbox/get" => jmap_response("Mailbox/get", json!({"list":[
                {"id":"drafts", "name":"Drafts", "role":"drafts"},
                {"id":"sent", "name":"Sent", "role":"sent"}
            ]})),
            "Identity/get" => jmap_response("Identity/get", json!({"list":[{"id":"i0", "name":"Sender", "email":"sender@example.test"}]})),
            "Email/set" => {
                let email = &calls[0][1]["create"]["email"];
                assert_eq!(email["mailboxIds"], json!({"drafts":true}));
                assert_eq!(email["keywords"]["$draft"], true);
                let update = &calls[1][1]["onSuccessUpdateEmail"]["#submission"];
                assert_eq!(update["mailboxIds"], json!({"sent":true}));
                assert_eq!(update["keywords/$draft"], Value::Null);
                json!({"methodResponses":[
                    ["Email/set", {"created":{"email":{"id":"draft-1"}}}, "e0"],
                    ["EmailSubmission/set", {"notCreated":{"submission":{"type":"forbidden", "description":"Not sent"}}}, "s0"]
                ]})
            }
            name => panic!("Unexpected method {name}"),
        };
        ResponseTemplate::new(200).set_body_json(response)
    }).mount(&server).await;
    let mut client = mock_client(&server.uri());
    client
        .available_capabilities
        .push("urn:ietf:params:jmap:submission".into());
    client
        .session
        .as_mut()
        .unwrap()
        .capabilities
        .insert("urn:ietf:params:jmap:submission".into(), json!({}));
    let error = client
        .send_email(
            vec![EmailAddress {
                email: "to@example.test".into(),
                name: None,
            }],
            "Subject",
            "Body",
            None,
            ComposeParams {
                cc: vec![],
                bcc: vec![],
                from: None,
                draft: false,
                html_body: None,
                attachments: vec![],
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Not sent"), "{error}");
}

#[tokio::test]
async fn test_email_state_reads_state_without_fetching_mail() {
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(body_string_contains(r#""ids":[]"#))
        .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
            "Email/get",
            serde_json::json!({ "state": "state-42", "list": [], "notFound": [] }),
        )))
        .mount(&mock_server)
        .await;

    let client = mock_client(&mock_server.uri());
    assert_eq!(client.email_state().await.unwrap(), "state-42");
}

#[tokio::test]
async fn test_email_changes_follows_has_more_changes() {
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // Each page is matched by the state it was asked to start from, so the
    // test asserts the cursor actually advances rather than trusting order.
    Mock::given(method("POST"))
        .and(body_string_contains(r#""sinceState":"s0""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
            "Email/changes",
            serde_json::json!({
                "oldState": "s0",
                "newState": "s1",
                "hasMoreChanges": true,
                "created": ["e1", "e2"],
                "updated": [],
                "destroyed": []
            }),
        )))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(body_string_contains(r#""sinceState":"s1""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
            "Email/changes",
            serde_json::json!({
                "oldState": "s1",
                "newState": "s2",
                "hasMoreChanges": false,
                "created": ["e3"],
                "updated": [],
                "destroyed": []
            }),
        )))
        .mount(&mock_server)
        .await;

    let client = mock_client(&mock_server.uri());
    let changes = client.email_changes("s0").await.unwrap();

    assert_eq!(changes.created, vec!["e1", "e2", "e3"]);
    assert_eq!(changes.new_state, "s2");
}

#[tokio::test]
async fn test_email_changes_surfaces_cannot_calculate_changes() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
            "error",
            serde_json::json!({ "type": "cannotCalculateChanges" }),
        )))
        .mount(&mock_server)
        .await;

    let client = mock_client(&mock_server.uri());
    let err = client.email_changes("ancient").await.unwrap_err();

    // The watcher keys its resync off this type, so it has to survive the
    // trip through parse_response intact.
    assert!(
        matches!(&err, Error::Jmap { error_type, .. } if error_type == "cannotCalculateChanges"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_get_email_summaries_skips_body_values() {
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // Bodies are the expensive half of Email/get; a watcher fetching them
    // by default would make every arrival cost a full document parse.
    Mock::given(method("POST"))
        .and(body_string_contains(r#""fetchTextBodyValues":false"#))
        .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
            "Email/get",
            serde_json::json!({
                "state": "s1",
                "list": [{ "id": "e1", "subject": "hi" }],
                "notFound": []
            }),
        )))
        .mount(&mock_server)
        .await;

    let client = mock_client(&mock_server.uri());
    let emails = client
        .get_email_summaries(&["e1".to_string()])
        .await
        .unwrap();

    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject.as_deref(), Some("hi"));
}

#[tokio::test]
async fn test_open_event_stream_fills_the_url_template() {
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // The session hands back a URI template; getting any placeholder wrong
    // fails silently as "push just never fires".
    Mock::given(method("GET"))
        .and(path("/jmap/event-source/"))
        .and(query_param("types", "Email"))
        .and(query_param("closeafter", "no"))
        .and(query_param("ping", "30"))
        .and(header("Authorization", "Bearer test-token"))
        .and(header("Last-Event-ID", "evt-7"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(": ping\n\n", "text/event-stream"))
        .mount(&mock_server)
        .await;

    let mut client = mock_client(&mock_server.uri());
    client.session.as_mut().unwrap().event_source_url = Some(format!(
        "{}/jmap/event-source/?types={{types}}&closeafter={{closeafter}}&ping={{ping}}",
        mock_server.uri()
    ));

    let resp = client.open_event_stream(30, Some("evt-7")).await.unwrap();
    assert!(resp.status().is_success());
}

#[tokio::test]
async fn test_open_event_stream_without_a_url_points_at_poll() {
    // create_test_session advertises no eventSourceUrl, standing in for a
    // server that does not offer push.
    let client = mock_client("https://api.example.com");
    let err = client.open_event_stream(30, None).await.unwrap_err();
    assert!(err.to_string().contains("--poll"), "unhelpful error: {err}");
}

#[tokio::test]
async fn test_upload_blob_success() {
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // Mock the upload endpoint — matches what Fastmail returns
    Mock::given(method("POST"))
        .and(header("Content-Type", "image/jpeg"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "test-account",
            "blobId": "G31e09448268297247a1b215a4ce1e7bc7ee05699",
            "expires": "2026-04-12T15:35:44Z",
            "size": 958081,
            "type": "image/jpeg"
        })))
        .mount(&mock_server)
        .await;

    let mut client = JmapClient::try_new("test-token".to_string()).unwrap();
    let mut session = create_test_session(vec!["urn:ietf:params:jmap:core"]);
    session.upload_url = format!("{}/upload/{{accountId}}/", mock_server.uri());
    client.session = Some(session);

    let blob_id = client
        .upload_blob(b"fake image data".to_vec(), "image/jpeg")
        .await
        .unwrap();
    assert_eq!(blob_id, "G31e09448268297247a1b215a4ce1e7bc7ee05699");
}

#[tokio::test]
async fn test_upload_blob_413_too_large() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(413).set_body_string("Request Entity Too Large"))
        .mount(&mock_server)
        .await;

    let mut client = JmapClient::try_new("test-token".to_string()).unwrap();
    let mut session = create_test_session(vec!["urn:ietf:params:jmap:core"]);
    session.upload_url = format!("{}/upload/{{accountId}}/", mock_server.uri());
    client.session = Some(session);

    let result = client
        .upload_blob(b"huge file".to_vec(), "application/pdf")
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("413"));
    assert!(err.contains("Too Large"));
}

#[tokio::test]
async fn test_upload_blob_rate_limited() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&mock_server)
        .await;

    let mut client = JmapClient::try_new("test-token".to_string()).unwrap();
    let mut session = create_test_session(vec!["urn:ietf:params:jmap:core"]);
    session.upload_url = format!("{}/upload/{{accountId}}/", mock_server.uri());
    client.session = Some(session);

    let result = client.upload_blob(b"data".to_vec(), "text/plain").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Rate limited"));
}
