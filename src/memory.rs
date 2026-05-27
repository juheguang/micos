use crate::context::estimate_text_tokens;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

pub const MEMORY_DIR: &str = ".micos/memory";
pub const MEMORY_INDEX_FILE: &str = "MEMORY.md";
pub const MEMORY_TOPICS_DIR: &str = "topics";

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
        std::fs::create_dir_all(&topics_dir)
            .with_context(|| format!("create memory topics directory {}", topics_dir.display()))?;

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

        Ok(Self {
            root,
            index_path,
            index_text,
            index_tokens,
            topics,
            created_index,
        })
    }

    pub fn active_index_text(&self) -> Option<&str> {
        active_index_text(&self.index_text)
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
        assert_eq!(memory.active_index_text(), None);
        assert_eq!(memory.index_tokens, 0);
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
    fn read_topic_rejects_path_traversal() {
        let cwd = temp_cwd();
        let memory = ProjectMemory::load_or_init(&cwd).unwrap();

        assert!(memory.read_topic("../config.toml").is_err());
        assert!(memory.read_topic("nested/topic.md").is_err());
        assert!(memory.read_topic("topic.txt").is_err());
    }
}
