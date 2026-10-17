use super::{EMAIL_FULL_PROPERTIES, EMAIL_SUMMARY_PROPERTIES, JmapClient, method_error};
use crate::error::{Error, Result};
use crate::models::{Email, Identity, Mailbox};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) struct DraftSnapshot {
    pub email: Email,
    pub state: String,
}

pub(crate) struct DraftSubmission {
    pub id: String,
    pub filing_warning: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct DraftMailboxes {
    sent: String,
    drafts: String,
    trash: String,
}

impl DraftMailboxes {
    pub(crate) fn check_not_trashed(&self, email: &Email) -> Result<()> {
        if email.mailbox_ids.get(&self.trash) == Some(&true) {
            return Err(Error::Config(
                "Draft is in Trash; restore it before sending".into(),
            ));
        }
        Ok(())
    }
}

fn role_id(mailboxes: &[Mailbox], role: &str) -> Result<String> {
    let mut matches = mailboxes
        .iter()
        .filter(|mailbox| mailbox.role.as_deref() == Some(role));
    let mailbox = matches
        .next()
        .ok_or_else(|| Error::MailboxNotFound(role.into()))?;
    if matches.next().is_some() {
        return Err(Error::Config(format!(
            "Multiple mailboxes have role {role}"
        )));
    }
    Ok(mailbox.id.clone())
}

fn invalid(method: &str, description: &str) -> Error {
    Error::Jmap {
        method: method.into(),
        error_type: "parse".into(),
        description: description.into(),
    }
}

fn response<'a>(responses: &'a [Value], method: &str, call_id: &str) -> Result<&'a Value> {
    let mut matching = responses
        .iter()
        .filter(|value| value[0] == method && value[2] == call_id);
    if let Some(value) = matching.next() {
        if matching.next().is_some() || value.as_array().is_none_or(|array| array.len() != 3) {
            return Err(invalid(method, "Conflicting or malformed method responses"));
        }
        return Ok(&value[1]);
    }
    if let Some(error) = responses
        .iter()
        .find(|value| value[0] == "error" && value[2] == call_id)
    {
        return Err(method_error(method, &error[1], "Draft operation failed"));
    }
    Err(invalid(method, "Missing method response"))
}

fn require_update(responses: &[Value], account: &str, id: &str, call_id: &str) -> Result<()> {
    let data = response(responses, "Email/set", call_id)?;
    if data["accountId"] != account {
        return Err(invalid("Email/set", "Account mismatch"));
    }
    if let Some(error) = data["notUpdated"].get(id) {
        return Err(method_error("Email/set", error, "Draft update failed"));
    }
    if !data["updated"]
        .as_object()
        .is_some_and(|map| map.contains_key(id))
    {
        return Err(invalid("Email/set", "No successful draft update returned"));
    }
    Ok(())
}

impl JmapClient {
    pub(crate) async fn prepare_draft_send(&self) -> Result<DraftMailboxes> {
        self.require_capability("urn:ietf:params:jmap:submission", "Email sending")?;
        let mailboxes = self.fetch_mailboxes().await?;
        Ok(DraftMailboxes {
            sent: role_id(&mailboxes, "sent")?,
            drafts: role_id(&mailboxes, "drafts")?,
            trash: role_id(&mailboxes, "trash")?,
        })
    }

    pub(crate) async fn draft_snapshot(&self, id: &str, full: bool) -> Result<DraftSnapshot> {
        let account = self.account_id()?;
        let responses = self.request(vec![json!([
            "Email/get", {
                "accountId": account, "ids": [id],
                "properties": if full { EMAIL_FULL_PROPERTIES } else { EMAIL_SUMMARY_PROPERTIES },
                "fetchTextBodyValues": full, "fetchHTMLBodyValues": full,
                "maxBodyValueBytes": 1024 * 1024
            }, "draft"
        ])]).await?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Snapshot {
            account_id: String,
            state: String,
            list: Vec<Email>,
            not_found: Vec<String>,
        }
        let data: Snapshot =
            serde_json::from_value(response(&responses, "Email/get", "draft")?.clone())
                .map_err(|error| invalid("Email/get", &error.to_string()))?;
        if data.account_id != account {
            return Err(invalid("Email/get", "Account mismatch"));
        }
        if data.list.is_empty() && data.not_found == [id] {
            return Err(Error::EmailNotFound(id.into()));
        }
        if data.list.len() != 1 || !data.not_found.is_empty() || data.list[0].id != id {
            return Err(invalid("Email/get", "Unexpected draft IDs"));
        }
        let email = data.list.into_iter().next().unwrap();
        if email.keywords.get("$draft") != Some(&true) {
            return Err(Error::Config("Email is not a draft".into()));
        }
        Ok(DraftSnapshot {
            email,
            state: data.state,
        })
    }

    pub(crate) async fn send_draft_with_identity(
        &self,
        draft: &Email,
        identity: &Identity,
        mailboxes: &DraftMailboxes,
    ) -> Result<DraftSubmission> {
        self.require_capability("urn:ietf:params:jmap:submission", "Email sending")?;
        if draft.keywords.get("$draft") != Some(&true) {
            return Err(Error::Config("Email is not a draft".into()));
        }
        mailboxes.check_not_trashed(draft)?;
        let account = self.account_id()?;
        let mailbox_path =
            |id: &str| format!("mailboxIds/{}", id.replace('~', "~0").replace('/', "~1"));
        // The immutable Email ID pins content. JMAP cannot atomically test its
        // mutable $draft flag during submission. Filing only patches our fields.
        let responses = self
            .request(vec![json!([
                "EmailSubmission/set", {
                    "accountId": account,
                    "create": {"submission": {"identityId": identity.id, "emailId": draft.id}},
                    "onSuccessUpdateEmail": {"#submission": {
                        (mailbox_path(&mailboxes.drafts)): null,
                        (mailbox_path(&mailboxes.sent)): true,
                        "keywords/$draft": null,
                        "keywords/$seen": true
                    }}
                }, "submitDraft"
            ])])
            .await?;
        let data = response(&responses, "EmailSubmission/set", "submitDraft")?;
        if data["accountId"] != account {
            return Err(invalid("EmailSubmission/set", "Account mismatch"));
        }
        if let Some(error) = data["notCreated"].get("submission") {
            return Err(method_error(
                "EmailSubmission/set",
                error,
                "Draft submission failed",
            ));
        }
        let id = data["created"]["submission"]["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid("EmailSubmission/set", "No successful submission returned"))?
            .to_owned();
        // An implicit Email/set can fail after submission succeeds. Never hide
        // that success or encourage sending the same message a second time.
        let filing_warning = require_update(&responses, account, &draft.id, "submitDraft")
            .err().map(|error| format!("Submitted, but filing in Sent is unconfirmed: {error}. Do not resend; inspect the email's mailboxes."));
        Ok(DraftSubmission { id, filing_warning })
    }

    pub(crate) async fn discard_draft(&mut self, id: &str) -> Result<()> {
        let trash = role_id(&self.fetch_mailboxes().await?, "trash")?;
        let snapshot = self.draft_snapshot(id, false).await?;
        let account = self.account_id()?;
        let responses = self
            .request(vec![json!([
                "Email/set", {
                    "accountId": account, "ifInState": snapshot.state,
                    "update": {(id): {"mailboxIds": {(trash): true}}}
                }, "discardDraft"
            ])])
            .await?;
        require_update(&responses, account, id, "discardDraft")
    }
}
