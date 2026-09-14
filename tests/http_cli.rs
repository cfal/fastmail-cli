use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::process::Command;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

async fn server() -> MockServer {
    server_with_identities(json!([{"id":"me","name":"Me","email":"server@example.com"}])).await
}

async fn server_with_identities(identities: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/cli/v1/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "capabilities":{"urn:ietf:params:jmap:core":{},"urn:ietf:params:jmap:mail":{},"urn:ietf:params:jmap:submission":{},"https://www.fastmail.com/dev/maskedemail":{}},
            "accounts":{},"primaryAccounts":{"urn:ietf:params:jmap:mail":"acct"},"username":"server@example.com",
            "apiUrl":"http://127.0.0.1:9/must-not-follow", "downloadUrl":"http://127.0.0.1:9/", "uploadUrl":"http://127.0.0.1:9/", "eventSourceUrl":"http://127.0.0.1:9/"
        }))).mount(&server).await;
    Mock::given(method("POST")).and(path("/cli/v1/jmap"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let responses: Vec<_> = body["methodCalls"].as_array().unwrap().iter().map(|call| {
                let value = match call[0].as_str().unwrap() {
                    "Mailbox/get" => json!({"list": [
                        {"id":"inbox","name":"Inbox","role":"inbox"},
                        {"id":"sent","name":"Sent","role":"sent"},
                        {"id":"drafts","name":"Drafts","role":"drafts"},
                        {"id":"junk","name":"Junk","role":"junk"}
                    ]}),
                    "Identity/get" => json!({"list":identities}),
                    "Email/query" => json!({"ids":["e1"],"queryState":"s1","position":0,"total":1}),
                    "Email/get" => json!({"state":"s1","list":[{"id":"e1","threadId":"t1","subject":"Remote message","from":[{"email":"sender@example.com"}],"textBody":[{"partId":"text","type":"text/plain"}],"bodyValues":{"text":{"value":"Message body"}},"attachments":[{"blobId":"blob1","name":"notes.txt","type":"text/plain","size":10}]}]}),
                    "Thread/get" => json!({"list":[{"id":"t1","emailIds":["e1"]}]}),
                    "Email/changes" => json!({"newState":"s1","hasMoreChanges":false,"created":[]}),
                    "Email/set" => json!({"created":{"email":{"id":"new"}},"updated":{"e1":null}}),
                    "EmailSubmission/set" => json!({"created":{"submission":{"id":"submitted"}}}),
                    "MaskedEmail/get" => json!({"list":[]}),
                    "MaskedEmail/set" => json!({"created":{"new":{"id":"mask1","email":"mask@example.com","state":"enabled"}},"updated":{"mask1":null},"destroyed":["mask1"]}),
                    name => panic!("Unexpected JMAP method {name}"),
                };
                json!([call[0],value,call[2]])
            }).collect();
            ResponseTemplate::new(200).set_body_json(json!({"methodResponses":responses}))
        }).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/cli/v1/contacts"))
        .respond_with(|req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let contact = json!({"id":"contact1","name":"Remote Contact","emails":[],"phones":[]});
            let data = match body["operation"].as_str().unwrap() {
                "addressbooks" => json!([{"href":"/dav/contacts/","name":"Contacts"}]),
                "list" => json!([contact]),
                "create" | "update" => contact,
                "delete" => Value::Null,
                _ => panic!("Unknown contacts operation"),
            };
            ResponseTemplate::new(200).set_body_json(data)
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cli/v1/download"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"attachment"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/cli/v1/upload"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"blobId":"uploaded"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cli/v1/events"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(": ping\n\n", "text/event-stream"))
        .mount(&server)
        .await;
    server
}

fn isolated_command(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fastmail"));
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env_remove("FASTMAIL_SERVER")
        .env_remove("FASTMAIL_SERVER_USER")
        .env_remove("FASTMAIL_SERVER_PASSWORD")
        .env_remove("RUST_LOG")
        .env("FASTMAIL_API_TOKEN", "must-not-be-used")
        .env("FASTMAIL_USERNAME", "must-not-be-used")
        .env("FASTMAIL_APP_PASSWORD", "must-not-be-used");
    command
}

