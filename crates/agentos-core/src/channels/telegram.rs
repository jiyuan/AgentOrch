use crate::channels::attachments::{file_size, AttachmentStore};
use crate::channels::text::split_text;
use agentos_interfaces::{Channel, ChannelError};
use agentos_proto::{
    Attachment, AttachmentKind, ChannelId, ConversationId, Envelope, Message, MessageRole,
};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// Telegram Bot API origin. Overridable via `AGENTOS_TELEGRAM_API_BASE` so the
/// channel can be pointed at a local mock during tests.
const DEFAULT_API_BASE: &str = "https://api.telegram.org";

pub struct TelegramChannel {
    token: Arc<str>,
    id: ChannelId,
    allowed_chat_id: Option<Arc<str>>,
    offset: Option<i64>,
    log_receive_errors: bool,
    attachments: AttachmentStore,
    api_base: Arc<str>,
    file_base: Arc<str>,
}

impl TelegramChannel {
    pub fn from_env() -> Result<Self, ChannelError> {
        let token = env::var("AGENTOS_TELEGRAM_BOT_TOKEN")
            .map_err(|_| ChannelError::Backend(Arc::from("missing AGENTOS_TELEGRAM_BOT_TOKEN")))?;
        // An empty value means "no allowlist" (accept any chat), same as the
        // variable being unset. Without this, an empty override would make
        // `parse_update` reject every inbound message.
        let allowed_chat_id = env::var("AGENTOS_TELEGRAM_CHAT_ID")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(Arc::from);
        let api_base = env::var("AGENTOS_TELEGRAM_API_BASE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_owned());
        let file_base = env::var("AGENTOS_TELEGRAM_FILE_BASE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| api_base.clone());
        Ok(Self {
            token: Arc::from(token),
            id: ChannelId::new("telegram"),
            allowed_chat_id,
            offset: None,
            log_receive_errors: false,
            attachments: AttachmentStore::from_env("telegram"),
            api_base: Arc::from(api_base.trim_end_matches('/').to_owned()),
            file_base: Arc::from(file_base.trim_end_matches('/').to_owned()),
        })
    }

    pub fn with_receive_error_logging(mut self, enabled: bool) -> Self {
        self.log_receive_errors = enabled;
        self
    }

    pub fn with_attachments_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.attachments = AttachmentStore::new(root, "telegram");
        self
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.api_base, self.token)
    }

    fn file_url(&self, file_path: &str) -> String {
        format!("{}/file/bot{}/{file_path}", self.file_base, self.token)
    }

