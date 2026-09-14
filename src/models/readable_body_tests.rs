use super::*;
use serde_json::json;

fn email(mime: &str, content: &str) -> Email {
    serde_json::from_value(json!({
        "id": "test", "textBody": [{"partId": "1", "type": mime}],
        "bodyValues": {"1": {"value": content}}
    }))
    .unwrap()
}

#[test]
fn readable_body_preserves_genuine_plain_text_and_raw_fields() {
    let message = email(
        "text/plain",
        "Literal <b>not HTML</b>\n![image](https://example.test/a)\n",
    );
    let before = serde_json::to_value(&message).unwrap();
    let body = message.readable_body(BodyPreference::Auto).unwrap();
    assert_eq!(body.format, ReadableBodyFormat::Text);
    assert_eq!(body.content, message.text_content().unwrap());
    assert!(!body.is_truncated);
    assert!(!body.is_encoding_problem);
    assert!(body.warnings.is_empty());
    assert_eq!(serde_json::to_value(&message).unwrap(), before);

    let markdown = message.readable_body(BodyPreference::Markdown).unwrap();
    assert_eq!(markdown.format, ReadableBodyFormat::Markdown);
    let events: Vec<_> = pulldown_cmark::Parser::new(&markdown.content).collect();
    assert!(matches!(
        events.first(),
        Some(pulldown_cmark::Event::Start(
            pulldown_cmark::Tag::CodeBlock(_)
        ))
    ));
    let literal: String = events
        .iter()
        .filter_map(|event| match event {
            pulldown_cmark::Event::Text(text) => Some(text.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(literal, message.text_content().unwrap());
}

#[test]
fn readable_body_preserves_plaintext_layout_in_markdown() {
    let content = "Hard-wrapped\nmessage\n\n    | total | $42 |\n\t* literal bullet\n```\n~~~~\n[link](https://example.test)\n";
    let body = email("text/plain", content)
        .readable_body(BodyPreference::Markdown)
        .unwrap();
    let events: Vec<_> = pulldown_cmark::Parser::new(&body.content).collect();
    assert_eq!(events.len(), 3, "{events:?}");
    assert!(matches!(
        events[0],
        pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(_))
    ));
    assert_eq!(events[1], pulldown_cmark::Event::Text(content.into()));
}

#[test]
fn readable_body_accepts_mime_parameters_without_changing_provenance() {
    for mime in ["text/html; charset=utf-8", " TEXT/HTML ; charset=\"utf-8\""] {
        let body = email(mime, "<p>Message</p>")
            .readable_body(BodyPreference::Auto)
            .unwrap();
        assert_eq!(body.content, "Message");
        assert_eq!(body.format, ReadableBodyFormat::Markdown);
        assert_eq!(body.source_parts[0].content_type.as_deref(), Some(mime));
        assert!(body.warnings.is_empty());
    }
}

#[test]
fn readable_body_preserves_loss_flags_on_omitted_parts() {
    for mime in ["image/png", "application/pdf", ""] {
        let mut message = email(mime, "unrendered value");
        let value = message.body_values.as_mut().unwrap().get_mut("1").unwrap();
        value.is_truncated = true;
        value.is_encoding_problem = true;
        let body = message.readable_body(BodyPreference::Auto).unwrap();
        assert_eq!(body.content, "[Non-text body part omitted]");
        assert!(body.is_truncated);
        assert!(body.is_encoding_problem);
        assert!(body.warnings.iter().any(|w| w.contains("JMAP truncated")));
        assert!(body.warnings.iter().any(|w| w.contains("encoding problem")));
    }
}

#[test]
fn readable_body_plain_text_reports_omitted_graphics() {
    for tag in ["svg", "math"] {
        let html = format!("<p>before</p><{tag}><text>private graphic</text></{tag}><p>after</p>");
        let body = email("text/html", &html)
            .readable_body(BodyPreference::Text)
            .unwrap();
        assert_eq!(body.content, "before\n\nafter\n\n[Embedded media omitted]");
        assert!(body.warnings.iter().any(|w| w.contains("Embedded media")));
    }
}

#[test]
fn readable_body_placeholders_cannot_create_markdown_links() {
    for html in [
        "<img alt='Click here'>(https://evil.example/x)",
        "<img>(https://evil.example/x)",
        "<svg></svg>(https://evil.example/x)",
        "<iframe></iframe>(https://evil.example/x)",
    ] {
        let body = email("text/html", html)
            .readable_body(BodyPreference::Auto)
            .unwrap();
        for event in pulldown_cmark::Parser::new(&body.content) {
            assert!(
                !matches!(
                    event,
                    pulldown_cmark::Event::Start(
                        pulldown_cmark::Tag::Link { .. } | pulldown_cmark::Tag::Image { .. }
                    )
                ),
                "unexpected generated link: {event:?}; {}",
                body.content
            );
        }
    }
}

#[test]
fn readable_body_converts_html_in_the_text_preferred_sequence() {
    let html = include_str!("../../tests/fixtures/html-email.html");
    let message = email("text/html", html);
    let body = message.readable_body(BodyPreference::Auto).unwrap();
    assert_eq!(body.format, ReadableBodyFormat::Markdown);
    for expected in [
        "Invoice \\& receipt",
        "**$42\\.00**",
        "[View invoice](https://billing.example.test/invoice/123?x=1&y=2)",
        "| Item | Price |",
        "| Plan | $42\\.00 |",
        "- One",
        "- Two",
        "> Earlier quoted message",
        "Billing department",
        "Cancel subscription",
        "Cancellation deadline: September 30",
        "Chart\\: 12 \\& 13 units",
    ] {
        assert!(
            body.content.contains(expected),
            "missing {expected}: {}",
            body.content
        );
    }
    for absent in [
        "privateScript",
        "color: red",
        "private comment",
        "Hidden preheader",
        "Private page title",
        "cid:chart",
        "pixel.example",
        "![",
    ] {
        assert!(
            !body.content.contains(absent),
            "unexpected {absent}: {}",
            body.content
        );
    }
    assert_eq!(body.source_parts[0].part_id.as_deref(), Some("1"));
    assert_eq!(
        body.source_parts[0].content_type.as_deref(),
        Some("text/html")
    );
    assert!(
        body.warnings
            .iter()
            .any(|w| w.contains("Images were not loaded"))
    );
    assert_eq!(message.text_content().as_deref(), Some(html));

    let text = message.readable_body(BodyPreference::Text).unwrap();
    assert_eq!(text.format, ReadableBodyFormat::Text);
    assert!(
        text.content
            .contains("View invoice (https://billing.example.test/invoice/123?x=1&y=2)"),
        "{}",
        text.content
    );
    assert!(
        text.content.contains("Chart: 12 & 13 units"),
        "{}",
        text.content
    );
    assert!(!text.content.contains("pixel.example"));
}

#[test]
fn readable_body_chooses_one_alternative_and_keeps_part_order() {
    let message: Email = serde_json::from_value(json!({
        "id": "test",
        "textBody": [{"partId":"1","type":"text/plain"}, {"partId":"3","type":"text/plain"}],
        "htmlBody": [{"partId":"2","type":"text/html"}, {"partId":"3","type":"text/plain"}],
        "bodyValues": {"1":{"value":"plain alternative"}, "2":{"value":"<p>rich alternative</p>"}, "3":{"value":"footer"}}
    })).unwrap();
    for preference in [BodyPreference::Auto, BodyPreference::Text] {
        let body = message.readable_body(preference).unwrap();
        assert_eq!(body.content, "plain alternative\n\nfooter");
        assert_eq!(
            body.source_parts
                .iter()
                .map(|p| p.part_id.as_deref())
                .collect::<Vec<_>>(),
            [Some("1"), Some("3")]
        );
    }
    let body = message.readable_body(BodyPreference::Markdown).unwrap();
    assert_eq!(
        body.content.trim(),
        "rich alternative\n\n```text\nfooter\n```"
    );
    assert!(!body.content.contains("plain alternative"));
    assert_eq!(
        body.source_parts
            .iter()
            .map(|p| p.part_id.as_deref())
            .collect::<Vec<_>>(),
        [Some("2"), Some("3")]
    );
}

#[test]
fn readable_body_handles_html_only_empty_missing_and_unsupported_parts() {
    let mut message = email("text/html", "<p>Only HTML</p>");
    message.html_body = message.text_body.take();
    assert_eq!(
        message
            .readable_body(BodyPreference::Auto)
            .unwrap()
            .content
            .trim(),
        "Only HTML"
    );
    assert!(
        Email::default()
            .readable_body(BodyPreference::Auto)
            .is_none()
    );
    assert_eq!(
        email("text/plain", "")
            .readable_body(BodyPreference::Auto)
            .unwrap()
            .content,
        ""
    );
    message.body_values = None;
    let body = message.readable_body(BodyPreference::Auto).unwrap();
    assert!(body.warnings.iter().any(|w| w.contains("unavailable")));
    assert_eq!(body.content, "[Body part unavailable]");
    for mime in ["image/png", "application/pdf", ""] {
        let body = email(mime, "do not treat binary data as text")
            .readable_body(BodyPreference::Auto)
            .unwrap();
        assert_eq!(body.content, "[Non-text body part omitted]");
        assert_eq!(body.warnings.len(), 1);
    }
}

#[test]
fn readable_body_reports_upstream_loss_and_conversion_failure() {
    let mut message = email("text/html", "<p>Partial content");
    let value = message.body_values.as_mut().unwrap().get_mut("1").unwrap();
    value.is_truncated = true;
    value.is_encoding_problem = true;
    let body = message.readable_body(BodyPreference::Auto).unwrap();
    assert!(body.content.contains("Partial content"));
    assert!(body.is_truncated);
    assert!(body.is_encoding_problem);
    assert!(body.warnings.iter().any(|w| w.contains("JMAP truncated")));
    assert!(body.warnings.iter().any(|w| w.contains("encoding problem")));

    let body = email("text/html", "%PDF-private secret")
        .readable_body(BodyPreference::Auto)
        .unwrap();
    assert_eq!(body.content, "[HTML conversion unavailable]");
    assert_eq!(
        body.warnings,
        ["HTML conversion failed; inspect the original body values."]
    );
}

#[test]
fn readable_body_handles_embedded_media_and_escapes_image_alt_text() {
    let message = email(
        "TEXT/HTML",
        r#"<p>&lt;img src="https://raw.example/a"&gt;</p>
        <table><tr><td><img src="https://pixel.example/a" alt="![click](https://pixel.example/b)"></td></tr></table>
        <a href="https://example.test"><img src="data:image/png;base64,c2VjcmV0" alt="photo"></a>
        <svg><image href="https://pixel.example/c"/></svg>
        <iframe src="https://pixel.example/d">secret frame</iframe>
        <object data="https://pixel.example/e">secret object</object>"#,
    );
    let body = message.readable_body(BodyPreference::Auto).unwrap();
    for absent in [
        "![click]",
        "https://pixel.example/a",
        "data:image",
        "https://pixel.example/c",
        "secret frame",
        "secret object",
        "<svg",
        "<iframe",
    ] {
        assert!(
            !body.content.contains(absent),
            "unexpected {absent}: {}",
            body.content
        );
    }
    assert!(body.content.contains("photo"));
    for event in
        pulldown_cmark::Parser::new_ext(&body.content, pulldown_cmark::Options::ENABLE_TABLES)
    {
        assert!(
            !matches!(
                event,
                pulldown_cmark::Event::Html(_)
                    | pulldown_cmark::Event::InlineHtml(_)
                    | pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image { .. })
            ),
            "unexpected rendered resource: {event:?}"
        );
    }
    assert!(body.warnings.iter().any(|w| w.contains("Embedded media")));
}

#[test]
fn readable_body_bounds_input_output_parts_and_depth() {
    let too_large = "a".repeat(1024 * 1024 + 1);
    for mime in ["text/plain", "text/html"] {
        let body = email(mime, &too_large)
            .readable_body(BodyPreference::Auto)
            .unwrap();
        assert!(body.is_truncated);
        assert!(body.warnings.iter().any(|w| w.contains("input limit")));
        assert!(body.content.len() <= 1024 * 1024);
    }
    let large_unicode = format!("{}\u{00e9}", "*".repeat(1024 * 1024 - 1));
    let body = email("text/plain", &large_unicode)
        .readable_body(BodyPreference::Markdown)
        .unwrap();
    assert!(body.is_truncated);
    assert!(body.warnings.iter().any(|w| w.contains("output limit")));
    assert!(body.content.len() <= 1024 * 1024);

    let mut message = email("text/plain", "part");
    message.text_body = Some(vec![message.text_body.as_ref().unwrap()[0].clone(); 129]);
    let body = message.readable_body(BodyPreference::Auto).unwrap();
    assert_eq!(body.source_parts.len(), 128);
    assert!(body.is_truncated);
    assert!(body.warnings.iter().any(|w| w.contains("128-part")));

    let html = format!(
        "<p>before</p>{}too deep{}<p>after</p>",
        "<div>".repeat(80),
        "</div>".repeat(80)
    );
    for preference in [BodyPreference::Auto, BodyPreference::Text] {
        let body = email("text/html", &html).readable_body(preference).unwrap();
        assert!(body.is_truncated);
        assert!(body.warnings.iter().any(|w| w.contains("depth limit")));
        assert!(body.content.contains("before"));
        assert!(body.content.contains("after"));
        assert!(!body.content.contains("too deep"));
    }
}

#[test]
fn readable_body_limits_apply_across_parts_and_report_unrendered_suffixes() {
    let message: Email = serde_json::from_value(json!({
        "id":"test", "textBody":[{"partId":"1","type":"text/html"},{"partId":"2","type":"text/html"}],
        "bodyValues":{
            "1":{"value":format!("<!--{}--><p>first</p>", "x".repeat(600_000))},
            "2":{"value":format!("<!--{}--><p>second</p>", "x".repeat(600_000))}
        }
    })).unwrap();
    let body = message.readable_body(BodyPreference::Auto).unwrap();
    assert!(body.content.starts_with("first"));
    assert!(!body.content.contains("second"));
    assert!(body.is_truncated);
    assert!(body.warnings.iter().any(|w| w.contains("input limit")));
    assert_eq!(body.source_parts.len(), 2);

    let mut message = email("text/plain", &"x".repeat(1024 * 1024));
    let first = message.text_body.as_ref().unwrap()[0].clone();
    message.text_body.as_mut().unwrap().push(first);
    let body = message.readable_body(BodyPreference::Auto).unwrap();
    assert_eq!(body.content.len(), 1024 * 1024);
    assert!(body.is_truncated);
    assert!(body.warnings.iter().any(|w| w.contains("output limit")));
}
