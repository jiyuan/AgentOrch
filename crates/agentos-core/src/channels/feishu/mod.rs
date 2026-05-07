use agentos_interfaces::{Channel, ChannelError};
use agentos_proto::{ChannelId, Envelope};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::env;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

mod event;
mod long_connection;
mod proto;
mod websocket;

use event::{curl_failure_message, feishu_allowed_source_ids_from_env, feishu_receive_id_type};
use long_connection::{FeishuEndpoint, FeishuLongConnection};

const DEFAULT_API_BASE: &str = "https://open.feishu.cn/open-apis";

pub struct FeishuChannel {
    app_id: Arc<str>,
    app_secret: Arc<str>,
    id: ChannelId,
    api_base: Arc<str>,
    receive_id_type: Arc<str>,
    allowed_source_ids: Vec<Arc<str>>,
    tenant_token: Mutex<Option<CachedTenantToken>>,
    long_connection: Option<FeishuLongConnection>,
    log_receive_errors: bool,
}

#[derive(Clone, Debug)]
struct CachedTenantToken {
    token: Arc<str>,
    expires_at: u64,
}

impl FeishuChannel {
    pub fn from_env() -> Result<Self, ChannelError> {
        let app_id = env::var("AGENTOS_FEISHU_APP_ID")
            .map_err(|_| ChannelError::Backend(Arc::from("missing AGENTOS_FEISHU_APP_ID")))?;
        let app_secret = env::var("AGENTOS_FEISHU_APP_SECRET")
            .map_err(|_| ChannelError::Backend(Arc::from("missing AGENTOS_FEISHU_APP_SECRET")))?;
        let api_base =
            env::var("AGENTOS_FEISHU_API_BASE").unwrap_or_else(|_| DEFAULT_API_BASE.to_owned());
        let receive_id_type =
            env::var("AGENTOS_FEISHU_RECEIVE_ID_TYPE").unwrap_or_else(|_| "chat_id".to_owned());
        let allowed_source_ids = feishu_allowed_source_ids_from_env();

        Ok(Self {
            app_id: Arc::from(app_id),
            app_secret: Arc::from(app_secret),
            id: ChannelId::new("feishu"),
            api_base: Arc::from(api_base.trim_end_matches('/').to_owned()),
            receive_id_type: Arc::from(receive_id_type),
            allowed_source_ids,
            tenant_token: Mutex::new(None),
            long_connection: None,
            log_receive_errors: false,
        })
    }

    pub fn with_receive_error_logging(mut self, enabled: bool) -> Self {
        self.log_receive_errors = enabled;
        self
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/{}", self.api_base, path.trim_start_matches('/'))
    }

