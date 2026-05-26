use crate::config::SessionConfig;
use crate::model::{ModelClient, ModelRequest};
use crate::session::{now, Session, SessionEvent, StopReason};
use crate::tools::{
    permission_decision, BuiltinTool, Tool, ToolContext, ToolPermissionDecision, ToolResult,
};
#[cfg(test)]
use crate::ui::NullUi;
use crate::ui::{AgentEvent, UiSink};
use anyhow::Result;
use serde_json::{json, Value};
use std::time::Instant;
use uuid::Uuid;

pub struct Agent<C> {
    config: SessionConfig,
    client: C,
    session: Session,
    tools: Vec<BuiltinTool>,
    transcript: Vec<Value>,
}

impl<C: ModelClient> Agent<C> {
    pub fn new(config: SessionConfig, client: C, session: Session) -> Self {
        Self {
            config,
            client,
            session,
            tools: BuiltinTool::all(),
            transcript: Vec::new(),
        }
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    pub fn session_id(&self) -> Uuid {
        self.session.id()
    }

    pub fn session_path(&self) -> &std::path::Path {
        self.session.path()
    }

    pub async fn stop(&self, reason: StopReason) -> Result<()> {
        self.session.append(&SessionEvent::Stop {
            timestamp: now(),
            reason,
        })
    }

    #[cfg(test)]
    pub async fn run_turn(&mut self, input: String) -> Result<StopReason> {
        let mut ui = NullUi;
        self.run_turn_with_ui(input, &mut ui).await
    }

    pub async fn run_turn_with_ui<S: UiSink + Send>(
        &mut self,
        input: String,
        ui: &mut S,
    ) -> Result<StopReason> {
        ui.on_event(AgentEvent::TurnStarted {
            input: input.clone(),
        })?;
        self.session.append(&SessionEvent::UserInput {
            timestamp: now(),
            text: input.clone(),
        })?;
        self.transcript.push(json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": input}]
        }));

        let mut consecutive_tool_failures = 0usize;

