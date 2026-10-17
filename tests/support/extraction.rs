use std::io::{Cursor, Write};

fn office_archive(files: &[(&str, &str)], compression: zip::CompressionMethod) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        archive
            .start_file(
                *name,
                zip::write::SimpleFileOptions::default().compression_method(compression),
            )
            .unwrap();
        archive.write_all(content.as_bytes()).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

pub fn office_documents() -> Vec<(&'static str, Vec<u8>)> {
    let docx = &[
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        ),
        (
            "word/document.xml",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello attachment</w:t></w:r></w:p></w:body></w:document>"#,
        ),
    ];
    let xlsx = &[
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Hello attachment</t></is></c></row></sheetData></worksheet>"#,
        ),
    ];
    let mut documents = Vec::new();
    for compression in [
        zip::CompressionMethod::Stored,
        zip::CompressionMethod::Deflated,
    ] {
        documents.push(("test.docx", office_archive(docx, compression)));
        documents.push(("test.xlsx", office_archive(xlsx, compression)));
    }
    documents
}

pub fn simple_pdf() -> Vec<u8> {
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

pub fn text_documents() -> [(&'static str, &'static str); 6] {
    [
        ("test.txt", "Hello attachment"),
        ("test.yaml", "message: Hello attachment\n"),
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
    ]
}