fn command(server: &MockServer, home: &std::path::Path) -> Command {
    let mut command = isolated_command(home);
    command
        .env("FASTMAIL_SERVER_PASSWORD", "http-password")
        .args(["--server", &server.uri(), "--server-user", "cli"]);
    command
}

fn url_command(server: &MockServer, home: &std::path::Path) -> Command {
    let mut command = isolated_command(home);
    command.env(
        "FASTMAIL_SERVER",
        format!(
            "http://cli%2Btest:http%3Apass%40word%2B@{}",
            server.address()
        ),
    );
    command
}

async fn bounded_output(command: &mut Command) -> std::process::Output {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        command.kill_on_drop(true).output(),
    )
    .await
    .expect("CLI timed out")
    .unwrap()
}

#[tokio::test]
async fn domain_sender_and_name_reach_send_reply_forward_and_drafts() {
    let server = server_with_identities(json!([
        {"id":"domain","name":"Saved Name","email":"*@example.com"}
    ]))
    .await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::body_string_contains("Email/get"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[[
                "Email/get", {"list":[{"id":"original", "threadId":"thread", "subject":"Original",
                    "from":[{"email":"other@example.com"}],
                    "to":[{"email":"new+tag@example.com"}],
                    "cc":[{"email":"NEW+TAG@example.com"},{"email":"another@example.com"}]
                }]}, "e0"
            ]]})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec![
            "send",
            "--to",
            "other@example.com",
            "--subject",
            "Subject",
            "--body",
            "Body",
        ],
        vec!["reply", "original", "--all", "--body", "Body"],
        vec![
            "forward",
            "original",
            "--to",
            "other@example.com",
            "--body",
            "Body",
        ],
    ] {
        for (draft, from) in [
            (false, "Fastmail-CLI Tester <New+tag@Example.COM>"),
            (true, "Fastmail-CLI Tester <New+tag@Example.COM>"),
            (false, r#""Fastmail-CLI Tester" <New+tag@Example.COM>"#),
            (true, r#""Fastmail-CLI Tester" <New+tag@Example.COM>"#),
        ] {
            let before = server.received_requests().await.unwrap().len();
            let mut cmd = command(&server, home.path());
            cmd.args(&args).args(["--from", from]);
            if draft {
                cmd.arg("--draft");
            }
            let output = bounded_output(&mut cmd).await;
            assert!(output.status.success(), "{args:?}: {output:?}");
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["success"], true);
            assert_eq!(
                result["data"]["status"],
                if draft { "draft" } else { "sent" }
            );
            let calls: Vec<Value> = server.received_requests().await.unwrap()[before..]
                .iter()
                .filter_map(|req| serde_json::from_slice::<Value>(&req.body).ok())
                .flat_map(|body| body["methodCalls"].as_array().cloned().unwrap_or_default())
                .collect();
            let creates: Vec<_> = calls.iter().filter(|call| call[0] == "Email/set").collect();
            assert_eq!(creates.len(), 1);
            let email = &creates[0][1]["create"]["email"];
            assert_eq!(
                email["from"],
                json!([{"email":"New+tag@Example.COM", "name":"Fastmail-CLI Tester"}])
            );
            assert_eq!(
                email["to"],
                json!([{"email":"other@example.com", "name":null}])
            );
            if args[0] == "reply" {
                assert_eq!(
                    email["cc"],
                    json!([{"email":"another@example.com", "name":null}])
                );
            }
            let submits: Vec<_> = calls
                .iter()
                .filter(|call| call[0] == "EmailSubmission/set")
                .collect();
            assert_eq!(submits.len(), usize::from(!draft));
            if !draft {
                assert_eq!(
                    submits[0][1]["create"]["submission"]["identityId"],
                    "domain"
                );
            }
            assert!(!calls.iter().any(|call| call[0] == "Identity/set"));
        }
    }
}

