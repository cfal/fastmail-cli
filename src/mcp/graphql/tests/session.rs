use super::*;

const SESSION: &str = "{ session { status username primaryAccountId capabilities detail
                                   carddavConfigured
                                   accounts { id name isPersonal isReadOnly } } }";

/// The `session` field, against a server mounting whatever the caller set up.
/// Asserts the whole point of the field as it goes: it answers, always.
async fn session_of(server: &MockServer) -> Value {
    let resp = run(server, SESSION).await;
    assert!(
        resp.errors.is_empty(),
        "session must report a bad connection, not raise one: {:?}",
        resp.errors
    );
    resp.data.into_json().unwrap()["session"].clone()
}

/// A JMAP server that answers the handshake with `status` and nothing else.
async fn session_endpoint(status: u16) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(status))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn session_reports_the_authenticated_account() {
    let server = mock_server(1).await;

    assert_eq!(
        session_of(&server).await,
        json!({
            "status": "CONNECTED",
            "username": "test@example.com",
            "primaryAccountId": "acct1",
            "capabilities": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
            "carddavConfigured": false,
            "accounts": [
                { "id": "acct1", "name": "test@example.com",
                  "isPersonal": true, "isReadOnly": false },
                { "id": "acct2", "name": "Shared",
                  "isPersonal": false, "isReadOnly": true },
            ],
            "detail": null,
        })
    );
}

#[tokio::test]
async fn session_is_a_live_check_not_a_cached_one() {
    let server = mock_server(1).await;
    assert_eq!(session_of(&server).await["status"], "CONNECTED");

    let handshakes = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == "/jmap/session")
        .count();
    assert_eq!(
        handshakes, 1,
        "a client built pre-authenticated must still re-verify the token"
    );
}

#[tokio::test]
async fn session_reports_a_revoked_token_as_data() {
    let server = session_endpoint(401).await;
    let session = session_of(&server).await;

    // The actionable half of the split: this one means re-authenticate.
    assert_eq!(session["status"], "INVALID_CREDENTIALS");
    assert!(session["username"].is_null());
    assert_eq!(session["capabilities"], json!([]));
    assert!(
        session["detail"]
            .as_str()
            .unwrap()
            .contains("Invalid API token")
    );
}

#[tokio::test]
async fn session_reports_a_server_that_wont_answer() {
    let server = session_endpoint(503).await;
    let session = session_of(&server).await;

    // Not INVALID_CREDENTIALS — nothing here says the token is bad, and telling
    // the user to re-authenticate over a 503 would be a lie.
    assert_eq!(session["status"], "UNREACHABLE");
    assert!(session["username"].is_null());
    assert!(!session["detail"].is_null());
}

#[tokio::test]
async fn session_does_not_call_rate_limiting_a_credential_problem() {
    let server = session_endpoint(429).await;
    assert_eq!(session_of(&server).await["status"], "UNREACHABLE");
}

fn carddav_creds() -> CardDavCreds {
    CardDavCreds {
        username: Some("test@example.com".into()),
        app_password: Some("app-password".into()),
    }
}

/// `session { carddavConfigured }` against `carddav`, without touching mail.
async fn carddav_configured(server: &MockServer, carddav: CardDavCreds) -> bool {
    let resp = run_with_carddav(server, "{ session { carddavConfigured } }", carddav).await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    resp.data.into_json().unwrap()["session"]["carddavConfigured"]
        .as_bool()
        .expect("carddavConfigured must be a non-null Boolean")
}

#[tokio::test]
async fn carddav_is_reported_configured_only_when_both_halves_are_present() {
    let server = mock_server(1).await;

    assert!(carddav_configured(&server, carddav_creds()).await);
    assert!(
        !carddav_configured(&server, CardDavCreds::default()).await,
        "neither half present"
    );
    // CardDAV rejects API tokens, so a username on its own gets nowhere —
    // reporting it as configured would send an agent down a path that fails.
    assert!(
        !carddav_configured(
            &server,
            CardDavCreds {
                app_password: None,
                ..carddav_creds()
            }
        )
        .await,
        "username without an app password"
    );
    assert!(
        !carddav_configured(
            &server,
            CardDavCreds {
                username: None,
                ..carddav_creds()
            }
        )
        .await,
        "app password without a username"
    );
}

#[tokio::test]
async fn carddav_state_survives_a_dead_token() {
    // The whole point of answering this on `Session`: the two credentials are
    // unrelated, so a revoked API token must not make contact reachability
    // unanswerable — that is precisely when a caller is re-planning.
    let server = session_endpoint(401).await;
    let resp = run_with_carddav(
        &server,
        "{ session { status carddavConfigured } }",
        carddav_creds(),
    )
    .await;

    let session = resp.data.into_json().unwrap()["session"].clone();
    assert_eq!(session["status"], "INVALID_CREDENTIALS");
    assert_eq!(session["carddavConfigured"], true);
}

#[tokio::test]
async fn contacts_and_session_agree_about_missing_credentials() {
    let server = mock_server(1).await;
    assert!(!carddav_configured(&server, CardDavCreds::default()).await);

    // The flag exists to be trusted, so the operation it describes has to fail
    // for the reason it advertised — and before any network call.
    let resp = run(&server, "{ contacts(query: \"anyone\") { nodes { id } } }").await;
    assert!(
        resp.errors
            .iter()
            .any(|e| e.message.contains("Username not configured")),
        "got {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn all_contact_mutations_require_request_scoped_credentials() {
    let server = mock_server(1).await;
    for query in [
        "mutation { createContact(name: \"Test\") { id } }",
        "mutation { updateContact(id: \"id\", name: \"Test\") { id } }",
        "mutation { deleteContact(id: \"id\") { success } }",
    ] {
        for username in [None, Some("first@example.com"), Some("second@example.com")] {
            let response = run_with_carddav(
                &server,
                query,
                CardDavCreds {
                    username: username.map(str::to_owned),
                    app_password: None,
                },
            )
            .await;
            assert!(
                response
                    .errors
                    .iter()
                    .any(|error| error.message.contains("not configured for this request")),
                "{:?}",
                response.errors
            );
        }
    }
}
