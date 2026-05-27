use super::state::{ComposerState, ModelPanelState};
use super::POPUP_LIMIT;
use crate::config::{ReasoningEffort, ThinkingMode};
use crate::tools::ToolResult;
use crossterm::event::{KeyEvent, KeyEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone)]
pub(super) struct FooterState {
    pub(super) model: String,
    pub(super) thinking: String,
    pub(super) reasoning_effort: String,
    pub(super) cwd: String,
    pub(super) show_reasoning: bool,
    pub(super) run_status: RunStatus,
    pub(super) animation_tick: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RunStatus {
    Idle,
    Working,
    Tool(String),
    Handoff(String),
    WaitingApproval,
}

impl RunStatus {
    pub(super) fn label(&self, _tick: usize) -> String {
        match self {
            RunStatus::Idle => "idle".into(),
            RunStatus::Working => "Working...".into(),
            RunStatus::Tool(name) => format!("Running {name}"),
            RunStatus::Handoff(trigger) => format!("Writing handoff ({trigger})"),
            RunStatus::WaitingApproval => "Waiting for approval".into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TuiMessage {
    pub(super) kind: MessageKind,
    pub(super) title: String,
    pub(super) body: String,
    pub(super) status: Option<MessageStatus>,
    pub(super) transient: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MessageStatus {
    Running,
    Success,
    Failed,
    Neutral,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MessageKind {
    User,
    Assistant,
    Reasoning,
    Tool,
    System,
    Warning,
    Error,
}

pub(super) fn draw_frame(
    frame: &mut Frame<'_>,
    message_lines: &[Line<'static>],
    scroll_top: usize,
    composer: &ComposerState,
    model_panel: Option<&ModelPanelState>,
    approval_selected: Option<usize>,
    footer: &FooterState,
) {
    let bottom_height = bottom_panel_height(composer, model_panel, approval_selected);
    let working_height = working_panel_height(footer);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(working_height),
            Constraint::Length(3),
            Constraint::Length(bottom_height),
        ])
        .split(frame.size());
    draw_messages(frame, chunks[0], message_lines, scroll_top);
    draw_working_line(frame, chunks[1], footer);
    draw_composer(frame, chunks[2], composer);
    frame.render_widget(Clear, chunks[3]);
    if let Some(panel) = model_panel {
        draw_model_panel(frame, chunks[3], panel);
    } else if let Some(selected) = approval_selected {
        draw_approval_picker(frame, chunks[3], selected);
    } else if composer.popup_open() {
        draw_popup(frame, chunks[3], composer);
    } else {
        draw_footer(frame, chunks[3], footer);
    }
}

pub(super) fn working_panel_height(footer: &FooterState) -> u16 {
    if matches!(footer.run_status, RunStatus::Idle) {
        0
    } else {
        1
    }
}

pub(super) fn bottom_panel_height(
    composer: &ComposerState,
    model_panel: Option<&ModelPanelState>,
    approval_selected: Option<usize>,
) -> u16 {
    if model_panel.is_some() {
        9
    } else if approval_selected.is_some() {
        6
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

fn draw_working_line(frame: &mut Frame<'_>, area: Rect, footer: &FooterState) {
    if area.height == 0 || matches!(footer.run_status, RunStatus::Idle) {
        return;
    }
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(working_line(footer)), area);
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

fn working_line(footer: &FooterState) -> Line<'static> {
    let label = footer.run_status.label(footer.animation_tick);
    let highlight = footer.animation_tick % label.chars().count().max(1);
    let mut spans = vec![Span::styled(
        "● ",
        status_style(MessageStatus::Running, footer.animation_tick).add_modifier(Modifier::BOLD),
    )];
    for (index, ch) in label.chars().enumerate() {
        let style = if index == highlight {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(ch.to_string(), style));
    }
    Line::from(spans)
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

    let selected = composer.selected().min(matches.len().saturating_sub(1));
    let visible_rows = POPUP_LIMIT.min(matches.len());
    let start = popup_window_start(selected, visible_rows, matches.len());
    let lines = matches
        .into_iter()
        .skip(start)
        .take(visible_rows)
        .enumerate()
        .map(|(index, item)| {
            let command_index = start + index;
            let style = if command_index == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(
                    format!("{:<37}", item.label()),
                    style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(item.description(), style),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn popup_window_start(selected: usize, visible_rows: usize, total: usize) -> usize {
    if total <= visible_rows {
        return 0;
    }
    selected.saturating_add(1).saturating_sub(visible_rows)
}

fn draw_approval_picker(frame: &mut Frame<'_>, footer_area: Rect, selected: usize) {
    let width = 68u16.min(footer_area.width);
    let area = Rect::new(footer_area.x, footer_area.y, width, footer_area.height);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" approval ");
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let items = [
        ("Session", "remember this rule for this session"),
        ("Once", "allow this call only"),
        ("Project", "save this rule to .micos/config.toml"),
        ("Deny", "skip this tool call"),
    ];
    let lines = items
        .into_iter()
        .enumerate()
        .map(|(index, (label, description))| {
            let style = if index == selected {
                Style::default().fg(Color::Black).bg(Color::Yellow)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(format!("{label:<12}"), style.add_modifier(Modifier::BOLD)),
                Span::styled(description, style),
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

pub(super) fn footer_text(footer: &FooterState) -> String {
    let reasoning = if footer.show_reasoning {
        "reasoning shown"
    } else {
        "reasoning hidden"
    };
    format!(
        "{} · {}/{} · {} · {}",
        footer.model, footer.thinking, footer.reasoning_effort, footer.cwd, reasoning
    )
}

pub(super) fn short_thinking(thinking: Option<ThinkingMode>) -> &'static str {
    match thinking {
        Some(ThinkingMode::Enabled) => "think",
        Some(ThinkingMode::Disabled) => "nothink",
        None => "unset",
    }
}

pub(super) fn short_reasoning_effort(effort: Option<ReasoningEffort>) -> &'static str {
    match effort {
        Some(ReasoningEffort::Low) => "low",
        Some(ReasoningEffort::Medium) => "med",
        Some(ReasoningEffort::High) => "high",
        Some(ReasoningEffort::Xhigh) => "xhigh",
        Some(ReasoningEffort::Max) => "max",
        None => "unset",
    }
}

pub(super) fn build_message_lines(
    messages: &[TuiMessage],
    width: usize,
    animation_tick: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for message in messages {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        if !matches!(message.kind, MessageKind::User | MessageKind::Assistant) {
            lines.push(message_title_line(message, animation_tick));
        }
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

fn message_title_line(message: &TuiMessage, animation_tick: usize) -> Line<'static> {
    let mut spans = Vec::new();
    if let Some(status) = visible_status(message) {
        spans.push(Span::styled(
            "● ",
            status_style(status, animation_tick).add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(
        message.title.clone(),
        message_style(message.kind).add_modifier(Modifier::BOLD),
    ));
    Line::from(spans)
}

pub(super) fn visible_status(message: &TuiMessage) -> Option<MessageStatus> {
    if matches!(message.kind, MessageKind::User | MessageKind::Assistant) {
        None
    } else {
        message.status
    }
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

pub(super) fn default_status_for_kind(kind: MessageKind) -> Option<MessageStatus> {
    match kind {
        MessageKind::User | MessageKind::Assistant => None,
        MessageKind::Reasoning | MessageKind::System | MessageKind::Tool => {
            Some(MessageStatus::Neutral)
        }
        MessageKind::Warning | MessageKind::Error => Some(MessageStatus::Failed),
    }
}

pub(super) fn status_style(status: MessageStatus, animation_tick: usize) -> Style {
    match status {
        MessageStatus::Running if animation_tick % 2 == 0 => Style::default().fg(Color::DarkGray),
        MessageStatus::Running => Style::default().fg(Color::Gray),
        MessageStatus::Success => Style::default().fg(Color::Green),
        MessageStatus::Failed => Style::default().fg(Color::Red),
        MessageStatus::Neutral => Style::default().fg(Color::DarkGray),
    }
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

pub(super) fn is_key_press(key: KeyEvent) -> bool {
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

pub(super) fn finish_tool_message(
    messages: &mut Vec<TuiMessage>,
    active_tool_message: &mut Option<usize>,
    name: &str,
    result: ToolResult,
    elapsed: Duration,
) {
    let kind = tool_result_kind(&result);
    let title = format!("tool {name}");
    let body = tool_finished_body(&result, elapsed);
    let status = Some(status_for_tool_result(&result));
    if let Some(index) = active_tool_message
        .take()
        .filter(|index| *index < messages.len() && messages[*index].title == title)
    {
        messages[index] = TuiMessage {
            kind,
            title,
            body,
            status,
            transient: false,
        };
    } else {
        messages.push(TuiMessage {
            kind,
            title,
            body,
            status,
            transient: false,
        });
    }
}

fn tool_finished_body(result: &ToolResult, elapsed: Duration) -> String {
    format!(
        "{} in {}\n{}",
        tool_result_label(result),
        format_duration(elapsed),
        summarize_result(result)
    )
}

fn status_for_tool_result(result: &ToolResult) -> MessageStatus {
    if result.success {
        MessageStatus::Success
    } else {
        MessageStatus::Failed
    }
}

pub(super) fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 1.0 {
        format!("{}ms", duration.as_millis())
    } else if seconds < 10.0 {
        format!("{seconds:.1}s")
    } else {
        format!("{seconds:.0}s")
    }
}
