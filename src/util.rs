use crate::jmap::AttachmentData;
use crate::models::EmailAddress;
use std::path::Path;

pub(crate) fn http_client() -> crate::error::Result<reqwest::Client> {
    static CLIENT: std::sync::LazyLock<Result<reqwest::Client, reqwest::Error>> =
        std::sync::LazyLock::new(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
        });
    CLIENT
        .as_ref()
        .cloned()
        .map_err(|e| crate::error::Error::Config(format!("Failed to initialize HTTP client: {e}")))
}

pub fn parse_addresses(input: &str) -> Vec<EmailAddress> {
    input
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Some(start) = s.find('<')
                && let Some(end) = s.find('>')
                && start < end
            {
                let name = s[..start].trim();
                let email = s[start + 1..end].trim();
                return EmailAddress {
                    name: if name.is_empty() {
                        None
                    } else {
                        Some(name.to_string())
                    },
                    email: email.to_string(),
                };
            }
            EmailAddress {
                name: None,
                email: s.to_string(),
            }
        })
        .collect()
}

// ============ Text Extraction ============

/// Extract text from attachment data using xberg
/// Supports: PDF, DOC, DOCX, ODT, XLSX, XLS, ODS, PPTX, PPT, EPUB, RTF,
/// HTML, XML, JSON, YAML, CSV, TSV, TXT, MD, EML, MSG, and more
/// NOTE: Returns None for images - use existing image pipeline instead
pub async fn extract_text(bytes: &[u8], filename: &str) -> anyhow::Result<Option<String>> {
    use xberg::{ExtractInput, ExtractionConfig, extract};

    // Skip images - we have our own pipeline for those (resize + send to Claude)
    if is_image_extension(filename) {
        return Ok(None);
    }

    let mime_type = mime_from_filename(filename);
    let config = ExtractionConfig {
        use_cache: false,
        disable_ocr: true,
        extraction_timeout_secs: Some(60),
        ..Default::default()
    };
    let input = ExtractInput::from_bytes(bytes.to_vec(), &mime_type, None);
    match extract(input, &config).await {
        Ok(result) => {
            let content = result
                .results
                .iter()
                .map(|r| r.content.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if content.is_empty() {
                Ok(None)
            } else {
                Ok(Some(content))
            }
        }
        Err(e) => {
            tracing::debug!("xberg extraction failed for {}: {}", filename, e);
            Ok(None)
        }
    }
}

/// Check if filename has an image extension (used to skip xberg for images)
fn is_image_extension(filename: &str) -> bool {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico" | "svg" | "heic"
    )
}

/// Infer MIME type from filename extension for documents
pub fn mime_from_filename(filename: &str) -> String {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        // Documents
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "odt" => "application/vnd.oasis.opendocument.text",
        "rtf" => "application/rtf",
        // Spreadsheets
        "xls" | "xla" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsm" => "application/vnd.ms-excel.sheet.macroEnabled.12",
        "xlsb" => "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
        "xlam" => "application/vnd.ms-excel.addin.macroEnabled.12",
        "xltm" => "application/vnd.ms-excel.template.macroEnabled.12",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        // Presentations
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ppsx" => "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
        // eBooks
        "epub" => "application/epub+zip",
        "fb2" => "application/x-fictionbook+xml",
        // Text & markup
        "txt" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" | "xhtml" => "text/html",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "rst" => "text/x-rst",
        "org" => "text/x-org",
        // Email
        "eml" => "message/rfc822",
        "msg" => "application/vnd.ms-outlook",
        // Archives
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "tgz" | "gz" => "application/gzip",
        "7z" => "application/x-7z-compressed",
        // Scientific & academic
        "bib" | "biblatex" => "application/x-bibtex",
        "ris" => "application/x-research-info-systems",
        "enw" => "application/x-endnote-refer",
        "csl" => "application/vnd.citationstyles.style+xml",
        "tex" | "latex" => "application/x-tex",
        "typst" => "application/x-typst",
        "jats" => "application/jats+xml",
        "ipynb" => "application/x-ipynb+json",
        "docbook" => "application/docbook+xml",
        // Documentation
        "opml" => "text/x-opml",
        "pod" => "text/x-pod",
        "mdoc" => "text/troff",
        "troff" => "text/troff",
        // Default - let xberg figure it out
        _ => "application/octet-stream",
    }
    .to_string()
}

