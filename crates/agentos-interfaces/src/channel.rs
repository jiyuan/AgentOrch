use agentos_proto::{ChannelId, Envelope};
use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChannelError {
    #[error("channel backend failed: {0}")]
    Backend(Arc<str>),
}

#[async_trait]
pub trait Channel: Send + Sync {
    /// Return the stable channel identifier used in envelopes and traces.
    fn id(&self) -> ChannelId;

    /// Receive the next inbound envelope.
    ///
    /// Returning `None` means the channel is closed.
    async fn receive(&mut self) -> Option<Envelope>;

    /// Send an outbound envelope.
    async fn send(&self, env: Envelope) -> Result<(), ChannelError>;
}
