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

    let attachments = email.attachments.as_ref();
    if attachments.is_none() || attachments.unwrap().is_empty() {
        Output::<()>::error("No attachments found").print();
        return Ok(());
    }

    // JSON format - extract text and return structured data
    if format == Some("json") {
        let mut results: Vec<AttachmentContent> = Vec::new();

        for attachment in attachments.unwrap() {
            let blob_id = match &attachment.blob_id {
                Some(id) => id,
                None => continue,
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

    for attachment in attachments.unwrap() {
        let blob_id = match &attachment.blob_id {
            Some(id) => id,
            None => continue,
        };

        let fallback = format!("{}.bin", blob_id);
        let raw_name = attachment.name.as_deref().unwrap_or("");
        let filename = sanitize_filename(raw_name, &fallback);

        let content_type = attachment
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream");

        let bytes = client.download_blob(blob_id).await?;

        // Resize images if --max-size specified
        let (final_bytes, final_filename) = if let Some(max) = max_bytes {
            let mime = if is_image(content_type, &filename) {
                infer_image_mime(&filename).unwrap_or(content_type)
            } else {
                content_type
            };

            if is_image(mime, &filename) {
                match resize_image(&bytes, mime, max) {
                    Ok((resized, new_mime)) => {
                        // Update extension if format changed (e.g., PNG -> JPEG)
                        let new_filename = if new_mime == "image/jpeg"
                            && !filename.to_lowercase().ends_with(".jpg")
                            && !filename.to_lowercase().ends_with(".jpeg")
                        {
                            let stem = Path::new(&filename)
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or(&filename);
                            format!("{}.jpg", stem)
                        } else {
                            filename.clone()
                        };
                        (resized, new_filename)
                    }
                    Err(message) => {
                        return Err(anyhow::anyhow!("Cannot resize {filename}: {message}"));
                    }
                }
            } else {
                (bytes, filename.clone())
            }
        } else {
            (bytes, filename.clone())
        };

        let (mut file, path) = create_attachment_file(Path::new(out_dir), &final_filename)?;
        file.write_all(&final_bytes)?;

        downloaded.push(path.to_string_lossy().to_string());
    }

    #[derive(serde::Serialize)]
    struct DownloadResponse {
        files: Vec<String>,
    }

    Output::success(DownloadResponse { files: downloaded }).print();

    Ok(())
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