// ============ Image Processing ============

/// Parse a human-readable size string like "500K", "1M", "1.5MB" into bytes
pub fn parse_size(s: &str) -> Result<usize, String> {
    let normalized = s.trim().to_ascii_uppercase();
    let size = normalized.strip_suffix('B').unwrap_or(&normalized);
    let (number, multiplier) = match size.as_bytes().last() {
        Some(b'K') => (&size[..size.len() - 1], 1024.0),
        Some(b'M') => (&size[..size.len() - 1], 1024.0 * 1024.0),
        Some(b'G') => (&size[..size.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (size, 1.0),
    };
    let bytes = number.parse::<f64>().unwrap_or(f64::NAN) * multiplier;
    if !bytes.is_finite() || bytes < 1.0 || bytes >= usize::MAX as f64 {
        return Err(
            "Size must be a positive byte count, optionally suffixed with K, M or G".into(),
        );
    }
    Ok(bytes as usize)
}

/// Check if content is an image based on MIME type or file extension
pub fn is_image(content_type: &str, filename: &str) -> bool {
    if content_type.starts_with("image/") {
        return true;
    }
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico" | "heic"
    )
}

/// Infer MIME type from filename extension (JMAP often returns application/octet-stream)
pub fn infer_image_mime(filename: &str) -> Option<&'static str> {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())?
        .to_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "tiff" | "tif" => Some("image/tiff"),
        _ => None,
    }
}

/// Default max size for MCP (Claude's ~1MB base64 limit means raw < 700KB)
pub const MCP_IMAGE_MAX_BYTES: usize = 700 * 1024;
pub const MAX_ATTACHMENT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn check_response_status(response: &reqwest::Response) -> crate::error::Result<()> {
    if !response.status().is_success() {
        return Err(crate::error::Error::Server(format!(
            "HTTP request failed ({})",
            response.status()
        )));
    }
    Ok(())
}

pub(crate) async fn read_bounded_response(
    mut response: reqwest::Response,
    limit: usize,
) -> crate::error::Result<Vec<u8>> {
    let too_large = || crate::error::Error::Server(format!("Response exceeds {limit} byte limit"));
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Resize image if needed to stay under a size limit
/// Returns (processed_bytes, mime_type)
pub fn resize_image(
    data: &[u8],
    content_type: &str,
    max_bytes: usize,
) -> Result<(Vec<u8>, String), String> {
    use image::ImageFormat;
    use std::io::Cursor;

    if max_bytes == 0 {
        return Err("Image byte limit must be positive".into());
    }

    // If already small enough, return as-is
    if data.len() <= max_bytes {
        return Ok((data.to_vec(), content_type.to_string()));
    }

    // Determine format
    let format = match content_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _ => return Err(format!("Unsupported image format: {}", content_type)),
    };

    // Load image
    let mut reader = image::ImageReader::with_format(Cursor::new(data), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| format!("Failed to load image: {}", e))?;

    // Resize to fit - scale down proportionally
    let (width, height) = (img.width(), img.height());
    let scale = (max_bytes as f64 / data.len() as f64).sqrt();
    let mut new_width = ((width as f64 * scale) as u32).max(1);
    let mut new_height = ((height as f64 * scale) as u32).max(1);
    loop {
        let resized = img
            .resize(new_width, new_height, image::imageops::FilterType::Lanczos3)
            .to_rgb8();
        let mut output = Vec::new();
        resized
            .write_to(&mut Cursor::new(&mut output), ImageFormat::Jpeg)
            .map_err(|e| format!("Failed to encode image: {e}"))?;
        if output.len() <= max_bytes {
            return Ok((output, "image/jpeg".to_string()));
        }
        if new_width == 1 && new_height == 1 {
            return Err(format!("Cannot encode an image within {max_bytes} bytes"));
        }
        new_width = (new_width / 2).max(1);
        new_height = (new_height / 2).max(1);
    }
}

