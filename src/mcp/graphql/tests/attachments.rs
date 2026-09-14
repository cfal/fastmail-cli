use super::*;

#[tokio::test]
async fn attachment_lists_skip_parts_without_blob_ids_and_missing_blob_is_null() {
    use wiremock::matchers::body_string_contains;
    let server = mock_server(1).await;
    Mock::given(body_string_contains("Email/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": [["Email/get", {"list": [{"id": "e0", "attachments": [
                {"name": "unavailable.txt"},
                {"blobId": "blob0", "name": "notes.txt", "type": "text/plain", "size": 18}
            ]}]}, "0"]]
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
