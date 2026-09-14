mod events;
mod watch;

use crate::commands::SearchFilter;
use crate::error::{Error, Result};
use crate::models::*;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, instrument};

pub use events::{EventParser, ServerEvent};
pub use watch::{ArrivalWatcher, Arrivals, SharedJmapClient};

const SESSION_URL: &str = "https://api.fastmail.com/jmap/session";

/// Properties fetched for list/search results: everything that is cheap to
/// serialise. Bodies and attachment metadata are deliberately excluded — those
/// are pulled on demand, in one batched call, by [`JmapClient::get_emails`].
pub const EMAIL_SUMMARY_PROPERTIES: &[&str] = &[
    "id",
    "blobId",
    "threadId",
    "mailboxIds",
    "keywords",
    "size",
    "receivedAt",
    "sentAt",
    "sender",
    "from",
    "to",
    "cc",
    "bcc",
    "replyTo",
    "subject",
    "preview",
    "hasAttachment",
];

/// The summary set plus bodies, attachment metadata and threading headers —
/// the expensive fetch.
pub const EMAIL_FULL_PROPERTIES: &[&str] = &[
    "id",
    "blobId",
    "threadId",
    "mailboxIds",
    "keywords",
    "size",
    "receivedAt",
    "sentAt",
    "messageId",
    "inReplyTo",
    "references",
    "sender",
    "from",
    "to",
    "cc",
    "bcc",
    "replyTo",
    "subject",
    "preview",
    "hasAttachment",
    "textBody",
    "htmlBody",
    "attachments",
    "bodyValues",
    "headers",
];

const DESIRED_CAPABILITIES: &[&str] = &[
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
    "https://www.fastmail.com/dev/maskedemail",
];

/// JMAP client with runtime-bound connection pools.
///
/// Keep a client and all its clones on the Tokio runtime where it was constructed.
/// A client constructed outside a runtime must be used on only one runtime.
/// For multiple runtimes, construct separate clients inside each runtime.
#[derive(Clone)]
pub struct JmapClient {
    client: Client,
    event_client: Arc<std::sync::Mutex<Option<(u32, Client)>>>,
    token: String,
    session_url: String,
    session: Option<Session>,
    available_capabilities: Vec<String>,
    server: Option<crate::remote::HttpServer>,
}

/// Create an authenticated JMAP client from config
pub async fn authenticated_client() -> crate::error::Result<JmapClient> {
    let mut client = match crate::remote::HttpServer::current() {
        Some(server) => JmapClient::try_via_server(server)?,
        None => {
            let config = crate::config::Config::load()?;
            JmapClient::try_new(config.get_token()?)?
        }
    };
    client.authenticate().await?;
    Ok(client)
}

/// File attachment data ready for upload
pub struct AttachmentData {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

/// Common parameters for compose operations (send, reply, forward)
pub struct ComposeParams<'a> {
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    /// Sender address or `Name <address>`; exact and domain identities are supported.
    pub from: Option<&'a str>,
    pub draft: bool,
    pub html_body: Option<String>,
    pub attachments: Vec<AttachmentData>,
}

/// Threading headers for reply/forward
struct ThreadingHeaders {
    in_reply_to: Vec<String>,
    references: Vec<String>,
}

/// Bundled content for the create_and_submit_email helper
struct EmailDraft<'a> {
    to: &'a [EmailAddress],
    cc: &'a [EmailAddress],
    bcc: &'a [EmailAddress],
    subject: &'a str,
    body: &'a str,
    html_body: Option<&'a str>,
    attachments: Vec<AttachmentData>,
    threading: Option<ThreadingHeaders>,
}

/// An attachment after blob upload — holds the server-assigned blobId.
#[derive(Debug)]
struct UploadedAttachment {
    blob_id: String,
    filename: String,
    content_type: String,
}

pub(crate) fn prefixed_subject(subject: Option<&str>, prefix: &str) -> String {
    let subject = subject.unwrap_or("");
    if subject.to_lowercase().starts_with(&prefix.to_lowercase()) {
        subject.to_string()
    } else {
        format!("{prefix} {subject}")
    }
}

fn addresses_json(addresses: &[EmailAddress]) -> Value {
    Value::Array(
        addresses
            .iter()
            .map(|address| json!({"email": address.email, "name": address.name}))
            .collect(),
    )
}

/// Build bodyValues and body structure fields on `email_create`.
///
/// Handles three JMAP body modes:
/// - Plain text only → `textBody` array
/// - Text + HTML (no attachments) → `textBody` + `htmlBody` arrays
/// - With attachments → explicit `bodyStructure` MIME tree
fn apply_body_structure(
    email_create: &mut HashMap<String, Value>,
    text_body: &str,
    html_body: Option<&str>,
    attachments: &[UploadedAttachment],
) {
    let mut body_values = json!({
        "textBody": { "value": text_body, "charset": "utf-8" }
    });
    if let Some(html) = html_body {
        body_values["htmlBody"] = json!({ "value": html, "charset": "utf-8" });
    }
    email_create.insert("bodyValues".into(), body_values);

    let has_html = html_body.is_some();
    let has_attachments = !attachments.is_empty();

    if has_attachments {
        let text_part = json!({ "partId": "textBody", "type": "text/plain" });
        let content_part = if has_html {
            let html_part = json!({ "partId": "htmlBody", "type": "text/html" });
            json!({ "type": "multipart/alternative", "subParts": [text_part, html_part] })
        } else {
            text_part
        };

        let mut sub_parts = vec![content_part];
        for att in attachments {
            sub_parts.push(json!({
                "blobId": att.blob_id,
                "name": att.filename,
                "type": att.content_type,
                "disposition": "attachment"
            }));
        }

        email_create.insert(
            "bodyStructure".into(),
            json!({ "type": "multipart/mixed", "subParts": sub_parts }),
        );
    } else {
        email_create.insert(
            "textBody".into(),
            json!([{ "partId": "textBody", "type": "text/plain" }]),
        );
        if has_html {
            email_create.insert(
                "htmlBody".into(),
                json!([{ "partId": "htmlBody", "type": "text/html" }]),
            );
        }
    }
}

/// Resolved context for a compose operation
struct ComposeContext {
    account_id: String,
    mailbox: Mailbox,
    sent_mailbox: Option<Mailbox>,
    identity: Option<Identity>,
    draft: bool,
}

impl ComposeContext {
    fn apply_to_email(&self, email_create: &mut HashMap<String, Value>) {
        email_create.insert(
            "mailboxIds".into(),
            json!({ self.mailbox.id.clone(): true }),
        );
        email_create.insert("keywords".into(), json!({ "$draft": true, "$seen": true }));
        if let Some(ref identity) = self.identity {
            email_create.insert(
                "from".into(),
                json!([{ "email": identity.email, "name": identity.name }]),
            );
        }
    }

