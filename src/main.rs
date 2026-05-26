mod agent;
mod config;
mod model;
mod session;
mod tools;
mod ui;

use agent::Agent;
use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use config::{
    resolve_api_key, ConfigOverrides, PermissionMode, ReasoningEffort, SessionConfig, ThinkingMode,
};
use model::OpenAiModelClient;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use session::{Session, StopReason};
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
    let mut agent = Agent::new(config, client, session);
    let mut ui = ConsoleUi::new();
    ui.banner(agent.config(), agent.session_id(), agent.session_path());

    let mut editor = DefaultEditor::new()?;
    loop {
        match editor.readline(&format!("{} ", ui.prompt())) {
            Ok(line) => {
                let _ = editor.add_history_entry(line.trim());
                match parse_input(&line) {
                    InputCommand::Empty => continue,
                    InputCommand::UserText(input) => {
                        let reason = agent.run_turn_with_ui(input, &mut ui).await?;
                        if matches!(reason, StopReason::UserExit | StopReason::UserInterrupt) {
                            break;
                        }
                    }
                    InputCommand::Slash(command) => match command {
                        SlashCommand::Help => ui.print_help(),
                        SlashCommand::Status => ui.print_status(
                            agent.config(),
                            agent.session_id(),
                            agent.session_path(),
                        ),
                        SlashCommand::Clear => ui.clear()?,
                        SlashCommand::Sessions => ui.print_sessions(&agent.config().cwd)?,
                        SlashCommand::Transcript => ui.print_transcript(agent.session_path())?,
                        SlashCommand::Exit => {
                            agent.stop(StopReason::UserExit).await?;
                            break;
                        }
                    },
                    InputCommand::UnknownSlash(command) => {
                        eprintln!("Unknown command: {command}. Type /help.");
                    }
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
