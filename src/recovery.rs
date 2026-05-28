use crate::context::estimate_text_tokens;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub const RECOVERY_DIR: &str = ".micos/recovery";
pub const LATEST_RECOVERY_FILE: &str = "latest.md";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    ApiError,
    ToolDenied,
    ToolError,
    MaxSteps,
    UserInterrupt,
    VerificationFailed,
    Unknown,
}

impl std::fmt::Display for FailureClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FailureClass::ApiError => write!(f, "api_error"),
            FailureClass::ToolDenied => write!(f, "tool_denied"),
            FailureClass::ToolError => write!(f, "tool_error"),
            FailureClass::MaxSteps => write!(f, "max_steps"),
            FailureClass::UserInterrupt => write!(f, "user_interrupt"),
            FailureClass::VerificationFailed => write!(f, "verification_failed"),
            FailureClass::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryDraft {
    pub stop_reason: Option<String>,
    pub failure_class: FailureClass,
    pub latest_user: Option<String>,
    pub attempted_tools: Vec<String>,
    pub files_touched: Vec<String>,
    pub commands_run: Vec<String>,
    pub verification_status: String,
    pub known_failures: Vec<String>,
    pub next_safe_step: String,
    pub last_updated: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryReport {
    pub path: PathBuf,
    pub trigger: String,
    pub stop_reason: Option<String>,
    pub failure_class: FailureClass,
    pub known_failures: usize,
    pub tokens_estimate: usize,
}

impl RecoveryDraft {
    pub fn from_session_log(path: impl AsRef<Path>, last_updated: String) -> Result<Self> {
        let events = read_session_values(path)?;
        Self::from_values(&events, last_updated)
    }

    pub fn from_values(events: &[Value], last_updated: String) -> Result<Self> {
        let latest_user = events.iter().rev().find_map(|event| {
            (event_type(event) == Some("user_input"))
                .then(|| json_string(event, "text"))
                .flatten()
                .map(|text| trim_one_line(&text, 220))
        });
        let latest_stop = events.iter().rev().find_map(|event| {
            (event_type(event) == Some("stop"))
                .then(|| json_string(event, "reason"))
                .flatten()
        });
        let latest_failed_verification = events.iter().rev().find_map(|event| {
            if event_type(event) != Some("verification_finished") {
                return None;
            }
            let success = event
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            (!success).then(|| json_string(event, "command").unwrap_or_else(|| "verify".into()))
        });

        let failure_class = latest_stop
            .as_deref()
            .and_then(failure_class_for_stop)
            .or_else(|| {
                latest_failed_verification
                    .as_ref()
                    .map(|_| FailureClass::VerificationFailed)
            })
            .unwrap_or(FailureClass::Unknown);

        if latest_stop.as_deref() == Some("final_answer") && latest_failed_verification.is_none() {
            bail!("no recoverable failure found in session");
        }

        let mut attempted_tools = Vec::new();
        let mut files_touched = BTreeSet::new();
        let mut commands_run = Vec::new();
        let mut shell_calls = HashMap::<String, String>::new();
        let mut known_failures = Vec::new();
        let mut latest_verification: Option<(String, bool)> = None;

        for event in events {
            match event_type(event) {
                Some("tool_call" | "tool_started") => {
                    let call_id = json_string(event, "call_id").unwrap_or_default();
                    let name = json_string(event, "name").unwrap_or_else(|| "unknown".into());
                    let arguments = event.get("arguments").unwrap_or(&Value::Null);
                    attempted_tools.push(format!("{name}: {}", tool_summary(&name, arguments)));
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
                    if let Some(command) = shell_calls.get(&call_id) {
                        if is_verification_command(command) {
                            latest_verification = Some((command.clone(), success));
                        }
                    }
                    if !success {
                        let detail = json_string(event, "error")
                            .or_else(|| json_string(event, "output"))
                            .unwrap_or_else(|| "tool failed".into());
                        known_failures
                            .push(format!("tool {call_id}: {}", trim_one_line(&detail, 180)));
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

        let verification_status = match latest_verification {
            Some((command, true)) => format!("verified: {command}"),
            Some((command, false)) => format!("failed: {command}"),
            None => "unverified".into(),
        };
        let known_failures = dedupe_keep_order(known_failures);
        let next_safe_step = next_safe_step(failure_class, latest_user.as_deref(), &known_failures);

        Ok(Self {
            stop_reason: latest_stop,
            failure_class,
            latest_user,
            attempted_tools: dedupe_keep_order(attempted_tools),
            files_touched: files_touched.into_iter().collect(),
            commands_run,
            verification_status,
            known_failures,
            next_safe_step,
            last_updated,
        })
    }

    pub fn to_markdown(&self) -> String {
        [
            "# Recovery Report".to_string(),
            section_text("Failure Class", &self.failure_class.to_string()),
            section_text(
                "Stop Reason",
                self.stop_reason.as_deref().unwrap_or("unknown"),
            ),
            section_text(
                "Latest User Request",
                self.latest_user.as_deref().unwrap_or("unknown"),
            ),
            section_list("Attempted Tools", &self.attempted_tools),
            section_list("Files Touched", &self.files_touched),
            section_list("Commands Run", &self.commands_run),
            section_text("Verification Status", &self.verification_status),
            section_list("Known Failures", &self.known_failures),
            section_text("Next Safe Step", &self.next_safe_step),
            section_text("Last Updated", &self.last_updated),
        ]
        .join("\n\n")
    }
}

pub fn write_recovery_report(
    cwd: impl AsRef<Path>,
    session_path: impl AsRef<Path>,
    trigger: &str,
    timestamp: String,
) -> Result<RecoveryReport> {
    let root = cwd.as_ref().join(RECOVERY_DIR);
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create recovery directory {}", root.display()))?;
    let path = root.join(LATEST_RECOVERY_FILE);
    let draft = RecoveryDraft::from_session_log(session_path, timestamp)?;
    let markdown = draft.to_markdown();
    std::fs::write(&path, &markdown)
        .with_context(|| format!("write recovery report {}", path.display()))?;
    Ok(RecoveryReport {
        path,
        trigger: trigger.to_string(),
        stop_reason: draft.stop_reason,
        failure_class: draft.failure_class,
        known_failures: draft.known_failures.len(),
        tokens_estimate: estimate_text_tokens(&markdown),
    })
}

pub fn failure_class_for_stop(reason: &str) -> Option<FailureClass> {
    match reason {
        "api_error" => Some(FailureClass::ApiError),
        "tool_denied" => Some(FailureClass::ToolDenied),
        "tool_error" => Some(FailureClass::ToolError),
        "max_steps" => Some(FailureClass::MaxSteps),
        "user_interrupt" => Some(FailureClass::UserInterrupt),
        _ => None,
    }
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

fn tool_summary(name: &str, arguments: &Value) -> String {
    match name {
        "write_file" => json_string(arguments, "path").unwrap_or_else(|| "<missing>".into()),
        "read_file" => json_string(arguments, "path").unwrap_or_else(|| "<missing>".into()),
        "shell" => json_string(arguments, "command").unwrap_or_else(|| "<missing>".into()),
        _ => trim_one_line(&arguments.to_string(), 120),
    }
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

fn next_safe_step(
    failure_class: FailureClass,
    latest_user: Option<&str>,
    known_failures: &[String],
) -> String {
    if let Some(failure) = known_failures.last() {
        return format!("Address latest blocker first: {failure}");
    }
    match failure_class {
        FailureClass::ToolDenied => {
            "Choose a permitted path or ask the user to adjust permissions.".into()
        }
        FailureClass::ToolError => {
            "Inspect the failed tool output and retry with a narrower command or path.".into()
        }
        FailureClass::ApiError => {
            "Retry the model request after checking API configuration and network availability."
                .into()
        }
        FailureClass::MaxSteps => {
            "Resume from the latest active plan and reduce the next step scope.".into()
        }
        FailureClass::UserInterrupt => {
            "Resume only after confirming the user still wants to continue.".into()
        }
        FailureClass::VerificationFailed => {
            "Fix the failing verification command before reporting success.".into()
        }
        FailureClass::Unknown => latest_user
            .map(|user| format!("Continue from latest user request: {user}"))
            .unwrap_or_else(|| "Inspect the session trace and choose the next safe action.".into()),
    }
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

fn trim_one_line(text: &str, max_chars: usize) -> String {
    let mut compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > max_chars {
        compact = compact.chars().take(max_chars).collect::<String>();
        compact.push_str("...");
    }
    compact
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn recovery_draft_extracts_failed_turn() {
        let events = vec![
            json!({"type":"user_input","text":"Edit file"}),
            json!({"type":"tool_call","call_id":"call_1","name":"write_file","arguments":{"path":"src/lib.rs","content":"x"}}),
            json!({"type":"permission_denied","call_id":"call_1","name":"write_file","reason":"denied by policy"}),
            json!({"type":"stop","reason":"tool_denied"}),
        ];

        let draft = RecoveryDraft::from_values(&events, "2026-05-27T00:00:00Z".into()).unwrap();

        assert_eq!(draft.failure_class, FailureClass::ToolDenied);
        assert_eq!(draft.stop_reason.as_deref(), Some("tool_denied"));
        assert_eq!(draft.files_touched, vec!["src/lib.rs"]);
        assert!(draft
            .known_failures
            .contains(&"permission denied for write_file: denied by policy".into()));
        assert!(draft.to_markdown().contains("## Next Safe Step"));
    }

    #[test]
    fn final_answer_without_failed_verification_is_not_recoverable() {
        let events = vec![json!({"type":"stop","reason":"final_answer"})];
        let error = RecoveryDraft::from_values(&events, "t".into()).unwrap_err();
        assert!(error.to_string().contains("no recoverable failure"));
    }

    #[test]
    fn write_recovery_report_persists_latest_file() {
        let cwd = std::env::temp_dir().join(format!("micos-recovery-{}", Uuid::new_v4()));
        let sessions = cwd.join(".micos/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let session = sessions.join("s.jsonl");
        std::fs::write(
            &session,
            r#"{"type":"user_input","text":"Run tests"}
{"type":"verification_finished","name":"test","command":"cargo test","success":false,"exit_code":101,"elapsed_ms":1,"output_preview":"failed","truncated":false}
"#,
        )
        .unwrap();

        let report =
            write_recovery_report(&cwd, &session, "manual", "2026-05-27T00:00:00Z".into()).unwrap();

        assert_eq!(report.failure_class, FailureClass::VerificationFailed);
        assert!(report.path.exists());
        assert!(std::fs::read_to_string(report.path)
            .unwrap()
            .contains("verification_failed"));
    }
}
