use crate::agent::Agent;
use crate::config::save_model_settings;
use crate::model::OpenAiModelClient;
use crate::session::StopReason;
use crate::tools::ToolSummary;
use crate::ui::{
    format_help, format_sessions, format_status, format_transcript, AgentEvent, SlashCommand,
    UiSink,
};
use anyhow::{Context, Result};
use crossterm::{
    cursor::Show,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Stdout};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

mod render;
mod state;

use render::*;
use state::*;

const POPUP_LIMIT: usize = 6;
const SCROLL_STEP: isize = 5;
const TICK_RATE: Duration = Duration::from_millis(120);

pub async fn run_tui_chat(agent: Agent<OpenAiModelClient>) -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend).context("create terminal")?;
    let mut ui = TuiUi::new(terminal, agent);
    ui.banner();
    ui.run().await
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)
            .context("enter alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            LeaveAlternateScreen,
            Show
        );
    }
}

enum TuiAgentMessage {
    Event(AgentEvent),
    ApprovalRequest {
        name: String,
        summary: String,
        response: std_mpsc::Sender<bool>,
    },
    TurnFinished {
        agent: Agent<OpenAiModelClient>,
        result: std::result::Result<StopReason, String>,
        elapsed: Duration,
    },
}

struct TuiAgentSink {
    tx: mpsc::UnboundedSender<TuiAgentMessage>,
}

impl UiSink for TuiAgentSink {
    fn on_event(&mut self, event: AgentEvent) -> Result<()> {
        let _ = self.tx.send(TuiAgentMessage::Event(event));
        Ok(())
    }

    fn approve_tool(&mut self, name: &str, summary: &str) -> Result<bool> {
        let (response, decision) = std_mpsc::channel();
        let _ = self.tx.send(TuiAgentMessage::ApprovalRequest {
            name: name.to_string(),
            summary: summary.to_string(),
            response,
        });
        Ok(decision.recv().unwrap_or(false))
    }
}

pub struct TuiUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    agent: Option<Agent<OpenAiModelClient>>,
    agent_tx: mpsc::UnboundedSender<TuiAgentMessage>,
    agent_rx: mpsc::UnboundedReceiver<TuiAgentMessage>,
    agent_task: Option<JoinHandle<()>>,
    messages: Vec<TuiMessage>,
    composer: ComposerState,
    model_panel: Option<ModelPanelState>,
    approval_picker: Option<ApprovalPickerState>,
    permission_message: Option<usize>,
    active_tool_message: Option<usize>,
    show_reasoning: bool,
    run_status: RunStatus,
    animation_tick: usize,
    footer_model: String,
    footer_thinking: String,
    footer_reasoning_effort: String,
    footer_cwd: String,
    scroll_top: usize,
    stick_to_bottom: bool,
    last_message_lines: usize,
    last_message_height: usize,
}

impl TuiUi {
    fn new(terminal: Terminal<CrosstermBackend<Stdout>>, agent: Agent<OpenAiModelClient>) -> Self {
        let (agent_tx, agent_rx) = mpsc::unbounded_channel();
        Self {
            terminal,
            agent: Some(agent),
            agent_tx,
            agent_rx,
            agent_task: None,
            messages: Vec::new(),
            composer: ComposerState::new(),
            model_panel: None,
            approval_picker: None,
            permission_message: None,
            active_tool_message: None,
            show_reasoning: false,
            run_status: RunStatus::Idle,
            animation_tick: 0,
            footer_model: String::new(),
            footer_thinking: String::new(),
            footer_reasoning_effort: String::new(),
            footer_cwd: String::new(),
            scroll_top: 0,
            stick_to_bottom: true,
            last_message_lines: 0,
            last_message_height: 0,
        }
    }

