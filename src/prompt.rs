use crate::config::{ApiKind, PermissionMode, SessionConfig};
use crate::context::estimate_text_tokens;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

#[derive(Clone, Debug, Default)]
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(config: &SessionConfig, runtime: &PromptRuntimeContext) -> PromptBuild {
        let mut sections = BASE_SECTIONS
            .iter()
            .map(|section| PromptSectionView::new(section.id, section.title, "base", section.body))
            .collect::<Vec<_>>();

        sections.push(PromptSectionView::new(
            "runtime",
            "Runtime context",
            "runtime",
            runtime.body(),
        ));

        if let Some(memory) = runtime.project_memory.as_ref() {
            sections.push(PromptSectionView::new(
                "project_memory",
                "Project memory",
                memory.source.display().to_string(),
                memory.text.clone(),
            ));
        }

        if let Some(active_plan) = runtime.active_plan.as_ref() {
            sections.push(PromptSectionView::new(
                "active_plan",
                "Active plan",
                active_plan.source.display().to_string(),
                active_plan.text.clone(),
            ));
        }

        if runtime.permission == crate::config::PermissionMode::Plan {
            sections.push(PromptSectionView::new(
                "plan_mode",
                "Plan mode",
                "plan_mode",
                "\
You are in plan mode — explore and design before implementing. \
Do NOT write code or edit source files. \
The only file you can write is .micos/plans/active.md for your plan. \
Use read-only tools and safe shell commands to gather information.\n\
\n\
Work through these phases:\n\
\n\
Phase 1 — Initial Understanding: Explore the codebase. \
Read relevant files to understand existing patterns, architecture, \
and dependencies. Identify what needs to change and what depends on it.\n\
\n\
Phase 2 — Design: Design the implementation approach. \
Consider edge cases, error handling, and testing. \
Break the work into concrete, ordered tasks — use task_create \
to define each step.\n\
\n\
Phase 3 — Write the plan: Write your plan to .micos/plans/active.md \
using write_file. Write to that path ONLY. \
Include: what files to change, the approach for each change, \
a task list, and verification steps. Keep it concise but actionable.\n\
\n\
Phase 4 — Call exit_plan_mode: When the plan is complete, \
call exit_plan_mode. The user will review your plan and can \
approve it, request changes, or provide additional guidance.",
            ));
        }

        if let Some(append) = config.append_system_prompt.as_deref() {
            let append = append.trim();
            if !append.is_empty() {
                sections.push(PromptSectionView::new(
                    "project_append",
                    "Project additional instructions",
                    "config.append_system_prompt",
                    append,
                ));
            }
        }

        let instructions = sections
            .iter()
            .map(|section| format!("## {}\n{}", section.title, section.body))
            .collect::<Vec<_>>()
            .join("\n\n");

        PromptBuild {
            instructions,
            sections,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptBuild {
    pub instructions: String,
    pub sections: Vec<PromptSectionView>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptSectionSnapshot {
    pub id: String,
    pub title: String,
    pub source: String,
    pub tokens_estimate: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptSectionView {
    pub id: String,
    pub title: String,
    pub source: String,
    pub tokens_estimate: usize,
    pub body: String,
}

impl PromptSectionView {
    fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        source: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        let body = body.into();
        Self {
            id: id.into(),
            title: title.into(),
            source: source.into(),
            tokens_estimate: estimate_text_tokens(&body),
            body,
        }
    }

    pub fn snapshot(&self) -> PromptSectionSnapshot {
        PromptSectionSnapshot {
            id: self.id.clone(),
            title: self.title.clone(),
            source: self.source.clone(),
            tokens_estimate: self.tokens_estimate,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptRuntimeContext {
    pub cwd: PathBuf,
    pub model: String,
    pub api_kind: ApiKind,
    pub permission: PermissionMode,
    pub context_window_tokens: usize,
    pub current_date: String,
    pub project_memory: Option<PromptMemoryIndex>,
    pub active_plan: Option<PromptMemoryIndex>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptMemoryIndex {
    pub source: PathBuf,
    pub text: String,
}

impl PromptRuntimeContext {
    pub fn from_config(config: &SessionConfig) -> Self {
        Self {
            cwd: config.cwd.clone(),
            model: config.model.clone(),
            api_kind: config.api_kind,
            permission: config.permission,
            context_window_tokens: config.context_window_tokens,
            current_date: OffsetDateTime::now_utc().date().to_string(),
            project_memory: None,
            active_plan: None,
        }
    }

    pub fn with_project_memory(
        mut self,
        source: impl AsRef<Path>,
        text: impl Into<String>,
    ) -> Self {
        self.project_memory = Some(PromptMemoryIndex {
            source: source.as_ref().to_path_buf(),
            text: text.into(),
        });
        self
    }

    pub fn with_active_plan(mut self, source: impl AsRef<Path>, text: impl Into<String>) -> Self {
        self.active_plan = Some(PromptMemoryIndex {
            source: source.as_ref().to_path_buf(),
            text: text.into(),
        });
        self
    }

    fn body(&self) -> String {
        [
            format!("cwd: {}", self.cwd.display()),
            format!("model: {}", self.model),
            format!("api kind: {}", self.api_kind),
            format!("permission mode: {}", self.permission),
            format!("context window tokens: {}", self.context_window_tokens),
            format!("current date: {}", self.current_date),
        ]
        .join("\n")
    }
}

struct PromptSection {
    id: &'static str,
    title: &'static str,
    body: &'static str,
}

const BASE_SECTIONS: &[PromptSection] = &[
    PromptSection {
        id: "identity",
        title: "Identity",
        body: "\
You are micos, a local coding agent running in the user's workspace. Your job is \
to help with software engineering tasks: read relevant context, edit files when \
asked, run commands when needed, and explain results directly. Be concise, \
precise, and focused on the task at hand.",
    },
    PromptSection {
        id: "task_discipline",
        title: "Task discipline",
        body: "\
Stay focused on the current user request and do only what was asked. Read \
relevant files before changing code. Keep edits scoped to the nearby subsystem \
and avoid unrelated refactors unless they are required to finish the task. \
Do not add features, error handling, or abstractions beyond what the task \
requires. Three similar lines of code is better than a premature abstraction. \
For long tasks, keep the current goal and next step clear. Do not invent test \
results or completed work.",
    },
    PromptSection {
        id: "tool_selection",
        title: "Tool selection",
        body: "\
Prefer dedicated tools over the shell when one exists for your purpose. \
Use `grep` instead of `shell(rg ...)` to search file contents. \
Use `glob` instead of `shell(find ...)` to locate files by pattern. \
Use `edit` instead of `write_file` when modifying an existing file. \
Use `write_file` for creating new files or when a full rewrite is needed. \
Use `shell` only for operations without a dedicated tool: git commands, \
package managers, build tools, test runners. \
Read-only tools (grep, glob, read_file, list_files) are always allowed and \
should be used freely to understand the codebase. \
Independent tool calls can be made in parallel.",
    },
    PromptSection {
        id: "file_editing",
        title: "File editing",
        body: "\
Read a file before editing it to confirm its current content. \
Use `edit` with search/replace to change a specific portion, or with line range \
to replace a span of lines. Only the targeted text is changed; the rest of the \
file is untouched. \
When using search/replace mode, provide the exact text to find — the harness \
will replace the first occurrence. If the search text is not found, the edit \
fails and you should re-read the file. \
Use `write_file` to create a new file or when the entire content needs to be \
rewritten. It overwrites the file completely, creating parent directories as \
needed. \
Keep edits minimal. Do not rename variables, reformat, or restructure code \
that is unrelated to the task. Do not write comments unless the WHY is \
non-obvious — well-named functions and variables speak for themselves.",
    },
    PromptSection {
        id: "permissions",
        title: "Permissions",
        body: "\
Respect permission denials as runtime constraints. When a tool call is denied, \
do not retry the same call — choose a permitted alternative path, or explain \
the blocker when no permitted path can satisfy the request. \
In `safe` mode, mutating tools (write_file, edit, shell) are restricted; use \
read-only tools to gather information. \
In `ask` mode, the user will be prompted to approve mutating operations. The \
tool summary is shown in the approval prompt. Approving for the session adds a \
temporary rule; approving for the project persists the rule to config. \
In `auto` mode, mutating tools on project files are allowed automatically. \
Destructive shell commands (rm, sudo, git push) remain denied in all modes.",
    },
    PromptSection {
        id: "error_recovery",
        title: "Error recovery",
        body: "\
When a tool fails, inspect the error before retrying. \
A timeout means the command took too long — try a narrower scope, a shorter \
timeout, or break the work into smaller steps. \
A permission error means the path or command is blocked — choose a different \
path or use a read-only alternative. \
A parse error means the arguments were malformed — check required fields and \
types in the tool schema. \
A non-zero process exit means the command itself failed — read stderr for \
details and adjust the command. \
Tool errors do not end the turn — you can try different approaches until the \
problem is solved. API errors are handled by the harness — the session state \
is preserved.",
    },
    PromptSection {
        id: "verification",
        title: "Verification",
        body: "\
Run relevant tests after making code changes. For a Rust project, this \
typically means `cargo test`, `cargo build`, and `cargo fmt --check`. \
Do not claim tests, builds, or checks passed unless you actually ran them and \
saw the result. If verification was skipped or failed, say so plainly and \
include the relevant limitation. \
Report command failures with the exit code and the specific error output, not \
a vague summary. The `/verify` command runs the project's configured checks; \
use it to validate your work before reporting completion.",
    },
    PromptSection {
        id: "context_governance",
        title: "Context governance",
        body: "\
Preserve the active goal during long tasks. When the context window fills up, \
the harness may compact earlier messages into a summary. Treat compacted \
summaries as authoritative context for earlier conversation. The most recent \
messages are always retained verbatim in the tail. \
Do not overwrite or revert user changes you did not make. Work with the \
current state of the worktree. \
Project memory and the active plan are injected at the start of each session. \
Use `/memory` to view or manage durable project facts. Use `/plan` to see the \
current task state.",
    },
    PromptSection {
        id: "safety",
        title: "Safety",
        body: "\
Do not run destructive git commands such as reset, checkout, clean, or \
force-push unless the user explicitly asks. \
Do not skip hooks (--no-verify, --no-gpg-sign) unless the user explicitly \
asks. \
When writing files, consider the blast radius — a small edit is safer than \
a full rewrite. Symlink paths and files outside the project directory are \
rejected by the harness. \
Do not guess or generate URLs unless you are confident they are valid \
references for programming tasks. \
Do not execute commands that could irreversibly modify the system outside \
the project directory.",
    },
    PromptSection {
        id: "tool_details",
        title: "Tool reference",
        body: "\
`grep` — Search file contents with a regex pattern. Respects .gitignore \
via ripgrep. Use `path` to scope to a directory or file, `include` to filter \
by glob (e.g. `*.rs`), `context` for surrounding lines. \
`glob` — Find files matching a glob pattern. Returns one path per line, \
relative to the working directory. Use `root` to scope to a subdirectory, \
`depth` to limit recursion. \
`edit` — Replace text in a file. Two modes: search/replace (replaces the first \
match of `search` with `replace`) or line range (replaces `line_start` through \
`line_end` with `new_content`). Only the targeted portion changes. \
`read_file` — Read a file as UTF-8 text. Output is truncated at 64KB. \
`write_file` — Create or overwrite a file completely. Creates parent \
directories. Use for new files or full rewrites. \
`list_files` — List entries in a single directory (non-recursive). \
`shell` — Run a command through the user's shell (`$SHELL -lc`). Default \
timeout 10s, max 120s. Output is truncated at 32KB.",
    },
    PromptSection {
        id: "reporting_style",
        title: "Reporting style",
        body: "\
Lead with the outcome, then mention the files or commands that matter. \
Reference files as `path/to/file:line` when pointing to specific code. \
Be brief — if you can say it in one sentence, do not use three. \
Skip background exposition unless it changes the user's next decision. \
At the end of each turn, state what changed and what is next in one or two \
lines. Do not add trailing summaries after you have already stated the result. \
Do not use a colon before tool calls. Do not use emojis unless asked.",
    },
];

pub const MEMORY_EXTRACTION_PROMPT: &str = "\
Extract durable project memories from this session. Output each memory as a \
single JSON line with `title`, `body`, and `type` fields. Do not output \
anything else.\n\
\n\
Memory types:\n\
- user: the user's role, preferences, knowledge, or working style\n\
- feedback: corrections the user gave about how to work (stop doing X, keep doing Y)\n\
- project: non-obvious project context — decisions, constraints, deadlines, \
  recurring issues. Use absolute dates, not relative ones.\n\
- reference: pointers to external systems (issue trackers, dashboards, Slack channels)\n\
\n\
Exclude: code patterns, architecture, file paths, git history, build commands \
(these are derivable from the repo). Exclude anything already in CLAUDE.md. \
Do not save ephemeral task details. Each memory should be 1-3 sentences, \
specific, and actionable.\n\
\n\
Output format (one JSON object per line, no other text):\n\
{\"title\":\"...\",\"body\":\"...\",\"type\":\"project\"}\n\
{\"title\":\"...\",\"body\":\"...\",\"type\":\"feedback\"}";

pub const COMPACT_SUMMARY_FORMAT: &[&str] = &[
    "Primary Request and Intent",
    "Key Technical Concepts",
    "Files and Code Sections",
    "Errors and Fixes",
    "Decisions Made",
    "Pending Tasks",
    "Current Work",
    "Next Step",
];

pub fn compact_instructions(base_instructions: &str) -> String {
    let headings = COMPACT_SUMMARY_FORMAT
        .iter()
        .map(|heading| format!("## {heading}\n..."))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "{base_instructions}\n\n## Compact task\nSummarize the model-visible conversation so future turns can continue with minimal loss. Preserve the user's original request, the latest user request verbatim or near-verbatim, explicit constraints and prohibitions, files read or changed, commands run, test and verification status, errors and fixes, decisions made, current work, and the next concrete step. Output only the summary. Use exactly these Markdown H2 headings, with the leading `##`, in this order:\n\n{headings}\n\nBe concise and factual. Do not invent test results or completed work."
    )
}

pub fn compact_repair_instructions(
    base_instructions: &str,
    validation_error: &str,
    previous_summary: &str,
) -> String {
    let headings = COMPACT_SUMMARY_FORMAT
        .iter()
        .map(|heading| format!("## {heading}\n..."))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "{base_instructions}\n\n## Compact repair task\nThe previous compact summary failed validation with `{validation_error}`. Regenerate a valid compact summary from the model-visible conversation. Output only the repaired summary. Use exactly these Markdown H2 headings, with the leading `##`, in this order:\n\n{headings}\n\nPreserve the latest user request verbatim or near-verbatim and include explicit test or verification status. Do not invent test results or completed work.\n\nPrevious failed summary:\n\n{previous_summary}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiKind, PermissionMode, SessionConfig, DEFAULT_RESPONSES_BASE_URL};
    use std::path::PathBuf;

    fn config(append_system_prompt: Option<String>) -> SessionConfig {
        SessionConfig {
            api_kind: ApiKind::Responses,
            model: "mock".into(),
            base_url: DEFAULT_RESPONSES_BASE_URL.into(),
            thinking: None,
            reasoning_effort: None,
            permission: PermissionMode::Safe,
            permission_rules: Vec::new(),
            max_steps: 3,
            context_window_tokens: crate::context::DEFAULT_CONTEXT_WINDOW_TOKENS,
            context_warning_percent: crate::config::DEFAULT_CONTEXT_WARNING_PERCENT,
            append_system_prompt,
            auto_compact: Default::default(),
            max_retries: crate::config::DEFAULT_MAX_RETRIES,
            cwd: PathBuf::from("/tmp/micos"),
        }
    }

    fn runtime(config: &SessionConfig) -> PromptRuntimeContext {
        let mut runtime = PromptRuntimeContext::from_config(config);
        runtime.current_date = "2026-05-27".into();
        runtime
    }

    #[test]
    fn prompt_sections_keep_stable_order() {
        let config = config(None);
        let runtime = runtime(&config)
            .with_project_memory(
                "/tmp/micos/.micos/memory/MEMORY.md",
                "# Repo Facts\nUse tests.",
            )
            .with_active_plan(
                "/tmp/micos/.micos/plans/active.md",
                "# Active Plan\nContinue implementation.",
            );
        let build = PromptBuilder::build(&config, &runtime);
        let prompt = build.instructions;
        let expected = [
            "## Identity",
            "## Task discipline",
            "## Tool selection",
            "## File editing",
            "## Permissions",
            "## Error recovery",
            "## Verification",
            "## Context governance",
            "## Safety",
            "## Tool reference",
            "## Reporting style",
            "## Runtime context",
            "## Project memory",
            "## Active plan",
        ];
        let positions = expected
            .iter()
            .map(|heading| prompt.find(heading).expect("heading exists"))
            .collect::<Vec<_>>();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(build.sections[0].id, "identity");
        assert_eq!(build.sections.last().unwrap().id, "active_plan");
        assert!(prompt.contains("Read relevant files before changing code"));
        assert!(prompt.contains("Do not overwrite or revert user changes"));
        assert!(prompt.contains("Do not claim tests, builds, or checks passed"));
        assert!(prompt.contains("Do not run destructive git commands"));
        assert!(prompt.contains("For long tasks, keep the current goal"));
        assert!(prompt.contains("cwd: /tmp/micos"));
        assert!(prompt.contains("current date: 2026-05-27"));
        assert!(prompt.contains("# Repo Facts"));
        assert!(prompt.contains("Continue implementation."));
    }

    #[test]
    fn append_system_prompt_adds_to_base_prompt() {
        let config = config(Some("Prefer short answers.".into()));
        let runtime =
            runtime(&config).with_project_memory("/tmp/micos/.micos/memory/MEMORY.md", "# Facts");
        let build = PromptBuilder::build(&config, &runtime);
        let prompt = build.instructions;
        assert!(prompt.contains("## Identity"));
        assert!(prompt.contains("## Project memory"));
        assert!(prompt.contains("## Project additional instructions"));
        assert!(prompt.contains("Prefer short answers."));
        assert!(
            prompt.find("## Project memory").unwrap()
                < prompt.find("Prefer short answers.").unwrap()
        );
        assert_eq!(build.sections.last().unwrap().id, "project_append");
        assert_eq!(
            build.sections.last().unwrap().source,
            "config.append_system_prompt"
        );
    }

    #[test]
    fn prompt_sections_have_snapshots_without_body() {
        let config = config(Some("Prefer short answers.".into()));
        let build = PromptBuilder::build(&config, &runtime(&config));
        let snapshots = build
            .sections
            .iter()
            .map(PromptSectionView::snapshot)
            .collect::<Vec<_>>();

        assert!(snapshots.iter().all(|section| section.tokens_estimate > 0));
        assert_eq!(snapshots[0].id, "identity");
        assert_eq!(snapshots[0].source, "base");
    }

    #[test]
    fn compact_prompt_uses_fixed_summary_format() {
        let prompt = compact_instructions("base");
        for heading in COMPACT_SUMMARY_FORMAT {
            assert!(prompt.contains(&format!("## {heading}")));
        }
        assert!(prompt.contains("Do not invent test results"));
        assert!(prompt.contains("explicit constraints and prohibitions"));
        assert!(prompt.contains("test and verification status"));
        assert!(prompt.contains("with the leading `##`"));
    }

    #[test]
    fn compact_repair_prompt_includes_error_and_previous_summary() {
        let prompt = compact_repair_instructions("base", "missing_heading:Next Step", "bad");
        assert!(prompt.contains("Compact repair task"));
        assert!(prompt.contains("missing_heading:Next Step"));
        assert!(prompt.contains("Previous failed summary"));
        assert!(prompt.contains("bad"));
        for heading in COMPACT_SUMMARY_FORMAT {
            assert!(prompt.contains(&format!("## {heading}")));
        }
    }
}