#[tokio::test]
async fn invalid_or_missing_senders_never_create_mail() {
    let server = server_with_identities(json!([
        {"id":"domain","name":"Saved Name","email":"*@example.com"}
    ]))
    .await;
    let home = tempfile::tempdir().unwrap();
    let output = bounded_output(command(&server, home.path()).args([
        "send",
        "--to",
        "other@example.com",
        "--subject",
        "Subject",
        "--body",
        "Body",
    ]))
    .await;
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["success"], false);
    assert_eq!(
        result["error"],
        "Config error: Only domain identities are available: specify a concrete sender using --from (GraphQL: from)"
    );
    for from in [
        "*@example.com",
        "new@sub.example.com",
        "new@notexample.com",
        "New <new@example.com> trailing",
    ] {
        for draft in [false, true] {
            let mut cmd = command(&server, home.path());
            cmd.args([
                "send",
                "--to",
                "other@example.com",
                "--subject",
                "Subject",
                "--body",
                "Body",
                "--from",
                from,
            ]);
            if draft {
                cmd.arg("--draft");
            }
            let output = bounded_output(&mut cmd).await;
            assert!(
                !output.status.success(),
                "{from:?}, draft={draft}: {output:?}"
            );
        }
    }
    for req in server.received_requests().await.unwrap() {
        assert_ne!(req.url.path(), "/cli/v1/upload");
        let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
        for call in body["methodCalls"].as_array().into_iter().flatten() {
            assert!(!call[0].as_str().unwrap().ends_with("/set"), "{call}");
        }
    }
}

#[tokio::test]
async fn drafts_without_from_can_be_saved_when_identity_lookup_is_empty_or_fails() {
    for lookup_fails in [false, true] {
        let server = server_with_identities(json!([])).await;
        if lookup_fails {
            Mock::given(method("POST"))
                .and(wiremock::matchers::body_string_contains("Identity/get"))
                .respond_with(ResponseTemplate::new(503))
                .with_priority(1)
                .mount(&server)
                .await;
        }
        let home = tempfile::tempdir().unwrap();
        for from in [None, Some("new@example.com")] {
            let mut cmd = command(&server, home.path());
            cmd.args([
                "send",
                "--draft",
                "--to",
                "other@example.com",
                "--subject",
                "Subject",
                "--body",
                "Body",
            ]);
            if let Some(from) = from {
                cmd.args(["--from", from]);
            }
            let output = bounded_output(&mut cmd).await;
            assert_eq!(output.status.success(), from.is_none(), "{output:?}");
            if from.is_none() {
                let result: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(result["success"], true);
                assert_eq!(result["data"]["status"], "draft");
            }
        }
        let mut creates = 0;
        for req in server.received_requests().await.unwrap() {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            for call in body["methodCalls"].as_array().into_iter().flatten() {
                assert_ne!(call[0], "EmailSubmission/set");
                if call[0] == "Email/set" {
                    creates += 1;
                    let email = &call[1]["create"]["email"];
                    assert!(email.get("from").is_none());
                    assert_eq!(email["keywords"]["$draft"], true);
                    assert_eq!(email["mailboxIds"], json!({"drafts":true}));
                }
            }
        }
        assert_eq!(creates, 1);
    }
}

#[tokio::test]
async fn server_url_env_authenticates_mail_and_contacts() {
    let server = server().await;
    let home = tempfile::tempdir().unwrap();
    for (args, expected_id) in [
        (["list", "mailboxes"], "inbox"),
        (["contacts", "list"], "contact1"),
    ] {
        let output = bounded_output(url_command(&server, home.path()).args(args)).await;
        assert!(output.status.success(), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["data"][0]["id"], expected_id);
        for text in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(text);
            assert!(!text.contains("http:pass@word+"));
            assert!(!text.contains("http%3Apass%40word%2B"));
        }
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    for request in requests {
        assert_eq!(
            request.headers["authorization"],
            format!("Basic {}", STANDARD.encode("cli+test:http:pass@word+"))
        );
        assert!(!request.headers.contains_key("x-fastmail-token"));
    }
    assert!(
        !home
            .path()
            .join(".config/fastmail-cli/config.toml")
            .exists()
    );
}

