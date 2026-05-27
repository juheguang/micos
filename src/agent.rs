use crate::config::{save_permission_rule, ModelSettings, SessionConfig, ThinkingMode};
use crate::context::{
    compacted_summary_message, estimate_text_tokens, ContextBuilder, ContextStats,
};
use crate::model::{ModelClient, ModelRequest, OpenAiModelClient};
use crate::prompt::{compact_instructions, PromptBuilder};
use crate::session::{now, Session, SessionEvent, SessionStore, StopReason};
use crate::tools::{
    BuiltinToolRegistry, DecisionReason, ModePermissionPolicy, PermissionDecision,
    PermissionPolicy, PermissionRule, PolicyDecision, PolicyEngine, RuleBehavior, RuleSource,
    ToolContext, ToolRegistry, ToolResult,
};
#[cfg(test)]
use crate::ui::NullUi;
use crate::ui::{AgentEvent, ApprovalDecision, UiSink};
use anyhow::Result;
use serde_json::{json, Value};
use std::time::Instant;
use uuid::Uuid;

pub type Agent<C> = AgentRuntime<C, Session, BuiltinToolRegistry, ModePermissionPolicy>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextCompactReport {
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub summary_tokens: usize,
    pub messages_replaced: usize,
}

pub struct AgentRuntime<C, R = Session, T = BuiltinToolRegistry, P = ModePermissionPolicy> {
    config: SessionConfig,
    client: C,
    session: R,
    tools: T,
    permission_policy: P,
    transcript: Vec<Value>,
}

impl<C> AgentRuntime<C, Session, BuiltinToolRegistry, ModePermissionPolicy> {
    pub fn new(config: SessionConfig, client: C, session: Session) -> Self {
        let permission_rules = config.permission_rules.clone();
        Self {
            config,
            client,
            session,
            tools: BuiltinToolRegistry::default(),
            permission_policy: PolicyEngine::new(permission_rules),
            transcript: Vec::new(),
        }
    }
}

impl<C, R, T, P> AgentRuntime<C, R, T, P> {
    pub fn with_parts(
        config: SessionConfig,
        client: C,
        session: R,
        tools: T,
        permission_policy: P,
    ) -> Self {
        Self {
            config,
            client,
            session,
            tools,
            permission_policy,
            transcript: Vec::new(),
        }
    }
}

