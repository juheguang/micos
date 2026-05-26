use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const DEFAULT_MODEL: &str = "gpt-5.3-codex";
pub const DEFAULT_RESPONSES_BASE_URL: &str = "https://api.openai.com/v1/responses";
pub const DEFAULT_MAX_STEPS: usize = 20;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Safe,
    Ask,
    Auto,
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PermissionMode::Safe => write!(f, "safe"),
            PermissionMode::Ask => write!(f, "ask"),
            PermissionMode::Auto => write!(f, "auto"),
        }
    }
}

impl FromStr for PermissionMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "safe" => Ok(PermissionMode::Safe),
            "ask" => Ok(PermissionMode::Ask),
            "auto" => Ok(PermissionMode::Auto),
            other => bail!("unknown permission mode: {other}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKind {
    Responses,
    ChatCompletions,
}

impl ApiKind {
    pub fn is_chat_completions(&self) -> bool {
        matches!(self, ApiKind::ChatCompletions)
    }
}

impl fmt::Display for ApiKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiKind::Responses => write!(f, "responses"),
            ApiKind::ChatCompletions => write!(f, "chat_completions"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingMode {
    Enabled,
    Disabled,
}

impl fmt::Display for ThinkingMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThinkingMode::Enabled => write!(f, "enabled"),
            ThinkingMode::Disabled => write!(f, "disabled"),
        }
    }
}

impl FromStr for ThinkingMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "enabled" | "on" | "true" => Ok(ThinkingMode::Enabled),
            "disabled" | "off" | "false" => Ok(ThinkingMode::Disabled),
            other => bail!("unknown thinking mode: {other}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReasoningEffort::Low => write!(f, "low"),
            ReasoningEffort::Medium => write!(f, "medium"),
            ReasoningEffort::High => write!(f, "high"),
            ReasoningEffort::Xhigh => write!(f, "xhigh"),
            ReasoningEffort::Max => write!(f, "max"),
        }
    }
}

