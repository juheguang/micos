use crate::config::PermissionMode;
use crate::tools::policy::{PermissionDecision, PermissionRule, PolicyDecision, PolicyEngine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;

#[allow(async_fn_in_trait)]
pub trait Tool {
    fn name(&self) -> &'static str;
    fn metadata(&self, arguments: &Value) -> ToolMetadata;
    fn schema(&self) -> Value;
    async fn execute(&self, input: Value, ctx: ToolContext) -> ToolResult;
}

#[allow(async_fn_in_trait)]
pub trait ToolRegistry {
    fn schemas(&self) -> Vec<Value>;
    fn schemas_for_policy(
        &self,
        policy: &dyn PermissionPolicy,
        permission: PermissionMode,
    ) -> Vec<Value>;
    fn metadata(&self, name: &str, arguments: &Value) -> Option<ToolMetadata>;
    async fn execute(&self, name: &str, input: Value, ctx: ToolContext) -> ToolResult;
}

pub trait PermissionPolicy {
    fn decide(
        &self,
        permission: PermissionMode,
        metadata: &ToolMetadata,
        tool_name: &str,
        arguments: &Value,
        cwd: &Path,
    ) -> PolicyDecision;

    fn hides_tool_schema(&self, permission: PermissionMode, tool_name: &str) -> bool;

    fn add_rule(&mut self, _rule: PermissionRule) {}
}

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub permission: PermissionMode,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    Timeout,
    Io,
    Parse,
    Permission,
    ProcessExit,
    Utf8,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    pub error_kind: Option<ToolErrorKind>,
    pub denied: bool,
    pub truncated: bool,
    pub original_bytes: usize,
    pub preview_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolMetadata {
    pub name: &'static str,
    pub read_only: bool,
    pub destructive: bool,
    pub concurrency_safe: bool,
    pub argument_summary: String,
    pub permission_hint: PermissionDecision,
}

impl ToolMetadata {
    pub fn unknown(name: &'static str, arguments: &Value) -> Self {
        Self {
            name,
            read_only: false,
            destructive: false,
            concurrency_safe: false,
            argument_summary: arguments.to_string(),
            permission_hint: PermissionDecision::Allow,
        }
    }
}

pub type ModePermissionPolicy = PolicyEngine;
pub type ToolPermissionDecision = PolicyDecision;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolSummary {
    pub summary: String,
}

impl ToolSummary {
    pub fn from_arguments(name: &str, arguments: &Value) -> Self {
        let summary = match name {
            "list_files" => format!(
                "path={}",
                arguments.get("path").and_then(Value::as_str).unwrap_or(".")
            ),
            "read_file" => format!(
                "path={}",
                arguments
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>")
            ),
            "write_file" => {
                let path = arguments
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>");
                let bytes = arguments
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::len)
                    .unwrap_or(0);
                format!("path={path} bytes={bytes}")
            }
            "shell" => format!(
                "command={}",
                arguments
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>")
            ),
            "grep" => format!(
                "pattern={} path={}",
                arguments
                    .get("pattern")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>"),
                arguments.get("path").and_then(Value::as_str).unwrap_or(".")
            ),
            "edit" => format!(
                "path={}",
                arguments
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>")
            ),
            "glob" => format!(
                "pattern={} root={}",
                arguments
                    .get("pattern")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>"),
                arguments.get("root").and_then(Value::as_str).unwrap_or(".")
            ),
            _ => arguments.to_string(),
        };
        Self { summary }
    }
}

impl From<PolicyDecision> for ToolResult {
    fn from(decision: PolicyDecision) -> Self {
        match decision.decision {
            PermissionDecision::Allow => ToolResult::ok("allowed"),
            PermissionDecision::Ask => ToolResult::error("approval required"),
            PermissionDecision::Deny => ToolResult::denied(decision.message),
        }
    }
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        let output = output.into();
        let bytes = output.len();
        Self {
            success: true,
            output,
            error: None,
            error_kind: None,
            denied: false,
            truncated: false,
            original_bytes: bytes,
            preview_bytes: bytes,
        }
    }

    pub fn ok_with_preview(
        output: impl Into<String>,
        truncated: bool,
        original_bytes: usize,
        preview_bytes: usize,
    ) -> Self {
        Self {
            success: true,
            output: output.into(),
            error: None,
            error_kind: None,
            denied: false,
            truncated,
            original_bytes,
            preview_bytes,
        }
    }

    pub fn error(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error.into()),
            error_kind: None,
            denied: false,
            truncated: false,
            original_bytes: 0,
            preview_bytes: 0,
        }
    }

    pub fn error_with_kind(kind: ToolErrorKind, error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error.into()),
            error_kind: Some(kind),
            denied: false,
            truncated: false,
            original_bytes: 0,
            preview_bytes: 0,
        }
    }

    pub fn denied(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error.into()),
            error_kind: Some(ToolErrorKind::Permission),
            denied: true,
            truncated: false,
            original_bytes: 0,
            preview_bytes: 0,
        }
    }
}