    /// Long-poll `getUpdates`. `Ok(None)` means the long poll elapsed with no
    /// new updates (or curl hit its own deadline waiting on an idle socket) —
    /// that is the steady state, not a failure, so the caller must not log it.
    fn fetch_updates(&self) -> Result<Option<Value>, ChannelError> {
        let mut command = Command::new("curl");
        // The server long-polls for `LONG_POLL_SECS`; curl's own `--max-time`
        // is kept comfortably above it so a slow TLS handshake on top of a
        // full-length poll never trips curl mid-poll and looks like an error.
        command.args([
            "--silent",
            "--show-error",
            "--connect-timeout",
            "10",
            "--max-time",
            CURL_MAX_TIME_SECS,
            "-X",
            "POST",
        ]);
        command.arg(self.api_url("getUpdates"));
        command.args(["-d", concat!("timeout=", "25")]);
        if let Some(offset) = self.offset {
            command.args(["-d", &format!("offset={offset}")]);
        }

        let output = command
            .output()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if !output.status.success() {
            // curl exit 28 == operation timed out. An idle long poll legitimately
            // ends this way; treat it as "no updates" so the receive loop just
            // polls again instead of logging a backend failure every cycle.
            if output.status.code() == Some(28) {
                return Ok(None);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ChannelError::Backend(Arc::from(stderr.trim().to_owned())));
        }
        serde_json::from_slice(&output.stdout)
            .map(Some)
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))
    }

    fn get_file_path(&self, file_id: &str) -> Result<String, ChannelError> {
        let body = format!("file_id={file_id}");
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", "10", "-X", "POST"])
            .arg(self.api_url("getFile"))
            .args(["--data-urlencode", &body])
            .output()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ChannelError::Backend(Arc::from(stderr.trim().to_owned())));
        }
        let response: Value = serde_json::from_slice(&output.stdout)
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(ChannelError::Backend(Arc::from(response.to_string())));
        }
        response
            .get("result")
            .and_then(|r| r.get("file_path"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                ChannelError::Backend(Arc::from("Telegram getFile response missing file_path"))
            })
    }

    fn download_to(&self, file_id: &str, target: &Path) -> Result<(), ChannelError> {
        let file_path = self.get_file_path(file_id)?;
        let url = self.file_url(&file_path);
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--fail", "--max-time", "60"])
            .arg("-o")
            .arg(target)
            .arg(url)
            .output()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ChannelError::Backend(Arc::from(format!(
                "Telegram file download failed: {}",
                stderr.trim()
            ))));
        }
        Ok(())
    }

    fn download_attachments(
        &self,
        descriptors: &[AttachmentDescriptor],
        conversation: &str,
        message_id: &str,
    ) -> Result<Vec<Attachment>, ChannelError> {
        let mut out = Vec::with_capacity(descriptors.len());
        for desc in descriptors {
            let path = self
                .attachments
                .target_path(conversation, message_id, &desc.name)?;
            self.download_to(&desc.file_id, &path)?;
            let size = desc.size.or_else(|| file_size(&path));
            out.push(Attachment {
                kind: desc.kind.clone(),
                name: Arc::from(desc.name.as_str()),
                path,
                mime: desc.mime.clone(),
                size,
                source: Some(Arc::from(desc.file_id.as_str())),
            });
        }
        Ok(out)
    }

    fn send_text(&self, chat_id: &str, text: &str) -> Result<(), ChannelError> {
        for chunk in split_text(text, TELEGRAM_TEXT_LIMIT) {
            self.send_text_chunk(chat_id, &chunk)?;
        }
        Ok(())
    }

    fn send_text_chunk(&self, chat_id: &str, text: &str) -> Result<(), ChannelError> {
        let text_arg = format!("text={text}");
        let chat_arg = format!("chat_id={chat_id}");
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", "10", "-X", "POST"])
            .arg(self.api_url("sendMessage"))
            .args(["-d", &chat_arg, "--data-urlencode", &text_arg])
            .output()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        check_send_response(&output.status, &output.stdout, &output.stderr)
    }

    fn send_attachment(
        &self,
        chat_id: &str,
        attachment: &Attachment,
        caption: Option<&str>,
    ) -> Result<(), ChannelError> {
        let (method, field) = match attachment.kind {
            AttachmentKind::Image => ("sendPhoto", "photo"),
            AttachmentKind::Document => ("sendDocument", "document"),
        };
        let file_form = format!("{field}=@{}", attachment.path.display());
        let chat_form = format!("chat_id={chat_id}");
        let mut command = Command::new("curl");
        command
            .args(["--silent", "--show-error", "--max-time", "60", "-X", "POST"])
            .arg(self.api_url(method))
            .args(["-F", &chat_form, "-F", &file_form]);
        if let Some(caption) = caption {
            if !caption.is_empty() {
                let caption_form = format!("caption={caption}");
                command.args(["-F", &caption_form]);
            }
        }
        let output = command
            .output()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        check_send_response(&output.status, &output.stdout, &output.stderr)
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn id(&self) -> ChannelId {
        self.id.clone()
    }

    async fn receive(&mut self) -> Option<Envelope> {
        let response = match self.fetch_updates() {
            Ok(Some(response)) => response,
            // Idle long poll: no updates this cycle. Not an error — poll again.
            Ok(None) => return None,
            Err(err) => {
                if self.log_receive_errors {
                    eprintln!("telegram getUpdates failed: {err}");
                }
                return None;
            }
        };
        let updates = response.get("result")?.as_array()?;
        for update in updates {
            let update_id = update.get("update_id")?.as_i64()?;
            let Some(parsed) = parse_update(update, &self.id, self.allowed_chat_id.as_deref())
            else {
                continue;
            };
            let attachments = match self.download_attachments(
                &parsed.attachments,
                parsed.envelope.conversation_id.as_str(),
                &parsed.message_id_str,
            ) {
                Ok(a) => a,
                Err(err) => {
                    if self.log_receive_errors {
                        eprintln!("telegram attachment download failed: {err}");
                    }
                    continue;
                }
            };
            if parsed.envelope.message.content.is_empty() && attachments.is_empty() {
                continue;
            }
            self.offset = Some(update_id + 1);
            let mut envelope = parsed.envelope;
            envelope.message.attachments = attachments;
            return Some(envelope);
        }
        None
    }

    async fn send(&self, env: Envelope) -> Result<(), ChannelError> {
        let chat_id = env.conversation_id.as_str();
        if env.message.attachments.is_empty() {
            return self.send_text(chat_id, &env.message.content);
        }

        // Telegram captions are capped at 1024 chars. If the reply text is
        // longer, send it as a separate message first and don't attach a
        // caption — otherwise the multipart sendPhoto/sendDocument would 400.
        let text = env.message.content.as_ref();
        let caption = if text.is_empty() {
            None
        } else if text.chars().count() <= TELEGRAM_CAPTION_LIMIT {
            Some(text)
        } else {
            self.send_text(chat_id, text)?;
            None
        };
        let mut caption = caption;
        for attachment in &env.message.attachments {
            self.send_attachment(chat_id, attachment, caption)?;
            caption = None;
        }
        Ok(())
    }
}

