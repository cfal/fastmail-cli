//! GraphQL mutation resolvers

use async_graphql::{Context, Object, Result};

use crate::carddav::{ContactEmail, ContactPhone};
use crate::jmap::prefixed_subject;
use crate::models::EmailAddress;
use crate::util::parse_addresses;

use super::types::*;

pub struct MutationRoot;

#[Object]
#[allow(clippy::too_many_arguments)]
impl MutationRoot {
    /// Compose and send a new email. ALWAYS use action=PREVIEW first, show the user, then CONFIRM or DRAFT with the confirmation_token from the preview.
    async fn send_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "PREVIEW first, then CONFIRM to send or DRAFT to save")]
        action: SendAction,
        #[graphql(desc = "Recipient email address(es), comma-separated")] to: String,
        #[graphql(desc = "Email subject line")] subject: String,
        #[graphql(desc = "Email body text")] body: String,
        #[graphql(desc = "CC recipients, comma-separated")] cc: Option<String>,
        #[graphql(desc = "BCC recipients (hidden), comma-separated")] bcc: Option<String>,
        #[graphql(desc = "Send from a specific identity/email address")] from: Option<String>,
        #[graphql(desc = "HTML body content (alternative to plain text)")] html_body: Option<
            String,
        >,
        #[graphql(desc = "Token from PREVIEW response — required for CONFIRM/DRAFT")]
        confirmation_token: Option<String>,
    ) -> Result<GqlComposeResult> {
        let to_addrs = parse_addresses(&to);
        let cc_addrs = cc.as_deref().map(parse_addresses).unwrap_or_default();
        let bcc_addrs = bcc.as_deref().map(parse_addresses).unwrap_or_default();
        let nonce_store = ctx.data::<super::types::NonceStore>()?;
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let mut client = client.lock().await;
        let sender = client.resolve_my_email(from.as_deref()).await;
        let binding = compose_binding(
            &client,
            "send",
            serde_json::json!({
                "to": to_addrs, "cc": cc_addrs, "bcc": bcc_addrs,
                "subject": subject, "body": body, "html": html_body,
                "from": from, "sender": sender,
            }),
        )?;
        let params = [binding.as_str()];

        if matches!(action, SendAction::Preview) {
            let nonce = super::types::issue_nonce(nonce_store, &params).await;
            let mut preview =
                format_send_preview(&to_addrs, &cc_addrs, &bcc_addrs, &subject, &body);
            append_compose_details(&mut preview, sender.as_deref(), html_body.as_deref());
            return Ok(GqlComposeResult::previewed(preview, nonce));
        }

        if let Err(msg) =
            super::types::consume_nonce(nonce_store, confirmation_token.as_deref(), &params).await
        {
            return Ok(GqlComposeResult::failed(msg.to_string()));
        }

        let draft = matches!(action, SendAction::Draft);
        match client
            .send_email(
                to_addrs,
                &subject,
                &body,
                None,
                crate::jmap::ComposeParams {
                    cc: cc_addrs,
                    bcc: bcc_addrs,
                    from: sender.as_deref().or(from.as_deref()),
                    draft,
                    html_body,
                    attachments: vec![],
                },
            )
            .await
        {
            Ok(email_id) => Ok(GqlComposeResult::completed(email_id)),
            Err(e) => Ok(GqlComposeResult::failed(e.to_string())),
        }
    }

    /// Reply to an existing email thread. ALWAYS use action=PREVIEW first.
    async fn reply_to_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "PREVIEW first, then CONFIRM to send or DRAFT to save")]
        action: SendAction,
        #[graphql(desc = "The email ID to reply to")] email_id: String,
        #[graphql(desc = "Reply body text (your response, without quoting original)")] body: String,
        #[graphql(desc = "Reply to all recipients")] all: Option<bool>,
        #[graphql(desc = "CC recipients, comma-separated")] cc: Option<String>,
        #[graphql(desc = "BCC recipients, comma-separated")] bcc: Option<String>,
        #[graphql(desc = "Send from a specific identity/email address")] from: Option<String>,
        #[graphql(desc = "HTML body content (alternative to plain text)")] html_body: Option<
            String,
        >,
        #[graphql(desc = "Token from PREVIEW response — required for CONFIRM/DRAFT")]
        confirmation_token: Option<String>,
    ) -> Result<GqlComposeResult> {
        let nonce_store = ctx.data::<super::types::NonceStore>()?;
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let mut client = client.lock().await;

        let original = client.get_email(&email_id).await?;
        let reply_all = all.unwrap_or(false);
        let extra_cc = cc.as_deref().map(parse_addresses).unwrap_or_default();
        let bcc_addrs = bcc.as_deref().map(parse_addresses).unwrap_or_default();

        let subject = prefixed_subject(original.subject.as_deref(), "Re:");

        // Compute the final recipient lists once, up front. Both PREVIEW
        // (for display) and CONFIRM/DRAFT (for the actual send) use these
        // exact values — preview and send can't diverge because they share
        // the same variables, not the same code path.
        let my_email = client.resolve_my_email(from.as_deref()).await;
        let (to_addrs, cc_addrs) = crate::jmap::expand_reply_recipients(
            &original,
            reply_all,
            my_email.as_deref(),
            extra_cc,
        );

        let binding = compose_binding(
            &client,
            "reply",
            serde_json::json!({
                "id": email_id, "to": to_addrs, "cc": cc_addrs, "bcc": bcc_addrs,
                "subject": subject, "body": body, "html": html_body,
                "from": from, "sender": my_email, "all": reply_all,
                "message_id": original.message_id, "references": original.references,
            }),
        )?;
        let params = [binding.as_str()];

        if matches!(action, SendAction::Preview) {
            let nonce = super::types::issue_nonce(nonce_store, &params).await;
            let in_reply_to = original
                .message_id
                .as_ref()
                .and_then(|v| v.first())
                .cloned()
                .unwrap_or_else(|| "(none)".to_string());
            let mut preview = format!(
                "REPLY PREVIEW:\nTo: {}\nCC: {}\nBCC: {}\nSubject: {}\nIn-Reply-To: {}\n\n--- Your Reply ---\n{}",
                format_addrs(&to_addrs),
                format_addrs(&cc_addrs),
                format_addrs(&bcc_addrs),
                subject,
                in_reply_to,
                body
            );
            append_compose_details(&mut preview, my_email.as_deref(), html_body.as_deref());
            return Ok(GqlComposeResult::previewed(preview, nonce));
        }

        if let Err(msg) =
            super::types::consume_nonce(nonce_store, confirmation_token.as_deref(), &params).await
        {
            return Ok(GqlComposeResult::failed(msg.to_string()));
        }

        let draft = matches!(action, SendAction::Draft);
        match client
            .reply_email(
                &original,
                &body,
                to_addrs,
                crate::jmap::ComposeParams {
                    cc: cc_addrs,
                    bcc: bcc_addrs,
                    from: my_email.as_deref().or(from.as_deref()),
                    draft,
                    html_body,
                    attachments: vec![],
                },
            )
            .await
        {
            Ok(email_id) => Ok(GqlComposeResult::completed(email_id)),
            Err(e) => Ok(GqlComposeResult::failed(e.to_string())),
        }
    }

    /// Forward an email to new recipients. ALWAYS use action=PREVIEW first.
    async fn forward_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "PREVIEW first, then CONFIRM to send or DRAFT to save")]
        action: SendAction,
        #[graphql(desc = "The email ID to forward")] email_id: String,
        #[graphql(desc = "Recipient email address(es), comma-separated")] to: String,
        #[graphql(desc = "Your message to include above forwarded content")] body: Option<String>,
        #[graphql(desc = "CC recipients, comma-separated")] cc: Option<String>,
        #[graphql(desc = "BCC recipients, comma-separated")] bcc: Option<String>,
        #[graphql(desc = "Send from a specific identity/email address")] from: Option<String>,
        #[graphql(desc = "HTML body content (alternative to plain text)")] html_body: Option<
            String,
        >,
        #[graphql(desc = "Token from PREVIEW response — required for CONFIRM/DRAFT")]
        confirmation_token: Option<String>,
    ) -> Result<GqlComposeResult> {
        let body_str = body.as_deref().unwrap_or("");
        let nonce_store = ctx.data::<super::types::NonceStore>()?;
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let mut client = client.lock().await;

        let original = client.get_email(&email_id).await?;
        let to_addrs = parse_addresses(&to);
        let cc_addrs = cc.as_deref().map(parse_addresses).unwrap_or_default();
        let bcc_addrs = bcc.as_deref().map(parse_addresses).unwrap_or_default();

        let subject = prefixed_subject(original.subject.as_deref(), "Fwd:");

        let sender = client.resolve_my_email(from.as_deref()).await;
        let binding = compose_binding(
            &client,
            "forward",
            serde_json::json!({
                "id": email_id, "to": to_addrs, "cc": cc_addrs, "bcc": bcc_addrs,
                "subject": subject, "body": body_str, "html": html_body,
                "from": from, "sender": sender,
                "original_body": original.text_content(), "original_from": original.from,
                "original_date": original.received_at, "original_subject": original.subject,
            }),
        )?;
        let params = [binding.as_str()];

        if matches!(action, SendAction::Preview) {
            let nonce = super::types::issue_nonce(nonce_store, &params).await;
            let original_body = original.text_content().unwrap_or_default();
            let original_sender = format_addrs(&original.from.clone().unwrap_or_default());

            let mut preview = format!(
                "FORWARD PREVIEW:\nTo: {}\nCC: {}\nBCC: {}\nSubject: {}\nForwarding from: {}\n\n--- Your Message ---\n{}\n\n--- Forwarded ---\nFrom: {}\nDate: {}\nSubject: {}\n\n{}",
                format_addrs(&to_addrs),
                format_addrs(&cc_addrs),
                format_addrs(&bcc_addrs),
                subject,
                original_sender,
                body_str,
                original_sender,
                original.received_at.as_deref().unwrap_or("unknown"),
                original.subject.as_deref().unwrap_or(""),
                original_body,
            );
            append_compose_details(&mut preview, sender.as_deref(), html_body.as_deref());
            return Ok(GqlComposeResult::previewed(preview, nonce));
        }

        if let Err(msg) =
            super::types::consume_nonce(nonce_store, confirmation_token.as_deref(), &params).await
        {
            return Ok(GqlComposeResult::failed(msg.to_string()));
        }

        let draft = matches!(action, SendAction::Draft);
        match client
            .forward_email(
                &original,
                to_addrs,
                body_str,
                crate::jmap::ComposeParams {
                    cc: cc_addrs,
                    bcc: bcc_addrs,
                    from: sender.as_deref().or(from.as_deref()),
                    draft,
                    html_body,
                    attachments: vec![],
                },
            )
            .await
        {
            Ok(email_id) => Ok(GqlComposeResult::completed(email_id)),
            Err(e) => Ok(GqlComposeResult::failed(e.to_string())),
        }
    }

    /// Move an email to a different mailbox/folder.
    async fn move_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The email ID to move")] email_id: String,
        #[graphql(desc = "Target mailbox name (e.g., 'Archive', 'Trash') or role")]
        target_mailbox: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let mut client = client.lock().await;

        let email = client.get_email(&email_id).await?;
        let target = client.find_mailbox(&target_mailbox).await?;

        match client.move_email(&email_id, &target.id).await {
            Ok(()) => Ok(GqlStatus::completed(format!(
                "Moved \"{}\" to {}",
                email.subject.as_deref().unwrap_or("(no subject)"),
                target.name
            ))),
            Err(e) => Ok(GqlStatus::failed(e.to_string())),
        }
    }

    /// Mark an email as read or unread.
    async fn mark_as_read(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The email ID")] email_id: String,
        #[graphql(desc = "true to mark read, false to mark unread (default: true)")] read: Option<
            bool,
        >,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let client = client.lock().await;
        let read = read.unwrap_or(true);

        let email = client.get_email(&email_id).await?;
        match client.mark_read(&email_id, read).await {
            Ok(()) => {
                let status = if read { "read" } else { "unread" };
                Ok(GqlStatus::completed(format!(
                    "Marked \"{}\" as {status}",
                    email.subject.as_deref().unwrap_or("(no subject)")
                )))
            }
            Err(e) => Ok(GqlStatus::failed(e.to_string())),
        }
    }

    /// Mark an email as spam. Moves to Junk AND trains the spam filter. MUST use action=PREVIEW first.
    async fn mark_as_spam(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The email ID")] email_id: String,
        #[graphql(desc = "PREVIEW first, then CONFIRM")] action: SpamAction,
        #[graphql(desc = "One-shot token from PREVIEW, required for CONFIRM")]
        confirmation_token: Option<String>,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let mut client = client.lock().await;
        let store = ctx.data::<super::types::NonceStore>()?;
        let binding = compose_binding(
            &client,
            "markAsSpam",
            serde_json::json!({"emailId":email_id, "action":"CONFIRM"}),
        )?;
        let params = [binding.as_str()];
        if matches!(action, SpamAction::Confirm)
            && let Err(error) =
                super::types::consume_nonce(store, confirmation_token.as_deref(), &params).await
        {
            return Ok(GqlStatus::failed(error.into()));
        }
        let email = client.get_email(&email_id).await?;

        if matches!(action, SpamAction::Preview) {
            let sender = email
                .from
                .as_ref()
                .and_then(|f| f.first())
                .map(|a| a.to_string())
                .unwrap_or_else(|| "(unknown)".to_string());
            return Ok(GqlStatus {
                confirmation_token: Some(super::types::issue_nonce(store, &params).await),
                success: true,
                message: Some(format!(
                    "SPAM PREVIEW — This will:\n1. Move to Junk folder\n2. Train spam filter\n\nEmail: \"{}\"\nFrom: {}\n\nUse action=CONFIRM with confirmationToken to proceed.",
                    email.subject.as_deref().unwrap_or("(no subject)"),
                    sender
                )),
                error: None,
            });
        }

        match client.mark_spam(&email_id).await {
            Ok(()) => Ok(GqlStatus::completed(format!(
                "Marked as spam: \"{}\"",
                email.subject.as_deref().unwrap_or("(no subject)")
            ))),
            Err(e) => Ok(GqlStatus::failed(e.to_string())),
        }
    }

    /// Create a new masked email address.
    async fn create_masked_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The website/domain this masked email is for")] for_domain: Option<String>,
        #[graphql(desc = "A note to remember what this is for")] description: Option<String>,
        #[graphql(desc = "Custom prefix for the email address")] prefix: Option<String>,
    ) -> Result<GqlMaskedEmail> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let client = client.lock().await;
        let masked = client
            .create_masked_email(
                for_domain.as_deref(),
                description.as_deref(),
                prefix.as_deref(),
            )
            .await?;
        Ok(GqlMaskedEmail::from(masked))
    }

    /// Enable a disabled masked email address.
    async fn enable_masked_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The masked email ID")] id: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let client = client.lock().await;
        match client
            .update_masked_email(&id, Some("enabled"), None, None)
            .await
        {
            Ok(()) => Ok(GqlStatus::completed(format!("Masked email {id} enabled."))),
            Err(e) => Ok(GqlStatus::failed(e.to_string())),
        }
    }

    /// Disable a masked email address. Emails sent to it will be rejected.
    async fn disable_masked_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The masked email ID")] id: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let client = client.lock().await;
        match client
            .update_masked_email(&id, Some("disabled"), None, None)
            .await
        {
            Ok(()) => Ok(GqlStatus::completed(format!("Masked email {id} disabled."))),
            Err(e) => Ok(GqlStatus::failed(e.to_string())),
        }
    }

    /// Permanently delete a masked email address. Cannot be undone!
    async fn delete_masked_email(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "The masked email ID")] id: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<crate::mcp::graphql::SharedClient>()?;
        let client = client.lock().await;
        match client
            .update_masked_email(&id, Some("deleted"), None, None)
            .await
        {
            Ok(()) => Ok(GqlStatus::completed(format!("Masked email {id} deleted."))),
            Err(e) => Ok(GqlStatus::failed(e.to_string())),
        }
    }

    /// Create a new contact via CardDAV. Requires FASTMAIL_USERNAME and FASTMAIL_APP_PASSWORD.
    async fn create_contact(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Full name")] name: String,
        #[graphql(desc = "Email address")] email: Option<String>,
        #[graphql(desc = "Phone number")] phone: Option<String>,
        #[graphql(desc = "Organization/company")] organization: Option<String>,
        #[graphql(desc = "Job title")] title: Option<String>,
        #[graphql(desc = "Notes")] notes: Option<String>,
    ) -> Result<GqlContact> {
        let client = ctx.data::<super::CardDavCreds>()?.client()?;
        let emails = ContactEmail::parse_list(email.as_deref());
        let phones = ContactPhone::parse_list(phone.as_deref());

        let contact = client
            .create_contact(&crate::carddav::ContactFields {
                name: Some(&name),
                emails: Some(&emails),
                phones: Some(&phones),
                organization: organization.as_deref(),
                title: title.as_deref(),
                notes: notes.as_deref(),
            })
            .await?;
        Ok(GqlContact::from(contact))
    }

    /// Update an existing contact via CardDAV. Only provided fields are changed; others are preserved.
    async fn update_contact(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Contact ID (UID from the vCard)")] id: String,
        #[graphql(desc = "New full name")] name: Option<String>,
        #[graphql(desc = "New email address (replaces existing)")] email: Option<String>,
        #[graphql(desc = "New phone number (replaces existing)")] phone: Option<String>,
        #[graphql(desc = "New organization/company")] organization: Option<String>,
        #[graphql(desc = "New job title")] title: Option<String>,
        #[graphql(desc = "New notes")] notes: Option<String>,
    ) -> Result<GqlContact> {
        let client = ctx.data::<super::CardDavCreds>()?.client()?;
        let emails = ContactEmail::parse_list(email.as_deref());
        let phones = ContactPhone::parse_list(phone.as_deref());

        let contact = client
            .update_contact(
                &id,
                &crate::carddav::ContactFields {
                    name: name.as_deref(),
                    emails: (!emails.is_empty()).then_some(emails.as_slice()),
                    phones: (!phones.is_empty()).then_some(phones.as_slice()),
                    organization: organization.as_deref(),
                    title: title.as_deref(),
                    notes: notes.as_deref(),
                },
            )
            .await?;
        Ok(GqlContact::from(contact))
    }

    /// Delete a contact via CardDAV. Cannot be undone!
    async fn delete_contact(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Contact ID (UID from the vCard)")] id: String,
    ) -> Result<GqlStatus> {
        let client = ctx.data::<super::CardDavCreds>()?.client()?;
        match client.delete_contact(&id).await {
            Ok(()) => Ok(GqlStatus::completed(format!("Contact {id} deleted."))),
            Err(e) => Ok(GqlStatus::failed(e.to_string())),
        }
    }
}