impl<C, R, T, P> AgentRuntime<C, R, T, P>
where
    C: ModelClient,
    R: SessionStore + Sync,
    T: ToolRegistry,
    P: PermissionPolicy,
{
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    pub fn session_id(&self) -> Uuid {
        self.session.id()
    }

    pub fn session_path(&self) -> &std::path::Path {
        self.session.path().as_path()
    }

    pub fn context_stats(&self) -> ContextStats {
        self.build_model_context().stats
    }

    pub async fn compact_context(&mut self) -> Result<ContextCompactReport> {
        let before_context = self.build_model_context();
        self.record_context_snapshot(&before_context.stats)?;
        let messages_replaced = self.transcript.len();

        let request = ModelRequest {
            model: self.config.model.clone(),
            input: before_context.input,
            tools: Vec::new(),
            instructions: compact_instructions(&before_context.instructions),
            parallel_tool_calls: false,
            thinking: self.config.thinking.map(|value| value.to_string()),
            reasoning_effort: if self.config.thinking == Some(ThinkingMode::Disabled) {
                None
            } else {
                self.config.reasoning_effort.map(|value| value.to_string())
            },
        };

        let response = match self.client.respond(request).await {
            Ok(response) => response,
            Err(error) => {
                self.session.append(&SessionEvent::Error {
                    timestamp: now(),
                    message: error.to_string(),
                })?;
                return Err(error);
            }
        };
        let summary = response.assistant_text.join("\n").trim().to_string();
        if summary.is_empty() {
            let error = anyhow::anyhow!("compact produced an empty summary");
            self.session.append(&SessionEvent::Error {
                timestamp: now(),
                message: error.to_string(),
            })?;
            return Err(error);
        }

        let summary_tokens = estimate_text_tokens(&summary);
        self.transcript = vec![compacted_summary_message(&summary)];
        let after_context = self.build_model_context();
        self.record_context_snapshot(&after_context.stats)?;
        self.session.append(&SessionEvent::ContextCompacted {
            timestamp: now(),
            before_tokens: before_context.stats.total_tokens_estimate,
            after_tokens: after_context.stats.total_tokens_estimate,
            summary_tokens,
            messages_replaced,
        })?;

        Ok(ContextCompactReport {
            before_tokens: before_context.stats.total_tokens_estimate,
            after_tokens: after_context.stats.total_tokens_estimate,
            summary_tokens,
            messages_replaced,
        })
    }

    pub async fn compact_context_with_ui<S: UiSink + Send>(
        &mut self,
        _ui: &mut S,
    ) -> Result<ContextCompactReport> {
        self.compact_context().await
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
            let reasoning_effort = if self.config.thinking == Some(ThinkingMode::Disabled) {
                None
            } else {
                self.config.reasoning_effort.map(|value| value.to_string())
            };
            let model_context = self.build_model_context();
            self.record_context_snapshot(&model_context.stats)?;
            let request = ModelRequest {
                model: self.config.model.clone(),
                input: model_context.input,
                tools: model_context.tools,
                instructions: model_context.instructions,
                parallel_tool_calls: false,
                thinking: self.config.thinking.map(|value| value.to_string()),
                reasoning_effort,
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
                let result = self
                    .execute_tool(&call.call_id, &call.name, arguments.clone(), ui)
                    .await;
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
        &mut self,
        call_id: &str,
        name: &str,
        arguments: Value,
        ui: &mut S,
    ) -> ToolResult {
        let metadata = self
            .tools
            .metadata(name, &arguments)
            .unwrap_or_else(|| crate::tools::ToolMetadata::unknown("unknown", &arguments));
        let start = Instant::now();
        let decision =
            self.permission_policy
                .decide(self.config.permission, &metadata, name, &arguments);
        if let Err(error) = self.record_permission_decision(
            call_id,
            name,
            &metadata.argument_summary,
            &decision,
            start.elapsed().as_millis(),
        ) {
            return ToolResult::error(error.to_string());
        }

        match decision.decision {
            PermissionDecision::Allow => {}
            PermissionDecision::Ask => {
                let approval_start = Instant::now();
                match ui.approve_tool(name, &metadata.argument_summary) {
                    Ok(ApprovalDecision::AllowOnce) => {
                        let runtime = PolicyDecision {
                            decision: PermissionDecision::Allow,
                            reason: DecisionReason::RuntimeApproval,
                            rule_source: Some(RuleSource::Session),
                            message: format!("{name} approved once by user"),
                        };
                        if let Err(error) = self.record_permission_decision(
                            call_id,
                            name,
                            &metadata.argument_summary,
                            &runtime,
                            approval_start.elapsed().as_millis(),
                        ) {
                            return ToolResult::error(error.to_string());
                        }
                    }
                    Ok(ApprovalDecision::AllowSession) => {
                        let runtime = PolicyDecision {
                            decision: PermissionDecision::Allow,
                            reason: DecisionReason::RuntimeApproval,
                            rule_source: Some(RuleSource::Session),
                            message: format!("{name} approved for this session"),
                        };
                        if let Err(error) = self.record_permission_decision(
                            call_id,
                            name,
                            &metadata.argument_summary,
                            &runtime,
                            approval_start.elapsed().as_millis(),
                        ) {
                            return ToolResult::error(error.to_string());
                        }
                        if let Err(error) = self.add_approval_rule(
                            RuleSource::Session,
                            call_id,
                            name,
                            &arguments,
                            &metadata.argument_summary,
                        ) {
                            return ToolResult::error(error.to_string());
                        }
                    }
                    Ok(ApprovalDecision::AllowProject) => {
                        let runtime = PolicyDecision {
                            decision: PermissionDecision::Allow,
                            reason: DecisionReason::RuntimeApproval,
                            rule_source: Some(RuleSource::Session),
                            message: format!("{name} approved by user"),
                        };
                        if let Err(error) = self.record_permission_decision(
                            call_id,
                            name,
                            &metadata.argument_summary,
                            &runtime,
                            approval_start.elapsed().as_millis(),
                        ) {
                            return ToolResult::error(error.to_string());
                        }
                        if let Err(error) = self.add_approval_rule(
                            RuleSource::Config,
                            call_id,
                            name,
                            &arguments,
                            &metadata.argument_summary,
                        ) {
                            return ToolResult::error(error.to_string());
                        }
                    }
                    Ok(ApprovalDecision::Deny) => {
                        let runtime = PolicyDecision {
                            decision: PermissionDecision::Deny,
                            reason: DecisionReason::RuntimeApproval,
                            rule_source: Some(RuleSource::Session),
                            message: format!("{name} denied by user"),
                        };
                        if let Err(error) = self.record_permission_decision(
                            call_id,
                            name,
                            &metadata.argument_summary,
                            &runtime,
                            approval_start.elapsed().as_millis(),
                        ) {
                            return ToolResult::error(error.to_string());
                        }
                        return ToolResult::denied(runtime.message);
                    }
                    Err(error) => return ToolResult::error(error.to_string()),
                }
            }
            PermissionDecision::Deny => return ToolResult::denied(decision.message),
        }

        self.tools
            .execute(
                name,
                arguments,
                ToolContext {
                    cwd: self.config.cwd.clone(),
                    permission: self.config.permission,
                },
            )
            .await
    }

    fn record_permission_decision(
        &self,
        call_id: &str,
        tool: &str,
        argument_summary: &str,
        decision: &PolicyDecision,
        elapsed_ms: u128,
    ) -> Result<()> {
        self.session.append(&SessionEvent::PermissionDecision {
            timestamp: now(),
            call_id: call_id.to_string(),
            tool: tool.to_string(),
            argument_summary: argument_summary.to_string(),
            decision: decision.decision,
            reason: decision.reason,
            rule_source: decision.rule_source,
            permission_mode: self.config.permission,
            elapsed_ms,
            message: Some(decision.message.clone()),
        })
    }

    fn build_model_context(&self) -> crate::context::ModelContext {
        let tools = self
            .tools
            .schemas_for_policy(&self.permission_policy, self.config.permission);
        ContextBuilder::new(self.config.context_window_tokens).build(
            self.transcript.clone(),
            PromptBuilder::build(&self.config),
            tools,
        )
    }

    fn record_context_snapshot(&self, stats: &ContextStats) -> Result<()> {
        self.session.append(&SessionEvent::ContextSnapshot {
            timestamp: now(),
            model: self.config.model.clone(),
            estimated_tokens: stats.total_tokens_estimate,
            max_tokens: stats.max_tokens,
            usage_percent: stats.usage_percent,
            categories: stats.categories.clone(),
        })
    }

    fn add_approval_rule(
        &mut self,
        source: RuleSource,
        call_id: &str,
        tool: &str,
        arguments: &Value,
        argument_summary: &str,
    ) -> Result<()> {
        let rule_text = suggest_approval_rule(tool, arguments);
        let rule = PermissionRule::parse(source, RuleBehavior::Allow, &rule_text)?;
        if source == RuleSource::Config {
            save_permission_rule(&self.config.cwd, RuleBehavior::Allow, &rule_text)?;
        }
        if !self.config.permission_rules.contains(&rule) {
            self.config.permission_rules.push(rule.clone());
        }
        self.permission_policy.add_rule(rule);
        let update = PolicyDecision {
            decision: PermissionDecision::Allow,
            reason: DecisionReason::RuntimeApproval,
            rule_source: Some(source),
            message: format!("added permission rule: {rule_text}"),
        };
        self.record_permission_decision(call_id, tool, argument_summary, &update, 0)
    }
}

impl<R, T, P> AgentRuntime<OpenAiModelClient, R, T, P>
where
    R: SessionStore,
{
    pub fn apply_model_settings(&mut self, settings: ModelSettings) -> Result<()> {
        self.config.apply_model_settings(&settings);
        self.client
            .update_endpoint(self.config.api_kind, self.config.base_url.clone());
        self.session.append(&SessionEvent::ConfigChanged {
            timestamp: now(),
            model: self.config.model.clone(),
            base_url: self.config.base_url.clone(),
            thinking: self.config.thinking,
            reasoning_effort: self.config.reasoning_effort,
        })
    }
}

struct SessionRecordingUi<'a, R, S> {
    session: &'a R,
    inner: &'a mut S,
}

