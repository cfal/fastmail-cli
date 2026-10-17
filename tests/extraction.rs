use fastmail_cli::util::extract_text;

#[path = "support/extraction.rs"]
mod fixtures;

#[tokio::test]
async fn office_documents_still_extract_after_backend_migration() {
    for (name, bytes) in fixtures::office_documents() {
        let text = extract_text(&bytes, name).await.unwrap().unwrap();
        assert!(text.contains("Hello attachment"), "{name}: {text}");
    }
}

#[tokio::test]
async fn pdf_text_extracts_without_a_native_library() {
    let text = extract_text(&fixtures::simple_pdf(), "test.pdf")
        .await
        .unwrap()
        .unwrap();
    assert!(text.contains("Hello attachment"), "{text}");
}

#[tokio::test]
async fn document_extraction_preserves_supported_formats() {
    for (name, input) in fixtures::text_documents() {
        let text = extract_text(input.as_bytes(), name).await.unwrap().unwrap();
        assert!(text.contains("Hello attachment"), "{name}: {text}");
    }
    assert!(
        extract_text(b"not a PDF", "broken.pdf")
            .await
            .unwrap()
            .is_none()
    );
    assert!(extract_text(b"image", "photo.png").await.unwrap().is_none());
}
