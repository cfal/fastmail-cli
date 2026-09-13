use crate::jmap::authenticated_client;
use crate::models::Output;

pub async fn mark_read(email_id: &str, read: bool) -> anyhow::Result<()> {
    let client = authenticated_client().await?;

    client.mark_read(email_id, read).await?;

    let status = if read { "read" } else { "unread" };
    Output::<()>::success_msg(format!("Email marked as {}", status)).print();

    Ok(())
}
