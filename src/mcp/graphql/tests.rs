//! Shared fixtures for real-schema tests against a local JMAP server.

use std::sync::Arc;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{CardDavCreds, SharedClient, build_schema, request};
use crate::jmap::JmapClient;

/// One `Email/get`-shaped invocation recorded from a request body.
struct Call {
    method: String,
    ids: Vec<String>,
    properties: Vec<String>,
}

/// Pull every method call out of the recorded JMAP request bodies, in order.
async fn calls(server: &MockServer) -> Vec<Call> {
    let mut out = Vec::new();
    for req in server.received_requests().await.unwrap_or_default() {
        let body: Value = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let Some(method_calls) = body.get("methodCalls").and_then(Value::as_array) else {
            continue;
        };
        for mc in method_calls {
            let name = mc.get(0).and_then(Value::as_str).unwrap_or("").to_string();
            let args = mc.get(1).cloned().unwrap_or(Value::Null);
            let str_list = |key: &str| -> Vec<String> {
                args.get(key)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            out.push(Call {
                method: name,
                ids: str_list("ids"),
                properties: str_list("properties"),
            });
        }
    }
    out
}

fn email_json(id: &str, thread: &str, with_body: bool) -> Value {
    // Ordering matters (threads sort oldest-first), so derive a distinct
    // timestamp from the numeric suffix of the id rather than a constant.
    let n: u32 = id.trim_start_matches('e').parse().unwrap_or(0);
    let mut e = json!({
        "id": id,
        "blobId": format!("blob-{id}"),
        "threadId": thread,
        "mailboxIds": { "mb1": true },
        "keywords": {},
        "size": 1234,
        "receivedAt": format!("2024-01-{:02}T00:00:00Z", n + 1),
        "subject": format!("Subject {id}"),
        "from": [{ "name": "Sender", "email": "sender@example.com" }],
        "sender": [{ "name": "On Behalf Of", "email": "agent@example.com" }],
        "preview": "preview text",
        // Present in the summary property set, so a list result already knows
        // an attachment exists without fetching its metadata.
        "hasAttachment": true,
    });
    if with_body {
        e["textBody"] = json!([{ "partId": "1", "type": "text/plain" }]);
        e["bodyValues"] = json!({ "1": { "value": format!("Body of {id}") } });
        e["headers"] = json!([
            { "name": "List-Unsubscribe", "value": "<https://example.com/u>" },
            { "name": "Received", "value": "from mx.example.com" },
        ]);
        e["attachments"] = json!([{
            "partId": "2",
            "blobId": format!("blob-att-{id}"),
            "name": "notes.txt",
            "type": "text/plain",
            "size": ATTACHMENT_BYTES.len(),
            "charset": "utf-8",
            "disposition": "attachment",
            "cid": format!("cid-{id}"),
        }]);
    }
    e
}

/// Body served for every blob download in these tests.
const ATTACHMENT_BYTES: &[u8] = b"attachment payload";

/// Blob downloads are plain GETs, not JMAP method calls, so they are counted
/// separately from [`calls`] — this is what the laziness tests assert on.
async fn downloads(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method == wiremock::http::Method::GET)
        .map(|r| r.url.path().to_string())
        .filter(|p| p.contains("/download/"))
        .collect()
}

fn session_json(uri: &str) -> Value {
    json!({
        "capabilities": {
            "urn:ietf:params:jmap:mail": {},
            "urn:ietf:params:jmap:core": {},
        },
        "accounts": {
            "acct1": { "name": "test@example.com", "isPersonal": true, "isReadOnly": false },
            "acct2": { "name": "Shared", "isPersonal": false, "isReadOnly": true },
        },
        "primaryAccounts": { "urn:ietf:params:jmap:mail": "acct1" },
        "username": "test@example.com",
        "apiUrl": format!("{uri}/jmap"),
        "downloadUrl": format!("{uri}/jmap/download/{{blobId}}"),
        "uploadUrl": format!("{uri}/jmap/upload"),
    })
}