    fn tenant_access_token(&self) -> Result<Arc<str>, ChannelError> {
        let now = unix_now()?;
        let mut cache = self
            .tenant_token
            .lock()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if let Some(cached) = cache.as_ref() {
            if cached.expires_at > now {
                return Ok(Arc::clone(&cached.token));
            }
        }

        let body = json!({
            "app_id": self.app_id.as_ref(),
            "app_secret": self.app_secret.as_ref(),
        })
        .to_string();
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", "10", "-X", "POST"])
            .arg(self.api_url("auth/v3/tenant_access_token/internal"))
            .args(["-H", "Content-Type: application/json", "--data", &body])
            .output()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ChannelError::Backend(Arc::from(stderr.trim().to_owned())));
        }

        let response: Value = serde_json::from_slice(&output.stdout)
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if response.get("code").and_then(Value::as_i64) != Some(0) {
            return Err(ChannelError::Backend(Arc::from(response.to_string())));
        }
        let token = response
            .get("tenant_access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ChannelError::Backend(Arc::from(
                    "Feishu token response missing tenant_access_token",
                ))
            })?;
        let expire = response
            .get("expire")
            .and_then(Value::as_u64)
            .unwrap_or(7_200);
        let token = Arc::from(token);
        *cache = Some(CachedTenantToken {
            token: Arc::clone(&token),
            expires_at: now.saturating_add(expire.saturating_sub(60)),
        });
        Ok(token)
    }

    fn send_text(&self, receive_id: &str, text: &str) -> Result<(), ChannelError> {
        let token = self.tenant_access_token()?;
        let authorization = format!("Authorization: Bearer {token}");
        let content = json!({ "text": text }).to_string();
        let body = json!({
            "receive_id": receive_id,
            "msg_type": "text",
            "content": content,
        })
        .to_string();
        let receive_id_type = feishu_receive_id_type(receive_id, self.receive_id_type.as_ref());
        let url = format!(
            "{}?receive_id_type={}",
            self.api_url("im/v1/messages"),
            receive_id_type
        );
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", "10", "-X", "POST"])
            .arg(url)
            .args([
                "-H",
                &authorization,
                "-H",
                "Content-Type: application/json",
                "--data",
                &body,
            ])
            .output()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ChannelError::Backend(Arc::from(stderr.trim().to_owned())));
        }

        let response: Value = serde_json::from_slice(&output.stdout)
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if response.get("code").and_then(Value::as_i64) == Some(0) {
            Ok(())
        } else {
            Err(ChannelError::Backend(Arc::from(response.to_string())))
        }
    }

    fn websocket_endpoint(&self) -> Result<FeishuEndpoint, ChannelError> {
        let body = json!({
            "AppID": self.app_id.as_ref(),
            "AppSecret": self.app_secret.as_ref(),
        })
        .to_string();
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", "10", "-X", "POST"])
            .arg(self.platform_url("callback/ws/endpoint"))
            .args([
                "-H",
                "Content-Type: application/json",
                "-H",
                "locale: zh",
                "--data",
                &body,
            ])
            .output()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if !output.status.success() {
            return Err(ChannelError::Backend(Arc::from(curl_failure_message(
                &output.stdout,
                &output.stderr,
            ))));
        }

        let response: Value = serde_json::from_slice(&output.stdout).map_err(|err| {
            ChannelError::Backend(Arc::from(format!(
                "Feishu WebSocket endpoint JSON parse failed: {err}; body={}",
                String::from_utf8_lossy(&output.stdout)
            )))
        })?;
        if response.get("code").and_then(Value::as_i64) != Some(0) {
            return Err(ChannelError::Backend(Arc::from(response.to_string())));
        }
        let url = response
            .get("data")
            .and_then(|data| data.get("URL").or_else(|| data.get("url")))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ChannelError::Backend(Arc::from("Feishu WebSocket endpoint response missing URL"))
            })?;
        Ok(FeishuEndpoint {
            url: url.to_owned(),
        })
    }

    async fn long_connection(&mut self) -> Result<&mut FeishuLongConnection, ChannelError> {
        if self.long_connection.is_none() {
            let endpoint = self.websocket_endpoint()?;
            self.long_connection = Some(FeishuLongConnection::connect(&endpoint).await?);
        }
        Ok(self
            .long_connection
            .as_mut()
            .expect("long connection was initialized"))
    }

    fn platform_url(&self, path: &str) -> String {
        let base = self
            .api_base
            .strip_suffix("/open-apis")
            .unwrap_or(self.api_base.as_ref());
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    async fn receive_long_connection(&mut self) -> Result<Option<Envelope>, ChannelError> {
        let channel_id = self.id.clone();
        let allowed_source_ids = self.allowed_source_ids.clone();
        let log_receive_errors = self.log_receive_errors;
        let connection = self.long_connection().await?;
        match connection
            .receive_next_event(&channel_id, &allowed_source_ids, log_receive_errors)
            .await
        {
            Ok(envelope) => Ok(envelope),
            Err(err) => {
                self.long_connection = None;
                Err(err)
            }
        }
    }
}

#[async_trait]
impl Channel for FeishuChannel {
    fn id(&self) -> ChannelId {
        self.id.clone()
    }

    async fn receive(&mut self) -> Option<Envelope> {
        match self.receive_long_connection().await {
            Ok(envelope) => envelope,
            Err(err) => {
                if self.log_receive_errors {
                    eprintln!("feishu long connection receive failed: {err}");
                }
                None
            }
        }
    }

    async fn send(&self, env: Envelope) -> Result<(), ChannelError> {
        self.send_text(env.conversation_id.as_str(), &env.message.content)
    }
}

fn unix_now() -> Result<u64, ChannelError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))
}
