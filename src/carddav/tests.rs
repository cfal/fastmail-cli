use super::*;

#[test]
fn contact_input_lists_preserve_empty_components_and_trim_values() {
    for (input, expected) in [
        (None, vec![]),
        (Some(""), vec![""]),
        (Some(" a , ,b, "), vec!["a", "", "b", ""]),
    ] {
        let emails = ContactEmail::parse_list(input);
        assert_eq!(
            emails
                .iter()
                .map(|entry| entry.email.as_str())
                .collect::<Vec<_>>(),
            expected,
        );
        assert!(emails.iter().all(|entry| entry.label.is_none()));
        let phones = ContactPhone::parse_list(input);
        assert_eq!(
            phones
                .iter()
                .map(|entry| entry.number.as_str())
                .collect::<Vec<_>>(),
            expected,
        );
        assert!(phones.iter().all(|entry| entry.label.is_none()));
    }
}

#[test]
fn resource_urls_cannot_redirect_credentials() {
    let client = CardDavClient::try_new("test".into(), "secret".into()).unwrap();
    for href in [
        "@evil.example/dav/",
        "//evil.example/dav/",
        "https://evil.example/",
        "/\\evil.example/",
        "https://user@carddav.fastmail.com/",
    ] {
        assert!(client.resource_url(href).is_err(), "{href}");
    }
    for href in [
        "/dav/contacts/",
        "https://carddav.fastmail.com/dav/contacts/",
    ] {
        assert_eq!(
            client.resource_url(href).unwrap().as_str(),
            "https://carddav.fastmail.com/dav/contacts/"
        );
    }
}

#[test]
fn vcard_fields_cannot_inject_properties() {
    let name = "Name\r\nEMAIL:injected@example.com";
    let notes = "first\nsecond; value, \\n";
    let vcard = build_vcard(
        "id",
        name,
        &[ContactEmail {
            email: "real@example.com".into(),
            label: Some("work\r\nEMAIL:injected@example.com".into()),
        }],
        &[],
        Some("Company; division"),
        None,
        Some(notes),
    );
    assert_eq!(vcard.lines().filter(|l| l.starts_with("EMAIL")).count(), 1);
    let contact = parse_vcard(&vcard).unwrap();
    assert_eq!(contact.name, name.replace("\r\n", "\n"));
    assert_eq!(contact.notes.as_deref(), Some(notes));
    assert_eq!(contact.organization.as_deref(), Some("Company; division"));
    assert_eq!(contact.emails[0].email, "real@example.com");
}

#[test]
fn test_unfold_vcard_lines() {
    // RFC 6350 §3.2: leading space/tab is the fold indicator and is consumed
    let input = "FN:John\n  Doe\nEMAIL:john@example.com";
    let result = unfold_vcard(input);
    assert_eq!(result, "FN:John Doe\nEMAIL:john@example.com");
}

#[test]
fn test_unfold_tab_continuation() {
    let input = "FN:John\n\tDoe";
    let result = unfold_vcard(input);
    assert_eq!(result, "FN:JohnDoe");
}

#[test]
fn test_decode_qp_basic() {
    assert_eq!(decode_qp("hello=20world"), "hello world");
    assert_eq!(decode_qp("caf=C3=A9"), "café");
}

#[test]
fn test_decode_qp_soft_linebreak() {
    assert_eq!(decode_qp("hello=\nworld"), "helloworld");
}

#[test]
fn test_parse_vcard_basic() {
    let vcard =
        "BEGIN:VCARD\nVERSION:3.0\nUID:abc123\nFN:Alice Smith\nEMAIL:alice@example.com\nEND:VCARD";
    let contact = parse_vcard(vcard).unwrap();
    assert_eq!(contact.id, "abc123");
    assert_eq!(contact.name, "Alice Smith");
    assert_eq!(contact.emails.len(), 1);
    assert_eq!(contact.emails[0].email, "alice@example.com");
}

#[test]
fn test_parse_vcard_with_line_folding() {
    // Fold happens mid-value: "Very Long Name Here" folded after "Na"
    // Continuation line starts with space (fold indicator consumed)
    let vcard = "BEGIN:VCARD\nFN:Very Long Na\n me Here\nEMAIL:test@example.com\nEND:VCARD";
    let contact = parse_vcard(vcard).unwrap();
    assert_eq!(contact.name, "Very Long Name Here");
}

#[test]
fn test_parse_vcard_with_params() {
    let vcard = "BEGIN:VCARD\nFN:Bob\nEMAIL;TYPE=work:bob@work.com\nTEL;TYPE=cell:+1234567890\nORG:Acme Inc\nTITLE:Engineer\nEND:VCARD";
    let contact = parse_vcard(vcard).unwrap();
    assert_eq!(contact.emails[0].email, "bob@work.com");
    assert_eq!(contact.emails[0].label, Some("work".to_string()));
    assert_eq!(contact.phones[0].number, "+1234567890");
    assert_eq!(contact.organization, Some("Acme Inc".to_string()));
    assert_eq!(contact.title, Some("Engineer".to_string()));
}

