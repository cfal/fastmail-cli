use async_graphql::{InputObject, SimpleObject};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::jmap::{AttachmentContent, JmapClient, OutgoingAttachment};

pub(super) const MAX_COMPOSE_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const MAX_ATTACHMENTS: usize = 128;

/// An outgoing file: provide exactly one of base64 and blobId, never a local path.
#[derive(InputObject)]
pub struct AttachmentInput {
    /// Filename shown to the recipient.
    pub name: String,
    /// MIME type; inferred from the filename when omitted.
    pub content_type: Option<String>,
    /// Standard base64 bytes. HTTP JSON and query-document limits still apply.
    pub base64: Option<String>,
    /// An existing blob in this account, reused without uploading it again.
    pub blob_id: Option<String>,
}

/// Uploaded bytes that can be attached by blobId in PREVIEW and CONFIRM.
#[derive(SimpleObject)]
#[graphql(name = "UploadedAttachment")]
pub struct GqlUploadedAttachment {
    pub blob_id: String,
    pub name: String,
    pub content_type: String,
    pub size: u64,
}

pub(super) struct Attachment {
    pub data: OutgoingAttachment,
    size: usize,
    descriptor: Value,
}

fn over_limit() -> String {
    "Attachments exceed the 25 MiB total limit".into()
}

pub(super) fn name_and_type(
    name: String,
    content_type: Option<String>,
) -> Result<(String, String), String> {
    if name.chars().any(char::is_control)
        || content_type
            .as_deref()
            .is_some_and(|value| value.chars().any(char::is_control))
    {
        return Err("Attachment names and MIME types must not contain control characters".into());
    }
    let name = name.trim().to_owned();
    if name.is_empty() || name.len() > 255 {
        return Err("Attachment names must contain 1 to 255 bytes".into());
    }
    let content_type = content_type
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::util::mime_from_filename(&name));
    if content_type.len() > 255 || !content_type.is_ascii() {
        return Err("Attachment MIME types must be ASCII and at most 255 bytes".into());
    }
    let valid_type = content_type.parse::<mime::Mime>().is_ok_and(|mime| {
        mime.essence_str().eq_ignore_ascii_case(&content_type)
            && !mime.subtype().as_str().is_empty()
            && mime.type_() != mime::MULTIPART
            && !mime.essence_str().contains('*')
    });
    if !valid_type {
        return Err(
            "Attachment MIME type must be type/subtype without parameters, wildcards, or multipart"
                .into(),
        );
    }
    Ok((name, content_type))
}

