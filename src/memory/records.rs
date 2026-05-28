use crate::plan::HandoffDraft;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryCandidate {
    pub id: String,
    pub title: String,
    pub body: String,
    pub source_session: Option<String>,
    pub created_at: String,
    pub scope: String,
    pub status: MemoryStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryEntry {
    pub id: String,
    pub title: String,
    pub body: String,
    pub source_session: Option<String>,
    pub created_at: String,
    pub last_validated_at: Option<String>,
    pub scope: String,
    pub status: MemoryStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Candidate,
    Active,
    Stale,
    Forgotten,
}

impl std::fmt::Display for MemoryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryStatus::Candidate => write!(f, "candidate"),
            MemoryStatus::Active => write!(f, "active"),
            MemoryStatus::Stale => write!(f, "stale"),
            MemoryStatus::Forgotten => write!(f, "forgotten"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryCandidateReport {
    pub candidate: MemoryCandidate,
    pub created: bool,
}

pub(super) fn candidate_from_draft(
    draft: &HandoffDraft,
    source_session: Option<String>,
    timestamp: String,
) -> MemoryCandidate {
    let id = source_session
        .as_deref()
        .map(|session| format!("session-{}", sanitize_id(session)))
        .unwrap_or_else(|| format!("session-{}", sanitize_id(&timestamp)));
    MemoryCandidate {
        id,
        title: draft_title(draft),
        body: draft_body(draft),
        source_session,
        created_at: timestamp,
        scope: "project".into(),
        status: MemoryStatus::Candidate,
    }
}

pub(super) fn entry_from_candidate(candidate: MemoryCandidate, timestamp: String) -> MemoryEntry {
    MemoryEntry {
        id: candidate.id,
        title: candidate.title,
        body: candidate.body,
        source_session: candidate.source_session,
        created_at: candidate.created_at,
        last_validated_at: Some(timestamp),
        scope: candidate.scope,
        status: MemoryStatus::Active,
    }
}

pub(super) fn scan_toml_dir<T>(dir: &Path) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let mut paths = std::fs::read_dir(dir)
        .with_context(|| format!("read memory record directory {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                return None;
            }
            Some(path)
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read memory record {}", path.display()))?;
            toml::from_str::<T>(&text)
                .with_context(|| format!("parse memory record {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()
}

pub(super) fn write_toml<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    let text = toml::to_string_pretty(value).context("serialize memory record")?;
    std::fs::write(path, text).with_context(|| format!("write memory record {}", path.display()))
}

pub(super) fn render_active_entries(entries: &[MemoryEntry]) -> String {
    entries
        .iter()
        .filter(|entry| entry.status == MemoryStatus::Active)
        .map(|entry| {
            format!(
                "### {} ({})\n{}\nsource_session: {}\nlast_validated_at: {}",
                entry.title.trim(),
                entry.id,
                entry.body.trim(),
                entry.source_session.as_deref().unwrap_or("unknown"),
                entry.last_validated_at.as_deref().unwrap_or("unknown")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(super) fn sanitize_id(text: &str) -> String {
    let id = text
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if id.is_empty() {
        "memory".into()
    } else {
        id
    }
}

fn draft_title(draft: &HandoffDraft) -> String {
    let mut title = draft
        .next_step
        .strip_prefix("Continue from latest user request:")
        .unwrap_or(&draft.next_step)
        .trim()
        .to_string();
    if title.is_empty() {
        title = "Session handoff".into();
    }
    trim_one_line(&title, 80)
}

fn draft_body(draft: &HandoffDraft) -> String {
    [
        format!("Current state:\n{}", draft.current_state.trim()),
        format!("Next step:\n{}", draft.next_step.trim()),
        format!("Verification status: {}", draft.verification_status.trim()),
        format!("Files touched: {}", join_or_none(&draft.files_touched)),
        format!("Commands run: {}", join_or_none(&draft.commands_run)),
        format!("Known failures: {}", join_or_none(&draft.known_failures)),
    ]
    .join("\n\n")
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "None".into()
    } else {
        items.join("; ")
    }
}

fn trim_one_line(text: &str, max_chars: usize) -> String {
    let mut compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > max_chars {
        compact = compact.chars().take(max_chars).collect::<String>();
        compact.push_str("...");
    }
    compact
}