    fn build_method_calls(&self, email_create: HashMap<String, Value>) -> Vec<Value> {
        let mut calls = vec![json!([
            "Email/set",
            {
                "accountId": self.account_id,
                "create": { "email": email_create }
            },
            "e0"
        ])];
        if !self.draft
            && let Some(ref identity) = self.identity
            && let Some(ref sent) = self.sent_mailbox
        {
            calls.push(json!([
                "EmailSubmission/set",
                {
                    "accountId": self.account_id,
                    "create": {
                        "submission": {
                            "identityId": identity.id,
                            "emailId": "#email"
                        }
                    },
                    "onSuccessUpdateEmail": {
                        "#submission": {
                            "mailboxIds": { (sent.id.clone()): true },
                            "keywords/$draft": null,
                            "keywords/$seen": true
                        }
                    }
                },
                "s0"
            ]));
        }
        calls
    }
}

/// How many changes to ask for per `Email/changes` call. The server may cap it
/// lower; `hasMoreChanges` then drives the next page.
const CHANGES_PAGE: u32 = 100;

/// What arrived since a known `Email` state.
#[derive(Debug)]
pub struct EmailChanges {
    /// The state to pass as `sinceState` next time.
    pub new_state: String,
    /// IDs created in that window, oldest change first.
    pub created: Vec<String>,
}

// Shared JMAP response types used across multiple methods
#[derive(Deserialize)]
struct GetResponse<T> {
    list: Vec<T>,
}

#[derive(Deserialize)]
struct QueryResponse {
    ids: Vec<String>,
    #[serde(default)]
    position: u64,
    #[serde(default)]
    total: Option<u64>,
    #[serde(default, rename = "queryState")]
    query_state: Option<String>,
}

/// Where a query window starts.
///
/// JMAP offers two ways to say this, and the anchor form is what makes stable
/// pagination possible: an index shifts whenever mail arrives, but an anchor is
/// an ID in the result set, so a cursor built from one still points at the same
/// message a minute later.
pub enum QueryStart {
    /// Zero-based index. Negative counts back from the end, so `-10` is "the
    /// last ten" without needing to know the total first.
    Position(i64),
    /// Start relative to a known ID: offset `1` is the item after it, `-n` the
    /// `n` items ending just before it.
    Anchor { id: String, offset: i64 },
}

impl Default for QueryStart {
    fn default() -> Self {
        Self::Position(0)
    }
}

/// Parameters for one `Email/query` window.
pub struct EmailQuery {
    /// A JMAP `FilterCondition` or `FilterOperator` object.
    pub filter: Value,
    /// JMAP sort comparators, most significant first.
    pub sort: Vec<Value>,
    /// Where the window begins.
    pub start: QueryStart,
    pub limit: u32,
    /// Ask the server for the total match count. Costs the server extra work,
    /// so only set it when the caller actually needs the number.
    pub calculate_total: bool,
    /// Return one email per conversation instead of every message.
    pub collapse_threads: bool,
    /// Also fetch summary records for the matched IDs, in query order.
    pub fetch_summaries: bool,
}

impl EmailQuery {
    /// Newest-first page of `limit` emails, with summaries and no total.
    pub fn first(limit: u32) -> Self {
        Self {
            filter: json!({}),
            sort: vec![json!({ "property": "receivedAt", "isAscending": false })],
            start: QueryStart::Position(0),
            limit,
            calculate_total: false,
            collapse_threads: false,
            fetch_summaries: true,
        }
    }
}

/// Normalise a date to ISO 8601, accepting a bare `YYYY-MM-DD`.
pub fn normalize_date(date: &str) -> String {
    if date.contains('T') {
        date.to_string()
    } else {
        format!("{date}T00:00:00Z")
    }
}

/// Build a flat JMAP `FilterCondition` from the CLI's [`SearchFilter`].
///
/// This is the flattened view the CLI needs. The GraphQL layer builds richer
/// `FilterOperator` trees directly — see `mcp::graphql::filter`.
pub fn search_filter_to_jmap(filter: &SearchFilter, mailbox_id: Option<&str>) -> Value {
    let mut f = json!({});

    for (key, value) in [
        ("text", filter.text.as_deref()),
        ("from", filter.from.as_deref()),
        ("to", filter.to.as_deref()),
        ("cc", filter.cc.as_deref()),
        ("bcc", filter.bcc.as_deref()),
        ("subject", filter.subject.as_deref()),
        ("body", filter.body.as_deref()),
        ("inMailbox", mailbox_id),
    ] {
        if let Some(value) = value {
            f[key] = json!(value);
        }
    }
    if filter.has_attachment {
        f["hasAttachment"] = json!(true);
    }
    if let Some(min_size) = filter.min_size {
        f["minSize"] = json!(min_size);
    }
    if let Some(max_size) = filter.max_size {
        f["maxSize"] = json!(max_size);
    }
    if let Some(ref before) = filter.before {
        f["before"] = json!(normalize_date(before));
    }
    if let Some(ref after) = filter.after {
        f["after"] = json!(normalize_date(after));
    }
    if filter.unread {
        f["notKeyword"] = json!("$seen");
    }
    if filter.flagged {
        f["hasKeyword"] = json!("$flagged");
    }

    f
}

/// One window of an `Email/query` result set.
pub struct EmailPage {
    /// Summary records in query order. Empty when summaries weren't requested.
    pub emails: Vec<Email>,
    /// The IDs this window matched, in order.
    pub ids: Vec<String>,
    /// Index of the first returned ID within the full result set.
    pub position: u64,
    /// Total matches, when `calculate_total` was set.
    pub total: Option<u64>,
    /// Opaque server state string for this query, letting a client tell whether
    /// the result set has changed since it last looked.
    pub query_state: Option<String>,
}