pub(super) fn decode(encoded: &str, room: usize) -> Result<Vec<u8>, String> {
    let encoded_limit = room.div_ceil(3) * 4;
    let compact: Vec<_> = encoded
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .take(encoded_limit + 1)
        .collect();
    if compact.len() > encoded_limit {
        return Err(over_limit());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map_err(|_| "Attachment is not valid base64".to_owned())?;
    if bytes.len() > room {
        return Err(over_limit());
    }
    Ok(bytes)
}

/// Decode and hash client inputs before acquiring the shared JMAP client lock.
pub(super) fn prepare(inputs: Vec<AttachmentInput>) -> Result<Vec<Attachment>, String> {
    prepare_with_limit(inputs, MAX_COMPOSE_ATTACHMENT_BYTES)
}

fn prepare_with_limit(
    inputs: Vec<AttachmentInput>,
    byte_limit: usize,
) -> Result<Vec<Attachment>, String> {
    if inputs.len() > MAX_ATTACHMENTS {
        return Err("At most 128 attachments are allowed".into());
    }
    let mut total = 0;
    inputs
        .into_iter()
        .map(|input| {
            let (name, content_type) = name_and_type(input.name, input.content_type)?;
            let mut descriptor = json!({"name": name, "contentType": content_type});
            let (content, size) = match (input.base64, input.blob_id) {
                (Some(encoded), None) => {
                    let bytes = decode(&encoded, byte_limit - total)?;
                    let size = bytes.len();
                    let digest: String = Sha256::digest(&bytes)
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect();
                    descriptor["sha256"] = json!(digest);
                    descriptor["size"] = json!(size);
                    (AttachmentContent::Bytes(bytes), size)
                }
                (None, Some(id)) => {
                    if id.is_empty()
                        || id.len() > 255
                        || !id
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                    {
                        return Err("Attachment blobId must be a valid JMAP Id".into());
                    }
                    descriptor["blobId"] = json!(id);
                    (AttachmentContent::Blob(id), 0)
                }
                _ => return Err("Give exactly one of base64 and blobId for each attachment".into()),
            };
            total += size;
            Ok(Attachment {
                data: OutgoingAttachment {
                    filename: name,
                    content_type,
                    content,
                },
                size,
                descriptor,
            })
        })
        .collect()
}

pub(super) async fn measure_preview(
    client: &JmapClient,
    attachments: &mut [Attachment],
) -> Result<(), String> {
    measure(client, attachments, MAX_COMPOSE_ATTACHMENT_BYTES).await
}

/// Blob IDs are immutable within the bound account. Only PREVIEW measures them;
/// CONFIRM and DRAFT must consume the token before uploading or creating mail.
async fn measure(
    client: &JmapClient,
    attachments: &mut [Attachment],
    byte_limit: usize,
) -> Result<(), String> {
    let mut total: usize = attachments
        .iter()
        .filter_map(|attachment| {
            matches!(attachment.data.content, AttachmentContent::Bytes(_))
                .then_some(attachment.size)
        })
        .sum();
    if total > byte_limit {
        return Err(over_limit());
    }
    for attachment in attachments {
        if let AttachmentContent::Blob(id) = &attachment.data.content {
            attachment.size = client
                .blob_size(id, byte_limit - total)
                .await
                .map_err(|error| format!("Attachment '{}': {error}", attachment.data.filename))?;
            if attachment.size > byte_limit - total {
                return Err(over_limit());
            }
            total += attachment.size;
        }
    }
    Ok(())
}

pub(super) fn binding(attachments: &[Attachment]) -> Vec<Value> {
    attachments
        .iter()
        .map(|attachment| attachment.descriptor.clone())
        .collect()
}

pub(super) fn append_preview(preview: &mut String, attachments: &[Attachment]) {
    if attachments.is_empty() {
        return;
    }
    preview.push_str("\n\nAttachments:");
    for attachment in attachments {
        let source = match &attachment.data.content {
            AttachmentContent::Bytes(_) => format!(
                "sha256:{}",
                attachment.descriptor["sha256"].as_str().unwrap()
            ),
            AttachmentContent::Blob(id) => format!("blobId:{id}"),
        };
        preview.push_str(&format!(
            "\n  {} ({}, {} bytes, {source})",
            attachment.data.filename, attachment.data.content_type, attachment.size
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_mime_types_accept_suffixes_and_inferred_types() {
        for content_type in [
            "image/svg+xml",
            "application/vnd.example.file",
            "Text/Plain",
        ] {
            assert!(name_and_type("a".into(), Some(content_type.into())).is_ok());
        }
        assert_eq!(
            name_and_type("photo.png".into(), None).unwrap().1,
            "image/png"
        );
    }

    #[test]
    fn decoding_checks_exact_remaining_bytes_with_padding_and_whitespace() {
        for (encoded, expected) in [
            ("", b"".as_slice()),
            (" Y Q ==\n", b"a"),
            ("YWI=", b"ab"),
            ("YWJj", b"abc"),
        ] {
            assert_eq!(decode(encoded, expected.len()).unwrap(), expected);
            if !expected.is_empty() {
                assert!(decode(encoded, expected.len() - 1).is_err());
            }
        }
        assert!(decode("not base64!", 100).is_err());
    }

    #[test]
    fn attachment_binding_uses_full_sha256_and_structured_metadata() {
        let attachments = prepare_with_limit(
            vec![AttachmentInput {
                name: "a.txt".into(),
                content_type: None,
                base64: Some("YWJj".into()),
                blob_id: None,
            }],
            MAX_COMPOSE_ATTACHMENT_BYTES,
        )
        .unwrap();
        assert_eq!(
            binding(&attachments),
            vec![json!({
                "name":"a.txt", "contentType":"text/plain", "size":3,
                "sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            })]
        );
    }

    #[tokio::test]
    async fn aggregate_inline_and_blob_sizes_share_one_budget_in_any_order() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};
        let server = MockServer::start().await;
        Mock::given(path("/jmap/download/blob"))
            .respond_with(ResponseTemplate::new(200).set_body_string("1234"))
            .mount(&server)
            .await;
        let client = JmapClient::with_test_session(&format!("{}/jmap", server.uri()));
        let inline = || AttachmentInput {
            name: "a.txt".into(),
            content_type: None,
            base64: Some("YWJj".into()),
            blob_id: None,
        };
        let blob = || AttachmentInput {
            name: "b.txt".into(),
            content_type: None,
            base64: None,
            blob_id: Some("blob".into()),
        };
        assert!(prepare_with_limit(vec![inline(), inline()], 5).is_err());
        for inputs in [vec![inline(), blob()], vec![blob(), inline()]] {
            let mut attachments = prepare_with_limit(inputs, 7).unwrap();
            assert!(measure(&client, &mut attachments, 6).await.is_err());
            measure(&client, &mut attachments, 7).await.unwrap();
            assert_eq!(
                attachments
                    .iter()
                    .map(|attachment| attachment.size)
                    .sum::<usize>(),
                7
            );
        }
    }
}
