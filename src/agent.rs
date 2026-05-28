mod approval;
mod checkpoint;
mod compact;
mod model_output;

use crate::config::{
    save_permission_rule, ModelSettings, PermissionMode, SessionConfig, ThinkingMode,
};
use crate::context::{
    compacted_summary_message, estimate_text_tokens, ContextBuilder, ContextStats,
};
use crate::memory::{MemoryCandidateReport, MemoryEntry, MemoryStatus, ProjectMemory};
use crate::model::{ModelClient, ModelRequest, OpenAiModelClient};
use crate::plan::{self, ActivePlan, HandoffReport};
use crate::prompt::{
    compact_instructions, compact_repair_instructions, PromptBuild, PromptBuilder,
    PromptRuntimeContext,
};
use crate::recovery::{self, RecoveryReport};
use crate::session::{now, Session, SessionEvent, SessionStore, StopReason};
use crate::session_replay::{replay_session, resolve_session_target, SessionResumeReport};
use crate::tools::{
    BuiltinToolRegistry, DecisionReason, ModePermissionPolicy, PermissionDecision,
    PermissionPolicy, PermissionRule, PolicyDecision, PolicyEngine, RuleBehavior, RuleSource,
    ToolContext, ToolRegistry, ToolResult,
};
#[cfg(test)]
use crate::ui::NullUi;
use crate::ui::{AgentEvent, ApprovalDecision, UiSink};
use crate::verify::{
    load_verification_checks, select_verification_checks, shell_exit_code, VerificationCheckReport,
    VerificationRunReport,
};
use anyhow::{Context, Result};
use approval::suggest_approval_rule;
use compact::{
    compression_ratio_percent, normalize_compact_summary, validate_compact_summary, CompactPlan,
    COMPACT_SUMMARY_FORMAT_VERSION,
};
use model_output::function_call_output;
#[cfg(test)]
use model_output::MODEL_VISIBLE_TOOL_OUTPUT_LIMIT;
use serde_json::{json, Value};
use std::time::Instant;
use uuid::Uuid;

pub type Agent<C> = AgentRuntime<C, Session, BuiltinToolRegistry, ModePermissionPolicy>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextCompactReport {
    pub compacted: bool,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub summary_tokens: usize,
    pub messages_replaced: usize,
    pub retained_messages: usize,
    pub compression_ratio_percent: usize,
    pub validation_status: String,
}

pub struct AgentRuntime<C, R = Session, T = BuiltinToolRegistry, P = ModePermissionPolicy> {
    config: SessionConfig,
    client: C,
    session: R,
    tools: T,
    permission_policy: P,
    transcript: Vec<Value>,
    project_memory: Option<ProjectMemory>,
    active_plan: Option<ActivePlan>,
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
            project_memory: None,
            active_plan: None,
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
            project_memory: None,
            active_plan: None,
        }
    }
}