#[tokio::test]
async fn help_and_argument_errors_hide_server_credentials() {
    let home = tempfile::tempdir().unwrap();
    let url = "https://private-user:private-password%40@example.invalid:8443";
    for (args, success) in [
        (vec!["--help"], true),
        (vec!["list", "mailboxes", "--help"], true),
        (vec!["--not-a-flag"], false),
    ] {
        let output = bounded_output(
            isolated_command(home.path())
                .env("FASTMAIL_SERVER", url)
                .env("FASTMAIL_SERVER_USER", "separate-http-login")
                .args(&args),
        )
        .await;
        assert_eq!(output.status.success(), success, "{args:?}");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!text.contains("private-user"));
        assert!(!text.contains("private-password"));
        assert!(!text.contains("separate-http-login"));
        if success {
            assert!(text.contains("FASTMAIL_SERVER"));
            assert!(text.contains("--server"));
        }
    }
}

#[tokio::test]
async fn mixed_server_login_sources_fail_before_network_access() {
    let server = server().await;
    let home = tempfile::tempdir().unwrap();
    for (user, password) in [
        (Some("other-user"), None),
        (None, Some("other-password")),
        (Some("other-user"), Some("other-password")),
        (Some(""), None),
        (None, Some("")),
        (Some(""), Some("")),
    ] {
        let mut command = url_command(&server, home.path());
        if let Some(user) = user {
            command.env("FASTMAIL_SERVER_USER", user);
        }
        if let Some(password) = password {
            command.env("FASTMAIL_SERVER_PASSWORD", password);
        }
        let output = bounded_output(command.args(["list", "mailboxes"])).await;
        assert_eq!(output.status.code(), Some(1));
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result,
            json!({"success":false, "error":
                "Config error: Use either server URL credentials or --server-user and FASTMAIL_SERVER_PASSWORD, not both"})
        );
        assert!(output.stderr.is_empty());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unsupported_images_are_reported_without_losing_other_downloads() {
    use wiremock::matchers::query_param;
    let server = server().await;
    Mock::given(method("POST"))
        .and(path("/cli/v1/jmap"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[[
                "Email/get", {"list":[{"id":"e1","attachments":[
                    {"blobId":"tiff","name":"scan.tiff","type":"image/tiff"},
                    {"blobId":"png","name":"corrupt.png","type":"image/png"},
                    {"blobId":"text","name":"notes.txt","type":"text/plain"}
                ]}]}, "g0"
            ]]})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    for blob in ["tiff", "png"] {
        Mock::given(method("GET"))
            .and(path("/cli/v1/download"))
            .and(query_param("blob_id", blob))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0; 1025]))
            .with_priority(1)
            .mount(&server)
            .await;
    }
    let home = tempfile::tempdir().unwrap();
    let output = command(&server, home.path())
        .args([
            "download",
            "e1",
            "--max-size",
            "1K",
            "--output",
            home.path().to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        output.status.success(),
        "Partial downloads retain the documented zero exit status"
    );
    assert_eq!(result["success"], false);
    assert_eq!(
        result["error"],
        "Some images could not be resized; see data.skipped"
    );
    assert_eq!(result["data"]["skipped"].as_array().unwrap().len(), 2);
    assert_eq!(result["data"]["files"].as_array().unwrap().len(), 1);
    assert_eq!(
        std::fs::read(home.path().join("notes.txt")).unwrap(),
        b"attachment"
    );
    assert!(!home.path().join("scan.tiff").exists());
    assert!(!home.path().join("corrupt.png").exists());
}