    fn banner(&mut self) {
        let Some((model, permission, cwd, session_id, session_path, thinking, effort)) =
            self.agent.as_ref().map(|agent| {
                (
                    agent.config().model.clone(),
                    agent.config().permission.to_string(),
                    agent.config().cwd.display().to_string(),
                    agent.session_id(),
                    agent.session_path().display().to_string(),
                    short_thinking(agent.config().thinking).to_string(),
                    short_reasoning_effort(agent.config().reasoning_effort).to_string(),
                )
            })
        else {
            return;
        };
        self.footer_model = model.clone();
        self.footer_thinking = thinking;
        self.footer_reasoning_effort = effort;
        self.footer_cwd = cwd.clone();
        self.push_message(
            MessageKind::System,
            "micos",
            format!(
                "model: {}\npermission: {}\ncwd: {}\nsession: {}\nlog: {}\nType /help for commands.",
                model, permission, cwd, session_id, session_path
            ),
        );
    }

    async fn run(&mut self) -> Result<()> {
        loop {
            self.handle_agent_messages()?;
            self.render()?;
            if !event::poll(TICK_RATE).context("poll terminal event")? {
                self.tick_animation();
                continue;
            };
            match event::read().context("read terminal event")? {
                Event::Key(key) => {
                    if !is_key_press(key) {
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        if let Some(agent) = self.agent.as_ref() {
                            agent.stop(StopReason::UserInterrupt).await?;
                            self.push_message(MessageKind::Warning, "stopped", "interrupted");
                            self.run_status = RunStatus::Idle;
                            self.render()?;
                            break;
                        } else {
                            self.push_message(
                                MessageKind::Warning,
                                "busy",
                                "wait for the current turn to finish before exiting",
                            );
                            continue;
                        }
                    }

                    if self.handle_approval_key(key)? {
                        continue;
                    }
                    if self.handle_model_panel_key(key)? {
                        continue;
                    }
                    if self.handle_scroll_key(key) {
                        continue;
                    }
                    if self.agent_task.is_some() && key.code == KeyCode::Enter {
                        self.push_message(
                            MessageKind::Warning,
                            "busy",
                            "agent is still working on the current turn",
                        );
                        continue;
                    }

                    let action = self.composer.handle_key(key);
                    if !self.process_composer_action(action).await? {
                        break;
                    }
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => self.scroll_by(-SCROLL_STEP),
                    MouseEventKind::ScrollDown => self.scroll_by(SCROLL_STEP),
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(())
    }

    fn footer_state_from_self(&self) -> FooterState {
        FooterState {
            model: self.footer_model.clone(),
            thinking: self.footer_thinking.clone(),
            reasoning_effort: self.footer_reasoning_effort.clone(),
            cwd: self.footer_cwd.clone(),
            show_reasoning: self.show_reasoning,
            run_status: self.run_status.clone(),
            animation_tick: self.animation_tick,
        }
    }

    fn render(&mut self) -> Result<()> {
        if let Some(agent) = self.agent.as_ref() {
            self.footer_model = agent.config().model.clone();
            self.footer_thinking = short_thinking(agent.config().thinking).to_string();
            self.footer_reasoning_effort =
                short_reasoning_effort(agent.config().reasoning_effort).to_string();
            self.footer_cwd = agent.config().cwd.display().to_string();
        }
        let footer = self.footer_state_from_self();
        self.render_with_footer(&footer)
    }

    fn render_with_footer(&mut self, footer: &FooterState) -> Result<()> {
        let size = self.terminal.size().context("read terminal size")?;
        let approval_selected = self
            .approval_picker
            .as_ref()
            .map(ApprovalPickerState::selected);
        let bottom_height =
            bottom_panel_height(&self.composer, self.model_panel.as_ref(), approval_selected);
        let working_height = working_panel_height(footer);
        let message_height =
            size.height
                .saturating_sub(3 + bottom_height + working_height) as usize;
        let message_width = size.width as usize;
        let lines = build_message_lines(&self.messages, message_width, footer.animation_tick);
        self.last_message_lines = lines.len();
        self.last_message_height = message_height;
        let max_top = self.max_scroll_top();
        if self.stick_to_bottom {
            self.scroll_top = max_top;
        } else {
            self.scroll_top = self.scroll_top.min(max_top);
        }

        let composer = self.composer.clone();
        let model_panel = self.model_panel.clone();
        let scroll_top = self.scroll_top;
        let footer = footer.clone();
        self.terminal
            .draw(|frame| {
                draw_frame(
                    frame,
                    &lines,
                    scroll_top,
                    &composer,
                    model_panel.as_ref(),
                    approval_selected,
                    &footer,
                )
            })
            .context("draw terminal")?;
        Ok(())
    }

    fn tick_animation(&mut self) {
        if !matches!(self.run_status, RunStatus::Idle) {
            self.animation_tick = self.animation_tick.wrapping_add(1);
        }
    }

    fn handle_agent_messages(&mut self) -> Result<()> {
        while let Ok(message) = self.agent_rx.try_recv() {
            match message {
                TuiAgentMessage::Event(event) => self.apply_agent_event(event),
                TuiAgentMessage::ApprovalRequest {
                    name,
                    summary,
                    response,
                } => {
                    self.run_status = RunStatus::WaitingApproval;
                    self.model_panel = None;
                    self.composer.popup_open = false;
                    self.push_permission_message(&name, &summary);
                    self.approval_picker = Some(ApprovalPickerState::new(response));
                }
                TuiAgentMessage::TurnFinished {
                    agent,
                    result,
                    elapsed,
                } => {
                    self.agent = Some(agent);
                    self.agent_task = None;
                    self.run_status = RunStatus::Idle;
                    self.active_tool_message = None;
                    match result {
                        Ok(reason) => self.push_turn_elapsed(reason, elapsed),
                        Err(message) => {
                            self.push_message(MessageKind::Error, "error", message);
                            self.push_message_with_status(
                                MessageKind::System,
                                "stopped",
                                format!("turn finished in {}", format_duration(elapsed)),
                                Some(MessageStatus::Neutral),
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn start_agent_turn(&mut self, input: String) {
        if self.agent_task.is_some() {
            self.push_message(
                MessageKind::Warning,
                "busy",
                "agent is still working on the current turn",
            );
            return;
        }
        let Some(mut agent) = self.agent.take() else {
            return;
        };
        self.run_status = RunStatus::Working;
        let tx = self.agent_tx.clone();
        self.agent_task = Some(tokio::spawn(async move {
            let start = Instant::now();
            let mut sink = TuiAgentSink { tx: tx.clone() };
            let result = agent
                .run_turn_with_ui(input, &mut sink)
                .await
                .map_err(|error| error.to_string());
            let elapsed = start.elapsed();
            let _ = tx.send(TuiAgentMessage::TurnFinished {
                agent,
                result,
                elapsed,
            });
        }));
    }

    async fn process_composer_action(&mut self, action: ComposerAction) -> Result<bool> {
        match action {
            ComposerAction::None => {}
            ComposerAction::Submit(input) => {
                self.start_agent_turn(input);
            }
            ComposerAction::Command(command) => {
                if !self.handle_slash(command).await? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    async fn handle_slash(&mut self, command: SlashCommand) -> Result<bool> {
        if self.agent.is_none() && !matches!(command, SlashCommand::Help | SlashCommand::Clear) {
            self.push_message(
                MessageKind::Warning,
                "busy",
                "command is unavailable while the agent is working",
            );
            return Ok(true);
        }
        match command {
            SlashCommand::Help => self.push_message(MessageKind::System, "/help", format_help()),
            SlashCommand::Status => {
                let agent = self.agent.as_ref().expect("agent checked above");
                self.push_message(
                    MessageKind::System,
                    "/status",
                    format_status(agent.config(), agent.session_id(), agent.session_path()),
                );
            }
            SlashCommand::Sessions => {
                let agent = self.agent.as_ref().expect("agent checked above");
                self.push_message(
                    MessageKind::System,
                    "/sessions",
                    format_sessions(&agent.config().cwd)?,
                );
            }
            SlashCommand::Transcript => {
                let agent = self.agent.as_ref().expect("agent checked above");
                self.push_message(
                    MessageKind::System,
                    "/transcript",
                    format_transcript(agent.session_path())?,
                );
            }
            SlashCommand::Model => {
                let agent = self.agent.as_ref().expect("agent checked above");
                self.model_panel = Some(ModelPanelState::from_settings(
                    agent.config(),
                    self.show_reasoning,
                ));
            }
            SlashCommand::Clear => {
                self.messages.clear();
                self.permission_message = None;
                self.active_tool_message = None;
                self.scroll_top = 0;
                self.stick_to_bottom = true;
            }
            SlashCommand::Exit => {
                if let Some(agent) = self.agent.as_ref() {
                    agent.stop(StopReason::UserExit).await?;
                }
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn handle_model_panel_key(&mut self, key: KeyEvent) -> Result<bool> {
        let Some(panel) = &mut self.model_panel else {
            return Ok(false);
        };
        match panel.handle_key(key) {
            ModelPanelAction::None => {}
            ModelPanelAction::Cancel => {
                self.model_panel = None;
            }
            ModelPanelAction::Apply {
                settings,
                show_reasoning,
            } => {
                let Some(status) = self.agent.as_mut().map(|agent| {
                    save_model_settings(&agent.config().cwd, &settings)?;
                    agent.apply_model_settings(settings)?;
                    Ok::<String, anyhow::Error>(format_status(
                        agent.config(),
                        agent.session_id(),
                        agent.session_path(),
                    ))
                }) else {
                    self.model_panel = None;
                    return Ok(true);
                };
                let status = status?;
                self.show_reasoning = show_reasoning;
                self.model_panel = None;
                self.push_message(MessageKind::System, "/model", status);
            }
        }
        Ok(true)
    }

    fn handle_approval_key(&mut self, key: KeyEvent) -> Result<bool> {
        if self.approval_picker.is_none() {
            return Ok(false);
        };
        if !matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Tab
                | KeyCode::Enter
                | KeyCode::Esc
        ) {
            return Ok(false);
        }
        let picker = self.approval_picker.as_mut().expect("picker checked above");
        match picker.handle_key(key) {
            ApprovalAction::None => {}
            ApprovalAction::Decide(approved) => {
                self.finish_approval(approved);
            }
        }
        Ok(true)
    }

    fn finish_approval(&mut self, approved: bool) {
        if let Some(mut picker) = self.approval_picker.take() {
            picker.send(approved);
        }
        self.remove_permission_message();
        self.run_status = RunStatus::Working;
    }

    fn handle_scroll_key(&mut self, key: KeyEvent) -> bool {
        if self.composer.popup_open() {
            return false;
        }
        match key.code {
            KeyCode::Up => self.scroll_by(-SCROLL_STEP),
            KeyCode::Down => self.scroll_by(SCROLL_STEP),
            KeyCode::PageUp => self.scroll_by(-((self.last_message_height as isize).max(1))),
            KeyCode::PageDown => self.scroll_by((self.last_message_height as isize).max(1)),
            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => self.scroll_to_top(),
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_to_bottom()
            }
            _ => return false,
        }
        true
    }

    fn scroll_by(&mut self, delta: isize) {
        let max_top = self.max_scroll_top();
        self.scroll_top = next_scroll_top(self.scroll_top, max_top, delta);
        self.stick_to_bottom = self.scroll_top >= max_top;
    }

    fn scroll_to_top(&mut self) {
        self.scroll_top = 0;
        self.stick_to_bottom = false;
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_top = self.max_scroll_top();
        self.stick_to_bottom = true;
    }

    fn max_scroll_top(&self) -> usize {
        self.last_message_lines
            .saturating_sub(self.last_message_height)
    }

    fn push_message(
        &mut self,
        kind: MessageKind,
        title: impl Into<String>,
        body: impl Into<String>,
    ) {
        self.messages.push(TuiMessage {
            kind,
            title: title.into(),
            body: body.into(),
            status: default_status_for_kind(kind),
            transient: false,
        });
    }

    fn push_message_with_status(
        &mut self,
        kind: MessageKind,
        title: impl Into<String>,
        body: impl Into<String>,
        status: Option<MessageStatus>,
    ) {
        self.messages.push(TuiMessage {
            kind,
            title: title.into(),
            body: body.into(),
            status,
            transient: false,
        });
    }

    fn push_turn_elapsed(&mut self, reason: StopReason, elapsed: Duration) {
        let title = if reason == StopReason::FinalAnswer {
            "completed"
        } else {
            "stopped"
        };
        self.push_message_with_status(
            MessageKind::System,
            title,
            format!("turn finished in {}", format_duration(elapsed)),
            Some(MessageStatus::Neutral),
        );
    }

    fn append_stream(&mut self, kind: MessageKind, title: &'static str, text: &str) {
        if let Some(message) = self
            .messages
            .last_mut()
            .filter(|message| message.kind == kind && message.title == title && !message.transient)
        {
            message.body.push_str(text);
            return;
        }
        self.push_message(kind, title, text);
    }

    fn push_permission_message(&mut self, name: &str, summary: &str) {
        self.remove_permission_message();
        self.permission_message = Some(self.messages.len());
        self.messages.push(TuiMessage {
            kind: MessageKind::Warning,
            title: format!("permission {name}"),
            body: summary.to_string(),
            status: Some(MessageStatus::Running),
            transient: true,
        });
        self.scroll_to_bottom();
    }

    fn remove_permission_message(&mut self) {
        if let Some(index) = self.permission_message.take() {
            if index < self.messages.len() && self.messages[index].transient {
                self.messages.remove(index);
            }
        }
    }
}

impl UiSink for TuiUi {
    fn on_event(&mut self, event: AgentEvent) -> Result<()> {
        self.apply_agent_event(event);
        self.render()
    }

    fn approve_tool(&mut self, name: &str, summary: &str) -> Result<bool> {
        self.push_permission_message(name, summary);
        self.render()?;
        loop {
            let Event::Key(key) = event::read().context("read permission event")? else {
                continue;
            };
            if !is_key_press(key) {
                continue;
            }
            let decision = match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(false),
                _ => None,
            };
            if let Some(approved) = decision {
                self.remove_permission_message();
                self.run_status = RunStatus::Working;
                self.render()?;
                return Ok(approved);
            }
        }
    }
}

impl TuiUi {
    fn apply_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted { input } => {
                self.run_status = RunStatus::Working;
                self.push_message(MessageKind::User, "you", input);
            }
            AgentEvent::AssistantDelta { text } => {
                self.tick_animation();
                self.append_stream(MessageKind::Assistant, "assistant", &text);
            }
            AgentEvent::ReasoningDelta { text } => {
                self.tick_animation();
                if self.show_reasoning {
                    self.append_stream(MessageKind::Reasoning, "reasoning", &text);
                }
            }
            AgentEvent::ToolCallStarted {
                call_id: _,
                name,
                arguments,
                permission: _,
            } => {
                self.run_status = RunStatus::Tool(name.clone());
                let summary = ToolSummary::from_arguments(&name, &arguments).summary;
                self.active_tool_message = Some(self.messages.len());
                self.push_message_with_status(
                    MessageKind::Tool,
                    format!("tool {name}"),
                    summary,
                    Some(MessageStatus::Running),
                );
            }
            AgentEvent::ToolCallFinished {
                call_id: _,
                name,
                result,
                elapsed,
            } => {
                self.run_status = RunStatus::Working;
                finish_tool_message(
                    &mut self.messages,
                    &mut self.active_tool_message,
                    &name,
                    result,
                    elapsed,
                );
            }
            AgentEvent::PermissionPrompt { name, summary } => {
                self.run_status = RunStatus::WaitingApproval;
                self.push_permission_message(&name, &summary);
            }
            AgentEvent::Stop { reason } => {
                self.run_status = RunStatus::Idle;
                if reason != StopReason::FinalAnswer {
                    self.push_message(MessageKind::Warning, "stopped", reason.to_string());
                }
            }
            AgentEvent::Error { message } => {
                self.run_status = RunStatus::Idle;
                self.push_message(MessageKind::Error, "error", message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ModelSettings, ReasoningEffort, SessionConfig, ThinkingMode,
        DEEPSEEK_CHAT_COMPLETIONS_BASE_URL,
    };
    use crate::tools::ToolResult;
    use crate::ui::SLASH_COMMANDS;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn composer_edits_text_and_cursor() {
        let mut composer = ComposerState::new();
        assert_eq!(
            composer.handle_key(key(KeyCode::Char('h'))),
            ComposerAction::None
        );
        assert_eq!(
            composer.handle_key(key(KeyCode::Char('i'))),
            ComposerAction::None
        );
        composer.handle_key(key(KeyCode::Left));
        composer.handle_key(key(KeyCode::Char('!')));
        assert_eq!(composer.buffer(), "h!i");
        composer.handle_key(key(KeyCode::Backspace));
        assert_eq!(composer.buffer(), "hi");
        composer.handle_key(key(KeyCode::End));
        assert_eq!(
            composer.handle_key(key(KeyCode::Enter)),
            ComposerAction::Submit("hi".into())
        );
        assert_eq!(composer.buffer(), "");
    }

    #[test]
    fn composer_uses_display_width_for_unicode_cursor() {
        let mut composer = ComposerState::new();
        composer.handle_key(key(KeyCode::Char('你')));
        composer.handle_key(key(KeyCode::Char('好')));
        let (visible, cursor_x) = composer.visible_input(10);
        assert_eq!(visible, "你好");
        assert_eq!(cursor_x, 4);

        composer.handle_key(key(KeyCode::Left));
        let (_, cursor_x) = composer.visible_input(10);
        assert_eq!(cursor_x, 2);
        composer.handle_key(key(KeyCode::Char('a')));
        assert_eq!(composer.buffer(), "你a好");
        let (_, cursor_x) = composer.visible_input(10);
        assert_eq!(cursor_x, 3);
    }

    #[test]
    fn composer_opens_filters_and_accepts_slash_popup() {
        let mut composer = ComposerState::new();
        composer.handle_key(key(KeyCode::Char('/')));
        assert!(composer.popup_open());
        assert_eq!(composer.matches().len(), SLASH_COMMANDS.len());
        composer.handle_key(key(KeyCode::Char('s')));
        composer.handle_key(key(KeyCode::Char('t')));
        composer.handle_key(key(KeyCode::Char('a')));
        assert_eq!(
            composer
                .matches()
                .into_iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            vec!["status"]
        );
        assert_eq!(
            composer.handle_key(key(KeyCode::Enter)),
            ComposerAction::Command(SlashCommand::Status)
        );
        assert_eq!(composer.buffer(), "");
    }

    #[test]
    fn popup_selection_wraps_and_tab_accepts_current_item() {
        let mut composer = ComposerState::new();
        composer.handle_key(key(KeyCode::Char('/')));
        composer.handle_key(key(KeyCode::Up));
        assert_eq!(composer.selected(), POPUP_LIMIT - 1);
        assert_eq!(
            composer.handle_key(key(KeyCode::Tab)),
            ComposerAction::Command(SLASH_COMMANDS[POPUP_LIMIT - 1].command)
        );
    }

    #[test]
    fn escape_closes_popup_and_retains_text() {
        let mut composer = ComposerState::new();
        composer.handle_key(key(KeyCode::Char('/')));
        composer.handle_key(key(KeyCode::Char('z')));
        assert!(composer.popup_open());
        composer.handle_key(key(KeyCode::Esc));
        assert!(!composer.popup_open());
        assert_eq!(composer.buffer(), "/z");
        composer.handle_key(key(KeyCode::Char('x')));
        assert!(composer.popup_open());
        assert_eq!(composer.buffer(), "/zx");
        assert_eq!(
            composer.handle_key(key(KeyCode::Enter)),
            ComposerAction::None
        );
    }

    #[test]
    fn model_panel_cycles_and_applies_settings() {
        let config = SessionConfig {
            api_kind: crate::config::ApiKind::ChatCompletions,
            model: "deepseek-v4-flash".into(),
            base_url: DEEPSEEK_CHAT_COMPLETIONS_BASE_URL.into(),
            thinking: Some(ThinkingMode::Enabled),
            reasoning_effort: Some(ReasoningEffort::High),
            permission: crate::config::PermissionMode::Ask,
            permission_rules: Vec::new(),
            max_steps: 20,
            cwd: std::path::PathBuf::from("/tmp/micos"),
        };
        let mut panel = ModelPanelState::from_settings(&config, false);
        assert_eq!(panel.settings().model, "deepseek-v4-flash");
        panel.handle_key(key(KeyCode::Right));
        panel.handle_key(key(KeyCode::Down));
        panel.handle_key(key(KeyCode::Left));
        panel.handle_key(key(KeyCode::Down));
        panel.handle_key(key(KeyCode::Right));
        assert_eq!(
            panel.handle_key(key(KeyCode::Enter)),
            ModelPanelAction::Apply {
                settings: ModelSettings {
                    model: "deepseek-v4-pro".into(),
                    base_url: DEEPSEEK_CHAT_COMPLETIONS_BASE_URL.into(),
                    thinking: Some(ThinkingMode::Disabled),
                    reasoning_effort: Some(ReasoningEffort::Max),
                },
                show_reasoning: false,
            }
        );
    }

    #[test]
    fn model_panel_toggles_reasoning_visibility() {
        let config = SessionConfig {
            api_kind: crate::config::ApiKind::ChatCompletions,
            model: "deepseek-v4-flash".into(),
            base_url: DEEPSEEK_CHAT_COMPLETIONS_BASE_URL.into(),
            thinking: Some(ThinkingMode::Enabled),
            reasoning_effort: Some(ReasoningEffort::High),
            permission: crate::config::PermissionMode::Ask,
            permission_rules: Vec::new(),
            max_steps: 20,
            cwd: std::path::PathBuf::from("/tmp/micos"),
        };
        let mut panel = ModelPanelState::from_settings(&config, false);
        panel.handle_key(key(KeyCode::Down));
        panel.handle_key(key(KeyCode::Down));
        panel.handle_key(key(KeyCode::Down));
        panel.handle_key(key(KeyCode::Right));
        assert!(panel.show_reasoning);
    }

    #[test]
    fn wrapped_message_lines_support_scroll_calculation() {
        let messages = vec![TuiMessage {
            kind: MessageKind::System,
            title: "title".into(),
            body: "你好hello".into(),
            status: Some(MessageStatus::Neutral),
            transient: false,
        }];
        let lines = build_message_lines(&messages, 4, 0);
        assert!(lines.len() >= 3);
    }

    #[test]
    fn scroll_step_moves_five_lines() {
        assert_eq!(next_scroll_top(20, 100, -SCROLL_STEP), 15);
        assert_eq!(next_scroll_top(20, 100, SCROLL_STEP), 25);
        assert_eq!(next_scroll_top(98, 100, SCROLL_STEP), 100);
    }

    #[test]
    fn footer_text_contains_status_and_reasoning_visibility() {
        let footer = FooterState {
            model: "deepseek-v4-flash".into(),
            thinking: "think".into(),
            reasoning_effort: "high".into(),
            cwd: "/tmp/micos".into(),
            show_reasoning: false,
            run_status: RunStatus::Working,
            animation_tick: 2,
        };
        let text = footer_text(&footer);
        assert!(text.contains("deepseek-v4-flash"));
        assert!(text.contains("think/high"));
        assert!(text.contains("reasoning hidden"));
        assert!(!text.contains("Working"));
    }

    #[test]
    fn footer_short_labels_cover_thinking_and_effort() {
        assert_eq!(short_thinking(Some(ThinkingMode::Enabled)), "think");
        assert_eq!(short_thinking(Some(ThinkingMode::Disabled)), "nothink");
        assert_eq!(short_thinking(None), "unset");
        assert_eq!(short_reasoning_effort(Some(ReasoningEffort::High)), "high");
        assert_eq!(short_reasoning_effort(Some(ReasoningEffort::Max)), "max");
        assert_eq!(short_reasoning_effort(None), "unset");
    }

    #[test]
    fn bottom_panel_expands_for_popup_below_composer() {
        let mut composer = ComposerState::new();
        composer.handle_key(key(KeyCode::Char('/')));
        assert!(bottom_panel_height(&composer, None, None) > 1);
        assert_eq!(bottom_panel_height(&ComposerState::new(), None, None), 1);
        assert_eq!(bottom_panel_height(&ComposerState::new(), None, Some(0)), 4);
    }

    #[test]
    fn message_status_dot_skips_user_and_assistant() {
        let tool = TuiMessage {
            kind: MessageKind::Tool,
            title: "tool read_file".into(),
            body: "path=src/tui.rs".into(),
            status: Some(MessageStatus::Running),
            transient: false,
        };
        let user = TuiMessage {
            kind: MessageKind::User,
            title: "you".into(),
            body: "hello".into(),
            status: None,
            transient: false,
        };
        assert!(visible_status(&tool).is_some());
        assert!(visible_status(&user).is_none());
        assert_ne!(
            status_style(MessageStatus::Running, 0),
            status_style(MessageStatus::Running, 1)
        );
    }

    #[test]
    fn user_and_assistant_messages_do_not_render_titles() {
        let lines = build_message_lines(
            &[
                TuiMessage {
                    kind: MessageKind::User,
                    title: "you".into(),
                    body: "hello".into(),
                    status: None,
                    transient: false,
                },
                TuiMessage {
                    kind: MessageKind::Assistant,
                    title: "assistant".into(),
                    body: "hi".into(),
                    status: None,
                    transient: false,
                },
            ],
            80,
            0,
        );
        let rendered = format!("{lines:?}");
        assert!(!rendered.contains("you"));
        assert!(!rendered.contains("assistant"));
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("hi"));
    }

    #[test]
    fn tool_finish_updates_active_message_without_internal_fields() {
        let mut messages = vec![TuiMessage {
            kind: MessageKind::Tool,
            title: "tool read_file".into(),
            body: "path=src/tui.rs".into(),
            status: Some(MessageStatus::Running),
            transient: false,
        }];
        let mut active = Some(0);
        finish_tool_message(
            &mut messages,
            &mut active,
            "read_file",
            ToolResult {
                success: true,
                output: "contents".into(),
                error: None,
                denied: false,
            },
            Duration::from_millis(250),
        );
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].status, Some(MessageStatus::Success));
        assert!(messages[0].body.contains("ok in 250ms"));
        assert!(!messages[0].body.contains("call"));
        assert!(!messages[0].body.contains("permission"));
    }

    #[test]
    fn working_panel_only_appears_while_running() {
        let mut footer = FooterState {
            model: "deepseek-v4-flash".into(),
            thinking: "enabled".into(),
            reasoning_effort: "high".into(),
            cwd: "/tmp/micos".into(),
            show_reasoning: false,
            run_status: RunStatus::Idle,
            animation_tick: 0,
        };
        assert_eq!(working_panel_height(&footer), 0);
        footer.run_status = RunStatus::Working;
        assert_eq!(working_panel_height(&footer), 1);
        assert!(footer
            .run_status
            .label(footer.animation_tick)
            .contains("Working"));
    }

    #[test]
    fn approval_picker_supports_selection_and_denies_on_escape() {
        let (response, decision) = std_mpsc::channel();
        let mut picker = ApprovalPickerState::new(response);
        assert_eq!(picker.selected(), 0);
        assert_eq!(picker.handle_key(key(KeyCode::Down)), ApprovalAction::None);
        assert_eq!(picker.selected(), 1);
        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            ApprovalAction::Decide(false)
        );
        drop(picker);
        assert!(!decision.recv().unwrap());

        let (response, decision) = std_mpsc::channel();
        let mut picker = ApprovalPickerState::new(response);
        assert_eq!(
            picker.handle_key(key(KeyCode::Esc)),
            ApprovalAction::Decide(false)
        );
        picker.send(false);
        assert!(!decision.recv().unwrap());
    }
}
