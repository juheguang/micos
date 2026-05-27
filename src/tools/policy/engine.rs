use super::super::{PermissionPolicy, ToolMetadata};
use crate::config::PermissionMode;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleBehavior {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSource {
    Config,
    Cli,
    Session,
    RuntimeDefault,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PermissionRule {
    pub source: RuleSource,
    pub behavior: RuleBehavior,
    pub tool: String,
    pub specifier: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    Rule,
    Mode,
    Tool,
    SafetyCheck,
    RuntimeApproval,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PolicyDecision {
    pub decision: PermissionDecision,
    pub reason: DecisionReason,
    pub rule_source: Option<RuleSource>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellSafety {
    Safe,
    Dangerous,
    Unknown,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PolicyEngine {
    rules: Vec<PermissionRule>,
}

impl PolicyEngine {
    pub fn new(rules: Vec<PermissionRule>) -> Self {
        Self { rules }
    }

    pub fn rules(&self) -> &[PermissionRule] {
        &self.rules
    }

    fn decide_rule(
        &self,
        behavior: RuleBehavior,
        tool_name: &str,
        arguments: &Value,
    ) -> Option<PolicyDecision> {
        match behavior {
            RuleBehavior::Allow if tool_name == "shell" => {
                let matching_rules = self
                    .rules
                    .iter()
                    .filter(|rule| {
                        rule.behavior == RuleBehavior::Allow && rule.applies_to_tool(tool_name)
                    })
                    .collect::<Vec<_>>();
                if matching_rules.is_empty() {
                    return None;
                }
                let command = shell_command(arguments);
                let segments = split_shell_command(command);
                let all_segments_allowed = segments.iter().all(|segment| {
                    matching_rules
                        .iter()
                        .any(|rule| rule.matches_command_segment(segment))
                });
                all_segments_allowed.then(|| {
                    let source = matching_rules
                        .iter()
                        .find(|rule| rule.specifier.is_none())
                        .or_else(|| matching_rules.first())
                        .map(|rule| rule.source);
                    decision(
                        PermissionDecision::Allow,
                        DecisionReason::Rule,
                        source,
                        "allowed by permission rule",
                    )
                })
            }
            RuleBehavior::Allow => self
                .rules
                .iter()
                .find(|rule| {
                    rule.behavior == RuleBehavior::Allow
                        && rule.matches_invocation(tool_name, arguments)
                })
                .map(|rule| {
                    decision(
                        PermissionDecision::Allow,
                        DecisionReason::Rule,
                        Some(rule.source),
                        "allowed by permission rule",
                    )
                }),
            RuleBehavior::Ask | RuleBehavior::Deny => self
                .rules
                .iter()
                .find(|rule| {
                    rule.behavior == behavior && rule.matches_invocation(tool_name, arguments)
                })
                .map(|rule| {
                    decision(
                        match behavior {
                            RuleBehavior::Ask => PermissionDecision::Ask,
                            RuleBehavior::Deny => PermissionDecision::Deny,
                            RuleBehavior::Allow => unreachable!(),
                        },
                        DecisionReason::Rule,
                        Some(rule.source),
                        match behavior {
                            RuleBehavior::Ask => "approval required by permission rule",
                            RuleBehavior::Deny => "denied by permission rule",
                            RuleBehavior::Allow => unreachable!(),
                        },
                    )
                }),
        }
    }

    fn default_for_mode(
        &self,
        permission: PermissionMode,
        metadata: &ToolMetadata,
        tool_name: &str,
        arguments: &Value,
    ) -> PolicyDecision {
        if matches!(tool_name, "list_files" | "read_file") || metadata.read_only {
            return decision(
                PermissionDecision::Allow,
                DecisionReason::Tool,
                Some(RuleSource::RuntimeDefault),
                "read-only tool",
            );
        }

        match tool_name {
            "write_file" => match permission {
                PermissionMode::Safe => decision(
                    PermissionDecision::Deny,
                    DecisionReason::Mode,
                    Some(RuleSource::RuntimeDefault),
                    "write_file is denied in safe permission mode",
                ),
                PermissionMode::Ask => decision(
                    PermissionDecision::Ask,
                    DecisionReason::Mode,
                    Some(RuleSource::RuntimeDefault),
                    "write_file requires approval in ask permission mode",
                ),
                PermissionMode::Auto => decision(
                    PermissionDecision::Allow,
                    DecisionReason::Mode,
                    Some(RuleSource::RuntimeDefault),
                    "write_file allowed in auto permission mode",
                ),
            },
            "shell" => default_for_shell(permission, arguments),
            _ => decision(
                PermissionDecision::Allow,
                DecisionReason::Tool,
                Some(RuleSource::RuntimeDefault),
                "unknown tool has no permission restriction",
            ),
        }
    }
}

impl PermissionPolicy for PolicyEngine {
    fn decide(
        &self,
        permission: PermissionMode,
        metadata: &ToolMetadata,
        tool_name: &str,
        arguments: &Value,
    ) -> PolicyDecision {
        if let Some(decision) = self.decide_rule(RuleBehavior::Deny, tool_name, arguments) {
            return decision;
        }
        if let Some(decision) = self.decide_rule(RuleBehavior::Ask, tool_name, arguments) {
            return decision;
        }
        if let Some(decision) = self.decide_rule(RuleBehavior::Allow, tool_name, arguments) {
            return decision;
        }
        if matches!(
            metadata.permission_hint,
            PermissionDecision::Allow | PermissionDecision::Deny
        ) && tool_name != "shell"
        {
            return decision(
                metadata.permission_hint,
                DecisionReason::Tool,
                Some(RuleSource::RuntimeDefault),
                "tool permission hint",
            );
        }
        self.default_for_mode(permission, metadata, tool_name, arguments)
    }

    fn hides_tool_schema(&self, _permission: PermissionMode, tool_name: &str) -> bool {
        self.rules.iter().any(|rule| {
            rule.behavior == RuleBehavior::Deny
                && rule.specifier.is_none()
                && rule.applies_to_tool(tool_name)
        })
    }
}

impl PermissionRule {
    pub fn parse(source: RuleSource, behavior: RuleBehavior, text: &str) -> Result<Self> {
        let raw = text.trim();
        if raw.is_empty() {
            bail!("permission rule cannot be empty");
        }
        let (tool, specifier) = if let Some(open) = raw.find('(') {
            if !raw.ends_with(')') {
                bail!("permission rule has unclosed specifier: {raw}");
            }
            if raw[open + 1..raw.len() - 1].contains(['(', ')']) {
                bail!("permission rule has nested specifier: {raw}");
            }
            let tool = raw[..open].trim();
            let specifier = raw[open + 1..raw.len() - 1].trim();
            if tool.is_empty() || specifier.is_empty() {
                bail!("permission rule is missing a tool or specifier: {raw}");
            }
            let specifier = (specifier != "*").then(|| specifier.to_string());
            (tool, specifier)
        } else if raw.contains(')') {
            bail!("permission rule has unmatched close paren: {raw}");
        } else {
            (raw, None)
        };
        Ok(Self {
            source,
            behavior,
            tool: tool.to_string(),
            specifier,
        })
    }

    pub fn matches_invocation(&self, tool_name: &str, arguments: &Value) -> bool {
        if !self.applies_to_tool(tool_name) {
            return false;
        }
        match (&self.specifier, tool_name) {
            (None, _) => true,
            (Some(pattern), "shell") => split_shell_command(shell_command(arguments))
                .iter()
                .any(|segment| wildcard_match(pattern, segment)),
            (Some(pattern), _) => wildcard_match(pattern, &argument_summary(tool_name, arguments)),
        }
    }

    fn applies_to_tool(&self, tool_name: &str) -> bool {
        self.tool == "*" || self.tool == tool_name
    }

    fn matches_command_segment(&self, segment: &str) -> bool {
        match &self.specifier {
            None => true,
            Some(pattern) => wildcard_match(pattern, segment),
        }
    }
}

pub fn parse_permission_rules(
    source: RuleSource,
    behavior: RuleBehavior,
    values: &[String],
) -> Result<Vec<PermissionRule>> {
    values
        .iter()
        .map(|value| PermissionRule::parse(source, behavior, value))
        .collect()
}

pub fn is_safe_shell_command(command: &str) -> bool {
    classify_shell_command(command) == ShellSafety::Safe
}

pub fn classify_shell_command(command: &str) -> ShellSafety {
    let segments = split_shell_command(command);
    if segments.is_empty() {
        return ShellSafety::Unknown;
    }
    let mut saw_unknown = false;
    for segment in segments {
        match classify_shell_segment(&segment) {
            ShellSafety::Safe => {}
            ShellSafety::Dangerous => return ShellSafety::Dangerous,
            ShellSafety::Unknown => saw_unknown = true,
        }
    }
    if saw_unknown {
        ShellSafety::Unknown
    } else {
        ShellSafety::Safe
    }
}

pub fn split_shell_command(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        let is_separator = match ch {
            '\n' | ';' => true,
            '&' => {
                if matches!(chars.peek(), Some('&')) {
                    chars.next();
                }
                true
            }
            '|' => {
                if matches!(chars.peek(), Some('|') | Some('&')) {
                    chars.next();
                }
                true
            }
            _ => false,
        };
        if is_separator {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
            current.clear();
        } else {
            current.push(ch);
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }
    parts
}

fn classify_shell_segment(command: &str) -> ShellSafety {
    let trimmed = command.trim();
    if trimmed.is_empty() || contains_unsafe_shell_syntax(trimmed) {
        return ShellSafety::Unknown;
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    match parts.as_slice() {
        ["pwd", ..] | ["ls", ..] | ["cat", ..] | ["head", ..] | ["tail", ..] | ["rg", ..] => {
            ShellSafety::Safe
        }
        ["find", args @ ..] => {
            if args
                .iter()
                .any(|arg| matches!(*arg, "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"))
            {
                ShellSafety::Dangerous
            } else {
                ShellSafety::Safe
            }
        }
        ["git", "status", ..] => ShellSafety::Safe,
        ["git", "diff", args @ ..] => {
            if args
                .iter()
                .any(|arg| *arg == "-o" || arg.starts_with("--output"))
            {
                ShellSafety::Dangerous
            } else {
                ShellSafety::Safe
            }
        }
        ["rm", ..]
        | ["rmdir", ..]
        | ["mv", ..]
        | ["cp", ..]
        | ["chmod", ..]
        | ["chown", ..]
        | ["sudo", ..]
        | ["curl", ..]
        | ["wget", ..]
        | ["git", "push", ..]
        | ["git", "commit", ..]
        | ["git", "reset", ..]
        | ["git", "checkout", ..]
        | ["git", "clean", ..] => ShellSafety::Dangerous,
        _ => ShellSafety::Unknown,
    }
}

fn default_for_shell(permission: PermissionMode, arguments: &Value) -> PolicyDecision {
    let command = shell_command(arguments);
    match (permission, classify_shell_command(command)) {
        (_, ShellSafety::Safe) => decision(
            PermissionDecision::Allow,
            DecisionReason::SafetyCheck,
            Some(RuleSource::RuntimeDefault),
            "shell command classified as safe",
        ),
        (PermissionMode::Safe, ShellSafety::Dangerous | ShellSafety::Unknown) => decision(
            PermissionDecision::Deny,
            DecisionReason::Mode,
            Some(RuleSource::RuntimeDefault),
            format!("shell command denied in safe permission mode: {command}"),
        ),
        (PermissionMode::Ask, _) => decision(
            PermissionDecision::Ask,
            DecisionReason::Mode,
            Some(RuleSource::RuntimeDefault),
            "shell command requires approval in ask permission mode",
        ),
        (PermissionMode::Auto, ShellSafety::Dangerous) => decision(
            PermissionDecision::Deny,
            DecisionReason::SafetyCheck,
            Some(RuleSource::RuntimeDefault),
            format!("shell command classified as dangerous: {command}"),
        ),
        (PermissionMode::Auto, ShellSafety::Unknown) => decision(
            PermissionDecision::Ask,
            DecisionReason::SafetyCheck,
            Some(RuleSource::RuntimeDefault),
            "unknown shell command requires approval in auto permission mode",
        ),
    }
}

fn contains_unsafe_shell_syntax(command: &str) -> bool {
    [">", "<", "$(", "`"]
        .iter()
        .any(|needle| command.contains(needle))
}

fn shell_command(arguments: &Value) -> &str {
    arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn argument_summary(tool_name: &str, arguments: &Value) -> String {
    match tool_name {
        "list_files" => arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_string(),
        "read_file" | "write_file" => arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => arguments.to_string(),
    }
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == value;
    }
    let starts_with_wildcard = pattern.starts_with('*');
    let ends_with_wildcard = pattern.ends_with('*');
    let parts = pattern
        .split('*')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return true;
    }
    let mut remainder = value;
    for (index, part) in parts.iter().enumerate() {
        let Some(pos) = remainder.find(part) else {
            return false;
        };
        if index == 0 && !starts_with_wildcard && pos != 0 {
            return false;
        }
        let next_index = pos + part.len();
        remainder = &remainder[next_index..];
    }
    if !ends_with_wildcard {
        if let Some(last) = parts.last() {
            return value.ends_with(last);
        }
    }
    true
}

fn decision(
    decision_value: PermissionDecision,
    reason: DecisionReason,
    rule_source: Option<RuleSource>,
    message: impl Into<String>,
) -> PolicyDecision {
    PolicyDecision {
        decision: decision_value,
        reason,
        rule_source,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata(name: &'static str) -> ToolMetadata {
        ToolMetadata {
            name,
            read_only: matches!(name, "list_files" | "read_file"),
            destructive: name == "write_file",
            concurrency_safe: matches!(name, "list_files" | "read_file"),
            argument_summary: String::new(),
            permission_hint: PermissionDecision::Ask,
        }
    }

    #[test]
    fn parses_whole_tool_and_scoped_rules() {
        assert_eq!(
            PermissionRule::parse(RuleSource::Config, RuleBehavior::Allow, "shell")
                .unwrap()
                .specifier,
            None
        );
        assert_eq!(
            PermissionRule::parse(RuleSource::Config, RuleBehavior::Allow, "shell(*)")
                .unwrap()
                .specifier,
            None
        );
        assert_eq!(
            PermissionRule::parse(RuleSource::Config, RuleBehavior::Deny, "shell(rm *)")
                .unwrap()
                .specifier,
            Some("rm *".into())
        );
        assert!(
            PermissionRule::parse(RuleSource::Config, RuleBehavior::Deny, "shell(rm *").is_err()
        );
    }

    #[test]
    fn wildcard_matcher_covers_basic_shapes() {
        assert!(wildcard_match("cargo test", "cargo test"));
        assert!(wildcard_match("cargo *", "cargo test --all"));
        assert!(wildcard_match("*test", "cargo test"));
        assert!(wildcard_match("cargo*--all", "cargo test --all"));
        assert!(!wildcard_match("cargo build", "cargo test"));
    }

    #[test]
    fn precedence_denies_before_ask_and_allows_before_mode_default() {
        let engine = PolicyEngine::new(vec![
            PermissionRule::parse(RuleSource::Config, RuleBehavior::Ask, "shell(rm *)").unwrap(),
            PermissionRule::parse(RuleSource::Config, RuleBehavior::Deny, "shell(rm -rf *)")
                .unwrap(),
            PermissionRule::parse(RuleSource::Config, RuleBehavior::Allow, "write_file").unwrap(),
        ]);
        assert_eq!(
            engine
                .decide(
                    PermissionMode::Auto,
                    &metadata("shell"),
                    "shell",
                    &json!({"command":"rm -rf x"})
                )
                .decision,
            PermissionDecision::Deny
        );
        assert_eq!(
            engine
                .decide(
                    PermissionMode::Ask,
                    &metadata("write_file"),
                    "write_file",
                    &json!({"path":"x"})
                )
                .decision,
            PermissionDecision::Allow
        );
    }

    #[test]
    fn shell_policy_handles_compound_segments() {
        assert!(is_safe_shell_command("pwd && ls -la"));
        assert_eq!(
            classify_shell_command("pwd && rm -rf x"),
            ShellSafety::Dangerous
        );

        let engine = PolicyEngine::new(vec![
            PermissionRule::parse(
                RuleSource::Config,
                RuleBehavior::Allow,
                "shell(cargo test*)",
            )
            .unwrap(),
            PermissionRule::parse(
                RuleSource::Config,
                RuleBehavior::Allow,
                "shell(cargo build*)",
            )
            .unwrap(),
        ]);
        assert_eq!(
            engine
                .decide(
                    PermissionMode::Ask,
                    &metadata("shell"),
                    "shell",
                    &json!({"command":"cargo test --all && cargo build"})
                )
                .decision,
            PermissionDecision::Allow
        );
        assert_eq!(
            engine
                .decide(
                    PermissionMode::Ask,
                    &metadata("shell"),
                    "shell",
                    &json!({"command":"cargo test --all && cargo fmt"})
                )
                .decision,
            PermissionDecision::Ask
        );
    }

    #[test]
    fn whole_tool_deny_hides_schema_but_scoped_deny_does_not() {
        let whole = PolicyEngine::new(vec![PermissionRule::parse(
            RuleSource::Config,
            RuleBehavior::Deny,
            "shell",
        )
        .unwrap()]);
        assert!(whole.hides_tool_schema(PermissionMode::Ask, "shell"));

        let scoped = PolicyEngine::new(vec![PermissionRule::parse(
            RuleSource::Config,
            RuleBehavior::Deny,
            "shell(rm *)",
        )
        .unwrap()]);
        assert!(!scoped.hides_tool_schema(PermissionMode::Ask, "shell"));
    }
}
