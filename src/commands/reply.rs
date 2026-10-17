use crate::jmap::{ComposeParams, authenticated_client, expand_reply_recipients};
use crate::models::Output;

pub async fn reply(
    email_id: &str,
    body: &str,
    reply_all: bool,
    params: ComposeParams<'_>,
) -> anyhow::Result<()> {
    let mut client = authenticated_client().await?;
    let draft = params.draft;

    let original = client.get_email(email_id).await?;

    // Expand reply-all and filter the sending identity on the caller side,
    // so this code path and the MCP preview path use the exact same helper.
    let identity = client.resolve_sender(params.from).await;
    let my_email = identity
        .as_ref()
        .ok()
        .map(|identity| identity.email.as_str());
    let (to_addrs, cc_addrs) = expand_reply_recipients(&original, reply_all, my_email, params.cc);
    let params = ComposeParams {
        cc: cc_addrs,
        ..params
    };

    let new_email_id = client
        .reply_email_with_identity(&original, body, to_addrs, params, Some(identity), vec![])
        .await?;

    #[derive(serde::Serialize)]
    struct ReplyResponse {
        email_id: String,
        in_reply_to: String,
        status: &'static str,
    }

    Output::success(ReplyResponse {
        email_id: new_email_id,
        in_reply_to: email_id.to_string(),
        status: if draft { "draft" } else { "sent" },
    })
    .print();

    Ok(())
}
