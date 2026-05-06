use agentos_interfaces::{Channel, ChannelError};
use agentos_proto::{ChannelId, ConversationId, Envelope, Message, MessageRole};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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

struct FeishuLongConnection {
    socket: WebSocketConnection,
    fragments: HashMap<String, Vec<Option<Vec<u8>>>>,
}

struct WebSocketConnection {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FeishuEndpoint {
    url: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct FeishuFrame {
    seq_id: u64,
    log_id: u64,
    service: i32,
    method: i32,
    headers: Vec<FeishuHeader>,
    payload_encoding: String,
    payload_type: String,
    payload: Vec<u8>,
    log_id_new: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FeishuHeader {
    key: String,
    value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedWsUrl {
    host: String,
    port: u16,
    path_and_query: String,
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

    fn long_connection(&mut self) -> Result<&mut FeishuLongConnection, ChannelError> {
        if self.long_connection.is_none() {
            let endpoint = self.websocket_endpoint()?;
            self.long_connection = Some(FeishuLongConnection::connect(&endpoint)?);
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

    fn receive_long_connection(&mut self) -> Result<Option<Envelope>, ChannelError> {
        loop {
            let channel_id = self.id.clone();
            let allowed_source_ids = self.allowed_source_ids.clone();
            let log_receive_errors = self.log_receive_errors;
            let result = self.long_connection()?.receive_next_event(
                &channel_id,
                &allowed_source_ids,
                log_receive_errors,
            );
            match result {
                Ok(envelope) => return Ok(envelope),
                Err(err) => {
                    self.long_connection = None;
                    return Err(err);
                }
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
        match self.receive_long_connection() {
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

impl FeishuLongConnection {
    fn connect(endpoint: &FeishuEndpoint) -> Result<Self, ChannelError> {
        Ok(Self {
            socket: WebSocketConnection::connect(&endpoint.url)?,
            fragments: HashMap::new(),
        })
    }

    fn receive_next_event(
        &mut self,
        channel_id: &ChannelId,
        allowed_source_ids: &[Arc<str>],
        log_receive_errors: bool,
    ) -> Result<Option<Envelope>, ChannelError> {
        loop {
            let payload = self.socket.read_frame()?;
            let frame = FeishuFrame::decode(&payload)
                .map_err(|err| ChannelError::Backend(Arc::from(err)))?;
            if frame.method == 0 {
                if header_value(&frame.headers, "type") == Some("ping") {
                    self.socket.write_frame(&pong_frame(&frame).encode())?;
                }
                continue;
            }
            if frame.method != 1 {
                continue;
            }

            let frame_type = header_value(&frame.headers, "type");
            if frame_type != Some("event") {
                continue;
            }
            let payload = self.event_payload(&frame)?;
            let Some(payload) = payload else {
                continue;
            };
            let payload: Value = serde_json::from_slice(&payload)
                .map_err(|err| {
                    ChannelError::Backend(Arc::from(format!(
                        "Feishu event payload JSON parse failed: {err}; payload_encoding={}, payload_type={}",
                        frame.payload_encoding, frame.payload_type
                    )))
                })?;
            let started = Instant::now();
            self.ack_event(&frame, started)?;
            if let Some(envelope) = envelope_from_event(&payload, channel_id, allowed_source_ids) {
                return Ok(Some(envelope));
            }
            if log_receive_errors {
                if let Some(reason) = feishu_drop_reason(&payload, allowed_source_ids) {
                    eprintln!("feishu event dropped: {reason}");
                }
            }
        }
    }

    fn ack_event(&mut self, frame: &FeishuFrame, started: Instant) -> Result<(), ChannelError> {
        let ack = success_frame(frame, started.elapsed().as_millis() as u64);
        self.socket.write_frame(&ack.encode())
    }

    fn event_payload(&mut self, frame: &FeishuFrame) -> Result<Option<Vec<u8>>, ChannelError> {
        if frame.payload.is_empty() {
            return Ok(None);
        }
        let sum = header_value(&frame.headers, "sum")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1);
        if sum <= 1 {
            return Ok(Some(frame.payload.clone()));
        }
        let seq = header_value(&frame.headers, "seq")
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| {
                ChannelError::Backend(Arc::from("Feishu fragmented event is missing seq header"))
            })?;
        let message_id = header_value(&frame.headers, "message_id")
            .ok_or_else(|| {
                ChannelError::Backend(Arc::from(
                    "Feishu fragmented event is missing message_id header",
                ))
            })?
            .to_owned();
        if seq >= sum {
            return Err(ChannelError::Backend(Arc::from(format!(
                "Feishu fragmented event has invalid seq={seq}, sum={sum}",
            ))));
        }
        let entry = self
            .fragments
            .entry(message_id.clone())
            .or_insert_with(|| vec![None; sum]);
        if entry.len() != sum {
            *entry = vec![None; sum];
        }
        entry[seq] = Some(frame.payload.clone());
        if entry.iter().any(Option::is_none) {
            return Ok(None);
        }
        let chunks = self
            .fragments
            .remove(&message_id)
            .expect("fragment entry should exist");
        let total = chunks
            .iter()
            .map(|chunk| chunk.as_ref().map_or(0, Vec::len))
            .sum();
        let mut combined = Vec::with_capacity(total);
        for chunk in chunks.into_iter().flatten() {
            combined.extend_from_slice(&chunk);
        }
        Ok(Some(combined))
    }
}

impl WebSocketConnection {
    fn connect(url: &str) -> Result<Self, ChannelError> {
        let parsed =
            ParsedWsUrl::parse(url).map_err(|err| ChannelError::Backend(Arc::from(err)))?;
        let proxy = https_proxy_for_openssl(&parsed.host);
        let mut command = Command::new("openssl");
        command
            .arg("s_client")
            .arg("-quiet")
            .arg("-connect")
            .arg(format!("{}:{}", parsed.host, parsed.port))
            .arg("-servername")
            .arg(&parsed.host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(proxy) = &proxy {
            command.arg("-proxy").arg(proxy);
        }
        let mut child = command
            .spawn()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ChannelError::Backend(Arc::from("openssl stdin unavailable")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ChannelError::Backend(Arc::from("openssl stdout unavailable")))?;
        let mut stdout = BufReader::new(stdout);
        let key = websocket_key()?;
        write!(
            stdin,
            "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
            parsed.path_and_query, parsed.host, key
        )
        .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        stdin
            .flush()
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;

        let mut status = String::new();
        stdout
            .read_line(&mut status)
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
        if !status.contains(" 101 ") {
            return Err(ChannelError::Backend(Arc::from(format!(
                "Feishu WebSocket upgrade failed: {}",
                status.trim()
            ))));
        }
        loop {
            let mut line = String::new();
            stdout
                .read_line(&mut line)
                .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
            if line == "\r\n" || line == "\n" || line.is_empty() {
                break;
            }
        }

        Ok(Self {
            _child: child,
            stdin,
            stdout,
        })
    }

    fn read_frame(&mut self) -> Result<Vec<u8>, ChannelError> {
        loop {
            let mut header = [0_u8; 2];
            self.stdout
                .read_exact(&mut header)
                .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
            let opcode = header[0] & 0x0f;
            let masked = header[1] & 0x80 != 0;
            let mut len = u64::from(header[1] & 0x7f);
            if len == 126 {
                let mut bytes = [0_u8; 2];
                self.stdout
                    .read_exact(&mut bytes)
                    .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
                len = u64::from(u16::from_be_bytes(bytes));
            } else if len == 127 {
                let mut bytes = [0_u8; 8];
                self.stdout
                    .read_exact(&mut bytes)
                    .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
                len = u64::from_be_bytes(bytes);
            }
            let mask = if masked {
                let mut mask = [0_u8; 4];
                self.stdout
                    .read_exact(&mut mask)
                    .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
                Some(mask)
            } else {
                None
            };
            if len > 16 * 1024 * 1024 {
                return Err(ChannelError::Backend(Arc::from(
                    "Feishu WebSocket frame is too large",
                )));
            }
            let mut payload = vec![0_u8; len as usize];
            self.stdout
                .read_exact(&mut payload)
                .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?;
            if let Some(mask) = mask {
                for (index, byte) in payload.iter_mut().enumerate() {
                    *byte ^= mask[index % 4];
                }
            }
            match opcode {
                0x2 => return Ok(payload),
                0x8 => {
                    return Err(ChannelError::Backend(Arc::from(
                        "Feishu WebSocket closed by server",
                    )))
                }
                0x9 => {
                    self.write_control_frame(0x0a, &payload)?;
                }
                0x0a => {}
                _ => {}
            }
        }
    }

    fn write_frame(&mut self, payload: &[u8]) -> Result<(), ChannelError> {
        self.write_ws_frame(0x2, payload)
    }

    fn write_control_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), ChannelError> {
        self.write_ws_frame(opcode, payload)
    }

    fn write_ws_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), ChannelError> {
        let mut frame = Vec::new();
        frame.push(0x80 | opcode);
        let mask_key = websocket_mask()?;
        if payload.len() < 126 {
            frame.push(0x80 | payload.len() as u8);
        } else if payload.len() <= u16::MAX as usize {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask_key);
        for (index, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask_key[index % 4]);
        }
        self.stdin
            .write_all(&frame)
            .and_then(|_| self.stdin.flush())
            .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))
    }
}

impl FeishuFrame {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.seq_id != 0 {
            write_varint_field(&mut out, 1, self.seq_id);
        }
        if self.log_id != 0 {
            write_varint_field(&mut out, 2, self.log_id);
        }
        if self.service != 0 {
            write_varint_field(&mut out, 3, self.service as u64);
        }
        if self.method != 0 {
            write_varint_field(&mut out, 4, self.method as u64);
        }
        for header in &self.headers {
            let mut encoded = Vec::new();
            write_bytes_field(&mut encoded, 1, header.key.as_bytes());
            write_bytes_field(&mut encoded, 2, header.value.as_bytes());
            write_bytes_field(&mut out, 5, &encoded);
        }
        if !self.payload_encoding.is_empty() {
            write_bytes_field(&mut out, 6, self.payload_encoding.as_bytes());
        }
        if !self.payload_type.is_empty() {
            write_bytes_field(&mut out, 7, self.payload_type.as_bytes());
        }
        if !self.payload.is_empty() {
            write_bytes_field(&mut out, 8, &self.payload);
        }
        if !self.log_id_new.is_empty() {
            write_bytes_field(&mut out, 9, self.log_id_new.as_bytes());
        }
        out
    }

    fn decode(input: &[u8]) -> Result<Self, String> {
        let mut frame = Self::default();
        let mut cursor = 0;
        while cursor < input.len() {
            let key = read_varint(input, &mut cursor)?;
            let field = key >> 3;
            let wire = key & 0x07;
            match (field, wire) {
                (1, 0) => frame.seq_id = read_varint(input, &mut cursor)?,
                (2, 0) => frame.log_id = read_varint(input, &mut cursor)?,
                (3, 0) => frame.service = read_varint(input, &mut cursor)? as i32,
                (4, 0) => frame.method = read_varint(input, &mut cursor)? as i32,
                (5, 2) => {
                    let bytes = read_bytes(input, &mut cursor)?;
                    frame.headers.push(decode_header(bytes)?);
                }
                (6, 2) => frame.payload_encoding = decode_string(read_bytes(input, &mut cursor)?)?,
                (7, 2) => frame.payload_type = decode_string(read_bytes(input, &mut cursor)?)?,
                (8, 2) => frame.payload = read_bytes(input, &mut cursor)?.to_vec(),
                (9, 2) => frame.log_id_new = decode_string(read_bytes(input, &mut cursor)?)?,
                (_, _) => skip_proto_field(input, &mut cursor, wire)?,
            }
        }
        Ok(frame)
    }
}

impl ParsedWsUrl {
    fn parse(input: &str) -> Result<Self, String> {
        let rest = input
            .strip_prefix("wss://")
            .ok_or_else(|| "Feishu WebSocket URL must use wss://".to_owned())?;
        let (authority, path) = rest
            .split_once('/')
            .map_or((rest, "/"), |(authority, path)| (authority, path));
        let (host, port) = if let Some((host, port)) = authority.rsplit_once(':') {
            (
                host.to_owned(),
                port.parse::<u16>()
                    .map_err(|err| format!("invalid WebSocket port: {err}"))?,
            )
        } else {
            (authority.to_owned(), 443)
        };
        if host.is_empty() {
            return Err("Feishu WebSocket URL is missing a host".to_owned());
        }
        Ok(Self {
            host,
            port,
            path_and_query: format!("/{path}"),
        })
    }
}

fn success_frame(frame: &FeishuFrame, biz_rt_ms: u64) -> FeishuFrame {
    let mut headers = frame.headers.clone();
    headers.push(FeishuHeader {
        key: "biz_rt".to_owned(),
        value: biz_rt_ms.to_string(),
    });
    FeishuFrame {
        seq_id: frame.seq_id,
        log_id: frame.log_id,
        service: frame.service,
        method: 1,
        headers,
        payload_encoding: frame.payload_encoding.clone(),
        payload_type: frame.payload_type.clone(),
        payload: br#"{"code":200,"headers":null,"data":null}"#.to_vec(),
        log_id_new: frame.log_id_new.clone(),
    }
}

fn pong_frame(frame: &FeishuFrame) -> FeishuFrame {
    FeishuFrame {
        seq_id: frame.seq_id,
        log_id: frame.log_id,
        service: frame.service,
        method: 0,
        headers: vec![FeishuHeader {
            key: "type".to_owned(),
            value: "pong".to_owned(),
        }],
        payload_encoding: "json".to_owned(),
        payload_type: "application/json".to_owned(),
        payload: Vec::new(),
        log_id_new: frame.log_id_new.clone(),
    }
}

fn header_value<'a>(headers: &'a [FeishuHeader], key: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|header| header.key == key)
        .map(|header| header.value.as_str())
}

fn decode_header(input: &[u8]) -> Result<FeishuHeader, String> {
    let mut cursor = 0;
    let mut key = String::new();
    let mut value = String::new();
    while cursor < input.len() {
        let field_key = read_varint(input, &mut cursor)?;
        let field = field_key >> 3;
        let wire = field_key & 0x07;
        match (field, wire) {
            (1, 2) => key = decode_string(read_bytes(input, &mut cursor)?)?,
            (2, 2) => value = decode_string(read_bytes(input, &mut cursor)?)?,
            (_, _) => skip_proto_field(input, &mut cursor, wire)?,
        }
    }
    Ok(FeishuHeader { key, value })
}

fn write_varint_field(out: &mut Vec<u8>, field: u64, value: u64) {
    write_varint(out, field << 3);
    write_varint(out, value);
}

fn write_bytes_field(out: &mut Vec<u8>, field: u64, value: &[u8]) {
    write_varint(out, (field << 3) | 2);
    write_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn read_varint(input: &[u8], cursor: &mut usize) -> Result<u64, String> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let Some(byte) = input.get(*cursor).copied() else {
            return Err("unexpected end of protobuf varint".to_owned());
        };
        *cursor += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("protobuf varint overflow".to_owned())
}

fn read_bytes<'a>(input: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], String> {
    let len = read_varint(input, cursor)? as usize;
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| "protobuf length overflow".to_owned())?;
    if end > input.len() {
        return Err("unexpected end of protobuf bytes".to_owned());
    }
    let bytes = &input[*cursor..end];
    *cursor = end;
    Ok(bytes)
}

