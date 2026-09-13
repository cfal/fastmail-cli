//! MCP (Model Context Protocol) server for Fastmail
//!
//! Exposes Fastmail functionality via two GraphQL tools:
//! - `schema_sdl` — returns the GraphQL SDL, whole or sliced to named types
//! - `graphql` — executes a GraphQL query/mutation

use std::collections::HashMap;
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use tokio::sync::Mutex;

use crate::config::Config;
use crate::jmap::JmapClient;

type ToolResult = std::result::Result<CallToolResult, McpError>;

mod cli_http;
pub mod graphql;
mod http_security;
mod graphiql_assets {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/web/dist/assets.rs"));
}
mod sdl;

use graphql::{CardDavCreds, FastmailSchema, SharedClient};

/// Cache of authenticated JMAP clients keyed by Fastmail token, so we don't
/// re-run the JMAP session handshake on every tool call. Shared across sessions.
type ClientCache = Arc<Mutex<HashMap<String, SharedClient>>>;

async fn cached_client(cache: &ClientCache, token: &str) -> anyhow::Result<SharedClient> {
    let mut cache = cache.lock().await;
    if let Some(client) = cache.get(token) {
        return Ok(client.clone());
    }
    let client = Arc::new(Mutex::new(JmapClient::try_new(token.to_string())?));
    cache.insert(token.into(), client.clone());
    Ok(client)
}

/// Ordinary operations authenticate on first use. Health probes own their
/// handshake so cold failures can be reported as structured session status.
async fn client_for(cache: &ClientCache, token: &str) -> anyhow::Result<SharedClient> {
    let shared = cached_client(cache, token).await?;
    {
        let mut client = shared.lock().await;
        if client.session().is_err() {
            client.authenticate().await?;
        }
    }
    Ok(shared)
}

/// Server-owned Fastmail credentials, resolved independently of HTTP login.
fn local_token() -> Option<String> {
    Config::load().ok().and_then(|c| c.get_token().ok())
}

// ============ Request Types ============

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GraphqlRequest {
    /// The GraphQL query or mutation string
    pub query: String,
    /// Optional JSON-encoded variables for the query
    #[serde(default)]
    pub variables: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct SchemaRequest {
    /// Type names to return, e.g. `["QueryRoot", "EmailFilter"]`. Omit for the
    /// whole schema, which is large. Each named type comes back whole, with its
    /// documentation, but the types *it* references do not — name those too.
    #[serde(default)]
    pub types: Option<Vec<String>>,
}

// ============ Server Implementation ============

#[derive(Clone)]
pub struct FastmailMcp {
    schema: Arc<FastmailSchema>,
    clients: ClientCache,
    /// Fastmail token configured on the machine running the server.
    default_token: Option<String>,
    /// CardDAV credentials configured on the machine running the server.
    default_carddav: CardDavCreds,
    #[allow(dead_code)] // referenced by #[tool_handler] macro expansion
    tool_router: ToolRouter<Self>,
}

impl FastmailMcp {
    fn build(default_token: Option<String>) -> Self {
        Self {
            schema: Arc::new(graphql::build_schema()),
            clients: Arc::new(Mutex::new(HashMap::new())),
            default_token,
            default_carddav: CardDavCreds::from_local_config(),
            tool_router: Self::tool_router(),
        }
    }

    /// Construct for stdio use: requires a token in config/env, used for every
    /// request. Errors if no token is configured.
    pub fn new() -> anyhow::Result<Self> {
        let config = Config::load()?;
        let token = config.get_token()?;
        Ok(Self::build(Some(token)))
    }

    /// Construct for HTTP use with server-owned credentials.
    pub fn http() -> Self {
        Self::build(local_token())
    }

    fn text_result(text: impl Into<String>) -> ToolResult {
        Ok(CallToolResult::success(vec![Content::text(text.into())]))
    }

    fn error_result(msg: impl Into<String>) -> ToolResult {
        Ok(CallToolResult::error(vec![Content::text(msg.into())]))
    }
}

#[tool_router]
impl FastmailMcp {
    #[tool(
        title = "Fastmail schema",
        description = "The full GraphQL SDL for the Fastmail API, with documentation on every type, argument and per-field cost.\n\
\n\
You do not need this for everyday mail — the `graphql` tool's own description already carries the queries, filters, fields and send flow for that. Reach for this when you want something it lists as not covered (attachment payloads, masked email, contacts, identities, moveEmail, markAsRead, markAsSpam, the remaining filter and sort options), or the exact cost of a field.\n\
\n\
Pass `types` to fetch only what you need: the whole schema is ~27KB, and `types: [\"MutationRoot\"]` or `[\"Attachment\", \"MaskedEmail\"]` is usually a few hundred bytes. Named types come back whole and documented, but the types they reference do not — name those too. An unrecognised name is reported back with the list of names that do exist. Omit `types` for the lot."
    )]
    async fn schema_sdl(&self, Parameters(req): Parameters<SchemaRequest>) -> ToolResult {
        let sdl = self.schema.sdl();
        match req.types {
            // An explicit empty list means "no types", which is never what a
            // caller wants; read it as the whole schema.
            Some(types) if !types.is_empty() => Self::text_result(sdl::slice(&sdl, &types)),
            _ => Self::text_result(sdl),
        }
    }

    #[tool(
        title = "Fastmail",
        description = "Execute a GraphQL query or mutation against the Fastmail API. Variables go as a JSON string.

