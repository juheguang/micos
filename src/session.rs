use crate::config::{ApiKind, PermissionMode, ReasoningEffort, SessionConfig, ThinkingMode};
use crate::context::ContextCategory;
use crate::prompt::PromptSectionSnapshot;
use crate::tools::{DecisionReason, PermissionDecision, RuleSource};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

pub const SESSION_DIR: &str = ".micos/sessions";

#[derive(Clone, Debug)]
pub struct Session {
    id: Uuid,
    path: PathBuf,
}

pub trait SessionStore {
    fn id(&self) -> Uuid;
    fn path(&self) -> &PathBuf;
    fn append(&self, event: &SessionEvent) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    FinalAnswer,
    MaxSteps,
    UserExit,
    UserInterrupt,
    ToolDenied,
    ToolError,
    ApiError,
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopReason::FinalAnswer => write!(f, "final_answer"),
            StopReason::MaxSteps => write!(f, "max_steps"),
            StopReason::UserExit => write!(f, "user_exit"),
            StopReason::UserInterrupt => write!(f, "user_interrupt"),
            StopReason::ToolDenied => write!(f, "tool_denied"),
            StopReason::ToolError => write!(f, "tool_error"),
            StopReason::ApiError => write!(f, "api_error"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    SessionStart {
        timestamp: String,
        session_id: Uuid,
        api_kind: ApiKind,
        model: String,
        base_url: String,
        thinking: Option<ThinkingMode>,
        reasoning_effort: Option<ReasoningEffort>,
        permission: PermissionMode,
        cwd: PathBuf,
    },
    SessionResumed {
        timestamp: String,
        source_session_id: Option<Uuid>,
        source_path: PathBuf,
        restored_messages: usize,
        used_summary: bool,
        restored_tail_messages: usize,
        estimated_tokens: usize,
    },
    ContextSnapshot {
        timestamp: String,
        model: String,
        estimated_tokens: usize,
        max_tokens: usize,
        usage_percent: usize,
        categories: Vec<ContextCategory>,
        #[serde(default)]
        prompt_sections: Vec<PromptSectionSnapshot>,
    },
    ContextCompacted {
        timestamp: String,
        before_tokens: usize,
        after_tokens: usize,
        summary_tokens: usize,
        messages_replaced: usize,
        #[serde(default)]
        retained_messages: usize,
        #[serde(default)]
        compression_ratio_percent: usize,
        #[serde(default)]
        validation_status: String,
    },
    ContextSummary {
        timestamp: String,
        summary: String,
        summary_tokens: usize,
        messages_replaced: usize,
        #[serde(default)]
        retained_messages: usize,
        #[serde(default)]
        summary_format_version: usize,
        trigger: String,
    },
    ConfigChanged {
        timestamp: String,
        model: String,
        base_url: String,
        thinking: Option<ThinkingMode>,
        reasoning_effort: Option<ReasoningEffort>,
    },
    UserInput {
        timestamp: String,
        text: String,
    },
    AssistantText {
        timestamp: String,
        text: String,
    },
    ToolCall {
        timestamp: String,
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolOutput {
        timestamp: String,
        call_id: String,
        success: bool,
        output: String,
        error: Option<String>,
    },
    AssistantDelta {
        timestamp: String,
        text: String,
    },
    ReasoningDelta {
        timestamp: String,
        text: String,
    },
    ToolStarted {
        timestamp: String,
        call_id: String,
        name: String,
        arguments: serde_json::Value,
        permission: PermissionMode,
    },
    PermissionDecision {
        timestamp: String,
        call_id: String,
        tool: String,
        argument_summary: String,
        decision: PermissionDecision,
        reason: DecisionReason,
        rule_source: Option<RuleSource>,
        permission_mode: PermissionMode,
        elapsed_ms: u128,
        #[serde(default)]
        message: Option<String>,
    },
    ToolFinished {
        timestamp: String,
        call_id: String,
        name: String,
        success: bool,
        output: String,
        error: Option<String>,
        elapsed_ms: u128,
    },
    PermissionDenied {
        timestamp: String,
        call_id: String,
        name: String,
        reason: String,
    },
    Error {
        timestamp: String,
        message: String,
    },
    Stop {
        timestamp: String,
        reason: StopReason,
    },
}

impl Session {
    pub fn new(config: &SessionConfig) -> Result<Self> {
        let id = Uuid::new_v4();
        let dir = config.cwd.join(SESSION_DIR);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create session directory {}", dir.display()))?;
        let path = dir.join(format!("{id}.jsonl"));
        let session = Self { id, path };
        session.append(&SessionEvent::SessionStart {
            timestamp: now(),
            session_id: id,
            api_kind: config.api_kind,
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            thinking: config.thinking,
            reasoning_effort: config.reasoning_effort,
            permission: config.permission,
            cwd: config.cwd.clone(),
        })?;
        Ok(session)
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn append(&self, event: &SessionEvent) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("open session log {}", self.path.display()))?;
        serde_json::to_writer(&mut file, event).context("serialize session event")?;
        file.write_all(b"\n").context("write session event")?;
        Ok(())
    }
}

impl SessionStore for Session {
    fn id(&self) -> Uuid {
        self.id()
    }