        for _step in 0..self.config.max_steps {
            let request = ModelRequest {
                model: self.config.model.clone(),
                input: self.transcript.clone(),
                tools: self.tools.iter().map(Tool::schema).collect(),
                instructions: system_instructions(),
                parallel_tool_calls: false,
                thinking: self.config.thinking.map(|value| value.to_string()),
                reasoning_effort: self.config.reasoning_effort.map(|value| value.to_string()),
            };

            let mut recording_ui = SessionRecordingUi {
                session: &self.session,
                inner: ui,
            };
            let response = match self
                .client
                .respond_streaming(request, &mut recording_ui)
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    self.session.append(&SessionEvent::Error {
                        timestamp: now(),
                        message: error.to_string(),
                    })?;
                    ui.on_event(AgentEvent::Error {
                        message: error.to_string(),
                    })?;
                    self.stop(StopReason::ApiError).await?;
                    ui.on_event(AgentEvent::Stop {
                        reason: StopReason::ApiError,
                    })?;
                    return Ok(StopReason::ApiError);
                }
            };

            for item in &response.output {
                self.transcript.push(item.clone());
            }

            for text in &response.assistant_text {
                self.session.append(&SessionEvent::AssistantText {
                    timestamp: now(),
                    text: text.clone(),
                })?;
            }

            if response.function_calls.is_empty() {
                self.stop(StopReason::FinalAnswer).await?;
                ui.on_event(AgentEvent::Stop {
                    reason: StopReason::FinalAnswer,
                })?;
                return Ok(StopReason::FinalAnswer);
            }

            for call in response.function_calls {
                let arguments: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|_| {
                    json!({
                        "_raw_arguments": call.arguments
                    })
                });
                self.session.append(&SessionEvent::ToolCall {
                    timestamp: now(),
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments: arguments.clone(),
                })?;

                self.session.append(&SessionEvent::ToolStarted {
                    timestamp: now(),
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments: arguments.clone(),
                    permission: self.config.permission,
                })?;
                ui.on_event(AgentEvent::ToolCallStarted {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments: arguments.clone(),
                    permission: self.config.permission,
                })?;

                let start = Instant::now();
                let result = self.execute_tool(&call.name, arguments.clone(), ui).await;
                let elapsed = start.elapsed();
                self.session.append(&SessionEvent::ToolOutput {
                    timestamp: now(),
                    call_id: call.call_id.clone(),
                    success: result.success,
                    output: result.output.clone(),
                    error: result.error.clone(),
                })?;
                self.session.append(&SessionEvent::ToolFinished {
                    timestamp: now(),
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    success: result.success,
                    output: result.output.clone(),
                    error: result.error.clone(),
                    elapsed_ms: elapsed.as_millis(),
                })?;
                if result.denied {
                    self.session.append(&SessionEvent::PermissionDenied {
                        timestamp: now(),
                        call_id: call.call_id.clone(),
                        name: call.name.clone(),
                        reason: result.error.clone().unwrap_or_else(|| "denied".into()),
                    })?;
                }
                ui.on_event(AgentEvent::ToolCallFinished {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    result: result.clone(),
                    elapsed,
                })?;

                if result.denied {
                    self.transcript
                        .push(function_call_output(&call.call_id, &result));
                    self.stop(StopReason::ToolDenied).await?;
                    ui.on_event(AgentEvent::Stop {
                        reason: StopReason::ToolDenied,
                    })?;
                    return Ok(StopReason::ToolDenied);
                }

                if result.success {
                    consecutive_tool_failures = 0;
                } else {
                    consecutive_tool_failures += 1;
                    if consecutive_tool_failures >= 2 {
                        self.transcript
                            .push(function_call_output(&call.call_id, &result));
                        self.stop(StopReason::ToolError).await?;
                        ui.on_event(AgentEvent::Stop {
                            reason: StopReason::ToolError,
                        })?;
                        return Ok(StopReason::ToolError);
                    }
                }

                self.transcript
                    .push(function_call_output(&call.call_id, &result));
            }
        }

        self.stop(StopReason::MaxSteps).await?;
        ui.on_event(AgentEvent::Stop {
            reason: StopReason::MaxSteps,
        })?;
        Ok(StopReason::MaxSteps)
    }

    async fn execute_tool<S: UiSink + Send>(
        &self,
        name: &str,
        arguments: Value,
        ui: &mut S,
    ) -> ToolResult {
        let Some(tool) = self.tools.iter().find(|tool| tool.name() == name) else {
            return ToolResult::error(format!("unknown tool: {name}"));
        };

        match permission_decision(self.config.permission, name, &arguments) {
            ToolPermissionDecision::Allowed => {}
            ToolPermissionDecision::NeedsApproval { summary } => {
                match ui.approve_tool(name, &summary) {
                    Ok(true) => {}
                    Ok(false) => return ToolResult::denied(format!("{name} denied by user")),
                    Err(error) => return ToolResult::error(error.to_string()),
                }
            }
            ToolPermissionDecision::Denied { reason } => return ToolResult::denied(reason),
        }

        tool.execute(
            arguments,
            ToolContext {
                cwd: self.config.cwd.clone(),
                permission: self.config.permission,
            },
        )
        .await
    }
}

struct SessionRecordingUi<'a, S> {
    session: &'a Session,
    inner: &'a mut S,
}

impl<S: UiSink> UiSink for SessionRecordingUi<'_, S> {
    fn on_event(&mut self, event: AgentEvent) -> Result<()> {
        match &event {
            AgentEvent::AssistantDelta { text } => {
                self.session.append(&SessionEvent::AssistantDelta {
                    timestamp: now(),
                    text: text.clone(),
                })?;
            }
            AgentEvent::ReasoningDelta { text } => {
                self.session.append(&SessionEvent::ReasoningDelta {
                    timestamp: now(),
                    text: text.clone(),
                })?;
            }
            AgentEvent::Error { message } => {
                self.session.append(&SessionEvent::Error {
                    timestamp: now(),
                    message: message.clone(),
                })?;
            }
            _ => {}
        }
        self.inner.on_event(event)
    }

    fn approve_tool(&mut self, name: &str, summary: &str) -> Result<bool> {
        self.inner.approve_tool(name, summary)
    }
}

fn function_call_output(call_id: &str, result: &ToolResult) -> Value {
    json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": serde_json::to_string(&json!({
            "success": result.success,
            "output": result.output,
            "error": result.error,
        })).unwrap()
    })
}