#[test]
fn email_and_phone_labels_keep_existing_parameter_semantics() {
    for (parameters, expected) in [
        ("", None),
        (";TYPE=", Some("")),
        (";TYPE=home,work", Some("home,work")),
        (";TYPE=work;PREF=1", Some("work;PREF=1")),
        (";TYPE=home;TYPE=work", Some("home;")),
        (";type=work", None),
    ] {
        let contact = parse_vcard(&format!(
            "FN:Name\nEMAIL{parameters}:a@example.test\nTEL{parameters}:123\n"
        ))
        .unwrap();
        assert_eq!(contact.emails[0].label.as_deref(), expected);
        assert_eq!(contact.phones[0].label.as_deref(), expected);
    }
}

#[test]
fn structured_names_split_only_at_the_first_space() {
    for (name, expected) in [
        ("", "N:;;;;"),
        ("Alice", "N:Alice;;;;"),
        ("Alice van Smith", "N:van Smith;Alice;;;"),
        (" Alice", "N:Alice;;;;"),
        ("Alice ", "N:;Alice;;;"),
    ] {
        let vcard = build_vcard("id", name, &[], &[], None, None, None);
        assert_eq!(
            vcard.lines().find(|line| line.starts_with("N:")),
            Some(expected)
        );
    }
}

#[test]
fn dav_properties_require_an_explicit_success_status() {
    for status in ["", "<d:status>HTTP/1.1 404 Not Found</d:status>"] {
        let xml = format!(
            "<d:response xmlns:d='DAV:'><d:propstat><d:prop><d:getetag>\"v1\"</d:getetag></d:prop>{status}</d:propstat></d:response>"
        );
        let doc = roxmltree::Document::parse(&xml).unwrap();
        assert_eq!(dav_property(doc.root_element(), "DAV:", "getetag"), None);
    }
}

#[test]
fn uidless_contacts_use_stable_distinct_resource_ids() {
    let vcard = "BEGIN:VCARD\nFN:No UID\nEND:VCARD";
    let client = CardDavClient::try_new("user".into(), "password".into()).unwrap();
    let a = client.contact_at(vcard, "/books/a.vcf").unwrap();
    let b = client.contact_at(vcard, "/books/b.vcf").unwrap();
    assert!(a.id.starts_with("href:"));
    assert_ne!(a.id, b.id);
    assert_eq!(
        a.id,
        client
            .contact_at(vcard, "https://carddav.fastmail.com/books/a.vcf")
            .unwrap()
            .id
    );
}

