//! Watching for newly arrived mail.
//!
//! The cursor is the point: JMAP's push channel says only *something changed*,
//! so this holds an `Email` state string and asks `Email/changes` what that
//! actually was. Push, polling and reconnect-after-a-drop all funnel through
//! the same call, which is why a lost notification costs latency rather than
//! mail — and why `--poll` is a timer swapped for a socket, not a second
//! implementation.

use crate::error::{Error, Result};
use crate::jmap::{EventParser, JmapClient};
use crate::models::Email;
use std::time::Duration;
use tracing::debug;

/// A client shared between a watcher and whatever else is using the connection.
///
/// The lock is held only across individual JMAP calls, never across a read of
/// the push channel — that read blocks until mail arrives, which on a quiet
/// account is hours.
/// All holders must use the client on the same Tokio runtime; see [`JmapClient`].
pub type SharedJmapClient = std::sync::Arc<tokio::sync::Mutex<JmapClient>>;

/// How often to ask the server for a keep-alive, and the basis for the read
/// timeout that notices a connection which died without saying so.
const PING_SECONDS: u32 = 30;

/// Reconnect backoff bounds. The ceiling is deliberately small: a watcher that
/// goes quiet for minutes after a blip is indistinguishable from a broken one.
const BACKOFF_START: u64 = 1;
const BACKOFF_MAX: u64 = 30;

/// What one wake-up turned up.
pub struct Arrivals {
    /// New emails, oldest first. Empty is normal — a wake-up is a prompt to
    /// look, not a promise of mail.
    pub emails: Vec<Email>,
    /// The server had discarded change history past our cursor, so it was reset
    /// to the present. Anything that arrived in the gap was never reported and
    /// now never will be.
    pub resynced: bool,
}

/// How the watcher learns it should look again.
enum Wake {
    Push {
        /// The live response, or `None` when it needs (re)opening.
        stream: Option<reqwest::Response>,
        parser: EventParser,
        backoff: u64,
        last_event_id: Option<String>,
    },
    Poll(Duration),
}

pub struct ArrivalWatcher {
    client: SharedJmapClient,
    state: String,
    mailbox_id: Option<String>,
    full: bool,
    wake: Wake,
    retry_pending: bool,
    retry_backoff: u64,
}

impl ArrivalWatcher {
    /// Start watching from the present moment.
    ///
    /// `poll` swaps the push connection for a timer of that interval; without
    /// it the watcher holds JMAP's event source open.
    pub async fn new(
        client: SharedJmapClient,
        mailbox: Option<&str>,
        full: bool,
        poll: Option<Duration>,
    ) -> Result<Self> {
        if poll.is_some_and(|interval| {
            interval < Duration::from_secs(1)
                || tokio::time::Instant::now().checked_add(interval).is_none()
        }) {
            return Err(Error::Config(
                "Polling interval must be at least one second and fit the system clock".into(),
            ));
        }
        let (mailbox_id, state) = {
            let mut locked = client.lock().await;
            let mailbox_id = match mailbox {
                Some(name) => Some(locked.find_mailbox(name).await?.id),
                None => None,
            };
            // Start from now: the caller asked what arrives next, not what is
            // already sitting there.
            (mailbox_id, locked.email_state().await?)
        };

        Ok(Self {
            client,
            state,
            mailbox_id,
            full,
            retry_pending: false,
            retry_backoff: BACKOFF_START,
            wake: match poll {
                Some(interval) => Wake::Poll(interval),
                None => Wake::Push {
                    stream: None,
                    parser: EventParser::default(),
                    backoff: BACKOFF_START,
                    last_event_id: None,
                },
            },
        })
    }

    /// Block until the next wake-up, then report what arrived.
    ///
    /// Errors are returned only when the watcher can never succeed again — a
    /// dead credential. Everything else is transient by nature (a dropped
    /// connection, a bad response, a server that has forgotten our cursor) and
    /// is retried internally with backoff, because a watcher that exits on one
    /// blip is useless in the loop it exists to feed.
    pub async fn next_arrivals(&mut self) -> Result<Arrivals> {
        if self.retry_pending {
            sleep_backoff(&mut self.retry_backoff).await;
        } else {
            self.wait().await?;
        }
        match self.drain().await {
            Ok(arrivals) => {
                self.retry_pending = false;
                self.retry_backoff = BACKOFF_START;
                Ok(arrivals)
            }
            Err(e) if is_fatal(&e) => Err(e),
            Err(e) => {
                debug!("Could not reconcile arrivals ({e}); retrying");
                self.retry_pending = true;
                Ok(Arrivals::none())
            }
        }
    }

