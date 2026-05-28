use crate::agent::AgentRuntime;
use crate::config::{
    ApiKind, PermissionMode, SessionConfig, DEFAULT_CONTEXT_WARNING_PERCENT,
    DEFAULT_RESPONSES_BASE_URL,
};
use crate::context::DEFAULT_CONTEXT_WINDOW_TOKENS;
use crate::model::{ModelClient, ModelRequest, ModelResponse};
use crate::session::{Session, StopReason};
use crate::tools::{BuiltinToolRegistry, PolicyEngine};
use crate::ui::NullUi;
use anyhow::{anyhow, Result};
use serde_json::json;
use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct EvalSuiteReport {
    pub fixtures: Vec<EvalFixtureReport>,
}

#[derive(Clone, Debug)]
pub struct EvalFixtureReport {
    pub name: String,
    pub passed: bool,
    pub stop_reason: Option<StopReason>,
    pub tool_count: usize,
    pub session_path: PathBuf,
    pub message: String,
}

#[derive(Clone)]
struct EvalModel {
    responses: Arc<Mutex<VecDeque<ModelResponse>>>,
}

impl EvalModel {
    fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
        }
    }
}

impl ModelClient for EvalModel {
    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow!("eval model response queue is empty"))
    }
}

pub async fn run_eval_suite(root: Option<PathBuf>) -> Result<EvalSuiteReport> {
    let root = root.unwrap_or_else(std::env::temp_dir);
    let root = root.join(format!("micos-eval-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root)?;

    let mut fixtures = Vec::new();
    fixtures.push(read_only_question(&root).await?);
    fixtures.push(small_edit(&root).await?);
    fixtures.push(denied_tool(&root).await?);
    fixtures.push(failing_tool_recovery(&root).await?);
    fixtures.push(compact_resume(&root).await?);
    Ok(EvalSuiteReport { fixtures })
}

async fn read_only_question(root: &std::path::Path) -> Result<EvalFixtureReport> {
    let mut agent = eval_agent(
        root,
        "read-only",
        PermissionMode::Safe,
        vec![assistant("workspace inspected")],
    )?;
    let reason = run_eval_turn(&mut agent, "What is this project?").await?;
    let path = agent.session_path().to_path_buf();
    fixture_report(
        "read_only_question",
        path,
        Some(reason),
        reason == StopReason::FinalAnswer,
    )
}

async fn small_edit(root: &std::path::Path) -> Result<EvalFixtureReport> {
    let mut agent = eval_agent(
        root,
        "small-edit",
        PermissionMode::Auto,
        vec![
            tool_call(
                "call_write",
                "write_file",
                json!({"path":"src/eval_sample.txt","content":"ok\n"}),
            ),
            assistant("edited"),
        ],
    )?;
    let reason = run_eval_turn(&mut agent, "Create a small file.").await?;
    let path = agent.session_path().to_path_buf();
    let edited = agent.config().cwd.join("src/eval_sample.txt").exists();
    fixture_report(
        "small_edit",
        path,
        Some(reason),
        reason == StopReason::FinalAnswer && edited,
    )
}

async fn denied_tool(root: &std::path::Path) -> Result<EvalFixtureReport> {
    let mut agent = eval_agent(
        root,
        "denied-tool",
        PermissionMode::Safe,
        vec![tool_call(
            "call_write",
            "write_file",
            json!({"path":"src/denied.txt","content":"no\n"}),
        )],
    )?;
    let reason = run_eval_turn(&mut agent, "Write a file.").await?;
    let path = agent.session_path().to_path_buf();
    let log = std::fs::read_to_string(&path)?;
    fixture_report(
        "denied_tool",
        path,
        Some(reason),
        reason == StopReason::ToolDenied && log.contains("recovery_report_written"),
    )
}

async fn failing_tool_recovery(root: &std::path::Path) -> Result<EvalFixtureReport> {
    let mut agent = eval_agent(
        root,
        "failing-tool",
        PermissionMode::Safe,
        vec![
            tool_call("call_read_1", "read_file", json!({"path":"missing.txt"})),
            tool_call("call_read_2", "read_file", json!({"path":"missing.txt"})),
        ],
    )?;
    let reason = run_eval_turn(&mut agent, "Read missing file.").await?;
    let path = agent.session_path().to_path_buf();
    let log = std::fs::read_to_string(&path)?;
    fixture_report(
        "failing_tool_recovery",
        path,
        Some(reason),
        reason == StopReason::ToolError && log.contains("recovery_report_written"),
    )
}

async fn compact_resume(root: &std::path::Path) -> Result<EvalFixtureReport> {
    let mut responses = Vec::new();
    for index in 0..10 {
        responses.push(assistant(format!("answer {index}")));
    }
    responses.push(assistant(compact_summary("turn 9")));
    let mut agent = eval_agent(root, "compact-source", PermissionMode::Safe, responses)?;
    for index in 0..10 {
        let reason = run_eval_turn(&mut agent, &format!("turn {index}")).await?;
        if reason != StopReason::FinalAnswer {
            let path = agent.session_path().to_path_buf();
            return fixture_report("compact_resume", path, Some(reason), false);
        }
    }
    let compact = agent.compact_context().await?;
    let source = agent.session_path().to_path_buf();
    let mut resumed = eval_agent(
        root,
        "compact-resume",
        PermissionMode::Safe,
        vec![assistant("resumed")],
    )?;
    let resume = resumed.resume_session(source.to_str().unwrap())?;
    fixture_report(
        "compact_resume",
        resumed.session_path().to_path_buf(),
        None,
        compact.compacted && resume.used_summary,
    )
}

fn eval_agent(
    root: &std::path::Path,
    name: &str,
    permission: PermissionMode,
    responses: Vec<ModelResponse>,
) -> Result<AgentRuntime<EvalModel, Session, BuiltinToolRegistry, crate::tools::ModePermissionPolicy>>
{
    let cwd = root.join(name);
    std::fs::create_dir_all(cwd.join("src"))?;
    std::fs::write(
        cwd.join("Cargo.toml"),
        "[package]\nname=\"eval\"\nversion=\"0.0.0\"\n",
    )?;
    let config = SessionConfig {
        api_kind: ApiKind::Responses,
        model: "eval-model".into(),
        base_url: DEFAULT_RESPONSES_BASE_URL.into(),
        thinking: None,
        reasoning_effort: None,
        permission,
        permission_rules: Vec::new(),
        max_steps: 8,
        context_window_tokens: DEFAULT_CONTEXT_WINDOW_TOKENS,
        context_warning_percent: DEFAULT_CONTEXT_WARNING_PERCENT,
        append_system_prompt: None,
        auto_compact: Default::default(),
        cwd,
    };
    let session = Session::new(&config)?;
    Ok(AgentRuntime::with_parts(
        config,
        EvalModel::new(responses),
        session,
        BuiltinToolRegistry::default(),
        PolicyEngine::new(Vec::new()),
    ))
}

async fn run_eval_turn(
    agent: &mut AgentRuntime<
        EvalModel,
        Session,
        BuiltinToolRegistry,
        crate::tools::ModePermissionPolicy,
    >,
    input: &str,
) -> Result<StopReason> {
    let mut ui = NullUi;
    agent.run_turn_with_ui(input.to_string(), &mut ui).await
}

fn fixture_report(
    name: &str,
    path: PathBuf,
    stop_reason: Option<StopReason>,
    passed: bool,
) -> Result<EvalFixtureReport> {
    let log = std::fs::read_to_string(&path)?;
    let tool_count = log.matches("\"type\":\"tool_call\"").count();
    Ok(EvalFixtureReport {
        name: name.into(),
        passed,
        stop_reason,
        tool_count,
        session_path: path,
        message: if passed { "ok" } else { "failed" }.into(),
    })
}

fn assistant(text: impl Into<String>) -> ModelResponse {
    ModelResponse::from_output(vec![json!({
        "type": "message",
        "role": "assistant",
        "content": [{"type":"output_text","text": text.into()}]
    })])
}

fn tool_call(call_id: &str, name: &str, arguments: serde_json::Value) -> ModelResponse {
    ModelResponse::from_output(vec![json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    })])
}