#[derive(Deserialize)]
struct EmailSetResponse {
    created: Option<HashMap<String, Value>>,
    #[serde(rename = "notCreated")]
    not_created: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
struct SetResponse {
    #[serde(rename = "notUpdated")]
    not_updated: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
struct MaskedEmailCreateResponse {
    created: Option<HashMap<String, MaskedEmail>>,
    #[serde(rename = "notCreated")]
    not_created: Option<HashMap<String, Value>>,
}

#[derive(Debug, Serialize)]
struct JmapRequest {
    using: Vec<String>,
    #[serde(rename = "methodCalls")]
    method_calls: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct JmapResponse {
    #[serde(rename = "methodResponses")]
    method_responses: Vec<Value>,
}

/// Substitute `{placeholder}` tokens in a URL template in a single pass.
///
/// Unlike chaining `str::replace`, this never re-scans an already-substituted
/// value, so a variable value that contains another template marker cannot
/// bleed into a later replacement.
fn apply_url_template(tmpl: &str, vars: &[(&str, &str)]) -> String {
    let mut result = String::with_capacity(tmpl.len());
    let mut rest = tmpl;
    while let Some(open) = rest.find('{') {
        result.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        if let Some(close) = after_open.find('}') {
            let key = &after_open[..close];
            match vars.iter().find(|(k, _)| *k == key) {
                Some((_, v)) => result.push_str(
                    &percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC)
                        .to_string(),
                ),
                None => {
                    // Unknown placeholder — preserve literally so a downstream
                    // system that recognises it still can.
                    result.push('{');
                    result.push_str(key);
                    result.push('}');
                }
            }
            rest = &after_open[close + 1..];
        } else {
            // Unterminated — emit the remainder verbatim and stop.
            result.push('{');
            result.push_str(after_open);
            return result;
        }
    }
    result.push_str(rest);
    result
}

fn pick_identity(identities: Vec<Identity>, from: Option<&str>) -> Result<Identity> {
    let Some(from) = from else {
        return identities
            .into_iter()
            .find(|identity| !identity.email.starts_with("*@"))
            .ok_or(Error::IdentityNotFound);
    };
    let invalid = || {
        Error::Config(
            "Invalid sender (--from or GraphQL from): use one concrete email address or Name <address>"
                .into(),
        )
    };
    if from.chars().any(char::is_control) {
        return Err(invalid());
    }
    let address: email_address::EmailAddress = from.trim().parse().map_err(|_| invalid())?;
    let name = address.display_part();
    if matches!(address.local_part(), "*" | "\"*\"" | "\"\\*\"") || name.contains(['<', '>', '@']) {
        return Err(invalid());
    }
    let email = address.email();
    let exact = identities
        .iter()
        .position(|identity| identity.email.eq_ignore_ascii_case(&email));
    let domain = identities.iter().position(|identity| {
        identity
            .email
            .strip_prefix("*@")
            .is_some_and(|domain| domain.eq_ignore_ascii_case(address.domain()))
    });
    let index = exact
        .or(domain)
        .ok_or_else(|| Error::IdentityNotFoundForEmail(email.clone()))?;
    let mut identity = identities.into_iter().nth(index).unwrap();
    if exact.is_none() {
        // RFC 8621 section 6: keep the wildcard identity ID, but use a concrete From.
        identity.email = email;
    }
    if !name.is_empty() {
        identity.name = if let Some(quoted) = name.strip_prefix('"') {
            let quoted = quoted.strip_suffix('"').ok_or_else(invalid)?;
            let mut chars = quoted.chars();
            let mut decoded = String::with_capacity(quoted.len());
            while let Some(ch) = chars.next() {
                decoded.push(match ch {
                    '\\' => chars.next().ok_or_else(invalid)?,
                    '"' => return Err(invalid()),
                    _ => ch,
                });
            }
            decoded
        } else {
            name.to_string()
        };
    }
    Ok(identity)
}

/// Build reply To/CC lists from an original email, expanding reply-all if
/// requested and filtering out the sending identity.
///
/// Returns `(to, cc)` where:
/// - `to` starts with `original.reply_to` when the sender set one, otherwise
///   `original.from`. When `reply_all` is set, the original `To` recipients are
///   appended (minus `my_email` if provided).
/// - `cc` starts with the caller-supplied `extra_cc`. When `reply_all` is
///   set, the original `Cc` recipients are appended (minus `my_email`).
///
/// Both lists are deduplicated by lowercase email, and anything already in
/// `to` is stripped from `cc` — so an overlap between `extra_cc` and
/// reply-all-expanded `to` never produces a duplicate delivery.
///
/// `my_email` is optional: pass `None` for the preview path before an
/// identity has been resolved. In that case the "filter me out" step is
/// skipped, so the resulting preview may list the user's own address —
/// still a safer failure mode than the old preview, which silently
/// under-reported recipients.
pub fn expand_reply_recipients(
    original: &Email,
    reply_all: bool,
    my_email: Option<&str>,
    extra_cc: Vec<EmailAddress>,
) -> (Vec<EmailAddress>, Vec<EmailAddress>) {
    let me_lower = my_email.map(str::to_lowercase);
    let is_me = |addr: &EmailAddress| -> bool {
        me_lower
            .as_deref()
            .is_some_and(|m| addr.email.eq_ignore_ascii_case(m))
    };

    // Reply-To wins over From, as in any mail client. Transactional and support
    // senders routinely put a branded, undeliverable address in From and the
    // inbox that actually receives mail in Reply-To — replying to From then
    // bounces.
    let mut to_addrs: Vec<EmailAddress> = original
        .reply_to
        .clone()
        .filter(|addrs| !addrs.is_empty())
        .or_else(|| original.from.clone())
        .unwrap_or_default();
    if reply_all && let Some(ref orig_to) = original.to {
        for addr in orig_to {
            if !is_me(addr) {
                to_addrs.push(addr.clone());
            }
        }
    }

    let mut cc_addrs = extra_cc;
    if reply_all && let Some(ref orig_cc) = original.cc {
        for addr in orig_cc {
            if !is_me(addr) {
                cc_addrs.push(addr.clone());
            }
        }
    }

    dedup_by_email(&mut to_addrs);
    let to_lower: std::collections::HashSet<String> =
        to_addrs.iter().map(|a| a.email.to_lowercase()).collect();
    cc_addrs.retain(|c| !to_lower.contains(&c.email.to_lowercase()));
    dedup_by_email(&mut cc_addrs);

    (to_addrs, cc_addrs)
}

fn dedup_by_email(addrs: &mut Vec<EmailAddress>) {
    let mut seen = std::collections::HashSet::<String>::new();
    addrs.retain(|a| seen.insert(a.email.to_lowercase()));
}

fn check_jmap_status(status: reqwest::StatusCode, unauthorized: &'static str) -> Result<()> {
    match status.as_u16() {
        401 => Err(Error::InvalidToken(unauthorized)),
        429 => Err(Error::RateLimited),
        500..=599 => Err(Error::Server(format!("Server error: {status}"))),
        _ => Ok(()),
    }
}

fn method_error(method: &str, error: &Value, fallback: &str) -> Error {
    Error::Jmap {
        method: method.into(),
        error_type: error
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .into(),
        description: error
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .into(),
    }
}

impl JmapClient {
    /// Infallible compatibility constructor. Servers should use `try_new`.
    pub fn new(token: String) -> Self {
        Self::try_new(token).expect("Failed to build HTTP client")
    }

    /// Use each client within one Tokio runtime.
    pub fn try_new(token: String) -> Result<Self> {
        Ok(Self {
            client: crate::util::http_client()?,
            event_client: Arc::new(std::sync::Mutex::new(None)),
            token,
            session_url: SESSION_URL.to_string(),
            session: None,
            available_capabilities: Vec::new(),
            server: None,
        })
    }

    pub fn try_via_server(server: crate::remote::HttpServer) -> Result<Self> {
        let mut client = Self::try_new(String::new())?;
        client.session_url = server.url("session").to_string();
        client.server = Some(server);
        Ok(client)
    }

    pub fn via_server(server: crate::remote::HttpServer) -> Self {
        Self::try_via_server(server).expect("Failed to build HTTP client")
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.server {
            Some(server) => server.authorize(request),
            None => request.bearer_auth(&self.token),
        }
    }

    async fn check_proxy_response(&self, response: reqwest::Response) -> Result<reqwest::Response> {
        if self.server.is_some() {
            crate::remote::check_response(response).await
        } else {
            Ok(response)
        }
    }

