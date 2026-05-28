use super::records::{candidate_from_draft, deduplicate_candidate, sanitize_id, write_toml};
use super::{MemoryCandidate, MemoryEntry, MemoryStatus, MemoryType};
use crate::plan::HandoffDraft;
use crate::prompt::MEMORY_EXTRACTION_PROMPT;
use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractedMemory {
    pub title: String,
    pub body: String,
    pub memory_type: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryExtractionReport {
    pub candidates: Vec<MemoryCandidate>,
    pub skipped: Vec<String>,
}

#[derive(Deserialize)]
struct MemoryLine {
    title: String,
    body: String,
    #[serde(rename = "type")]
    memory_type: Option<String>,
}

pub fn parse_extraction_output(text: &str) -> Vec<ExtractedMemory> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            serde_json::from_str::<MemoryLine>(line)
                .ok()
                .map(|m| ExtractedMemory {
                    title: m.title.trim().to_string(),
                    body: m.body.trim().to_string(),
                    memory_type: m.memory_type.unwrap_or_else(|| "project".to_string()),
                })
        })
        .filter(|m| !m.title.is_empty() && !m.body.is_empty())
        .collect()
}

pub fn extraction_request(draft: &HandoffDraft, existing_entries: &[&MemoryEntry]) -> String {
    let files = if draft.files_touched.is_empty() {
        "none".to_string()
    } else {
        draft.files_touched.join(", ")
    };
    let commands = if draft.commands_run.is_empty() {
        "none".to_string()
    } else {
        draft.commands_run.join(", ")
    };
    let failures = if draft.known_failures.is_empty() {
        "none".to_string()
    } else {
        draft.known_failures.join(", ")
    };

    let mut parts = vec![MEMORY_EXTRACTION_PROMPT.to_string()];
    parts.push("\n## Session summary\n".to_string());
    parts.push(format!("Current state: {}", draft.current_state));
    parts.push(format!("Next step: {}", draft.next_step));
    parts.push(format!("Files touched: {}", files));
    parts.push(format!("Commands run: {}", commands));
    parts.push(format!("Verification: {}", draft.verification_status));
    parts.push(format!("Known failures: {}", failures));

    if !existing_entries.is_empty() {
        parts.push("\n## Existing memories (avoid duplicates)\n".to_string());
        for entry in existing_entries {
            parts.push(format!("- [{}] {}: {}", entry.id, entry.title, entry.body));
        }
    }

    parts.join("\n")
}

pub fn build_candidates(
    extracted: Vec<ExtractedMemory>,
    source_session: Option<String>,
    timestamp: &str,
    existing_entries: &[&MemoryEntry],
    candidates_dir: &Path,
    fallback: &HandoffDraft,
) -> Result<MemoryExtractionReport> {
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();

    for (i, mem) in extracted.into_iter().enumerate() {
        let dup = deduplicate_candidate(&mem.title, &mem.body, existing_entries);
        if dup == "duplicate" {
            skipped.push(format!("duplicate: {}", mem.title));
            continue;
        }
        if dup == "similar" {
            skipped.push(format!("similar to existing: {}", mem.title));
        }

        let memory_type = match mem.memory_type.as_str() {
            "user" => Some(MemoryType::User),
            "feedback" => Some(MemoryType::Feedback),
            "project" => Some(MemoryType::Project),
            "reference" => Some(MemoryType::Reference),
            _ => Some(MemoryType::Project),
        };

        let id = source_session
            .as_deref()
            .map(|s| format!("session-{}-{}", sanitize_id(s), i))
            .unwrap_or_else(|| format!("extracted-{}", sanitize_id(timestamp)));
        let candidate = MemoryCandidate {
            id,
            title: mem.title,
            body: mem.body,
            source_session: source_session.clone(),
            created_at: timestamp.to_string(),
            scope: "project".to_string(),
            status: MemoryStatus::Candidate,
            memory_type,
        };
        write_toml(
            &candidates_dir.join(format!("{}.toml", sanitize_id(&candidate.id))),
            &candidate,
        )?;
        candidates.push(candidate);
    }

    if candidates.is_empty() && skipped.iter().all(|s| s.starts_with("duplicate")) {
        // all extracted memories are duplicates — fall back to deterministic draft
        let fallback_candidate =
            candidate_from_draft(fallback, source_session, timestamp.to_string());
        write_toml(
            &candidates_dir.join(format!("{}.toml", sanitize_id(&fallback_candidate.id))),
            &fallback_candidate,
        )?;
        candidates.push(fallback_candidate);
    }

    Ok(MemoryExtractionReport {
        candidates,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_json_lines() {
        let text = "{\"title\":\"Use cargo test\",\"body\":\"Always run tests before committing.\",\"type\":\"feedback\"}\n{\"title\":\"Project deadline\",\"body\":\"Must ship by 2026-06-15.\",\"type\":\"project\"}";
        let memories = parse_extraction_output(text);
        assert_eq!(memories.len(), 2);
        assert_eq!(memories[0].title, "Use cargo test");
        assert_eq!(memories[0].memory_type, "feedback");
        assert_eq!(memories[1].title, "Project deadline");
        assert_eq!(memories[1].memory_type, "project");
    }

    #[test]
    fn skips_empty_lines_and_invalid_json() {
        let text = "{\"title\":\"Valid\",\"body\":\"ok\",\"type\":\"project\"}\n\nnot json\n{\"title\":\"Also valid\",\"body\":\"ok\",\"type\":\"user\"}";
        let memories = parse_extraction_output(text);
        assert_eq!(memories.len(), 2);
    }

    #[test]
    fn skips_empty_title_or_body() {
        let text = "{\"title\":\"\",\"body\":\"no title\",\"type\":\"project\"}\n{\"title\":\"No body\",\"body\":\"\",\"type\":\"project\"}";
        let memories = parse_extraction_output(text);
        assert_eq!(memories.len(), 0);
    }
}
