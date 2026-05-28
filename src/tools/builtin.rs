use super::{
    classify_shell_command, path_has_symlink_component, resolve_under_cwd,
    truncate_text_with_metadata, PermissionDecision, ShellSafety, Tool, ToolContext,
    ToolErrorKind, ToolMetadata, ToolRegistry, ToolResult, ToolSummary,
};
use anyhow::Context;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const READ_LIMIT: usize = 64 * 1024;
const SHELL_OUTPUT_LIMIT: usize = 32 * 1024;
const DEFAULT_SHELL_TIMEOUT_MS: u64 = 10_000;
const MAX_SHELL_TIMEOUT_MS: u64 = 120_000;

#[derive(Clone, Debug)]
pub enum BuiltinTool {
    ListFiles,
    ReadFile,
    WriteFile,
    Shell,
    Grep,
    Edit,
    Glob,
}

#[derive(Clone, Debug)]
pub struct BuiltinToolRegistry {
    tools: Vec<BuiltinTool>,
}

impl BuiltinTool {
    pub fn all() -> Vec<Self> {
        vec![
            Self::ListFiles,
            Self::ReadFile,
            Self::WriteFile,
            Self::Shell,
            Self::Grep,
            Self::Edit,
            Self::Glob,
        ]
    }
}

impl Default for BuiltinToolRegistry {
    fn default() -> Self {
        Self {
            tools: BuiltinTool::all(),
        }
    }
}

impl BuiltinToolRegistry {
    pub fn new(tools: Vec<BuiltinTool>) -> Self {
        Self { tools }
    }

    pub fn tools(&self) -> &[BuiltinTool] {
        &self.tools
    }
}

impl ToolRegistry for BuiltinToolRegistry {
    fn schemas(&self) -> Vec<Value> {
        self.tools.iter().map(Tool::schema).collect()
    }

    fn schemas_for_policy(
        &self,
        policy: &dyn super::PermissionPolicy,
        permission: crate::config::PermissionMode,
    ) -> Vec<Value> {
        self.tools
            .iter()
            .filter(|tool| !policy.hides_tool_schema(permission, tool.name()))
            .map(Tool::schema)
            .collect()
    }

    fn metadata(&self, name: &str, arguments: &Value) -> Option<ToolMetadata> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.metadata(arguments))
    }

    async fn execute(&self, name: &str, input: Value, ctx: ToolContext) -> ToolResult {
        let Some(tool) = self.tools.iter().find(|tool| tool.name() == name) else {
            return ToolResult::error(format!("unknown tool: {name}"));
        };
        tool.execute(input, ctx).await
    }
}

