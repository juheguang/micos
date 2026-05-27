use super::{
    classify_shell_command, path_has_symlink_component, resolve_under_cwd, truncate_text,
    PermissionDecision, ShellSafety, Tool, ToolContext, ToolMetadata, ToolRegistry, ToolResult,
    ToolSummary,
};
use anyhow::{anyhow, Context};
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
        }
    }

    fn metadata(&self, arguments: &Value) -> ToolMetadata {
        let (read_only, destructive, concurrency_safe, permission_hint) = match self {
            BuiltinTool::ListFiles | BuiltinTool::ReadFile => {
                (true, false, true, PermissionDecision::Allow)
            }
            BuiltinTool::WriteFile => (false, true, false, PermissionDecision::Ask),
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
        }
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> ToolResult {
        match self.execute_inner(input, ctx).await {
            Ok(result) => result,
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
    let path = resolve_under_cwd(&ctx.cwd, &input.path)?;
    if path_has_symlink_component(&ctx.cwd, &path)? {
        return Err(ToolExecError::Other(anyhow!(
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

async fn shell(input: Value, ctx: &ToolContext) -> std::result::Result<String, ToolExecError> {
    let input: ShellInput = serde_json::from_value(input).context("parse shell input")?;

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

impl fmt::Display for BuiltinTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
