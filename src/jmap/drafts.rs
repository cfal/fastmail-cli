use super::responses::{invalid, method_response, require_updated, submission_response};
use super::{EMAIL_FULL_PROPERTIES, EMAIL_SUMMARY_PROPERTIES, JmapClient, mailbox_by_role};
use crate::error::{Error, Result};
use crate::models::{Email, Identity};
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

fn require_update(responses: &[Value], account: &str, id: &str, call_id: &str) -> Result<()> {
    let data = method_response(responses, "Email/set", call_id, account)?;
    require_updated(data, "Email/set", id)
}

impl JmapClient {
    pub(crate) async fn prepare_draft_send(&self) -> Result<DraftMailboxes> {
        self.require_capability("urn:ietf:params:jmap:submission", "Email sending")?;
        let mailboxes = self.fetch_mailboxes().await?;
        Ok(DraftMailboxes {
            sent: mailbox_by_role(&mailboxes, "sent")?.id.clone(),
            drafts: mailbox_by_role(&mailboxes, "drafts")?.id.clone(),
            trash: mailbox_by_role(&mailboxes, "trash")?.id.clone(),
        })
    }

    pub(crate) async fn draft_snapshot(&self, id: &str, full: bool) -> Result<DraftSnapshot> {
        let account = self.account_id()?;
        let properties = if full {
            EMAIL_FULL_PROPERTIES
        } else {
            EMAIL_SUMMARY_PROPERTIES
        };
        let responses = self
            .request(vec![json!([
                "Email/get", {
                    "accountId": account, "ids": [id],
                    "properties": properties,
                    "fetchTextBodyValues": full, "fetchHTMLBodyValues": full,
                    "maxBodyValueBytes": 1024 * 1024
                }, "draft"
            ])])
            .await?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Snapshot {
            account_id: String,
            state: String,
            list: Vec<Email>,
            not_found: Vec<String>,
        }
        let data: Snapshot = serde_json::from_value(
            method_response(&responses, "Email/get", "draft", account)?.clone(),
        )
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
        let (id, filing_warning) =
            submission_response(&responses, "submitDraft", account, &draft.id)?;
        Ok(DraftSubmission { id, filing_warning })
    }

    pub(crate) async fn discard_draft(&mut self, id: &str) -> Result<()> {
        let trash = mailbox_by_role(&self.fetch_mailboxes().await?, "trash")?
            .id
            .clone();
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
