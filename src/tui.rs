use crate::agent::Agent;
use crate::config::{
    save_model_settings, ModelSettings, ReasoningEffort, ThinkingMode,
    DEEPSEEK_CHAT_COMPLETIONS_BASE_URL,
};
use crate::model::OpenAiModelClient;
use crate::session::StopReason;
use crate::tools::{ToolResult, ToolSummary};
use crate::ui::{
    format_help, format_sessions, format_status, format_transcript, slash_command_exact,
    slash_command_matches, AgentEvent, SlashCommand, SlashCommandInfo, UiSink,
};
use anyhow::{Context, Result};
use crossterm::{
    cursor::Show,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame, Terminal,
};
use std::io::{self, Stdout};
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const POPUP_LIMIT: usize = 6;
const SCROLL_STEP: isize = 5;
const TICK_RATE: Duration = Duration::from_millis(120);

pub async fn run_tui_chat(agent: &mut Agent<OpenAiModelClient>) -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend).context("create terminal")?;
    let mut ui = TuiUi::new(terminal);
    ui.banner(agent);
    ui.run(agent).await
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

pub struct TuiUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    messages: Vec<TuiMessage>,
    composer: ComposerState,
    model_panel: Option<ModelPanelState>,
    permission_message: Option<usize>,
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
    fn new(terminal: Terminal<CrosstermBackend<Stdout>>) -> Self {
        Self {
            terminal,
            messages: Vec::new(),
            composer: ComposerState::new(),
            model_panel: None,
            permission_message: None,
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

    fn banner(&mut self, agent: &Agent<OpenAiModelClient>) {
        self.sync_footer_config(agent);
        self.push_message(
            MessageKind::System,
            "micos",
            format!(
                "model: {}\npermission: {}\ncwd: {}\nsession: {}\nlog: {}\nType /help for commands.",
                agent.config().model,
                agent.config().permission,
                agent.config().cwd.display(),
                agent.session_id(),
                agent.session_path().display()
            ),
        );
    }

    async fn run(&mut self, agent: &mut Agent<OpenAiModelClient>) -> Result<()> {
        loop {
            self.render_with_agent(agent)?;
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
                        agent.stop(StopReason::UserInterrupt).await?;
                        self.push_message(MessageKind::Warning, "stopped", "interrupted");
                        self.run_status = RunStatus::Idle;
                        self.render_with_agent(agent)?;
                        break;
                    }

                    if self.handle_model_panel_key(key, agent)? {
                        continue;
                    }
                    if self.handle_scroll_key(key) {
                        continue;
                    }

                    let action = self.composer.handle_key(key);
                    if !self.process_composer_action(action, agent).await? {
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

    fn footer_state(&self, agent: &Agent<OpenAiModelClient>) -> FooterState {
        let mut footer = self.footer_state_from_self();
        footer.model = agent.config().model.clone();
        footer.thinking = agent
            .config()
            .thinking
            .map_or("unset".into(), |value| value.to_string());
        footer.reasoning_effort = agent
            .config()
            .reasoning_effort
            .map_or("unset".into(), |value| value.to_string());
        footer.cwd = agent.config().cwd.display().to_string();
        footer
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

    fn sync_footer_config(&mut self, agent: &Agent<OpenAiModelClient>) {
        self.footer_model = agent.config().model.clone();
        self.footer_thinking = agent
            .config()
            .thinking
            .map_or("unset".into(), |value| value.to_string());
        self.footer_reasoning_effort = agent
            .config()
            .reasoning_effort
            .map_or("unset".into(), |value| value.to_string());
        self.footer_cwd = agent.config().cwd.display().to_string();
    }

    fn render_with_agent(&mut self, agent: &Agent<OpenAiModelClient>) -> Result<()> {
        self.sync_footer_config(agent);
        let footer = self.footer_state(agent);
        self.render_with_footer(&footer)
    }

    fn render(&mut self) -> Result<()> {
        let footer = self.footer_state_from_self();
        self.render_with_footer(&footer)
    }

    fn render_with_footer(&mut self, footer: &FooterState) -> Result<()> {
        let size = self.terminal.size().context("read terminal size")?;
        let bottom_height = bottom_panel_height(&self.composer, self.model_panel.as_ref());
        let message_height = size.height.saturating_sub(3 + bottom_height) as usize;
        let message_width = size.width as usize;
        let lines = build_message_lines(&self.messages, message_width);
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

    async fn run_agent_turn(
        &mut self,
        input: String,
        agent: &mut Agent<OpenAiModelClient>,
    ) -> Result<StopReason> {
        self.run_status = RunStatus::Working;
        let reason = agent.run_turn_with_ui(input, self).await?;
        self.run_status = RunStatus::Idle;
        Ok(reason)
    }

    async fn process_composer_action(
        &mut self,
        action: ComposerAction,
        agent: &mut Agent<OpenAiModelClient>,
    ) -> Result<bool> {
        match action {
            ComposerAction::None => {}
            ComposerAction::Submit(input) => {
                let reason = self.run_agent_turn(input, agent).await?;
                if matches!(reason, StopReason::UserExit | StopReason::UserInterrupt) {
                    return Ok(false);
                }
            }
            ComposerAction::Command(command) => {
                if !self.handle_slash(command, agent).await? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    async fn handle_slash(
        &mut self,
        command: SlashCommand,
        agent: &mut Agent<OpenAiModelClient>,
    ) -> Result<bool> {
        match command {
            SlashCommand::Help => self.push_message(MessageKind::System, "/help", format_help()),
            SlashCommand::Status => self.push_message(
                MessageKind::System,
                "/status",
                format_status(agent.config(), agent.session_id(), agent.session_path()),
            ),
            SlashCommand::Sessions => self.push_message(
                MessageKind::System,
                "/sessions",
                format_sessions(&agent.config().cwd)?,
            ),
            SlashCommand::Transcript => self.push_message(
                MessageKind::System,
                "/transcript",
                format_transcript(agent.session_path())?,
            ),
            SlashCommand::Model => {
                self.model_panel = Some(ModelPanelState::from_settings(
                    agent.config(),
                    self.show_reasoning,
                ));
            }
            SlashCommand::Clear => {
                self.messages.clear();
                self.permission_message = None;
                self.scroll_top = 0;
                self.stick_to_bottom = true;
            }
            SlashCommand::Exit => {
                agent.stop(StopReason::UserExit).await?;
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn handle_model_panel_key(
        &mut self,
        key: KeyEvent,
        agent: &mut Agent<OpenAiModelClient>,
    ) -> Result<bool> {
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
                save_model_settings(&agent.config().cwd, &settings)?;
                agent.apply_model_settings(settings)?;
                self.show_reasoning = show_reasoning;
                self.model_panel = None;
                self.push_message(
                    MessageKind::System,
                    "/model",
                    format_status(agent.config(), agent.session_id(), agent.session_path()),
                );
            }
        }
        Ok(true)
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
            transient: false,
        });
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
            body: format!("{summary}\nEnter/y approve    Esc/n deny"),
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
                call_id,
                name,
                arguments,
                permission,
            } => {
                self.run_status = RunStatus::Tool(name.clone());
                let summary = ToolSummary::from_arguments(&name, &arguments).summary;
                self.push_message(
                    MessageKind::Tool,
                    format!("tool {name}"),
                    format!("permission: {permission}\ncall: {call_id}\n{summary}"),
                );
            }
            AgentEvent::ToolCallFinished {
                call_id,
                name,
                result,
                elapsed,
            } => {
                self.run_status = RunStatus::Working;
                self.push_message(
                    tool_result_kind(&result),
                    format!("tool {name}"),
                    format!(
                        "{} in {:.2?}\ncall: {}\n{}",
                        tool_result_label(&result),
                        elapsed,
                        call_id,
                        summarize_result(&result)
                    ),
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposerState {
    buffer: String,
    cursor: usize,
    popup_open: bool,
    popup_dismissed: bool,
    selected: usize,
}

impl ComposerState {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            popup_open: false,
            popup_dismissed: false,
            selected: 0,
        }
    }

    #[cfg(test)]
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn popup_open(&self) -> bool {
        self.popup_open
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn matches(&self) -> Vec<SlashCommandInfo> {
        if self.buffer.starts_with('/') {
            slash_command_matches(&self.buffer)
        } else {
            Vec::new()
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ComposerAction {
        match key.code {
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert(ch);
                ComposerAction::None
            }
            KeyCode::Backspace => {
                self.backspace();
                ComposerAction::None
            }
            KeyCode::Delete => {
                self.delete();
                ComposerAction::None
            }
            KeyCode::Left => {
                self.move_left();
                ComposerAction::None
            }
            KeyCode::Right => {
                self.move_right();
                ComposerAction::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                ComposerAction::None
            }
            KeyCode::End => {
                self.cursor = self.buffer.len();
                ComposerAction::None
            }
            KeyCode::Up if self.popup_open => {
                self.move_selection(-1);
                ComposerAction::None
            }
            KeyCode::Down if self.popup_open => {
                self.move_selection(1);
                ComposerAction::None
            }
            KeyCode::Tab if self.popup_open => self.accept_popup(),
            KeyCode::Enter => self.enter(),
            KeyCode::Esc if self.popup_open => {
                self.popup_open = false;
                self.popup_dismissed = true;
                ComposerAction::None
            }
            KeyCode::Esc => {
                self.buffer.clear();
                self.cursor = 0;
                self.refresh_popup();
                ComposerAction::None
            }
            _ => ComposerAction::None,
        }
    }

    fn insert(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.popup_dismissed = false;
        self.refresh_popup();
    }

    fn backspace(&mut self) {
        let Some(previous) = self.previous_boundary() else {
            return;
        };
        self.buffer.drain(previous..self.cursor);
        self.cursor = previous;
        self.popup_dismissed = false;
        self.refresh_popup();
    }

    fn delete(&mut self) {
        let Some(next) = self.next_boundary() else {
            return;
        };
        self.buffer.drain(self.cursor..next);
        self.popup_dismissed = false;
        self.refresh_popup();
    }

    fn move_left(&mut self) {
        if let Some(previous) = self.previous_boundary() {
            self.cursor = previous;
        }
    }

    fn move_right(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.cursor = next;
        }
    }

    fn previous_boundary(&self) -> Option<usize> {
        self.buffer
            .grapheme_indices(true)
            .take_while(|(index, _)| *index < self.cursor)
            .last()
            .map(|(index, _)| index)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.buffer
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .find(|index| *index > self.cursor)
            .or_else(|| (self.cursor < self.buffer.len()).then_some(self.buffer.len()))
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.matches().len().min(POPUP_LIMIT);
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = match delta {
            -1 if self.selected == 0 => len - 1,
            -1 => self.selected - 1,
            1 => (self.selected + 1) % len,
            _ => self.selected,
        };
    }

    fn enter(&mut self) -> ComposerAction {
        if self.popup_open {
            let matches = self.matches();
            if !matches.is_empty() {
                return self.accept_popup();
            }
            return ComposerAction::None;
        }
        let trimmed = self.buffer.trim();
        if trimmed.is_empty() {
            self.clear();
            return ComposerAction::None;
        }
        if let Some(command) = slash_command_exact(trimmed) {
            self.clear();
            return ComposerAction::Command(command);
        }
        if trimmed.starts_with('/') {
            return ComposerAction::None;
        }
        let input = trimmed.to_string();
        self.clear();
        ComposerAction::Submit(input)
    }

    fn accept_popup(&mut self) -> ComposerAction {
        let matches = self.matches();
        let Some(command) = matches.get(self.selected.min(matches.len().saturating_sub(1))) else {
            return ComposerAction::None;
        };
        let action = ComposerAction::Command(command.command);
        self.clear();
        action
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.popup_open = false;
        self.popup_dismissed = false;
        self.selected = 0;
    }

    fn refresh_popup(&mut self) {
        self.popup_open = self.buffer.starts_with('/') && !self.popup_dismissed;
        let len = self.matches().len().min(POPUP_LIMIT);
        if len == 0 || self.selected >= len {
            self.selected = 0;
        }
    }

    fn visible_input(&self, max_width: usize) -> (String, usize) {
        if max_width == 0 {
            return (String::new(), 0);
        }
        let graphemes = self
            .buffer
            .grapheme_indices(true)
            .map(|(byte_index, grapheme)| {
                (
                    byte_index,
                    grapheme,
                    UnicodeWidthStr::width(grapheme).max(1),
                )
            })
            .collect::<Vec<_>>();

        let cursor_col = graphemes
            .iter()
            .take_while(|(byte_index, _, _)| *byte_index < self.cursor)
            .map(|(_, _, width)| *width)
            .sum::<usize>();
        let desired_start = cursor_col.saturating_add(1).saturating_sub(max_width);
        let mut col = 0usize;
        let mut start_col = 0usize;
        let mut visible = String::new();

        for (_, grapheme, width) in graphemes {
            let next_col = col + width;
            if next_col <= desired_start {
                col = next_col;
                start_col = col;
                continue;
            }
            if col.saturating_sub(start_col) >= max_width {
                break;
            }
            if next_col.saturating_sub(start_col) > max_width && !visible.is_empty() {
                break;
            }
            visible.push_str(grapheme);
            col = next_col;
        }

        let cursor_x = cursor_col.saturating_sub(start_col).min(max_width);
        (visible, cursor_x)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ComposerAction {
    None,
    Submit(String),
    Command(SlashCommand),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelPreset {
    name: &'static str,
    base_url: &'static str,
}

const MODEL_PRESETS: &[ModelPreset] = &[
    ModelPreset {
        name: "deepseek-v4-flash",
        base_url: DEEPSEEK_CHAT_COMPLETIONS_BASE_URL,
    },
    ModelPreset {
        name: "deepseek-v4-pro",
        base_url: DEEPSEEK_CHAT_COMPLETIONS_BASE_URL,
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelPanelState {
    selected_field: usize,
    model_index: usize,
    thinking_index: usize,
    effort_index: usize,
    show_reasoning: bool,
}

impl ModelPanelState {
    fn from_settings(config: &crate::config::SessionConfig, show_reasoning: bool) -> Self {
        let model_index = MODEL_PRESETS
            .iter()
            .position(|preset| preset.name == config.model)
            .unwrap_or(0);
        let thinking_index = if config.thinking == Some(ThinkingMode::Disabled) {
            0
        } else {
            1
        };
        let effort_index = if config.reasoning_effort == Some(ReasoningEffort::Max) {
            1
        } else {
            0
        };
        Self {
            selected_field: 0,
            model_index,
            thinking_index,
            effort_index,
            show_reasoning,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ModelPanelAction {
        match key.code {
            KeyCode::Esc => ModelPanelAction::Cancel,
            KeyCode::Enter => ModelPanelAction::Apply {
                settings: self.settings(),
                show_reasoning: self.show_reasoning,
            },
            KeyCode::Up => {
                self.selected_field = if self.selected_field == 0 {
                    3
                } else {
                    self.selected_field - 1
                };
                ModelPanelAction::None
            }
            KeyCode::Down | KeyCode::Tab => {
                self.selected_field = (self.selected_field + 1) % 4;
                ModelPanelAction::None
            }
            KeyCode::Left => {
                self.cycle_selected(-1);
                ModelPanelAction::None
            }
            KeyCode::Right => {
                self.cycle_selected(1);
                ModelPanelAction::None
            }
            _ => ModelPanelAction::None,
        }
    }

    fn cycle_selected(&mut self, delta: isize) {
        match self.selected_field {
            0 => self.model_index = cycle_index(self.model_index, MODEL_PRESETS.len(), delta),
            1 => self.thinking_index = cycle_index(self.thinking_index, 2, delta),
            2 => self.effort_index = cycle_index(self.effort_index, 2, delta),
            3 => self.show_reasoning = !self.show_reasoning,
            _ => {}
        }
    }

    fn settings(&self) -> ModelSettings {
        let preset = &MODEL_PRESETS[self.model_index];
        ModelSettings {
            model: preset.name.to_string(),
            base_url: preset.base_url.to_string(),
            thinking: Some(if self.thinking_index == 0 {
                ThinkingMode::Disabled
            } else {
                ThinkingMode::Enabled
            }),
            reasoning_effort: Some(if self.effort_index == 0 {
                ReasoningEffort::High
            } else {
                ReasoningEffort::Max
            }),
        }
    }

    fn field_values(&self) -> [(&'static str, String); 4] {
        [
            ("model", MODEL_PRESETS[self.model_index].name.to_string()),
            (
                "thinking",
                if self.thinking_index == 0 {
                    "disabled"
                } else {
                    "enabled"
                }
                .to_string(),
            ),
            (
                "reasoning",
                if self.effort_index == 0 {
                    "high"
                } else {
                    "max"
                }
                .to_string(),
            ),
            (
                "show",
                if self.show_reasoning {
                    "reasoning"
                } else {
                    "answer only"
                }
                .to_string(),
            ),
        ]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ModelPanelAction {
    None,
    Cancel,
    Apply {
        settings: ModelSettings,
        show_reasoning: bool,
    },
}

fn cycle_index(index: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    if delta.is_negative() {
        index.checked_sub(1).unwrap_or(len - 1)
    } else {
        (index + 1) % len
    }
}

fn next_scroll_top(current: usize, max_top: usize, delta: isize) -> usize {
    if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        current.saturating_add(delta as usize).min(max_top)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FooterState {
    model: String,
    thinking: String,
    reasoning_effort: String,
    cwd: String,
    show_reasoning: bool,
    run_status: RunStatus,
    animation_tick: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RunStatus {
    Idle,
    Working,
    Tool(String),
    WaitingApproval,
}

impl RunStatus {
    fn label(&self, tick: usize) -> String {
        match self {
            RunStatus::Idle => "idle".into(),
            RunStatus::Working => format!("{} Working...", sweep(tick)),
            RunStatus::Tool(name) => format!("{} Running {name}", sweep(tick)),
            RunStatus::WaitingApproval => "Waiting for approval".into(),
        }
    }
}

fn sweep(tick: usize) -> &'static str {
    match tick % 4 {
        0 => ".",
        1 => "·",
        2 => "•",
        _ => "·",
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TuiMessage {
    kind: MessageKind,
    title: String,
    body: String,
    transient: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageKind {
    User,
    Assistant,
    Reasoning,
    Tool,
    System,
    Warning,
    Error,
}

fn draw_frame(
    frame: &mut Frame<'_>,
    message_lines: &[Line<'static>],
    scroll_top: usize,
    composer: &ComposerState,
    model_panel: Option<&ModelPanelState>,
    footer: &FooterState,
) {
    let bottom_height = bottom_panel_height(composer, model_panel);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(bottom_height),
        ])
        .split(frame.size());
    draw_messages(frame, chunks[0], message_lines, scroll_top);
    draw_composer(frame, chunks[1], composer);
    frame.render_widget(Clear, chunks[2]);
    if let Some(panel) = model_panel {
        draw_model_panel(frame, chunks[2], panel);
    } else if composer.popup_open() {
        draw_popup(frame, chunks[2], composer);
    } else {
        draw_footer(frame, chunks[2], footer);
    }
}

fn bottom_panel_height(composer: &ComposerState, model_panel: Option<&ModelPanelState>) -> u16 {
    if model_panel.is_some() {
        9
    } else if composer.popup_open() {
        let rows = composer.matches().len().min(POPUP_LIMIT).max(1);
        rows as u16 + 2
    } else {
        1
    }
}

fn draw_messages(
    frame: &mut Frame<'_>,
    area: Rect,
    message_lines: &[Line<'static>],
    scroll_top: usize,
) {
    let visible_height = area.height as usize;
    let lines = message_lines
        .iter()
        .skip(scroll_top)
        .take(visible_height)
        .cloned()
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_composer(frame: &mut Frame<'_>, area: Rect, composer: &ComposerState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" micos ");
    let inner = block.inner(area);
    let width = inner.width as usize;
    let (visible, cursor_x) = composer.visible_input(width);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(visible), inner);
    frame.set_cursor(inner.x + cursor_x as u16, inner.y);
}

fn draw_popup(frame: &mut Frame<'_>, footer_area: Rect, composer: &ComposerState) {
    let matches = composer.matches();
    let width = 68u16.min(footer_area.width);
    let area = Rect::new(footer_area.x, footer_area.y, width, footer_area.height);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" commands ");
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    if matches.is_empty() {
        frame.render_widget(
            Paragraph::new("no matches").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let lines = matches
        .into_iter()
        .take(POPUP_LIMIT)
        .enumerate()
        .map(|(index, command)| {
            let style = if index == composer.selected() {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(
                    format!("/{:<12}", command.name),
                    style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(command.description, style),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_model_panel(frame: &mut Frame<'_>, footer_area: Rect, panel: &ModelPanelState) {
    let width = 72u16.min(footer_area.width);
    let area = Rect::new(footer_area.x, footer_area.y, width, footer_area.height);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(" model ");
    let mut lines = Vec::new();
    for (index, (label, value)) in panel.field_values().into_iter().enumerate() {
        let style = if index == panel.selected_field {
            Style::default().fg(Color::Black).bg(Color::Blue)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{label:<10}"), style.add_modifier(Modifier::BOLD)),
            Span::styled(value, style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(
        "Up/Down field  Left/Right value  Enter apply  Esc cancel",
    ));
    if panel.thinking_index == 0 {
        lines.push(Line::from(
            "reasoning is saved but not sent while thinking is disabled",
        ));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, footer: &FooterState) {
    let text = footer_text(footer);
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn footer_text(footer: &FooterState) -> String {
    let reasoning = if footer.show_reasoning {
        "reasoning shown"
    } else {
        "reasoning hidden"
    };
    format!(
        "{} · thinking:{} · effort:{} · {} · {} · {}",
        footer.model,
        footer.thinking,
        footer.reasoning_effort,
        footer.cwd,
        reasoning,
        footer.run_status.label(footer.animation_tick)
    )
}

fn build_message_lines(messages: &[TuiMessage], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for message in messages {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(vec![Span::styled(
            message.title.clone(),
            message_style(message.kind).add_modifier(Modifier::BOLD),
        )]));
        for raw_line in message.body.lines() {
            lines.extend(wrap_styled_line(
                raw_line,
                message_style(message.kind),
                width,
            ));
        }
    }
    lines
}

fn wrap_styled_line(text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    if text.is_empty() || width == 0 {
        return vec![Line::from("")];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
        if current_width > 0 && current_width + grapheme_width > width {
            lines.push(Line::from(Span::styled(
                std::mem::take(&mut current),
                style,
            )));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
    }
    if !current.is_empty() {
        lines.push(Line::from(Span::styled(current, style)));
    }
    lines
}

fn message_style(kind: MessageKind) -> Style {
    match kind {
        MessageKind::User => Style::default().fg(Color::Green),
        MessageKind::Assistant => Style::default().fg(Color::White),
        MessageKind::Reasoning => Style::default().fg(Color::DarkGray),
        MessageKind::Tool => Style::default().fg(Color::Cyan),
        MessageKind::System => Style::default().fg(Color::Blue),
        MessageKind::Warning => Style::default().fg(Color::Yellow),
        MessageKind::Error => Style::default().fg(Color::Red),
    }
}

fn is_key_press(key: KeyEvent) -> bool {
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
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

fn tool_result_kind(result: &ToolResult) -> MessageKind {
    if result.success {
        MessageKind::Tool
    } else if result.denied {
        MessageKind::Warning
    } else {
        MessageKind::Error
    }
}

fn tool_result_label(result: &ToolResult) -> &'static str {
    if result.success {
        "ok"
    } else if result.denied {
        "denied"
    } else {
        "failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SessionConfig;
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
            transient: false,
        }];
        let lines = build_message_lines(&messages, 4);
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
            thinking: "enabled".into(),
            reasoning_effort: "high".into(),
            cwd: "/tmp/micos".into(),
            show_reasoning: false,
            run_status: RunStatus::Working,
            animation_tick: 2,
        };
        let text = footer_text(&footer);
        assert!(text.contains("deepseek-v4-flash"));
        assert!(text.contains("reasoning hidden"));
        assert!(text.contains("Working"));
    }

    #[test]
    fn bottom_panel_expands_for_popup_below_composer() {
        let mut composer = ComposerState::new();
        composer.handle_key(key(KeyCode::Char('/')));
        assert!(bottom_panel_height(&composer, None) > 1);
        assert_eq!(bottom_panel_height(&ComposerState::new(), None), 1);
    }
}
