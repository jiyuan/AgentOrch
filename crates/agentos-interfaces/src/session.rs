use agentos_proto::{ConversationId, Message};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session backend failed: {0}")]
    Backend(Arc<str>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub message: Message,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<Arc<str>, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub items: Vec<Item>,
}

#[async_trait]
pub trait Session: Send + Sync {
    /// Load the transcript for a conversation before a run starts.
    ///
    /// Missing conversations should return an empty transcript rather than an
    /// error.
    async fn load(&self, conv_id: &ConversationId) -> Result<Transcript, SessionError>;

    /// Append items after a run progresses.
    ///
    /// Implementations must preserve item ordering exactly as supplied.
    async fn append(&self, conv_id: &ConversationId, items: Vec<Item>) -> Result<(), SessionError>;
}