/// curl `--max-time` for the `getUpdates` long poll. Must stay well above the
/// server-side `timeout=25` so a full-length poll plus connect/TLS setup never
/// trips curl's own deadline and surfaces as a spurious backend error.
const CURL_MAX_TIME_SECS: &str = "40";

/// Telegram sendMessage hard limit: 4096 characters per message body.
const TELEGRAM_TEXT_LIMIT: usize = 4096;

/// Telegram sendPhoto/sendDocument caption hard limit: 1024 characters.
const TELEGRAM_CAPTION_LIMIT: usize = 1024;

fn check_send_response(
    status: &std::process::ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<(), ChannelError> {
    if !status.success() {
        let stderr = String::from_utf8_lossy(stderr);
        return Err(ChannelError::Backend(Arc::from(stderr.trim().to_owned())));
    }
    let response: Value = serde_json::from_slice(stdout)
        .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err(ChannelError::Backend(Arc::from(response.to_string())))
    }
}

#[derive(Debug)]
struct AttachmentDescriptor {
    kind: AttachmentKind,
    file_id: String,
    name: String,
    mime: Option<Arc<str>>,
    size: Option<u64>,
}

struct ParsedUpdate {
    envelope: Envelope,
    attachments: Vec<AttachmentDescriptor>,
    message_id_str: String,
}

fn parse_update(
    update: &Value,
    channel_id: &ChannelId,
    allowed_chat_id: Option<&str>,
) -> Option<ParsedUpdate> {
    let message = update.get("message")?;
    let chat_id = chat_id_string(message.get("chat")?)?;
    if allowed_chat_id.is_some_and(|allowed| allowed != chat_id) {
        return None;
    }

    let attachments = collect_attachment_descriptors(message);
    let text = message
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| message.get("caption").and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_owned();
    if text.is_empty() && attachments.is_empty() {
        return None;
    }

    let sender = message
        .get("from")
        .and_then(|from| {
            from.get("id")
                .and_then(Value::as_i64)
                .map(|id| id.to_string())
                .or_else(|| {
                    from.get("username")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
        })
        .map_or_else(|| Arc::from("telegram-user"), Arc::from);
    let update_id = update.get("update_id")?.as_i64()?;
    let message_id = message.get("message_id").and_then(Value::as_i64);
    let message_id_str = message_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| format!("u{update_id}"));

    let mut metadata = BTreeMap::new();
    metadata.insert(Arc::from("kind"), Value::String("telegram".to_owned()));
    metadata.insert(Arc::from("update_id"), Value::from(update_id));
    if let Some(message_id) = message_id {
        metadata.insert(Arc::from("message_id"), Value::from(message_id));
    }

    Some(ParsedUpdate {
        envelope: Envelope {
            channel_id: channel_id.clone(),
            conversation_id: ConversationId::new(chat_id),
            sender,
            message: Message::text(MessageRole::User, text),
            metadata,
        },
        attachments,
        message_id_str,
    })
}

fn collect_attachment_descriptors(message: &Value) -> Vec<AttachmentDescriptor> {
    let mut out = Vec::new();
    if let Some(photos) = message.get("photo").and_then(Value::as_array) {
        if let Some(largest) = largest_photo(photos) {
            let file_id = largest
                .get("file_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if !file_id.is_empty() {
                let size = largest.get("file_size").and_then(Value::as_u64);
                out.push(AttachmentDescriptor {
                    kind: AttachmentKind::Image,
                    name: photo_name(largest),
                    file_id,
                    mime: Some(Arc::from("image/jpeg")),
                    size,
                });
            }
        }
    }
    if let Some(document) = message.get("document") {
        let file_id = document
            .get("file_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if !file_id.is_empty() {
            let name = document
                .get("file_name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("{file_id}.bin"));
            let mime = document
                .get("mime_type")
                .and_then(Value::as_str)
                .map(Arc::from);
            let size = document.get("file_size").and_then(Value::as_u64);
            out.push(AttachmentDescriptor {
                kind: AttachmentKind::Document,
                file_id,
                name,
                mime,
                size,
            });
        }
    }
    out
}

fn largest_photo(photos: &[Value]) -> Option<&Value> {
    photos.iter().max_by_key(|p| {
        p.get("file_size")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                let w = p.get("width").and_then(Value::as_u64).unwrap_or(0);
                let h = p.get("height").and_then(Value::as_u64).unwrap_or(0);
                w.saturating_mul(h)
            })
    })
}