/// Sanitize an attachment filename so it's safe to use as a path component.
///
/// Email senders control `attachment.name`, so an unsanitized value can contain
/// `../` segments, absolute paths, NUL bytes, or Windows-reserved names. This
/// returns only the final path component, stripped of separators and control
/// characters, with Windows-reserved stems replaced. Returns `fallback` if the
/// input is empty or entirely composed of unsafe characters.
pub fn sanitize_filename(raw: &str, fallback: &str) -> String {
    sanitize_component(raw)
        .or_else(|| sanitize_component(fallback))
        .unwrap_or_else(|| "attachment".to_owned())
}

fn sanitize_component(raw: &str) -> Option<String> {
    // Split on both forward and backslash — Windows-style names from
    // cross-platform clients show up on Unix where only `/` is a separator.
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");

    let filtered: String = base
        .chars()
        .filter(|c| !c.is_control() && !"/\\<>:\"|?*".contains(*c))
        .collect();

    let trimmed = filtered.trim_matches(|c: char| c.is_whitespace() || c == '.');

    if trimmed.is_empty() || is_windows_reserved_stem(trimmed) {
        return None;
    }

    // Leave headroom below common 255-byte filesystem limits.
    const MAX_LEN: usize = 200;
    if trimmed.len() <= MAX_LEN {
        return Some(trimmed.to_string());
    }
    Some(
        match Path::new(trimmed).extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.len() < 15 => {
                let stem_len = MAX_LEN - ext.len() - 1;
                let stem = &trimmed[..trimmed.floor_char_boundary(stem_len)];
                format!("{}.{}", stem, ext)
            }
            _ => trimmed[..trimmed.floor_char_boundary(MAX_LEN)].to_owned(),
        },
    )
}

