use crate::error::Error;
use crate::jmap::authenticated_client;
use crate::models::Output;
use serde::Serialize;

/// A failed change window, never a checkpoint that can be committed directly.
#[derive(Debug, Serialize, thiserror::Error)]
#[serde(tag = "type", rename = "resync-required", rename_all = "camelCase")]
#[error("Email change history is unavailable; backfill before using currentState")]
pub struct EmailResyncRequired {
    pub account_id: String,
    pub stale_state: String,
    /// Null when even the replacement-state lookup failed. Obtain a fresh state
    /// before starting the backfill in that case.
    pub current_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_state_error: Option<String>,
}

pub async fn email_state() -> anyhow::Result<()> {
    let client = authenticated_client().await?;
    Output::success(client.email_checkpoint().await?).print();
    Ok(())
}

/// Print one complete ID-only batch. The caller owns all checkpoint persistence.
pub async fn changes(since_state: &str) -> anyhow::Result<()> {
    let client = authenticated_client().await?;
    match client.email_change_batch(since_state).await {
        Ok(batch) => {
            Output::success(batch).print();
            Ok(())
        }
        Err(Error::Jmap { error_type, .. }) if error_type == "cannotCalculateChanges" => {
            let account_id = client
                .session()?
                .primary_account_id()
                .ok_or_else(|| anyhow::anyhow!("No primary account"))?
                .to_string();
            let (current_state, current_state_error) = match client.email_checkpoint().await {
                Ok(checkpoint) => (Some(checkpoint.state), None),
                Err(error) => (None, Some(error.to_string())),
            };
            Err(EmailResyncRequired {
                account_id,
                stale_state: since_state.into(),
                current_state,
                current_state_error,
            }
            .into())
        }
        Err(error) => Err(error.into()),
    }
}