fn system_instructions() -> String {
    [
        "You are micos, a minimal local coding agent harness.",
        "Use the provided tools when local filesystem or shell context is required.",
        "All tool execution is performed by the harness. Respect tool errors and permission denials.",
        "When the task is complete, answer directly without calling another tool.",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PermissionMode, SessionConfig};
    use crate::model::{ModelFunctionCall, ModelRequest, ModelResponse};
    use anyhow::anyhow;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    struct MockModel {
        responses: Arc<Mutex<VecDeque<anyhow::Result<ModelResponse>>>>,
    }

    impl MockModel {
        fn new(responses: Vec<anyhow::Result<ModelResponse>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
            }
        }
    }

    impl ModelClient for MockModel {
        async fn respond(&self, _request: ModelRequest) -> anyhow::Result<ModelResponse> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(anyhow!("no mock response")))
        }
    }

    fn final_response(text: &str) -> ModelResponse {
        ModelResponse::from_output(vec![json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}]
        })])
    }

    fn tool_response(name: &str, call_id: &str, arguments: Value) -> ModelResponse {
        ModelResponse {
            output: vec![json!({
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": arguments.to_string()
            })],
            assistant_text: Vec::new(),
            function_calls: vec![ModelFunctionCall {
                call_id: call_id.to_string(),
                name: name.to_string(),
                arguments: arguments.to_string(),
            }],
        }
    }

    fn temp_config(permission: PermissionMode, max_steps: usize) -> SessionConfig {
        let cwd = std::env::temp_dir().join(format!("micos-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        SessionConfig {
            api_kind: crate::config::ApiKind::Responses,
            model: "mock".into(),
            base_url: crate::config::DEFAULT_RESPONSES_BASE_URL.into(),
            thinking: None,
            reasoning_effort: None,
            permission,
            max_steps,
            cwd,
        }
    }

    fn test_agent(
        permission: PermissionMode,
        max_steps: usize,
        responses: Vec<anyhow::Result<ModelResponse>>,
    ) -> Agent<MockModel> {
        let config = temp_config(permission, max_steps);
        let session = Session::new(&config).unwrap();
        Agent::new(config, MockModel::new(responses), session)
    }

    #[tokio::test]
    async fn final_answer_without_tools_stops_cleanly() {
        let mut agent = test_agent(PermissionMode::Safe, 3, vec![Ok(final_response("done"))]);

        let reason = agent.run_turn("hello".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
    }

    #[tokio::test]
    async fn one_tool_call_then_final_answer_persists_events() {
        let mut agent = test_agent(
            PermissionMode::Safe,
            3,
            vec![
                Ok(tool_response("list_files", "call_1", json!({"path": "."}))),
                Ok(final_response("listed")),
            ],
        );
        let path = agent.session.path().clone();

        let reason = agent.run_turn("list files".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("\"type\":\"tool_call\""));
        assert!(log.contains("\"type\":\"tool_output\""));
        assert!(log.contains("\"reason\":\"final_answer\""));
    }

    #[tokio::test]
    async fn tool_denial_stops_turn() {
        let mut agent = test_agent(
            PermissionMode::Safe,
            3,
            vec![Ok(tool_response(
                "write_file",
                "call_1",
                json!({"path": "x.txt", "content": "x"}),
            ))],
        );

        let reason = agent.run_turn("write".into()).await.unwrap();
        assert_eq!(reason, StopReason::ToolDenied);
    }

    #[tokio::test]
    async fn repeated_tool_failure_stops_turn() {
        let mut agent = test_agent(
            PermissionMode::Auto,
            4,
            vec![
                Ok(tool_response("missing", "call_1", json!({}))),
                Ok(tool_response("missing", "call_2", json!({}))),
            ],
        );

        let reason = agent.run_turn("fail twice".into()).await.unwrap();
        assert_eq!(reason, StopReason::ToolError);
    }

    #[tokio::test]
    async fn max_steps_prevents_infinite_tool_loop() {
        let mut agent = test_agent(
            PermissionMode::Safe,
            1,
            vec![Ok(tool_response(
                "list_files",
                "call_1",
                json!({"path": "."}),
            ))],
        );

        let reason = agent.run_turn("loop".into()).await.unwrap();
        assert_eq!(reason, StopReason::MaxSteps);
    }
}