#[test]
fn contact_updates_preserve_unrequested_content_lines() {
    let original = concat!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:contact\r\nFN:Original\r\nN:Family;Given;;;\r\n",
        "ADR;TYPE=home:;;Street;City;;;\r\nBDAY:2000-01-01\r\n",
        "PHOTO;ENCODING=b:YWJj\r\n ZGVm\r\nURL:https://example.test/\r\n",
        "item1.EMAIL;TYPE=home:person@example.test\r\nitem1.X-ABLabel:Custom\r\n",
        "X-CUSTOM:keep\r\nTITLE:Old\r\nEND:VCARD\r\n",
    );
    let updated = update_vcard(
        original,
        &ContactFields {
            title: Some("New"),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(updated, original.replace("TITLE:Old", "TITLE:New"));
    let renamed = update_vcard(
        original,
        &ContactFields {
            name: Some("New Name"),
            emails: Some(&[]),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!renamed.contains("item1.EMAIL"));
    assert!(renamed.contains("FN:New Name\r\nN:Name;New;;;\r\n"));
    assert!(renamed.contains("item1.X-ABLabel:Custom\r\n"));
    assert!(renamed.contains("PHOTO;ENCODING=b:YWJj\r\n ZGVm\r\n"));
}

#[test]
fn contact_updates_reject_incomplete_or_multiple_cards() {
    let fields = ContactFields {
        title: Some("Updated"),
        ..Default::default()
    };
    for original in [
        "BEGIN:OTHER\nFN:Name\nEND:VCARD\n",
        "BEGIN:VCARD\nFN:Name\n",
        "BEGIN:VCARD\nFN:Name\nEND:VCARD\nBEGIN:VCARD\nFN:Other\nEND:VCARD\n",
    ] {
        assert!(update_vcard(original, &fields).is_err());
    }
}

async fn contact_server(vcard: &str, etag: &str) -> (CardDavClient, wiremock::MockServer) {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
    let server = MockServer::start().await;
    let mut client = CardDavClient::try_new("user".into(), "password".into()).unwrap();
    client.base_url = server.uri();
    Mock::given(method("PROPFIND")).respond_with(ResponseTemplate::new(207).set_body_string(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/books/</d:href><d:propstat><d:prop><d:resourcetype><c:addressbook/></d:resourcetype></d:prop></d:propstat></d:response></d:multistatus>"#
    )).mount(&server).await;
    Mock::given(method("REPORT")).respond_with(ResponseTemplate::new(207).set_body_string(format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/books/contact.vcf</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag><c:address-data><![CDATA[{vcard}]]></c:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
    ))).mount(&server).await;
    (client, server)
}

#[tokio::test]
async fn contact_mutations_use_successful_properties_and_trim_etags() {
    use wiremock::{
        Mock, ResponseTemplate,
        matchers::{header, method},
    };
    let original = "BEGIN:VCARD\nUID:contact\nFN:Name\nEND:VCARD\n";
    let (client, server) = contact_server(original, "\"old\"").await;
    Mock::given(method("REPORT")).respond_with(
        ResponseTemplate::new(207).set_body_string(format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/books/contact.vcf</d:href><d:propstat><d:prop><d:getetag/><c:address-data/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat><d:propstat><d:prop><d:getetag>
            "version-1"
            </d:getetag><c:address-data><![CDATA[{original}]]></c:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        ))
    ).with_priority(1).mount(&server).await;
    for verb in ["PUT", "DELETE"] {
        Mock::given(method(verb))
            .and(header("If-Match", "\"version-1\""))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    client
        .update_contact(
            "contact",
            &ContactFields {
                title: Some("New"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    client.delete_contact("contact").await.unwrap();
}

#[test]
fn bad_uidless_resources_do_not_abort_contact_listing() {
    let client = CardDavClient::try_new("user".into(), "password".into()).unwrap();
    let responses = ["contact.vcf", "", "/books/good.vcf"].into_iter().map(|href| format!(
        r#"<d:response><d:href>{href}</d:href><d:propstat><d:prop><c:address-data>BEGIN:VCARD
FN:Name
END:VCARD
</c:address-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#
    )).collect::<String>();
    let contacts = client.parse_contacts_response(&format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav">{responses}</d:multistatus>"#
    )).unwrap();
    assert_eq!(contacts.len(), 1);
    assert_eq!(
        contacts[0].id,
        client.contact_at("FN:Name", "/books/good.vcf").unwrap().id
    );
}

#[tokio::test]
async fn uidless_resource_mutations_use_etags_and_preserve_data() {
    use wiremock::{
        Mock, ResponseTemplate,
        matchers::{header, method, path},
    };
    let original = "BEGIN:VCARD\nVERSION:3.0\nFN:Same Name\nBDAY:2000-01-01\nEND:VCARD\n";
    let (client, server) = contact_server(original, "\"version-1\"").await;
    for verb in ["PUT", "DELETE"] {
        Mock::given(method(verb))
            .and(path("/books/contact.vcf"))
            .and(header("If-Match", "\"version-1\""))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    let contact = client.list_contacts("/books/").await.unwrap().remove(0);
    let updated = client
        .update_contact(
            &contact.id,
            &ContactFields {
                title: Some("Updated"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.id, contact.id);
    client.delete_contact(&contact.id).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let put = requests.iter().find(|r| r.method == "PUT").unwrap();
    let body = std::str::from_utf8(&put.body).unwrap();
    assert!(body.contains("BDAY:2000-01-01"));
    assert!(body.contains("TITLE:Updated"));
    assert!(!body.contains("UID:"));
}

#[tokio::test]
async fn contact_mutations_report_conflicts_and_refuse_missing_etags() {
    use wiremock::{Mock, ResponseTemplate, matchers::method};
    let original = "BEGIN:VCARD\nUID:contact\nFN:Name\nEND:VCARD\n";
    let (client, server) = contact_server(original, "\"version-1\"").await;
    for verb in ["PUT", "DELETE"] {
        Mock::given(method(verb))
            .respond_with(ResponseTemplate::new(412))
            .expect(1)
            .mount(&server)
            .await;
    }
    let fields = ContactFields {
        title: Some("Updated"),
        ..Default::default()
    };
    assert!(
        client
            .update_contact("contact", &fields)
            .await
            .unwrap_err()
            .to_string()
            .contains("concurrently")
    );
    assert!(
        client
            .delete_contact("contact")
            .await
            .unwrap_err()
            .to_string()
            .contains("concurrently")
    );
    let (client, server) = contact_server(original, "").await;
    assert!(
        client
            .update_contact("contact", &fields)
            .await
            .unwrap_err()
            .to_string()
            .contains("ETag")
    );
    assert!(
        client
            .delete_contact("contact")
            .await
            .unwrap_err()
            .to_string()
            .contains("ETag")
    );
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.method == "PUT" || r.method == "DELETE")
    );
}

#[tokio::test]
async fn weak_etags_never_allow_unconditional_mutations() {
    let (client, server) =
        contact_server("BEGIN:VCARD\nUID:contact\nFN:Name\nEND:VCARD\n", "W/\"v1\"").await;
    assert!(
        client
            .delete_contact("contact")
            .await
            .unwrap_err()
            .to_string()
            .contains("strong ETag")
    );
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.method == "DELETE")
    );
}

#[tokio::test]
async fn shared_transport_keeps_carddav_credentials_request_scoped() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
    let server = MockServer::start().await;
    Mock::given(method("PROPFIND"))
        .respond_with(
            ResponseTemplate::new(207).set_body_string(r#"<d:multistatus xmlns:d="DAV:"/>"#),
        )
        .mount(&server)
        .await;
    for username in ["alice", "bob"] {
        let mut client = CardDavClient::try_new(username.into(), "password".into()).unwrap();
        client.base_url = server.uri();
        client.list_addressbooks().await.unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests[0].headers["authorization"],
        "Basic YWxpY2U6cGFzc3dvcmQ="
    );
    assert_eq!(
        requests[1].headers["authorization"],
        "Basic Ym9iOnBhc3N3b3Jk"
    );
}

#[test]
fn test_parse_vcard_returns_none_without_name() {
    let vcard = "BEGIN:VCARD\nUID:abc\nEMAIL:test@example.com\nEND:VCARD";
    assert!(parse_vcard(vcard).is_none());
}

#[test]
fn test_build_vcard_basic() {
    let vcard = build_vcard(
        "test-uid-123",
        "Jane Doe",
        &[ContactEmail {
            email: "jane@example.com".to_string(),
            label: None,
        }],
        &[],
        Some("Acme Corp"),
        None,
        None,
    );
    assert!(vcard.contains("BEGIN:VCARD"));
    assert!(vcard.contains("VERSION:3.0"));
    assert!(vcard.contains("UID:test-uid-123"));
    assert!(vcard.contains("FN:Jane Doe"));
    assert!(vcard.contains("N:Doe;Jane;;;"));
    assert!(vcard.contains("EMAIL:jane@example.com"));
    assert!(vcard.contains("ORG:Acme Corp"));
    assert!(vcard.contains("END:VCARD"));
}

#[test]
fn test_build_vcard_with_labels() {
    let vcard = build_vcard(
        "uid-456",
        "Bob",
        &[ContactEmail {
            email: "bob@work.com".to_string(),
            label: Some("work".to_string()),
        }],
        &[ContactPhone {
            number: "+1234567890".to_string(),
            label: Some("cell".to_string()),
        }],
        None,
        Some("Engineer"),
        Some("A note"),
    );
    assert!(vcard.contains("EMAIL;TYPE=work:bob@work.com"));
    assert!(vcard.contains("TEL;TYPE=cell:+1234567890"));
    assert!(vcard.contains("N:Bob;;;;"));
    assert!(vcard.contains("TITLE:Engineer"));
    assert!(vcard.contains("NOTE:A note"));
}

#[test]
fn test_build_vcard_roundtrips() {
    let vcard = build_vcard(
        "roundtrip-uid",
        "Alice Smith",
        &[
            ContactEmail {
                email: "alice@home.com".to_string(),
                label: Some("home".to_string()),
            },
            ContactEmail {
                email: "alice@work.com".to_string(),
                label: Some("work".to_string()),
            },
        ],
        &[ContactPhone {
            number: "+9876543210".to_string(),
            label: None,
        }],
        Some("Widgets Inc"),
        Some("CEO"),
        Some("Important person"),
    );

    // parse_vcard expects \n line endings, build_vcard uses \r\n
    let unix_vcard = vcard.replace("\r\n", "\n");
    let contact = parse_vcard(&unix_vcard).expect("Should parse built vcard");
    assert_eq!(contact.id, "roundtrip-uid");
    assert_eq!(contact.name, "Alice Smith");
    assert_eq!(contact.emails.len(), 2);
    assert_eq!(contact.emails[0].email, "alice@home.com");
    assert_eq!(contact.emails[0].label, Some("home".to_string()));
    assert_eq!(contact.phones.len(), 1);
    assert_eq!(contact.phones[0].number, "+9876543210");
    assert_eq!(contact.organization, Some("Widgets Inc".to_string()));
    assert_eq!(contact.title, Some("CEO".to_string()));
    assert_eq!(contact.notes, Some("Important person".to_string()));
}

#[test]
fn test_generate_uid_unique() {
    let uid1 = generate_uid();
    let uid2 = generate_uid();
    assert_ne!(uid1, uid2);
    // Should look UUID-ish
    assert_eq!(uid1.matches('-').count(), 4);
}