/// Mock JMAP endpoint that answers `Mailbox/get`, `Email/query`, `Email/get`
/// and `Thread/get` for a fixed inbox of `count` emails.
async fn mock_server(count: usize) -> MockServer {
    let server = MockServer::start().await;
    let ids: Vec<String> = (0..count).map(|i| format!("e{i}")).collect();

    let responder = move |req: &wiremock::Request| -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let empty = vec![];
        let method_calls = body
            .get("methodCalls")
            .and_then(Value::as_array)
            .unwrap_or(&empty);

        let mut responses = Vec::new();
        for mc in method_calls {
            let name = mc.get(0).and_then(Value::as_str).unwrap_or("");
            let args = mc.get(1).cloned().unwrap_or(Value::Null);
            let tag = mc.get(2).and_then(Value::as_str).unwrap_or("c0");

            let payload = match name {
                "Mailbox/get" => json!({ "list": [{
                    "id": "mb1", "name": "Inbox", "role": "inbox",
                    "totalEmails": 10, "unreadEmails": 2,
                    "totalThreads": 8, "unreadThreads": 2, "sortOrder": 0,
                    "isSubscribed": true,
                    "myRights": {
                        "mayReadItems": true, "mayAddItems": true,
                        "mayRemoveItems": true, "maySetSeen": true,
                        "maySetKeywords": true, "mayCreateChild": false,
                        "mayRename": false, "mayDelete": false, "maySubmit": true
                    }
                }] }),
                "Email/query" => {
                    let total = ids.len() as i64;
                    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(total) as usize;

                    // Resolve the window start the way a JMAP server would:
                    // anchor + offset when given, otherwise position, where a
                    // negative value counts back from the end.
                    let start: i64 = match args.get("anchor").and_then(Value::as_str) {
                        Some(anchor) => {
                            let Some(idx) = ids.iter().position(|i| i == anchor) else {
                                return ResponseTemplate::new(200).set_body_json(json!({
                                    "methodResponses": [[
                                        "error",
                                        { "type": "anchorNotFound",
                                          "description": "anchor not in results" },
                                        tag
                                    ]]
                                }));
                            };
                            idx as i64
                                + args
                                    .get("anchorOffset")
                                    .and_then(Value::as_i64)
                                    .unwrap_or(0)
                        }
                        None => {
                            let p = args.get("position").and_then(Value::as_i64).unwrap_or(0);
                            if p < 0 { total + p } else { p }
                        }
                    };
                    let start = start.max(0) as usize;
                    let window: Vec<&String> = ids.iter().skip(start).take(limit).collect();
                    let mut payload =
                        json!({ "ids": window, "position": start, "queryState": "qs-1" });
                    if args.get("calculateTotal").and_then(Value::as_bool) == Some(true) {
                        payload["total"] = json!(ids.len());
                    }
                    payload
                }
                "Email/get" => {
                    // A back-reference (`#ids`) means this is the list fetch;
                    // an explicit `ids` array means a targeted (batched) fetch.
                    let requested: Vec<String> = match args.get("ids").and_then(Value::as_array) {
                        Some(a) => a
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                        None => ids.clone(),
                    };
                    let full = args
                        .get("properties")
                        .and_then(Value::as_array)
                        .is_some_and(|p| p.iter().any(|v| v.as_str() == Some("textBody")));
                    json!({
                        "list": requested.iter()
                            .map(|id| email_json(id, "t1", full))
                            .collect::<Vec<_>>(),
                        "notFound": []
                    })
                }
                "Thread/get" => json!({ "list": [{ "id": "t1", "emailIds": ids }] }),
                _ => json!({ "list": [], "notFound": [] }),
            };
            responses.push(json!([name, payload, tag]));
        }

        ResponseTemplate::new(200).set_body_json(json!({ "methodResponses": responses }))
    };

    Mock::given(method("POST"))
        .and(path("/jmap"))
        .respond_with(responder)
        .mount(&server)
        .await;

    // The session handshake, which `session` re-runs. Mounted ahead of the
    // catch-all GET so it is not answered with blob bytes.
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_json(&server.uri())))
        .mount(&server)
        .await;

    // Blob downloads: one GET per blob, which the laziness tests count.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(ATTACHMENT_BYTES))
        .mount(&server)
        .await;

    server
}

fn client_for(server: &MockServer) -> SharedClient {
    Arc::new(tokio::sync::Mutex::new(JmapClient::with_test_session(
        &format!("{}/jmap", server.uri()),
    )))
}

async fn run(server: &MockServer, query: &str) -> async_graphql::Response {
    run_with_carddav(server, query, CardDavCreds::default()).await
}

/// As [`run`], with CardDAV credentials supplied. Injected rather than read from
/// the environment so these tests don't depend on the machine running them.
async fn run_with_carddav(
    server: &MockServer,
    query: &str,
    carddav: CardDavCreds,
) -> async_graphql::Response {
    let schema = build_schema();
    schema
        .execute(request(query, client_for(server), carddav))
        .await
}

mod attachments;
mod connections;
mod mutations;
mod readable_body;
mod resolution;
mod schema;
mod session;
mod subscriptions;
