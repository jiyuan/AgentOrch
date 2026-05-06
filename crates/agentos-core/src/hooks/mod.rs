use agentos_proto::TraceEvent;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Hooks {
    tx: mpsc::Sender<TraceEvent>,
}

impl Hooks {
    pub fn bounded(capacity: usize) -> (Self, mpsc::Receiver<TraceEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    pub async fn emit(&self, event: TraceEvent) -> Result<(), mpsc::error::SendError<TraceEvent>> {
        self.tx.send(event).await
    }

    pub fn try_emit(&self, event: TraceEvent) {
        let _ = self.tx.try_send(event);
    }
}