Everyday mail is covered below — call `schema_sdl` only for what isn't.

QUERIES
  session: Session!                     # { status carddavConfigured username }
  mailboxes(first: Int): MailboxConnection!
  mailbox(name: String!): Mailbox       # name or role — \"INBOX\", \"sent\", \"drafts\"
  emails(filter: EmailFilter, sort: [EmailSort!], collapseThreads: Boolean,
         first: Int, after: String): EmailConnection!
  email(id: String!): Email
  thread(emailId: String!): Thread!     # { total emails { nodes { ... } } }

EmailFilter — scalars on one object AND together, and/or/not nest arbitrarily:
  text from to cc subject body: String  # text searches all of them
  inMailbox: String                     # name or role
  inMailboxOtherThan: [String!]
  unread flagged hasAttachment: Boolean
  before after: String                  # YYYY-MM-DD or ISO 8601
  hasKeyword notKeyword: String         # e.g. \"$answered\", \"$draft\"
  and: [EmailFilter!]  or: [EmailFilter!]  not: EmailFilter

Email fields: id subject preview textBody htmlBody receivedAt sentAt size
  from to cc bcc { name email }  isUnread isFlagged isDraft hasAttachment
  mailboxes { name role }  thread { total }  attachments { nodes { name size } }

Connections: `nodes` for items, first/last/after/before to page (default 25,
max 100), cursors are IDs, `pageInfo { hasNextPage endCursor }`. `totalCount`
is only computed when selected.

MUTATIONS — sendEmail, replyToEmail and forwardEmail all take
`action: PREVIEW | CONFIRM | DRAFT`. PREVIEW sends nothing and returns a
confirmationToken; CONFIRM repeats the same to/subject/body plus that token, and
is rejected if they differ. Recipients are comma-separated strings, not lists.
  sendEmail(action: SendAction!, to: String!, subject: String!, body: String!,
            cc: String, bcc: String, from: String, htmlBody: String,
            confirmationToken: String): ComposeResult!
  ComposeResult { success emailId preview confirmationToken error }

EXAMPLES
```
{ emails(filter: {unread: true, inMailbox: \"INBOX\", not: {hasKeyword: \"$answered\"}, or: [{from: \"a@b.com\"}, {to: \"a@b.com\"}]}, first: 10) { totalCount nodes { id subject from { email } } } }
mutation { sendEmail(action: PREVIEW, to: \"a@b.com\", subject: \"Hi\", body: \"...\") { preview confirmationToken } }
mutation { sendEmail(action: CONFIRM, to: \"a@b.com\", subject: \"Hi\", body: \"...\", confirmationToken: \"<from preview>\") { success emailId } }
```

SUBSCRIPTIONS are not available through this tool — it is request/response, and
a subscription never returns. The schema defines one (`emails`, streaming mail
as it arrives) for callers on the HTTP surface, which serves it over SSE at
`/graphql/stream`; `fastmail watch` is the same thing as a CLI.

