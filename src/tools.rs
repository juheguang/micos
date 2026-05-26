use crate::config::PermissionMode;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const READ_LIMIT: usize = 64 * 1024;
const SHELL_OUTPUT_LIMIT: usize = 32 * 1024;
const DEFAULT_SHELL_TIMEOUT_MS: u64 = 10_000;
const MAX_SHELL_TIMEOUT_MS: u64 = 120_000;

#[allow(async_fn_in_trait)]
pub trait Tool {
    fn name(&self) -> &'static str;
    fn schema(&self) -> Value;
    async fn execute(&self, input: Value, ctx: ToolContext) -> ToolResult;
}

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub permission: PermissionMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    pub denied: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolPermissionDecision {
    Allowed,
    NeedsApproval { summary: String },
    Denied { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolSummary {
    pub summary: String,
}

impl ToolSummary {
    pub fn from_arguments(name: &str, arguments: &Value) -> Self {
        let summary = match name {
            "list_files" => format!(
                "path={}",
                arguments.get("path").and_then(Value::as_str).unwrap_or(".")
            ),
            "read_file" => format!(
                "path={}",
                arguments
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>")
            ),
            "write_file" => {
                let path = arguments
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>");
                let bytes = arguments
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::len)
                    .unwrap_or(0);
                format!("path={path} bytes={bytes}")
            }
            "shell" => format!(
                "command={}",
                arguments
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>")
            ),
            _ => arguments.to_string(),
        };
        Self { summary }
    }
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            error: None,
            denied: false,
        }
    }

    pub fn error(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error.into()),
            denied: false,
        }
    }

    pub fn denied(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error.into()),
            denied: true,
        }
    }
}

#[derive(Clone, Debug)]
pub enum BuiltinTool {
    ListFiles,
    ReadFile,
    WriteFile,
    Shell,
}

impl BuiltinTool {
    pub fn all() -> Vec<Self> {
        vec![
            Self::ListFiles,
            Self::ReadFile,
            Self::WriteFile,
            Self::Shell,
        ]
    }
}

impl Tool for BuiltinTool {
    fn name(&self) -> &'static str {
        match self {
            BuiltinTool::ListFiles => "list_files",
            BuiltinTool::ReadFile => "read_file",
            BuiltinTool::WriteFile => "write_file",
            BuiltinTool::Shell => "shell",
        }
    }

    fn schema(&self) -> Value {
        match self {
            BuiltinTool::ListFiles => json!({
                "type": "function",
                "name": "list_files",
                "description": "List non-recursive entries in a directory under the session cwd.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Directory path relative to cwd. Defaults to ."}
                    },
                    "additionalProperties": false
                },
                "strict": false
            }),
            BuiltinTool::ReadFile => json!({
                "type": "function",
                "name": "read_file",
                "description": "Read a UTF-8 file under the session cwd with output truncation.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path relative to cwd."}
                    },
                    "required": ["path"],
                    "additionalProperties": false
                },
                "strict": false
            }),
            BuiltinTool::WriteFile => json!({
                "type": "function",
                "name": "write_file",
                "description": "Overwrite a UTF-8 file under the session cwd, creating parent directories.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path relative to cwd."},
                        "content": {"type": "string", "description": "Full file contents."}
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                },
                "strict": false
            }),
            BuiltinTool::Shell => json!({
                "type": "function",
                "name": "shell",
                "description": "Run a shell command in the session cwd with timeout and output truncation.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "Command to run through the user's shell."},
                        "timeout_ms": {"type": "integer", "description": "Optional timeout in milliseconds."}
                    },
                    "required": ["command"],
                    "additionalProperties": false
                },
                "strict": false
            }),
        }
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> ToolResult {
        match self.execute_inner(input, ctx).await {
            Ok(result) => result,
            Err(ToolExecError::Denied(message)) => ToolResult::denied(message),
            Err(ToolExecError::Other(error)) => ToolResult::error(error.to_string()),
        }
    }
}

impl BuiltinTool {
    async fn execute_inner(
        &self,
        input: Value,
        ctx: ToolContext,
    ) -> std::result::Result<ToolResult, ToolExecError> {
        match self {
            BuiltinTool::ListFiles => list_files(input, &ctx.cwd).await.map(ToolResult::ok),
            BuiltinTool::ReadFile => read_file(input, &ctx.cwd).await.map(ToolResult::ok),
            BuiltinTool::WriteFile => write_file(input, &ctx).await.map(ToolResult::ok),
            BuiltinTool::Shell => shell(input, &ctx).await.map(ToolResult::ok),
        }
    }
}

#[derive(Debug, Error)]
enum ToolExecError {
    #[error("{0}")]
    Denied(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Deserialize)]
struct PathInput {
    path: Option<String>,
}

#[derive(Deserialize)]
struct ReadInput {
    path: String,
}