    /// Wait until there is reason to believe something changed.
    async fn wait(&mut self) -> Result<()> {
        let interval = match &mut self.wake {
            Wake::Poll(interval) => *interval,
            Wake::Push { .. } => return self.wait_for_push().await,
        };
        tokio::time::sleep(interval).await;
        Ok(())
    }

    async fn wait_for_push(&mut self) -> Result<()> {
        loop {
            let Wake::Push {
                stream,
                parser,
                backoff,
                last_event_id,
            } = &mut self.wake
            else {
                unreachable!("wait_for_push is only entered for Wake::Push")
            };

            if stream.is_none() {
                // Opened under the lock, read outside it: the read blocks for
                // as long as the account is quiet.
                let opened = {
                    let client = self.client.lock().await;
                    client
                        .open_event_stream(PING_SECONDS, last_event_id.as_deref())
                        .await
                };
                match opened {
                    Ok(resp) => *stream = Some(resp),
                    Err(e) if is_fatal(&e) => return Err(e),
                    Err(e) => {
                        debug!("Could not open event stream ({e}); retrying");
                        sleep_backoff(backoff).await;
                        // Reconcile anyway: while push is down, each retry is
                        // also a poll tick, so mail still surfaces.
                        return Ok(());
                    }
                }
            }

            let chunk = stream
                .as_mut()
                .expect("stream was just opened")
                .chunk()
                .await;

            match chunk {
                Ok(Some(bytes)) => {
                    // Reset only once the connection has carried something.
                    // Resetting on connect alone would let a server that
                    // accepts and immediately hangs up spin at full rate.
                    *backoff = BACKOFF_START;

                    let mut changed = false;
                    let events = match parser.feed_bytes(&bytes) {
                        Ok(events) => events,
                        Err(message) => {
                            debug!("Invalid event stream ({message}); reconnecting");
                            *stream = None;
                            *parser = EventParser::default();
                            sleep_backoff(backoff).await;
                            return Ok(());
                        }
                    };
                    for event in events {
                        if let Some(id) = event.id {
                            *last_event_id = Some(id);
                        }
                        // Keep-alives carry no state change.
                        if event.event.as_deref() == Some("ping") || event.data.is_empty() {
                            continue;
                        }
                        changed = true;
                    }
                    if changed {
                        return Ok(());
                    }
                }
                Ok(None) | Err(_) => {
                    debug!("Event stream ended; reconnecting");
                    *stream = None;
                    *parser = EventParser::default();
                    sleep_backoff(backoff).await;
                    // Reconcile across the gap before waiting again, so mail
                    // that landed while disconnected is reported now rather
                    // than whenever the next message happens to arrive.
                    return Ok(());
                }
            }
        }
    }

    /// Advance the cursor and collect whatever it turned up.
    async fn drain(&mut self) -> Result<Arrivals> {
        let client = self.client.lock().await;

        let changes = match client.email_changes(&self.state).await {
            Ok(changes) => changes,
            Err(Error::Jmap { ref error_type, .. }) if error_type == "cannotCalculateChanges" => {
                // History is gone back to our cursor. There is no way to know
                // what was missed, and replaying the mailbox as "new" would be
                // a lie, so reset to the present and say so.
                self.state = client.email_state().await?;
                return Ok(Arrivals {
                    emails: Vec::new(),
                    resynced: true,
                });
            }
            Err(e) => return Err(e),
        };

        if changes.created.is_empty() {
            self.state = changes.new_state;
            return Ok(Arrivals::none());
        }

        let fetched = if self.full {
            client.get_emails(&changes.created).await
        } else {
            client.get_email_summaries(&changes.created).await
        };

        let mut emails = fetched?;

        emails.retain(|email| in_mailbox(email, self.mailbox_id.as_deref()));
        // `Email/get` makes no ordering guarantee, and a stream reads
        // chronologically.
        emails.sort_by(|a, b| a.received_at.cmp(&b.received_at));
        self.state = changes.new_state;

        Ok(Arrivals {
            emails,
            resynced: false,
        })
    }
}

impl Arrivals {
    fn none() -> Self {
        Self {
            emails: Vec::new(),
            resynced: false,
        }
    }
}

