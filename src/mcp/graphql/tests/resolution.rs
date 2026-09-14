use super::*;

#[tokio::test]
async fn missing_emails_distinguish_nullable_lookups_from_required_records() {
    use wiremock::matchers::body_string_contains;
    let server = mock_server(0).await;
    Mock::given(method("POST"))
        .and(path("/jmap"))
        .and(body_string_contains("Email/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": [["Email/get", {"list": [], "notFound": ["missing"]}, "0"]]
        })))
        .with_priority(1)
        .expect(4)
        .mount(&server)
        .await;

    let response = run(
        &server,
        r#"{ email(id: "missing") { id } mailbox(name: "missing") { id } }"#,
    )
    .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(
        response.data.into_json().unwrap(),
        json!({"email": null, "mailbox": null})
    );

    for query in [
        r#"{ thread(emailId: "missing") { id } }"#,
        r#"{ attachments(emailId: "missing") { nodes { blobId } } }"#,
        r#"{ attachment(emailId: "missing", blobId: "blob") { blobId } }"#,
    ] {
        let response = run(&server, query).await;
        assert_eq!(response.errors.len(), 1, "{query}: {:?}", response.errors);
        assert_eq!(
            response.errors[0].message, "Email missing not found",
            "{query}"
        );
    }
    assert!(downloads(&server).await.is_empty());
}