    fn path(&self) -> &PathBuf {
        self.path()
    }

    fn append(&self, event: &SessionEvent) -> Result<()> {
        self.append(event)
    }
}

pub fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_compacted_event_serializes_with_expected_fields() {
        let event = SessionEvent::ContextCompacted {
            timestamp: "2026-05-27T00:00:00Z".into(),
            before_tokens: 100,
            after_tokens: 40,
            summary_tokens: 20,
            messages_replaced: 3,
            retained_messages: 2,
            compression_ratio_percent: 60,
            validation_status: "passed".into(),
        };

        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "context_compacted");
        assert_eq!(value["before_tokens"], 100);
        assert_eq!(value["after_tokens"], 40);
        assert_eq!(value["summary_tokens"], 20);
        assert_eq!(value["messages_replaced"], 3);
        assert_eq!(value["retained_messages"], 2);
        assert_eq!(value["compression_ratio_percent"], 60);
        assert_eq!(value["validation_status"], "passed");
    }

    #[test]
    fn session_resumed_event_serializes_with_expected_fields() {
        let source_session_id = Uuid::new_v4();
        let event = SessionEvent::SessionResumed {
            timestamp: "2026-05-27T00:00:00Z".into(),
            source_session_id: Some(source_session_id),
            source_path: PathBuf::from(".micos/sessions/source.jsonl"),
            restored_messages: 9,
            used_summary: true,
            restored_tail_messages: 8,
            estimated_tokens: 123,
        };

        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "session_resumed");
        assert_eq!(value["source_session_id"], source_session_id.to_string());
        assert_eq!(value["source_path"], ".micos/sessions/source.jsonl");
        assert_eq!(value["restored_messages"], 9);
        assert_eq!(value["used_summary"], true);
        assert_eq!(value["restored_tail_messages"], 8);
        assert_eq!(value["estimated_tokens"], 123);
    }

    #[test]
    fn context_summary_event_serializes_with_expected_fields() {
        let event = SessionEvent::ContextSummary {
            timestamp: "2026-05-27T00:00:00Z".into(),
            summary: "## Primary Request and Intent\nContinue.".into(),
            summary_tokens: 12,
            messages_replaced: 4,
            retained_messages: 2,
            summary_format_version: 1,
            trigger: "manual".into(),
        };

        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "context_summary");
        assert_eq!(value["summary"], "## Primary Request and Intent\nContinue.");
        assert_eq!(value["summary_tokens"], 12);
        assert_eq!(value["messages_replaced"], 4);
        assert_eq!(value["retained_messages"], 2);
        assert_eq!(value["summary_format_version"], 1);
        assert_eq!(value["trigger"], "manual");
    }

    #[test]
    fn context_snapshot_event_serializes_prompt_sections() {
        let event = SessionEvent::ContextSnapshot {
            timestamp: "2026-05-27T00:00:00Z".into(),
            model: "mock".into(),
            estimated_tokens: 20,
            max_tokens: 100,
            usage_percent: 20,
            categories: vec![ContextCategory {
                name: "prompt.identity".into(),
                tokens: 10,
            }],
            prompt_sections: vec![PromptSectionSnapshot {
                id: "identity".into(),
                title: "Identity".into(),
                source: "base".into(),
                tokens_estimate: 8,
            }],
        };

        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "context_snapshot");
        assert_eq!(value["prompt_sections"][0]["id"], "identity");
        assert_eq!(value["prompt_sections"][0]["source"], "base");
    }
}
