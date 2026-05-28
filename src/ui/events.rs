use crate::config::PermissionMode;
use crate::session::StopReason;
use crate::tools::ToolResult;
use anyhow::Result;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalDecision {
    AllowOnce,
    AllowSession,
    AllowProject,
    Deny,
}

#[derive(Clone, Debug)]
pub enum PlanApprovalDecision {
    Approve,
    Edit,
    MoreGuidance(String),
}

#[derive(Clone, Debug)]
pub enum AgentEvent {
    TurnStarted {
        input: String,
    },
    AssistantDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallStarted {
        call_id: String,
        name: String,
        arguments: Value,
        permission: PermissionMode,
    },
    ToolCallFinished {
        call_id: String,
        name: String,
        result: ToolResult,
        elapsed: Duration,
    },
    PermissionPrompt {
        name: String,
        summary: String,
    },
    ToolOutputDelta {
        name: String,
        delta: String,
        is_stderr: bool,
    },
    Stop {
        reason: StopReason,
    },
    Error {
        message: String,
    },
}

pub trait UiSink {
    fn on_event(&mut self, event: AgentEvent) -> Result<()>;

    fn approve_tool(&mut self, _name: &str, _summary: &str) -> Result<ApprovalDecision> {
        Ok(ApprovalDecision::Deny)
    }

    fn approve_plan(&mut self, _plan_text: &str) -> Result<PlanApprovalDecision> {
        Ok(PlanApprovalDecision::Approve)
    }

    fn confirm_compact(&mut self, _usage_percent: usize) -> Result<bool> {
        Ok(true)
    }
}

#[derive(Default)]
pub struct NullUi;

impl UiSink for NullUi {
    fn on_event(&mut self, _event: AgentEvent) -> Result<()> {
        Ok(())
    }

    fn approve_tool(&mut self, _name: &str, _summary: &str) -> Result<ApprovalDecision> {
        Ok(ApprovalDecision::Deny)
    }

    fn approve_plan(&mut self, _plan_text: &str) -> Result<PlanApprovalDecision> {
        Ok(PlanApprovalDecision::Approve)
    }
}
