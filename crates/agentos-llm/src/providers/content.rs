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

/// Max bytes we'll inline as text when extracting a non-PDF document body.
/// Caps context blow-up from a user dropping a 50 MB log file.
pub(crate) const MAX_INLINE_TEXT_BYTES: u64 = 256 * 1024;

/// Read a text-like document into a fenced code block the model can consume.
/// Returns `None` when the attachment isn't text-like, or `Some(Err(_))` when
/// it looks text-like but can't be read (oversize, missing, decode failed).
///
/// We bias toward including the body even if UTF-8 decoding is lossy — losing
/// a few unprintable bytes is better than the model getting only a descriptor.
pub(crate) fn read_text_document(attachment: &Attachment) -> Option<Result<String, String>> {
    if !is_text_like(attachment) {
        return None;
    }
    let path = attachment.path.as_path();
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(err) => {
            return Some(Err(format!(
                "text attachment {} stat failed: {err}",
                path.display()
            )));
        }
    };
    if metadata.len() > MAX_INLINE_TEXT_BYTES {
        return Some(Err(format!(
            "text attachment {} is {} bytes (text cap {})",
            path.display(),
            metadata.len(),
            MAX_INLINE_TEXT_BYTES
        )));
    }
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(err) => {
            return Some(Err(format!(
                "text attachment {} read failed: {err}",
                path.display()
            )));
        }
    };
    Some(Ok(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Format a text document body as a labelled fenced code block. The label
/// includes the attachment name so the model knows which file it's reading.
pub(crate) fn format_text_document(name: &str, body: &str) -> String {
    let fence = pick_fence(body);
    format!("File: {name}\n{fence}\n{body}\n{fence}")
}

fn pick_fence(body: &str) -> String {
    let mut len = 3;
    let mut needle = "`".repeat(len);
    while body.contains(&needle) {
        len += 1;
        needle = "`".repeat(len);
    }
    needle
}

fn is_text_like(attachment: &Attachment) -> bool {
    if let Some(mime) = attachment.mime.as_deref() {
        let lower = mime.to_ascii_lowercase();
        if lower.starts_with("text/") {
            return true;
        }
        if matches!(
            lower.as_str(),
            "application/json"
                | "application/jsonl"
                | "application/ld+json"
                | "application/xml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
                | "application/x-sh"
                | "application/javascript"
                | "application/typescript"
                | "application/sql"
        ) {
            return true;
        }
    }
    matches!(
        attachment
            .path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some(
            "txt"
                | "md"
                | "markdown"
                | "csv"
                | "tsv"
                | "json"
                | "jsonl"
                | "ndjson"
                | "xml"
                | "html"
                | "htm"
                | "log"
                | "yaml"
                | "yml"
                | "toml"
                | "ini"
                | "cfg"
                | "conf"
                | "env"
                | "sh"
                | "bash"
                | "zsh"
                | "fish"
                | "py"
                | "rs"
                | "go"
                | "java"
                | "c"
                | "cc"
                | "cpp"
                | "cxx"
                | "h"
                | "hpp"
                | "rb"
                | "php"
                | "pl"
                | "sql"
                | "r"
                | "scala"
                | "swift"
                | "kt"
                | "lua"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "vue"
                | "svelte"
                | "tex"
                | "diff"
                | "patch"
        )
    )
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

    fn write_tmp(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("content-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn read_text_document_reads_text_extensions() {
        let path = write_tmp("notes.md", b"# hi\nbody");
        let att = Attachment {
            kind: AttachmentKind::Document,
            name: Arc::from("notes.md"),
            path,
            mime: None,
            size: Some(9),
            source: None,
        };
        let body = read_text_document(&att)
            .expect("should attempt")
            .expect("ok");
        assert!(body.contains("# hi"));
    }

    #[test]
    fn read_text_document_skips_non_text() {
        let att = attachment(
            "a.bin",
            Some("application/octet-stream"),
            AttachmentKind::Document,
        );
        assert!(read_text_document(&att).is_none());
    }

    #[test]
    fn read_text_document_caps_size() {
        let big = vec![b'x'; (MAX_INLINE_TEXT_BYTES + 1) as usize];
        let path = write_tmp("huge.log", &big);
        let att = Attachment {
            kind: AttachmentKind::Document,
            name: Arc::from("huge.log"),
            path,
            mime: None,
            size: None,
            source: None,
        };
        let result = read_text_document(&att).expect("should attempt");
        assert!(result.is_err());
    }

    #[test]
    fn format_text_document_escapes_collisions() {
        let body = "let s = \"```rust\";";
        let formatted = format_text_document("a.rs", body);
        // The fence has to be longer than any backtick run inside the body.
        assert!(formatted.starts_with("File: a.rs\n````\n"));
        assert!(formatted.ends_with("\n````"));
    }
}