fn compact_summary(latest: &str) -> String {
    format!(
        "## Primary Request and Intent\nContinue eval compact smoke; latest user request: {latest}.\n\n## Key Technical Concepts\nContext compaction and resume.\n\n## Files and Code Sections\nNone.\n\n## Errors and Fixes\nNone.\n\n## Decisions Made\nUse deterministic eval model.\n\n## Pending Tasks\nRun verification.\n\n## Current Work\nEval compact smoke.\n\n## Next Step\nRun verification."
    )
}

impl fmt::Display for EvalSuiteReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let passed = self
            .fixtures
            .iter()
            .filter(|fixture| fixture.passed)
            .count();
        writeln!(f, "eval: {passed}/{} passed", self.fixtures.len())?;
        for fixture in &self.fixtures {
            writeln!(
                f,
                "{}: {} stop={} tools={} log={} {}",
                fixture.name,
                if fixture.passed { "passed" } else { "failed" },
                fixture
                    .stop_reason
                    .map(|reason| reason.to_string())
                    .unwrap_or_else(|| "n/a".into()),
                fixture.tool_count,
                fixture.session_path.display(),
                fixture.message
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn eval_suite_runs_deterministic_fixtures() {
        let report = run_eval_suite(None).await.unwrap();
        assert_eq!(report.fixtures.len(), 5);
        assert!(report.fixtures.iter().all(|fixture| fixture.passed));
        assert!(report.to_string().contains("eval: 5/5 passed"));
    }
}
