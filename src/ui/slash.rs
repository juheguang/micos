use crate::config::SessionConfig;
use crate::session::SESSION_DIR;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::Path;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlashCommand {
    Help,
    Status,
    Sessions,
    Transcript,
    Model,
    Clear,
    Exit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlashCommandInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub command: SlashCommand,
}

pub const SLASH_COMMANDS: &[SlashCommandInfo] = &[
    SlashCommandInfo {
        name: "help",
        description: "show this help",
        command: SlashCommand::Help,
    },
    SlashCommandInfo {
        name: "status",
        description: "show model, API, permission, cwd, and log path",
        command: SlashCommand::Status,
    },
    SlashCommandInfo {
        name: "sessions",
        description: "list recent session logs",
        command: SlashCommand::Sessions,
    },
    SlashCommandInfo {
        name: "transcript",
        description: "show current transcript and recent events",
        command: SlashCommand::Transcript,
    },
    SlashCommandInfo {
        name: "model",
        description: "choose model and thinking settings",
        command: SlashCommand::Model,
    },
    SlashCommandInfo {
        name: "clear",
        description: "clear the terminal",
        command: SlashCommand::Clear,
    },
    SlashCommandInfo {
        name: "exit",
        description: "exit",
        command: SlashCommand::Exit,
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputCommand {
    Slash(SlashCommand),
    UnknownSlash(String),
    UserText(String),
    Empty,
}

pub fn parse_input(input: &str) -> InputCommand {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return InputCommand::Empty;
    }
    if !trimmed.starts_with('/') {
        return InputCommand::UserText(trimmed.to_string());
    }
    match slash_command_exact(trimmed) {
        Some(command) => InputCommand::Slash(command),
        None => InputCommand::UnknownSlash(trimmed.to_string()),
    }
}

pub fn slash_command_exact(input: &str) -> Option<SlashCommand> {
    let name = input.strip_prefix('/')?;
    if name == "quit" {
        return Some(SlashCommand::Exit);
    }
    SLASH_COMMANDS
        .iter()
        .find(|command| command.name == name)
        .map(|command| command.command)
}

pub fn slash_command_matches(input: &str) -> Vec<SlashCommandInfo> {
    let Some(prefix) = input.strip_prefix('/') else {
        return Vec::new();
    };
    SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|command| command.name.starts_with(prefix))
        .collect()
}

pub fn format_help() -> String {
    let mut lines = vec!["Commands:".to_string()];
    for command in SLASH_COMMANDS {
        lines.push(format!("  /{:<11} {}", command.name, command.description));
    }
    lines.join("\n")
}

pub fn format_status(config: &SessionConfig, session_id: Uuid, session_path: &Path) -> String {
    [
        format!("session: {session_id}"),
        format!("model: {}", config.model),
        format!("api kind: {}", config.api_kind),
        format!("base url: {}", config.base_url),
        format!(
            "thinking: {}",
            config.thinking.map_or("unset".into(), |v| v.to_string())
        ),
        format!(
            "reasoning effort: {}",
            config
                .reasoning_effort
                .map_or("unset".into(), |v| v.to_string())
        ),
        format!("permission: {}", config.permission),
        format!("cwd: {}", config.cwd.display()),
        format!("log: {}", session_path.display()),
    ]
    .join("\n")
}

pub fn format_sessions(cwd: &Path) -> Result<String> {
    let dir = cwd.join(SESSION_DIR);
    if !dir.exists() {
        return Ok(format!("No sessions found at {}", dir.display()));
    }

    let mut entries = fs::read_dir(&dir)
        .with_context(|| format!("read session directory {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            Some((entry.path(), metadata.modified().ok()?))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    if entries.is_empty() {
        return Ok(format!("No sessions found at {}", dir.display()));
    }

    Ok(entries
        .into_iter()
        .take(10)
        .map(|(path, modified)| format!("{}  {}", humantime(modified), path.display()))
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn format_transcript(path: &Path) -> Result<String> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read transcript {}", path.display()))?;
    let lines = text.lines().rev().take(8).collect::<Vec<_>>();
    if lines.is_empty() {
        return Ok(format!(
            "transcript: {}\nNo events recorded.",
            path.display()
        ));
    }
    let mut output = vec![
        format!("transcript: {}", path.display()),
        "recent events:".into(),
    ];
    for line in lines.into_iter().rev() {
        output.push(format!("  {}", summarize_json_line(line)));
    }
    Ok(output.join("\n"))
}

fn trim_one_line(text: &str, max_chars: usize) -> String {
    let mut compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > max_chars {
        compact = compact.chars().take(max_chars).collect::<String>();
        compact.push_str("...");
    }
    compact
}

fn summarize_json_line(line: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return trim_one_line(line, 180);
    };
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("event");
    match event_type {
        "user_input" => format!(
            "user_input: {}",
            trim_one_line(value.get("text").and_then(Value::as_str).unwrap_or(""), 120)
        ),
        "assistant_text" | "assistant_delta" => format!(
            "{event_type}: {}",
            trim_one_line(value.get("text").and_then(Value::as_str).unwrap_or(""), 120)
        ),
        "tool_call" | "tool_started" => format!(
            "{event_type}: {}",
            value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ),
        "tool_output" | "tool_finished" => format!(
            "{event_type}: success={}",
            value
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        ),
        "stop" => format!(
            "stop: {}",
            value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ),
        _ => event_type.to_string(),
    }
}

fn humantime(time: std::time::SystemTime) -> String {
    match time.elapsed() {
        Ok(elapsed) if elapsed.as_secs() < 60 => format!("{:>4}s ago", elapsed.as_secs()),
        Ok(elapsed) if elapsed.as_secs() < 3600 => format!("{:>4}m ago", elapsed.as_secs() / 60),
        Ok(elapsed) if elapsed.as_secs() < 86_400 => {
            format!("{:>4}h ago", elapsed.as_secs() / 3600)
        }
        Ok(elapsed) => format!("{:>4}d ago", elapsed.as_secs() / 86_400),
        Err(_) => "   now".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_slash_commands_and_user_text() {
        assert_eq!(parse_input(""), InputCommand::Empty);
        assert_eq!(parse_input("hello"), InputCommand::UserText("hello".into()));
        assert_eq!(
            parse_input("/help"),
            InputCommand::Slash(SlashCommand::Help)
        );
        assert_eq!(
            parse_input("/missing"),
            InputCommand::UnknownSlash("/missing".into())
        );
    }

    #[test]
    fn slash_catalog_order_and_matching_are_shared() {
        let names = SLASH_COMMANDS
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "help",
                "status",
                "sessions",
                "transcript",
                "model",
                "clear",
                "exit"
            ]
        );
        assert_eq!(slash_command_exact("/status"), Some(SlashCommand::Status));
        assert_eq!(slash_command_exact("/quit"), Some(SlashCommand::Exit));
        assert_eq!(
            slash_command_matches("/sta")
                .into_iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            vec!["status"]
        );
        assert!(slash_command_matches("/missing").is_empty());
    }
}
