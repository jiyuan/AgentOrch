use crate::ids::{ChannelId, ConversationId};
use crate::message::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub channel_id: ChannelId,
    pub conversation_id: ConversationId,
    pub sender: Arc<str>,
    pub message: Message,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<Arc<str>, Value>,
}
