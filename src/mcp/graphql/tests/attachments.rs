use super::*;

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