NOT LISTED ABOVE — ask `schema_sdl` for these rather than guessing: attachment
payloads (base64/image/text), masked email, contacts and contact CRUD (CardDAV,
so check `session { carddavConfigured }` first), identities, moveEmail,
markAsRead, markAsSpam, and the remaining filter and sort options."
    )]
    async fn graphql(&self, Parameters(req): Parameters<GraphqlRequest>) -> ToolResult {
        if let Err(message) = graphql::check_query(&req.query) {
            return Self::error_result(message);
        }
        let Some(token) = self.default_token.as_deref() else {
            return Self::error_result(
                "No Fastmail token available. Configure one on the server via `fastmail auth`.",
            );
        };
        let resolved = if is_local_query(&req.query, true, None) {
            cached_client(&self.clients, token).await
        } else {
            client_for(&self.clients, token).await
        };
        let client = match resolved {
            Ok(client) => client,
            Err(e) => return Self::error_result(format!("Fastmail authentication failed: {e}")),
        };

        let mut request = graphql::request(&req.query, client, self.default_carddav.clone());

        if let Some(ref vars) = req.variables {
            match serde_json::from_str::<serde_json::Value>(vars) {
                Ok(serde_json::Value::Object(map)) => {
                    request = request.variables(async_graphql::Variables::from_json(
                        serde_json::Value::Object(map),
                    ));
                }
                Ok(_) => {
                    return Self::error_result("Variables must be a JSON object");
                }
                Err(e) => {
                    return Self::error_result(format!("Invalid variables JSON: {e}"));
                }
            }
        }

        let response = self.schema.execute(request).await;
        let json = serde_json::to_string_pretty(&response)
            .unwrap_or_else(|e| format!("{{\"error\": \"Serialization failed: {e}\"}}"));

        Self::text_result(json)
    }
}

#[tool_handler]
impl ServerHandler for FastmailMcp {
    fn get_info(&self) -> ServerInfo {
        let server_info = Implementation::new("fastmail", env!("CARGO_PKG_VERSION"))
            .with_title("Fastmail MCP Server")
            .with_website_url("https://github.com/cfal/fastmail-cli");

        // No protocol version is declared: rmcp defaults to the newest it
        // implements, and negotiation settles on the lower of ours and the
        // client's. The declared version is therefore a *ceiling* — pinning an
        // old one (this was stuck on 2024-11-05) caps every client to it, which
        // is how `title` on a tool went unused.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(server_info)
            .with_instructions(
                "Fastmail, as a GraphQL API.\n\n\
                The `graphql` tool's description carries the queries, filters, \
                fields and send flow for everyday mail, so most sessions need no \
                schema fetch at all. `schema_sdl` has the rest, and takes a \
                `types` list so you can read one corner of it rather than all \
                ~27KB. Variables go as a JSON string.\n\n\
                Contacts are the one thing to check before planning around: they \
                go over CardDAV, which the API token does not cover, and \
                `{ session { status carddavConfigured } }` answers it without \
                failing a query first.\n\n\
                ## Querying well\n\
                - The graph is fully nested and everything below a list is \
                  batched, so ask for what you need in ONE query rather than \
                  looping. Selecting `textBody` across 25 emails costs one extra \
                  API call, not 25.\n\
                - Collections are connections: `nodes { ... }` for the items, \
                  `first`/`after` to page, cursors are IDs. Default page is 25, \
                  so check `pageInfo.hasNextPage` before assuming that is all of \
                  them.\n\
                - Ask only for fields you will use; their descriptions say what \
                  each costs. Attachment metadata is free, `text` parses the \
                  whole document. `emails { totalCount }` on its own answers \
                  \"how many?\" while fetching no mail at all.\n\n\
                ## Safety\n\
                Never send without showing the user a PREVIEW first, and never \
                CONFIRM without their explicit approval. Marking spam trains the \
                filter, so preview that too and pass its one-shot \
                confirmationToken to CONFIRM.",
            )
    }
}

/// Run the MCP server with stdio transport. The Fastmail token comes from
/// config/env and is used for every request.
pub async fn run_server() -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};

    let service = FastmailMcp::new()?;
    let server = service
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {}", e))?;

    server
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP server error: {}", e))?;

    Ok(())
}

/// Body of a GraphQL-over-HTTP request, as GraphiQL sends it.
#[derive(serde::Deserialize)]
struct HttpGraphqlRequest {
    query: String,
    #[serde(default)]
    variables: Option<serde_json::Value>,
    #[serde(default, rename = "operationName")]
    operation_name: Option<String>,
}

/// The GraphiQL IDE uses locally embedded, lockfile-built assets.
#[derive(askama::Template)]
#[template(path = "graphiql.html")]
struct GraphiqlPage<'a> {
    title: &'a str,
}

async fn graphiql_asset(
    axum::extract::Path(name): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if let Some((_, mime, bytes)) = graphiql_assets::ASSETS
        .iter()
        .find(|(path, _, _)| *path == name)
    {
        (
            [
                (http::header::CONTENT_TYPE, *mime),
                (http::header::CONTENT_ENCODING, "gzip"),
            ],
            *bytes,
        )
            .into_response()
    } else {
        http::StatusCode::NOT_FOUND.into_response()
    }
}

