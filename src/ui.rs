mod console;
mod events;
mod slash;

pub use console::ConsoleUi;
pub use events::{AgentEvent, ApprovalDecision, NullUi, UiSink};
pub use slash::{
    format_active_plan, format_compact_report, format_context, format_handoff_report, format_help,
    format_memory, format_memory_index, format_prompt, format_resume_report, format_sessions,
    format_status, format_summary, format_trace, format_transcript, parse_input,
    slash_command_exact, slash_command_matches, InputCommand, SlashCommand, SlashCommandInfo,
    SlashInvocation, SLASH_COMMANDS,
};