    /// Build a client that is already "authenticated" against `api_url`, so
    /// tests can point it at a mock JMAP server without going through the
    /// real session endpoint. Re-authentication is redirected there too, so
    /// tests can exercise the handshake itself.
    #[cfg(test)]
    pub fn with_test_session(api_url: &str) -> Self {
        let mut client = Self::try_new("test-token".into()).unwrap();
        client.session_url = format!("{api_url}/session");
        client.available_capabilities =
            DESIRED_CAPABILITIES.iter().map(|s| s.to_string()).collect();
        client.session = Some(Session {
            capabilities: DESIRED_CAPABILITIES
                .iter()
                .map(|c| (c.to_string(), json!({})))
                .collect(),
            accounts: HashMap::new(),
            primary_accounts: HashMap::from([(
                "urn:ietf:params:jmap:mail".to_string(),
                "acct1".to_string(),
            )]),
            username: "test@example.com".into(),
            api_url: api_url.to_string(),
            download_url: format!("{api_url}/download/{{blobId}}"),
            upload_url: format!("{api_url}/upload"),
            event_source_url: None,
            state: None,
        });
        client
    }

    #[cfg(test)]
    pub fn with_test_session_endpoint(url: &str) -> Self {
        let mut client = Self::try_new("test-token".into()).unwrap();
        client.session_url = url.into();
        client
    }

    #[instrument(skip(self))]
    pub async fn authenticate(&mut self) -> Result<&Session> {
        debug!("Fetching JMAP session");
        let resp = self
            .authorize(self.client.get(&self.session_url))
            .send()
            .await?;
        let resp = self.check_proxy_response(resp).await?;

        check_jmap_status(resp.status(), "Authentication failed")?;

        crate::util::check_response_status(&resp)?;
        let bytes =
            crate::util::read_bounded_response(resp, crate::util::MAX_ATTACHMENT_BYTES).await?;
        let mut session: Session = serde_json::from_slice(&bytes)?;
        if let Some(server) = &self.server {
            // Never follow endpoints supplied by a remote server with HTTP login credentials.
            session.api_url = server.url("jmap").to_string();
            session.download_url = format!("{}?blob_id={{blobId}}", server.url("download"));
            session.upload_url = server.url("upload").to_string();
            session.event_source_url = session
                .event_source_url
                .as_ref()
                .map(|_| format!("{}?ping={{ping}}", server.url("events")));
        }
        debug!(username = %session.username, "Session established");
        self.available_capabilities = DESIRED_CAPABILITIES
            .iter()
            .filter(|cap| session.capabilities.contains_key(**cap))
            .map(|s| s.to_string())
            .collect();
        self.session = Some(session);
        Ok(self.session.as_ref().unwrap())
    }

    pub fn session(&self) -> Result<&Session> {
        self.session.as_ref().ok_or(Error::NotAuthenticated)
    }

    fn account_id(&self) -> Result<&str> {
        self.session()?
            .primary_account_id()
            .ok_or_else(|| Error::Config("No primary account".into()))
    }

    fn require_capability(&self, capability: &str, action: &str) -> Result<()> {
        let session = self.session()?;

        if !session.capabilities.contains_key(capability) {
            return Err(Error::Config(format!(
                "{action} requires the '{capability}' capability. \
                Your API token may be read-only. Generate a new token with appropriate permissions \
                at Fastmail Settings > Privacy & Security > Integrations > API tokens."
            )));
        }
        Ok(())
    }

    #[instrument(skip(self, method_calls))]
    pub(crate) async fn request(&self, method_calls: Vec<Value>) -> Result<Vec<Value>> {
        let session = self.session()?;
        let req = JmapRequest {
            using: self.available_capabilities.clone(),
            method_calls,
        };

        debug!(url = %session.api_url, "Making JMAP request");
        let resp = self
            .authorize(self.client.post(&session.api_url))
            .json(&req)
            .send()
            .await?;
        let resp = self.check_proxy_response(resp).await?;

        check_jmap_status(resp.status(), "Token expired or invalid")?;

        crate::util::check_response_status(&resp)?;
        let body =
            crate::util::read_bounded_response(resp, crate::util::MAX_ATTACHMENT_BYTES).await?;
        let jmap_resp: JmapResponse = serde_json::from_slice(&body)?;
        Ok(jmap_resp.method_responses)
    }

    fn parse_response<T: for<'de> Deserialize<'de>>(
        response: &Value,
        expected_method: &str,
    ) -> Result<T> {
        let arr = response.as_array().ok_or_else(|| Error::Jmap {
            method: expected_method.into(),
            error_type: "parse".into(),
            description: "Response is not an array".into(),
        })?;

        let method_name = arr.first().and_then(|v: &Value| v.as_str()).unwrap_or("");

        if method_name == "error" {
            let error_obj = arr.get(1).unwrap_or(&Value::Null);
            return Err(method_error(expected_method, error_obj, "No description"));
        }

        let data = arr.get(1).ok_or_else(|| Error::Jmap {
            method: expected_method.into(),
            error_type: "parse".into(),
            description: "Missing response data".into(),
        })?;

        serde_json::from_value(data.clone()).map_err(|e| Error::Jmap {
            method: expected_method.into(),
            error_type: "parse".into(),
            description: e.to_string(),
        })
    }