#[tokio::test]
async fn a_deleted_email_during_lazy_loading_reports_an_actionable_error() {
    use wiremock::matchers::body_string_contains;
    let server = mock_server(1).await;
    Mock::given(body_string_contains("textBody"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": [["Email/get", {"list": [], "notFound": ["e0"]}, "0"]]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let response = run(&server, "{ emails { nodes { id textBody } } }").await;
    assert_eq!(response.errors.len(), 1, "{:?}", response.errors);
    assert_eq!(response.errors[0].message, "Email e0 no longer exists");
    assert!(downloads(&server).await.is_empty());
}

#[tokio::test]
async fn threads_without_a_thread_id_report_the_specific_missing_field() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/jmap"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": [["Email/get", {"list": [{"id": "e0"}]}, "0"]]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let response = run(&server, r#"{ thread(emailId: "e0") { id } }"#).await;
    assert_eq!(response.errors.len(), 1, "{:?}", response.errors);
    assert_eq!(response.errors[0].message, "Email has no thread ID");
}

#[tokio::test]
async fn listing_without_bodies_makes_no_extra_fetch() {
    let server = mock_server(5).await;
    let resp = run(&server, "{ emails { nodes { id subject } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let calls = calls(&server).await;
    let gets = calls.iter().filter(|c| c.method == "Email/get").count();
    assert_eq!(
        gets, 1,
        "header-only selection should not trigger the detail fetch"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.method == "Email/get" && c.properties.contains(&"textBody".to_string())),
        "list fetch must not ask for bodies"
    );
}

#[tokio::test]
async fn bodies_across_a_list_collapse_into_one_batched_fetch() {
    let server = mock_server(5).await;
    let resp = run(&server, "{ emails { nodes { id textBody } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let calls = calls(&server).await;
    let detail: Vec<&Call> = calls
        .iter()
        .filter(|c| c.method == "Email/get" && c.properties.contains(&"textBody".to_string()))
        .collect();

    // The N+1 shape would be 5 detail calls of 1 id each.
    assert_eq!(detail.len(), 1, "expected a single batched detail fetch");
    assert_eq!(detail[0].ids.len(), 5, "batch should cover the whole page");
}

#[tokio::test]
async fn body_and_attachments_together_share_one_fetch() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ emails { nodes { textBody htmlBody attachments { nodes { name } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let detail = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get" && c.properties.contains(&"textBody".to_string()))
        .count();
    assert_eq!(
        detail, 1,
        "three body/attachment fields over three emails is still one fetch"
    );
}

#[tokio::test]
async fn every_mailbox_path_shares_one_fetch() {
    // The list, the tree, a name lookup, a filter naming a folder, and the
    // folders an email belongs to — five routes to the same data, one call.
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ mailboxes { nodes { name parent { name } children { nodes { name } } } } \
           mailbox(name: \"INBOX\") { name } \
           emails(filter: { inMailbox: \"Inbox\" }, first: 3) { \
             nodes { mailboxes { name role } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let fetches = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Mailbox/get")
        .count();
    assert_eq!(fetches, 1, "every mailbox path must share one Mailbox/get");
}

#[tokio::test]
async fn mailboxes_are_refetched_for_each_request() {
    // Clients are pooled per token for the life of the process, so caching the
    // mailbox list on the client would mean a folder created after start-up
    // never appears. The loader's cache is per request; this pins that.
    let server = mock_server(1).await;
    let client = client_for(&server);
    let schema = build_schema();

    for _ in 0..2 {
        let resp = schema
            .execute(request(
                "{ mailboxes { nodes { name } } }",
                client.clone(),
                CardDavCreds::default(),
            ))
            .await;
        assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    }

    let fetches = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Mailbox/get")
        .count();
    assert_eq!(
        fetches, 2,
        "a second request on a pooled client must see fresh mailboxes"
    );
}

#[tokio::test]
async fn newly_exposed_fields_come_back() {
    // Fields the API was already returning, or that cost one more property on
    // a call we make anyway. `sender` rides the summary set; `headers` is on
    // the full fetch, so it resolves lazily like the bodies.
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ mailboxes { nodes { isSubscribed myRights { mayAddItems mayDelete } } } \
           emails(first: 1) { nodes { \
             sender { name email } \
             headers { name value } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let mb = &data["mailboxes"]["nodes"][0];
    assert_eq!(mb["isSubscribed"], true);
    assert_eq!(mb["myRights"]["mayAddItems"], true);
    assert_eq!(mb["myRights"]["mayDelete"], false);

    let email = &data["emails"]["nodes"][0];
    assert_eq!(email["sender"][0]["email"], "agent@example.com");
    let names: Vec<&str> = email["headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"List-Unsubscribe"), "got {names:?}");
}

#[tokio::test]
async fn sender_needs_no_extra_fetch() {
    // It is in the summary property set, so a list already has it.
    let server = mock_server(3).await;
    let resp = run(&server, "{ emails { nodes { sender { email } } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let detail = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get" && c.properties.contains(&"textBody".to_string()))
        .count();
    assert_eq!(detail, 0, "`sender` must not trigger the full fetch");
}

#[tokio::test]
async fn repeated_email_ids_are_deduplicated() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ a: email(id: \"e0\") { subject } b: email(id: \"e0\") { textBody } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let gets = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get")
        .count();
    assert_eq!(gets, 1, "same id twice in one query is one fetch");
}

#[tokio::test]
async fn mailbox_nests_into_emails_and_back() {
    let server = mock_server(2).await;
    let resp = run(
        &server,
        "{ mailbox(name: \"INBOX\") { name emails(first: 2) { nodes { subject mailboxes { name role } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let mailbox = &data["mailbox"];
    assert_eq!(mailbox["name"], "Inbox");
    assert_eq!(mailbox["emails"]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        mailbox["emails"]["nodes"][0]["mailboxes"][0]["role"],
        "inbox"
    );

    // The mailbox loader plus the client's own cache mean the whole query costs
    // one Mailbox/get, however many emails reference it.
    let mailbox_gets = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Mailbox/get")
        .count();
    assert_eq!(mailbox_gets, 1);
}

#[tokio::test]
async fn email_nests_into_its_thread() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ email(id: \"e0\") { subject thread { total emails { nodes { id textBody } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    assert_eq!(data["email"]["thread"]["total"], 3);
    assert_eq!(
        data["email"]["thread"]["emails"]["nodes"][0]["textBody"],
        "Body of e0"
    );

    let calls = calls(&server).await;
    assert_eq!(
        calls.iter().filter(|c| c.method == "Thread/get").count(),
        1,
        "one Thread/get for the conversation"
    );
    // e0 was already loaded for the outer selection, so the thread fetch only
    // needs the two it doesn't have.
    let thread_fetch = calls
        .iter()
        .rfind(|c| c.method == "Email/get")
        .expect("an Email/get");
    assert!(
        !thread_fetch.ids.contains(&"e0".to_string()),
        "already-cached email should not be refetched, got {:?}",
        thread_fetch.ids
    );
}
