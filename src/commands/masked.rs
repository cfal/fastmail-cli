use crate::jmap::authenticated_client;
use crate::models::Output;

pub async fn list_masked_emails() -> anyhow::Result<()> {
    let client = authenticated_client().await?;

    let masked_emails = client.list_masked_emails().await?;

    Output::success(masked_emails).print();
    Ok(())
}

pub async fn create_masked_email(
    for_domain: Option<&str>,
    description: Option<&str>,
    prefix: Option<&str>,
) -> anyhow::Result<()> {
    let client = authenticated_client().await?;

    let masked_email = client
        .create_masked_email(for_domain, description, prefix)
        .await?;

    Output::success(masked_email).print();
    Ok(())
}

pub async fn enable_masked_email(id: &str) -> anyhow::Result<()> {
    set_masked_email_state(id, "enabled").await
}

pub async fn disable_masked_email(id: &str) -> anyhow::Result<()> {
    set_masked_email_state(id, "disabled").await
}

pub async fn delete_masked_email(id: &str) -> anyhow::Result<()> {
    set_masked_email_state(id, "deleted").await
}

async fn set_masked_email_state(id: &str, state: &str) -> anyhow::Result<()> {
    let client = authenticated_client().await?;

    client
        .update_masked_email(id, Some(state), None, None)
        .await?;

    Output::<()>::success_msg(format!("Masked email {id} {state}")).print();
    Ok(())
}