    /// Fetch the mailbox list from the server, always. Takes `&self` so callers
    /// holding a shared client can use it concurrently.
    ///
    /// This is the primitive the GraphQL mailbox loader wraps: the loader has a
    /// request-scoped cache of its own, and a cache *here* would outlive the
    /// request — clients are pooled per token for the life of the process, so a
    /// folder created after start-up would never appear.
    #[instrument(skip(self))]
    pub async fn fetch_mailboxes(&self) -> Result<Vec<Mailbox>> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Mailbox/get",
                {
                    "accountId": account_id,
                    "properties": [
                        "id", "name", "parentId", "role",
                        "totalEmails", "unreadEmails",
                        "totalThreads", "unreadThreads", "sortOrder",
                        "isSubscribed", "myRights"
                    ]
                },
                "m0"
            ])])
            .await?;

        let resp: GetResponse<Mailbox> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Mailbox/get")?;

        Ok(resp.list)
    }

    /// Fetch current mailbox names and roles, including on pooled server clients.
    #[instrument(skip(self))]
    pub async fn list_mailboxes(&mut self) -> Result<Vec<Mailbox>> {
        self.fetch_mailboxes().await
    }

    pub async fn find_mailbox(&mut self, name: &str) -> Result<Mailbox> {
        let mailboxes = self.list_mailboxes().await?;
        let name_lower = name.to_lowercase();

        if let Some(m) = mailboxes
            .iter()
            .find(|m| m.name.to_lowercase() == name_lower)
        {
            return Ok(m.clone());
        }

        if let Some(m) = mailboxes.iter().find(|m| {
            m.role
                .as_deref()
                .is_some_and(|role| role.to_lowercase() == name_lower)
        }) {
            return Ok(m.clone());
        }

        Err(Error::MailboxNotFound(name.into()))
    }

    /// One window of an `Email/query` result.
    #[instrument(skip(self, query))]
    pub async fn query_emails(&self, query: EmailQuery) -> Result<EmailPage> {
        let account_id = self.account_id()?;

        let mut args = json!({
            "accountId": account_id,
            "filter": query.filter,
            "sort": query.sort,
            "limit": query.limit,
            "calculateTotal": query.calculate_total,
            "collapseThreads": query.collapse_threads
        });
        match query.start {
            QueryStart::Position(p) => args["position"] = json!(p),
            QueryStart::Anchor { ref id, offset } => {
                args["anchor"] = json!(id);
                args["anchorOffset"] = json!(offset);
            }
        }

        let mut method_calls = vec![json!(["Email/query", args, "q0"])];
        let chained_get =
            query.fetch_summaries && query.limit as usize <= self.max_objects_in_get();

        // Chain the summary fetch off the query with a JMAP back-reference, so a
        // page costs one HTTP round trip rather than two. Skipped entirely when
        // the caller only wants counts.
        if chained_get {
            method_calls.push(json!([
                "Email/get",
                {
                    "accountId": account_id,
                    "#ids": {
                        "resultOf": "q0",
                        "name": "Email/query",
                        "path": "/ids"
                    },
                    "properties": EMAIL_SUMMARY_PROPERTIES
                },
                "g0"
            ]));
        }

        let responses = self.request(method_calls).await?;

        let query_resp: QueryResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/query")?;

        let mut emails = Vec::new();
        if query.fetch_summaries {
            let records = if chained_get {
                Self::parse_response::<GetResponse<Email>>(
                    responses.get(1).unwrap_or(&Value::Null),
                    "Email/get",
                )?
                .list
            } else {
                self.get_email_summaries(&query_resp.ids).await?
            };
            // `Email/get` makes no ordering guarantee, so restore the sort order
            // the query asked for rather than trusting the response order.
            let mut by_id: HashMap<String, Email> =
                records.into_iter().map(|e| (e.id.clone(), e)).collect();
            emails = query_resp
                .ids
                .iter()
                .filter_map(|id| by_id.remove(id.as_str()))
                .collect();
        }

        Ok(EmailPage {
            emails,
            ids: query_resp.ids,
            position: query_resp.position,
            total: query_resp.total,
            query_state: query_resp.query_state,
        })
    }

    #[instrument(skip(self))]
    pub async fn list_emails(&self, mailbox_id: &str, limit: u32) -> Result<Vec<Email>> {
        let page = self
            .query_emails(EmailQuery {
                filter: json!({ "inMailbox": mailbox_id }),
                ..EmailQuery::first(limit)
            })
            .await?;
        Ok(page.emails)
    }

    /// Fetch full content — bodies, attachment metadata, threading headers — for
    /// many emails in `Email/get` batches within the advertised object limit.
    ///
    /// IDs that don't exist are simply absent from the result; the caller decides
    /// whether that is an error. This is the batch primitive behind the GraphQL
    /// email DataLoader.
    #[instrument(skip(self))]
    pub async fn get_emails(&self, ids: &[String]) -> Result<Vec<Email>> {
        self.get_email_records(ids, EMAIL_FULL_PROPERTIES, true)
            .await
    }

    /// Summary records for known IDs — the cheap counterpart to [`Self::get_emails`].
    ///
    /// `Email/query` already returns summaries for the page it matched; this is
    /// for the callers that arrive holding IDs from somewhere else, such as
    /// `Email/changes`.
    #[instrument(skip(self))]
    pub async fn get_email_summaries(&self, ids: &[String]) -> Result<Vec<Email>> {
        self.get_email_records(ids, EMAIL_SUMMARY_PROPERTIES, false)
            .await
    }

    fn max_objects_in_get(&self) -> usize {
        self.session
            .as_ref()
            .and_then(|session| session.capabilities.get("urn:ietf:params:jmap:core"))
            .and_then(|core| core.get("maxObjectsInGet"))
            .and_then(Value::as_u64)
            .and_then(|limit| usize::try_from(limit).ok())
            .filter(|limit| *limit > 0)
            .unwrap_or(100)
    }

    async fn get_email_records(
        &self,
        ids: &[String],
        properties: &[&str],
        fetch_bodies: bool,
    ) -> Result<Vec<Email>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let account_id = self.account_id()?;

        let mut emails = Vec::new();
        for ids in ids.chunks(self.max_objects_in_get()) {
            let responses = self
                .request(vec![json!([
                    "Email/get",
                    {
                        "accountId": account_id,
                        "ids": ids,
                        "properties": properties,
                        "fetchTextBodyValues": fetch_bodies,
                        "fetchHTMLBodyValues": fetch_bodies
                    },
                    "g0"
                ])])
                .await?;

            let resp: GetResponse<Email> =
                Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/get")?;

            emails.extend(resp.list);
        }
        Ok(emails)
    }

    /// The account's current `Email` state string: the cursor
    /// [`Self::email_changes`] reads forward from.
    ///
    /// Fetched with an empty `ids` list, so the server returns the state and no
    /// mail.
    #[instrument(skip(self))]
    pub async fn email_state(&self) -> Result<String> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Email/get",
                { "accountId": account_id, "ids": [] },
                "s0"
            ])])
            .await?;

        #[derive(Deserialize)]
        struct StateOnly {
            state: String,
        }

        let resp: StateOnly =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/get")?;
        Ok(resp.state)
    }

    /// IDs of emails created since `since_state`, and the state they leave the
    /// caller at.
    ///
    /// Follows `hasMoreChanges` to the end, so the returned state is always
    /// current: a partial read would silently drop everything past the first
    /// page on the next call. Only creations are reported — a watcher wants
    /// arrivals, and updates would replay every flag change as news.
    ///
    /// Fails with a `cannotCalculateChanges` JMAP error when the server has
    /// discarded history back that far; the caller resyncs via
    /// [`Self::email_state`].
    #[instrument(skip(self))]
    pub async fn email_changes(&self, since_state: &str) -> Result<EmailChanges> {
        let account_id = self.account_id()?;
        let mut state = since_state.to_string();
        let mut created = Vec::new();

        loop {
            let responses = self
                .request(vec![json!([
                    "Email/changes",
                    {
                        "accountId": account_id,
                        "sinceState": state,
                        "maxChanges": CHANGES_PAGE
                    },
                    "c0"
                ])])
                .await?;

            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct ChangesResponse {
                new_state: String,
                #[serde(default)]
                has_more_changes: bool,
                #[serde(default)]
                created: Vec<String>,
            }

            let resp: ChangesResponse =
                Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/changes")?;

            created.extend(resp.created);
            state = resp.new_state;

            if !resp.has_more_changes {
                return Ok(EmailChanges {
                    new_state: state,
                    created,
                });
            }
        }
    }

    /// Open the JMAP push channel and return the live response to read frames
    /// from.
    ///
    /// `last_event_id` asks the server to replay from where a dropped
    /// connection left off. Missing it is not a correctness problem — the
    /// caller holds its own `Email` state and reconciles through
    /// [`Self::email_changes`] — but it saves a round trip.
    #[instrument(skip(self))]
    pub async fn open_event_stream(
        &self,
        ping: u32,
        last_event_id: Option<&str>,
    ) -> Result<reqwest::Response> {
        let template = self
            .session()?
            .event_source_url
            .as_deref()
            .ok_or_else(|| {
                Error::Config(
                    "Server advertises no eventSourceUrl for push. Use --poll to fall back to \
                     periodic checks."
                        .into(),
                )
            })?
            .to_string();

        let url = template
            .replace("{types}", "Email")
            .replace("{closeafter}", "no")
            .replace("{ping}", &ping.to_string());

        // The shared client caps every request at 30s; a push channel is meant
        // to stay open for days. A read timeout of a few ping intervals stands
        // in for it, so silence reads as a dead connection rather than an idle
        // one — the difference between reconnecting and hanging forever.
        let client = {
            let mut cached = self
                .event_client
                .lock()
                .map_err(|_| Error::Config("Event HTTP client lock poisoned".into()))?;
            if cached
                .as_ref()
                .is_none_or(|(interval, _)| *interval != ping)
            {
                *cached = Some((
                    ping,
                    Client::builder()
                        .read_timeout(Duration::from_secs(u64::from(ping) * 3))
                        .redirect(reqwest::redirect::Policy::none())
                        .build()?,
                ));
            }
            cached.as_ref().unwrap().1.clone()
        };

        let mut req = self
            .authorize(client.get(&url))
            .header("Accept", "text/event-stream");
        if let Some(id) = last_event_id {
            req = req.header("Last-Event-ID", id);
        }

        debug!(url = %url, "Opening JMAP event source");
        let resp = req.send().await?;
        let resp = self.check_proxy_response(resp).await?;

        check_jmap_status(resp.status(), "Token expired or invalid")?;

        crate::util::check_response_status(&resp)?;
        if !resp
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
            })
        {
            return Err(Error::Server("Expected an event stream response".into()));
        }
        Ok(resp)
    }

    #[instrument(skip(self))]
    pub async fn get_email(&self, email_id: &str) -> Result<Email> {
        let ids = [email_id.to_string()];
        self.get_emails(&ids)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::EmailNotFound(email_id.into()))
    }

    /// Resolve thread IDs in batches within the advertised object limit.
    ///
    /// Threads that don't exist are absent from the map. This is the batch
    /// primitive behind the GraphQL thread DataLoader.
    #[instrument(skip(self))]
    pub async fn thread_email_ids(
        &self,
        thread_ids: &[String],
    ) -> Result<HashMap<String, Vec<String>>> {
        if thread_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let account_id = self.account_id()?;

        let mut threads = HashMap::new();
        for thread_ids in thread_ids.chunks(self.max_objects_in_get()) {
            let responses = self
                .request(vec![json!([
                    "Thread/get",
                    {
                        "accountId": account_id,
                        "ids": thread_ids
                    },
                    "t0"
                ])])
                .await?;

            #[derive(Deserialize)]
            struct Thread {
                id: String,
                #[serde(rename = "emailIds")]
                email_ids: Vec<String>,
            }

            let resp: GetResponse<Thread> =
                Self::parse_response(responses.first().unwrap_or(&Value::Null), "Thread/get")?;

            threads.extend(resp.list.into_iter().map(|t| (t.id, t.email_ids)));
        }
        Ok(threads)
    }

    /// Get all emails in a thread, with full content.
    #[instrument(skip(self))]
    pub async fn get_thread(&self, email_id: &str) -> Result<Vec<Email>> {
        let email = self.get_email(email_id).await?;
        let thread_id = email
            .thread_id
            .ok_or_else(|| Error::Config("Email has no thread ID".into()))?;

        let ids = self
            .thread_email_ids(std::slice::from_ref(&thread_id))
            .await?
            .remove(&thread_id)
            .ok_or_else(|| Error::Config("Thread not found".into()))?;

        self.get_emails(&ids).await
    }

    /// Search emails with full JMAP filter support
    #[instrument(skip(self, filter))]
    pub async fn search_emails_filtered(
        &self,
        filter: &SearchFilter,
        mailbox_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Email>> {
        let page = self
            .query_emails(EmailQuery {
                filter: search_filter_to_jmap(filter, mailbox_id),
                ..EmailQuery::first(limit)
            })
            .await?;
        Ok(page.emails)
    }

    #[instrument(skip(self))]
    pub async fn list_identities(&self) -> Result<Vec<Identity>> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Identity/get",
                { "accountId": account_id },
                "i0"
            ])])
            .await?;

        let resp: GetResponse<Identity> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Identity/get")?;

        Ok(resp.list)
    }

    async fn resolve_identity(&self, from: Option<&str>) -> Result<Identity> {
        let identities = self.list_identities().await?;
        pick_identity(identities, from)
    }

    /// Return the email address that would be used as the sender for a reply/
    /// send/forward — i.e. the resolved identity's email. Returns `None` if
    /// identity resolution fails, so callers (notably the MCP preview path)
    /// can still produce a useful preview without erroring out.
    pub async fn resolve_my_email(&self, from: Option<&str>) -> Option<String> {
        self.resolve_sender(from)
            .await
            .ok()
            .map(|sender| sender.email)
    }

    pub(crate) async fn resolve_sender(&self, from: Option<&str>) -> Result<Identity> {
        self.resolve_identity(from).await
    }

    async fn prepare_compose(
        &mut self,
        from: Option<&str>,
        draft: bool,
        resolved_identity: Option<Result<Identity>>,
    ) -> Result<ComposeContext> {
        if !draft {
            self.require_capability("urn:ietf:params:jmap:submission", "Email sending")?;
        }
        let account_id = self.account_id()?.to_string();
        let mailbox = self.find_mailbox("drafts").await?;
        let sent_mailbox = if draft {
            None
        } else {
            Some(self.find_mailbox("sent").await?)
        };
        // Confirmation supplies its reviewed identity, including a failed lookup.
        let identity = match resolved_identity {
            Some(identity) => identity,
            None => self.resolve_identity(from).await,
        };
        let identity = match identity {
            Ok(id) => Some(id),
            Err(_) if draft && from.is_none() => None,
            Err(e) => return Err(e),
        };
        Ok(ComposeContext {
            account_id,
            mailbox,
            sent_mailbox,
            identity,
            draft,
        })
    }

    fn parse_email_create_response(responses: &[Value]) -> Result<String> {
        let email_resp: EmailSetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;

        if let Some(ref not_created) = email_resp.not_created
            && let Some(err) = not_created.get("email")
        {
            return Err(method_error("Email/set", err, "Failed to create email"));
        }

        // Check EmailSubmission/set response if present (index 1)
        if let Some(submission_resp) = responses.get(1) {
            let sub: EmailSetResponse =
                Self::parse_response(submission_resp, "EmailSubmission/set")?;
            if let Some(ref not_created) = sub.not_created
                && let Some(err) = not_created.get("submission")
            {
                return Err(method_error(
                    "EmailSubmission/set",
                    err,
                    "Email created but submission failed",
                ));
            }
        }

        email_resp
            .created
            .as_ref()
            .and_then(|created| created.get("email"))
            .and_then(|email| email.get("id"))
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| Error::Jmap {
                method: "Email/set".into(),
                error_type: "unknown".into(),
                description: "No email ID returned".into(),
            })
    }

    /// Shared helper: build email_create map with common fields and submit it.
    /// Handles plain text, HTML, and attachment body structures.
    async fn create_and_submit_email(
        &self,
        ctx: &ComposeContext,
        draft: EmailDraft<'_>,
    ) -> Result<String> {
        let mut email_create: HashMap<String, Value> = HashMap::new();
        ctx.apply_to_email(&mut email_create);
        email_create.insert("to".into(), addresses_json(draft.to));
        if !draft.cc.is_empty() {
            email_create.insert("cc".into(), addresses_json(draft.cc));
        }
        if !draft.bcc.is_empty() {
            email_create.insert("bcc".into(), addresses_json(draft.bcc));
        }
        email_create.insert("subject".into(), json!(draft.subject));

        // Upload attachments and collect blob IDs
        let mut uploaded_attachments: Vec<UploadedAttachment> = Vec::new();
        for att in draft.attachments {
            let blob_id = self.upload_blob(att.data, &att.content_type).await?;
            uploaded_attachments.push(UploadedAttachment {
                blob_id,
                filename: att.filename,
                content_type: att.content_type,
            });
        }

        apply_body_structure(
            &mut email_create,
            draft.body,
            draft.html_body,
            &uploaded_attachments,
        );

        if let Some(ref headers) = draft.threading {
            if !headers.in_reply_to.is_empty() {
                email_create.insert("inReplyTo".into(), json!(headers.in_reply_to));
            }
            if !headers.references.is_empty() {
                email_create.insert("references".into(), json!(headers.references));
            }
        }

        let responses = self.request(ctx.build_method_calls(email_create)).await?;
        let email_id = Self::parse_email_create_response(&responses)?;

        debug!(email_id = %email_id, draft = ctx.draft, "Email created successfully");
        Ok(email_id)
    }

    #[instrument(skip(self, to, subject, body, params))]
    pub async fn send_email(
        &mut self,
        to: Vec<EmailAddress>,
        subject: &str,
        body: &str,
        in_reply_to: Option<&str>,
        params: ComposeParams<'_>,
    ) -> Result<String> {
        self.send_email_with_identity(to, subject, body, in_reply_to, params, None)
            .await
    }

    pub(crate) async fn send_email_with_identity(
        &mut self,
        to: Vec<EmailAddress>,
        subject: &str,
        body: &str,
        in_reply_to: Option<&str>,
        params: ComposeParams<'_>,
        resolved_identity: Option<Result<Identity>>,
    ) -> Result<String> {
        let ctx = self
            .prepare_compose(params.from, params.draft, resolved_identity)
            .await?;
        self.create_and_submit_email(
            &ctx,
            EmailDraft {
                to: &to,
                cc: &params.cc,
                bcc: &params.bcc,
                subject,
                body,
                html_body: params.html_body.as_deref(),
                attachments: params.attachments,
                threading: in_reply_to.map(|id| ThreadingHeaders {
                    in_reply_to: vec![id.to_string()],
                    references: vec![],
                }),
            },
        )
        .await
    }

    #[instrument(skip(self))]
    pub async fn move_email(&self, email_id: &str, mailbox_id: &str) -> Result<()> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Email/set",
                {
                    "accountId": account_id,
                    "update": {
                        (email_id): {
                            "mailboxIds": { (mailbox_id): true }
                        }
                    }
                },
                "m0"
            ])])
            .await?;

        let resp: SetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;

        if let Some(ref not_updated) = resp.not_updated
            && let Some(err) = not_updated.get(email_id)
        {
            return Err(method_error("Email/set", err, "Failed to move email"));
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn mark_spam(&mut self, email_id: &str) -> Result<()> {
        let junk = self.find_mailbox("junk").await?;
        self.move_email(email_id, &junk.id).await
    }

    /// Download a blob (attachment) by ID
    #[instrument(skip(self))]
    pub async fn download_blob(&self, blob_id: &str) -> Result<Vec<u8>> {
        let account_id = self.account_id()?;
        let session = self.session()?;

        // downloadUrl template: https://api.fastmail.com/jmap/download/{accountId}/{blobId}/{name}?accept={type}
        //
        // Single-pass substitution — chained .replace() calls could recursively
        // replace a value that happened to contain another template marker.
        let url = apply_url_template(
            &session.download_url,
            &[
                ("accountId", account_id),
                ("blobId", blob_id),
                ("name", "attachment"),
                ("type", "application/octet-stream"),
            ],
        );

        debug!(url = %url, "Downloading blob");
        let resp = self.authorize(self.client.get(&url)).send().await?;
        let resp = self.check_proxy_response(resp).await?;

        check_jmap_status(resp.status(), "Token expired or invalid")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(Error::Config(format!("Blob not found: {}", blob_id)));
        }

        crate::util::check_response_status(&resp)?;
        crate::util::read_bounded_response(resp, crate::util::MAX_ATTACHMENT_BYTES).await
    }

    /// Upload a blob (for attachments) and return the blobId
    #[instrument(skip(self, data))]
    pub async fn upload_blob(&self, data: Vec<u8>, content_type: &str) -> Result<String> {
        if data.len() > crate::util::MAX_ATTACHMENT_BYTES {
            return Err(Error::Config(
                "Attachment exceeds the 64 MiB upload limit".into(),
            ));
        }
        let account_id = self.account_id()?;
        let session = self.session()?;

        let url = apply_url_template(&session.upload_url, &[("accountId", account_id)]);

        debug!(url = %url, content_type = %content_type, size = data.len(), "Uploading blob");
        let resp = self
            .authorize(self.client.post(&url))
            .header("Content-Type", content_type)
            .body(data)
            .send()
            .await?;
        let resp = self.check_proxy_response(resp).await?;

        check_jmap_status(resp.status(), "Token expired or invalid")?;
        if !resp.status().is_success() {
            return Err(Error::Server(format!("Upload failed ({})", resp.status())));
        }

        let bytes =
            crate::util::read_bounded_response(resp, crate::util::MAX_ATTACHMENT_BYTES).await?;
        let body: Value = serde_json::from_slice(&bytes)?;
        body.get("blobId")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| Error::Server("Upload response missing blobId".into()))
    }

    /// Send a reply to an existing email with proper threading headers.
    ///
    /// The caller is responsible for computing `to` and `params.cc` — usually
    /// by calling [`expand_reply_recipients`] after resolving the sending
    /// identity with [`JmapClient::resolve_my_email`]. Keeping the expansion
    /// on the caller side means the MCP preview path and the send path use
    /// exactly the same recipient lists, so the preview cannot under-report
    /// or diverge from what will actually be sent.
    #[instrument(skip(self, original, to, body, params))]
    pub async fn reply_email(
        &mut self,
        original: &Email,
        body: &str,
        to: Vec<EmailAddress>,
        params: ComposeParams<'_>,
    ) -> Result<String> {
        self.reply_email_with_identity(original, body, to, params, None)
            .await
    }

    pub(crate) async fn reply_email_with_identity(
        &mut self,
        original: &Email,
        body: &str,
        to: Vec<EmailAddress>,
        params: ComposeParams<'_>,
        resolved_identity: Option<Result<Identity>>,
    ) -> Result<String> {
        let ctx = self
            .prepare_compose(params.from, params.draft, resolved_identity)
            .await?;
        let to_addrs = to;
        let cc_addrs = params.cc;

        let subject = prefixed_subject(original.subject.as_deref(), "Re:");

        // Build References header: original references + original message-id
        let references: Vec<String> = {
            let mut refs = original.references.clone().unwrap_or_default();
            if let Some(ref msg_id) = original.message_id {
                for id in msg_id {
                    if !refs.contains(id) {
                        refs.push(id.clone());
                    }
                }
            }
            refs
        };

        self.create_and_submit_email(
            &ctx,
            EmailDraft {
                to: &to_addrs,
                cc: &cc_addrs,
                bcc: &params.bcc,
                subject: &subject,
                body,
                html_body: params.html_body.as_deref(),
                attachments: params.attachments,
                threading: Some(ThreadingHeaders {
                    in_reply_to: original.message_id.clone().unwrap_or_default(),
                    references,
                }),
            },
        )
        .await
    }

    /// Forward an email with proper attribution
    #[instrument(skip(self, original, to, body, params))]
    pub async fn forward_email(
        &mut self,
        original: &Email,
        to: Vec<EmailAddress>,
        body: &str,
        params: ComposeParams<'_>,
    ) -> Result<String> {
        self.forward_email_with_identity(original, to, body, params, None)
            .await
    }

    pub(crate) async fn forward_email_with_identity(
        &mut self,
        original: &Email,
        to: Vec<EmailAddress>,
        body: &str,
        params: ComposeParams<'_>,
        resolved_identity: Option<Result<Identity>>,
    ) -> Result<String> {
        let ctx = self
            .prepare_compose(params.from, params.draft, resolved_identity)
            .await?;

        let subject = prefixed_subject(original.subject.as_deref(), "Fwd:");

        // Build forwarded body with attribution
        let original_body = original.text_content().unwrap_or_default();

        let sender = original
            .from
            .as_ref()
            .and_then(|f| f.first())
            .map(|a| a.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let date = original.received_at.as_deref().unwrap_or("unknown date");

        let full_body = format!(
            "{}\n\n---------- Forwarded message ---------\nFrom: {}\nDate: {}\nSubject: {}\n\n{}",
            body,
            sender,
            date,
            original.subject.as_deref().unwrap_or(""),
            original_body
        );

        self.create_and_submit_email(
            &ctx,
            EmailDraft {
                to: &to,
                cc: &params.cc,
                bcc: &params.bcc,
                subject: &subject,
                body: &full_body,
                html_body: params.html_body.as_deref(),
                attachments: params.attachments,
                threading: None,
            },
        )
        .await
    }

    #[instrument(skip(self))]
    pub async fn mark_read(&self, email_id: &str, read: bool) -> Result<()> {
        self.update_keywords(
            email_id,
            json!({"keywords/$seen": if read { json!(true) } else { Value::Null }}),
        )
        .await
    }

    /// Replace the complete keyword map. Use `mark_read` for atomic seen changes.
    pub async fn set_keywords(
        &self,
        email_id: &str,
        keywords: HashMap<String, bool>,
    ) -> Result<()> {
        self.update_keywords(email_id, json!({"keywords":keywords}))
            .await
    }

    async fn update_keywords(&self, email_id: &str, patch: Value) -> Result<()> {
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "Email/set",
                {
                    "accountId": account_id,
                    "update": {
                        (email_id): patch
                    }
                },
                "k0"
            ])])
            .await?;

        let resp: SetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "Email/set")?;

        if let Some(ref not_updated) = resp.not_updated
            && let Some(err) = not_updated.get(email_id)
        {
            return Err(method_error("Email/set", err, "Failed to update keywords"));
        }

        Ok(())
    }

    /// List all masked email addresses
    #[instrument(skip(self))]
    pub async fn list_masked_emails(&self) -> Result<Vec<MaskedEmail>> {
        self.require_capability("https://www.fastmail.com/dev/maskedemail", "Masked email")?;
        let account_id = self.account_id()?;

        let responses = self
            .request(vec![json!([
                "MaskedEmail/get",
                {
                    "accountId": account_id,
                    "ids": null
                },
                "me0"
            ])])
            .await?;

        let resp: GetResponse<MaskedEmail> =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "MaskedEmail/get")?;

        Ok(resp.list)
    }

    /// Create a new masked email address
    #[instrument(skip(self))]
    pub async fn create_masked_email(
        &self,
        for_domain: Option<&str>,
        description: Option<&str>,
        email_prefix: Option<&str>,
    ) -> Result<MaskedEmail> {
        self.require_capability("https://www.fastmail.com/dev/maskedemail", "Masked email")?;
        let account_id = self.account_id()?;

        let mut create_obj: HashMap<String, Value> = HashMap::new();
        create_obj.insert("state".into(), json!("enabled"));

        if let Some(domain) = for_domain {
            create_obj.insert("forDomain".into(), json!(domain));
        }
        if let Some(desc) = description {
            create_obj.insert("description".into(), json!(desc));
        }
        if let Some(prefix) = email_prefix {
            create_obj.insert("emailPrefix".into(), json!(prefix));
        }

        let responses = self
            .request(vec![json!([
                "MaskedEmail/set",
                {
                    "accountId": account_id,
                    "create": { "new": create_obj }
                },
                "me0"
            ])])
            .await?;

        let resp: MaskedEmailCreateResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "MaskedEmail/set")?;

        if let Some(ref not_created) = resp.not_created
            && let Some(err) = not_created.get("new")
        {
            return Err(method_error(
                "MaskedEmail/set",
                err,
                "Failed to create masked email",
            ));
        }

        resp.created
            .and_then(|mut c| c.remove("new"))
            .ok_or_else(|| Error::Jmap {
                method: "MaskedEmail/set".into(),
                error_type: "unknown".into(),
                description: "No masked email returned".into(),
            })
    }

    /// Update a masked email's state (enable/disable/delete)
    #[instrument(skip(self))]
    pub async fn update_masked_email(
        &self,
        id: &str,
        state: Option<&str>,
        for_domain: Option<&str>,
        description: Option<&str>,
    ) -> Result<()> {
        self.require_capability("https://www.fastmail.com/dev/maskedemail", "Masked email")?;
        let account_id = self.account_id()?;

        let mut update_obj: HashMap<String, Value> = HashMap::new();
        if let Some(s) = state {
            update_obj.insert("state".into(), json!(s));
        }
        if let Some(domain) = for_domain {
            update_obj.insert("forDomain".into(), json!(domain));
        }
        if let Some(desc) = description {
            update_obj.insert("description".into(), json!(desc));
        }

        let responses = self
            .request(vec![json!([
                "MaskedEmail/set",
                {
                    "accountId": account_id,
                    "update": { (id): update_obj }
                },
                "me0"
            ])])
            .await?;

        let resp: SetResponse =
            Self::parse_response(responses.first().unwrap_or(&Value::Null), "MaskedEmail/set")?;

        if let Some(ref not_updated) = resp.not_updated
            && let Some(err) = not_updated.get(id)
        {
            return Err(method_error(
                "MaskedEmail/set",
                err,
                "Failed to update masked email",
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests;
