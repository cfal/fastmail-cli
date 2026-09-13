use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Query, State},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http::{HeaderMap, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{FastmailMcp, client_for};
use crate::{
    carddav::{ContactEmail, ContactFields, ContactPhone},
    error::Error,
    jmap::JmapClient,
};

pub(super) fn router() -> Router<FastmailMcp> {
    Router::new()
        .route("/cli/v1/session", get(session))
        .route("/cli/v1/jmap", post(jmap))
        .route("/cli/v1/download", get(download))
        .route(
            "/cli/v1/upload",
            post(upload).layer(DefaultBodyLimit::max(crate::util::MAX_ATTACHMENT_BYTES)),
        )
        .route("/cli/v1/events", get(events))
        .route("/cli/v1/contacts", post(contacts))
}

struct ApiError(Error);

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = if matches!(self.0, Error::RateLimited) {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::BAD_GATEWAY
        };
        (status, Json(json!({"error":self.0.to_string()}))).into_response()
    }
}

async fn shared_client(mcp: &FastmailMcp) -> Result<super::SharedClient, ApiError> {
    let token = mcp
        .default_token
        .as_deref()
        .ok_or_else(|| Error::Config("Configure Fastmail credentials on the server".into()))?;
    let shared = client_for(&mcp.clients, token)
        .await
        .map_err(|_| Error::Server("Server could not authenticate with Fastmail".into()))?;
    Ok(shared)
}

async fn client(mcp: &FastmailMcp) -> Result<JmapClient, ApiError> {
    Ok(shared_client(mcp).await?.lock().await.clone())
}

async fn session(State(mcp): State<FastmailMcp>) -> Result<Json<crate::models::Session>, ApiError> {
    let shared = shared_client(&mcp).await?;
    let mut client = shared.lock().await;
    Ok(Json(client.authenticate().await?.clone()))
}

#[derive(Deserialize)]
struct JmapRequest {
    #[serde(rename = "methodCalls")]
    method_calls: Vec<Value>,
}

async fn jmap(
    State(mcp): State<FastmailMcp>,
    Json(request): Json<JmapRequest>,
) -> Result<Json<Value>, ApiError> {
    if request.method_calls.len() > 64 {
        return Err(Error::Config("At most 64 JMAP method calls per request".into()).into());
    }
    let responses = client(&mcp).await?.request(request.method_calls).await?;
    Ok(Json(json!({"methodResponses": responses})))
}

#[derive(Deserialize)]
struct Download {
    blob_id: String,
}

async fn download(
    State(mcp): State<FastmailMcp>,
    Query(query): Query<Download>,
) -> Result<Response, ApiError> {
    let bytes = client(&mcp).await?.download_blob(&query.blob_id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CONTENT_DISPOSITION, "attachment"),
        ],
        bytes,
    )
        .into_response())
}

async fn upload(
    State(mcp): State<FastmailMcp>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");
    let blob_id = client(&mcp)
        .await?
        .upload_blob(body.to_vec(), content_type)
        .await?;
    Ok(Json(json!({"blobId":blob_id})))
}

#[derive(Deserialize)]
struct Events {
    ping: Option<u32>,
}

async fn events(
    State(mcp): State<FastmailMcp>,
    Query(query): Query<Events>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let last_id = headers.get("last-event-id").and_then(|v| v.to_str().ok());
    let response = client(&mcp)
        .await?
        .open_event_stream(query.ping.unwrap_or(30).clamp(1, 300), last_id)
        .await?;
    let chunks =
        async_graphql::futures_util::stream::try_unfold(response, |mut response| async move {
            response
                .chunk()
                .await
                .map(|chunk| chunk.map(|chunk| (chunk, response)))
        });
    Ok((
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (http::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Body::from_stream(chunks),
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ContactRequest {
    Addressbooks,
    List { href: String },
    Create { fields: ContactInput },
    Update { id: String, fields: ContactInput },
    Delete { id: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContactInput {
    name: Option<String>,
    emails: Option<Vec<ContactEmail>>,
    phones: Option<Vec<ContactPhone>>,
    organization: Option<String>,
    title: Option<String>,
    notes: Option<String>,
}

impl ContactInput {
    fn fields(&self) -> ContactFields<'_> {
        ContactFields {
            name: self.name.as_deref(),
            emails: self.emails.as_deref(),
            phones: self.phones.as_deref(),
            organization: self.organization.as_deref(),
            title: self.title.as_deref(),
            notes: self.notes.as_deref(),
        }
    }
}

async fn contacts(
    State(mcp): State<FastmailMcp>,
    Json(request): Json<ContactRequest>,
) -> Result<Json<Value>, ApiError> {
    let client = mcp
        .default_carddav
        .client()
        .map_err(|_| Error::Config("Configure CardDAV credentials on the server".into()))?;
    let value = match request {
        ContactRequest::Addressbooks => serde_json::to_value(client.list_addressbooks().await?),
        ContactRequest::List { href } => serde_json::to_value(client.list_contacts(&href).await?),
        ContactRequest::Create { fields } => {
            serde_json::to_value(client.create_contact(&fields.fields()).await?)
        }
        ContactRequest::Update { id, fields } => {
            serde_json::to_value(client.update_contact(&id, &fields.fields()).await?)
        }
        ContactRequest::Delete { id } => {
            client.delete_contact(&id).await?;
            Ok(Value::Null)
        }
    }
    .map_err(Error::from)?;
    Ok(Json(value))
}
