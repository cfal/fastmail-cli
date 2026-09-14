use std::sync::{Arc, Mutex};

use html_to_markdown_rs::visitor::{HtmlVisitor, NodeContext, VisitResult};
use html_to_markdown_rs::{ConversionOptions, OutputFormat, PreprocessingOptions, convert};
use serde::Serialize;

use super::{Email, EmailBodyPart};

const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_PARTS: usize = 128;
const MAX_DEPTH: usize = 64;

/// Which JMAP body alternative and output format to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BodyPreference {
    /// Prefer plain-text parts, converting any HTML parts to Markdown.
    #[default]
    Auto,
    /// Prefer the HTML alternative and render all selected parts as Markdown.
    Markdown,
    /// Prefer plain-text parts, rendering any HTML parts as plain text.
    Text,
}

/// The actual format of a readable body, never raw HTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadableBodyFormat {
    Text,
    Markdown,
}

/// A selected JMAP body part. Its unchanged value is available in `bodyValues`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadableBodySource {
    pub part_id: Option<String>,
    #[serde(rename = "type")]
    pub content_type: Option<String>,
}

/// Best-effort reading view, not a sanitized or authoritative replacement for MIME.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadableBody {
    pub format: ReadableBodyFormat,
    pub content: String,
    pub source_parts: Vec<ReadableBodySource>,
    pub is_truncated: bool,
    pub is_encoding_problem: bool,
    pub warnings: Vec<String>,
}

impl ReadableBody {
    fn warn(&mut self, warning: &str) {
        if !self.warnings.iter().any(|existing| existing == warning) {
            self.warnings.push(warning.to_owned());
        }
    }

    fn append(&mut self, content: &str) {
        if !self.content.is_empty() && !content.is_empty() {
            let remaining = MAX_OUTPUT_BYTES - self.content.len();
            self.content.push_str(&"\n\n"[..remaining.min(2)]);
        }
        let remaining = MAX_OUTPUT_BYTES - self.content.len();
        let prefix = utf8_prefix(content, remaining);
        self.content.push_str(prefix);
        if prefix.len() < content.len() {
            self.is_truncated = true;
            self.warn(
                "Readable body reached the 1 MiB output limit; inspect the original body values.",
            );
        }
    }
}

impl Email {
    /// Read the JMAP-selected body sequence without joining alternative representations.
    /// Raw body values and the legacy joined-body helpers remain unchanged.
    pub fn readable_body(&self, preference: BodyPreference) -> Option<ReadableBody> {
        let text = self.text_body.as_deref().filter(|p| !p.is_empty());
        let html = self.html_body.as_deref().filter(|p| !p.is_empty());
        let parts = match preference {
            BodyPreference::Markdown => html.or(text),
            BodyPreference::Auto | BodyPreference::Text => text.or(html),
        }?;
        let format = match preference {
            BodyPreference::Text => ReadableBodyFormat::Text,
            BodyPreference::Auto if !parts.iter().any(|p| is_type(p, "text/html")) => {
                ReadableBodyFormat::Text
            }
            _ => ReadableBodyFormat::Markdown,
        };
        let mut body = ReadableBody {
            format,
            content: String::new(),
            source_parts: Vec::new(),
            is_truncated: false,
            is_encoding_problem: false,
            warnings: Vec::new(),
        };
        if parts.len() > MAX_PARTS {
            body.is_truncated = true;
            body.warn(
                "Readable body reached the 128-part limit; inspect the original body values.",
            );
        }
        let mut remaining_input = MAX_INPUT_BYTES;
        for (index, part) in parts.iter().take(MAX_PARTS).enumerate() {
            body.source_parts.push(ReadableBodySource {
                part_id: part.part_id.clone(),
                content_type: part.content_type.clone(),
            });
            let value = part
                .part_id
                .as_ref()
                .and_then(|id| self.body_values.as_ref()?.get(id));
            if let Some(value) = value {
                body.is_truncated |= value.is_truncated;
                body.is_encoding_problem |= value.is_encoding_problem;
                if value.is_truncated {
                    body.warn(
                        "JMAP truncated a selected body value; this reading view is incomplete.",
                    );
                }
                if value.is_encoding_problem {
                    body.warn("JMAP reported an encoding problem in a selected body value.");
                }
            }
            if !is_type(part, "text/plain") && !is_type(part, "text/html") {
                body.warn("A non-text or untyped body part was omitted; inspect its original metadata or attachment.");
                body.append("[Non-text body part omitted]");
                continue;
            }
            let Some(value) = value else {
                body.warn("A selected body value is unavailable; this reading view is incomplete.");
                body.append("[Body part unavailable]");
                continue;
            };
            if value.value.len() > remaining_input {
                body.is_truncated = true;
                body.warn("Readable body reached the 1 MiB input limit; inspect the original body values.");
                // Do not parse half an HTML attribute or invent a closing-tag structure.
                if is_type(part, "text/html") {
                    body.append("[HTML body part exceeds the conversion limit]");
                    break;
                }
            }
            let input = utf8_prefix(&value.value, remaining_input);
            remaining_input -= input.len();
            if is_type(part, "text/html") {
                convert_html(input, &mut body);
            } else if format == ReadableBodyFormat::Markdown {
                body.append(&literal_markdown(input));
            } else {
                body.append(input);
            }
            if body.content.len() == MAX_OUTPUT_BYTES && index + 1 < parts.len() {
                body.is_truncated = true;
                body.warn("Readable body reached the 1 MiB output limit; inspect the original body values.");
            }
            if input.len() < value.value.len() || body.content.len() == MAX_OUTPUT_BYTES {
                break;
            }
        }
        Some(body)
    }
}

