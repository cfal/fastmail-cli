use super::body::{EmailBodyFormat, EmailReading};
use crate::jmap::authenticated_client;
use crate::models::Output;

pub async fn get_email(email_id: &str) -> anyhow::Result<()> {
    get_email_with_body_format(email_id, EmailBodyFormat::Auto).await
}

pub async fn get_email_with_body_format(
    email_id: &str,
    format: EmailBodyFormat,
) -> anyhow::Result<()> {
    let client = authenticated_client().await?;

    let email = client.get_email(email_id).await?;
    Output::success(EmailReading::new(&email, format)).print();

    Ok(())
}