fn decode_string(input: &[u8]) -> Result<String, String> {
    std::str::from_utf8(input)
        .map(ToOwned::to_owned)
        .map_err(|err| err.to_string())
}

fn skip_proto_field(input: &[u8], cursor: &mut usize, wire: u64) -> Result<(), String> {
    match wire {
        0 => {
            let _ = read_varint(input, cursor)?;
            Ok(())
        }
        1 => {
            *cursor = cursor
                .checked_add(8)
                .ok_or_else(|| "protobuf fixed64 overflow".to_owned())?;
            if *cursor > input.len() {
                Err("unexpected end of protobuf fixed64".to_owned())
            } else {
                Ok(())
            }
        }
        2 => {
            let _ = read_bytes(input, cursor)?;
            Ok(())
        }
        5 => {
            *cursor = cursor
                .checked_add(4)
                .ok_or_else(|| "protobuf fixed32 overflow".to_owned())?;
            if *cursor > input.len() {
                Err("unexpected end of protobuf fixed32".to_owned())
            } else {
                Ok(())
            }
        }
        other => Err(format!("unsupported protobuf wire type {other}")),
    }
}

fn websocket_key() -> Result<String, ChannelError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?
        .as_nanos();
    Ok(base64_encode(&now.to_be_bytes()))
}

