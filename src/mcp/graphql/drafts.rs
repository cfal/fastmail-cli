use async_graphql::SimpleObject;
use serde_json::json;

use super::types::GqlComposeResult;
use crate::models::{Email, EmailBodyPart, Identity};

#[derive(SimpleObject)]
#[graphql(name = "DraftSendResult")]
pub struct GqlDraftSendResult {
    pub success: bool,
    pub email_id: Option<String>,
    /// Positive evidence that the server accepted the submission, not delivery confirmation.
    pub submission_id: Option<String>,
    pub preview: Option<String>,
    pub confirmation_token: Option<String>,
    /// Submission succeeded, but filing may need attention. Never resend to fix filing.
    pub filing_warning: Option<String>,
    pub error: Option<String>,
}

impl From<GqlComposeResult> for GqlDraftSendResult {
    fn from(result: GqlComposeResult) -> Self {
        Self {
            success: result.success,
            email_id: result.email_id,
            submission_id: None,
            preview: result.preview,
            confirmation_token: result.confirmation_token,
            filing_warning: None,
            error: result.error,
        }
    }
}

pub(super) fn sender(email: &Email) -> Result<&str, String> {
    let headers = email
        .headers
        .as_deref()
        .ok_or("Draft headers are missing; no approval issued")?;
    // Convenience address fields expose only the last occurrence; envelope
    // generation can otherwise include recipients not visible in the preview.
    for name in ["From", "Sender", "To", "Cc", "Bcc", "Reply-To", "Subject"] {
        let count = headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case(name))
            .count();
        if count > 1 || (name == "From" && count != 1) {
            return Err(format!(
                "Draft has missing or repeated {name} headers; no approval issued"
            ));
        }
    }
    let from = email.from.as_deref().unwrap_or_default();
    if from.len() != 1 {
        return Err("Sending a saved draft requires exactly one From address".into());
    }
    let sender = email.sender.as_deref().unwrap_or_default();
    if !sender.is_empty()
        && (sender.len() != 1 || !sender[0].email.eq_ignore_ascii_case(&from[0].email))
    {
        return Err(
            "Sending drafts with a different or multiple Sender addresses is unsupported".into(),
        );
    }
    Ok(&from[0].email)
}

fn part_metadata(part: &EmailBodyPart) -> String {
    json!({
        "name":part.name, "type":part.content_type, "size":part.size,
        "blobId":part.blob_id, "cid":part.cid, "disposition":part.disposition
    })
    .to_string()
}

pub(super) fn preview(email: &Email, identity: &Identity) -> Result<String, String> {
    let text = email
        .text_body
        .as_deref()
        .ok_or("Draft textBody metadata is missing")?;
    let html = email
        .html_body
        .as_deref()
        .ok_or("Draft htmlBody metadata is missing")?;
    let attachments = email
        .attachments
        .as_deref()
        .ok_or("Draft attachment metadata is missing")?;
    if text.len() + html.len() + attachments.len() > 128 {
        return Err("Draft preview exceeds the 128-part limit".into());
    }
    let mut preview = format!(
        "SAVED DRAFT PREVIEW:\nEmail ID: {}\nFrom: {}\nSender: {}\nReply-To: {}\nTo: {}\nCC: {}\nBCC: {}\nSubject: {}\nSubmission identity: {}\n",
        json!(email.id),
        json!(email.from),
        json!(email.sender),
        json!(email.reply_to),
        json!(email.to),
        json!(email.cc),
        json!(email.bcc),
        json!(email.subject),
        json!({"id": identity.id, "email":identity.email, "name":identity.name})
    );
    for (label, parts) in [
        ("Text-preferred parts", text),
        ("HTML-preferred parts", html),
    ] {
        preview.push_str(&format!("\n--- {label} ---\n"));
        for part in parts {
            let content_type = part
                .content_type
                .as_deref()
                .ok_or("Draft body part type is missing")?;
            preview.push_str(&part_metadata(part));
            preview.push('\n');
            if content_type.to_ascii_lowercase().starts_with("text/") {
                let value = part
                    .part_id
                    .as_ref()
                    .and_then(|id| email.body_values.as_ref()?.get(id))
                    .ok_or("Draft body value is missing; no approval issued")?;
                if value.is_truncated || value.is_encoding_problem {
                    return Err(
                        "Draft body is truncated or has encoding problems; no approval issued"
                            .into(),
                    );
                }
                preview.push_str(&value.value);
                preview.push('\n');
            } else if part.blob_id.is_none() {
                return Err("Draft inline part has no blob ID; no approval issued".into());
            }
        }
    }
    preview.push_str("\n--- Attachments (metadata only) ---\n");
    for part in attachments {
        if part.blob_id.is_none() || part.content_type.is_none() {
            return Err("Draft attachment metadata is incomplete; no approval issued".into());
        }
        preview.push_str(&part_metadata(part));
        preview.push('\n');
    }
    if preview.len() > 1024 * 1024 {
        return Err("Draft preview exceeds the 1 MiB limit; no approval issued".into());
    }
    Ok(preview)
}