fn is_type(part: &EmailBodyPart, expected: &str) -> bool {
    part.content_type.as_deref().is_some_and(|mime| {
        mime.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case(expected)
    })
}

fn utf8_prefix(value: &str, limit: usize) -> &str {
    &value[..value.floor_char_boundary(limit.min(value.len()))]
}

fn escape_markdown(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_punctuation() {
            output.push('\\');
        }
        output.push(ch);
    }
    output
}

fn literal_markdown(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    // A fence longer than any source run preserves both layout and literal Markdown.
    let longest_run = value.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(3.max(longest_run + 1));
    let newline = if value.ends_with('\n') { "" } else { "\n" };
    format!("{fence}text\n{value}{newline}{fence}")
}

#[derive(Debug, Default)]
struct EmailHtmlVisitor {
    markdown: bool,
    omitted_images: bool,
    omitted_media: bool,
    omitted_graphics: bool,
}

impl EmailHtmlVisitor {
    fn placeholder(&self, label: &str) -> VisitResult {
        VisitResult::Custom(if self.markdown {
            format!("\\[{}\\]", escape_markdown(label))
        } else {
            format!("[{label}]")
        })
    }
}

impl HtmlVisitor for EmailHtmlVisitor {
    fn visit_element_start(&mut self, ctx: &NodeContext<'_>) -> VisitResult {
        match ctx.tag_name.as_ref() {
            "img" => {
                self.omitted_images = true;
                let alt = ctx
                    .attributes()
                    .get("alt")
                    .map(String::as_str)
                    .unwrap_or("");
                let alt = html_escape::decode_html_entities(alt);
                if alt.trim().is_empty() {
                    self.placeholder("Image omitted")
                } else {
                    self.placeholder(&format!("Image: {}", alt.trim()))
                }
            }
            "svg" | "math" | "iframe" | "object" | "embed" | "video" | "audio" => {
                self.omitted_media = true;
                self.omitted_graphics |= matches!(ctx.tag_name.as_ref(), "svg" | "math");
                self.placeholder("Embedded media omitted")
            }
            "script" | "style" | "head" | "template" => VisitResult::Skip,
            _ => VisitResult::Continue,
        }
    }

    fn visit_element_end(&mut self, ctx: &NodeContext<'_>, output: &str) -> VisitResult {
        if !self.markdown
            && ctx.tag_name == "a"
            && let Some(href) = ctx.attributes().get("href")
        {
            let href = html_escape::decode_html_entities(href);
            if !href.is_empty() && output.trim() != href {
                return VisitResult::Custom(format!("{output} ({href})"));
            }
        }
        VisitResult::Continue
    }
}

fn convert_html(input: &str, body: &mut ReadableBody) {
    let visitor = Arc::new(Mutex::new(EmailHtmlVisitor {
        markdown: body.format == ReadableBodyFormat::Markdown,
        ..Default::default()
    }));
    let options = ConversionOptions {
        output_format: match body.format {
            ReadableBodyFormat::Markdown => OutputFormat::Markdown,
            ReadableBodyFormat::Text => OutputFormat::Plain,
        },
        preprocessing: PreprocessingOptions {
            enabled: false,
            ..Default::default()
        },
        extract_metadata: false,
        include_document_structure: false,
        extract_images: false,
        capture_svg: false,
        infer_dimensions: false,
        max_depth: Some(MAX_DEPTH),
        compact_tables: true,
        escape_asterisks: true,
        escape_underscores: true,
        escape_misc: true,
        autolinks: false,
        visitor: Some(visitor.clone()),
        ..Default::default()
    };
    match convert(input, options) {
        Ok(result) => {
            for warning in result.warnings {
                use html_to_markdown_rs::types::WarningKind;
                match warning.kind {
                    WarningKind::DepthLimitExceeded => {
                        body.is_truncated = true;
                        body.warn("HTML exceeded the 64-level conversion depth limit; deeper content was omitted.");
                    }
                    WarningKind::TruncatedInput => {
                        body.is_truncated = true;
                        body.warn("The HTML converter truncated content; inspect the original body values.");
                    }
                    _ => body.warn("The HTML converter reported a best-effort conversion warning; inspect the original body values."),
                }
            }
            body.append(
                result
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .trim_matches('\n'),
            );
        }
        Err(_) => {
            body.warn("HTML conversion failed; inspect the original body values.");
            body.append("[HTML conversion unavailable]");
        }
    }
    let visitor = visitor.lock().unwrap();
    if !visitor.markdown && visitor.omitted_graphics {
        // The converter's plain walker skips SVG/MathML before invoking visitors.
        body.append("[Embedded media omitted]");
    }
    if visitor.omitted_images {
        body.warn(
            "Images were not loaded; alt text is not a substitute for inspecting image content.",
        );
    }
    if visitor.omitted_media {
        body.warn(
            "Embedded media was not loaded; inspect the original body or attachments when needed.",
        );
    }
}
