use super::{format_help, format_sessions, format_status, format_transcript, AgentEvent, UiSink};
use crate::config::SessionConfig;
use crate::session::StopReason;
use crate::tools::{ToolResult, ToolSummary};
use anyhow::{Context, Result};
use console::{style, Term};
use crossterm::{
    execute,
    terminal::{Clear, ClearType},
};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

pub struct ConsoleUi {
    term: Term,
    color: bool,
    plain: bool,
    active_spinner: Option<ProgressBar>,
    assistant_open: bool,
    reasoning_open: bool,
}

impl ConsoleUi {
    pub fn new() -> Self {
        let term = Term::stdout();
        let plain = !term.is_term();
        let color = std::env::var_os("NO_COLOR").is_none() && !plain;
        Self {
            term,
            color,
            plain,
            active_spinner: None,
            assistant_open: false,
            reasoning_open: false,
        }
    }

    pub fn prompt(&self) -> String {
        if self.color {
            format!("{}", style("micos>").cyan().bold())
        } else {
            "micos>".to_string()
        }
    }

    pub fn banner(&mut self, config: &SessionConfig, session_id: Uuid, session_path: &Path) {
        if self.plain {
            println!(
                "micos session {session_id} | api={} | model={} | permission={} | cwd={} | log={}",
                config.api_kind,
                config.model,
                config.permission,
                config.cwd.display(),
                session_path.display()
            );
            println!("Type /help for commands.");
            return;
        }

        let title = self.paint("micos", Paint::AccentBold);
        println!("{title}  local agent harness");
        println!(
            "{} {}  {} {}  {} {}",
            self.paint("model", Paint::Muted),
            config.model,
            self.paint("permission", Paint::Muted),
            config.permission,
            self.paint("cwd", Paint::Muted),
            config.cwd.display()
        );
        println!(
            "{} {}  {} {}",
            self.paint("session", Paint::Muted),
            session_id,
            self.paint("log", Paint::Muted),
            session_path.display()
        );
        println!("{}", self.paint("Type /help for commands.", Paint::Muted));
    }

    pub fn print_help(&mut self) {
        println!("{}", format_help());
    }

    pub fn print_status(&mut self, config: &SessionConfig, session_id: Uuid, session_path: &Path) {
        println!("{}", format_status(config, session_id, session_path));
    }

    pub fn print_sessions(&mut self, cwd: &Path) -> Result<()> {
        println!("{}", format_sessions(cwd)?);
        Ok(())
    }

    pub fn print_transcript(&mut self, path: &Path) -> Result<()> {
        println!("{}", format_transcript(path)?);
        Ok(())
    }

    pub fn clear(&mut self) -> Result<()> {
        if self.plain {
            return Ok(());
        }
        execute!(io::stdout(), Clear(ClearType::All)).context("clear terminal")?;
        self.term.move_cursor_to(0, 0).context("move cursor")?;
        Ok(())
    }

    fn finish_spinner(&mut self) {
        if let Some(spinner) = self.active_spinner.take() {
            spinner.finish_and_clear();
        }
    }

    fn ensure_line_after_stream(&mut self) {
        if self.assistant_open || self.reasoning_open {
            println!();
            self.assistant_open = false;
            self.reasoning_open = false;
        }
    }

    fn paint(&self, text: impl AsRef<str>, paint: Paint) -> String {
        let text = text.as_ref();
        if !self.color {
            return text.to_string();
        }
        match paint {
            Paint::AccentBold => style(text).cyan().bold().to_string(),
            Paint::Muted => style(text).dim().to_string(),
            Paint::Success => style(text).green().to_string(),
            Paint::Error => style(text).red().to_string(),
            Paint::Warn => style(text).yellow().to_string(),
        }
    }
}

impl UiSink for ConsoleUi {
    fn on_event(&mut self, event: AgentEvent) -> Result<()> {
        match event {
            AgentEvent::TurnStarted { input } => {
                self.finish_spinner();
                self.assistant_open = false;
                self.reasoning_open = false;
                if self.plain {
                    println!("user: {input}");
                }
            }
            AgentEvent::AssistantDelta { text } => {
                self.finish_spinner();
                if !self.assistant_open {
                    print!("{}", self.paint("assistant: ", Paint::AccentBold));
                    self.assistant_open = true;
                }
                print!("{text}");
                io::stdout().flush().context("flush assistant delta")?;
            }
            AgentEvent::ReasoningDelta { text } => {
                self.finish_spinner();
                if !self.reasoning_open {
                    print!("{}", self.paint("reasoning: ", Paint::Muted));
                    self.reasoning_open = true;
                }
                if self.color {
                    print!("{}", style(text).dim());
                } else {
                    print!("{text}");
                }
                io::stdout().flush().context("flush reasoning delta")?;
            }
            AgentEvent::ToolCallStarted {
                call_id: _,
                name,
                arguments,
                permission,
            } => {
                self.ensure_line_after_stream();
                let summary = ToolSummary::from_arguments(&name, &arguments).summary;
                println!(
                    "{} {}  {} {}",
                    self.paint("tool", Paint::AccentBold),
                    name,
                    self.paint("permission", Paint::Muted),
                    permission
                );
                println!("  {summary}");
                if !self.plain {
                    let spinner = ProgressBar::new_spinner();
                    spinner.set_style(
                        ProgressStyle::with_template("{spinner:.cyan} {msg}")
                            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                    );
                    spinner.set_message(format!("running {name}"));
                    spinner.enable_steady_tick(Duration::from_millis(90));
                    self.active_spinner = Some(spinner);
                }
            }
            AgentEvent::ToolCallFinished {
                call_id: _,
                name,
                result,
                elapsed,
            } => {
                self.finish_spinner();
                let status = if result.success {
                    self.paint("ok", Paint::Success)
                } else if result.denied {
                    self.paint("denied", Paint::Warn)
                } else {
                    self.paint("failed", Paint::Error)
                };
                println!(
                    "{} {}  {}  {} {:.2?}",
                    self.paint("tool", Paint::Muted),
                    name,
                    status,
                    self.paint("elapsed", Paint::Muted),
                    elapsed
                );
                let summary = summarize_result(&result);
                if !summary.is_empty() {
                    println!("  {summary}");
                }
            }
            AgentEvent::PermissionPrompt { name, summary } => {
                self.ensure_line_after_stream();
                println!(
                    "{} {}  {}",
                    self.paint("permission", Paint::Warn),
                    name,
                    summary
                );
            }
            AgentEvent::Stop { reason } => {
                self.finish_spinner();
                self.ensure_line_after_stream();
                if reason != StopReason::FinalAnswer {
                    println!("{} {reason}", self.paint("stopped:", Paint::Warn));
                }
            }
            AgentEvent::Error { message } => {
                self.finish_spinner();
                self.ensure_line_after_stream();
                println!("{} {message}", self.paint("error:", Paint::Error));
            }
        }
        Ok(())
    }

    fn approve_tool(&mut self, name: &str, summary: &str) -> Result<bool> {
        self.on_event(AgentEvent::PermissionPrompt {
            name: name.to_string(),
            summary: summary.to_string(),
        })?;
        print!("Allow? [y/N] ");
        io::stdout().flush().context("flush permission prompt")?;
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("read permission response")?;
        Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
    }
}

enum Paint {
    AccentBold,
    Muted,
    Success,
    Error,
    Warn,
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
