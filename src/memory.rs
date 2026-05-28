use crate::context::estimate_text_tokens;
use crate::plan::HandoffDraft;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

mod extract;
mod records;
use records::{
    candidate_from_draft, deduplicate_candidate, entry_from_candidate, render_active_entries,
    sanitize_id, scan_toml_dir, write_toml,
};
pub use extract::{
    build_candidates, extraction_request, parse_extraction_output, ExtractedMemory,
    MemoryExtractionReport,
};
pub use records::{
    MemoryCandidate, MemoryCandidateReport, MemoryEntry, MemoryStatus, MemoryType,
};

pub const MEMORY_DIR: &str = ".micos/memory";
pub const MEMORY_INDEX_FILE: &str = "MEMORY.md";
pub const MEMORY_TOPICS_DIR: &str = "topics";
pub const MEMORY_CANDIDATES_DIR: &str = "candidates";
pub const MEMORY_ENTRIES_DIR: &str = "entries";

const MEMORY_INDEX_TEMPLATE: &str = r#"# Project Memory

<!-- Add stable project facts, conventions, recurring failures, and runbooks here. -->
"#;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectMemory {
    pub root: PathBuf,
    pub index_path: PathBuf,
    pub index_text: String,
    pub index_tokens: usize,
    pub topics: Vec<MemoryTopic>,
    pub candidates: Vec<MemoryCandidate>,
    pub entries: Vec<MemoryEntry>,
    pub active_entries_tokens: usize,
    pub created_index: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryTopic {
    pub file_name: String,
    pub path: PathBuf,
    pub title: String,
    pub bytes: u64,
}

impl ProjectMemory {
    pub fn load_or_init(cwd: &Path) -> Result<Self> {
        let root = cwd.join(MEMORY_DIR);
        let topics_dir = root.join(MEMORY_TOPICS_DIR);
        let candidates_dir = root.join(MEMORY_CANDIDATES_DIR);
        let entries_dir = root.join(MEMORY_ENTRIES_DIR);
        std::fs::create_dir_all(&topics_dir)
            .with_context(|| format!("create memory topics directory {}", topics_dir.display()))?;
        std::fs::create_dir_all(&candidates_dir).with_context(|| {
            format!(
                "create memory candidates directory {}",
                candidates_dir.display()
            )
        })?;
        std::fs::create_dir_all(&entries_dir).with_context(|| {
            format!("create memory entries directory {}", entries_dir.display())
        })?;

        let index_path = root.join(MEMORY_INDEX_FILE);
        let created_index = if index_path.exists() {
            false
        } else {
            std::fs::write(&index_path, MEMORY_INDEX_TEMPLATE)
                .with_context(|| format!("write memory index {}", index_path.display()))?;
            true
        };

        let index_text = std::fs::read_to_string(&index_path)
            .with_context(|| format!("read memory index {}", index_path.display()))?;
        let topics = scan_topics(&topics_dir)?;
        let index_tokens = estimate_text_tokens(active_index_text(&index_text).unwrap_or(""));
        let candidates = scan_toml_dir::<MemoryCandidate>(&candidates_dir)?;
        let entries = scan_toml_dir::<MemoryEntry>(&entries_dir)?;
        let active_entries_tokens = estimate_text_tokens(&render_active_entries(&entries));

        Ok(Self {
            root,
            index_path,
            index_text,
            index_tokens,
            topics,
            candidates,
            entries,
            active_entries_tokens,
            created_index,
        })
    }

    pub fn active_index_text(&self) -> Option<&str> {
        active_index_text(&self.index_text)
    }

    pub fn active_entries_text(&self) -> Option<String> {
        let text = render_active_entries(&self.entries);
        (!text.trim().is_empty()).then_some(text)
    }

