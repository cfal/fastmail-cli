use super::{CHANGES_PAGE, JmapClient};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use tracing::instrument;

/// An opaque, account-specific Email state. Obtaining it does not commit it.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EmailCheckpoint {
    pub account_id: String,
    pub state: String,
}

/// All pages of an account-wide Email change window, without fetching mail.
///
/// IDs retain server/page order, not chronological order. Duplicates and IDs
/// present in multiple arrays are retained, including creations later destroyed.
/// This is discovery data, not an audit log or a net change set. Persist the IDs
/// and `new_state` atomically before processing them; no state is saved here.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EmailChangeBatch {
    pub account_id: String,
    pub old_state: String,
    pub new_state: String,
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
}

pub(super) struct ChangeLimits {
    pub pages: usize,
    pub ids: usize,
    pub bytes: usize,
}

const LIMITS: ChangeLimits = ChangeLimits {
    pages: 1_000,
    ids: 100_000,
    bytes: 16 * 1024 * 1024,
};

fn invalid_response(method: &str, description: &str) -> Error {
    Error::Jmap {
        method: method.into(),
        error_type: "parse".into(),
        description: description.into(),
    }
}

fn limit_exceeded() -> Error {
    Error::Jmap {
        method: "Email/changes".into(),
        error_type: "limitExceeded".into(),
        description: "Complete change batch exceeds safety limits; no checkpoint returned".into(),
    }
}

fn checkpoint_response<T: for<'de> Deserialize<'de>>(
    responses: &[Value],
    method: &str,
    call_id: &str,
) -> Result<T> {
    let response = responses.first().unwrap_or(&Value::Null);
    if responses.len() != 1
        || response.as_array().is_none_or(|array| array.len() != 3)
        || (response[0] != method && response[0] != "error")
        || response[2] != call_id
    {
        return Err(invalid_response(
            method,
            "Unexpected method response or call ID",
        ));
    }
    JmapClient::parse_response(response, method)
}

impl JmapClient {
    /// Read the current account and Email state, without retrieving any emails.
    ///
    /// Capture this *before* an initial backfill, then use
    /// [`Self::email_change_batch`] to discover changes that occurred during it.
    #[instrument(skip(self))]
    pub async fn email_checkpoint(&self) -> Result<EmailCheckpoint> {
        let account_id = self.account_id()?;
        let responses = self
            .request(vec![json!([
                "Email/get", {"accountId": account_id, "ids": []}, "s0"
            ])])
            .await?;

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct StateResponse {
            account_id: String,
            state: String,
            list: Vec<Value>,
            not_found: Vec<String>,
        }

        let response: StateResponse = checkpoint_response(&responses, "Email/get", "s0")?;
        if response.account_id != account_id
            || !response.list.is_empty()
            || !response.not_found.is_empty()
        {
            return Err(invalid_response(
                "Email/get",
                "Unexpected account or nonempty ID result",
            ));
        }
        Ok(EmailCheckpoint {
            account_id: response.account_id,
            state: response.state,
        })
    }

    /// Immediately collect all `Email/changes` pages from an opaque saved state.
    ///
    /// No partial result or checkpoint is returned on failure, including
    /// `cannotCalculateChanges`. That error requires caller-managed backfill,
    /// not an automatic reset. No bodies are fetched and no cursor is persisted.
    /// Aggregation fails above 1,000 pages, 100,000 IDs (including duplicates),
    /// or 16 MiB of ID and state strings across pages.
    #[instrument(skip(self, since_state))]
    pub async fn email_change_batch(&self, since_state: &str) -> Result<EmailChangeBatch> {
        self.email_change_batch_with_limits(since_state, LIMITS)
            .await
    }

    pub(super) async fn email_change_batch_with_limits(
        &self,
        since_state: &str,
        limits: ChangeLimits,
    ) -> Result<EmailChangeBatch> {
        let account_id = self.account_id()?;
        let mut bytes = since_state.len();
        if bytes > limits.bytes {
            return Err(limit_exceeded());
        }
        let mut batch = EmailChangeBatch {
            account_id: account_id.into(),
            old_state: since_state.into(),
            new_state: since_state.into(),
            created: Vec::new(),
            updated: Vec::new(),
            destroyed: Vec::new(),
        };
        let mut seen_states = HashSet::from([since_state.to_string()]);

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ChangePage {
            account_id: String,
            old_state: String,
            new_state: String,
            has_more_changes: bool,
            created: Vec<String>,
            updated: Vec<String>,
            destroyed: Vec<String>,
        }

        for _ in 0..limits.pages {
            let responses = self
                .request(vec![json!([
                    "Email/changes",
                    {"accountId": account_id, "sinceState": batch.new_state, "maxChanges": CHANGES_PAGE},
                    "c0"
                ])])
                .await?;
            let page: ChangePage = checkpoint_response(&responses, "Email/changes", "c0")?;
            if page.account_id != account_id || page.old_state != batch.new_state {
                return Err(invalid_response(
                    "Email/changes",
                    "Account or oldState mismatch",
                ));
            }
            let page_ids = page.created.len() + page.updated.len() + page.destroyed.len();
            if page_ids > CHANGES_PAGE as usize {
                return Err(invalid_response(
                    "Email/changes",
                    "Response exceeds maxChanges",
                ));
            }
            if page.new_state == batch.new_state {
                if page.has_more_changes || page_ids != 0 {
                    return Err(invalid_response(
                        "Email/changes",
                        "Changes did not advance state",
                    ));
                }
            } else if seen_states.contains(&page.new_state) {
                return Err(invalid_response("Email/changes", "Cyclic change states"));
            }

            let ids = page
                .created
                .iter()
                .chain(&page.updated)
                .chain(&page.destroyed);
            if ids.clone().any(String::is_empty) {
                return Err(invalid_response("Email/changes", "Empty email ID"));
            }
            bytes += page.new_state.len() + ids.map(String::len).sum::<usize>();
            if bytes > limits.bytes
                || batch.created.len() + batch.updated.len() + batch.destroyed.len() + page_ids
                    > limits.ids
            {
                return Err(limit_exceeded());
            }
            seen_states.insert(page.new_state.clone());
            batch.new_state = page.new_state;
            batch.created.extend(page.created);
            batch.updated.extend(page.updated);
            batch.destroyed.extend(page.destroyed);
            if !page.has_more_changes {
                return Ok(batch);
            }
        }
        Err(limit_exceeded())
    }
}
