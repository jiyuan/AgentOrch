use crate::memory::{MemoryCaller, MemoryManager, ReflectionReport, ReflectionRequest};
use agentos_interfaces::memory::MemoryError;
use agentos_proto::{ChannelId, ConversationId, Envelope, Message, MessageRole};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum CronError {
    #[error("cron interval must be greater than zero")]
    InvalidInterval,
    #[error("gateway receiver is closed")]
    GatewayClosed,
    #[error("memory maintenance failed: {0}")]
    Memory(#[from] MemoryError),
    #[error("cron storage failed: {0}")]
    Storage(Arc<str>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CronSchedule {
    pub interval_seconds: u64,
    pub next_due_unix: u64,
}

impl CronSchedule {
    pub fn every_hours(interval_hours: u64, next_due_unix: u64) -> Result<Self, CronError> {
        let interval_seconds = interval_hours
            .checked_mul(60 * 60)
            .ok_or(CronError::InvalidInterval)?;
        Self::every_seconds(interval_seconds, next_due_unix)
    }

    pub fn every_seconds(interval_seconds: u64, next_due_unix: u64) -> Result<Self, CronError> {
        if interval_seconds == 0 {
            return Err(CronError::InvalidInterval);
        }
        Ok(Self {
            interval_seconds,
            next_due_unix,
        })
    }

    fn is_due(&self, now_unix: u64) -> bool {
        self.next_due_unix <= now_unix
    }

    fn advance_after(&mut self, now_unix: u64) {
        while self.next_due_unix <= now_unix {
            let next = self.next_due_unix.saturating_add(self.interval_seconds);
            if next == self.next_due_unix {
                self.next_due_unix = now_unix.saturating_add(self.interval_seconds);
                break;
            }
            self.next_due_unix = next;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CronTask {
    pub id: Arc<str>,
    pub channel_id: ChannelId,
    pub conversation_id: ConversationId,
    pub sender: Arc<str>,
    pub prompt: Arc<str>,
    pub schedule: CronSchedule,
    #[serde(default)]
    pub retry: CronRetryPolicy,
    #[serde(default)]
    pub retry_state: CronRetryState,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CronRetryPolicy {
    pub max_retries: u32,
    pub backoff_seconds: u64,
}

impl Default for CronRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            backoff_seconds: 300,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CronRetryState {
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CronInvocation {
    pub task_id: Arc<str>,
    pub envelope: Envelope,
}

impl CronTask {
    pub fn new(
        id: impl Into<Arc<str>>,
        channel_id: ChannelId,
        conversation_id: ConversationId,
        prompt: impl Into<Arc<str>>,
        schedule: CronSchedule,
    ) -> Self {
        let id = id.into();
        Self {
            sender: Arc::from(format!("cron:{id}")),
            id,
            channel_id,
            conversation_id,
            prompt: prompt.into(),
            schedule,
            retry: CronRetryPolicy::default(),
            retry_state: CronRetryState::default(),
            enabled: true,
        }
    }

    pub fn to_envelope(&self) -> Envelope {
        let mut metadata = BTreeMap::new();
        metadata.insert(Arc::from("kind"), Value::String("cron".to_owned()));
        metadata.insert(
            Arc::from("cron_id"),
            Value::String(self.id.as_ref().to_owned()),
        );
        if self.retry_state.consecutive_failures > 0 {
            metadata.insert(
                Arc::from("cron_retry_attempt"),
                Value::from(self.retry_state.consecutive_failures),
            );
        }

        Envelope {
            channel_id: self.channel_id.clone(),
            conversation_id: self.conversation_id.clone(),
            sender: Arc::clone(&self.sender),
            message: Message::text(MessageRole::User, Arc::clone(&self.prompt)),
            metadata,
        }
    }

    fn is_due(&self, now_unix: u64) -> bool {
        match self.retry_state.next_retry_unix {
            Some(next_retry) => next_retry <= now_unix,
            None => self.schedule.is_due(now_unix),
        }
    }

    fn mark_success(&mut self, now_unix: u64) {
        self.retry_state = CronRetryState::default();
        self.schedule.advance_after(now_unix);
    }

    fn mark_failure(&mut self, now_unix: u64, error: impl Into<Arc<str>>) {
        self.retry_state.consecutive_failures =
            self.retry_state.consecutive_failures.saturating_add(1);
        self.retry_state.last_error = Some(error.into());
        if self.retry_state.consecutive_failures <= self.retry.max_retries {
            let delay = self
                .retry
                .backoff_seconds
                .saturating_mul(u64::from(self.retry_state.consecutive_failures));
            self.retry_state.next_retry_unix = Some(now_unix.saturating_add(delay));
        } else {
            self.retry_state.consecutive_failures = 0;
            self.retry_state.next_retry_unix = None;
            self.schedule.advance_after(now_unix);
        }
    }
}

fn default_enabled() -> bool {
    true
}

#[derive(Default)]
pub struct CronScheduler {
    tasks: Vec<CronTask>,
}

pub struct CronStore {
    root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryMaintenanceCron {
    pub id: Arc<str>,
    pub caller: MemoryCaller,
    pub request: ReflectionRequest,
    pub schedule: CronSchedule,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl MemoryMaintenanceCron {
    pub fn new(
        id: impl Into<Arc<str>>,
        caller: MemoryCaller,
        request: ReflectionRequest,
        schedule: CronSchedule,
    ) -> Self {
        Self {
            id: id.into(),
            caller,
            request,
            schedule,
            enabled: true,
        }
    }

    pub async fn run_due(
        &mut self,
        now_unix: u64,
        manager: &MemoryManager,
    ) -> Result<Option<ReflectionReport>, CronError> {
        if !self.enabled || !self.schedule.is_due(now_unix) {
            return Ok(None);
        }
        let report = manager.reflect(&self.caller, self.request.clone()).await?;
        self.schedule.advance_after(now_unix);
        Ok(Some(report))
    }
}

impl CronScheduler {
    pub fn new(tasks: impl IntoIterator<Item = CronTask>) -> Self {
        Self {
            tasks: tasks.into_iter().collect(),
        }
    }

    pub fn tasks(&self) -> &[CronTask] {
        &self.tasks
    }

    pub fn upsert_task(&mut self, task: CronTask) {
        if let Some(existing) = self
            .tasks
            .iter_mut()
            .find(|existing| existing.id == task.id)
        {
            *existing = task;
        } else {
            self.tasks.push(task);
        }
        self.tasks.sort_by(|left, right| left.id.cmp(&right.id));
    }

    pub fn due_invocations(&self, now_unix: u64) -> Vec<CronInvocation> {
        self.tasks
            .iter()
            .filter(|task| task.enabled && task.is_due(now_unix))
            .map(|task| CronInvocation {
                task_id: Arc::clone(&task.id),
                envelope: task.to_envelope(),
            })
            .collect()
    }

    pub fn record_success(&mut self, task_id: &str, now_unix: u64) -> Result<(), CronError> {
        let task = self
            .tasks
            .iter_mut()
            .find(|task| task.id.as_ref() == task_id)
            .ok_or_else(|| {
                CronError::Storage(Arc::from(format!("unknown cron task '{task_id}'")))
            })?;
        task.mark_success(now_unix);
        Ok(())
    }

    pub fn record_failure(
        &mut self,
        task_id: &str,
        now_unix: u64,
        error: impl Into<Arc<str>>,
    ) -> Result<(), CronError> {
        let task = self
            .tasks
            .iter_mut()
            .find(|task| task.id.as_ref() == task_id)
            .ok_or_else(|| {
                CronError::Storage(Arc::from(format!("unknown cron task '{task_id}'")))
            })?;
        task.mark_failure(now_unix, error);
        Ok(())
    }

    pub async fn enqueue_due(
        &mut self,
        now_unix: u64,
        gateway: &mpsc::Sender<Envelope>,
    ) -> Result<usize, CronError> {
        let mut sent = 0;
        for task in &mut self.tasks {
            if !task.enabled || !task.schedule.is_due(now_unix) {
                continue;
            }
            gateway
                .send(task.to_envelope())
                .await
                .map_err(|_| CronError::GatewayClosed)?;
            task.schedule.advance_after(now_unix);
            sent += 1;
        }
        Ok(sent)
    }
}

impl CronStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load_scheduler(&self) -> Result<CronScheduler, CronError> {
        let mut tasks = Vec::new();
        for path in self.cron_files()? {
            let input = std::fs::read_to_string(&path).map_err(storage_error)?;
            tasks.push(toml::from_str(&input).map_err(toml_de_error)?);
        }
        Ok(CronScheduler::new(tasks))
    }

    pub fn save_task(&self, task: &CronTask) -> Result<(), CronError> {
        std::fs::create_dir_all(&self.root).map_err(storage_error)?;
        let encoded = toml::to_string_pretty(task).map_err(toml_ser_error)?;
        std::fs::write(self.task_path(&task.id)?, encoded).map_err(storage_error)
    }

    pub fn save_scheduler(&self, scheduler: &CronScheduler) -> Result<(), CronError> {
        for task in scheduler.tasks() {
            self.save_task(task)?;
        }
        Ok(())
    }

    pub fn task_path(&self, id: &str) -> Result<PathBuf, CronError> {
        let file_name = cron_file_name(id)?;
        Ok(self.root.join(file_name))
    }

    fn cron_files(&self) -> Result<Vec<PathBuf>, CronError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(storage_error(err)),
        };
        let mut files = entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_error)?
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .collect::<Vec<_>>();
        files.sort();
        Ok(files)
    }
}

fn cron_file_name(id: &str) -> Result<String, CronError> {
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(CronError::Storage(Arc::from(format!(
            "invalid cron id '{id}'; expected letters, digits, '-' or '_'"
        ))));
    }
    Ok(format!("{id}.toml"))
}

fn storage_error(err: std::io::Error) -> CronError {
    CronError::Storage(Arc::from(err.to_string()))
}

fn toml_de_error(err: toml::de::Error) -> CronError {
    CronError::Storage(Arc::from(err.to_string()))
}

fn toml_ser_error(err: toml::ser::Error) -> CronError {
    CronError::Storage(Arc::from(err.to_string()))
}

