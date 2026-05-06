use agentos_interfaces::orchestrator::{MemoryFragment, OrchestratorTemplate};
use agentos_proto::TaskId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TaskWorkspaceError {
    #[error("task workspace I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("task workspace TOML failed at {path}: {source}")]
    TomlSer {
        path: PathBuf,
        source: toml::ser::Error,
    },
    #[error("task workspace TOML failed at {path}: {source}")]
    TomlDe {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("task workspace JSON failed at {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("immutable task config already exists at {path}")]
    ImmutableConfig { path: PathBuf },
}

#[derive(Clone, Debug)]
pub struct TaskWorkspace {
    root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskMetadata {
    pub task_id: TaskId,
    pub origin: Arc<str>,
    pub status: Arc<str>,
    pub created_at: Arc<str>,
    pub updated_at: Arc<str>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_step: Option<Arc<str>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fragments: Vec<MemoryFragment>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<Arc<str>, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubAgentWorkspaceConfig {
    pub role: Arc<str>,
    pub instructions: Arc<str>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<Arc<str>>,
}

impl TaskWorkspace {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn task_dir(&self, task_id: &TaskId) -> PathBuf {
        if matches!(task_id.as_str(), "main" | "min") {
            return self
                .root
                .parent()
                .unwrap_or_else(|| self.root())
                .join(task_id.as_str());
        }
        self.root.join(task_id.as_str())
    }

    pub fn init_task(&self, task_id: &TaskId) -> Result<(), TaskWorkspaceError> {
        let dir = self.task_dir(task_id);
        create_dir_all(&dir)?;
        create_dir_all(&dir.join("subagents"))?;
        create_dir_all(&dir.join("suborchestrators"))?;
        create_dir_all(&dir.join("sessions"))?;

        let metadata_path = dir.join("task.toml");
        if !metadata_path.exists() {
            let now = timestamp();
            write_toml(
                &metadata_path,
                &TaskMetadata {
                    task_id: task_id.clone(),
                    origin: Arc::from("run_loop"),
                    status: Arc::from("active"),
                    created_at: Arc::from(now.as_str()),
                    updated_at: Arc::from(now),
                },
            )?;
        }

        let state_path = dir.join("state.toml");
        if !state_path.exists() {
            write_toml(&state_path, &TaskState::default())?;
        }
        Ok(())
    }

    pub fn load_state(&self, task_id: &TaskId) -> Result<Option<TaskState>, TaskWorkspaceError> {
        let path = self.task_dir(task_id).join("state.toml");
        match fs::read_to_string(&path) {
            Ok(input) => toml::from_str(&input)
                .map(Some)
                .map_err(|source| TaskWorkspaceError::TomlDe { path, source }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(TaskWorkspaceError::Io { path, source }),
        }
    }

    pub fn save_state(
        &self,
        task_id: &TaskId,
        state: &TaskState,
    ) -> Result<(), TaskWorkspaceError> {
        write_toml(&self.task_dir(task_id).join("state.toml"), state)
    }

    pub fn create_subagent_config(
        &self,
        task_id: &TaskId,
        name: &str,
        config: &SubAgentWorkspaceConfig,
    ) -> Result<(), TaskWorkspaceError> {
        let dir = self.task_dir(task_id).join("subagents").join(name);
        create_dir_all(&dir)?;
        let path = dir.join("config.toml");
        if path.exists() {
            return Err(TaskWorkspaceError::ImmutableConfig { path });
        }
        write_toml(&path, config)
    }

    pub fn write_suborchestrator_graph(
        &self,
        task_id: &TaskId,
        template: &OrchestratorTemplate,
    ) -> Result<(), TaskWorkspaceError> {
        let dir = self
            .task_dir(task_id)
            .join("suborchestrators")
            .join(template.name.as_ref());
        create_dir_all(&dir)?;
        write_toml(&dir.join("graph.toml"), template)
    }

    pub fn append_session_event(
        &self,
        task_id: &TaskId,
        session_id: &str,
        event: &Value,
    ) -> Result<(), TaskWorkspaceError> {
        let path = self
            .task_dir(task_id)
            .join("sessions")
            .join(format!("{session_id}.jsonl"));
        let encoded = serde_json::to_string(event).map_err(|source| TaskWorkspaceError::Json {
            path: path.clone(),
            source,
        })?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| TaskWorkspaceError::Io {
                path: path.clone(),
                source,
            })?;
        writeln!(file, "{encoded}").map_err(|source| TaskWorkspaceError::Io { path, source })
    }
}

fn create_dir_all(path: &Path) -> Result<(), TaskWorkspaceError> {
    fs::create_dir_all(path).map_err(|source| TaskWorkspaceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_toml<T>(path: &Path, value: &T) -> Result<(), TaskWorkspaceError>
where
    T: Serialize,
{
    let encoded = toml::to_string_pretty(value).map_err(|source| TaskWorkspaceError::TomlSer {
        path: path.to_path_buf(),
        source,
    })?;
    fs::write(path, encoded).map_err(|source| TaskWorkspaceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_owned())
}