#[tokio::test]
async fn colliding_download_names_preserve_existing_and_new_files() {
    use wiremock::matchers::query_param;
    let server = server().await;
    Mock::given(method("POST"))
        .and(path("/cli/v1/jmap"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"methodResponses":[[
                "Email/get", {"list":[{"id":"e1","attachments":[
                    {"blobId":"first","name":"../notes.txt","type":"text/plain"},
                    {"blobId":"second","name":"notes.txt","type":"text/plain"}
                ]}]}, "g0"
            ]]})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    for blob in ["first", "second"] {
        Mock::given(method("GET"))
            .and(path("/cli/v1/download"))
            .and(query_param("blob_id", blob))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(blob.as_bytes()))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
    }
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("attachments");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("notes.txt"), b"existing").unwrap();
    let output = command(&server, home.path())
        .args(["download", "e1", "--output", destination.to_str().unwrap()])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result,
        json!({"success":true, "data":{"files":[
            destination.join("notes-2.txt"), destination.join("notes-3.txt")
        ]}})
    );
    assert_eq!(
        std::fs::read(destination.join("notes.txt")).unwrap(),
        b"existing"
    );
    assert_eq!(
        std::fs::read(destination.join("notes-2.txt")).unwrap(),
        b"first"
    );
    assert_eq!(
        std::fs::read(destination.join("notes-3.txt")).unwrap(),
        b"second"
    );
    assert!(!home.path().join("notes.txt").exists());
}