impl<C, R, T, P> AgentRuntime<C, R, T, P>
where
    R: SessionStore,
{
    pub fn apply_permission_mode(&mut self, permission: PermissionMode) -> Result<()> {
        self.config.permission = permission;
        self.session.append(&SessionEvent::PermissionModeChanged {
            timestamp: now(),
            permission,
        })
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

    pub fn session(&self) -> &R {
        &self.session
    }

    pub fn context_stats(&self) -> ContextStats {
        self.build_model_context().stats
    }

    pub fn prompt_build(&self) -> PromptBuild {
        PromptBuilder::build(&self.config, &self.prompt_runtime_context())
    }

    pub fn project_memory(&self) -> Option<&ProjectMemory> {
        self.project_memory.as_ref()
    }

    pub fn active_plan(&self) -> Option<&ActivePlan> {
        self.active_plan.as_ref()
    }

    pub fn install_project_memory(&mut self, memory: ProjectMemory) -> Result<()> {
        self.session.append(&SessionEvent::MemoryLoaded {
            timestamp: now(),
            root: memory.root.clone(),
            index_path: memory.index_path.clone(),
            index_tokens: memory.index_tokens,
            topic_count: memory.topics.len(),
            candidate_count: memory.candidates.len(),
            entry_count: memory.entries.len(),
            active_entry_count: memory.active_entries().len(),
            active_entry_tokens: memory.active_entries_tokens,
            created_index: memory.created_index,
        })?;
        self.project_memory = Some(memory);
        Ok(())
    }

    pub fn refresh_memory_candidates(&mut self) -> Result<MemoryCandidateReport> {
        let memory = self
            .project_memory
            .as_mut()
            .context("project memory is not loaded")?;
        let report = memory.refresh_candidates_from_session(self.session.path(), now())?;
        self.session.append(&SessionEvent::MemoryCandidateCreated {
            timestamp: now(),
            id: report.candidate.id.clone(),
            title: report.candidate.title.clone(),
            source_session: report.candidate.source_session.clone(),
            created: report.created,
        })?;
        Ok(report)
    }

    pub fn promote_memory_candidate(&mut self, id: &str) -> Result<MemoryEntry> {
        let memory = self
            .project_memory
            .as_mut()
            .context("project memory is not loaded")?;
        let entry = memory.promote_candidate(id, now())?;
        self.session.append(&SessionEvent::MemoryPromoted {
            timestamp: now(),
            id: entry.id.clone(),
            title: entry.title.clone(),
            source_session: entry.source_session.clone(),
        })?;
        Ok(entry)
    }

    pub fn mark_memory_stale(&mut self, id: &str) -> Result<MemoryEntry> {
        self.mark_memory_entry_status(id, MemoryStatus::Stale)
    }

    pub fn forget_memory(&mut self, id: &str) -> Result<String> {
        let memory = self
            .project_memory
            .as_mut()
            .context("project memory is not loaded")?;
        if memory.entries.iter().any(|entry| entry.id == id) {
            let entry = memory.mark_entry_status(id, MemoryStatus::Forgotten)?;
            self.session.append(&SessionEvent::MemoryStatusChanged {
                timestamp: now(),
                id: entry.id.clone(),
                status: entry.status.to_string(),
                title: entry.title.clone(),
            })?;
            return Ok(format!("forgotten entry {}", entry.id));
        }
        let candidate = memory.forget_candidate(id)?;
        self.session.append(&SessionEvent::MemoryStatusChanged {
            timestamp: now(),
            id: candidate.id.clone(),
            status: candidate.status.to_string(),
            title: candidate.title.clone(),
        })?;
        Ok(format!("forgotten candidate {}", candidate.id))
    }

    pub fn install_active_plan(&mut self, active_plan: ActivePlan) {
        self.active_plan = Some(active_plan);
    }

    pub fn write_handoff(&mut self, trigger: &str) -> Result<HandoffReport> {
        let timestamp = now();
        let (active_plan, report) = plan::write_handoff(
            &self.config.cwd,
            self.session.path(),
            trigger,
            timestamp.clone(),
        )?;
        self.session.append(&SessionEvent::HandoffWritten {
            timestamp,
            path: report.path.clone(),
            trigger: report.trigger.clone(),
            files_touched: report.files_touched,
            commands_run: report.commands_run,
            verification_status: report.verification_status.clone(),
            known_failures: report.known_failures,
            tokens_estimate: report.tokens_estimate,
        })?;
        self.active_plan = Some(active_plan);
        Ok(report)
    }

    pub fn write_recovery_report(&mut self, trigger: &str) -> Result<RecoveryReport> {
        let timestamp = now();
        let report = recovery::write_recovery_report(
            &self.config.cwd,
            self.session.path(),
            trigger,
            timestamp.clone(),
        )?;
        self.session.append(&SessionEvent::RecoveryReportWritten {
            timestamp,
            path: report.path.clone(),
            trigger: report.trigger.clone(),
            stop_reason: report.stop_reason.clone(),
            failure_class: report.failure_class.to_string(),
            known_failures: report.known_failures,
            tokens_estimate: report.tokens_estimate,
        })?;
        Ok(report)
    }

    pub fn resume_session(&mut self, target: &str) -> Result<SessionResumeReport> {
        let source_path = resolve_session_target(&self.config.cwd, target)?;
        let replay = replay_session(&source_path)?;
        self.transcript = replay.transcript.clone();
        let context = self.build_model_context();
        let mut report = SessionResumeReport::from_replay(&replay);
        report.estimated_tokens = context.stats.total_tokens_estimate;
        self.session.append(&SessionEvent::SessionResumed {
            timestamp: now(),
            source_session_id: report.source_session_id,
            source_path: report.source_path.clone(),
            restored_messages: report.restored_messages,
            used_summary: report.used_summary,
            restored_tail_messages: report.restored_tail_messages,
            estimated_tokens: report.estimated_tokens,
        })?;
        Ok(report)
    }

    pub async fn compact_context(&mut self) -> Result<ContextCompactReport> {
        if self.transcript.is_empty() {
            let stats = self.build_model_context().stats;
            return Ok(ContextCompactReport {
                compacted: false,
                before_tokens: stats.total_tokens_estimate,
                after_tokens: stats.total_tokens_estimate,
                summary_tokens: 0,
                messages_replaced: 0,
                retained_messages: 0,
                compression_ratio_percent: 0,
                validation_status: "skipped_empty_transcript".into(),
            });
        }

        let before_context = self.build_model_context();
        let compact_plan = CompactPlan::from_transcript(&self.transcript);
        if compact_plan.messages_replaced == 0 {
            return Ok(ContextCompactReport {
                compacted: false,
                before_tokens: before_context.stats.total_tokens_estimate,
                after_tokens: before_context.stats.total_tokens_estimate,
                summary_tokens: 0,
                messages_replaced: 0,
                retained_messages: compact_plan.retained_messages,
                compression_ratio_percent: 0,
                validation_status: "skipped_small_transcript".into(),
            });
        }
        self.record_context_snapshot(&before_context)?;
        let messages_replaced = compact_plan.messages_replaced;

        let request = self.compact_model_request(
            compact_plan.summary_input.clone(),
            compact_instructions(&before_context.instructions),
        );

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
        let mut summary = normalize_compact_summary(&response.assistant_text.join("\n"));
        let mut validation =
            validate_compact_summary(&summary, compact_plan.latest_user_text.as_deref());
        let mut validation_status = validation.status.clone();
        if !validation.passed {
            let repair_reason = validation.message.clone();
            let repair_request = self.compact_model_request(
                compact_plan.summary_input.clone(),
                compact_repair_instructions(&before_context.instructions, &repair_reason, &summary),
            );
            let repair_response = match self.client.respond(repair_request).await {
                Ok(response) => response,
                Err(error) => {
                    self.session.append(&SessionEvent::Error {
                        timestamp: now(),
                        message: error.to_string(),
                    })?;
                    return Err(error);
                }
            };
            summary = normalize_compact_summary(&repair_response.assistant_text.join("\n"));
            validation =
                validate_compact_summary(&summary, compact_plan.latest_user_text.as_deref());
            if !validation.passed {
                let error = anyhow::anyhow!(
                    "compact summary validation failed after repair: {}",
                    validation.message
                );
                self.session.append(&SessionEvent::Error {
                    timestamp: now(),
                    message: error.to_string(),
                })?;
                return Err(error);
            }
            validation_status = format!("repaired:{repair_reason}");
        }

        let summary_tokens = estimate_text_tokens(&summary);
        self.session.append(&SessionEvent::ContextSummary {
            timestamp: now(),
            summary: summary.clone(),
            summary_tokens,
            messages_replaced,
            retained_messages: compact_plan.retained_messages,
            summary_format_version: COMPACT_SUMMARY_FORMAT_VERSION,
            trigger: "manual".into(),
        })?;
        self.transcript = std::iter::once(compacted_summary_message(&summary))
            .chain(compact_plan.retained_tail)
            .collect();
        let after_context = self.build_model_context();
        self.record_context_snapshot(&after_context)?;
        let compression_ratio_percent = compression_ratio_percent(
            before_context.stats.total_tokens_estimate,
            after_context.stats.total_tokens_estimate,
        );
        self.session.append(&SessionEvent::ContextCompacted {
            timestamp: now(),
            before_tokens: before_context.stats.total_tokens_estimate,
            after_tokens: after_context.stats.total_tokens_estimate,
            summary_tokens,
            messages_replaced,
            retained_messages: compact_plan.retained_messages,
            compression_ratio_percent,
            validation_status: validation_status.clone(),
        })?;
        let _ = self.write_handoff("compact")?;

        Ok(ContextCompactReport {
            compacted: true,
            before_tokens: before_context.stats.total_tokens_estimate,
            after_tokens: after_context.stats.total_tokens_estimate,
            summary_tokens,
            messages_replaced,
            retained_messages: compact_plan.retained_messages,
            compression_ratio_percent,
            validation_status,
        })
    }

    fn compact_model_request(&self, input: Vec<Value>, instructions: String) -> ModelRequest {
        ModelRequest {
            model: self.config.model.clone(),
            input,
            tools: Vec::new(),
            instructions,
            parallel_tool_calls: false,
            thinking: self.config.thinking.map(|value| value.to_string()),
            reasoning_effort: if self.config.thinking == Some(ThinkingMode::Disabled) {
                None
            } else {
                self.config.reasoning_effort.map(|value| value.to_string())
            },
        }
    }

    pub async fn compact_context_with_ui<S: UiSink + Send>(
        &mut self,
        _ui: &mut S,
    ) -> Result<ContextCompactReport> {
        self.compact_context().await
    }

    pub async fn run_verification_with_ui<S: UiSink + Send>(
        &mut self,
        check_name: Option<&str>,
        ui: &mut S,
    ) -> Result<VerificationRunReport> {
        let checks = load_verification_checks(&self.config.cwd)?;
        let checks = select_verification_checks(&checks, check_name)?;
        if checks.is_empty() {
            anyhow::bail!("no verification checks configured");
        }

        let mut reports = Vec::new();
        for (index, check) in checks.into_iter().enumerate() {
            self.session.append(&SessionEvent::VerificationStarted {
                timestamp: now(),
                name: check.name.clone(),
                command: check.command.clone(),
            })?;
            let start = Instant::now();
            let result = self
                .execute_tool(
                    &format!("verify_{}_{}", sanitize_call_id(&check.name), index),
                    "shell",
                    json!({ "command": check.command }),
                    ui,
                )
                .await;
            let elapsed = elapsed_millis_u64(start.elapsed());
            let exit_code = shell_exit_code(&result.output);
            let success = result.success && !result.denied && exit_code.unwrap_or(0) == 0;
            let output_preview = result
                .error
                .clone()
                .filter(|error| !error.is_empty())
                .unwrap_or_else(|| result.output.clone());
            self.session.append(&SessionEvent::VerificationFinished {
                timestamp: now(),
                name: check.name.clone(),
                command: check.command.clone(),
                success,
                exit_code,
                elapsed_ms: elapsed,
                output_preview: output_preview.clone(),
                truncated: result.truncated,
            })?;
            reports.push(VerificationCheckReport {
                name: check.name,
                command: check.command,
                success,
                exit_code,
                elapsed_ms: elapsed,
                output_preview,
                truncated: result.truncated,
            });
        }

        if reports.iter().any(|report| !report.success) {
            let _ = self.write_recovery_report("verification_failed");
        }

        Ok(VerificationRunReport { checks: reports })
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
            self.record_context_snapshot(&model_context)?;
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
                    self.write_handoff(&StopReason::ApiError.to_string())?;
                    let _ = self.write_recovery_report(&StopReason::ApiError.to_string());
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
                    truncated: result.truncated,
                    original_bytes: result.original_bytes,
                    preview_bytes: result.preview_bytes,
                })?;
                self.session.append(&SessionEvent::ToolFinished {
                    timestamp: now(),
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    success: result.success,
                    output: result.output.clone(),
                    error: result.error.clone(),
                    elapsed_ms: elapsed_millis_u64(elapsed),
                    truncated: result.truncated,
                    original_bytes: result.original_bytes,
                    preview_bytes: result.preview_bytes,
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
                    self.write_handoff(&StopReason::ToolDenied.to_string())?;
                    let _ = self.write_recovery_report(&StopReason::ToolDenied.to_string());
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
                        self.write_handoff(&StopReason::ToolError.to_string())?;
                        let _ = self.write_recovery_report(&StopReason::ToolError.to_string());
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
        self.write_handoff(&StopReason::MaxSteps.to_string())?;
        let _ = self.write_recovery_report(&StopReason::MaxSteps.to_string());
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
        let decision = self.permission_policy.decide(
            self.config.permission,
            &metadata,
            name,
            &arguments,
            &self.config.cwd,
        );
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

        if !metadata.read_only {
            let _ = checkpoint::record_before_tool(
                &self.session,
                &self.config.cwd,
                call_id,
                name,
                &metadata.argument_summary,
            );
        }
        let result = self
            .tools
            .execute(
                name,
                arguments,
                ToolContext {
                    cwd: self.config.cwd.clone(),
                    permission: self.config.permission,
                },
            )
            .await;
        if !metadata.read_only {
            let _ = checkpoint::record_after_tool(
                &self.session,
                &self.config.cwd,
                call_id,
                name,
                &result,
            );
        }
        result
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
            elapsed_ms: millis_u64(elapsed_ms),
            message: Some(decision.message.clone()),
        })
    }

    fn build_model_context(&self) -> crate::context::ModelContext {
        let tools = self
            .tools
            .schemas_for_policy(&self.permission_policy, self.config.permission);
        ContextBuilder::new(self.config.context_window_tokens).build(
            self.transcript.clone(),
            self.prompt_build(),
            tools,
        )
    }

    fn prompt_runtime_context(&self) -> PromptRuntimeContext {
        let mut runtime = PromptRuntimeContext::from_config(&self.config);
        if let Some(memory) = self.project_memory.as_ref() {
            let mut parts = Vec::new();
            if let Some(index) = memory.active_index_text() {
                parts.push(index.to_string());
            }
            if let Some(entries) = memory.active_entries_text() {
                parts.push(format!("## Accepted durable memory\n{entries}"));
            }
            if !parts.is_empty() {
                runtime = runtime.with_project_memory(&memory.index_path, parts.join("\n\n"));
            }
        }
        if let Some(active_plan) = self.active_plan.as_ref() {
            if let Some(text) = active_plan.active_text() {
                runtime = runtime.with_active_plan(&active_plan.active_path, text);
            }
        }
        runtime
    }

    fn record_context_snapshot(&self, context: &crate::context::ModelContext) -> Result<()> {
        self.session.append(&SessionEvent::ContextSnapshot {
            timestamp: now(),
            model: self.config.model.clone(),
            estimated_tokens: context.stats.total_tokens_estimate,
            max_tokens: context.stats.max_tokens,
            usage_percent: context.stats.usage_percent,
            warning_percent: self.config.context_warning_percent,
            pressure_status: if context.stats.usage_percent >= self.config.context_warning_percent {
                "warning".into()
            } else {
                "ok".into()
            },
            categories: context.stats.categories.clone(),
            prompt_sections: context.prompt_sections.clone(),
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

    fn mark_memory_entry_status(&mut self, id: &str, status: MemoryStatus) -> Result<MemoryEntry> {
        let memory = self
            .project_memory
            .as_mut()
            .context("project memory is not loaded")?;
        let entry = memory.mark_entry_status(id, status)?;
        self.session.append(&SessionEvent::MemoryStatusChanged {
            timestamp: now(),
            id: entry.id.clone(),
            status: entry.status.to_string(),
            title: entry.title.clone(),
        })?;
        Ok(entry)
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

fn elapsed_millis_u64(elapsed: std::time::Duration) -> u64 {
    elapsed.as_millis().min(u64::MAX as u128) as u64
}

fn sanitize_call_id(text: &str) -> String {
    let sanitized = text
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "check".into()
    } else {
        sanitized
    }
}

fn millis_u64(elapsed_ms: u128) -> u64 {
    elapsed_ms.min(u64::MAX as u128) as u64
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
            context_warning_percent: crate::config::DEFAULT_CONTEXT_WARNING_PERCENT,
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
        assert!(requests[0].instructions.contains("## Runtime context"));
        assert!(requests[0].instructions.contains("permission mode: safe"));
        assert!(requests[0]
            .instructions
            .contains("## Project additional instructions"));
        assert!(requests[0]
            .instructions
            .contains("Prefer project-specific wording."));
    }

    #[tokio::test]
    async fn ordinary_request_includes_project_memory_when_loaded() {
        let config = temp_config(PermissionMode::Safe, 3);
        let memory_root = config.cwd.join(crate::memory::MEMORY_DIR);
        std::fs::create_dir_all(memory_root.join(crate::memory::MEMORY_TOPICS_DIR)).unwrap();
        std::fs::create_dir_all(memory_root.join(crate::memory::MEMORY_ENTRIES_DIR)).unwrap();
        std::fs::write(
            memory_root.join(crate::memory::MEMORY_INDEX_FILE),
            "# Project Facts\nUse cargo test before reporting success.",
        )
        .unwrap();
        std::fs::write(
            memory_root
                .join(crate::memory::MEMORY_ENTRIES_DIR)
                .join("accepted.toml"),
            r#"id = "accepted"
title = "Accepted fact"
body = "Use scripts/smoke-memory.sh for memory lifecycle checks."
source_session = "session-1"
created_at = "2026-05-27T00:00:00Z"
last_validated_at = "2026-05-27T00:00:00Z"
scope = "project"
status = "active"
"#,
        )
        .unwrap();
        let memory = ProjectMemory::load_or_init(&config.cwd).unwrap();
        let mut agent = test_agent_with_config(config, vec![Ok(final_response("done"))]);
        let path = agent.session.path().clone();
        agent.install_project_memory(memory).unwrap();

        let reason = agent.run_turn("hello".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);

        let requests = agent.client.requests.lock().unwrap();
        assert!(requests[0].instructions.contains("## Project memory"));
        assert!(requests[0].instructions.contains("Use cargo test"));
        assert!(requests[0]
            .instructions
            .contains("Use scripts/smoke-memory.sh"));

        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("\"type\":\"memory_loaded\""));
        assert!(log.contains("\"index_tokens\""));
        assert!(log.contains("\"topic_count\":0"));
        assert!(log.contains("\"active_entry_count\":1"));
    }

    #[tokio::test]
    async fn ordinary_request_includes_active_plan_when_loaded() {
        let config = temp_config(PermissionMode::Safe, 3);
        let plan_root = config.cwd.join(crate::plan::PLAN_DIR);
        std::fs::create_dir_all(&plan_root).unwrap();
        std::fs::write(
            plan_root.join(crate::plan::ACTIVE_PLAN_FILE),
            "# Active Plan\n\n## Next Step\nRun cargo test.",
        )
        .unwrap();
        let active_plan = ActivePlan::load_or_init(&config.cwd).unwrap();
        let mut agent = test_agent_with_config(config, vec![Ok(final_response("done"))]);
        agent.install_active_plan(active_plan);

        let reason = agent.run_turn("continue".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);

        let requests = agent.client.requests.lock().unwrap();
        assert!(requests[0].instructions.contains("## Active plan"));
        assert!(requests[0].instructions.contains("Run cargo test."));
    }

    #[tokio::test]
    async fn write_handoff_updates_active_plan_and_session_event() {
        let mut agent = test_agent(PermissionMode::Safe, 3, vec![Ok(final_response("done"))]);
        let path = agent.session.path().clone();

        let reason = agent.run_turn("finish docs".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        let report = agent.write_handoff("manual").unwrap();

        assert_eq!(report.trigger, "manual");
        assert_eq!(report.verification_status, "unverified");
        assert!(report.path.exists());
        assert!(agent
            .active_plan()
            .and_then(ActivePlan::active_text)
            .unwrap()
            .contains("Latest user request: finish docs"));

        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("\"type\":\"handoff_written\""));
        assert!(log.contains("\"trigger\":\"manual\""));
    }

    #[tokio::test]
    async fn non_final_stop_writes_handoff() {
        let mut agent = test_agent(
            PermissionMode::Safe,
            3,
            vec![Ok(tool_response(
                "write_file",
                "call_1",
                json!({"path": "x.txt", "content": "x"}),
            ))],
        );
        let path = agent.session.path().clone();

        let reason = agent.run_turn("write".into()).await.unwrap();
        assert_eq!(reason, StopReason::ToolDenied);

        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("\"type\":\"handoff_written\""));
        assert!(log.contains("\"trigger\":\"tool_denied\""));
        assert!(log.contains("\"type\":\"recovery_report_written\""));
        assert!(log.contains("\"failure_class\":\"tool_denied\""));
        let plan = std::fs::read_to_string(
            agent
                .config
                .cwd
                .join(crate::plan::PLAN_DIR)
                .join(crate::plan::ACTIVE_PLAN_FILE),
        )
        .unwrap();
        assert!(plan.contains("non-final stop: tool_denied"));
        let recovery = std::fs::read_to_string(
            agent
                .config
                .cwd
                .join(crate::recovery::RECOVERY_DIR)
                .join(crate::recovery::LATEST_RECOVERY_FILE),
        )
        .unwrap();
        assert!(recovery.contains("tool_denied"));
    }

    #[tokio::test]
    async fn verification_runs_configured_checks_and_records_events() {
        let config = temp_config(PermissionMode::Safe, 3);
        std::fs::create_dir_all(config.cwd.join(".micos")).unwrap();
        std::fs::write(
            config.cwd.join(crate::verify::VERIFY_CONFIG_PATH),
            "[[checks]]\nname=\"pwd\"\ncommand=\"pwd\"\n\n[[checks]]\nname=\"missing\"\ncommand=\"ls missing-file\"\n",
        )
        .unwrap();
        let mut agent = test_agent_with_config(config, Vec::new());
        let path = agent.session.path().clone();
        let mut ui = NullUi;

        let report = agent.run_verification_with_ui(None, &mut ui).await.unwrap();

        assert_eq!(report.checks.len(), 2);
        assert!(report.checks[0].success);
        assert!(!report.checks[1].success);
        assert_eq!(report.checks[1].exit_code, Some(1));
        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("\"type\":\"verification_started\""));
        assert!(log.contains("\"type\":\"verification_finished\""));
        assert!(log.contains("\"name\":\"missing\""));
        assert!(log.contains("\"success\":false"));
    }

    #[tokio::test]
    async fn compact_replaces_prefix_with_summary_and_retains_tail() {
        let summary = "## Primary Request and Intent\nContinue the task after latest request turn 5.\n\n## Key Technical Concepts\nPrompt governance.\n\n## Files and Code Sections\nsrc/agent.rs.\n\n## Errors and Fixes\nNone.\n\n## Decisions Made\nUse manual compact.\n\n## Pending Tasks\nRun tests.\n\n## Current Work\nImplementing compact.\n\n## Next Step\nVerify with cargo test.";
        let mut agent = test_agent(
            PermissionMode::Safe,
            3,
            vec![
                Ok(final_response("answer 0")),
                Ok(final_response("answer 1")),
                Ok(final_response("answer 2")),
                Ok(final_response("answer 3")),
                Ok(final_response("answer 4")),
                Ok(final_response("answer 5")),
                Ok(final_response(summary)),
                Ok(final_response("done")),
            ],
        );
        let path = agent.session.path().clone();

        for index in 0..=5 {
            let reason = agent.run_turn(format!("turn {index}")).await.unwrap();
            assert_eq!(reason, StopReason::FinalAnswer);
        }
        let report = agent.compact_context().await.unwrap();
        assert!(report.compacted);
        assert_eq!(report.messages_replaced, 4);
        assert_eq!(report.retained_messages, 8);
        assert_eq!(report.validation_status, "passed");
        assert!(report.before_tokens > report.summary_tokens);
        assert!(report.after_tokens > report.summary_tokens);

        let reason = agent.run_turn("continue".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        let requests = agent.client.requests.lock().unwrap();
        assert!(requests[6].tools.is_empty());
        assert!(requests[6].instructions.contains("## Compact task"));
        assert_eq!(requests[7].input.len(), 10);
        assert!(requests[7].input[0]
            .to_string()
            .contains("This is a compacted summary of earlier model-visible context"));
        assert!(requests[7].input[0].to_string().contains("## Next Step"));
        assert!(requests[7].input[1].to_string().contains("turn 2"));
        assert!(requests[7].input[8].to_string().contains("answer 5"));

        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("\"type\":\"context_summary\""));
        assert!(log.contains("## Primary Request and Intent"));
        assert!(log.contains("\"type\":\"context_compacted\""));
        assert!(
            log.find("\"type\":\"context_summary\"").unwrap()
                < log.find("\"type\":\"context_compacted\"").unwrap()
        );
        assert!(log.contains("\"messages_replaced\":4"));
        assert!(log.contains("\"retained_messages\":8"));
        assert!(log.contains("\"summary_format_version\":1"));
        assert!(log.contains("\"validation_status\":\"passed\""));
        assert!(log.contains("\"summary_tokens\""));
    }

    #[tokio::test]
    async fn resume_restores_summary_tail_for_next_model_request() {
        let source_config = temp_config(PermissionMode::Safe, 3);
        let source = Session::new(&source_config).unwrap();
        source
            .append(&SessionEvent::UserInput {
                timestamp: now(),
                text: "older request".into(),
            })
            .unwrap();
        source
            .append(&SessionEvent::AssistantText {
                timestamp: now(),
                text: "older answer".into(),
            })
            .unwrap();
        source
            .append(&SessionEvent::UserInput {
                timestamp: now(),
                text: "latest request".into(),
            })
            .unwrap();
        source
            .append(&SessionEvent::AssistantText {
                timestamp: now(),
                text: "latest answer".into(),
            })
            .unwrap();
        source
            .append(&SessionEvent::ContextSummary {
                timestamp: now(),
                summary: "## Primary Request and Intent\nResume latest request.\n\n## Next Step\nContinue verification.".into(),
                summary_tokens: 12,
                messages_replaced: 2,
                retained_messages: 2,
                summary_format_version: 1,
                trigger: "manual".into(),
            })
            .unwrap();

        let mut config = temp_config(PermissionMode::Safe, 3);
        config.cwd = source_config.cwd.clone();
        let mut agent = test_agent_with_config(config, vec![Ok(final_response("done"))]);
        let current_path = agent.session.path().clone();

        let report = agent
            .resume_session(source.path().to_str().unwrap())
            .unwrap();
        assert!(report.used_summary);
        assert_eq!(report.restored_messages, 3);
        assert_eq!(report.restored_tail_messages, 2);

        let reason = agent.run_turn("continue".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        let requests = agent.client.requests.lock().unwrap();
        assert_eq!(requests[0].input.len(), 4);
        assert!(requests[0].input[0]
            .to_string()
            .contains("This is a compacted summary"));
        assert!(requests[0].input[1].to_string().contains("latest request"));
        assert!(requests[0].input[2].to_string().contains("latest answer"));
        assert!(requests[0].input[3].to_string().contains("continue"));

        let log = std::fs::read_to_string(current_path).unwrap();
        assert!(log.contains("\"type\":\"session_resumed\""));
        assert!(log.contains("\"used_summary\":true"));
    }

    #[tokio::test]
    async fn compact_empty_transcript_does_not_call_model_or_write_compacted_event() {
        let mut agent = test_agent(PermissionMode::Safe, 3, Vec::new());
        let path = agent.session.path().clone();

        let report = agent.compact_context().await.unwrap();

        assert!(!report.compacted);
        assert_eq!(report.messages_replaced, 0);
        assert_eq!(report.summary_tokens, 0);
        assert_eq!(agent.client.requests.lock().unwrap().len(), 0);
        let log = std::fs::read_to_string(path).unwrap();
        assert!(!log.contains("\"type\":\"context_summary\""));
        assert!(!log.contains("\"type\":\"context_compacted\""));
    }

    #[tokio::test]
    async fn compact_small_transcript_does_not_call_model_or_write_compacted_event() {
        let mut agent = test_agent(PermissionMode::Safe, 3, vec![Ok(final_response("answer"))]);
        let path = agent.session.path().clone();

        let reason = agent.run_turn("start".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);
        let report = agent.compact_context().await.unwrap();

        assert!(!report.compacted);
        assert_eq!(report.validation_status, "skipped_small_transcript");
        assert_eq!(agent.client.requests.lock().unwrap().len(), 1);
        let log = std::fs::read_to_string(path).unwrap();
        assert!(!log.contains("\"type\":\"context_summary\""));
        assert!(!log.contains("\"type\":\"context_compacted\""));
    }

    #[tokio::test]
    async fn compact_failure_does_not_write_context_summary() {
        let mut responses = (0..5)
            .map(|index| Ok(final_response(&format!("answer {index}"))))
            .collect::<Vec<_>>();
        responses.push(Err(anyhow!("api down")));
        let mut agent = test_agent(PermissionMode::Safe, 3, responses);
        let path = agent.session.path().clone();

        for index in 0..5 {
            let reason = agent.run_turn(format!("turn {index}")).await.unwrap();
            assert_eq!(reason, StopReason::FinalAnswer);
        }
        let error = agent.compact_context().await.unwrap_err();
        assert!(error.to_string().contains("api down"));

        let log = std::fs::read_to_string(path).unwrap();
        assert!(!log.contains("\"type\":\"context_summary\""));
        assert!(!log.contains("\"type\":\"context_compacted\""));
    }

    #[tokio::test]
    async fn compact_repairs_invalid_summary_once() {
        let repaired = "## Primary Request and Intent\nContinue after latest user request turn 4.\n\n## Key Technical Concepts\nCompact repair.\n\n## Files and Code Sections\nsrc/agent.rs.\n\n## Errors and Fixes\nInitial summary missed required sections.\n\n## Decisions Made\nRetry once.\n\n## Pending Tasks\nRun tests.\n\n## Current Work\nRepairing compact.\n\n## Next Step\nVerify with cargo test.";
        let mut responses = (0..5)
            .map(|index| Ok(final_response(&format!("answer {index}"))))
            .collect::<Vec<_>>();
        responses.push(Ok(final_response(
            "## Primary Request and Intent\nMissing required sections.",
        )));
        responses.push(Ok(final_response(repaired)));
        let mut agent = test_agent(PermissionMode::Safe, 3, responses);
        let path = agent.session.path().clone();

        for index in 0..5 {
            let reason = agent.run_turn(format!("turn {index}")).await.unwrap();
            assert_eq!(reason, StopReason::FinalAnswer);
        }
        let report = agent.compact_context().await.unwrap();

        assert!(report.compacted);
        assert_eq!(
            report.validation_status,
            "repaired:missing_heading:Key Technical Concepts"
        );
        let requests = agent.client.requests.lock().unwrap();
        assert_eq!(requests.len(), 7);
        assert!(requests[5].instructions.contains("## Compact task"));
        assert!(requests[6].instructions.contains("## Compact repair task"));
        assert!(requests[6]
            .instructions
            .contains("missing_heading:Key Technical Concepts"));
        assert!(requests[6].tools.is_empty());
        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains("\"type\":\"context_summary\""));
        assert!(log.contains("## Primary Request and Intent"));
        assert!(log.contains("\"type\":\"context_compacted\""));
        assert!(log
            .contains("\"validation_status\":\"repaired:missing_heading:Key Technical Concepts\""));
    }

    #[tokio::test]
    async fn compact_validation_failure_keeps_original_transcript() {
        let mut responses = (0..5)
            .map(|index| Ok(final_response(&format!("answer {index}"))))
            .collect::<Vec<_>>();
        responses.push(Ok(final_response(
            "## Primary Request and Intent\nMissing required sections.",
        )));
        responses.push(Ok(final_response("still invalid")));
        responses.push(Ok(final_response("done")));
        let mut agent = test_agent(PermissionMode::Safe, 3, responses);
        let path = agent.session.path().clone();

        for index in 0..5 {
            let reason = agent.run_turn(format!("turn {index}")).await.unwrap();
            assert_eq!(reason, StopReason::FinalAnswer);
        }
        let error = agent.compact_context().await.unwrap_err();
        assert!(error
            .to_string()
            .contains("compact summary validation failed after repair"));
        let reason = agent.run_turn("after failure".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);

        let requests = agent.client.requests.lock().unwrap();
        assert_eq!(requests[7].input.len(), 11);
        assert!(requests[7].input[0].to_string().contains("turn 0"));
        let log = std::fs::read_to_string(path).unwrap();
        assert!(!log.contains("\"type\":\"context_summary\""));
        assert!(!log.contains("\"type\":\"context_compacted\""));
        assert!(log.contains("compact summary validation failed after repair"));
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
        assert!(log.contains("\"prompt_sections\""));
        assert!(log.contains("\"id\":\"identity\""));
        assert!(log.contains("\"name\":\"prompt.identity\""));
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

    struct LargeOutputRegistry {
        output: String,
    }

    impl ToolRegistry for LargeOutputRegistry {
        fn schemas(&self) -> Vec<Value> {
            vec![
                json!({"name":"large_output","description":"large output","parameters":{"type":"object"}}),
            ]
        }

        fn schemas_for_policy(
            &self,
            _policy: &dyn PermissionPolicy,
            _permission: PermissionMode,
        ) -> Vec<Value> {
            self.schemas()
        }

        fn metadata(&self, _name: &str, arguments: &Value) -> Option<crate::tools::ToolMetadata> {
            Some(crate::tools::ToolMetadata {
                name: "large_output",
                read_only: true,
                destructive: false,
                concurrency_safe: true,
                argument_summary: arguments.to_string(),
                permission_hint: PermissionDecision::Allow,
            })
        }

        async fn execute(&self, _name: &str, _input: Value, _ctx: ToolContext) -> ToolResult {
            ToolResult::ok(self.output.clone())
        }
    }

    #[tokio::test]
    async fn session_log_keeps_original_tool_output_while_model_sees_preview() {
        let output = "z".repeat(MODEL_VISIBLE_TOOL_OUTPUT_LIMIT + 32);
        let config = temp_config(PermissionMode::Safe, 3);
        let session = Session::new(&config).unwrap();
        let path = session.path().clone();
        let model = MockModel::new(vec![
            Ok(tool_response("large_output", "call_1", json!({}))),
            Ok(final_response("done")),
        ]);
        let requests = model.requests.clone();
        let mut agent = AgentRuntime::with_parts(
            config,
            model,
            session,
            LargeOutputRegistry {
                output: output.clone(),
            },
            PolicyEngine::new(Vec::new()),
        );

        let reason = agent.run_turn("large output".into()).await.unwrap();
        assert_eq!(reason, StopReason::FinalAnswer);

        let log = std::fs::read_to_string(path).unwrap();
        assert!(log.contains(&output));

        let requests = requests.lock().unwrap();
        let projected = requests[1].input[2]["output"].as_str().unwrap();
        let projected: Value = serde_json::from_str(projected).unwrap();
        assert_eq!(projected["truncated"], true);
        assert_eq!(projected["original_bytes"], output.len());
        assert_eq!(
            projected["preview_bytes"].as_u64().unwrap() as usize,
            MODEL_VISIBLE_TOOL_OUTPUT_LIMIT
        );
        assert_eq!(
            projected["output"].as_str().unwrap().len(),
            MODEL_VISIBLE_TOOL_OUTPUT_LIMIT
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