async fn sleep_backoff(backoff: &mut u64) {
    tokio::time::sleep(Duration::from_secs(*backoff)).await;
    *backoff = (*backoff * 2).min(BACKOFF_MAX);
}

fn in_mailbox(email: &Email, mailbox_id: Option<&str>) -> bool {
    mailbox_id.is_none_or(|id| email.mailbox_ids.contains_key(id))
}

/// Whether an error means the watcher can never succeed again.
fn is_fatal(e: &Error) -> bool {
    matches!(e, Error::InvalidToken(_) | Error::NotAuthenticated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jmap::tests::{jmap_response, mock_client};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, method},
    };

    fn watcher(client: JmapClient, full: bool) -> ArrivalWatcher {
        ArrivalWatcher {
            client: std::sync::Arc::new(tokio::sync::Mutex::new(client)),
            state: "s0".into(),
            mailbox_id: None,
            full,
            wake: Wake::Poll(Duration::from_secs(1)),
            retry_pending: false,
            retry_backoff: BACKOFF_START,
        }
    }

    async fn arrivals_server(created: &[&str]) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("Email/changes"))
            .and(body_string_contains(r#""sinceState":"s0""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response("Email/changes", json!({
                "newState": "s1", "created": created, "updated": [], "destroyed": [], "hasMoreChanges": false
            }))))
            .mount(&server).await;
        Mock::given(method("POST"))
            .and(body_string_contains("Email/get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
                "Email/get",
                json!({
                    "state": "s1", "list": created.iter().map(|id| json!({"id":id})).collect::<Vec<_>>(), "notFound": []
                }),
            )))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn failed_detail_fetch_preserves_cursor_and_retries_without_a_push() {
        for full in [true, false] {
            let server = arrivals_server(&["e1"]).await;
            Mock::given(method("POST"))
                .and(body_string_contains("Email/get"))
                .respond_with(ResponseTemplate::new(503))
                .up_to_n_times(1)
                .with_priority(1)
                .mount(&server)
                .await;
            let mut watcher = watcher(mock_client(&server.uri()), full);
            watcher.retry_pending = true;
            watcher.retry_backoff = 0;
            assert!(watcher.next_arrivals().await.unwrap().emails.is_empty());
            assert_eq!(watcher.state, "s0");
            assert!(watcher.retry_pending);
            watcher.wake = Wake::Push {
                stream: None,
                parser: EventParser::default(),
                backoff: 1,
                last_event_id: None,
            };
            let arrivals = watcher.next_arrivals().await.unwrap();
            assert_eq!(arrivals.emails[0].id, "e1");
            assert_eq!(watcher.state, "s1");
            assert!(!watcher.retry_pending);
        }
    }

    #[tokio::test]
    async fn invalid_push_frame_reconciles_instead_of_ending_watch() {
        let server = arrivals_server(&["e1"]).await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("data: exceeds limit\n\n", "text/event-stream"),
            )
            .mount(&server)
            .await;
        let mut client = mock_client(&server.uri());
        client.session.as_mut().unwrap().event_source_url = Some(server.uri());
        let stream = client.open_event_stream(30, None).await.unwrap();
        let mut watcher = watcher(client, false);
        watcher.wake = Wake::Push {
            stream: Some(stream),
            parser: EventParser::with_limit(16),
            backoff: 1,
            last_event_id: None,
        };
        assert_eq!(watcher.next_arrivals().await.unwrap().emails[0].id, "e1");
        assert!(matches!(watcher.wake, Wake::Push { stream: None, .. }));
    }

    #[tokio::test]
    async fn empty_changes_advance_the_cursor_without_fetching_details() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("Email/changes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
                "Email/changes",
                json!({"newState":"s1", "created":[], "updated":["old"], "destroyed":["gone"], "hasMoreChanges":false}),
            )))
            .expect(1)
            .mount(&server)
            .await;
        let mut watcher = watcher(mock_client(&server.uri()), true);
        let arrivals = watcher.drain().await.unwrap();
        assert!(arrivals.emails.is_empty());
        assert!(!arrivals.resynced);
        assert_eq!(watcher.state, "s1");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn arrivals_are_filtered_and_sorted_for_full_and_summary_fetches() {
        for full in [true, false] {
            let server = arrivals_server(&["late", "other", "early"]).await;
            Mock::given(method("POST"))
                .and(body_string_contains("Email/get"))
                .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
                    "Email/get",
                    json!({"list":[
                        {"id":"late", "mailboxIds":{"inbox":true}, "receivedAt":"2024-01-03T00:00:00Z"},
                        {"id":"other", "mailboxIds":{"archive":true}, "receivedAt":"2024-01-01T00:00:00Z"},
                        {"id":"early", "mailboxIds":{"inbox":true}, "receivedAt":"2024-01-02T00:00:00Z"}
                    ], "notFound": []}),
                )))
                .with_priority(1)
                .expect(1)
                .mount(&server)
                .await;
            let mut watcher = watcher(mock_client(&server.uri()), full);
            watcher.mailbox_id = Some("inbox".into());
            let arrivals = watcher.drain().await.unwrap();
            assert_eq!(
                arrivals
                    .emails
                    .iter()
                    .map(|e| e.id.as_str())
                    .collect::<Vec<_>>(),
                ["early", "late"]
            );
            assert_eq!(watcher.state, "s1");
            assert!(!arrivals.resynced);
            let requests = server.received_requests().await.unwrap();
            let fetched: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
            let args = &fetched["methodCalls"][0][1];
            assert_eq!(args["ids"], json!(["late", "other", "early"]));
            assert_eq!(args["fetchTextBodyValues"], full);
            assert_eq!(args["fetchHTMLBodyValues"], full);
            assert_eq!(
                args["properties"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("textBody")),
                full
            );
        }
    }

    #[tokio::test]
    async fn history_resync_commits_only_after_fetching_the_current_state() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("Email/changes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
                "error",
                json!({"type":"cannotCalculateChanges"}),
            )))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("Email/get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jmap_response(
                "Email/get",
                json!({"state":"present", "list":[]}),
            )))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("Email/get"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        let mut watcher = watcher(mock_client(&server.uri()), false);
        watcher.retry_pending = true;
        watcher.retry_backoff = 0;
        let failed = watcher.next_arrivals().await.unwrap();
        assert!(failed.emails.is_empty());
        assert!(!failed.resynced);
        assert!(watcher.retry_pending);
        assert_eq!(watcher.state, "s0");

        let recovered = watcher.next_arrivals().await.unwrap();
        assert!(recovered.emails.is_empty());
        assert!(recovered.resynced);
        assert_eq!(watcher.state, "present");
        assert!(!watcher.retry_pending);
        assert_eq!(watcher.retry_backoff, BACKOFF_START);
        for request in server.received_requests().await.unwrap() {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let call = &body["methodCalls"][0];
            if call[0] == "Email/get" {
                assert_eq!(call[1]["ids"], json!([]), "Resync must not replay old mail");
            } else {
                assert_eq!(call[1]["sinceState"], "s0");
            }
        }
    }

    #[tokio::test]
    async fn authentication_failures_end_watch_during_push_or_reconciliation() {
        for push in [true, false] {
            let server = MockServer::start().await;
            Mock::given(method(if push { "GET" } else { "POST" }))
                .respond_with(ResponseTemplate::new(401))
                .expect(1)
                .mount(&server)
                .await;
            let mut client = mock_client(&server.uri());
            client.session.as_mut().unwrap().event_source_url = Some(server.uri());
            let mut watcher = watcher(client, false);
            watcher.wake = Wake::Push {
                stream: None,
                parser: EventParser::default(),
                backoff: 1,
                last_event_id: None,
            };
            watcher.retry_pending = !push;
            watcher.retry_backoff = 0;
            assert!(matches!(
                watcher.next_arrivals().await,
                Err(Error::InvalidToken(_))
            ));
            assert_eq!(watcher.state, "s0");
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn push_reconnect_preserves_event_ids_and_ignores_keepalives() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("id: change-id\ndata: {}\n\n", "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "id: ping-id\nevent: ping\ndata: keepalive\n\n",
                "text/event-stream",
            ))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        let mut client = mock_client(&server.uri());
        client.session.as_mut().unwrap().event_source_url = Some(server.uri());
        let mut watcher = watcher(client, false);
        watcher.wake = Wake::Push {
            stream: None,
            parser: EventParser::default(),
            backoff: 1,
            last_event_id: None,
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            watcher.wait_for_push().await.unwrap();
            assert!(matches!(&watcher.wake, Wake::Push { stream: None, last_event_id: Some(id), .. } if id == "ping-id"));
            watcher.wait_for_push().await.unwrap();
            assert!(matches!(&watcher.wake, Wake::Push { stream: Some(_), last_event_id: Some(id), .. } if id == "change-id"));
        }).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        assert!(!requests[0].headers.contains_key("last-event-id"));
        assert_eq!(requests[1].headers["last-event-id"], "ping-id");
    }

    #[tokio::test]
    async fn invalid_poll_intervals_are_rejected_before_network_access() {
        let server = MockServer::start().await;
        let client = std::sync::Arc::new(tokio::sync::Mutex::new(mock_client(&server.uri())));
        for interval in [Duration::ZERO, Duration::from_millis(999), Duration::MAX] {
            let result = ArrivalWatcher::new(client.clone(), None, false, Some(interval)).await;
            assert!(
                matches!(result, Err(Error::Config(message)) if message == "Polling interval must be at least one second and fit the system clock")
            );
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn watching_starts_at_the_present_state_after_resolving_the_mailbox() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(|request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let call = &body["methodCalls"][0];
                let response = match call[0].as_str().unwrap() {
                    "Mailbox/get" => jmap_response(
                        "Mailbox/get",
                        json!({"list":[{"id":"mb1", "name":"Inbox", "role":"inbox"}]}),
                    ),
                    "Email/get" => {
                        assert_eq!(call[1]["ids"], json!([]));
                        jmap_response("Email/get", json!({"state":"present", "list":[]}))
                    }
                    method => panic!("Unexpected {method} during watcher initialization"),
                };
                ResponseTemplate::new(200).set_body_json(response)
            })
            .mount(&server)
            .await;
        let client = std::sync::Arc::new(tokio::sync::Mutex::new(mock_client(&server.uri())));
        let watcher = ArrivalWatcher::new(
            client.clone(),
            Some("INBOX"),
            true,
            Some(Duration::from_secs(1)),
        )
        .await
        .unwrap();
        assert_eq!(watcher.state, "present");
        assert_eq!(watcher.mailbox_id.as_deref(), Some("mb1"));
        assert!(watcher.full);
        assert!(!watcher.retry_pending);
        assert!(matches!(watcher.wake, Wake::Poll(interval) if interval == Duration::from_secs(1)));
        let result = ArrivalWatcher::new(client, Some("missing"), false, None).await;
        assert!(matches!(result, Err(Error::MailboxNotFound(name)) if name == "missing"));
        let methods: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                body["methodCalls"][0][0].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(methods, ["Mailbox/get", "Email/get", "Mailbox/get"]);
    }

    fn email_in(mailbox_ids: &[&str]) -> Email {
        let ids: serde_json::Map<_, _> = mailbox_ids
            .iter()
            .map(|id| (id.to_string(), serde_json::json!(true)))
            .collect();
        serde_json::from_value(serde_json::json!({ "id": "e1", "mailboxIds": ids })).unwrap()
    }

    #[test]
    fn no_mailbox_filter_accepts_everything() {
        assert!(in_mailbox(&email_in(&["archive"]), None));
    }

    #[test]
    fn mailbox_filter_matches_membership() {
        let email = email_in(&["inbox", "important"]);
        assert!(in_mailbox(&email, Some("inbox")));
        assert!(!in_mailbox(&email, Some("trash")));
    }

    #[test]
    fn a_dead_credential_is_fatal_but_a_bad_response_is_not() {
        assert!(is_fatal(&Error::NotAuthenticated));
        assert!(is_fatal(&Error::InvalidToken("nope")));
        assert!(!is_fatal(&Error::RateLimited));
        assert!(!is_fatal(&Error::Server("boom".into())));
    }

    // Paused clock: the sleeps are the point of the function, but waiting out
    // half a minute of them is not.
    #[tokio::test(start_paused = true)]
    async fn backoff_doubles_up_to_the_ceiling() {
        let mut backoff = BACKOFF_START;
        sleep_backoff(&mut backoff).await;
        assert_eq!(backoff, BACKOFF_START * 2);

        let mut backoff = BACKOFF_MAX - 1;
        sleep_backoff(&mut backoff).await;
        assert_eq!(backoff, BACKOFF_MAX);
        sleep_backoff(&mut backoff).await;
        assert_eq!(
            backoff, BACKOFF_MAX,
            "backoff must not grow past the ceiling"
        );
    }
}