#[tokio::test]
async fn json_download_extracts_text_without_creating_attachment_files() {
    let server = server().await;
    let home = tempfile::tempdir().unwrap();
    let output = command(&server, home.path())
        .args([
            "download",
            "e1",
            "--format",
            "json",
            "--output",
            home.path().to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result,
        json!({"success":true, "data":[{
            "filename":"notes.txt", "content_type":"text/plain", "size":10, "text":"attachment"
        }]})
    );
    assert!(!home.path().join("notes.txt").exists());
}

#[tokio::test]
async fn destructive_commands_require_confirmation_before_network_access() {
    let server = server().await;
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec!["spam", "e1"],
        vec!["masked", "delete", "mask1"],
        vec!["contacts", "delete", "contact1"],
    ] {
        let output = command(&server, home.path())
            .args(&args)
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Use -y to confirm"),
            "{args:?}"
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn mail_and_contact_commands_use_http_without_local_credentials() {
    let server = server().await;
    let home = tempfile::tempdir().unwrap();
    let commands: &[&[&str]] = &[
        &["list", "mailboxes"],
        &["list", "emails"],
        &["list", "identities"],
        &["get", "e1"],
        &["thread", "e1"],
        &["search", "--text", "Remote"],
        &[
            "send",
            "--to",
            "to@example.com",
            "--subject",
            "Hello",
            "--body",
            "Body",
        ],
        &["reply", "e1", "--body", "Reply"],
        &[
            "forward",
            "e1",
            "--to",
            "to@example.com",
            "--body",
            "Forward",
        ],
        &["move", "e1", "--to", "inbox"],
        &["spam", "e1", "-y"],
        &["mark-read", "e1"],
        &["masked", "list"],
        &["masked", "create"],
        &["masked", "enable", "mask1"],
        &["masked", "disable", "mask1"],
        &["masked", "delete", "mask1", "-y"],
        &["contacts", "list"],
        &["contacts", "search", "Remote"],
        &["contacts", "create", "--name", "Remote Contact"],
        &["contacts", "update", "contact1", "--name", "Changed"],
        &["contacts", "delete", "contact1", "-y"],
    ];
    for args in commands {
        let output = command(&server, home.path())
            .args(*args)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["success"], true, "{args:?}: {value}");
    }
    let download = home.path().join("download");
    std::fs::create_dir(&download).unwrap();
    let output = command(&server, home.path())
        .args(["download", "e1", "--output", download.to_str().unwrap()])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        std::fs::read(download.join("notes.txt")).unwrap(),
        b"attachment"
    );
    let output = command(&server, home.path())
        .args([
            "send",
            "--to",
            "to@example.com",
            "--subject",
            "Attachment",
            "--body",
            "Body",
            "--attachment",
            download.join("notes.txt").to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for req in server.received_requests().await.unwrap() {
        assert_eq!(
            req.headers["authorization"],
            "Basic Y2xpOmh0dHAtcGFzc3dvcmQ="
        );
        assert!(!req.headers.contains_key("x-fastmail-token"));
    }
    assert!(
        !home
            .path()
            .join(".config/fastmail-cli/config.toml")
            .exists()
    );
}

#[tokio::test]
async fn compose_file_errors_happen_before_network_access() {
    let server = server().await;
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("missing.txt");
    for args in [
        vec![
            "send",
            "--to",
            "to@example.com",
            "--subject",
            "Hi",
            "--body",
            "Body",
        ],
        vec!["reply", "e1", "--body", "Body"],
        vec!["forward", "e1", "--to", "to@example.com"],
    ] {
        let output = command(&server, home.path())
            .args(args)
            .arg("--attachment")
            .arg(&missing)
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["success"], false);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn masked_state_commands_keep_their_messages_and_wire_states() {
    let server = server().await;
    let home = tempfile::tempdir().unwrap();
    for (operation, state) in [
        ("enable", "enabled"),
        ("disable", "disabled"),
        ("delete", "deleted"),
    ] {
        let mut cmd = command(&server, home.path());
        cmd.args(["masked", operation, "mask1"]);
        if operation == "delete" {
            cmd.arg("-y");
        }
        let output = cmd.output().await.unwrap();
        assert!(output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result,
            json!({"success": true, "message": format!("Masked email mask1 {state}")})
        );
    }
    let requests = server.received_requests().await.unwrap();
    let states: Vec<Value> = requests
        .iter()
        .filter(|r| r.url.path() == "/cli/v1/jmap")
        .map(|r| {
            let body: Value = serde_json::from_slice(&r.body).unwrap();
            body["methodCalls"][0][1]["update"]["mask1"]["state"].clone()
        })
        .collect();
    assert_eq!(
        states,
        vec![json!("enabled"), json!("disabled"), json!("deleted")]
    );
}

#[tokio::test]
async fn watch_uses_http_event_stream_and_polling() {
    let home = tempfile::tempdir().unwrap();
    for args in [&["watch"][..], &["watch", "--poll", "1"][..]] {
        // Each mode gets fresh fixtures so requests cannot satisfy the next assertion.
        let server = self::server().await;
        let mut child = command(&server, home.path())
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let requests = server.received_requests().await.unwrap();
                let ready = if args.len() == 1 {
                    requests.iter().any(|r| r.url.path() == "/cli/v1/events")
                } else {
                    requests
                        .iter()
                        .any(|r| String::from_utf8_lossy(&r.body).contains("Email/changes"))
                };
                if ready {
                    break;
                }
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("watch exited: {status}");
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await;
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        assert!(result.is_ok(), "watch mode {args:?} never reached server");
    }
}

#[tokio::test]
async fn zero_poll_is_rejected_without_contacting_the_server() {
    let server = self::server().await;
    let home = tempfile::tempdir().unwrap();
    let output = command(&server, home.path())
        .args(["watch", "--poll", "0"])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn invalid_image_size_is_rejected_without_contacting_the_server() {
    let server = self::server().await;
    let home = tempfile::tempdir().unwrap();
    let output = command(&server, home.path())
        .args(["download", "e1", "--max-size", "invalid"])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn remote_errors_keep_the_servers_actionable_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_json(json!({"error":"Fastmail is temporarily unavailable"})),
        )
        .mount(&server)
        .await;
    let home = tempfile::tempdir().unwrap();
    let output = command(&server, home.path())
        .args(["list", "mailboxes"])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("Fastmail is temporarily unavailable"),
        "{text}"
    );
}

#[tokio::test]
async fn http_login_never_follows_redirects_or_falls_back_to_fastmail() {
    let destination = MockServer::start().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(307).insert_header("Location", destination.uri()))
        .mount(&server)
        .await;
    let home = tempfile::tempdir().unwrap();
    for mut command in [
        command(&server, home.path()),
        url_command(&server, home.path()),
    ] {
        let output = bounded_output(command.args(["list", "mailboxes"])).await;
        assert!(!output.status.success());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["success"], false);
        assert!(!result.to_string().contains("http%3Apass%40word%2B"));
        assert!(!result.to_string().contains("http:pass@word+"));
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    assert!(destination.received_requests().await.unwrap().is_empty());
}