impl Tool for BuiltinTool {
    fn name(&self) -> &'static str {
        match self {
            BuiltinTool::ListFiles => "list_files",
            BuiltinTool::ReadFile => "read_file",
            BuiltinTool::WriteFile => "write_file",
            BuiltinTool::Shell => "shell",
            BuiltinTool::Grep => "grep",
            BuiltinTool::Edit => "edit",
            BuiltinTool::Glob => "glob",
        }
    }

    fn metadata(&self, arguments: &Value) -> ToolMetadata {
        let (read_only, destructive, concurrency_safe, permission_hint) = match self {
            BuiltinTool::ListFiles | BuiltinTool::ReadFile | BuiltinTool::Grep
            | BuiltinTool::Glob => {
                (true, false, true, PermissionDecision::Allow)
            }
            BuiltinTool::WriteFile | BuiltinTool::Edit => {
                (false, true, false, PermissionDecision::Ask)
            }
            BuiltinTool::Shell => match classify_shell_command(
                arguments
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ) {
                ShellSafety::Safe => (false, false, false, PermissionDecision::Allow),
                ShellSafety::Dangerous => (false, true, false, PermissionDecision::Deny),
                ShellSafety::Unknown => (false, false, false, PermissionDecision::Ask),
            },
        };
        ToolMetadata {
            name: self.name(),
            read_only,
            destructive,
            concurrency_safe,
            argument_summary: ToolSummary::from_arguments(self.name(), arguments).summary,
            permission_hint,
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
            BuiltinTool::Grep => json!({
                "type": "function",
                "name": "grep",
                "description": "Search file contents with a regex pattern under session cwd. Respects .gitignore via ripgrep.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Regex pattern to search for."},
                        "path": {"type": "string", "description": "File or directory relative to cwd. Defaults to ."},
                        "include": {"type": "string", "description": "Glob to filter files (e.g. '*.rs')."},
                        "context": {"type": "integer", "description": "Lines of context around each match."}
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                },
                "strict": false
            }),
            BuiltinTool::Edit => json!({
                "type": "function",
                "name": "edit",
                "description": "Replace text in a file by search-and-replace or line range. Only the specified portion is changed.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path relative to cwd."},
                        "search": {"type": "string", "description": "Exact text to locate for replacement."},
                        "replace": {"type": "string", "description": "New text to substitute (paired with search)."},
                        "line_start": {"type": "integer", "description": "Start line for line-range edit (1-based, inclusive). Requires line_end and new_content."},
                        "line_end": {"type": "integer", "description": "End line for line-range edit (1-based, inclusive). Requires line_start and new_content."},
                        "new_content": {"type": "string", "description": "Replacement content for line-range edit."}
                    },
                    "required": ["path"],
                    "additionalProperties": false
                },
                "strict": false
            }),
            BuiltinTool::Glob => json!({
                "type": "function",
                "name": "glob",
                "description": "Find files matching a glob pattern under session cwd. Returns newline-separated relative paths.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Glob pattern (e.g. '**/*.rs', '*.md')."},
                        "root": {"type": "string", "description": "Root directory relative to cwd. Defaults to ."},
                        "depth": {"type": "integer", "description": "Maximum recursion depth. No limit by default."}
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                },
                "strict": false
            }),
        }
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> ToolResult {
        match self.execute_inner(input, ctx).await {
            Ok(result) => result,
            Err(err) => match err {
                ToolExecError::Timeout { duration_ms } => {
                    ToolResult::error_with_kind(ToolErrorKind::Timeout,
                        format!("timed out after {duration_ms}ms"))
                }
                ToolExecError::Io { source, context } => {
                    ToolResult::error_with_kind(ToolErrorKind::Io,
                        format!("{context}: {source}"))
                }
                ToolExecError::Parse(error) => {
                    ToolResult::error_with_kind(ToolErrorKind::Parse, error.to_string())
                }
                ToolExecError::Permission(msg) => {
                    ToolResult::error_with_kind(ToolErrorKind::Permission, msg)
                }
                ToolExecError::ProcessExit { code } => {
                    ToolResult::error_with_kind(ToolErrorKind::ProcessExit,
                        format!("process exited with code {code:?}"))
                }
                ToolExecError::Utf8(error) => {
                    ToolResult::error_with_kind(ToolErrorKind::Utf8, error.to_string())
                }
                ToolExecError::Other(error) => {
                    ToolResult::error_with_kind(ToolErrorKind::Unknown, error.to_string())
                }
            },
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
            BuiltinTool::ReadFile => read_file(input, &ctx.cwd).await,
            BuiltinTool::WriteFile => write_file(input, &ctx).await.map(ToolResult::ok),
            BuiltinTool::Shell => shell(input, &ctx).await,
            BuiltinTool::Grep => grep(input, &ctx.cwd).await,
            BuiltinTool::Edit => edit(input, &ctx).await,
            BuiltinTool::Glob => glob(input, &ctx.cwd).await,
        }
    }
}