impl FromStr for ReasoningEffort {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "low" => Ok(ReasoningEffort::Low),
            "medium" => Ok(ReasoningEffort::Medium),
            "high" => Ok(ReasoningEffort::High),
            "xhigh" => Ok(ReasoningEffort::Xhigh),
            "max" => Ok(ReasoningEffort::Max),
            other => bail!("unknown reasoning effort: {other}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionConfig {
    pub api_kind: ApiKind,
    pub model: String,
    pub base_url: String,
    pub thinking: Option<ThinkingMode>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub permission: PermissionMode,
    pub max_steps: usize,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct ConfigOverrides {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub thinking: Option<ThinkingMode>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub permission: Option<PermissionMode>,
    pub max_steps: Option<usize>,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct FileConfig {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub thinking: Option<ThinkingMode>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub permission: Option<PermissionMode>,
    pub max_steps: Option<usize>,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct EnvConfig {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub thinking: Option<ThinkingMode>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub permission: Option<PermissionMode>,
    pub max_steps: Option<usize>,
    pub cwd: Option<PathBuf>,
}

impl SessionConfig {
    pub fn load(cli: ConfigOverrides) -> Result<Self> {
        let base_cwd = cli
            .cwd
            .clone()
            .unwrap_or(std::env::current_dir().context("read current directory")?);
        let file_config = read_config_file(&base_cwd)?;
        let env = EnvConfig::from_process()?;
        Self::resolve(base_cwd, file_config, env, cli)
    }

    pub fn resolve(
        process_cwd: PathBuf,
        file: FileConfig,
        env: EnvConfig,
        cli: ConfigOverrides,
    ) -> Result<Self> {
        let cwd = cli.cwd.or(env.cwd).or(file.cwd).unwrap_or(process_cwd);
        let cwd = absolutize(&cwd).context("resolve cwd")?;

        let base_url = cli
            .base_url
            .or(env.base_url)
            .or(file.base_url)
            .unwrap_or_else(|| DEFAULT_RESPONSES_BASE_URL.to_string());
        let api_kind = infer_api_kind(&base_url);
        let model = cli
            .model
            .or(env.model)
            .or(file.model)
            .unwrap_or_else(|| default_model_for_base_url(&base_url).to_string());

        Ok(Self {
            api_kind,
            model,
            base_url,
            thinking: cli.thinking.or(env.thinking).or(file.thinking),
            reasoning_effort: cli
                .reasoning_effort
                .or(env.reasoning_effort)
                .or(file.reasoning_effort),
            permission: cli
                .permission
                .or(env.permission)
                .or(file.permission)
                .unwrap_or(PermissionMode::Ask),
            max_steps: cli
                .max_steps
                .or(env.max_steps)
                .or(file.max_steps)
                .unwrap_or(DEFAULT_MAX_STEPS),
            cwd,
        })
    }
}

impl EnvConfig {
    pub fn from_process() -> Result<Self> {
        let permission = match std::env::var("MICOS_PERMISSION") {
            Ok(value) => Some(value.parse()?),
            Err(_) => None,
        };
        let max_steps = match std::env::var("MICOS_MAX_STEPS") {
            Ok(value) => Some(value.parse().context("parse MICOS_MAX_STEPS")?),
            Err(_) => None,
        };
        let thinking = match std::env::var("MICOS_THINKING") {
            Ok(value) => Some(value.parse()?),
            Err(_) => None,
        };
        let reasoning_effort = match std::env::var("MICOS_REASONING_EFFORT") {
            Ok(value) => Some(value.parse()?),
            Err(_) => None,
        };
        let cwd = std::env::var("MICOS_CWD").ok().map(PathBuf::from);

        Ok(Self {
            model: std::env::var("MICOS_MODEL").ok(),
            base_url: std::env::var("MICOS_BASE_URL").ok(),
            thinking,
            reasoning_effort,
            permission,
            max_steps,
            cwd,
        })
    }
}

fn read_config_file(cwd: &Path) -> Result<FileConfig> {
    let path = cwd.join(".micos/config.toml");
    if !path.exists() {
        return Ok(FileConfig::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read config file {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse config file {}", path.display()))
}

fn absolutize(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path.canonicalize().context("canonicalize existing path");
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub fn infer_api_kind(base_url: &str) -> ApiKind {
    if base_url.trim_end_matches('/').ends_with("/responses") {
        ApiKind::Responses
    } else {
        ApiKind::ChatCompletions
    }
}

pub fn default_model_for_base_url(base_url: &str) -> &'static str {
    if base_url.contains("api.deepseek.com") {
        "deepseek-chat"
    } else {
        DEFAULT_MODEL
    }
}

pub fn resolve_api_key() -> Result<String> {
    std::env::var("MICOS_API_KEY").with_context(|| "missing API key: set MICOS_API_KEY")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_uses_defaults() {
        let cfg = SessionConfig::resolve(
            PathBuf::from("/tmp/micos-defaults"),
            FileConfig::default(),
            EnvConfig::default(),
            ConfigOverrides::default(),
        )
        .unwrap();

        assert_eq!(cfg.model, DEFAULT_MODEL);
        assert_eq!(cfg.api_kind, ApiKind::Responses);
        assert_eq!(cfg.base_url, DEFAULT_RESPONSES_BASE_URL);
        assert_eq!(cfg.thinking, None);
        assert_eq!(cfg.reasoning_effort, None);
        assert_eq!(cfg.permission, PermissionMode::Ask);
        assert_eq!(cfg.max_steps, DEFAULT_MAX_STEPS);
    }

    #[test]
    fn config_precedence_cli_env_file_defaults() {
        let cfg = SessionConfig::resolve(
            PathBuf::from("/tmp/micos"),
            FileConfig {
                model: Some("file-model".into()),
                base_url: Some("https://file.example/v1/chat/completions".into()),
                thinking: Some(ThinkingMode::Disabled),
                reasoning_effort: Some(ReasoningEffort::High),
                permission: Some(PermissionMode::Safe),
                max_steps: Some(3),
                cwd: None,
            },
            EnvConfig {
                model: Some("env-model".into()),
                base_url: Some("https://env.example/v1/chat/completions".into()),
                thinking: Some(ThinkingMode::Enabled),
                reasoning_effort: Some(ReasoningEffort::Max),
                permission: Some(PermissionMode::Auto),
                max_steps: Some(4),
                cwd: None,
            },
            ConfigOverrides {
                model: Some("cli-model".into()),
                base_url: Some("https://cli.example/v1/responses".into()),
                thinking: Some(ThinkingMode::Disabled),
                reasoning_effort: Some(ReasoningEffort::Medium),
                permission: Some(PermissionMode::Ask),
                max_steps: Some(5),
                cwd: None,
            },
        )
        .unwrap();

        assert_eq!(cfg.model, "cli-model");
        assert_eq!(cfg.api_kind, ApiKind::Responses);
        assert_eq!(cfg.base_url, "https://cli.example/v1/responses");
        assert_eq!(cfg.thinking, Some(ThinkingMode::Disabled));
        assert_eq!(cfg.reasoning_effort, Some(ReasoningEffort::Medium));
        assert_eq!(cfg.permission, PermissionMode::Ask);
        assert_eq!(cfg.max_steps, 5);
    }

    #[test]
    fn deepseek_base_url_uses_chat_completions_and_model_default() {
        let cfg = SessionConfig::resolve(
            PathBuf::from("/tmp/micos"),
            FileConfig::default(),
            EnvConfig {
                base_url: Some("https://api.deepseek.com/chat/completions".into()),
                ..EnvConfig::default()
            },
            ConfigOverrides::default(),
        )
        .unwrap();

        assert_eq!(cfg.model, "deepseek-chat");
        assert_eq!(cfg.base_url, "https://api.deepseek.com/chat/completions");
        assert!(cfg.api_kind.is_chat_completions());
    }

    #[test]
    fn file_config_accepts_base_url_only() {
        let cfg: FileConfig = toml::from_str(
            r#"
model = "deepseek-chat"
base_url = "https://api.deepseek.com/chat/completions"
thinking = "enabled"
reasoning_effort = "max"
"#,
        )
        .unwrap();

        assert_eq!(cfg.model, Some("deepseek-chat".into()));
        assert_eq!(
            cfg.base_url,
            Some("https://api.deepseek.com/chat/completions".into())
        );
        assert_eq!(cfg.thinking, Some(ThinkingMode::Enabled));
        assert_eq!(cfg.reasoning_effort, Some(ReasoningEffort::Max));
    }
}
