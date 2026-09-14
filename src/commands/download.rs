use crate::jmap::authenticated_client;
use crate::models::Output;
use crate::util::{
    extract_text, infer_image_mime, is_image, parse_size_checked, resize_image, sanitize_filename,
};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

pub async fn download_attachment(
    email_id: &str,
    output_dir: Option<&str>,
    format: Option<&str>,
    max_size: Option<&str>,
) -> anyhow::Result<()> {
    let max_bytes = max_size
        .map(parse_size_checked)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let client = authenticated_client().await?;

    let email = client.get_email(email_id).await?;

    let attachments = email.attachments.as_deref().unwrap_or_default();
    if attachments.is_empty() {
        Output::<()>::error("No attachments found").print();
        return Ok(());
    }

    // JSON format - extract text and return structured data
    if format == Some("json") {
        let mut results: Vec<AttachmentContent> = Vec::new();

        for attachment in attachments {
            let Some(blob_id) = &attachment.blob_id else {
                continue;
            };

            let fallback = format!("{}.bin", blob_id);
            let raw_name = attachment.name.as_deref().unwrap_or("");
            let filename = sanitize_filename(raw_name, &fallback);

            let content_type = attachment.content_type.clone().unwrap_or_default();
            let bytes = client.download_blob(blob_id).await?;

            let text = extract_text(&bytes, &filename).await?;

            results.push(AttachmentContent {
                filename,
                content_type,
                size: bytes.len(),
                text,
            });
        }

        Output::success(results).print();
        return Ok(());
    }

    // Default: download to files
    let out_dir = output_dir.unwrap_or(".");
    let mut downloaded: Vec<String> = Vec::new();
    let mut skipped = Vec::new();

    for attachment in attachments {
        let Some(blob_id) = &attachment.blob_id else {
            continue;
        };

        let fallback = format!("{}.bin", blob_id);
        let raw_name = attachment.name.as_deref().unwrap_or("");
        let filename = sanitize_filename(raw_name, &fallback);

        let content_type = attachment
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream");

        let bytes = client.download_blob(blob_id).await?;

        let (final_bytes, final_filename) =
            match prepare_download(bytes, &filename, content_type, max_bytes) {
                Ok(prepared) => prepared,
                Err(message) => {
                    eprintln!("Skipping {filename}: {message}");
                    skipped.push(DownloadFailure {
                        filename,
                        error: message,
                    });
                    continue;
                }
            };

        let (mut file, path) = create_attachment_file(Path::new(out_dir), &final_filename)?;
        file.write_all(&final_bytes)?;

        downloaded.push(path.to_string_lossy().to_string());
    }

    #[derive(serde::Serialize)]
    struct DownloadResponse {
        files: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        skipped: Vec<DownloadFailure>,
    }

    let incomplete = !skipped.is_empty();
    let mut output = Output::success(DownloadResponse {
        files: downloaded,
        skipped,
    });
    if incomplete {
        output.success = false;
        output.error = Some("Some images could not be resized; see data.skipped".into());
    }
    output.print();

    Ok(())
}

#[derive(serde::Serialize)]
struct DownloadFailure {
    filename: String,
    error: String,
}

fn prepare_download(
    bytes: Vec<u8>,
    filename: &str,
    content_type: &str,
    max_bytes: Option<usize>,
) -> Result<(Vec<u8>, String), String> {
    let Some(max_bytes) = max_bytes else {
        return Ok((bytes, filename.to_owned()));
    };
    if !is_image(content_type, filename) {
        return Ok((bytes, filename.to_owned()));
    }

    let mime = infer_image_mime(filename).unwrap_or(content_type);
    let (bytes, new_mime) = resize_image(&bytes, mime, max_bytes)?;
    let lowercase_name = filename.to_lowercase();
    let filename = if new_mime == "image/jpeg"
        && !lowercase_name.ends_with(".jpg")
        && !lowercase_name.ends_with(".jpeg")
    {
        let stem = Path::new(filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(filename);
        format!("{stem}.jpg")
    } else {
        filename.to_owned()
    };
    Ok((bytes, filename))
}

fn create_attachment_file(dir: &Path, filename: &str) -> std::io::Result<(std::fs::File, PathBuf)> {
    let name = Path::new(filename);
    let stem = name
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    let extension = name
        .extension()
        .and_then(|s| s.to_str())
        .map(|ext| format!(".{ext}"))
        .unwrap_or_default();
    for number in 1..=1000 {
        let path = dir.join(if number == 1 {
            filename.to_owned()
        } else {
            format!("{stem}-{number}{extension}")
        });
        // Keep exclusive creation on every attempt, including existing symlinks.
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "No unused attachment filename after 1000 attempts",
    ))
}

#[derive(serde::Serialize)]
struct AttachmentContent {
    filename: String,
    content_type: String,
    size: usize,
    text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_preparation_preserves_passthrough_bytes_and_filename_rules() {
        for (name, content_type, limit, expected_name) in [
            ("large.png", "image/png", None, "large.png"),
            ("notes.txt", "text/plain", Some(1), "notes.txt"),
            ("small.tiff", "image/tiff", Some(10), "small.tiff"),
            ("small.JPEG", "image/jpeg", Some(10), "small.JPEG"),
            ("small.bin", "image/jpeg", Some(10), "small.jpg"),
            ("small.png", "image/jpeg", Some(10), "small.png"),
        ] {
            let (bytes, filename) =
                prepare_download(b"data".to_vec(), name, content_type, limit).unwrap();
            assert_eq!(bytes, b"data");
            assert_eq!(filename, expected_name);
        }
    }

    #[test]
    fn colliding_filenames_get_distinct_files_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        for (raw, expected) in [
            ("first/signature.png", "signature.png"),
            ("second/signature.png", "signature-2.png"),
            ("signature.png", "signature-3.png"),
        ] {
            let name = sanitize_filename(raw, "attachment");
            let (mut file, path) = create_attachment_file(dir.path(), &name).unwrap();
            file.write_all(raw.as_bytes()).unwrap();
            assert_eq!(path.file_name().unwrap(), expected);
        }
        assert_eq!(
            std::fs::read(dir.path().join("signature.png")).unwrap(),
            b"first/signature.png"
        );
    }

    #[cfg(unix)]
    #[test]
    fn collisions_do_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("file.txt")).unwrap();
        let (mut file, path) = create_attachment_file(dir.path(), "file.txt").unwrap();
        file.write_all(b"attachment").unwrap();
        assert_eq!(path.file_name().unwrap(), "file-2.txt");
        assert_eq!(std::fs::read(target).unwrap(), b"unchanged");
    }
}
