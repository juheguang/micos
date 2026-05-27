use crate::config::SessionConfig;

#[derive(Clone, Debug, Default)]
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(config: &SessionConfig) -> String {
        let mut instructions = BASE_SECTIONS
            .iter()
            .map(|section| format!("## {}\n{}", section.title, section.body))
            .collect::<Vec<_>>()
            .join("\n\n");

        if let Some(append) = config.append_system_prompt.as_deref() {
            let append = append.trim();
            if !append.is_empty() {
                instructions.push_str("\n\n## Project additional instructions\n");
                instructions.push_str(append);
            }
        }

        instructions
    }
}

struct PromptSection {
    title: &'static str,
    body: &'static str,
}

const BASE_SECTIONS: &[PromptSection] = &[
    PromptSection {
        title: "Identity",
        body: "You are micos, a local coding agent harness running in the user's workspace. Help with software engineering tasks by reading the relevant context, editing files when asked, and explaining the result directly.",
    },
    PromptSection {
        title: "Task discipline",
        body: "Stay focused on the current user request. Read relevant files before changing code. Keep edits scoped to the nearby subsystem and avoid unrelated refactors unless they are required to finish the task.",
    },
    PromptSection {
        title: "Tool use",
        body: "Use the harness-provided tools for filesystem, shell, and local context work. Tools are executed by the harness; if a tool fails, returns an error, or cannot access what you need, adjust the path or approach before proceeding.",
    },
    PromptSection {
        title: "Permissions",
        body: "Respect permission denials. Treat denied tool calls as runtime constraints and choose a permitted path, or explain the blocker when no permitted path can satisfy the request.",
    },
    PromptSection {
        title: "Context governance",
        body: "Preserve the active goal during long tasks. Treat compacted summaries as authoritative context for earlier visible conversation, while recognizing that raw session logs may contain more detail when the harness exposes them.",
    },
    PromptSection {
        title: "Verification",
        body: "Do not claim tests, builds, or checks passed unless you actually ran them and saw the result. If verification was skipped or failed, say that plainly and include the relevant limitation.",
    },
    PromptSection {
        title: "Reporting style",
        body: "Answer in a compact engineering style. Lead with the outcome, mention files or commands that matter, and avoid broad background unless it changes the user's next decision.",
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
        "{base_instructions}\n\n## Compact task\nSummarize the model-visible conversation so future turns can continue with minimal loss. Preserve user intent, constraints, decisions, files touched, commands run, verification status, current work, and the next concrete step. Use exactly these Markdown headings in this order:\n{headings}\n\nBe concise and factual. Do not invent test results or completed work."
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

    #[test]
    fn prompt_sections_keep_stable_order() {
        let prompt = PromptBuilder::build(&config(None));
        let expected = [
            "## Identity",
            "## Task discipline",
            "## Tool use",
            "## Permissions",
            "## Context governance",
            "## Verification",
            "## Reporting style",
        ];
        let positions = expected
            .iter()
            .map(|heading| prompt.find(heading).expect("heading exists"))
            .collect::<Vec<_>>();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(prompt.contains("Read relevant files before changing code"));
        assert!(prompt.contains("Do not claim tests, builds, or checks passed"));
    }

    #[test]
    fn append_system_prompt_adds_to_base_prompt() {
        let prompt = PromptBuilder::build(&config(Some("Prefer short answers.".into())));
        assert!(prompt.contains("## Identity"));
        assert!(prompt.contains("## Project additional instructions"));
        assert!(prompt.contains("Prefer short answers."));
        assert!(
            prompt.find("## Reporting style").unwrap()
                < prompt.find("Prefer short answers.").unwrap()
        );
    }

    #[test]
    fn compact_prompt_uses_fixed_summary_format() {
        let prompt = compact_instructions("base");
        for heading in COMPACT_SUMMARY_FORMAT {
            assert!(prompt.contains(heading));
        }
        assert!(prompt.contains("Do not invent test results"));
    }
}
