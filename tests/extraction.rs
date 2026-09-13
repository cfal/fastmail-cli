use fastmail_cli::util::extract_text;

fn simple_pdf() -> Vec<u8> {
    let content = "BT /F1 24 Tf 72 720 Td (Hello attachment) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
    }
    let xref = pdf.len();
    pdf.push_str("xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    ));
    pdf.into_bytes()
}

#[tokio::test]
async fn pdf_text_extracts_without_a_native_library() {
    let text = extract_text(&simple_pdf(), "test.pdf")
        .await
        .unwrap()
        .unwrap();
    assert!(text.contains("Hello attachment"), "{text}");
}

#[tokio::test]
async fn document_extraction_preserves_supported_formats() {
    for (name, input) in [
        ("test.txt", "Hello attachment"),
        (
            "test.html",
            "<html><body><p>Hello attachment</p></body></html>",
        ),
        (
            "test.xml",
            "<?xml version=\"1.0\"?><document>Hello attachment</document>",
        ),
        (
            "test.eml",
            "From: test@example.com\r\nTo: other@example.com\r\nSubject: Test\r\nContent-Type: text/plain\r\n\r\nHello attachment\r\n",
        ),
        ("test.rtf", "{\\rtf1\\ansi Hello attachment}"),
    ] {
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
