use agentos_interfaces::{Channel, ChannelError};
use agentos_proto::{ChannelId, ConversationId, Envelope, Message, MessageRole};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::process::Command;
use std::sync::Arc;

pub struct TelegramChannel {
    token: Arc<str>,
    id: ChannelId,
    allowed_chat_id: Option<Arc<str>>,
    offset: Option<i64>,
    log_receive_errors: bool,
}

impl TelegramChannel {
    pub fn from_env() -> Result<Self, ChannelError> {
        let token = env::var("AGENTOS_TELEGRAM_BOT_TOKEN")
            .map_err(|_| ChannelError::Backend(Arc::from("missing AGENTOS_TELEGRAM_BOT_TOKEN")))?;
        let allowed_chat_id = env::var("AGENTOS_TELEGRAM_CHAT_ID").ok().map(Arc::from);
        Ok(Self {
            token: Arc::from(token),
            id: ChannelId::new("telegram"),
            allowed_chat_id,
            offset: None,
            log_receive_errors: false,
        })
    }

    pub fn with_receive_error_logging(mut self, enabled: bool) -> Self {
        self.log_receive_errors = enabled;
        self
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.token)
    }

    fn fetch_updates(&self) -> Result<Value, ChannelError> {
        let mut command = Command::new("curl");
        command.args(["--silent", "--show-error", "--max-time", "35", "-X", "POST"]);
        command.arg(self.api_url("getUpdates"));
        command.args(["-d", "timeout=30"]);
        if let Some(offset) = self.offset {
            command.args(["-d", &format!("offset={offset}")]);
        }

        let output = command
            .output()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ChannelError::Backend(Arc::from(stderr.trim().to_owned())));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))
    }

    fn send_text(&self, chat_id: &str, text: &str) -> Result<(), ChannelError> {
        let text_arg = format!("text={text}");
        let chat_arg = format!("chat_id={chat_id}");
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", "10", "-X", "POST"])
            .arg(self.api_url("sendMessage"))
            .args(["-d", &chat_arg, "--data-urlencode", &text_arg])
            .output()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ChannelError::Backend(Arc::from(stderr.trim().to_owned())));
        }

        let response: Value = serde_json::from_slice(&output.stdout)
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if response.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(ChannelError::Backend(Arc::from(response.to_string())))
        }
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn id(&self) -> ChannelId {
        self.id.clone()
    }

    async fn receive(&mut self) -> Option<Envelope> {
        let response = match self.fetch_updates() {
            Ok(response) => response,
            Err(err) => {
                if self.log_receive_errors {
                    eprintln!("telegram getUpdates failed: {err}");
                }
                return None;
            }
        };
        let (envelope, next_offset) =
            first_envelope_from_updates(&response, &self.id, self.allowed_chat_id.as_deref())?;
        self.offset = Some(next_offset);
        Some(envelope)
    }

    async fn send(&self, env: Envelope) -> Result<(), ChannelError> {
        self.send_text(env.conversation_id.as_str(), &env.message.content)
    }
}

fn first_envelope_from_updates(
    response: &Value,
    channel_id: &ChannelId,
    allowed_chat_id: Option<&str>,
) -> Option<(Envelope, i64)> {
    let updates = response.get("result")?.as_array()?;
    for update in updates {
        let update_id = update.get("update_id")?.as_i64()?;
        let Some(envelope) = envelope_from_update(update, channel_id, allowed_chat_id) else {
            continue;
        };
        return Some((envelope, update_id + 1));
    }
    None
}

fn envelope_from_update(
    update: &Value,
    channel_id: &ChannelId,
    allowed_chat_id: Option<&str>,
) -> Option<Envelope> {
    let message = update.get("message")?;
    let text = message.get("text")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }

    let chat_id = chat_id_string(message.get("chat")?)?;
    if allowed_chat_id.is_some_and(|allowed| allowed != chat_id) {
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
    let mut metadata = BTreeMap::new();
    metadata.insert(Arc::from("kind"), Value::String("telegram".to_owned()));
    metadata.insert(Arc::from("update_id"), Value::from(update_id));
    if let Some(message_id) = message.get("message_id").and_then(Value::as_i64) {
        metadata.insert(Arc::from("message_id"), Value::from(message_id));
    }

    Some(Envelope {
        channel_id: channel_id.clone(),
        conversation_id: ConversationId::new(chat_id),
        sender,
        message: Message::text(MessageRole::User, text),
        metadata,
    })
}

fn chat_id_string(chat: &Value) -> Option<String> {
    if let Some(id) = chat.get("id").and_then(Value::as_i64) {
        return Some(id.to_string());
    }
    chat.get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

