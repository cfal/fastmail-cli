use super::super::attachment_scope::{AttachmentScopes, base64_size};
use super::*;

#[tokio::test]
async fn attachment_aliases_share_input_but_each_consume_output_budget() {
    let server = mock_server(1).await;
    let size = base64_size(ATTACHMENT_BYTES.len());
    for (fields, output, accepted) in [
        ("a:base64 b:base64", 2 * size, true),
        ("a:base64 b:base64 c:base64", 2 * size, false),
        ("text", 3, false),
    ] {
        let query = format!("{{ attachments(emailId:\"e0\") {{ nodes {{ {fields} }} }} }}");
        let response = build_schema()
            .execute(
                request(&query, client_for(&server), CardDavCreds::default()).data(
                    AttachmentScopes::with_limits(ATTACHMENT_BYTES.len(), output, 1),
                ),
            )
            .await;
        assert_eq!(
            response.errors.is_empty(),
            accepted,
            "{fields}: {:?}",
            response.errors
        );
        if !accepted {
            assert!(
                response
                    .errors
                    .iter()
                    .any(|e| e.message.contains("budget") || e.message.contains("too large")),
                "{:?}",
                response.errors
            );
        }
    }
    assert_eq!(downloads(&server).await.len(), 3);
}

#[tokio::test]
async fn attachment_scope_caches_download_failures_and_limits_unique_blobs() {
    let server = mock_server(1).await;
    let client = client_for(&server);
    let scopes = AttachmentScopes::with_limits(ATTACHMENT_BYTES.len() - 1, 100, 1);
    let scope = scopes.current();
    let first = scope.bytes(&client, "blob-att-e0").await.unwrap_err();
    let again = scope.bytes(&client, "blob-att-e0").await.unwrap_err();
    assert!(Arc::ptr_eq(&first, &again));
    assert!(first.to_string().contains("download budget"));
    assert!(
        scope
            .bytes(&client, "another")
            .await
            .unwrap_err()
            .to_string()
            .contains("unique blob")
    );
    assert_eq!(downloads(&server).await.len(), 1);
}

