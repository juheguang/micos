use crate::agent::ContextCompactReport;
use crate::config::SessionConfig;
use crate::context::ContextStats;
use crate::memory::{MemoryCandidateReport, MemoryEntry, ProjectMemory};
use crate::plan::{ActivePlan, HandoffReport};
use crate::prompt::PromptBuild;
use crate::recovery::RecoveryReport;
use crate::session::SESSION_DIR;
use crate::session_replay::SessionResumeReport;
use crate::verify::VerificationRunReport;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlashCommand {
    Help,
    Status,
    Permission,
    Sessions,
    Transcript,
    Summary,
    Trace,
    Prompt,
    Context,
    Compact,
    Verify,
    Resume,
    Memory,
    MemorySweep,
    Handoff,
    Recover,
    Plan,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionChoice {
    pub id: String,
    pub path: PathBuf,
    pub modified: SystemTime,
}

impl SessionChoice {
    pub fn label(&self) -> String {
        self.id.clone()
    }

    pub fn description(&self) -> String {
        format!("{}  {}", humantime(self.modified), self.path.display())
    }
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
        name: "permission",
        description: "show or set permission mode: safe, ask, or auto",
        command: SlashCommand::Permission,
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
        name: "summary",
        description: "show latest compact summary",
        command: SlashCommand::Summary,
    },
    SlashCommandInfo {
        name: "trace",
        description: "show recent tool and permission trace",
        command: SlashCommand::Trace,
    },
    SlashCommandInfo {
        name: "prompt",
        description: "show prompt sections and estimated tokens",
        command: SlashCommand::Prompt,
    },
    SlashCommandInfo {
        name: "context",
        description: "show estimated context usage",
        command: SlashCommand::Context,
    },
    SlashCommandInfo {
        name: "compact",
        description: "summarize and replace current model-visible context",
        command: SlashCommand::Compact,
    },
    SlashCommandInfo {
        name: "verify",
        description: "run configured verification checks",
        command: SlashCommand::Verify,
    },
    SlashCommandInfo {
        name: "resume",
        description: "restore model-visible context from a session",
        command: SlashCommand::Resume,
    },
    SlashCommandInfo {
        name: "memory",
        description: "show project memory, candidates, and accepted entries",
        command: SlashCommand::Memory,
    },
    SlashCommandInfo {
        name: "memory sweep",
        description: "scan for stale and duplicated memory entries",
        command: SlashCommand::MemorySweep,
    },
    SlashCommandInfo {
        name: "handoff",
        description: "write current active handoff to .micos/plans/active.md",
        command: SlashCommand::Handoff,
    },
    SlashCommandInfo {
        name: "recover",
        description: "write and show latest recovery report",
        command: SlashCommand::Recover,
    },
    SlashCommandInfo {
        name: "plan",
        description: "show current active plan handoff",
        command: SlashCommand::Plan,
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
    Slash(SlashInvocation),
    UnknownSlash(String),
    UserText(String),
    Empty,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlashInvocation {
    pub command: SlashCommand,
    pub args: String,
}

pub fn parse_input(input: &str) -> InputCommand {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return InputCommand::Empty;
    }
    if !trimmed.starts_with('/') {
        return InputCommand::UserText(trimmed.to_string());
    }
    match slash_invocation(trimmed) {
        Some(invocation) => InputCommand::Slash(invocation),
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

pub fn slash_invocation(input: &str) -> Option<SlashInvocation> {
    let body = input.strip_prefix('/')?;
    let mut parts = body.splitn(2, char::is_whitespace);
    let name = parts.next()?;
    let args = parts.next().unwrap_or_default().trim().to_string();
    let command = if name == "quit" {
        SlashCommand::Exit
    } else {
        SLASH_COMMANDS
            .iter()
            .find(|command| command.name == name)
            .map(|command| command.command)?
    };
    Some(SlashInvocation { command, args })
}

pub fn slash_command_matches(input: &str) -> Vec<SlashCommandInfo> {
    let Some(prefix) = input.strip_prefix('/') else {
        return Vec::new();
    };
    let prefix = prefix.split_whitespace().next().unwrap_or(prefix);
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
        format!(
            "context window: {} tokens",
            format_tokens(config.context_window_tokens)
        ),
        format!("context warning: {}%", config.context_warning_percent),
        format!("cwd: {}", config.cwd.display()),
        format!("log: {}", session_path.display()),
    ]
    .join("\n")
}

pub fn format_sessions(cwd: &Path) -> Result<String> {
    let choices = recent_session_choices(cwd, 10)?;
    let dir = cwd.join(SESSION_DIR);
    if !dir.exists() {
        return Ok(format!("No sessions found at {}", dir.display()));
    }

    if choices.is_empty() {
        return Ok(format!("No sessions found at {}", dir.display()));
    }

    Ok(choices
        .into_iter()
        .map(|choice| {
            format!(
                "{}  {}  {}",
                humantime(choice.modified),
                choice.id,
                choice.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn recent_session_choices(cwd: &Path, limit: usize) -> Result<Vec<SessionChoice>> {
    let dir = cwd.join(SESSION_DIR);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut choices = fs::read_dir(&dir)
        .with_context(|| format!("read session directory {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                return None;
            }
            let id = entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())?
                .to_string();
            Some(SessionChoice {
                id,
                path: entry.path(),
                modified: metadata.modified().ok()?,
            })
        })
        .collect::<Vec<_>>();
    sort_session_choices(&mut choices);
    choices.truncate(limit);
    Ok(choices)
}

fn sort_session_choices(choices: &mut [SessionChoice]) {
    choices.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
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

pub fn format_summary(path: &Path) -> Result<String> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read summary {}", path.display()))?;
    for line in text.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("context_summary") {
            continue;
        }
        let summary = value
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if summary.is_empty() {
            continue;
        }
        return Ok(summary.to_string());
    }
    Ok("No compact summary recorded.".into())
}

pub fn format_trace(path: &Path) -> Result<String> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read trace {}", path.display()))?;
    let lines = text
        .lines()
        .filter_map(summarize_trace_line)
        .rev()
        .take(12)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return Ok(format!(
            "trace: {}\nNo trace events recorded.",
            path.display()
        ));
    }
    let mut output = vec![format!("trace: {}", path.display()), "recent trace:".into()];
    for line in lines.into_iter().rev() {
        output.push(format!("  {line}"));
    }
    Ok(output.join("\n"))
}

pub fn format_context(config: &SessionConfig, session_path: &Path, stats: &ContextStats) -> String {
    let mut output = vec![
        format!("context: {}", session_path.display()),
        format!("model: {}", config.model),
        format!(
            "estimated tokens: {} / {} ({}%)",
            format_tokens(stats.total_tokens_estimate),
            format_tokens(stats.max_tokens),
            stats.usage_percent
        ),
        format!(
            "pressure: {} (warning at {}%)",
            if stats.usage_percent >= config.context_warning_percent {
                "warning"
            } else {
                "ok"
            },
            config.context_warning_percent
        ),
        "categories:".into(),
    ];
    for category in &stats.categories {
        if category.tokens == 0 {
            continue;
        }
        output.push(format!(
            "  {:<28} {}",
            category.name,
            format_tokens(category.tokens)
        ));
    }
    if !stats.layers.is_empty() {
        output.push("layers:".into());
        for layer in &stats.layers {
            if layer.tokens == 0 {
                continue;
            }
            let pct = if stats.total_tokens_estimate > 0 {
                (layer.tokens as f64 / stats.total_tokens_estimate as f64 * 100.0) as usize
            } else {
                0
            };
            output.push(format!(
                "  {:<22} {:>6} tokens  {}%",
                layer.name,
                format_tokens(layer.tokens),
                pct
            ));
        }
    }
    output.push(format!(
        "largest risk: {}",
        largest_context_risk(stats).unwrap_or("none")
    ));
    output.join("\n")
}

pub fn format_prompt(prompt: &PromptBuild) -> String {
    let mut output = vec![
        "prompt: current model instructions".to_string(),
        format!(
            "estimated tokens: {}",
            format_tokens(crate::context::estimate_text_tokens(&prompt.instructions))
        ),
        "sections:".into(),
    ];
    for section in &prompt.sections {
        output.push(format!(
            "  {:<22} {:<28} {:>7}",
            section.id,
            section.source,
            format_tokens(section.tokens_estimate)
        ));
    }
    output.join("\n")
}

pub fn format_compact_report(report: &ContextCompactReport) -> String {
    if !report.compacted {
        return "nothing to compact".to_string();
    }

    [
        "context compacted".to_string(),
        format!("messages replaced: {}", report.messages_replaced),
        format!("messages retained: {}", report.retained_messages),
        format!(
            "tokens: {} -> {}",
            format_tokens(report.before_tokens),
            format_tokens(report.after_tokens)
        ),
        format!("compression ratio: {}%", report.compression_ratio_percent),
        format!("summary tokens: {}", format_tokens(report.summary_tokens)),
        format!("validation: {}", report.validation_status),
    ]
    .join("\n")
}

pub fn format_resume_report(report: &SessionResumeReport) -> String {
    [
        "session resumed".to_string(),
        format!(
            "source session: {}",
            report
                .source_session_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "unknown".into())
        ),
        format!("source log: {}", report.source_path.display()),
        format!(
            "restore mode: {}",
            if report.used_summary {
                "compact_summary_tail"
            } else {
                "event_replay"
            }
        ),
        format!("restored messages: {}", report.restored_messages),
        format!("used summary: {}", report.used_summary),
        format!("tail messages: {}", report.restored_tail_messages),
        format!(
            "estimated tokens: {}",
            format_tokens(report.estimated_tokens)
        ),
    ]
    .join("\n")
}

pub fn format_verification_report(report: &VerificationRunReport) -> String {
    if report.checks.is_empty() {
        return "no verification checks ran".into();
    }
    let passed = report.checks.iter().filter(|check| check.success).count();
    let mut lines = vec![format!(
        "verification: {passed}/{} passed",
        report.checks.len()
    )];
    for check in &report.checks {
        lines.push(format!(
            "{}: {} exit={} elapsed={}ms{}",
            check.name,
            if check.success { "passed" } else { "failed" },
            check
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".into()),
            check.elapsed_ms,
            if check.truncated {
                " truncated=true"
            } else {
                ""
            }
        ));
        if !check.output_preview.trim().is_empty() {
            lines.push(format!("  {}", trim_one_line(&check.output_preview, 180)));
        }
    }
    lines.join("\n")
}

pub fn format_memory(memory: &ProjectMemory) -> String {
    let mut output = vec![
        "project memory".to_string(),
        format!("root: {}", memory.root.display()),
        format!("index: {}", memory.index_path.display()),
        format!("index tokens: {}", format_tokens(memory.index_tokens)),
        format!(
            "entries: {} active={} tokens={}",
            memory.entries.len(),
            memory.active_entries().len(),
            format_tokens(memory.active_entries_tokens)
        ),
        format!("candidates: {}", memory.pending_candidates().len()),
        format!("topics: {}", memory.topics.len()),
    ];
    if memory.topics.is_empty() {
        output.push("topic list: none".into());
    } else {
        output.push("topic list:".into());
        for topic in &memory.topics {
            output.push(format!(
                "  {:<24} {:<40} {} bytes",
                topic.file_name, topic.title, topic.bytes
            ));
        }
    }
    output.push(
        "usage: /memory index | /memory candidates | /memory candidates refresh | /memory promote <id> | /memory stale <id> | /memory forget <id> | /memory <topic-file.md>"
            .into(),
    );
    output.join("\n")
}

pub fn format_memory_index(memory: &ProjectMemory) -> String {
    memory
        .active_index_text()
        .unwrap_or("Project memory index is empty.")
        .to_string()
}

pub fn format_memory_candidates(memory: &ProjectMemory) -> String {
    let candidates = memory.pending_candidates();
    if candidates.is_empty() {
        return "memory candidates: none".into();
    }
    let mut output = vec!["memory candidates:".to_string()];
    for candidate in candidates {
        let type_tag = candidate
            .memory_type
            .as_ref()
            .map(|t| format!("[{t}] "))
            .unwrap_or_default();
        output.push(format!(
            "  {:<32} {:<9} {}{}",
            candidate.id,
            candidate.status,
            type_tag,
            trim_one_line(&candidate.title, 80)
        ));
    }
    output.join("\n")
}

pub fn format_memory_candidate_report(report: &MemoryCandidateReport) -> String {
    [
        if report.created {
            "memory candidate created".to_string()
        } else {
            "memory candidate refreshed".to_string()
        },
        format!("id: {}", report.candidate.id),
        format!("title: {}", report.candidate.title),
        format!(
            "source session: {}",
            report
                .candidate
                .source_session
                .as_deref()
                .unwrap_or("unknown")
        ),
    ]
    .join("\n")
}

pub fn format_memory_sweep(stale: &[MemoryEntry], stale_days: u64) -> String {
    if stale.is_empty() {
        return format!("memory sweep: all entries fresh (stale threshold: {stale_days} days)");
    }
    let mut lines = vec![format!(
        "memory sweep: {} stale entries (≥{stale_days} days since last validation):",
        stale.len()
    )];
    for entry in stale {
        let last = entry
            .last_validated_at
            .as_deref()
            .unwrap_or("unknown");
        lines.push(format!("  {} — last validated {}", entry.title, last));
    }
    lines.push("Use /memory stale <id> to mark as stale, /memory forget <id> to remove.".into());
    lines.join("\n")
}

pub fn format_memory_entry(entry: &MemoryEntry, action: &str) -> String {
    let type_line = entry
        .memory_type
        .as_ref()
        .map(|t| format!("type: {t}\n"))
        .unwrap_or_default();
    [
        format!("memory {action}"),
        format!("id: {}", entry.id),
        format!("status: {}", entry.status),
        format!("{type_line}title: {}", entry.title),
    ]
    .join("\n")
}

pub fn format_active_plan(plan: Option<&ActivePlan>) -> String {
    plan.and_then(ActivePlan::active_text)
        .unwrap_or("No active plan recorded.")
        .to_string()
}

pub fn format_handoff_report(report: &HandoffReport) -> String {
    [
        "handoff written".to_string(),
        format!("path: {}", report.path.display()),
        format!("trigger: {}", report.trigger),
        format!("files touched: {}", report.files_touched),
        format!("commands run: {}", report.commands_run),
        format!("verification: {}", report.verification_status),
        format!("known failures: {}", report.known_failures),
        format!("tokens: {}", format_tokens(report.tokens_estimate)),
    ]
    .join("\n")
}

pub fn format_recovery_report(report: &RecoveryReport) -> String {
    [
        "recovery report written".to_string(),
        format!("path: {}", report.path.display()),
        format!("trigger: {}", report.trigger),
        format!(
            "stop reason: {}",
            report.stop_reason.as_deref().unwrap_or("unknown")
        ),
        format!("failure class: {}", report.failure_class),
        format!("known failures: {}", report.known_failures),
        format!("tokens: {}", format_tokens(report.tokens_estimate)),
    ]
    .join("\n")
}

fn trim_one_line(text: &str, max_chars: usize) -> String {
    let mut compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > max_chars {
        compact = compact.chars().take(max_chars).collect::<String>();
        compact.push_str("...");
    }
    compact
}

fn json_summary(value: &Value, max_chars: usize) -> String {
    trim_one_line(&value.to_string(), max_chars)
}

fn format_tokens(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn largest_context_risk(stats: &ContextStats) -> Option<&str> {
    stats
        .categories
        .iter()
        .filter(|category| category.name != "free_space" && category.tokens > 0)
        .max_by_key(|category| category.tokens)
        .map(|category| category.name.as_str())
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
        "context_summary" => format!(
            "context_summary: {}",
            summary_heading(value.get("summary").and_then(Value::as_str).unwrap_or(""))
                .unwrap_or("empty")
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

fn summary_heading(summary: &str) -> Option<&str> {
    summary.lines().map(str::trim).find(|line| !line.is_empty())
}

fn summarize_trace_line(line: &str) -> Option<String> {
    let value = match serde_json::from_str::<Value>(line) {
        Ok(value) => value,
        Err(_) => return Some(format!("unparsed: {}", trim_one_line(line, 180))),
    };
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("event");
    match event_type {
        "tool_call" => Some(format!(
            "tool_call: {} args={}",
            value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            value
                .get("arguments")
                .map(|arguments| json_summary(arguments, 120))
                .unwrap_or_else(|| "{}".into())
        )),
        "permission_decision" => Some(format!(
            "permission_decision: {} {} decision={} reason={} source={} mode={} elapsed={}ms{}",
            value
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            trim_one_line(
                value
                    .get("argument_summary")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                80
            ),
            value
                .get("decision")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            value
                .get("rule_source")
                .and_then(Value::as_str)
                .unwrap_or("none"),
            value
                .get("permission_mode")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            value.get("elapsed_ms").and_then(Value::as_u64).unwrap_or(0),
            value
                .get("message")
                .and_then(Value::as_str)
                .map(|message| format!(" message={}", trim_one_line(message, 100)))
                .unwrap_or_default()
        )),
        "tool_finished" => Some(format!(
            "tool_finished: {} success={} elapsed={}ms truncated={} bytes={}/{}",
            value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            value
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            value.get("elapsed_ms").and_then(Value::as_u64).unwrap_or(0),
            value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            value
                .get("preview_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            value
                .get("original_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        )),
        "verification_started" => Some(format!(
            "verification_started: {} command={}",
            value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            trim_one_line(
                value.get("command").and_then(Value::as_str).unwrap_or(""),
                120
            )
        )),
        "verification_finished" => Some(format!(
            "verification_finished: {} success={} exit={} elapsed={}ms truncated={}",
            value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            value
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            value
                .get("exit_code")
                .and_then(Value::as_i64)
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".into()),
            value.get("elapsed_ms").and_then(Value::as_u64).unwrap_or(0),
            value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        )),
        "permission_denied" => Some(format!(
            "permission_denied: {} reason={}",
            value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            trim_one_line(
                value
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("denied"),
                120
            )
        )),
        "stop" => Some(format!(
            "stop: {}",
            value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        )),
        _ => None,
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

    fn invocation(command: SlashCommand, args: &str) -> InputCommand {
        InputCommand::Slash(SlashInvocation {
            command,
            args: args.into(),
        })
    }

    #[test]
    fn parses_slash_commands_and_user_text() {
        assert_eq!(parse_input(""), InputCommand::Empty);
        assert_eq!(parse_input("hello"), InputCommand::UserText("hello".into()));
        assert_eq!(parse_input("/help"), invocation(SlashCommand::Help, ""));
        assert_eq!(
            parse_input("/compact"),
            invocation(SlashCommand::Compact, "")
        );
        assert_eq!(
            parse_input("/summary"),
            invocation(SlashCommand::Summary, "")
        );
        assert_eq!(
            parse_input("/permission auto"),
            invocation(SlashCommand::Permission, "auto")
        );
        assert_eq!(parse_input("/verify"), invocation(SlashCommand::Verify, ""));
        assert_eq!(
            parse_input("/verify test"),
            invocation(SlashCommand::Verify, "test")
        );
        assert_eq!(
            parse_input("/resume 019e6367-bb0e-7ec0-9243-1ac2f75295c4"),
            invocation(SlashCommand::Resume, "019e6367-bb0e-7ec0-9243-1ac2f75295c4")
        );
        assert_eq!(
            parse_input("/memory build.md"),
            invocation(SlashCommand::Memory, "build.md")
        );
        assert_eq!(
            parse_input("/handoff"),
            invocation(SlashCommand::Handoff, "")
        );
        assert_eq!(
            parse_input("/recover"),
            invocation(SlashCommand::Recover, "")
        );
        assert_eq!(parse_input("/plan"), invocation(SlashCommand::Plan, ""));
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
                "permission",
                "sessions",
                "transcript",
                "summary",
                "trace",
                "prompt",
                "context",
                "compact",
                "verify",
                "resume",
                "memory",
                "memory sweep",
                "handoff",
                "recover",
                "plan",
                "model",
                "clear",
                "exit"
            ]
        );
        assert_eq!(slash_command_exact("/status"), Some(SlashCommand::Status));
        assert_eq!(
            slash_command_exact("/permission"),
            Some(SlashCommand::Permission)
        );
        assert_eq!(slash_command_exact("/summary"), Some(SlashCommand::Summary));
        assert_eq!(slash_command_exact("/prompt"), Some(SlashCommand::Prompt));
        assert_eq!(slash_command_exact("/compact"), Some(SlashCommand::Compact));
        assert_eq!(slash_command_exact("/verify"), Some(SlashCommand::Verify));
        assert_eq!(slash_command_exact("/handoff"), Some(SlashCommand::Handoff));
        assert_eq!(slash_command_exact("/recover"), Some(SlashCommand::Recover));
        assert_eq!(slash_command_exact("/plan"), Some(SlashCommand::Plan));
        assert_eq!(slash_command_exact("/quit"), Some(SlashCommand::Exit));
        assert_eq!(slash_command_exact("/resume target"), None);
        assert_eq!(
            slash_command_matches("/sta")
                .into_iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            vec!["status"]
        );
        assert_eq!(
            slash_command_matches("/resume target")
                .into_iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            vec!["resume"]
        );
        assert!(slash_command_matches("/missing").is_empty());
    }

    #[test]
    fn formats_context_stats() {
        let cwd = std::env::temp_dir();
        let config = SessionConfig {
            api_kind: crate::config::ApiKind::Responses,
            model: "mock".into(),
            base_url: crate::config::DEFAULT_RESPONSES_BASE_URL.into(),
            thinking: None,
            reasoning_effort: None,
            permission: crate::config::PermissionMode::Safe,
            permission_rules: Vec::new(),
            max_steps: 3,
            context_window_tokens: 1_000,
            context_warning_percent: crate::config::DEFAULT_CONTEXT_WARNING_PERCENT,
            append_system_prompt: None,
            auto_compact: Default::default(),
            cwd,
        };
        let path = std::env::temp_dir().join(format!("micos-context-{}.jsonl", Uuid::new_v4()));
        let stats = ContextStats {
            total_tokens_estimate: 123,
            max_tokens: 1_000,
            usage_percent: 12,
            categories: vec![
                crate::context::ContextCategory {
                    name: "instructions".into(),
                    tokens: 10,
                },
                crate::context::ContextCategory {
                    name: "tool_outputs".into(),
                    tokens: 80,
                },
                crate::context::ContextCategory {
                    name: "free_space".into(),
                    tokens: 877,
                },
            ],
            layers: Vec::new(),
        };

        let output = format_context(&config, &path, &stats);

        assert!(output.contains("context:"));
        assert!(output.contains("model: mock"));
        assert!(output.contains("estimated tokens: 123 / 1.0k (12%)"));
        assert!(output.contains("tool_outputs"));
        assert!(output.contains("largest risk: tool_outputs"));
    }

    #[test]
    fn formats_prompt_sections() {
        let config = SessionConfig {
            api_kind: crate::config::ApiKind::Responses,
            model: "mock".into(),
            base_url: crate::config::DEFAULT_RESPONSES_BASE_URL.into(),
            thinking: None,
            reasoning_effort: None,
            permission: crate::config::PermissionMode::Safe,
            permission_rules: Vec::new(),
            max_steps: 3,
            context_window_tokens: 1_000,
            context_warning_percent: crate::config::DEFAULT_CONTEXT_WARNING_PERCENT,
            append_system_prompt: Some("Prefer concise replies.".into()),
            auto_compact: Default::default(),
            cwd: std::env::temp_dir(),
        };
        let runtime = crate::prompt::PromptRuntimeContext::from_config(&config);
        let prompt = crate::prompt::PromptBuilder::build(&config, &runtime);

        let output = format_prompt(&prompt);

        assert!(output.contains("prompt: current model instructions"));
        assert!(output.contains("identity"));
        assert!(output.contains("runtime"));
        assert!(output.contains("project_append"));
        assert!(output.contains("config.append_system_prompt"));
    }

    #[test]
    fn formats_resume_report() {
        let report = SessionResumeReport {
            source_session_id: Some(Uuid::nil()),
            source_path: std::path::PathBuf::from(".micos/sessions/source.jsonl"),
            restored_messages: 3,
            used_summary: true,
            restored_tail_messages: 2,
            estimated_tokens: 1200,
        };

        let output = format_resume_report(&report);

        assert!(output.contains("session resumed"));
        assert!(output.contains("restore mode: compact_summary_tail"));
        assert!(output.contains("restored messages: 3"));
        assert!(output.contains("estimated tokens: 1.2k"));
    }

    #[test]
    fn formats_project_memory() {
        let memory = ProjectMemory {
            root: std::path::PathBuf::from(".micos/memory"),
            index_path: std::path::PathBuf::from(".micos/memory/MEMORY.md"),
            index_text: "# Facts\nUse cargo test.".into(),
            index_tokens: 6,
            topics: vec![crate::memory::MemoryTopic {
                file_name: "build.md".into(),
                path: std::path::PathBuf::from(".micos/memory/topics/build.md"),
                title: "Build".into(),
                bytes: 20,
            }],
            candidates: Vec::new(),
            entries: Vec::new(),
            active_entries_tokens: 0,
            created_index: false,
        };

        let overview = format_memory(&memory);
        assert!(overview.contains("project memory"));
        assert!(overview.contains("build.md"));
        assert!(overview.contains("Build"));
        assert_eq!(format_memory_index(&memory), "# Facts\nUse cargo test.");
    }

    #[test]
    fn session_choices_sort_by_last_modified_descending() {
        let mut choices = vec![
            SessionChoice {
                id: "older".into(),
                path: std::path::PathBuf::from(".micos/sessions/older.jsonl"),
                modified: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
            },
            SessionChoice {
                id: "newer".into(),
                path: std::path::PathBuf::from(".micos/sessions/newer.jsonl"),
                modified: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2),
            },
            SessionChoice {
                id: "same-time".into(),
                path: std::path::PathBuf::from(".micos/sessions/same-time.jsonl"),
                modified: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
            },
        ];

        sort_session_choices(&mut choices);

        assert_eq!(
            choices
                .into_iter()
                .map(|choice| choice.id)
                .collect::<Vec<_>>(),
            vec!["newer", "older", "same-time"]
        );
    }

    #[test]
    fn formats_active_plan_and_handoff_report() {
        let plan = ActivePlan {
            root: std::path::PathBuf::from(".micos/plans"),
            active_path: std::path::PathBuf::from(".micos/plans/active.md"),
            active_text: "# Active Plan\n\n## Next Step\nContinue.".into(),
            active_tokens: 10,
            created_dir: false,
        };
        let report = HandoffReport {
            path: std::path::PathBuf::from(".micos/plans/active.md"),
            trigger: "manual".into(),
            files_touched: 1,
            commands_run: 2,
            verification_status: "unverified".into(),
            known_failures: 0,
            tokens_estimate: 10,
        };

        assert!(format_active_plan(Some(&plan)).contains("## Next Step"));
        assert_eq!(format_active_plan(None), "No active plan recorded.");
        let output = format_handoff_report(&report);
        assert!(output.contains("handoff written"));
        assert!(output.contains("trigger: manual"));
        assert!(output.contains("verification: unverified"));
    }

    #[test]
    fn formats_recovery_report() {
        let report = RecoveryReport {
            path: std::path::PathBuf::from(".micos/recovery/latest.md"),
            trigger: "manual".into(),
            stop_reason: Some("tool_error".into()),
            failure_class: crate::recovery::FailureClass::ToolError,
            known_failures: 2,
            tokens_estimate: 44,
        };

        let output = format_recovery_report(&report);

        assert!(output.contains("recovery report written"));
        assert!(output.contains("failure class: tool_error"));
        assert!(output.contains("known failures: 2"));
    }

    #[test]
    fn formats_trace_events() {
        let path = std::env::temp_dir().join(format!("micos-trace-{}.jsonl", Uuid::new_v4()));
        fs::write(
            &path,
            r#"{"type":"user_input","text":"ignored"}
{"type":"tool_call","call_id":"call_1","name":"shell","arguments":{"command":"rm -rf x"}}
{"type":"permission_decision","call_id":"call_1","tool":"shell","argument_summary":"rm -rf x","decision":"deny","reason":"rule","rule_source":"config","permission_mode":"safe","elapsed_ms":2}
{"type":"tool_finished","call_id":"call_1","name":"shell","success":false,"elapsed_ms":3}
{"type":"permission_denied","call_id":"call_1","name":"shell","reason":"denied by rule"}
{"type":"stop","reason":"tool_denied"}
"#,
        )
        .unwrap();

        let output = format_trace(&path).unwrap();
        fs::remove_file(&path).unwrap();

        assert!(output.contains("trace:"));
        assert!(output.contains("tool_call: shell"));
        assert!(output.contains("permission_decision: shell rm -rf x decision=deny"));
        assert!(output.contains("source=config"));
        assert!(output.contains("permission_denied: shell reason=denied by rule"));
        assert!(output.contains("stop: tool_denied"));
        assert!(!output.contains("call_1"));
        assert!(!output.contains("user_input"));
    }

    #[test]
    fn formats_latest_compact_summary() {
        let path = std::env::temp_dir().join(format!("micos-summary-{}.jsonl", Uuid::new_v4()));
        fs::write(
            &path,
            r###"{"type":"context_summary","timestamp":"t1","summary":"## First\nold","summary_tokens":2,"messages_replaced":1,"trigger":"manual"}
{"type":"context_compacted","timestamp":"t1","before_tokens":10,"after_tokens":5,"summary_tokens":2,"messages_replaced":1}
{"type":"context_summary","timestamp":"t2","summary":"## Primary Request and Intent\nnew\n\n## Next Step\ncontinue","summary_tokens":8,"messages_replaced":3,"trigger":"manual"}
"###,
        )
        .unwrap();

        let output = format_summary(&path).unwrap();
        fs::remove_file(&path).unwrap();

        assert!(output.contains("## Primary Request and Intent"));
        assert!(output.contains("## Next Step"));
        assert!(!output.contains("## First"));
    }

    #[test]
    fn formats_missing_compact_summary() {
        let path = std::env::temp_dir().join(format!("micos-no-summary-{}.jsonl", Uuid::new_v4()));
        fs::write(&path, r#"{"type":"user_input","text":"hello"}"#).unwrap();

        let output = format_summary(&path).unwrap();
        fs::remove_file(&path).unwrap();

        assert_eq!(output, "No compact summary recorded.");
    }

    #[test]
    fn transcript_summarizes_context_summary_without_expanding_body() {
        let path =
            std::env::temp_dir().join(format!("micos-summary-transcript-{}.jsonl", Uuid::new_v4()));
        fs::write(
            &path,
            r###"{"type":"context_summary","timestamp":"t1","summary":"## Primary Request and Intent\nline that should not appear","summary_tokens":8,"messages_replaced":3,"trigger":"manual"}
"###,
        )
        .unwrap();

        let output = format_transcript(&path).unwrap();
        fs::remove_file(&path).unwrap();

        assert!(output.contains("context_summary: ## Primary Request and Intent"));
        assert!(!output.contains("line that should not appear"));
    }

    #[test]
    fn formats_empty_and_malformed_trace() {
        let empty_path =
            std::env::temp_dir().join(format!("micos-empty-trace-{}.jsonl", Uuid::new_v4()));
        fs::write(&empty_path, "").unwrap();
        let empty = format_trace(&empty_path).unwrap();
        fs::remove_file(&empty_path).unwrap();
        assert!(empty.contains("No trace events recorded."));

        let malformed_path =
            std::env::temp_dir().join(format!("micos-bad-trace-{}.jsonl", Uuid::new_v4()));
        fs::write(&malformed_path, "not json\n").unwrap();
        let malformed = format_trace(&malformed_path).unwrap();
        fs::remove_file(&malformed_path).unwrap();
        assert!(malformed.contains("unparsed: not json"));
    }
}
