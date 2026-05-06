use crate::orchestrator::{SubAgentSpec, SubOrchSpec};
use crate::session::Transcript;
use agentos_proto::{
    AgentId, InterruptionId, RunId, SchemaVersion, TaskId, ToolCall, TraceEvent, TraceSpan, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize)]
pub struct Interruption {
    pub id: InterruptionId,
    pub action: InterruptionAction,
    pub status: ApprovalStatus,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum InterruptionAction {
    ToolCall(ToolCall),
    Delegate(SubAgentSpec),
    Escalate(SubOrchSpec),
    Handoff {
        agent_id: AgentId,
        payload: Option<Value>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected { reason: Arc<str> },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RunState {
    pub run_id: RunId,
    pub active_agent: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_session_id: Option<Arc<str>>,
    pub transcript: Transcript,
    pub pending_approvals: Vec<Interruption>,
    pub usage: Usage,
    pub version: SchemaVersion,
    #[serde(default)]
    pub trace_spans: Vec<TraceSpan>,
    #[serde(default)]
    pub trace_events: Vec<TraceEvent>,
}

impl RunState {
    pub fn new(run_id: RunId, active_agent: AgentId) -> Self {
        Self {
            run_id,
            active_agent,
            task_id: None,
            task_session_id: None,
            transcript: Transcript::default(),
            pending_approvals: Vec::new(),
            usage: Usage::default(),
            version: SchemaVersion::default(),
            trace_spans: Vec::new(),
            trace_events: Vec::new(),
        }
    }

    pub fn approve(&mut self, id: &InterruptionId) -> bool {
        for interruption in &mut self.pending_approvals {
            if &interruption.id == id {
                interruption.status = ApprovalStatus::Approved;
                return true;
            }
        }
        false
    }

    pub fn reject(&mut self, id: &InterruptionId, reason: impl Into<Arc<str>>) -> bool {
        let reason = reason.into();
        for interruption in &mut self.pending_approvals {
            if &interruption.id == id {
                interruption.status = ApprovalStatus::Rejected {
                    reason: Arc::clone(&reason),
                };
                return true;
            }
        }
        false
    }

    pub fn take_approved_action(&mut self) -> Option<InterruptionAction> {
        let index = self
            .pending_approvals
            .iter()
            .position(|interruption| interruption.status == ApprovalStatus::Approved)?;
        Some(self.pending_approvals.remove(index).action)
    }

    pub fn take_approved_tool_call(&mut self) -> Option<ToolCall> {
        match self.take_approved_action()? {
            InterruptionAction::ToolCall(call) => Some(call),
            InterruptionAction::Delegate(_)
            | InterruptionAction::Escalate(_)
            | InterruptionAction::Handoff { .. } => None,
        }
    }

    pub fn take_rejected_reason(&mut self) -> Option<Arc<str>> {
        let index = self.pending_approvals.iter().position(|interruption| {
            matches!(interruption.status, ApprovalStatus::Rejected { .. })
        })?;
        let interruption = self.pending_approvals.remove(index);
        match interruption.status {
            ApprovalStatus::Rejected { reason } => Some(reason),
            ApprovalStatus::Pending | ApprovalStatus::Approved => None,
        }
    }
}
