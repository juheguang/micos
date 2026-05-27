use crate::config::{
    ModelSettings, ReasoningEffort, ThinkingMode, DEEPSEEK_CHAT_COMPLETIONS_BASE_URL,
};
use crate::ui::{
    parse_input, slash_command_matches, ApprovalDecision, InputCommand, SlashCommandInfo,
    SlashInvocation,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::mpsc as std_mpsc;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) struct ApprovalPickerState {
    selected: usize,
    response: Option<std_mpsc::Sender<ApprovalDecision>>,
}

impl ApprovalPickerState {
    pub(super) fn new(response: std_mpsc::Sender<ApprovalDecision>) -> Self {
        Self {
            selected: 0,
            response: Some(response),
        }
    }

    pub(super) fn selected(&self) -> usize {
        self.selected
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ApprovalAction {
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                self.selected = if matches!(key.code, KeyCode::Up | KeyCode::Left) {
                    self.selected.checked_sub(1).unwrap_or(3)
                } else {
                    (self.selected + 1) % 4
                };
                ApprovalAction::None
            }
            KeyCode::Tab | KeyCode::Enter => ApprovalAction::Decide(self.selected_decision()),
            KeyCode::Char('s') | KeyCode::Char('S') => {
                ApprovalAction::Decide(ApprovalDecision::AllowSession)
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                ApprovalAction::Decide(ApprovalDecision::AllowOnce)
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                ApprovalAction::Decide(ApprovalDecision::AllowProject)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                ApprovalAction::Decide(ApprovalDecision::Deny)
            }
            _ => ApprovalAction::None,
        }
    }

    pub(super) fn send(&mut self, decision: ApprovalDecision) {
        if let Some(response) = self.response.take() {
            let _ = response.send(decision);
        }
    }

    fn selected_decision(&self) -> ApprovalDecision {
        match self.selected {
            0 => ApprovalDecision::AllowSession,
            1 => ApprovalDecision::AllowOnce,
            2 => ApprovalDecision::AllowProject,
            _ => ApprovalDecision::Deny,
        }
    }
}

impl Drop for ApprovalPickerState {
    fn drop(&mut self) {
        self.send(ApprovalDecision::Deny);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ApprovalAction {
    None,
    Decide(ApprovalDecision),
}

#[derive(Clone, Debug)]
pub(super) struct ComposerState {
    buffer: String,
    cursor: usize,
    pub(super) popup_open: bool,
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

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ComposerAction {
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
        let len = self.matches().len();
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
        if let InputCommand::Slash(invocation) = parse_input(trimmed) {
            self.clear();
            return ComposerAction::Command(invocation);
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
        let action = ComposerAction::Command(SlashInvocation {
            command: command.command,
            args: String::new(),
        });
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
        let slash_without_args = self
            .buffer
            .strip_prefix('/')
            .is_some_and(|body| !body.chars().any(char::is_whitespace));
        self.popup_open = slash_without_args && !self.popup_dismissed;
        let len = self.matches().len();
        if len == 0 || self.selected >= len {
            self.selected = 0;
        }
    }

    pub(super) fn visible_input(&self, max_width: usize) -> (String, usize) {
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
pub(super) enum ComposerAction {
    None,
    Submit(String),
    Command(SlashInvocation),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ModelPreset {
    name: &'static str,
    base_url: &'static str,
}

pub(super) const MODEL_PRESETS: &[ModelPreset] = &[
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
pub(super) struct ModelPanelState {
    pub(super) selected_field: usize,
    model_index: usize,
    pub(super) thinking_index: usize,
    effort_index: usize,
    pub(super) show_reasoning: bool,
}

impl ModelPanelState {
    pub(super) fn from_settings(
        config: &crate::config::SessionConfig,
        show_reasoning: bool,
    ) -> Self {
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

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> ModelPanelAction {
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

    pub(super) fn settings(&self) -> ModelSettings {
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

    pub(super) fn field_values(&self) -> [(&'static str, String); 4] {
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
pub(super) enum ModelPanelAction {
    None,
    Cancel,
    Apply {
        settings: ModelSettings,
        show_reasoning: bool,
    },
}

pub(super) fn cycle_index(index: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    if delta.is_negative() {
        index.checked_sub(1).unwrap_or(len - 1)
    } else {
        (index + 1) % len
    }
}

pub(super) fn next_scroll_top(current: usize, max_top: usize, delta: isize) -> usize {
    if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        current.saturating_add(delta as usize).min(max_top)
    }
}