fn websocket_mask() -> Result<[u8; 4], ChannelError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))?
        .as_nanos();
    let bytes = now.to_be_bytes();
    Ok([bytes[12], bytes[13], bytes[14], bytes[15]])
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        output.push(TABLE[(a >> 2) as usize] as char);
        output.push(TABLE[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(c & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn https_proxy_for_openssl(host: &str) -> Option<String> {
    if no_proxy_matches(host) {
        return None;
    }
    env::var("HTTPS_PROXY")
        .or_else(|_| env::var("https_proxy"))
        .ok()
        .and_then(|proxy| proxy.strip_prefix("http://").map(ToOwned::to_owned))
        .and_then(|proxy| {
            let proxy = proxy.trim_end_matches('/').to_owned();
            (!proxy.is_empty()).then_some(proxy)
        })
}

fn no_proxy_matches(host: &str) -> bool {
    env::var("NO_PROXY")
        .or_else(|_| env::var("no_proxy"))
        .ok()
        .is_some_and(|no_proxy| {
            no_proxy
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .any(|entry| no_proxy_entry_matches(host, entry))
        })
}

fn no_proxy_entry_matches(host: &str, entry: &str) -> bool {
    entry == "*"
        || host.eq_ignore_ascii_case(entry)
        || entry
            .strip_prefix('.')
            .is_some_and(|suffix| host.ends_with(suffix))
}

fn curl_failure_message(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    if !stdout.is_empty() {
        return stdout;
    }
    "curl command failed".to_owned()
}

fn envelope_from_event(
    payload: &Value,
    channel_id: &ChannelId,
    allowed_source_ids: &[Arc<str>],
) -> Option<Envelope> {
    let event_type = payload
        .get("header")
        .and_then(|header| header.get("event_type"))
        .and_then(Value::as_str);
    if event_type.is_some_and(|value| value != "im.message.receive_v1") {
        return None;
    }

    let event = payload.get("event")?;
    let message = event.get("message")?;
    if message.get("message_type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let chat_id = message.get("chat_id").and_then(Value::as_str)?;
    let sender_id = event
        .get("sender")
        .and_then(|sender| sender.get("sender_id"));
    if !feishu_allowed_source_matches(allowed_source_ids, sender_id) {
        return None;
    }
    let text_content = text_content(message.get("content").and_then(Value::as_str)?)?;
    let text = text_content.trim();
    if text.is_empty() {
        return None;
    }

    let sender = sender_id
        .and_then(preferred_feishu_sender_id)
        .map_or_else(|| Arc::from("feishu-user"), Arc::from);
    let mut metadata = BTreeMap::new();
    metadata.insert(Arc::from("kind"), Value::String("feishu".to_owned()));
    if let Some(event_id) = payload
        .get("header")
        .and_then(|header| header.get("event_id"))
        .and_then(Value::as_str)
    {
        metadata.insert(Arc::from("event_id"), Value::String(event_id.to_owned()));
    }
    if let Some(tenant_key) = payload
        .get("header")
        .and_then(|header| header.get("tenant_key"))
        .and_then(Value::as_str)
    {
        metadata.insert(
            Arc::from("tenant_key"),
            Value::String(tenant_key.to_owned()),
        );
    }
    if let Some(message_id) = message.get("message_id").and_then(Value::as_str) {
        metadata.insert(
            Arc::from("message_id"),
            Value::String(message_id.to_owned()),
        );
    }
    if let Some(chat_type) = message.get("chat_type").and_then(Value::as_str) {
        metadata.insert(Arc::from("chat_type"), Value::String(chat_type.to_owned()));
    }

    Some(Envelope {
        channel_id: channel_id.clone(),
        conversation_id: ConversationId::new(chat_id),
        sender,
        message: Message::text(MessageRole::User, text),
        metadata,
    })
}

fn text_content(content: &str) -> Option<String> {
    serde_json::from_str::<Value>(content)
        .ok()?
        .get("text")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn feishu_drop_reason(payload: &Value, allowed_source_ids: &[Arc<str>]) -> Option<String> {
    let event_type = payload
        .get("header")
        .and_then(|header| header.get("event_type"))
        .and_then(Value::as_str);
    if event_type.is_some_and(|value| value != "im.message.receive_v1") {
        return Some(format!(
            "unsupported event_type={}, expected im.message.receive_v1",
            event_type.unwrap_or("<missing>")
        ));
    }

    let Some(event) = payload.get("event") else {
        return Some("missing event body".to_owned());
    };
    let Some(message) = event.get("message") else {
        return Some("missing event.message".to_owned());
    };
    let message_type = message.get("message_type").and_then(Value::as_str);
    if message_type != Some("text") {
        return Some(format!(
            "unsupported message_type={}, expected text",
            message_type.unwrap_or("<missing>")
        ));
    }
    let Some(chat_id) = message.get("chat_id").and_then(Value::as_str) else {
        return Some("missing message.chat_id".to_owned());
    };
    let sender_id = event
        .get("sender")
        .and_then(|sender| sender.get("sender_id"));
    if !feishu_allowed_source_matches(allowed_source_ids, sender_id) {
        return Some(format!(
            "filtered by allowed sender ids: allowed={}, chat_id={}, sender_ids={}",
            feishu_allowed_ids_summary(allowed_source_ids),
            chat_id,
            feishu_sender_ids_summary(sender_id)
        ));
    }
    let Some(raw_content) = message.get("content").and_then(Value::as_str) else {
        return Some("missing message.content".to_owned());
    };
    let Some(text) = text_content(raw_content) else {
        return Some("message.content is not a text json payload".to_owned());
    };
    if text.trim().is_empty() {
        return Some("message text is empty after trimming".to_owned());
    }
    None
}

fn feishu_allowed_source_matches(
    allowed_source_ids: &[Arc<str>],
    sender_id: Option<&Value>,
) -> bool {
    if allowed_source_ids.is_empty() {
        return true;
    }
    allowed_source_ids.iter().any(|allowed| {
        sender_id.is_some_and(|sender_id| feishu_sender_id_matches(sender_id, allowed))
    })
}

fn feishu_sender_id_matches(sender_id: &Value, allowed: &str) -> bool {
    ["open_id", "user_id", "union_id"]
        .into_iter()
        .any(|key| sender_id.get(key).and_then(Value::as_str) == Some(allowed))
}

fn preferred_feishu_sender_id(sender_id: &Value) -> Option<&str> {
    sender_id
        .get("open_id")
        .and_then(Value::as_str)
        .or_else(|| sender_id.get("user_id").and_then(Value::as_str))
        .or_else(|| sender_id.get("union_id").and_then(Value::as_str))
}

fn feishu_sender_ids_summary(sender_id: Option<&Value>) -> String {
    let Some(sender_id) = sender_id else {
        return "<missing>".to_owned();
    };
    let mut parts = Vec::new();
    for key in ["open_id", "user_id", "union_id"] {
        if let Some(value) = sender_id.get(key).and_then(Value::as_str) {
            parts.push(format!("{key}={value}"));
        }
    }
    if parts.is_empty() {
        "<missing>".to_owned()
    } else {
        parts.join(",")
    }
}

fn feishu_allowed_ids_summary(allowed_source_ids: &[Arc<str>]) -> String {
    if allowed_source_ids.is_empty() {
        "<unset>".to_owned()
    } else {
        allowed_source_ids
            .iter()
            .map(|value| value.as_ref())
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn feishu_receive_id_type<'a>(receive_id: &str, configured: &'a str) -> &'a str {
    if receive_id.starts_with("oc_") {
        "chat_id"
    } else if receive_id.starts_with("ou_") {
        "open_id"
    } else if receive_id.starts_with("on_") {
        "union_id"
    } else {
        configured
    }
}

fn feishu_allowed_source_ids_from_env() -> Vec<Arc<str>> {
    let mut values = Vec::new();
    extend_allowed_source_ids(&mut values, env::var("AGENTOS_FEISHU_ALLOWED_ID").ok());
    values
}

fn extend_allowed_source_ids(values: &mut Vec<Arc<str>>, raw: Option<String>) {
    let Some(raw) = raw else {
        return;
    };
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if values.iter().any(|existing| existing.as_ref() == value) {
            continue;
        }
        values.push(Arc::from(value));
    }
}

fn unix_now() -> Result<u64, ChannelError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|err| ChannelError::Backend(Arc::from(err.to_string())))
}