#[tokio::test]
async fn concurrent_blob_downloads_cannot_cross_the_aggregate_limit() {
    let server = mock_server(2).await;
    let client = client_for(&server);
    let scopes = AttachmentScopes::with_limits(ATTACHMENT_BYTES.len(), 100, 2);
    let scope = scopes.current();
    let (a, b) = tokio::join!(
        scope.bytes(&client, "blob-att-e0"),
        scope.bytes(&client, "blob-att-e1")
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(downloads(&server).await.len(), 2);
}

#[tokio::test]
async fn attachment_download_slots_bound_overlapping_calls_without_holding_client_lock() {
    use async_graphql::futures_util::{future::join_all, stream};
    use axum::{Router, body::Body, routing::get};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let router = Router::new().route(
        "/jmap/download/{id}",
        get({
            let peak = peak.clone();
            let release = release.clone();
            move || {
                let (active, peak, release, started) = (
                    active.clone(),
                    peak.clone(),
                    release.clone(),
                    started.clone(),
                );
                async move {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    started.send(()).unwrap();
                    release.acquire().await.unwrap().forget();
                    active.fetch_sub(1, Ordering::SeqCst);
                    Body::from_stream(stream::iter([Ok::<_, std::io::Error>("ab"), Ok("cd")]))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let uri = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = Arc::new(tokio::sync::Mutex::new(JmapClient::with_test_session(
        &format!("{uri}/jmap"),
    )));
    let scopes = AttachmentScopes::with_limits(24, 100, 6);
    let scope = scopes.current();
    let jobs = tokio::spawn({
        let client = client.clone();
        async move {
            join_all((0..6).map(|id| {
                let (scope, client) = (scope.clone(), client.clone());
                async move { scope.bytes(&client, &format!("blob-{id}")).await }
            }))
            .await
        }
    });
    for _ in 0..4 {
        tokio::time::timeout(std::time::Duration::from_secs(5), starts.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(starts.try_recv().is_err());
    drop(
        client
            .try_lock()
            .expect("downloads must release the account client lock"),
    );
    release.add_permits(6);
    let results = tokio::time::timeout(std::time::Duration::from_secs(5), jobs)
        .await
        .unwrap()
        .unwrap();
    assert!(results.iter().all(|result| {
        result
            .as_ref()
            .is_ok_and(|bytes| bytes.as_slice() == b"abcd")
    }));
    assert_eq!(peak.load(Ordering::SeqCst), 4);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn repeated_blobs_have_unique_occurrence_cursors_and_legacy_ambiguity_is_explicit() {
    use std::collections::HashSet;
    use wiremock::matchers::body_string_contains;
    let server = mock_server(1).await;
    Mock::given(body_string_contains("Email/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[[
            "Email/get", {"accountId":"acct1","state":"s1","notFound":[],"list":[{"id":"e0","attachments":[
                {"blobId":"same","partId":"part1","name":"one"},
                {"blobId":"same","partId":"part2","name":"two"},
                {"blobId":"same","name":"three"},
                {"blobId":"unique","name":"four"},
                {"blobId":"same","name":"five"}
            ]}]}, "g0"
        ]]}))).with_priority(1).mount(&server).await;
    for nested in [false, true] {
        let query = |args: &str| {
            let field = if nested {
                format!("attachments({args})")
            } else {
                format!("attachments(emailId:\"e0\", {args})")
            };
            let selection = format!(
                "{field} {{ edges {{ cursor node {{ name }} }} pageInfo {{ hasNextPage hasPreviousPage }} }}"
            );
            if nested {
                format!("{{ email(id:\"e0\") {{ {selection} }} }}")
            } else {
                format!("{{ {selection} }}")
            }
        };
        let mut after = Value::Null;
        let mut cursors = Vec::new();
        for (index, expected) in ["one", "two", "three", "four", "five"]
            .into_iter()
            .enumerate()
        {
            let response = run(&server, &query(&format!("first:1,after:{after}"))).await;
            assert!(response.errors.is_empty(), "{:?}", response.errors);
            let data = response.data.into_json().unwrap();
            let data = if nested { &data["email"] } else { &data };
            let page = &data["attachments"];
            assert_eq!(page["edges"].as_array().unwrap().len(), 1);
            assert_eq!(page["edges"][0]["node"]["name"], expected);
            assert_eq!(page["pageInfo"]["hasNextPage"], index < 4);
            after = page["edges"][0]["cursor"].clone();
            cursors.push(after.as_str().unwrap().to_owned());
        }
        assert_eq!(cursors.iter().collect::<HashSet<_>>().len(), 5);
        let response = run(&server, &query("first:1, after:\"same\"")).await;
        assert_eq!(
            response.errors[0].message,
            "Ambiguous legacy attachment cursor. Restart pagination to obtain per-part cursors."
        );
        for (arguments, expected) in [
            ("first:1, after:\"unique\"".to_owned(), "five"),
            (format!("last:1, before:{}", json!(cursors[3])), "three"),
            ("last:1, before:\"unique\"".to_owned(), "three"),
        ] {
            let response = run(&server, &query(&arguments)).await;
            assert!(response.errors.is_empty(), "{:?}", response.errors);
            let data = response.data.into_json().unwrap();
            let data = if nested { &data["email"] } else { &data };
            assert_eq!(data["attachments"]["edges"][0]["node"]["name"], expected);
        }
    }
    assert!(downloads(&server).await.is_empty());
}

#[tokio::test]
async fn attachment_lists_skip_parts_without_blob_ids_and_missing_blob_is_null() {
    use wiremock::matchers::body_string_contains;
    let server = mock_server(1).await;
    Mock::given(body_string_contains("Email/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": [["Email/get", {"accountId":"acct1","state":"state","notFound":[],"list": [{"id": "e0", "attachments": [
                {"name": "unavailable.txt"},
                {"blobId": "blob0", "name": "notes.txt", "type": "text/plain", "size": 18}
            ]}]}, "g0"]]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let response = run(
        &server,
        r#"{
        attachments(emailId: "e0") { totalCount nodes { blobId name } }
        attachment(emailId: "e0", blobId: "missing") { blobId }
    }"#,
    )
    .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(
        response.data.into_json().unwrap(),
        json!({
            "attachments": {"totalCount": 1, "nodes": [{"blobId": "blob0", "name": "notes.txt"}]},
            "attachment": null
        })
    );
    assert!(downloads(&server).await.is_empty());
}

#[tokio::test]
async fn attachment_metadata_downloads_nothing() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ emails { nodes { attachments { nodes { \
            blobId name contentType size disposition cid charset partId } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert!(
        downloads(&server).await.is_empty(),
        "attachment metadata comes with the email — it must not download blobs"
    );
}

#[tokio::test]
async fn base64_downloads_once_per_attachment() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ emails { nodes { attachments { nodes { base64 } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert_eq!(
        downloads(&server).await.len(),
        3,
        "one download per attachment, no more"
    );
}

#[tokio::test]
async fn siblings_needing_the_same_blob_download_it_once() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ emails(first: 1) { nodes { attachments { nodes { base64 text image } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert_eq!(
        downloads(&server).await.len(),
        1,
        "three payload fields on one attachment share a single download"
    );
}

#[tokio::test]
async fn payload_fields_resolve_independently() {
    // Selecting the cheap payload fields must not drag `text` along.
    //
    // Extraction is unobservable from outside once the blob is downloaded — it
    // returns Ok(None) rather than failing on input it cannot parse — so the
    // guarantee is structural: `crate::util::extract_text` has exactly one call
    // site in this crate's GraphQL layer, inside the `text` resolver. The
    // observable half is `attachment_metadata_downloads_nothing`: no download
    // means no bytes, and no bytes means nothing was extracted.
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ emails(first: 1) { nodes { attachments { nodes { name size base64 image } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let att = &data["emails"]["nodes"][0]["attachments"]["nodes"][0];
    assert!(att.get("text").is_none(), "text was not selected");
    assert!(
        att["base64"].is_string(),
        "base64 should resolve for any type"
    );
    assert!(
        att["image"].is_null(),
        "a text/plain attachment is not an image"
    );
}

#[tokio::test]
async fn text_extracts_document_content() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ emails(first: 1) { nodes { attachments { nodes { text } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let text = data["emails"]["nodes"][0]["attachments"]["nodes"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        text.contains("attachment payload"),
        "expected the extracted body, got {text:?}"
    );
}

#[tokio::test]
async fn bounded_attachment_queries_remain_usable() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ emails(first: 25) { nodes { attachments { nodes { text base64 image } } } } }",
    )
    .await;
    assert!(
        resp.errors.is_empty(),
        "ordinary attachment queries should remain usable: {:?}",
        resp.errors
    );
}