    pub fn active_entries_index(&self) -> Option<String> {
        let lines: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.status == MemoryStatus::Active)
            .map(|e| {
                let type_tag = e
                    .memory_type
                    .as_ref()
                    .map(|t| format!("[{t}] "))
                    .unwrap_or_default();
                format!(
                    "- {}{} — {}",
                    type_tag,
                    e.title.trim(),
                    trim_first_line(&e.body, 120)
                )
            })
            .collect();
        (!lines.is_empty()).then(|| {
            format!(
                "## Accepted durable memory (use `/memory <id>` for details)\n{}",
                lines.join("\n")
            )
        })
    }

    pub fn pending_candidates(&self) -> Vec<&MemoryCandidate> {
        self.candidates
            .iter()
            .filter(|candidate| candidate.status == MemoryStatus::Candidate)
            .collect()
    }

    pub fn active_entries(&self) -> Vec<&MemoryEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.status == MemoryStatus::Active)
            .collect()
    }

    pub fn read_topic(&self, file_name: &str) -> Result<String> {
        let file_name = file_name.trim();
        if file_name.is_empty() || file_name.contains('/') || file_name.contains('\\') {
            bail!("memory topic must be a file name under topics/");
        }
        if file_name == "." || file_name == ".." || file_name.contains("..") {
            bail!("memory topic must not contain path traversal");
        }
        let path = self.root.join(MEMORY_TOPICS_DIR).join(file_name);
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            bail!("memory topic must be a markdown file");
        }
        std::fs::read_to_string(&path)
            .with_context(|| format!("read memory topic {}", path.display()))
    }

    pub fn refresh_candidates_from_session(
        &mut self,
        session_path: &Path,
        timestamp: String,
    ) -> Result<MemoryCandidateReport> {
        let draft =
            HandoffDraft::from_session_log(session_path, "memory_refresh", timestamp.clone())
                .with_context(|| {
                    format!("build memory candidate from {}", session_path.display())
                })?;
        let source_session = session_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(ToOwned::to_owned);
        let candidate = candidate_from_draft(&draft, source_session, timestamp);
        let path = self.candidate_path(&candidate.id);
        let created = !path.exists();
        write_toml(&path, &candidate)?;
        self.reload_records()?;
        Ok(MemoryCandidateReport { candidate, created })
    }

    pub fn promote_candidate(&mut self, id: &str, timestamp: String) -> Result<MemoryEntry> {
        let candidate = self
            .candidates
            .iter()
            .find(|candidate| candidate.id == id && candidate.status == MemoryStatus::Candidate)
            .cloned()
            .with_context(|| format!("memory candidate not found: {id}"))?;

        // dedup check against existing active entries
        let active: Vec<&MemoryEntry> = self
            .entries
            .iter()
            .filter(|e| e.status == MemoryStatus::Active)
            .collect();
        let dup_status =
            deduplicate_candidate(&candidate.title, &candidate.body, &active);
        if dup_status == "duplicate" {
            bail!("duplicate of existing entry — use /memory promote only for new facts");
        }

        let prev_forgotten = self
            .entries
            .iter()
            .any(|e| e.id == candidate.id && e.status == MemoryStatus::Forgotten);
        let is_new_entry = !self.entries.iter().any(|e| e.id == candidate.id);

        let entry = entry_from_candidate(candidate.clone(), timestamp);
        write_toml(&self.entry_path(&entry.id), &entry)?;
        let _ = std::fs::remove_file(self.candidate_path(&candidate.id));

        // update MEMORY.md index
        if is_new_entry && !prev_forgotten {
            self.append_to_index(&entry)?;
        }
        if prev_forgotten {
            self.remove_from_index(&entry.id)?;
        }

        self.reload_records()?;
        Ok(entry)
    }

    pub fn mark_entry_status(&mut self, id: &str, status: MemoryStatus) -> Result<MemoryEntry> {
        if !matches!(status, MemoryStatus::Stale | MemoryStatus::Forgotten) {
            bail!("memory entry status can only be stale or forgotten");
        }
        let mut entry = self
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .with_context(|| format!("memory entry not found: {id}"))?;
        entry.status = status;
        write_toml(&self.entry_path(&entry.id), &entry)?;
        if status == MemoryStatus::Forgotten {
            self.remove_from_index(&entry.id)?;
        }
        self.reload_records()?;
        Ok(entry)
    }

    pub fn forget_candidate(&mut self, id: &str) -> Result<MemoryCandidate> {
        let mut candidate = self
            .candidates
            .iter()
            .find(|candidate| candidate.id == id)
            .cloned()
            .with_context(|| format!("memory candidate not found: {id}"))?;
        candidate.status = MemoryStatus::Forgotten;
        write_toml(&self.candidate_path(&candidate.id), &candidate)?;
        self.reload_records()?;
        Ok(candidate)
    }

    pub fn sweep(&mut self, stale_days: u64) -> Vec<MemoryEntry> {
        let now = time::OffsetDateTime::now_utc();
        let now_date = now.date();
        let mut to_stale: Vec<(String, MemoryEntry)> = Vec::new();
        for entry in &self.entries {
            if entry.status != MemoryStatus::Active {
                continue;
            }
            if let Some(ref validated) = entry.last_validated_at {
                let date_str = validated.split('T').next().unwrap_or(validated);
                if let Ok((year, month, day)) = parse_date(date_str) {
                    if let Ok(entry_date) = time::Date::from_calendar_date(year, month, day) {
                        let age = now_date - entry_date;
                        if age.whole_days() as u64 >= stale_days {
                            let mut e = entry.clone();
                            e.status = MemoryStatus::Stale;
                            to_stale.push((self.entry_path(&e.id).display().to_string(), e));
                        }
                    }
                }
            }
        }
        let mut stale_entries = Vec::new();
        for (path, entry) in to_stale {
            write_toml(&std::path::PathBuf::from(&path), &entry).ok();
            stale_entries.push(entry);
        }
        self.reload_records().ok();
        stale_entries
    }

    pub fn reload_records(&mut self) -> Result<()> {
        self.candidates = scan_toml_dir::<MemoryCandidate>(&self.root.join(MEMORY_CANDIDATES_DIR))?;
        self.entries = scan_toml_dir::<MemoryEntry>(&self.root.join(MEMORY_ENTRIES_DIR))?;
        self.active_entries_tokens = estimate_text_tokens(&render_active_entries(&self.entries));
        Ok(())
    }

    fn append_to_index(&self, entry: &MemoryEntry) -> Result<()> {
        let line = format!(
            "- [{}]({}.md) — {}",
            entry.title.trim(),
            sanitize_id(&entry.id),
            trim_first_line(&entry.body, 120)
        );
        let mut index = std::fs::read_to_string(&self.index_path)
            .unwrap_or_default();
        if index.contains(&entry.id) {
            return Ok(());
        }
        let lines: Vec<&str> = index.lines().collect();
        if lines.len() >= 190 {
            index = lines
                .iter()
                .take(180)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            index.push_str("\n<!-- auto-trimmed -->\n");
        }
        if !index.ends_with('\n') {
            index.push('\n');
        }
        index.push_str(&line);
        index.push('\n');
        std::fs::write(&self.index_path, &index)
            .with_context(|| format!("write memory index {}", self.index_path.display()))?;
        Ok(())
    }

    fn remove_from_index(&self, id: &str) -> Result<()> {
        let index = std::fs::read_to_string(&self.index_path)
            .unwrap_or_default();
        let filtered: Vec<&str> = index
            .lines()
            .filter(|line| !line.contains(id))
            .collect();
        std::fs::write(&self.index_path, filtered.join("\n"))
            .with_context(|| format!("write memory index {}", self.index_path.display()))?;
        Ok(())
    }

    fn candidate_path(&self, id: &str) -> PathBuf {
        self.root
            .join(MEMORY_CANDIDATES_DIR)
            .join(format!("{}.toml", sanitize_id(id)))
    }

    fn entry_path(&self, id: &str) -> PathBuf {
        self.root
            .join(MEMORY_ENTRIES_DIR)
            .join(format!("{}.toml", sanitize_id(id)))
    }
}

