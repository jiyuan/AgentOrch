//! Shared helpers for serializing `Message` values into provider request bodies.
//!
//! Each provider builds its own multimodal payload, but they all need the same
//! primitives: figure out an attachment's mime type, read the bytes as base64
//! within a size cap, and produce a human-readable fallback descriptor when
//! the model can't actually consume the file.

use agentos_proto::{Attachment, AttachmentKind};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use std::fs;
use std::path::Path;

/// Cap on the raw file size we'll embed in a request. Above this we skip the
/// inline payload and emit a text descriptor instead. Picked to stay under
/// every provider's per-image and per-message ceiling.
pub(crate) const MAX_INLINE_BYTES: u64 = 20 * 1024 * 1024;

/// Mime types Anthropic and OpenAI both accept as image content blocks.
pub(crate) const SUPPORTED_IMAGE_MIMES: &[&str] =
    &["image/jpeg", "image/png", "image/gif", "image/webp"];

pub(crate) fn image_mime(attachment: &Attachment) -> Option<&'static str> {
    if let Some(mime) = attachment.mime.as_deref() {
        if let Some(canonical) = canonical_image_mime(mime) {
            return Some(canonical);
        }
    }
    image_mime_from_extension(&attachment.path)
}

fn canonical_image_mime(mime: &str) -> Option<&'static str> {
    let lower = mime.to_ascii_lowercase();
    SUPPORTED_IMAGE_MIMES
        .iter()
        .copied()
        .find(|candidate| *candidate == lower.as_str())
}

fn image_mime_from_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_ascii_lowercase();
    match ext.to_str()? {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

pub(crate) fn document_mime(attachment: &Attachment) -> Option<&'static str> {
    if let Some(mime) = attachment.mime.as_deref() {
        if mime.eq_ignore_ascii_case("application/pdf") {
            return Some("application/pdf");
        }
    }
    let ext = attachment.path.extension()?.to_ascii_lowercase();
    if ext.to_str()? == "pdf" {
        Some("application/pdf")
    } else {
        None
    }
}

/// Read a file from disk and base64-encode it. Returns `Err` if the file
/// is missing, unreadable, or exceeds [`MAX_INLINE_BYTES`].
pub(crate) fn read_base64(path: &Path) -> Result<String, String> {
    let metadata = fs::metadata(path)
        .map_err(|err| format!("attachment {} stat failed: {err}", path.display()))?;
    if metadata.len() > MAX_INLINE_BYTES {
        return Err(format!(
            "attachment {} is {} bytes (cap {})",
            path.display(),
            metadata.len(),
            MAX_INLINE_BYTES
        ));
    }
    let bytes = fs::read(path)
        .map_err(|err| format!("attachment {} read failed: {err}", path.display()))?;
    Ok(BASE64_STANDARD.encode(bytes))
}

/// Human-readable descriptor for cases where we can't (or won't) inline the
/// file content. Keeps the model aware that an attachment exists, with enough
/// metadata for it to call a filesystem tool if one is available.
pub(crate) fn descriptor(attachment: &Attachment) -> String {
    let kind = match attachment.kind {
        AttachmentKind::Image => "image",
        AttachmentKind::Document => "document",
    };
    let mut parts = vec![format!("name={}", attachment.name)];
    if let Some(mime) = attachment.mime.as_deref() {
        parts.push(format!("mime={mime}"));
    }
    if let Some(size) = attachment.size {
        parts.push(format!("size={size}"));
    }
    parts.push(format!("path={}", attachment.path.display()));
    format!("[attached {kind}: {}]", parts.join(", "))
}

/// Append descriptor lines for every attachment to a base text body. Used by
/// providers without multimodal support, or for attachments a multimodal
/// provider can't accept (oversize, wrong mime, etc).
pub(crate) fn append_descriptors(text: &str, attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return text.to_owned();
    }
    let mut buf = String::with_capacity(text.len() + attachments.len() * 64);
    buf.push_str(text);
    for attachment in attachments {
        if !buf.is_empty() && !buf.ends_with('\n') {
            buf.push('\n');
        }
        buf.push_str(&descriptor(attachment));
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_proto::AttachmentKind;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn attachment(name: &str, mime: Option<&str>, kind: AttachmentKind) -> Attachment {
        Attachment {
            kind,
            name: Arc::from(name),
            path: PathBuf::from(format!("/tmp/{name}")),
            mime: mime.map(Arc::from),
            size: Some(1234),
            source: None,
        }
    }

    #[test]
    fn image_mime_uses_explicit_mime_first() {
        let att = attachment("foo.bin", Some("image/png"), AttachmentKind::Image);
        assert_eq!(image_mime(&att), Some("image/png"));
    }

    #[test]
    fn image_mime_falls_back_to_extension() {
        let att = attachment("foo.WEBP", None, AttachmentKind::Image);
        assert_eq!(image_mime(&att), Some("image/webp"));
    }

    #[test]
    fn image_mime_rejects_unsupported() {
        let att = attachment("foo.bmp", None, AttachmentKind::Image);
        assert_eq!(image_mime(&att), None);
    }

    #[test]
    fn document_mime_recognises_pdf() {
        let att = attachment("report.pdf", None, AttachmentKind::Document);
        assert_eq!(document_mime(&att), Some("application/pdf"));
        let att2 = attachment("a.bin", Some("application/pdf"), AttachmentKind::Document);
        assert_eq!(document_mime(&att2), Some("application/pdf"));
        let att3 = attachment("a.txt", Some("text/plain"), AttachmentKind::Document);
        assert_eq!(document_mime(&att3), None);
    }

    #[test]
    fn descriptor_includes_metadata() {
        let att = attachment("photo.jpg", Some("image/jpeg"), AttachmentKind::Image);
        let line = descriptor(&att);
        assert!(line.starts_with("[attached image:"));
        assert!(line.contains("name=photo.jpg"));
        assert!(line.contains("mime=image/jpeg"));
        assert!(line.contains("size=1234"));
        assert!(line.contains("path=/tmp/photo.jpg"));
    }

    #[test]
    fn append_descriptors_joins_with_newline() {
        let att = attachment("a.png", Some("image/png"), AttachmentKind::Image);
        let out = append_descriptors("hello", std::slice::from_ref(&att));
        assert!(out.starts_with("hello\n[attached image:"));
        let out2 = append_descriptors("", std::slice::from_ref(&att));
        assert!(out2.starts_with("[attached image:"));
    }
}
