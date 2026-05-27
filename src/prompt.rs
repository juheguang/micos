use crate::config::{ApiKind, PermissionMode, SessionConfig};
use crate::context::estimate_text_tokens;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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
        }
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
        body: "You are micos, a local coding agent harness running in the user's workspace. Help with software engineering tasks by reading the relevant context, editing files when asked, and explaining the result directly.",
    },
    PromptSection {
        id: "task_discipline",
        title: "Task discipline",
        body: "Stay focused on the current user request. Read relevant files before changing code. Keep edits scoped to the nearby subsystem and avoid unrelated refactors unless they are required to finish the task. For long tasks, keep the current goal and next step clear.",
    },
    PromptSection {
        id: "tool_use",
        title: "Tool use",
        body: "Use the harness-provided tools for filesystem, shell, and local context work. Tools are executed by the harness; if a tool fails, returns an error, or cannot access what you need, adjust the path or approach before proceeding.",
    },
    PromptSection {
        id: "permissions",
        title: "Permissions",
        body: "Respect permission denials. Treat denied tool calls as runtime constraints and choose a permitted path, or explain the blocker when no permitted path can satisfy the request.",
    },
    PromptSection {
        id: "context_governance",
        title: "Context governance",
        body: "Preserve the active goal during long tasks. Treat compacted summaries as authoritative context for earlier visible conversation, while recognizing that raw session logs may contain more detail when the harness exposes them. Do not overwrite or revert user changes you did not make; work with the current worktree state.",
    },
    PromptSection {
        id: "verification",
        title: "Verification",
        body: "Do not claim tests, builds, or checks passed unless you actually ran them and saw the result. If verification was skipped or failed, say that plainly and include the relevant limitation. Report command failures with enough detail for the user to act.",
    },
    PromptSection {
        id: "reporting_style",
        title: "Reporting style",
        body: "Answer in a compact engineering style. Lead with the outcome, mention files or commands that matter, and avoid broad background unless it changes the user's next decision. Do not run destructive git commands such as reset or checkout unless the user explicitly asks.",
    },
];

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
        .map(|heading| format!("- {heading}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{base_instructions}\n\n## Compact task\nSummarize the model-visible conversation so future turns can continue with minimal loss. Preserve the user's original request, the latest user request verbatim or near-verbatim, explicit constraints and prohibitions, files read or changed, commands run, test and verification status, errors and fixes, decisions made, current work, and the next concrete step. Use exactly these Markdown headings in this order:\n{headings}\n\nBe concise and factual. Do not invent test results or completed work."
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
            append_system_prompt,
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
        let build = PromptBuilder::build(&config, &runtime(&config));
        let prompt = build.instructions;
        let expected = [
            "## Identity",
            "## Task discipline",
            "## Tool use",
            "## Permissions",
            "## Context governance",
            "## Verification",
            "## Reporting style",
            "## Runtime context",
        ];
        let positions = expected
            .iter()
            .map(|heading| prompt.find(heading).expect("heading exists"))
            .collect::<Vec<_>>();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(build.sections[0].id, "identity");
        assert_eq!(build.sections.last().unwrap().id, "runtime");
        assert!(prompt.contains("Read relevant files before changing code"));
        assert!(prompt.contains("Do not overwrite or revert user changes"));
        assert!(prompt.contains("Do not claim tests, builds, or checks passed"));
        assert!(prompt.contains("Do not run destructive git commands"));
        assert!(prompt.contains("For long tasks, keep the current goal"));
        assert!(prompt.contains("cwd: /tmp/micos"));
        assert!(prompt.contains("current date: 2026-05-27"));
    }

    #[test]
    fn append_system_prompt_adds_to_base_prompt() {
        let config = config(Some("Prefer short answers.".into()));
        let build = PromptBuilder::build(&config, &runtime(&config));
        let prompt = build.instructions;
        assert!(prompt.contains("## Identity"));
        assert!(prompt.contains("## Project additional instructions"));
        assert!(prompt.contains("Prefer short answers."));
        assert!(
            prompt.find("## Runtime context").unwrap()
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
            assert!(prompt.contains(heading));
        }
        assert!(prompt.contains("Do not invent test results"));
        assert!(prompt.contains("explicit constraints and prohibitions"));
        assert!(prompt.contains("test and verification status"));
    }
}