fn is_windows_reserved_stem(name: &str) -> bool {
    let stem = name.split('.').next().map(|s| s.to_uppercase());
    matches!(
        stem.as_deref(),
        Some(
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
    )
}

/// Load a file from disk as an attachment, inferring MIME type from extension.
pub fn load_attachment(path: &str) -> anyhow::Result<AttachmentData> {
    use std::io::Read;
    let p = Path::new(path);
    let filename = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("attachment")
        .to_string();
    let content_type = mime_from_filename(&filename);
    let mut data = Vec::new();
    std::fs::File::open(p)?
        .take(MAX_ATTACHMENT_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| anyhow::anyhow!("Failed to read attachment '{}': {}", path, e))?;
    if data.len() > MAX_ATTACHMENT_BYTES {
        anyhow::bail!("Attachment exceeds the 64 MiB upload limit");
    }
    Ok(AttachmentData {
        filename,
        content_type,
        data,
    })
}

/// Resolve HTML body from either inline string or file path.
pub fn resolve_html(
    html_body: Option<String>,
    html_file: Option<String>,
) -> anyhow::Result<Option<String>> {
    if let Some(html) = html_body {
        return Ok(Some(html));
    }
    if let Some(path) = html_file {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read HTML file '{}': {}", path, e))?;
        return Ok(Some(content));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_limits_reject_invalid_or_nonpositive_values() {
        assert_eq!(parse_size("1.5MB").unwrap(), 1_572_864);
        assert_eq!(parse_size("500K").unwrap(), 512_000);
        assert_eq!(parse_size("1B").unwrap(), 1);
        for size in [
            "", "oops", "0", "-1K", "NaNM", "infG", "1e100G", "1BB", "0.1B",
        ] {
            assert!(parse_size(size).is_err(), "{size}");
        }
    }

    #[test]
    fn resized_images_fit_the_encoded_byte_limit_or_fail() {
        let img = image::RgbImage::from_fn(64, 64, |x, y| {
            image::Rgb([(x * y) as u8, (x * 31) as u8, (y * 13) as u8])
        });
        let mut data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut data),
            image::ImageFormat::Png,
        )
        .unwrap();
        let (resized, mime) = resize_image(&data, "image/png", 1000).unwrap();
        assert!(resized.len() <= 1000);
        assert_eq!(mime, "image/jpeg");
        assert!(image::load_from_memory(&resized).is_ok());
        assert!(resize_image(&data, "image/png", 1).is_err());
        assert!(resize_image(&data, "image/png", 0).is_err());
    }
    use std::io::Write;

    #[tokio::test]
    async fn bounded_responses_reject_declared_and_streamed_overflow() {
        use async_graphql::futures_util::stream;
        use axum::{Router, body::Body, routing::get};
        let router = Router::new()
            .route("/known", get(|| async { "0123456789" }))
            .route(
                "/stream",
                get(|| async {
                    Body::from_stream(stream::iter([
                        Ok::<_, std::io::Error>("01234"),
                        Ok("56789"),
                    ]))
                }),
            );
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        for path in ["known", "stream"] {
            for (limit, accepted) in [(9, false), (10, true)] {
                let response = reqwest::get(format!("http://127.0.0.1:{port}/{path}"))
                    .await
                    .unwrap();
                assert_eq!(
                    read_bounded_response(response, limit).await.is_ok(),
                    accepted,
                    "{path}: {limit}"
                );
            }
        }
        task.abort();
        let _ = task.await;
    }

    #[test]
    fn oversized_image_dimensions_are_rejected_before_allocation() {
        let mut image = Vec::new();
        image::DynamicImage::new_rgb8(16_385, 1)
            .write_to(
                &mut std::io::Cursor::new(&mut image),
                image::ImageFormat::Png,
            )
            .unwrap();
        let error = resize_image(&image, "image/png", 1).unwrap_err();
        assert!(error.contains("limit"), "{error}");
    }

    #[test]
    fn test_resolve_html_inline() {
        let result = resolve_html(Some("<h1>Hi</h1>".into()), None).unwrap();
        assert_eq!(result, Some("<h1>Hi</h1>".into()));
    }

    #[test]
    fn test_resolve_html_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "<p>from file</p>").unwrap();
        let result = resolve_html(None, Some(tmp.path().to_str().unwrap().into())).unwrap();
        assert_eq!(result, Some("<p>from file</p>".into()));
    }

    #[test]
    fn test_resolve_html_none() {
        let result = resolve_html(None, None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_html_missing_file() {
        let result = resolve_html(None, Some("/nonexistent/file.html".into()));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_attachment_success() {
        let mut tmp = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
        write!(tmp, "fake pdf").unwrap();
        let att = load_attachment(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(
            att.filename,
            tmp.path().file_name().unwrap().to_str().unwrap()
        );
        assert_eq!(att.content_type, "application/pdf");
        assert_eq!(att.data, b"fake pdf");
    }

    #[test]
    fn test_load_attachment_missing_file() {
        let result = load_attachment("/nonexistent/file.txt");
        assert!(result.is_err());
    }

    #[test]
    fn test_load_attachment_mime_inference() {
        let mut tmp = tempfile::Builder::new().suffix(".xlsx").tempfile().unwrap();
        write!(tmp, "data").unwrap();
        let att = load_attachment(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(
            att.content_type,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
    }

    #[test]
    fn test_parse_single_email() {
        let result = parse_addresses("test@example.com");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].email, "test@example.com");
        assert!(result[0].name.is_none());
    }

    #[test]
    fn test_parse_multiple_emails() {
        let result = parse_addresses("a@example.com, b@example.com");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].email, "a@example.com");
        assert_eq!(result[1].email, "b@example.com");
    }

    #[test]
    fn test_parse_email_with_name() {
        let result = parse_addresses("John Doe <john@example.com>");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].email, "john@example.com");
        assert_eq!(result[0].name, Some("John Doe".to_string()));
    }

    #[test]
    fn test_parse_mixed_formats() {
        let result = parse_addresses("plain@example.com, Named User <named@example.com>");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].email, "plain@example.com");
        assert!(result[0].name.is_none());
        assert_eq!(result[1].email, "named@example.com");
        assert_eq!(result[1].name, Some("Named User".to_string()));
    }

    #[test]
    fn test_parse_empty_string() {
        let result = parse_addresses("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_whitespace_handling() {
        let result = parse_addresses("  spaced@example.com  ,  other@example.com  ");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].email, "spaced@example.com");
        assert_eq!(result[1].email, "other@example.com");
    }

    #[test]
    fn test_parse_angle_brackets_no_name() {
        let result = parse_addresses("<bare@example.com>");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].email, "bare@example.com");
        assert!(result[0].name.is_none());
    }

    #[test]
    fn malformed_address_brackets_do_not_panic() {
        for address in [">bad<", "name> <address", "<unfinished", "<>"] {
            assert_eq!(parse_addresses(address).len(), 1);
        }
    }

    #[test]
    fn filenames_are_bounded_in_bytes_and_fallbacks_are_sanitized() {
        for name in [
            format!("{}.pdf", "\u{754c}".repeat(200)),
            "\u{e9}".repeat(300),
        ] {
            let filename = sanitize_filename(&name, "fallback");
            assert!(filename.len() <= 200);
        }
        assert_eq!(sanitize_filename("", "../../fallback"), "fallback");
        assert_eq!(sanitize_filename("", "../.."), "attachment");
        assert_eq!(sanitize_filename("CON.foo.txt", "ok"), "ok");
        assert_eq!(sanitize_filename("file:stream.txt", "ok"), "filestream.txt");
    }

    #[test]
    fn test_sanitize_filename_strips_path_traversal() {
        assert_eq!(sanitize_filename("../../etc/passwd", "fb"), "passwd");
        assert_eq!(sanitize_filename("../../../../foo.txt", "fb"), "foo.txt");
    }

    #[test]
    fn test_sanitize_filename_strips_absolute_path() {
        assert_eq!(sanitize_filename("/etc/passwd", "fb"), "passwd");
        assert_eq!(sanitize_filename("/tmp/evil.sh", "fb"), "evil.sh");
    }

    #[test]
    fn test_sanitize_filename_rejects_nul_bytes() {
        assert_eq!(sanitize_filename("foo\0bar.txt", "fb"), "foobar.txt");
    }

    #[test]
    fn test_sanitize_filename_rejects_windows_reserved() {
        assert_eq!(sanitize_filename("CON", "fb"), "fb");
        assert_eq!(sanitize_filename("nul.txt", "fb"), "fb");
        assert_eq!(sanitize_filename("com1", "fb"), "fb");
        assert_eq!(sanitize_filename("LPT9.log", "fb"), "fb");
    }

    #[test]
    fn test_sanitize_filename_trims_dots_and_whitespace() {
        assert_eq!(sanitize_filename("   .hidden.txt  ", "fb"), "hidden.txt");
        assert_eq!(sanitize_filename("file.", "fb"), "file");
        assert_eq!(sanitize_filename("...", "fb"), "fb");
    }

    #[test]
    fn test_sanitize_filename_empty_returns_fallback() {
        assert_eq!(sanitize_filename("", "fallback.bin"), "fallback.bin");
        assert_eq!(sanitize_filename("   ", "fallback.bin"), "fallback.bin");
    }

    #[test]
    fn test_sanitize_filename_preserves_normal_names() {
        assert_eq!(sanitize_filename("report.pdf", "fb"), "report.pdf");
        assert_eq!(sanitize_filename("My Photo.jpg", "fb"), "My Photo.jpg");
    }

    #[test]
    fn test_sanitize_filename_strips_backslash_path() {
        // Windows-style separators in attachment names from cross-platform clients
        assert_eq!(sanitize_filename("foo\\bar\\baz.txt", "fb"), "baz.txt");
    }

    #[test]
    fn test_sanitize_filename_truncates_long_names_with_extension() {
        let long = format!("{}.pdf", "a".repeat(300));
        let result = sanitize_filename(&long, "fb");
        assert!(result.len() <= 200);
        assert!(result.ends_with(".pdf"));
    }
}