#[derive(Debug, Error)]
enum ToolExecError {
    #[error("timed out after {duration_ms}ms")]
    Timeout { duration_ms: u64 },
    #[error("I/O error: {context}")]
    Io {
        source: std::io::Error,
        context: String,
    },
    #[error(transparent)]
    Parse(#[from] serde_json::Error),
    #[error("permission denied: {0}")]
    Permission(String),
    #[error("process exited with code {code:?}")]
    ProcessExit { code: Option<i32> },
    #[error(transparent)]
    Utf8(#[from] std::string::FromUtf8Error),
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

#[derive(Deserialize)]
struct GrepInput {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
    context: Option<u32>,
}

#[derive(Deserialize)]
struct EditInput {
    path: String,
    search: Option<String>,
    replace: Option<String>,
    line_start: Option<usize>,
    line_end: Option<usize>,
    new_content: Option<String>,
}

#[derive(Deserialize)]
struct GlobInput {
    pattern: String,
    root: Option<String>,
    depth: Option<u32>,
}

#[derive(serde::Serialize)]
struct DirEntryInfo {
    name: String,
    kind: String,
}

async fn list_files(input: Value, cwd: &Path) -> std::result::Result<String, ToolExecError> {
    let input: PathInput = serde_json::from_value(input)?;
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

async fn read_file(input: Value, cwd: &Path) -> std::result::Result<ToolResult, ToolExecError> {
    let input: ReadInput = serde_json::from_value(input)?;
    let path = resolve_under_cwd(cwd, &input.path)?;
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("read file {}", path.display()))?;
    let text = String::from_utf8(bytes)?;
    let preview = truncate_text_with_metadata(&text, READ_LIMIT);
    Ok(ToolResult::ok_with_preview(
        preview.text,
        preview.truncated,
        preview.original_bytes,
        preview.preview_bytes,
    ))
}

async fn write_file(input: Value, ctx: &ToolContext) -> std::result::Result<String, ToolExecError> {
    let input: WriteInput = serde_json::from_value(input)?;
    let path = resolve_under_cwd(&ctx.cwd, &input.path)?;
    if path_has_symlink_component(&ctx.cwd, &path)? {
        return Err(ToolExecError::Permission(format!(
            "refusing to write through symlink path {}",
            path.display()
        )));
    }
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

async fn shell(input: Value, ctx: &ToolContext) -> std::result::Result<ToolResult, ToolExecError> {
    let input: ShellInput = serde_json::from_value(input)?;

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
            return Err(ToolExecError::Timeout {
                duration_ms: timeout_ms,
            });
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
    let preview = truncate_text_with_metadata(&combined, SHELL_OUTPUT_LIMIT);
    Ok(ToolResult::ok_with_preview(
        preview.text,
        preview.truncated,
        preview.original_bytes,
        preview.preview_bytes,
    ))
}

const GREP_OUTPUT_LIMIT: usize = 64 * 1024;
const GREP_TIMEOUT_MS: u64 = 30_000;
const GREP_MAX_MATCHES: &str = "500";

async fn grep(input: Value, cwd: &Path) -> std::result::Result<ToolResult, ToolExecError> {
    let input: GrepInput = serde_json::from_value(input)?;
    let path = resolve_under_cwd(cwd, input.path.as_deref().unwrap_or("."))?;

    let mut cmd = Command::new("rg");
    cmd.arg("--no-heading")
        .arg("--line-number")
        .arg("--max-count")
        .arg(GREP_MAX_MATCHES)
        .arg("--regexp")
        .arg(&input.pattern)
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    if let Some(include) = &input.include {
        cmd.arg("--glob").arg(include);
    }
    if let Some(ctx) = input.context {
        cmd.arg("--context").arg(ctx.to_string());
    }

    let mut child = cmd.spawn().map_err(|e| ToolExecError::Io {
        source: e,
        context: "spawn rg".into(),
    })?;

    let mut stdout = child.stdout.take().context("capture rg stdout")?;
    let mut stderr = child.stderr.take().context("capture rg stderr")?;
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).await.map(|_| buf)
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stderr.read_to_end(&mut buf).await.map(|_| buf)
    });

    let status = match tokio::time::timeout(Duration::from_millis(GREP_TIMEOUT_MS), child.wait()).await
    {
        Ok(result) => result.context("wait for rg")?,
        Err(_) => {
            let _ = child.kill().await;
            return Err(ToolExecError::Timeout {
                duration_ms: GREP_TIMEOUT_MS,
            });
        }
    };

    let stdout = stdout_task
        .await
        .context("join rg stdout task")?
        .context("read rg stdout")?;
    let stderr = stderr_task
        .await
        .context("join rg stderr task")?
        .context("read rg stderr")?;

    let code = status.code();
    if code == Some(2) {
        return Err(ToolExecError::Other(anyhow::anyhow!(
            "rg failed: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }

    let output = String::from_utf8(stdout)?;
    let output = if output.is_empty() && code == Some(1) {
        "no matches found".to_string()
    } else if output.is_empty() {
        String::from_utf8_lossy(&stderr).trim().to_string()
    } else {
        output
    };

    let preview = truncate_text_with_metadata(&output, GREP_OUTPUT_LIMIT);
    Ok(ToolResult::ok_with_preview(
        preview.text,
        preview.truncated,
        preview.original_bytes,
        preview.preview_bytes,
    ))
}

const EDIT_SIZE_LIMIT: usize = 1024 * 1024;
const GLOB_OUTPUT_LIMIT: usize = 32 * 1024;
const GLOB_TIMEOUT_MS: u64 = 30_000;

async fn edit(input: Value, ctx: &ToolContext) -> std::result::Result<ToolResult, ToolExecError> {
    let input: EditInput = serde_json::from_value(input)?;
    let path = resolve_under_cwd(&ctx.cwd, &input.path)?;
    if path_has_symlink_component(&ctx.cwd, &path)? {
        return Err(ToolExecError::Permission(format!(
            "refusing to edit through symlink path {}",
            path.display()
        )));
    }

    let original = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| ToolExecError::Io {
            source: e,
            context: format!("read file for edit {}", path.display()),
        })?;

    let edited = if let (Some(search), Some(replace)) = (&input.search, &input.replace) {
        // search/replace mode: replace first occurrence
        if !original.contains(search) {
            return Err(ToolExecError::Other(anyhow::anyhow!(
                "search text not found in {}",
                path.display()
            )));
        }
        original.replacen(search, replace, 1)
    } else if let (Some(start), Some(end), Some(new_content)) =
        (&input.line_start, &input.line_end, &input.new_content)
    {
        // line-based mode: replace range of lines
        let lines: Vec<&str> = original.lines().collect();
        let total = lines.len();
        if *start < 1 || *end < *start || *end > total {
            return Err(ToolExecError::Other(anyhow::anyhow!(
                "invalid line range {start}..{end} (file has {total} lines)"
            )));
        }
        let mut new_lines: Vec<&str> = Vec::new();
        new_lines.extend(&lines[..start - 1]);
        for line in new_content.lines() {
            new_lines.push(line);
        }
        new_lines.extend(&lines[*end..]);
        new_lines.join("\n")
    } else {
        return Err(ToolExecError::Other(anyhow::anyhow!(
            "edit requires search+replace or line_start+line_end+new_content"
        )));
    };

    if edited.len() > EDIT_SIZE_LIMIT {
        return Err(ToolExecError::Other(anyhow::anyhow!(
            "edit result exceeds {EDIT_SIZE_LIMIT} bytes"
        )));
    }

    tokio::fs::write(&path, &edited)
        .await
        .map_err(|e| ToolExecError::Io {
            source: e,
            context: format!("write edited file {}", path.display()),
        })?;

    let rel = path.strip_prefix(&ctx.cwd).unwrap_or(&path).display();
    Ok(ToolResult::ok(format!(
        "edited {} ({} → {} bytes)",
        rel,
        original.len(),
        edited.len()
    )))
}

async fn glob(input: Value, cwd: &Path) -> std::result::Result<ToolResult, ToolExecError> {
    let input: GlobInput = serde_json::from_value(input)?;
    let root = resolve_under_cwd(cwd, input.root.as_deref().unwrap_or("."))?;

    let mut cmd = Command::new("find");
    cmd.arg(&root)
        .arg("-name")
        .arg(&input.pattern)
        .arg("-type")
        .arg("f")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    if let Some(depth) = input.depth {
        cmd.arg("-maxdepth").arg(depth.to_string());
    }

    let mut child = cmd.spawn().map_err(|e| ToolExecError::Io {
        source: e,
        context: "spawn find".into(),
    })?;

    let mut stdout = child.stdout.take().context("capture find stdout")?;
    let mut stderr = child.stderr.take().context("capture find stderr")?;
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).await.map(|_| buf)
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stderr.read_to_end(&mut buf).await.map(|_| buf)
    });

    let _status =
        match tokio::time::timeout(Duration::from_millis(GLOB_TIMEOUT_MS), child.wait()).await {
            Ok(result) => result.context("wait for find")?,
            Err(_) => {
                let _ = child.kill().await;
                return Err(ToolExecError::Timeout {
                    duration_ms: GLOB_TIMEOUT_MS,
                });
            }
        };

    let stdout = stdout_task
        .await
        .context("join find stdout task")?
        .context("read find stdout")?;
    let _stderr = stderr_task
        .await
        .context("join find stderr task")?
        .context("read find stderr")?;

    let output = String::from_utf8(stdout)?;
    let output = if output.is_empty() {
        "no files matched".to_string()
    } else {
        // strip the root prefix from paths
        let root_prefix = format!("{}/", root.display());
        output
            .lines()
            .map(|line| line.strip_prefix(&root_prefix).unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let preview = truncate_text_with_metadata(&output, GLOB_OUTPUT_LIMIT);
    Ok(ToolResult::ok_with_preview(
        preview.text,
        preview.truncated,
        preview.original_bytes,
        preview.preview_bytes,
    ))
}

impl fmt::Display for BuiltinTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
