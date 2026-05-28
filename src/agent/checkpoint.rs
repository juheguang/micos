use crate::session::{now, SessionEvent, SessionStore};
use crate::tools::ToolResult;
use anyhow::Result;
use std::path::Path;
use std::process::Command;

pub(super) fn record_before_tool<R: SessionStore>(
    session: &R,
    cwd: &Path,
    call_id: &str,
    tool: &str,
    argument_summary: &str,
) -> Result<()> {
    let snapshot = GitSnapshot::capture(cwd);
    session.append(&SessionEvent::CheckpointBeforeTool {
        timestamp: now(),
        call_id: call_id.to_string(),
        tool: tool.to_string(),
        argument_summary: argument_summary.to_string(),
        git_available: snapshot.git_available,
        branch: snapshot.branch,
        status_short: snapshot.status_short,
    })
}

pub(super) fn record_after_tool<R: SessionStore>(
    session: &R,
    cwd: &Path,
    call_id: &str,
    tool: &str,
    result: &ToolResult,
) -> Result<()> {
    let snapshot = GitSnapshot::capture(cwd);
    let changed_files = changed_files_from_status(&snapshot.status_short);
    session.append(&SessionEvent::CheckpointAfterTool {
        timestamp: now(),
        call_id: call_id.to_string(),
        tool: tool.to_string(),
        success: result.success,
        git_available: snapshot.git_available,
        branch: snapshot.branch,
        changed_files,
        status_short: snapshot.status_short,
    })
}

struct GitSnapshot {
    git_available: bool,
    branch: Option<String>,
    status_short: String,
}

impl GitSnapshot {
    fn capture(cwd: &Path) -> Self {
        let status = Command::new("git")
            .args(["status", "--short"])
            .current_dir(cwd)
            .output();
        let Ok(status) = status else {
            return Self {
                git_available: false,
                branch: None,
                status_short: String::new(),
            };
        };
        if !status.status.success() {
            return Self {
                git_available: false,
                branch: None,
                status_short: String::from_utf8_lossy(&status.stderr).trim().to_string(),
            };
        }
        let branch = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(cwd)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|branch| !branch.is_empty());
        Self {
            git_available: true,
            branch,
            status_short: String::from_utf8_lossy(&status.stdout).trim().to_string(),
        }
    }
}

fn changed_files_from_status(status_short: &str) -> Vec<String> {
    status_short
        .lines()
        .filter_map(|line| {
            if line.len() < 4 {
                return None;
            }
            Some(line[3..].trim().to_string())
        })
        .filter(|path| !path.is_empty())
        .take(20)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_changed_files_from_git_status_short() {
        assert_eq!(
            changed_files_from_status(" M src/lib.rs\n?? src/recovery.rs"),
            vec!["src/lib.rs", "src/recovery.rs"]
        );
    }
}