fn scan_topics(topics_dir: &Path) -> Result<Vec<MemoryTopic>> {
    let mut topics = std::fs::read_dir(topics_dir)
        .with_context(|| format!("read memory topics directory {}", topics_dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let file_name = path.file_name()?.to_str()?.to_string();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            Some(MemoryTopic {
                file_name,
                title: first_heading(&text).unwrap_or_else(|| "Untitled".into()),
                path,
                bytes: metadata.len(),
            })
        })
        .collect::<Vec<_>>();
    topics.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(topics)
}

fn first_heading(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
}

fn active_index_text(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == MEMORY_INDEX_TEMPLATE.trim() {
        return None;
    }
    Some(trimmed)
}

fn parse_date(date_str: &str) -> Result<(i32, time::Month, u8)> {
    let parts: Vec<&str> = date_str.split('-').collect();
    if parts.len() != 3 {
        bail!("invalid date format: {date_str}");
    }
    let year: i32 = parts[0].parse().context("parse year")?;
    let month_num: u8 = parts[1].parse().context("parse month")?;
    let day: u8 = parts[2].parse().context("parse day")?;
    let month = time::Month::try_from(month_num).map_err(|_| anyhow::anyhow!("invalid month: {month_num}"))?;
    Ok((year, month, day))
}