// ============ Formatting helpers (preview only) ============

fn compose_binding(
    client: &crate::jmap::JmapClient,
    operation: &str,
    payload: serde_json::Value,
) -> Result<String> {
    let session = client.session()?;
    Ok(serde_json::to_string(&serde_json::json!({
        "operation": operation,
        "account": [Some(session.username.as_str()), session.primary_account_id()],
        "payload": payload,
    }))?)
}

fn append_compose_details(preview: &mut String, sender: Option<&str>, html: Option<&str>) {
    preview.push_str(&format!("\nFrom: {}", sender.unwrap_or("(unresolved)")));
    if let Some(html) = html {
        preview.push_str(&format!("\n--- HTML Body ---\n{html}"));
    }
}

fn format_addrs(addrs: &[EmailAddress]) -> String {
    if addrs.is_empty() {
        "(none)".to_string()
    } else {
        addrs
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn format_send_preview(
    to: &[EmailAddress],
    cc: &[EmailAddress],
    bcc: &[EmailAddress],
    subject: &str,
    body: &str,
) -> String {
    format!(
        "EMAIL PREVIEW:\nTo: {}\nCC: {}\nBCC: {}\nSubject: {}\n\n--- Body ---\n{}\n\nTo send: use action=CONFIRM. To save draft: use action=DRAFT.",
        format_addrs(to),
        format_addrs(cc),
        format_addrs(bcc),
        subject,
        body
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_preview_preserves_empty_recipient_labels() {
        assert_eq!(
            format_send_preview(
                &parse_addresses("Name <to@example.com>"),
                &[],
                &[],
                "Subject",
                "Body"
            ),
            "EMAIL PREVIEW:\nTo: Name <to@example.com>\nCC: (none)\nBCC: (none)\nSubject: Subject\n\n--- Body ---\nBody\n\nTo send: use action=CONFIRM. To save draft: use action=DRAFT.",
        );
    }
}
