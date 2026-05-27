use crate::context::estimate_text_tokens;
use crate::session::SessionEvent;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub const PLAN_DIR: &str = ".micos/plans";
pub const ACTIVE_PLAN_FILE: &str = "active.md";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivePlan {
    pub root: PathBuf,
    pub active_path: PathBuf,
    pub active_text: String,
    pub active_tokens: usize,
    pub created_dir: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffReport {
    pub path: PathBuf,
    pub trigger: String,
    pub files_touched: usize,
    pub commands_run: usize,
    pub verification_status: String,
    pub known_failures: usize,
    pub tokens_estimate: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffDraft {
    pub current_state: String,
    pub next_step: String,
    pub files_touched: Vec<String>,
    pub commands_run: Vec<String>,
    pub verification_status: String,
    pub known_failures: Vec<String>,
    pub last_updated: String,
}

impl ActivePlan {
    pub fn load_or_init(cwd: impl AsRef<Path>) -> Result<Self> {
        let root = cwd.as_ref().join(PLAN_DIR);
        let created_dir = !root.exists();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("create plan directory {}", root.display()))?;
        let active_path = root.join(ACTIVE_PLAN_FILE);
        let active_text = if active_path.exists() {
            std::fs::read_to_string(&active_path)
                .with_context(|| format!("read active plan {}", active_path.display()))?
        } else {
            String::new()
        };
        let active_tokens = estimate_text_tokens(&active_text);
        Ok(Self {
            root,
            active_path,
            active_text,
            active_tokens,
            created_dir,
        })
    }

    pub fn active_text(&self) -> Option<&str> {
        let text = self.active_text.trim();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

impl HandoffDraft {
    pub fn from_session_log(
        path: impl AsRef<Path>,
        trigger: &str,
        last_updated: String,
    ) -> Result<Self> {
        let events = read_session_values(path)?;
        Ok(Self::from_values(&events, trigger, last_updated))
    }

    pub fn from_events(events: &[SessionEvent], trigger: &str, last_updated: String) -> Self {
        let values = events
            .iter()
            .map(|event| serde_json::to_value(event).expect("session event serializes"))
            .collect::<Vec<_>>();
        Self::from_values(&values, trigger, last_updated)
    }

    fn from_values(events: &[Value], trigger: &str, last_updated: String) -> Self {
        let latest_user = events.iter().rev().find_map(|event| {
            (event_type(event) == Some("user_input"))
                .then(|| json_string(event, "text"))
                .flatten()
                .map(|text| trim_one_line(&text, 220))
        });
        let latest_assistant = events.iter().rev().find_map(|event| {
            (event_type(event) == Some("assistant_text"))
                .then(|| json_string(event, "text"))
                .flatten()
                .map(|text| trim_one_line(&text, 220))
        });
        let latest_stop = events.iter().rev().find_map(|event| {
            (event_type(event) == Some("stop"))
                .then(|| json_string(event, "reason"))
                .flatten()
        });

        let mut files_touched = BTreeSet::new();
        let mut commands_run = Vec::new();
        let mut shell_calls = HashMap::<String, String>::new();
        let mut known_failures = Vec::new();
        let mut latest_verification: Option<(String, bool)> = None;

        for event in events {
            match event_type(event) {
                Some("tool_call" | "tool_started") => {
                    let call_id = json_string(event, "call_id").unwrap_or_default();
                    let name = json_string(event, "name").unwrap_or_default();
                    let arguments = event.get("arguments").unwrap_or(&Value::Null);
                    if name == "write_file" {
                        if let Some(path) = json_string(arguments, "path") {
                            files_touched.insert(path);
                        }
                    }
                    if name == "shell" {
                        if let Some(command) = json_string(arguments, "command") {
                            if !shell_calls.contains_key(&call_id) {
                                shell_calls.insert(call_id, command.clone());
                                commands_run.push(command);
                            }
                        }
                    }
                }
                Some("tool_output" | "tool_finished") => {
                    let call_id = json_string(event, "call_id").unwrap_or_default();
                    let success = event
                        .get("success")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let error = json_string(event, "error");
                    if let Some(command) = shell_calls.get(&call_id) {
                        if is_verification_command(command) {
                            latest_verification = Some((command.clone(), success));
                        }
                    }
                    if !success {
                        let detail = error.as_deref().unwrap_or("tool failed");
                        known_failures
                            .push(format!("tool {call_id}: {}", trim_one_line(detail, 180)));
                    }
                }
                Some("permission_denied") => {
                    let name = json_string(event, "name").unwrap_or_else(|| "unknown".into());
                    let reason = json_string(event, "reason").unwrap_or_else(|| "denied".into());
                    known_failures.push(format!(
                        "permission denied for {name}: {}",
                        trim_one_line(&reason, 180)
                    ));
                }
                Some("verification_finished") => {
                    let command = json_string(event, "command").unwrap_or_else(|| "verify".into());
                    let success = event
                        .get("success")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    latest_verification = Some((command.clone(), success));
                    if !success {
                        known_failures.push(format!("verification failed: {command}"));
                    }
                }
                Some("error") => {
                    let message = json_string(event, "message").unwrap_or_else(|| "unknown".into());
                    known_failures.push(format!("error: {}", trim_one_line(&message, 180)));
                }
                Some("stop") if json_string(event, "reason").as_deref() != Some("final_answer") => {
                    let reason = json_string(event, "reason").unwrap_or_else(|| "unknown".into());
                    known_failures.push(format!("non-final stop: {reason}"));
                }
                _ => {}
            }
        }

        let current_state = current_state(
            trigger,
            latest_user.as_deref(),
            latest_assistant.as_deref(),
            latest_stop.as_deref(),
        );
        let next_step = next_step(trigger, latest_user.as_deref(), &known_failures);
        let verification_status = match latest_verification {
            Some((command, true)) => format!("verified: {command}"),
            Some((command, false)) => format!("failed: {command}"),
            None => "unverified".into(),
        };

        Self {
            current_state,
            next_step,
            files_touched: files_touched.into_iter().collect(),
            commands_run,
            verification_status,
            known_failures: dedupe_keep_order(known_failures),
            last_updated,
        }
    }

    pub fn to_markdown(&self) -> String {
        [
            "# Active Plan".to_string(),
            section_text("Current State", &self.current_state),
            section_text("Next Step", &self.next_step),
            section_list("Files Touched", &self.files_touched),
            section_list("Commands Run", &self.commands_run),
            section_text("Verification Status", &self.verification_status),
            section_list("Known Failures", &self.known_failures),
            section_text("Last Updated", &self.last_updated),
        ]
        .join("\n\n")
    }
}

pub fn write_handoff(
    cwd: impl AsRef<Path>,
    session_path: impl AsRef<Path>,
    trigger: &str,
    timestamp: String,
) -> Result<(ActivePlan, HandoffReport)> {
    let mut plan = ActivePlan::load_or_init(cwd)?;
    let draft = HandoffDraft::from_session_log(session_path, trigger, timestamp)?;
    let markdown = draft.to_markdown();
    std::fs::write(&plan.active_path, &markdown)
        .with_context(|| format!("write active plan {}", plan.active_path.display()))?;
    plan.active_text = markdown;
    plan.active_tokens = estimate_text_tokens(&plan.active_text);
    let report = HandoffReport {
        path: plan.active_path.clone(),
        trigger: trigger.to_string(),
        files_touched: draft.files_touched.len(),
        commands_run: draft.commands_run.len(),
        verification_status: draft.verification_status,
        known_failures: draft.known_failures.len(),
        tokens_estimate: plan.active_tokens,
    };
    Ok((plan, report))
}

fn read_session_values(path: impl AsRef<Path>) -> Result<Vec<Value>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read session log {}", path.display()))?;
    let mut events = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str::<Value>(line).with_context(|| {
            format!(
                "parse session event {} in {}",
                line_index + 1,
                path.display()
            )
        })?;
        events.push(event);
    }
    Ok(events)
}

fn event_type(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn is_verification_command(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    [
        "cargo test",
        "cargo build",
        "cargo check",
        "cargo fmt",
        "npm test",
        "npm run test",
        "npm run build",
        "pytest",
        "go test",
        "bun test",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

fn current_state(
    trigger: &str,
    latest_user: Option<&str>,
    latest_assistant: Option<&str>,
    latest_stop: Option<&str>,
) -> String {
    let mut lines = vec![format!("Handoff trigger: {trigger}.")];
    if let Some(user) = latest_user {
        lines.push(format!("Latest user request: {user}"));
    }
    if let Some(assistant) = latest_assistant {
        lines.push(format!("Latest assistant result: {assistant}"));
    }
    if let Some(stop) = latest_stop {
        lines.push(format!("Latest stop reason: {stop}."));
    }
    lines.join("\n")
}

fn next_step(trigger: &str, latest_user: Option<&str>, known_failures: &[String]) -> String {
    if let Some(failure) = known_failures.last() {
        return format!("Address latest blocker first: {failure}");
    }
    if trigger == "compact" {
        return "Continue with the compacted context and the latest active plan.".into();
    }
    if let Some(user) = latest_user {
        return format!("Continue from latest user request: {user}");
    }
    "Continue the current task from this active plan.".into()
}

fn section_text(title: &str, body: &str) -> String {
    format!("## {title}\n{}", body.trim())
}

fn section_list(title: &str, items: &[String]) -> String {
    if items.is_empty() {
        format!("## {title}\n- None")
    } else {
        let body = items
            .iter()
            .map(|item| format!("- {}", item.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        format!("## {title}\n{body}")
    }
}

fn trim_one_line(text: &str, max_chars: usize) -> String {
    let mut compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > max_chars {
        compact = compact.chars().take(max_chars).collect::<String>();
        compact.push_str("...");
    }
    compact
}

fn dedupe_keep_order(items: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            deduped.push(item);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiKind, PermissionMode};
    use crate::session::{StopReason, SESSION_DIR};
    use std::io::Write;
    use uuid::Uuid;

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("micos-plan-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_session(cwd: &Path, events: &[SessionEvent]) -> PathBuf {
        let dir = cwd.join(SESSION_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.jsonl", Uuid::new_v4()));
        let mut file = std::fs::File::create(&path).unwrap();
        for event in events {
            serde_json::to_writer(&mut file, event).unwrap();
            file.write_all(b"\n").unwrap();
        }
        path
    }

    #[test]
    fn active_plan_loads_non_empty_text() {
        let cwd = temp_dir();
        let root = cwd.join(PLAN_DIR);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(ACTIVE_PLAN_FILE), "# Active Plan\n\nNext.").unwrap();

        let plan = ActivePlan::load_or_init(&cwd).unwrap();

        assert_eq!(plan.active_text(), Some("# Active Plan\n\nNext."));
        assert!(plan.active_tokens > 0);
        assert!(!plan.created_dir);
    }

    #[test]
    fn handoff_extracts_commands_files_and_verification() {
        let events = vec![
            SessionEvent::UserInput {
                timestamp: "t".into(),
                text: "Implement the plan.".into(),
            },
            SessionEvent::ToolCall {
                timestamp: "t".into(),
                call_id: "write_1".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path":"src/plan.rs","content":"..."}),
            },
            SessionEvent::ToolCall {
                timestamp: "t".into(),
                call_id: "shell_1".into(),
                name: "shell".into(),
                arguments: serde_json::json!({"command":"cargo test"}),
            },
            SessionEvent::ToolOutput {
                timestamp: "t".into(),
                call_id: "shell_1".into(),
                success: true,
                output: "ok".into(),
                error: None,
                truncated: false,
                original_bytes: 2,
                preview_bytes: 2,
            },
            SessionEvent::Stop {
                timestamp: "t".into(),
                reason: StopReason::FinalAnswer,
            },
        ];

        let draft = HandoffDraft::from_events(&events, "manual", "2026-05-27T00:00:00Z".into());

        assert_eq!(draft.files_touched, vec!["src/plan.rs"]);
        assert_eq!(draft.commands_run, vec!["cargo test"]);
        assert_eq!(draft.verification_status, "verified: cargo test");
        assert!(draft.known_failures.is_empty());
        assert!(draft.to_markdown().contains("## Current State"));
        assert!(draft.to_markdown().contains("## Last Updated"));
    }

    #[test]
    fn write_handoff_persists_active_plan() {
        let cwd = temp_dir();
        let session_path = write_session(
            &cwd,
            &[SessionEvent::SessionStart {
                timestamp: "t".into(),
                session_id: Uuid::new_v4(),
                api_kind: ApiKind::Responses,
                model: "mock".into(),
                base_url: "http://localhost".into(),
                thinking: None,
                reasoning_effort: None,
                permission: PermissionMode::Safe,
                cwd: cwd.clone(),
            }],
        );

        let (plan, report) =
            write_handoff(&cwd, &session_path, "manual", "2026-05-27T00:00:00Z".into()).unwrap();

        assert_eq!(report.path, cwd.join(PLAN_DIR).join(ACTIVE_PLAN_FILE));
        assert_eq!(report.trigger, "manual");
        assert_eq!(report.verification_status, "unverified");
        assert!(plan.active_text.contains("# Active Plan"));
        assert!(plan.active_path.exists());
    }
}