fn photo_name(photo: &Value) -> String {
    photo
        .get("file_unique_id")
        .and_then(Value::as_str)
        .map(|id| format!("{id}.jpg"))
        .unwrap_or_else(|| "photo.jpg".to_owned())
}

fn chat_id_string(chat: &Value) -> Option<String> {
    if let Some(id) = chat.get("id").and_then(Value::as_i64) {
        return Some(id.to_string());
    }
    chat.get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel_id() -> ChannelId {
        ChannelId::new("telegram")
    }

    #[test]
    fn parse_update_extracts_text_only() {
        let update = json!({
            "update_id": 1,
            "message": {
                "message_id": 10,
                "chat": { "id": 99 },
                "from": { "id": 7 },
                "text": "hello world"
            }
        });
        let parsed = parse_update(&update, &channel_id(), None).expect("envelope");
        assert_eq!(parsed.envelope.message.content.as_ref(), "hello world");
        assert!(parsed.attachments.is_empty());
        assert_eq!(parsed.message_id_str, "10");
    }

    #[test]
    fn parse_update_picks_largest_photo_and_caption() {
        let update = json!({
            "update_id": 2,
            "message": {
                "message_id": 11,
                "chat": { "id": 99 },
                "caption": "look at this",
                "photo": [
                    { "file_id": "small", "file_unique_id": "u1", "width": 90, "height": 60, "file_size": 1000 },
                    { "file_id": "big",   "file_unique_id": "u2", "width": 800, "height": 600, "file_size": 50_000 },
                ]
            }
        });
        let parsed = parse_update(&update, &channel_id(), None).expect("envelope");
        assert_eq!(parsed.envelope.message.content.as_ref(), "look at this");
        assert_eq!(parsed.attachments.len(), 1);
        let desc = &parsed.attachments[0];
        assert_eq!(desc.kind, AttachmentKind::Image);
        assert_eq!(desc.file_id, "big");
        assert_eq!(desc.name, "u2.jpg");
        assert_eq!(desc.size, Some(50_000));
    }

    #[test]
    fn parse_update_extracts_document() {
        let update = json!({
            "update_id": 3,
            "message": {
                "message_id": 12,
                "chat": { "id": 99 },
                "document": {
                    "file_id": "doc-1",
                    "file_name": "report.pdf",
                    "mime_type": "application/pdf",
                    "file_size": 4096
                }
            }
        });
        let parsed = parse_update(&update, &channel_id(), None).expect("envelope");
        assert!(parsed.envelope.message.content.is_empty());
        assert_eq!(parsed.attachments.len(), 1);
        let desc = &parsed.attachments[0];
        assert_eq!(desc.kind, AttachmentKind::Document);
        assert_eq!(desc.file_id, "doc-1");
        assert_eq!(desc.name, "report.pdf");
        assert_eq!(desc.mime.as_deref(), Some("application/pdf"));
    }

    #[test]
    fn parse_update_drops_empty_message() {
        let update = json!({
            "update_id": 4,
            "message": { "message_id": 13, "chat": { "id": 99 } }
        });
        assert!(parse_update(&update, &channel_id(), None).is_none());
    }

    #[test]
    fn parse_update_filters_chat_id() {
        let update = json!({
            "update_id": 5,
            "message": {
                "message_id": 14,
                "chat": { "id": 99 },
                "text": "hi"
            }
        });
        assert!(parse_update(&update, &channel_id(), Some("100")).is_none());
        assert!(parse_update(&update, &channel_id(), Some("99")).is_some());
    }
}