/// Whether every top-level selection is an introspection field, and so can be
/// answered from the schema alone.
///
/// GraphiQL sends exactly this on load to build its docs, autocomplete and
/// explorer. Requiring a Fastmail token for it would mean bad credentials leave
/// you with an IDE that cannot describe the API you are trying to explore.
/// Anything it cannot parse, or that mixes in real fields, is not introspection.
fn is_introspection_only(query: &str) -> bool {
    is_local_query(query, false, None)
}

fn is_local_query(query: &str, allow_session: bool, operation_name: Option<&str>) -> bool {
    use async_graphql::parser::types::{OperationType, Selection};

    if graphql::check_query(query).is_err() {
        return false;
    }
    let Ok(doc) = async_graphql::parser::parse_query(query) else {
        return false;
    };
    let mut selections = Vec::new();
    for (name, op) in doc.operations.iter() {
        if operation_name.is_some_and(|wanted| name.map(|n| n.as_str()) != Some(wanted)) {
            continue;
        }
        if op.node.ty != OperationType::Query {
            return false;
        }
        selections.extend(op.node.selection_set.node.items.iter());
    }
    if selections.is_empty() {
        return false;
    }
    let mut visited = std::collections::HashSet::new();
    while let Some(selection) = selections.pop() {
        match &selection.node {
            Selection::Field(field) => {
                let name = field.node.name.node.as_str();
                if !name.starts_with("__") && !(allow_session && name == "session") {
                    return false;
                }
            }
            Selection::InlineFragment(fragment) => {
                selections.extend(fragment.node.selection_set.node.items.iter())
            }
            Selection::FragmentSpread(spread) => {
                let name = &spread.node.fragment_name.node;
                if visited.insert(name) {
                    let Some(fragment) = doc.fragments.get(name) else {
                        return false;
                    };
                    selections.extend(fragment.node.selection_set.node.items.iter());
                }
            }
        }
    }
    true
}

/// Resolve credentials and build the GraphQL request an HTTP body describes.
///
/// Shared by the query and subscription endpoints so they cannot drift on which
/// token wins or what counts as introspection. The error case is a message for
/// the caller, not a status code — GraphQL reports its own failures in the
/// response body.
async fn build_http_request(
    mcp: &FastmailMcp,
    req: HttpGraphqlRequest,
) -> std::result::Result<async_graphql::Request, String> {
    graphql::check_query(&req.query)?;
    // Introspection is answered from the schema, so it neither needs a token nor
    // touches the network — the IDE stays usable while credentials are wrong.
    let mut request = if is_introspection_only(&req.query) {
        async_graphql::Request::new(&req.query)
    } else {
        let Some(token) = mcp.default_token.as_deref() else {
            return Err(
                "No Fastmail token available. Configure one on the server via `fastmail auth`."
                    .into(),
            );
        };
        // Authenticated on first use rather than at startup, so a missing or
        // expired token surfaces in the response pane instead of stopping the
        // server booting.
        let client = if is_local_query(&req.query, true, req.operation_name.as_deref()) {
            cached_client(&mcp.clients, token)
                .await
                .map_err(|e| e.to_string())?
        } else {
            client_for(&mcp.clients, token)
                .await
                .map_err(|e| format!("Fastmail authentication failed: {e}"))?
        };
        graphql::request(&req.query, client, mcp.default_carddav.clone())
    };
    if let Some(vars) = req.variables {
        request = request.variables(async_graphql::Variables::from_json(vars));
    }
    if let Some(name) = req.operation_name {
        request = request.operation_name(name);
    }
    Ok(request)
}

/// Plain GraphQL-over-HTTP, for browsers and anything else that speaks it
/// directly rather than through MCP's JSON-RPC envelope. Shares the server's
/// schema, client cache and token resolution with the `graphql` tool.
async fn graphql_endpoint(
    axum::extract::State(mcp): axum::extract::State<FastmailMcp>,
    axum::Json(req): axum::Json<HttpGraphqlRequest>,
) -> axum::Json<async_graphql::Response> {
    match build_http_request(&mcp, req).await {
        Ok(request) => axum::Json(mcp.schema.execute(request).await),
        Err(msg) => axum::Json(async_graphql::Response::from_errors(vec![
            async_graphql::ServerError::new(msg, None),
        ])),
    }
}

