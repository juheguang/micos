use crate::context::{compacted_summary_message, estimate_text_tokens};
use crate::session::{SessionEvent, SESSION_DIR};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const REPLAY_TOOL_OUTPUT_LIMIT: usize = 12 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionReplay {
    pub source_session_id: Option<Uuid>,
    pub source_path: PathBuf,
    pub transcript: Vec<Value>,
    pub used_summary: bool,
    pub restored_tail_messages: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionResumeReport {
    pub source_session_id: Option<Uuid>,
    pub source_path: PathBuf,
    pub restored_messages: usize,
    pub used_summary: bool,
    pub restored_tail_messages: usize,
    pub estimated_tokens: usize,
}

pub fn resolve_session_target(cwd: &Path, target: &str) -> Result<PathBuf> {
    let target = target.trim();
    if target.is_empty() {
        bail!("resume target is required");
    }

    if let Ok(id) = Uuid::parse_str(target) {
        return Ok(cwd.join(SESSION_DIR).join(format!("{id}.jsonl")));
    }

    let path = PathBuf::from(target);
    if path.is_absolute() {
        return Ok(path);
    }

    let cwd_relative = cwd.join(&path);
    if cwd_relative.exists() {
        return Ok(cwd_relative);
    }

    let session_relative = cwd.join(SESSION_DIR).join(&path);
    if session_relative.exists() {
        return Ok(session_relative);
    }

    if path.extension().is_none() {
        return Ok(cwd.join(SESSION_DIR).join(format!("{target}.jsonl")));
    }

    Ok(cwd_relative)
}

pub fn replay_session(path: impl AsRef<Path>) -> Result<SessionReplay> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read session log {}", path.display()))?;
    let mut transcript = Vec::new();
    let mut source_session_id = None;
    let mut used_summary = false;
    let mut restored_tail_messages = 0usize;

    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str::<SessionEvent>(line).with_context(|| {
            format!(
                "parse session event {} in {}",
                line_index + 1,
                path.display()
            )
        })?;
        match event {
            SessionEvent::SessionStart { session_id, .. } => {
                source_session_id = Some(session_id);
            }
            SessionEvent::UserInput { text, .. } => transcript.push(user_message(&text)),
            SessionEvent::AssistantText { text, .. } => transcript.push(assistant_message(&text)),
            SessionEvent::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } => transcript.push(tool_call_message(&call_id, &name, &arguments)),
            SessionEvent::ToolOutput {
                call_id,
                success,
                output,
                error,
                ..
            } => transcript.push(tool_output_message(
                &call_id,
                success,
                &output,
                error.as_deref(),
            )),
            SessionEvent::ContextSummary {
                summary,
                retained_messages,
                ..
            } => {
                let tail = select_replay_tail(&transcript, retained_messages);
                restored_tail_messages = tail.len();
                transcript = std::iter::once(compacted_summary_message(&summary))
                    .chain(tail)
                    .collect();
                used_summary = true;
            }
            _ => {}
        }
    }

    Ok(SessionReplay {
        source_session_id,
        source_path: path.to_path_buf(),
        transcript,
        used_summary,
        restored_tail_messages,
    })
}

impl SessionResumeReport {
    pub fn from_replay(replay: &SessionReplay) -> Self {
        Self {
            source_session_id: replay.source_session_id,
            source_path: replay.source_path.clone(),
            restored_messages: replay.transcript.len(),
            used_summary: replay.used_summary,
            restored_tail_messages: replay.restored_tail_messages,
            estimated_tokens: estimate_text_tokens(
                &Value::Array(replay.transcript.clone()).to_string(),
            ),
        }
    }
}

fn user_message(text: &str) -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": text}]
    })
}

fn assistant_message(text: &str) -> Value {
    json!({
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": text}]
    })
}

fn tool_call_message(call_id: &str, name: &str, arguments: &Value) -> Value {
    json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    })
}