#[derive(Deserialize)]
struct WriteInput {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct ShellInput {
    command: String,
    timeout_ms: Option<u64>,
}

#[derive(serde::Serialize)]
struct DirEntryInfo {
    name: String,
    kind: String,
}

async fn list_files(input: Value, cwd: &Path) -> std::result::Result<String, ToolExecError> {
    let input: PathInput = serde_json::from_value(input).context("parse list_files input")?;
    let path = resolve_under_cwd(cwd, input.path.as_deref().unwrap_or("."))?;
    let mut dir = tokio::fs::read_dir(&path)
        .await
        .with_context(|| format!("read directory {}", path.display()))?;
    let mut entries = Vec::new();
    while let Some(entry) = dir.next_entry().await.context("read directory entry")? {
        let ty = entry.file_type().await.context("read file type")?;
        entries.push(DirEntryInfo {
            name: entry.file_name().to_string_lossy().to_string(),
            kind: if ty.is_dir() {
                "directory"
            } else if ty.is_file() {
                "file"
            } else if ty.is_symlink() {
                "symlink"
            } else {
                "other"
            }
            .to_string(),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    serde_json::to_string_pretty(&entries)
        .map_err(anyhow::Error::from)
        .map_err(ToolExecError::Other)
}

async fn read_file(input: Value, cwd: &Path) -> std::result::Result<String, ToolExecError> {
    let input: ReadInput = serde_json::from_value(input).context("parse read_file input")?;
    let path = resolve_under_cwd(cwd, &input.path)?;
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("read file {}", path.display()))?;
    let text = String::from_utf8(bytes).context("file is not valid UTF-8")?;
    Ok(truncate_text(&text, READ_LIMIT))
}

async fn write_file(input: Value, ctx: &ToolContext) -> std::result::Result<String, ToolExecError> {
    let input: WriteInput = serde_json::from_value(input).context("parse write_file input")?;
    if let ToolPermissionDecision::Denied { reason } = permission_decision(
        ctx.permission,
        "write_file",
        &json!({"path": input.path.clone()}),
    ) {
        return Err(ToolExecError::Denied(reason));
    }
    let path = resolve_under_cwd(&ctx.cwd, &input.path)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create parent directory {}", parent.display()))?;
    }
    tokio::fs::write(&path, input.content)
        .await
        .with_context(|| format!("write file {}", path.display()))?;
    Ok(format!(
        "wrote {}",
        path.strip_prefix(&ctx.cwd).unwrap_or(&path).display()
    ))
}

async fn shell(input: Value, ctx: &ToolContext) -> std::result::Result<String, ToolExecError> {
    let input: ShellInput = serde_json::from_value(input).context("parse shell input")?;
    if let ToolPermissionDecision::Denied { reason } = permission_decision(
        ctx.permission,
        "shell",
        &json!({"command": input.command.clone()}),
    ) {
        return Err(ToolExecError::Denied(reason));
    }

    let timeout_ms = input
        .timeout_ms
        .unwrap_or(DEFAULT_SHELL_TIMEOUT_MS)
        .min(MAX_SHELL_TIMEOUT_MS);
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut child = Command::new(shell)
        .arg("-lc")
        .arg(&input.command)
        .current_dir(&ctx.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn shell command")?;

    let mut stdout = child.stdout.take().context("capture stdout")?;
    let mut stderr = child.stderr.take().context("capture stderr")?;
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).await.map(|_| buf)
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stderr.read_to_end(&mut buf).await.map(|_| buf)
    });

    let status = match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(result) => result.context("wait for shell command")?,
        Err(_) => {
            let _ = child.kill().await;
            return Err(ToolExecError::Other(anyhow!(
                "shell command timed out after {timeout_ms}ms"
            )));
        }
    };

    let stdout = stdout_task
        .await
        .context("join stdout task")?
        .context("read stdout")?;
    let stderr = stderr_task
        .await
        .context("join stderr task")?
        .context("read stderr")?;
    let combined = format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        status
            .code()
            .map_or_else(|| "signal".to_string(), |code| code.to_string()),
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    Ok(truncate_text(&combined, SHELL_OUTPUT_LIMIT))
}

pub fn permission_decision(
    permission: PermissionMode,
    tool_name: &str,
    arguments: &Value,
) -> ToolPermissionDecision {
    match tool_name {
        "list_files" | "read_file" => ToolPermissionDecision::Allowed,
        "write_file" => match permission {
            PermissionMode::Safe => ToolPermissionDecision::Denied {
                reason: "write_file is denied in safe permission mode".to_string(),
            },
            PermissionMode::Ask => ToolPermissionDecision::NeedsApproval {
                summary: ToolSummary::from_arguments(tool_name, arguments).summary,
            },
            PermissionMode::Auto => ToolPermissionDecision::Allowed,
        },
        "shell" => {
            let command = arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match permission {
                PermissionMode::Safe if is_safe_shell_command(command) => {
                    ToolPermissionDecision::Allowed
                }
                PermissionMode::Safe => ToolPermissionDecision::Denied {
                    reason: format!("shell command denied in safe permission mode: {command}"),
                },
                PermissionMode::Ask => ToolPermissionDecision::NeedsApproval {
                    summary: ToolSummary::from_arguments(tool_name, arguments).summary,
                },
                PermissionMode::Auto => ToolPermissionDecision::Allowed,
            }
        }
        _ => ToolPermissionDecision::Allowed,
    }
}

