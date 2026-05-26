use crate::config::{PermissionMode, SessionConfig};
use crate::session::{StopReason, SESSION_DIR};
use crate::tools::{ToolResult, ToolSummary};
use anyhow::{Context, Result};
use console::{style, Term};
use crossterm::{
    execute,
    terminal::{Clear, ClearType},
};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::Value;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

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
    Stop {
        reason: StopReason,
    },
    Error {
        message: String,
    },
}

pub trait UiSink {
    fn on_event(&mut self, event: AgentEvent) -> Result<()>;

    fn approve_tool(&mut self, _name: &str, _summary: &str) -> Result<bool> {
        Ok(false)
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct NullUi;

#[cfg(test)]
impl UiSink for NullUi {
    fn on_event(&mut self, _event: AgentEvent) -> Result<()> {
        Ok(())
    }

    fn approve_tool(&mut self, _name: &str, _summary: &str) -> Result<bool> {
        Ok(false)
    }
}

pub struct ConsoleUi {
    term: Term,
    color: bool,
    plain: bool,
    active_spinner: Option<ProgressBar>,
    assistant_open: bool,
    reasoning_open: bool,
}

impl ConsoleUi {
    pub fn new() -> Self {
        let term = Term::stdout();
        let plain = !term.is_term();
        let color = std::env::var_os("NO_COLOR").is_none() && !plain;
        Self {
            term,
            color,
            plain,
            active_spinner: None,
            assistant_open: false,
            reasoning_open: false,
        }
    }

    pub fn prompt(&self) -> String {
        if self.color {
            format!("{}", style("micos>").cyan().bold())
        } else {
            "micos>".to_string()
        }
    }

    pub fn banner(&mut self, config: &SessionConfig, session_id: Uuid, session_path: &Path) {
        if self.plain {
            println!(
                "micos session {session_id} | api={} | model={} | permission={} | cwd={} | log={}",
                config.api_kind,
                config.model,
                config.permission,
                config.cwd.display(),
                session_path.display()
            );
            println!("Type /help for commands.");
            return;
        }

        let title = self.paint("micos", Paint::AccentBold);
        println!("{title}  local agent harness");
        println!(
            "{} {}  {} {}  {} {}",
            self.paint("model", Paint::Muted),
            config.model,
            self.paint("permission", Paint::Muted),
            config.permission,
            self.paint("cwd", Paint::Muted),
            config.cwd.display()
        );
        println!(
            "{} {}  {} {}",
            self.paint("session", Paint::Muted),
            session_id,
            self.paint("log", Paint::Muted),
            session_path.display()
        );
        println!("{}", self.paint("Type /help for commands.", Paint::Muted));
    }

    pub fn print_help(&mut self) {
        println!("{}", format_help());
    }

    pub fn print_status(&mut self, config: &SessionConfig, session_id: Uuid, session_path: &Path) {
        println!("{}", format_status(config, session_id, session_path));
    }

    pub fn print_sessions(&mut self, cwd: &Path) -> Result<()> {
        println!("{}", format_sessions(cwd)?);
        Ok(())
    }

    pub fn print_transcript(&mut self, path: &Path) -> Result<()> {
        println!("{}", format_transcript(path)?);
        Ok(())
    }

    pub fn clear(&mut self) -> Result<()> {
        if self.plain {
            return Ok(());
        }
        execute!(io::stdout(), Clear(ClearType::All)).context("clear terminal")?;
        self.term.move_cursor_to(0, 0).context("move cursor")?;
        Ok(())
    }

    fn finish_spinner(&mut self) {
        if let Some(spinner) = self.active_spinner.take() {
            spinner.finish_and_clear();
        }
    }

    fn ensure_line_after_stream(&mut self) {
        if self.assistant_open || self.reasoning_open {
            println!();
            self.assistant_open = false;
            self.reasoning_open = false;
        }
    }

    fn paint(&self, text: impl AsRef<str>, paint: Paint) -> String {
        let text = text.as_ref();
        if !self.color {
            return text.to_string();
        }
        match paint {
            Paint::AccentBold => style(text).cyan().bold().to_string(),
            Paint::Muted => style(text).dim().to_string(),
            Paint::Success => style(text).green().to_string(),
            Paint::Error => style(text).red().to_string(),
            Paint::Warn => style(text).yellow().to_string(),
        }
    }
}

impl UiSink for ConsoleUi {
    fn on_event(&mut self, event: AgentEvent) -> Result<()> {
        match event {
            AgentEvent::TurnStarted { input } => {
                self.finish_spinner();
                self.assistant_open = false;
                self.reasoning_open = false;
                if self.plain {
                    println!("user: {input}");
                }
            }
            AgentEvent::AssistantDelta { text } => {
                self.finish_spinner();
                if !self.assistant_open {
                    print!("{}", self.paint("assistant: ", Paint::AccentBold));
                    self.assistant_open = true;
                }
                print!("{text}");
                io::stdout().flush().context("flush assistant delta")?;
            }
            AgentEvent::ReasoningDelta { text } => {
                self.finish_spinner();
                if !self.reasoning_open {
                    print!("{}", self.paint("reasoning: ", Paint::Muted));
                    self.reasoning_open = true;
                }
                if self.color {
                    print!("{}", style(text).dim());
                } else {
                    print!("{text}");
                }
                io::stdout().flush().context("flush reasoning delta")?;
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                arguments,
                permission,
            } => {
                self.ensure_line_after_stream();
                let summary = ToolSummary::from_arguments(&name, &arguments).summary;
                println!(
                    "{} {}  {} {}  {} {}",
                    self.paint("tool", Paint::AccentBold),
                    name,
                    self.paint("permission", Paint::Muted),
                    permission,
                    self.paint("call", Paint::Muted),
                    call_id
                );
                println!("  {summary}");
                if !self.plain {
                    let spinner = ProgressBar::new_spinner();
                    spinner.set_style(
                        ProgressStyle::with_template("{spinner:.cyan} {msg}")
                            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                    );
                    spinner.set_message(format!("running {name}"));
                    spinner.enable_steady_tick(Duration::from_millis(90));
                    self.active_spinner = Some(spinner);
                }
            }
            AgentEvent::ToolCallFinished {
                call_id,
                name,
                result,
                elapsed,
            } => {
                self.finish_spinner();
                let status = if result.success {
                    self.paint("ok", Paint::Success)
                } else if result.denied {
                    self.paint("denied", Paint::Warn)
                } else {
                    self.paint("failed", Paint::Error)
                };
                println!(
                    "{} {}  {}  {} {:.2?}  {} {}",
                    self.paint("tool", Paint::Muted),
                    name,
                    status,
                    self.paint("elapsed", Paint::Muted),
                    elapsed,
                    self.paint("call", Paint::Muted),
                    call_id
                );
                let summary = summarize_result(&result);
                if !summary.is_empty() {
                    println!("  {summary}");
                }
            }
            AgentEvent::PermissionPrompt { name, summary } => {
                self.ensure_line_after_stream();
                println!(
                    "{} {}  {}",
                    self.paint("permission", Paint::Warn),
                    name,
                    summary
                );
            }
            AgentEvent::Stop { reason } => {
                self.finish_spinner();
                self.ensure_line_after_stream();
                if reason != StopReason::FinalAnswer {
                    println!("{} {reason}", self.paint("stopped:", Paint::Warn));
                }
            }
            AgentEvent::Error { message } => {
                self.finish_spinner();
                self.ensure_line_after_stream();
                println!("{} {message}", self.paint("error:", Paint::Error));
            }
        }
        Ok(())
    }

    fn approve_tool(&mut self, name: &str, summary: &str) -> Result<bool> {
        self.on_event(AgentEvent::PermissionPrompt {
            name: name.to_string(),
            summary: summary.to_string(),
        })?;
        print!("Allow? [y/N] ");
        io::stdout().flush().context("flush permission prompt")?;
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("read permission response")?;
        Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
    }
}

enum Paint {
    AccentBold,
    Muted,
    Success,
    Error,
    Warn,
}

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

fn summarize_result(result: &ToolResult) -> String {
    if let Some(error) = &result.error {
        return trim_one_line(error, 240);
    }
    trim_one_line(&result.output, 240)
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