/// GraphQL subscriptions over Server-Sent Events, one event per response.
///
/// Each POST starts a new watcher. This downstream stream is not resumable;
/// subscribers must query for arrivals during any disconnected interval.
async fn graphql_stream_endpoint(
    axum::extract::State(mcp): axum::extract::State<FastmailMcp>,
    axum::Json(req): axum::Json<HttpGraphqlRequest>,
) -> axum::response::Response {
    use async_graphql::futures_util::stream::StreamExt;
    use axum::response::{IntoResponse, Sse, sse};

    let request = match build_http_request(&mcp, req).await {
        Ok(request) => request,
        Err(msg) => {
            return axum::Json(async_graphql::Response::from_errors(vec![
                async_graphql::ServerError::new(msg, None),
            ]))
            .into_response();
        }
    };

    let events = mcp
        .schema
        .execute_stream(request)
        .map(|response| {
            let data = serde_json::to_string(&response)
                .unwrap_or_else(|e| format!(r#"{{"errors":[{{"message":"{e}"}}]}}"#));
            Ok::<_, std::convert::Infallible>(sse::Event::default().event("next").data(data))
        })
        .chain(async_graphql::futures_util::stream::once(async {
            Ok(sse::Event::default().event("complete").data(""))
        }));

    // Proxies drop connections that go quiet, and a mail subscription is quiet
    // most of the time.
    Sse::new(events)
        .keep_alive(sse::KeepAlive::default())
        .into_response()
}

/// Which surfaces [`run_http_server`] mounts alongside MCP at `/mcp`.
#[derive(Clone, Copy)]
pub struct HttpSurfaces {
    /// Plain GraphQL-over-HTTP at `/graphql`.
    pub graphql: bool,
    /// The GraphiQL IDE at `/`. Implies `graphql` — it is the IDE's endpoint.
    pub graphiql: bool,
    /// Open the IDE in the default browser once listening.
    pub browser: bool,
}

/// Run the HTTP server on `addr`: MCP streamable-HTTP at `/mcp`, plus whichever
/// of [`HttpSurfaces`] is enabled.
///
/// Uses server-owned Fastmail credentials without requiring an HTTP login.
pub async fn run_http_server(addr: &str, surfaces: HttpSurfaces) -> anyhow::Result<()> {
    run_http_server_with_auth(addr, surfaces, None, Vec::new()).await
}

pub async fn run_http_server_with_auth(
    addr: &str,
    surfaces: HttpSurfaces,
    auth_file: Option<&std::path::Path>,
    allowed_hosts: Vec<String>,
) -> anyhow::Result<()> {
    let auth = auth_file.map(http_security::BasicAuth::load).transpose()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let policy = http_security::HttpSecurity::new(listener.local_addr()?, auth, allowed_hosts)?;
    let router = http_router(FastmailMcp::http(), surfaces, policy)?;
    tracing::info!("HTTP server listening on http://{addr}");

    if surfaces.browser {
        let url = format!("http://{addr}/");
        if let Err(e) = open::that_detached(&url) {
            tracing::warn!("Could not open a browser at {url}: {e}");
        }
    }

    axum::serve(listener, router).await?;
    Ok(())
}

fn http_router(
    mcp: FastmailMcp,
    surfaces: HttpSurfaces,
    policy: http_security::HttpSecurity,
) -> anyhow::Result<axum::Router> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    // One outer policy protects MCP, GraphQL and the IDE consistently.
    let config = StreamableHttpServerConfig::default().disable_allowed_hosts();
    let service = StreamableHttpService::new(
        {
            let template = mcp.clone();
            move || Ok(template.clone())
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let mut router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(http_security::limit_mcp_body))
        .merge(cli_http::router());

    if surfaces.graphql || surfaces.graphiql {
        router = router
            .route("/graphql", axum::routing::post(graphql_endpoint))
            .route(
                "/graphql/stream",
                axum::routing::post(graphql_stream_endpoint),
            );
    }

    if surfaces.graphiql {
        // Rendered once: nothing in the page varies per request, and a template
        // error should stop the server rather than 500 on every hit.
        let ide = askama::Template::render(&GraphiqlPage {
            title: "Fastmail GraphQL",
        })?;
        router = router.route("/assets/{name}", axum::routing::get(graphiql_asset));
        router = router.route(
            "/",
            axum::routing::get(move || {
                let ide = ide.clone();
                async move { axum::response::Html(ide) }
            }),
        );
    }

    Ok(router
        .with_state(mcp)
        .layer(axum::middleware::from_fn_with_state(
            policy,
            http_security::guard,
        )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn graphql_transports_check_syntax_before_authentication() {
        let mcp = FastmailMcp::build(None);
        let query = "[".repeat(33);
        let error = build_http_request(
            &mcp,
            HttpGraphqlRequest {
                query: query.clone(),
                variables: None,
                operation_name: None,
            },
        )
        .await
        .err()
        .unwrap();
        assert!(error.contains("syntax nesting"));
        let result = mcp
            .graphql(Parameters(GraphqlRequest {
                query,
                variables: None,
            }))
            .await
            .unwrap();
        assert!(text_of(result).contains("syntax nesting"));
    }

    #[tokio::test]
    async fn http_auth_and_browser_policy_cover_every_surface() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b"[users]\nalice = 'password'\nbob = 'second'")
            .unwrap();
        let auth = http_security::BasicAuth::load(file.path()).unwrap();
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let policy = http_security::HttpSecurity::new(
            format!("127.0.0.1:{port}").parse().unwrap(),
            Some(auth),
            vec![],
        )
        .unwrap();
        let router = http_router(
            FastmailMcp::build(None),
            HttpSurfaces {
                graphql: true,
                graphiql: true,
                browser: false,
            },
            policy,
        )
        .unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{port}");
        for path in [
            "/",
            "/mcp",
            "/graphql",
            "/graphql/stream",
            "/assets/main.js",
            "/cli/v1/session",
            "/cli/v1/jmap",
            "/cli/v1/upload",
            "/cli/v1/download",
            "/cli/v1/contacts",
            "/cli/v1/events",
        ] {
            let response = client.post(format!("{base}{path}")).send().await.unwrap();
            assert_eq!(response.status(), http::StatusCode::UNAUTHORIZED, "{path}");
            assert!(
                response
                    .headers()
                    .contains_key(http::header::WWW_AUTHENTICATE)
            );
        }
        for (user, password, status) in [
            ("alice", "password", 200),
            ("bob", "second", 200),
            ("alice", "second", 401),
        ] {
            let response = client
                .post(format!("{base}/graphql"))
                .basic_auth(user, Some(password))
                .json(&serde_json::json!({"query": "{ __typename }"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
        }
        for (name, value) in [
            ("host", "rebind.example"),
            ("origin", "https://evil.example"),
        ] {
            let response = client
                .post(format!("{base}/graphql"))
                .basic_auth("alice", Some("password"))
                .header(name, value)
                .json(&serde_json::json!({"query": "{ __typename }"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), http::StatusCode::FORBIDDEN);
        }
        let response = client
            .post(format!("{base}/graphql"))
            .basic_auth("alice", Some("password"))
            .header("x-fastmail-token", "must-not-be-used")
            .json(&serde_json::json!({"query": "{ session { status } }"}))
            .send()
            .await
            .unwrap();
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("No Fastmail token available")
        );
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn cli_http_roundtrip_keeps_fastmail_credentials_on_server() {
        use serde_json::{Value, json};
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let upstream = MockServer::start().await;
        let origin = upstream.uri();
        Mock::given(method("GET"))
            .and(path("/jmap/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "capabilities":{"urn:ietf:params:jmap:core":{},"urn:ietf:params:jmap:mail":{}},
                "accounts":{}, "primaryAccounts":{"urn:ietf:params:jmap:mail":"acct1"},
                "username":"server@example.com", "apiUrl":format!("{origin}/jmap"),
                "downloadUrl":format!("{origin}/download?blob={{blobId}}"),
                "uploadUrl":format!("{origin}/upload"),
                "eventSourceUrl":format!("{origin}/events?ping={{ping}}")
            })))
            .mount(&upstream)
            .await;
        Mock::given(method("POST"))
            .and(path("/jmap"))
            .respond_with(|req: &wiremock::Request| {
                let value: Value = serde_json::from_slice(&req.body).unwrap();
                ResponseTemplate::new(200)
                    .set_body_json(json!({"methodResponses":value["methodCalls"]}))
            })
            .mount(&upstream)
            .await;
        Mock::given(method("GET"))
            .and(path("/download"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"attachment"))
            .mount(&upstream)
            .await;
        Mock::given(method("POST"))
            .and(path("/upload"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"blobId":"uploaded"})))
            .mount(&upstream)
            .await;
        Mock::given(method("GET"))
            .and(path("/events"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("id: event2\ndata: {}\n\n", "text/event-stream"),
            )
            .mount(&upstream)
            .await;
        let mcp = FastmailMcp::build(Some("server-only".into()));
        let local = JmapClient::with_test_session(&format!("{origin}/jmap"));
        mcp.clients
            .lock()
            .await
            .insert("server-only".into(), Arc::new(Mutex::new(local)));
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b"[users]\ncli = 'login'").unwrap();
        let auth = http_security::BasicAuth::load(file.path()).unwrap();
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let policy = http_security::HttpSecurity::new(addr, Some(auth), vec![]).unwrap();
        let router = http_router(
            mcp,
            HttpSurfaces {
                graphql: false,
                graphiql: false,
                browser: false,
            },
            policy,
        )
        .unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let remote = crate::remote::HttpServer::new(
            &format!("http://127.0.0.1:{}", addr.port()),
            Some("cli"),
            Some("login"),
        )
        .unwrap();
        crate::remote::HttpServer::scope(Some(remote), async {
            let client = crate::jmap::authenticated_client().await.unwrap();
            assert_eq!(client.session().unwrap().username, "server@example.com");
            let calls = vec![json!(["Mailbox/get", {"accountId":"acct1"}, "0"])];
            assert_eq!(client.request(calls.clone()).await.unwrap(), calls);
            assert_eq!(
                client.download_blob("blob/a?b&c").await.unwrap(),
                b"attachment"
            );
            assert_eq!(
                client
                    .upload_blob(b"upload".to_vec(), "text/plain")
                    .await
                    .unwrap(),
                "uploaded"
            );
            let response = client.open_event_stream(30, Some("event1")).await.unwrap();
            assert!(response.text().await.unwrap().contains("id: event2"));
        })
        .await;
        for request in upstream.received_requests().await.unwrap() {
            assert_eq!(request.headers["authorization"], "Bearer test-token");
            assert!(!request.headers.contains_key("x-fastmail-token"));
            if request.url.path() == "/download" {
                assert_eq!(
                    request.url.query_pairs().collect::<Vec<_>>(),
                    vec![("blob".into(), "blob/a?b&c".into())]
                );
            }
            if request.url.path() == "/events" {
                assert_eq!(request.headers["last-event-id"], "event1");
            }
            if request.url.path() == "/upload" {
                assert_eq!(request.body, b"upload");
            }
        }
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn http_auth_is_optional_even_on_a_non_loopback_listener() {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let policy = http_security::HttpSecurity::new(addr, None, vec![]).unwrap();
        let router = http_router(
            FastmailMcp::build(None),
            HttpSurfaces {
                graphql: true,
                graphiql: false,
                browser: false,
            },
            policy,
        )
        .unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{}/graphql", addr.port()))
            .json(&serde_json::json!({"query": "{ __typename }"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        assert!(response.text().await.unwrap().contains("QueryRoot"));
        task.abort();
        let _ = task.await;
    }

    /// The text a tool call came back with.
    fn text_of(result: CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect()
    }

    #[tokio::test]
    async fn schema_sdl_without_arguments_still_returns_everything() {
        // `types` was added to a tool that took no arguments at all, and rmcp
        // reads absent arguments as `{}` — so an existing client that sends
        // none must keep getting the whole schema.
        let empty: SchemaRequest = serde_json::from_str("{}").unwrap();
        assert!(empty.types.is_none());

        let sdl = text_of(
            FastmailMcp::http()
                .schema_sdl(Parameters(empty))
                .await
                .unwrap(),
        );
        assert!(sdl.contains("type QueryRoot {"));
        assert!(sdl.contains("type Session {"));
        assert!(sdl.contains("input EmailFilter {"));
    }

    #[tokio::test]
    async fn schema_sdl_with_types_returns_only_those() {
        let mcp = FastmailMcp::http();
        let sliced = text_of(
            mcp.schema_sdl(Parameters(SchemaRequest {
                types: Some(vec!["Session".into()]),
            }))
            .await
            .unwrap(),
        );

        assert!(sliced.contains("type Session {"));
        assert!(!sliced.contains("input EmailFilter {"));

        let full = text_of(
            mcp.schema_sdl(Parameters(SchemaRequest::default()))
                .await
                .unwrap(),
        );
        assert!(
            sliced.len() * 10 < full.len(),
            "{} of {} is not a saving worth the argument",
            sliced.len(),
            full.len()
        );
    }

    #[tokio::test]
    async fn an_empty_types_list_is_read_as_the_whole_schema() {
        // Never a useful request, and returning nothing would look like a bug
        // in the schema rather than in the call.
        let sdl = text_of(
            FastmailMcp::http()
                .schema_sdl(Parameters(SchemaRequest {
                    types: Some(Vec::new()),
                }))
                .await
                .unwrap(),
        );
        assert!(sdl.contains("type QueryRoot {"));
    }

    #[test]
    fn introspection_needs_no_token() {
        // What GraphiQL sends on load, plus the shapes around it.
        assert!(is_introspection_only("{ __schema { queryType { name } } }"));
        assert!(is_introspection_only(
            "query IntrospectionQuery { __schema { types { name } } }"
        ));
        assert!(is_introspection_only(
            "{ __type(name: \"Email\") { name } }"
        ));
        assert!(is_introspection_only("{ __typename }"));
        assert!(is_introspection_only(
            "{ ...F } fragment F on QueryRoot { __typename }"
        ));
    }

    #[test]
    fn real_fields_still_need_a_token() {
        assert!(!is_introspection_only("{ mailboxes { name } }"));
        // Mixed with introspection, and nested below it, still count as real.
        assert!(!is_introspection_only("{ __typename mailboxes { name } }"));
        assert!(!is_introspection_only(
            "mutation { sendEmail(action: PREVIEW) { preview } }"
        ));
        // Classify fragment contents as well as direct selections.
        assert!(!is_introspection_only(
            "{ ...F } fragment F on QueryRoot { mailboxes { name } }"
        ));
        assert!(!is_introspection_only("{ this is not graphql"));
    }

    #[tokio::test]
    async fn cold_health_failures_are_structured_through_http_and_mcp() {
        use serde_json::json;
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        for (status, expected) in [
            (401, "INVALID_CREDENTIALS"),
            (503, "UNREACHABLE"),
            (0, "UNREACHABLE"),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(status.max(400)))
                .mount(&server)
                .await;
            let endpoint = if status == 0 {
                "http://127.0.0.1:1".to_owned()
            } else {
                server.uri()
            };
            for http in [true, false] {
                let mcp = FastmailMcp::build(Some("test-token".into()));
                // Inject only the endpoint, never an authenticated session.
                let client = JmapClient::with_test_session_endpoint(&endpoint);
                assert!(client.session().is_err());
                mcp.clients
                    .lock()
                    .await
                    .insert("test-token".into(), Arc::new(Mutex::new(client)));
                let query =
                    "query Health { ...Probe } fragment Probe on QueryRoot { session { status } }";
                let response = if http {
                    let result = graphql_endpoint(
                        axum::extract::State(mcp),
                        axum::Json(HttpGraphqlRequest {
                            query: query.into(),
                            variables: None,
                            operation_name: Some("Health".into()),
                        }),
                    )
                    .await;
                    serde_json::to_value(result.0).unwrap()
                } else {
                    serde_json::from_str(&text_of(
                        mcp.graphql(Parameters(GraphqlRequest {
                            query: query.into(),
                            variables: None,
                        }))
                        .await
                        .unwrap(),
                    ))
                    .unwrap()
                };
                assert_eq!(
                    response["data"]["session"]["status"],
                    json!(expected),
                    "{response}"
                );
            }
        }
    }

    #[tokio::test]
    async fn graphql_stream_emits_next_and_complete_protocol_events() {
        let mcp = FastmailMcp::build(Some("test-token".into()));
        let client = JmapClient::with_test_session("http://127.0.0.1:1");
        mcp.clients
            .lock()
            .await
            .insert("test-token".into(), Arc::new(Mutex::new(client)));
        let response = graphql_stream_endpoint(
            axum::extract::State(mcp),
            axum::Json(HttpGraphqlRequest {
                query: "subscription { emails(pollSeconds: 0) { id } }".into(),
                variables: None,
                operation_name: None,
            }),
        )
        .await;
        assert_eq!(
            response.headers()[http::header::CONTENT_TYPE],
            "text/event-stream"
        );
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("event: next\n"), "{text}");
        assert!(text.contains("event: complete\n"), "{text}");
        assert!(text.contains("at least one second"));
    }

    #[test]
    fn only_selected_health_queries_defer_authentication() {
        let document = "query Health { session { status } } query Mail { emails { nodes { id } } }";
        assert!(is_local_query(document, true, Some("Health")));
        assert!(!is_local_query(document, true, Some("Mail")));
        assert!(!is_local_query(
            "{ session { status } mailboxes { name } }",
            true,
            None
        ));
    }

    #[tokio::test]
    async fn graphql_falls_back_to_local_config_when_no_headers() {
        let mcp = FastmailMcp::build(Some("fake-token".to_string()));

        // Pre-seed the client cache with a client whose session already
        // points at an address that refuses connections instantly, so the
        // resolver's JMAP call fails fast instead of reaching the real
        // Fastmail API with a fake token.
        let client = JmapClient::with_test_session("http://127.0.0.1:1");
        mcp.clients
            .lock()
            .await
            .insert("fake-token".to_string(), Arc::new(Mutex::new(client)));

        let req = HttpGraphqlRequest {
            query: "{ session { status } }".to_string(),
            variables: None,
            operation_name: None,
        };

        let response = graphql_endpoint(axum::extract::State(mcp), axum::Json(req)).await;

        let body = serde_json::to_string(&response.0).unwrap();
        assert!(
            !body.contains("No Fastmail token available"),
            "expected the default token to be used, got: {body}"
        );
    }
}