impl<R, S> UiSink for SessionRecordingUi<'_, R, S>
where
    R: SessionStore,
    S: UiSink,
{
    fn on_event(&mut self, event: AgentEvent) -> Result<()> {
        match &event {
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

    fn approve_tool(&mut self, name: &str, summary: &str) -> Result<ApprovalDecision> {
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

fn suggest_approval_rule(tool: &str, arguments: &Value) -> String {
    match tool {
        "write_file" => suggest_write_file_rule(arguments),
        "shell" => suggest_shell_rule(arguments),
        _ => tool.to_string(),
    }
}

fn suggest_write_file_rule(arguments: &Value) -> String {
    let path = arguments
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .trim_start_matches("./");
    if path.is_empty() || path.contains(['(', ')']) {
        return "write_file".into();
    }
    let first = path.split('/').next().unwrap_or(path);
    if matches!(first, "src" | "tests" | "docs" | "assets") && path.contains('/') {
        format!("write_file({first}/*)")
    } else {
        format!("write_file({path})")
    }
}

fn suggest_shell_rule(arguments: &Value) -> String {
    let command = arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if command.is_empty() || command.contains(['(', ')']) {
        return "shell".into();
    }
    if command.starts_with("git status") {
        return "shell(git status*)".into();
    }
    if command.starts_with("git diff") {
        return "shell(git diff*)".into();
    }
    format!("shell({command})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PermissionMode, SessionConfig};
    use crate::model::{ModelFunctionCall, ModelRequest, ModelResponse};
    use crate::tools::{PermissionRule, RuleBehavior};
    use crate::ui::ApprovalDecision;
    use anyhow::anyhow;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    struct MockModel {
        responses: Arc<Mutex<VecDeque<anyhow::Result<ModelResponse>>>>,
        requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    struct ApprovalUi {
        approvals: Arc<Mutex<VecDeque<ApprovalDecision>>>,
        calls: Arc<Mutex<usize>>,
    }

    impl ApprovalUi {
        fn new(approvals: Vec<ApprovalDecision>) -> Self {
            Self {
                approvals: Arc::new(Mutex::new(approvals.into())),
                calls: Arc::new(Mutex::new(0)),
            }
        }

        fn call_count(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    impl UiSink for ApprovalUi {
        fn on_event(&mut self, _event: AgentEvent) -> Result<()> {
            Ok(())
        }

        fn approve_tool(&mut self, _name: &str, _summary: &str) -> Result<ApprovalDecision> {
            *self.calls.lock().unwrap() += 1;
            Ok(self
                .approvals
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(ApprovalDecision::Deny))
        }
    }

    impl MockModel {
        fn new(responses: Vec<anyhow::Result<ModelResponse>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl ModelClient for MockModel {
        async fn respond(&self, request: ModelRequest) -> anyhow::Result<ModelResponse> {
            self.requests.lock().unwrap().push(request);
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
            permission_rules: Vec::new(),
            max_steps,
            context_window_tokens: crate::context::DEFAULT_CONTEXT_WINDOW_TOKENS,
            append_system_prompt: None,
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

    fn test_agent_with_config(
        config: SessionConfig,
        responses: Vec<anyhow::Result<ModelResponse>>,
    ) -> Agent<MockModel> {
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
    async fn disabled_thinking_omits_reasoning_effort() {
        let mut agent = test_agent(PermissionMode::Safe, 3, vec![Ok(final_response("done"))]);
        agent.config.thinking = Some(ThinkingMode::Disabled);
        agent.config.reasoning_effort = Some(crate::config::ReasoningEffort::Max);

        let reason = agent.run_turn("hello".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        let requests = agent.client.requests.lock().unwrap();
        assert_eq!(requests[0].thinking.as_deref(), Some("disabled"));
        assert_eq!(requests[0].reasoning_effort, None);
    }

    #[tokio::test]
    async fn ordinary_request_uses_prompt_builder_with_append_prompt() {
        let mut config = temp_config(PermissionMode::Safe, 3);
        config.append_system_prompt = Some("Prefer project-specific wording.".into());
        let mut agent = test_agent_with_config(config, vec![Ok(final_response("done"))]);

        let reason = agent.run_turn("hello".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        let requests = agent.client.requests.lock().unwrap();
        assert!(requests[0].instructions.contains("## Identity"));
        assert!(requests[0]
            .instructions
            .contains("Read relevant files before changing code"));
        assert!(requests[0]
            .instructions
            .contains("## Project additional instructions"));
        assert!(requests[0]
            .instructions
            .contains("Prefer project-specific wording."));
    }

    #[tokio::test]
    async fn compact_replaces_visible_transcript_with_summary() {
        let summary = "## Primary Request and Intent\nContinue the task.\n\n## Key Technical Concepts\nPrompt governance.\n\n## Files and Code Sections\nsrc/agent.rs.\n\n## Errors and Fixes\nNone.\n\n## Decisions Made\nUse manual compact.\n\n## Pending Tasks\nRun tests.\n\n## Current Work\nImplementing compact.\n\n## Next Step\nVerify.";
        let mut agent = test_agent(
            PermissionMode::Safe,
            3,
            vec![
                Ok(final_response("first answer")),
                Ok(final_response(summary)),
                Ok(final_response("done")),
            ],
        );
        let path = agent.session.path().clone();

        let reason = agent.run_turn("start".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        let report = agent.compact_context().await.unwrap();
        assert_eq!(report.messages_replaced, 2);
        assert!(report.before_tokens > report.summary_tokens);
        assert!(report.after_tokens > report.summary_tokens);

        let reason = agent.run_turn("continue".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        let requests = agent.client.requests.lock().unwrap();
        assert!(requests[1].tools.is_empty());
        assert!(requests[1].instructions.contains("## Compact task"));
        assert_eq!(requests[2].input.len(), 2);
        assert!(requests[2].input[0]
            .to_string()
            .contains("This is a compacted summary of earlier model-visible context"));
        assert!(requests[2].input[0].to_string().contains("## Next Step"));

        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("\"type\":\"context_compacted\""));
        assert!(log.contains("\"messages_replaced\":2"));
        assert!(log.contains("\"summary_tokens\""));
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
        assert!(log.contains("\"type\":\"context_snapshot\""));
        assert!(log.contains("\"estimated_tokens\""));
        assert!(log.contains("\"name\":\"tool_outputs\""));
        assert!(log.contains("\"type\":\"permission_decision\""));
        assert!(log.contains("\"decision\":\"allow\""));
        assert!(log.contains("\"reason\":\"tool\""));
        assert!(log.contains("\"type\":\"tool_output\""));
        assert!(log.contains("\"type\":\"assistant_text\""));
        assert!(!log.contains("\"type\":\"assistant_delta\""));
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
    async fn whole_tool_deny_filters_model_schema() {
        let mut config = temp_config(PermissionMode::Ask, 3);
        config.permission_rules =
            vec![PermissionRule::parse(RuleSource::Config, RuleBehavior::Deny, "shell").unwrap()];
        let mut agent = test_agent_with_config(config, vec![Ok(final_response("done"))]);

        let reason = agent.run_turn("hello".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        let requests = agent.client.requests.lock().unwrap();
        let tool_names = requests[0]
            .tools
            .iter()
            .map(|schema| schema["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(tool_names, vec!["list_files", "read_file", "write_file"]);
    }

    #[tokio::test]
    async fn scoped_shell_deny_keeps_schema_and_records_trace() {
        let mut config = temp_config(PermissionMode::Auto, 3);
        config.permission_rules =
            vec![
                PermissionRule::parse(RuleSource::Config, RuleBehavior::Deny, "shell(rm *)")
                    .unwrap(),
            ];
        let path_config = config.clone();
        let mut agent = test_agent_with_config(
            config,
            vec![Ok(tool_response(
                "shell",
                "call_1",
                json!({"command":"rm -rf x"}),
            ))],
        );
        let path = agent.session.path().clone();

        let reason = agent.run_turn("remove".into()).await.unwrap();
        assert_eq!(reason, StopReason::ToolDenied);
        let requests = agent.client.requests.lock().unwrap();
        let tool_names = requests[0]
            .tools
            .iter()
            .map(|schema| schema["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            tool_names,
            vec!["list_files", "read_file", "write_file", "shell"]
        );
        assert_eq!(path_config.permission_rules.len(), 1);

        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("\"type\":\"permission_decision\""));
        assert!(log.contains("\"tool\":\"shell\""));
        assert!(log.contains("\"argument_summary\":\"command=rm -rf x\""));
        assert!(log.contains("\"decision\":\"deny\""));
        assert!(log.contains("\"reason\":\"rule\""));
        assert!(log.contains("\"rule_source\":\"config\""));
        assert!(log.contains("\"elapsed_ms\""));
    }

    #[test]
    fn suggests_permission_rules_for_common_tools() {
        assert_eq!(
            suggest_approval_rule("write_file", &json!({"path":"src/agent.rs"})),
            "write_file(src/*)"
        );
        assert_eq!(
            suggest_approval_rule("write_file", &json!({"path":"README.md"})),
            "write_file(README.md)"
        );
        assert_eq!(
            suggest_approval_rule("shell", &json!({"command":"git diff -- src/agent.rs"})),
            "shell(git diff*)"
        );
        assert_eq!(
            suggest_approval_rule("shell", &json!({"command":"cargo test"})),
            "shell(cargo test)"
        );
    }

    #[tokio::test]
    async fn session_approval_allows_matching_later_tool_without_prompt() {
        let config = temp_config(PermissionMode::Ask, 5);
        let mut agent = test_agent_with_config(
            config,
            vec![
                Ok(tool_response(
                    "write_file",
                    "call_1",
                    json!({"path":"src/a.rs","content":"a"}),
                )),
                Ok(tool_response(
                    "write_file",
                    "call_2",
                    json!({"path":"src/b.rs","content":"b"}),
                )),
                Ok(final_response("done")),
            ],
        );
        let path = agent.session.path().clone();
        let mut ui = ApprovalUi::new(vec![ApprovalDecision::AllowSession]);

        let reason = agent
            .run_turn_with_ui("write files".into(), &mut ui)
            .await
            .unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        assert_eq!(ui.call_count(), 1);

        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("added permission rule: write_file(src/*)"));
        assert!(log.contains("\"rule_source\":\"session\""));
    }

    #[tokio::test]
    async fn project_approval_writes_config_rule() {
        let config = temp_config(PermissionMode::Ask, 3);
        let config_path = config.cwd.join(".micos/config.toml");
        let mut agent = test_agent_with_config(
            config,
            vec![
                Ok(tool_response(
                    "write_file",
                    "call_1",
                    json!({"path":"README.md","content":"readme"}),
                )),
                Ok(final_response("done")),
            ],
        );
        let mut ui = ApprovalUi::new(vec![ApprovalDecision::AllowProject]);

        let reason = agent
            .run_turn_with_ui("write readme".into(), &mut ui)
            .await
            .unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);

        let text = std::fs::read_to_string(config_path).unwrap();
        assert!(text.contains("[permissions]"));
        assert!(text.contains("allow = [\"write_file(README.md)\"]"));
    }

    #[tokio::test]
    async fn deny_rule_does_not_prompt_for_approval() {
        let mut config = temp_config(PermissionMode::Ask, 3);
        config.permission_rules =
            vec![
                PermissionRule::parse(RuleSource::Config, RuleBehavior::Deny, "shell(rm *)")
                    .unwrap(),
            ];
        let mut agent = test_agent_with_config(
            config,
            vec![Ok(tool_response(
                "shell",
                "call_1",
                json!({"command":"rm -rf x"}),
            ))],
        );
        let mut ui = ApprovalUi::new(vec![ApprovalDecision::AllowSession]);

        let reason = agent
            .run_turn_with_ui("remove".into(), &mut ui)
            .await
            .unwrap();
        assert_eq!(reason, StopReason::ToolDenied);
        assert_eq!(ui.call_count(), 0);
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