pub fn is_safe_shell_command(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() || contains_shell_control(trimmed) {
        return false;
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    match parts.as_slice() {
        ["pwd", ..] | ["ls", ..] | ["cat", ..] | ["head", ..] | ["tail", ..] | ["rg", ..] => true,
        ["find", args @ ..] => !args
            .iter()
            .any(|arg| matches!(*arg, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir")),
        ["git", "status", ..] => true,
        ["git", "diff", args @ ..] => !args
            .iter()
            .any(|arg| *arg == "-o" || arg.starts_with("--output")),
        _ => false,
    }
}

fn contains_shell_control(command: &str) -> bool {
    [">", "<", "|", ";", "&&", "||", "$(", "`"]
        .iter()
        .any(|needle| command.contains(needle))
}

pub fn resolve_under_cwd(cwd: &Path, raw_path: &str) -> Result<PathBuf> {
    let raw = Path::new(raw_path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    };
    let cwd = normalize_path(cwd)?;
    let normalized = normalize_path(&joined)?;
    if normalized.starts_with(&cwd) {
        Ok(normalized)
    } else {
        bail!(
            "path {} escapes cwd {}",
            normalized.display(),
            cwd.display()
        )
    }
}

fn normalize_path(path: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new(OsStr::new("/"))),
            Component::CurDir => {}
            Component::Normal(part) => out.push(part),
            Component::ParentDir => {
                if !out.pop() {
                    bail!("path contains too many parent components");
                }
            }
        }
    }
    Ok(out)
}

pub fn truncate_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[truncated: {} bytes omitted]",
        &text[..end],
        text.len() - end
    )
}

impl fmt::Display for BuiltinTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_containment_rejects_escapes() {
        let cwd = Path::new("/tmp/micos");
        assert!(resolve_under_cwd(cwd, "../secret").is_err());
        assert!(resolve_under_cwd(cwd, "/etc/passwd").is_err());
        assert_eq!(
            resolve_under_cwd(cwd, "src/main.rs").unwrap(),
            PathBuf::from("/tmp/micos/src/main.rs")
        );
    }

    #[test]
    fn safe_shell_allows_readonly_prefixes() {
        assert!(is_safe_shell_command("ls -la"));
        assert!(is_safe_shell_command("git status --short"));
        assert!(is_safe_shell_command("git diff -- src/main.rs"));
        assert!(!is_safe_shell_command("rm -rf target"));
        assert!(!is_safe_shell_command("cat file > out"));
        assert!(!is_safe_shell_command("find . -delete"));
        assert!(!is_safe_shell_command("git diff --output=patch.txt"));
        assert!(!is_safe_shell_command("ls | head"));
    }

    #[test]
    fn permission_policy_covers_tools_and_modes() {
        assert_eq!(
            permission_decision(PermissionMode::Safe, "list_files", &json!({})),
            ToolPermissionDecision::Allowed
        );
        assert!(matches!(
            permission_decision(PermissionMode::Safe, "write_file", &json!({"path":"x"})),
            ToolPermissionDecision::Denied { .. }
        ));
        assert!(matches!(
            permission_decision(PermissionMode::Ask, "write_file", &json!({"path":"x"})),
            ToolPermissionDecision::NeedsApproval { .. }
        ));
        assert_eq!(
            permission_decision(
                PermissionMode::Auto,
                "shell",
                &json!({"command":"rm -rf x"})
            ),
            ToolPermissionDecision::Allowed
        );
        assert!(matches!(
            permission_decision(
                PermissionMode::Safe,
                "shell",
                &json!({"command":"rm -rf x"})
            ),
            ToolPermissionDecision::Denied { .. }
        ));
    }

    #[test]
    fn schemas_have_expected_names_and_required_fields() {
        let schemas: Vec<_> = BuiltinTool::all()
            .into_iter()
            .map(|tool| tool.schema())
            .collect();
        let names: Vec<_> = schemas
            .iter()
            .map(|schema| schema["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["list_files", "read_file", "write_file", "shell"]
        );
        assert_eq!(schemas[1]["parameters"]["required"], json!(["path"]));
        assert_eq!(
            schemas[2]["parameters"]["required"],
            json!(["path", "content"])
        );
        assert_eq!(schemas[3]["parameters"]["required"], json!(["command"]));
    }

    #[test]
    fn truncation_is_deterministic() {
        let text = "abcdef";
        assert_eq!(truncate_text(text, 3), "abc\n[truncated: 3 bytes omitted]");
    }
}