fn trim_first_line(text: &str, max_chars: usize) -> String {
    let line = text.lines().next().unwrap_or(text).trim();
    let compact: String = line
        .chars()
        .take(max_chars)
        .collect();
    if line.chars().count() > max_chars {
        format!("{compact}...")
    } else {
        compact
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_cwd() -> PathBuf {
        std::env::temp_dir().join(format!("micos-memory-{}", Uuid::new_v4()))
    }

    #[test]
    fn missing_memory_directory_creates_template_and_topics() {
        let cwd = temp_cwd();
        let memory = ProjectMemory::load_or_init(&cwd).unwrap();

        assert!(memory.created_index);
        assert!(memory.index_path.exists());
        assert!(memory.root.join(MEMORY_TOPICS_DIR).is_dir());
        assert!(memory.root.join(MEMORY_CANDIDATES_DIR).is_dir());
        assert!(memory.root.join(MEMORY_ENTRIES_DIR).is_dir());
        assert_eq!(memory.active_index_text(), None);
        assert_eq!(memory.index_tokens, 0);
        assert!(memory.candidates.is_empty());
        assert!(memory.entries.is_empty());
    }

    #[test]
    fn loads_active_index_and_topics() {
        let cwd = temp_cwd();
        let root = cwd.join(MEMORY_DIR);
        let topics = root.join(MEMORY_TOPICS_DIR);
        std::fs::create_dir_all(&topics).unwrap();
        std::fs::write(root.join(MEMORY_INDEX_FILE), "# Facts\nUse cargo test.").unwrap();
        std::fs::write(topics.join("build.md"), "# Build\nRun cargo test.").unwrap();
        std::fs::write(topics.join("ignored.txt"), "# Ignored").unwrap();

        let memory = ProjectMemory::load_or_init(&cwd).unwrap();

        assert!(!memory.created_index);
        assert_eq!(memory.active_index_text(), Some("# Facts\nUse cargo test."));
        assert!(memory.index_tokens > 0);
        assert_eq!(memory.topics.len(), 1);
        assert_eq!(memory.topics[0].file_name, "build.md");
        assert_eq!(memory.topics[0].title, "Build");
    }

    #[test]
    fn promotes_stales_and_forgets_memory_entries() {
        let cwd = temp_cwd();
        let mut memory = ProjectMemory::load_or_init(&cwd).unwrap();
        let session_dir = cwd.join(crate::session::SESSION_DIR);
        std::fs::create_dir_all(&session_dir).unwrap();
        let session_path = session_dir.join("abc.jsonl");
        std::fs::write(
            &session_path,
            r#"{"type":"user_input","text":"Implement memory lifecycle."}
{"type":"assistant_text","text":"Changed src/memory.rs and ran cargo test."}
{"type":"verification_finished","name":"test","command":"cargo test","success":true,"exit_code":0,"elapsed_ms":1,"output_preview":"ok","truncated":false}
"#,
        )
        .unwrap();

        let report = memory
            .refresh_candidates_from_session(&session_path, "2026-05-27T00:00:00Z".into())
            .unwrap();

        assert!(report.created);
        assert_eq!(memory.pending_candidates().len(), 1);
        let entry = memory
            .promote_candidate(&report.candidate.id, "2026-05-27T00:01:00Z".into())
            .unwrap();
        assert_eq!(entry.status, MemoryStatus::Active);
        assert!(memory.active_entries_text().unwrap().contains("cargo test"));
        assert!(memory.pending_candidates().is_empty());

        let entry = memory
            .mark_entry_status(&entry.id, MemoryStatus::Stale)
            .unwrap();
        assert_eq!(entry.status, MemoryStatus::Stale);
        assert_eq!(memory.active_entries_text(), None);
        let entry = memory
            .mark_entry_status(&entry.id, MemoryStatus::Forgotten)
            .unwrap();
        assert_eq!(entry.status, MemoryStatus::Forgotten);
    }

    #[test]
    fn read_topic_rejects_path_traversal() {
        let cwd = temp_cwd();
        let memory = ProjectMemory::load_or_init(&cwd).unwrap();

        assert!(memory.read_topic("../config.toml").is_err());
        assert!(memory.read_topic("nested/topic.md").is_err());
        assert!(memory.read_topic("topic.txt").is_err());
    }
}
