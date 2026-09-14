use clap::ValueEnum;
use serde::Serialize;

use crate::models::{BodyPreference, Email, ReadableBody};

/// Reading view added to full CLI email records. Raw JMAP fields are unchanged.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum EmailBodyFormat {
    /// Prefer plain text; convert HTML-only parts to Markdown.
    #[default]
    Auto,
    /// Prefer the HTML alternative, converted to Markdown.
    Markdown,
    /// Prefer plain text; convert HTML-only parts to plain text.
    Text,
    /// Return only the original JMAP fields, without conversion.
    Raw,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EmailReading<'a> {
    #[serde(flatten)]
    email: &'a Email,
    #[serde(skip_serializing_if = "Option::is_none")]
    readable_body: Option<ReadableBody>,
}

impl<'a> EmailReading<'a> {
    pub(super) fn new(email: &'a Email, format: EmailBodyFormat) -> Self {
        let preference = match format {
            EmailBodyFormat::Auto => Some(BodyPreference::Auto),
            EmailBodyFormat::Markdown => Some(BodyPreference::Markdown),
            EmailBodyFormat::Text => Some(BodyPreference::Text),
            EmailBodyFormat::Raw => None,
        };
        Self {
            email,
            readable_body: preference.and_then(|p| email.readable_body(p)),
        }
    }
}
