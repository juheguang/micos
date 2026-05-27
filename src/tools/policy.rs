use super::{ModePermissionPolicy, PermissionPolicy, ToolPermissionDecision, ToolSummary};
use crate::config::PermissionMode;
use serde_json::Value;

pub fn permission_decision(
    permission: PermissionMode,
    tool_name: &str,
    arguments: &Value,
) -> ToolPermissionDecision {
    match tool_name {
        "list_files" | "read_file" => ToolPermissionDecision::Allowed,
        "write_file" => match permission {
            PermissionMode::Safe => ToolPermissionDecision::Denied {
                reason: "write_file is denied in safe permission mode".to_string(),
            },
            PermissionMode::Ask => ToolPermissionDecision::NeedsApproval {
                summary: ToolSummary::from_arguments(tool_name, arguments).summary,
            },
            PermissionMode::Auto => ToolPermissionDecision::Allowed,
        },
        "shell" => {
            let command = arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match permission {
                PermissionMode::Safe if is_safe_shell_command(command) => {
                    ToolPermissionDecision::Allowed
                }
                PermissionMode::Safe => ToolPermissionDecision::Denied {
                    reason: format!("shell command denied in safe permission mode: {command}"),
                },
                PermissionMode::Ask => ToolPermissionDecision::NeedsApproval {
                    summary: ToolSummary::from_arguments(tool_name, arguments).summary,
                },
                PermissionMode::Auto => ToolPermissionDecision::Allowed,
            }
        }
        _ => ToolPermissionDecision::Allowed,
    }
}

impl PermissionPolicy for ModePermissionPolicy {
    fn decide(
        &self,
        permission: PermissionMode,
        tool_name: &str,
        arguments: &Value,
    ) -> ToolPermissionDecision {
        permission_decision(permission, tool_name, arguments)
    }
}

pub fn is_safe_shell_command(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() || contains_shell_control(trimmed) {
        return false;
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    match parts.as_slice() {
        ["pwd", ..] | ["ls", ..] | ["cat", ..] | ["head", ..] | ["tail", ..] | ["rg", ..] => true,
        ["find", args @ ..] => !args
            .iter()
            .any(|arg| matches!(*arg, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir")),
        ["git", "status", ..] => true,
        ["git", "diff", args @ ..] => !args
            .iter()
            .any(|arg| *arg == "-o" || arg.starts_with("--output")),
        _ => false,
    }
}

fn contains_shell_control(command: &str) -> bool {
    [">", "<", "|", ";", "&&", "||", "$(", "`"]
        .iter()
        .any(|needle| command.contains(needle))
}
