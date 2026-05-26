mod agent;
mod config;
mod model;
mod session;
mod tools;
mod tui;
mod ui;

use agent::Agent;
use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use config::{
    resolve_api_key, ConfigOverrides, PermissionMode, ReasoningEffort, SessionConfig, ThinkingMode,
};
use model::{ModelClient, OpenAiModelClient};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use session::{Session, StopReason};
use std::io::{self, BufRead, IsTerminal};
use std::path::PathBuf;
use ui::{parse_input, ConsoleUi, InputCommand, SlashCommand};

#[derive(Debug, Parser)]
#[command(name = "micos", version, about = "A minimal local agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Chat(ChatArgs),
}

#[derive(Debug, Parser)]
struct ChatArgs {
    #[arg(long, value_enum)]
    permission: Option<PermissionArg>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long)]
    base_url: Option<String>,

    #[arg(long, value_enum)]
    thinking: Option<ThinkingArg>,

    #[arg(long, value_enum)]
    reasoning_effort: Option<ReasoningEffortArg>,

    #[arg(long)]
    max_steps: Option<usize>,

    #[arg(long)]
    cwd: Option<PathBuf>,

    #[arg(long)]
    no_tui: bool,
}

#[derive(Clone, Debug, ValueEnum)]
enum PermissionArg {
    Safe,
    Ask,
    Auto,
}

#[derive(Clone, Debug, ValueEnum)]
enum ThinkingArg {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug, ValueEnum)]
enum ReasoningEffortArg {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl From<ThinkingArg> for ThinkingMode {
    fn from(value: ThinkingArg) -> Self {
        match value {
            ThinkingArg::Enabled => ThinkingMode::Enabled,
            ThinkingArg::Disabled => ThinkingMode::Disabled,
        }
    }
}

impl From<ReasoningEffortArg> for ReasoningEffort {
    fn from(value: ReasoningEffortArg) -> Self {
        match value {
            ReasoningEffortArg::Low => ReasoningEffort::Low,
            ReasoningEffortArg::Medium => ReasoningEffort::Medium,
            ReasoningEffortArg::High => ReasoningEffort::High,
            ReasoningEffortArg::Xhigh => ReasoningEffort::Xhigh,
            ReasoningEffortArg::Max => ReasoningEffort::Max,
        }
    }
}

impl From<PermissionArg> for PermissionMode {
    fn from(value: PermissionArg) -> Self {
        match value {
            PermissionArg::Safe => PermissionMode::Safe,
            PermissionArg::Ask => PermissionMode::Ask,
            PermissionArg::Auto => PermissionMode::Auto,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match cli.command {
        Commands::Chat(args) => run_chat(args).await,
    }
}

async fn run_chat(args: ChatArgs) -> anyhow::Result<()> {
    let overrides = ConfigOverrides {
        model: args.model,
        base_url: args.base_url,
        thinking: args.thinking.map(Into::into),
        reasoning_effort: args.reasoning_effort.map(Into::into),
        permission: args.permission.map(Into::into),
        max_steps: args.max_steps,
        cwd: args.cwd,
    };
    let config = SessionConfig::load(overrides).context("load configuration")?;
    let session = Session::new(&config).context("create session")?;
    let api_key = resolve_api_key()?;
    let client = OpenAiModelClient::new(api_key, config.api_kind, config.base_url.clone());
    let agent = Agent::new(config, client, session);

    if should_use_tui(
        args.no_tui,
        io::stdin().is_terminal(),
        io::stdout().is_terminal(),
    ) {
        return tui::run_tui_chat(agent).await;
    }

    let mut agent = agent;
    run_console_chat(&mut agent, io::stdin().is_terminal()).await
}

async fn run_console_chat<C: ModelClient>(
    agent: &mut Agent<C>,
    interactive: bool,
) -> anyhow::Result<()> {
    let mut ui = ConsoleUi::new();
    ui.banner(agent.config(), agent.session_id(), agent.session_path());

    if !interactive {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            if !handle_console_line(line.context("read stdin line")?, agent, &mut ui).await? {
                break;
            }
        }
        return Ok(());
    }

    let mut editor = DefaultEditor::new()?;
    loop {
        match editor.readline(&format!("{} ", ui.prompt())) {
            Ok(line) => {
                let _ = editor.add_history_entry(line.trim());
                if !handle_console_line(line, agent, &mut ui).await? {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) => {
                agent.stop(StopReason::UserInterrupt).await?;
                eprintln!("Interrupted.");
                break;
            }
            Err(ReadlineError::Eof) => {
                agent.stop(StopReason::UserExit).await?;
                break;
            }
            Err(err) => return Err(err.into()),
        }
    }

    Ok(())
}

async fn handle_console_line<C: ModelClient>(
    line: String,
    agent: &mut Agent<C>,
    ui: &mut ConsoleUi,
) -> anyhow::Result<bool> {
    match parse_input(&line) {
        InputCommand::Empty => Ok(true),
        InputCommand::UserText(input) => {
            let reason = agent.run_turn_with_ui(input, ui).await?;
            Ok(!matches!(
                reason,
                StopReason::UserExit | StopReason::UserInterrupt
            ))
        }
        InputCommand::Slash(command) => handle_console_slash(command, agent, ui).await,
        InputCommand::UnknownSlash(command) => {
            eprintln!("Unknown command: {command}. Type /help.");
            Ok(true)
        }
    }
}

async fn handle_console_slash<C: ModelClient>(
    command: SlashCommand,
    agent: &mut Agent<C>,
    ui: &mut ConsoleUi,
) -> anyhow::Result<bool> {
    match command {
        SlashCommand::Help => ui.print_help(),
        SlashCommand::Status => {
            ui.print_status(agent.config(), agent.session_id(), agent.session_path())
        }
        SlashCommand::Sessions => ui.print_sessions(&agent.config().cwd)?,
        SlashCommand::Transcript => ui.print_transcript(agent.session_path())?,
        SlashCommand::Model => {
            eprintln!("The /model picker is only available in TUI mode. Restart without --no-tui.")
        }
        SlashCommand::Clear => ui.clear()?,
        SlashCommand::Exit => {
            agent.stop(StopReason::UserExit).await?;
            return Ok(false);
        }
    }
    Ok(true)
}

fn should_use_tui(no_tui: bool, stdin_tty: bool, stdout_tty: bool) -> bool {
    !no_tui && stdin_tty && stdout_tty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_is_used_only_for_default_interactive_terminal() {
        assert!(should_use_tui(false, true, true));
        assert!(!should_use_tui(true, true, true));
        assert!(!should_use_tui(false, false, true));
        assert!(!should_use_tui(false, true, false));
    }
}