fn tool_output_message(call_id: &str, success: bool, output: &str, error: Option<&str>) -> Value {
    let projection = project_tool_output(success, output, error);
    json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": serde_json::to_string(&projection).unwrap()
    })
}

fn project_tool_output(success: bool, output: &str, error: Option<&str>) -> Value {
    let preview = truncate_preview(output, REPLAY_TOOL_OUTPUT_LIMIT);
    json!({
        "success": success,
        "output": preview.text,
        "error": error,
        "truncated": preview.truncated,
        "original_bytes": preview.original_bytes,
        "preview_bytes": preview.preview_bytes,
        "omitted_bytes": preview.omitted_bytes,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Preview {
    text: String,
    truncated: bool,
    original_bytes: usize,
    preview_bytes: usize,
    omitted_bytes: usize,
}

fn truncate_preview(text: &str, limit: usize) -> Preview {
    if text.len() <= limit {
        return Preview {
            text: text.to_string(),
            truncated: false,
            original_bytes: text.len(),
            preview_bytes: text.len(),
            omitted_bytes: 0,
        };
    }

    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Preview {
        text: text[..end].to_string(),
        truncated: true,
        original_bytes: text.len(),
        preview_bytes: end,
        omitted_bytes: text.len() - end,
    }
}

fn select_replay_tail(transcript: &[Value], retained_messages: usize) -> Vec<Value> {
    if retained_messages == 0 {
        return Vec::new();
    }
    let start = transcript.len().saturating_sub(retained_messages);
    transcript[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiKind, PermissionMode, ReasoningEffort, ThinkingMode, DEEPSEEK_CHAT_COMPLETIONS_BASE_URL,
    };
    use std::io::Write;

    fn write_session(lines: &[SessionEvent]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("micos-replay-{}.jsonl", Uuid::new_v4()));
        let mut file = std::fs::File::create(&path).unwrap();
        for event in lines {
            serde_json::to_writer(&mut file, event).unwrap();
            file.write_all(b"\n").unwrap();
        }
        path
    }

    fn session_start(id: Uuid) -> SessionEvent {
        SessionEvent::SessionStart {
            timestamp: "2026-05-27T00:00:00Z".into(),
            session_id: id,
            api_kind: ApiKind::ChatCompletions,
            model: "deepseek-v4-flash".into(),
            base_url: DEEPSEEK_CHAT_COMPLETIONS_BASE_URL.into(),
            thinking: Some(ThinkingMode::Enabled),
            reasoning_effort: Some(ReasoningEffort::High),
            permission: PermissionMode::Safe,
            cwd: PathBuf::from("/tmp/micos"),
        }
    }

    #[test]
    fn resolves_uuid_and_paths() {
        let cwd = PathBuf::from("/tmp/project");
        let id = Uuid::new_v4();
        assert_eq!(
            resolve_session_target(&cwd, &id.to_string()).unwrap(),
            cwd.join(SESSION_DIR).join(format!("{id}.jsonl"))
        );
        assert_eq!(
            resolve_session_target(&cwd, "/tmp/source.jsonl").unwrap(),
            PathBuf::from("/tmp/source.jsonl")
        );
    }

    #[test]
    fn replays_plain_session_transcript() {
        let id = Uuid::new_v4();
        let path = write_session(&[
            session_start(id),
            SessionEvent::UserInput {
                timestamp: "t".into(),
                text: "hello".into(),
            },
            SessionEvent::AssistantText {
                timestamp: "t".into(),
                text: "hi".into(),
            },
            SessionEvent::ToolCall {
                timestamp: "t".into(),
                call_id: "call_1".into(),
                name: "list_files".into(),
                arguments: json!({"path":"."}),
            },
            SessionEvent::ToolOutput {
                timestamp: "t".into(),
                call_id: "call_1".into(),
                success: true,
                output: "done".into(),
                error: None,
            },
            SessionEvent::Stop {
                timestamp: "t".into(),
                reason: crate::session::StopReason::FinalAnswer,
            },
        ]);

        let replay = replay_session(&path).unwrap();

        assert_eq!(replay.source_session_id, Some(id));
        assert!(!replay.used_summary);
        assert_eq!(replay.transcript.len(), 4);
        assert_eq!(replay.transcript[0]["role"], "user");
        assert_eq!(replay.transcript[2]["type"], "function_call");
        assert_eq!(replay.transcript[3]["type"], "function_call_output");
    }

    #[test]
    fn replay_ignores_permission_trace_events() {
        let path = write_session(&[
            session_start(Uuid::new_v4()),
            SessionEvent::PermissionDecision {
                timestamp: "t".into(),
                call_id: "call_1".into(),
                tool: "shell".into(),
                argument_summary: "command=cargo test".into(),
                decision: crate::tools::PermissionDecision::Allow,
                reason: crate::tools::DecisionReason::Tool,
                rule_source: None,
                permission_mode: PermissionMode::Safe,
                elapsed_ms: 2,
                message: Some("allowed".into()),
            },
            SessionEvent::ToolFinished {
                timestamp: "t".into(),
                call_id: "call_1".into(),
                name: "shell".into(),
                success: true,
                output: "ok".into(),
                error: None,
                elapsed_ms: 3,
            },
            SessionEvent::UserInput {
                timestamp: "t".into(),
                text: "continue".into(),
            },
        ]);

        let replay = replay_session(&path).unwrap();

        assert_eq!(replay.transcript.len(), 1);
        assert_eq!(replay.transcript[0]["role"], "user");
    }

    #[test]
    fn replays_compacted_summary_with_retained_tail() {
        let path = write_session(&[
            session_start(Uuid::new_v4()),
            SessionEvent::UserInput {
                timestamp: "t".into(),
                text: "turn 1".into(),
            },
            SessionEvent::AssistantText {
                timestamp: "t".into(),
                text: "answer 1".into(),
            },
            SessionEvent::UserInput {
                timestamp: "t".into(),
                text: "turn 2".into(),
            },
            SessionEvent::AssistantText {
                timestamp: "t".into(),
                text: "answer 2".into(),
            },
            SessionEvent::ContextSummary {
                timestamp: "t".into(),
                summary: "## Primary Request and Intent\ncontinue".into(),
                summary_tokens: 10,
                messages_replaced: 2,
                retained_messages: 2,
                summary_format_version: 1,
                trigger: "manual".into(),
            },
        ]);

        let replay = replay_session(&path).unwrap();

        assert!(replay.used_summary);
        assert_eq!(replay.restored_tail_messages, 2);
        assert_eq!(replay.transcript.len(), 3);
        assert!(replay.transcript[0]
            .to_string()
            .contains("compacted summary"));
        assert!(replay.transcript[1].to_string().contains("turn 2"));
    }

    #[test]
    fn replay_projects_large_tool_output() {
        let path = write_session(&[
            session_start(Uuid::new_v4()),
            SessionEvent::ToolOutput {
                timestamp: "t".into(),
                call_id: "call_1".into(),
                success: true,
                output: "x".repeat(REPLAY_TOOL_OUTPUT_LIMIT + 4),
                error: None,
            },
        ]);

        let replay = replay_session(&path).unwrap();
        let output = replay.transcript[0]["output"].as_str().unwrap();
        let projected: Value = serde_json::from_str(output).unwrap();

        assert_eq!(projected["truncated"], true);
        assert_eq!(
            projected["preview_bytes"].as_u64().unwrap() as usize,
            REPLAY_TOOL_OUTPUT_LIMIT
        );
    }

    #[test]
    fn malformed_jsonl_returns_clear_error() {
        let path = std::env::temp_dir().join(format!("micos-replay-{}.jsonl", Uuid::new_v4()));
        std::fs::write(&path, "{\"type\":\"user_input\"").unwrap();

        let error = replay_session(&path).unwrap_err();

        assert!(error.to_string().contains("parse session event 1"));
    }
}
