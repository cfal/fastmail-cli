use super::body::{EmailBodyFormat, EmailReading};
use crate::jmap::authenticated_client;
use crate::models::Output;

pub async fn get_thread(email_id: &str) -> anyhow::Result<()> {
    get_thread_with_body_format(email_id, EmailBodyFormat::Auto).await
}

pub async fn get_thread_with_body_format(
    email_id: &str,
    format: EmailBodyFormat,
) -> anyhow::Result<()> {
    let client = authenticated_client().await?;

    let emails = client.get_thread(email_id).await?;
    let readings: Vec<_> = emails
        .iter()
        .map(|email| EmailReading::new(email, format))
        .collect();
    Output::success(readings).print();

    Ok(())
}
