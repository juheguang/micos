mod builtin;
mod path;
mod policy;
mod types;

pub use builtin::{BuiltinTool, BuiltinToolRegistry};
pub use path::{
    path_has_symlink_component, resolve_under_cwd, truncate_text, truncate_text_with_metadata,
    TruncatedText,
};
pub use policy::{
    classify_shell_command, is_safe_shell_command, parse_permission_rules, split_shell_command,
    DecisionReason, PermissionDecision, PermissionRule, PolicyDecision, PolicyEngine, RuleBehavior,
    RuleSource, ShellSafety,
};
pub use types::{
    ModePermissionPolicy, PermissionPolicy, Tool, ToolContext, ToolErrorKind, ToolMetadata,
    ToolPermissionDecision, ToolRegistry, ToolResult, ToolSummary,
};
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PermissionMode;
    use serde_json::json;
    use std::path::{Path, PathBuf};

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
        assert!(is_safe_shell_command("ls | head"));
        assert!(!is_safe_shell_command("ls | rm -rf x"));
    }

    #[test]
    fn permission_policy_covers_tools_and_modes() {
        let policy = PolicyEngine::default();
        let cwd = std::env::temp_dir();
        let read_meta = BuiltinTool::ReadFile.metadata(&json!({"path":"x"}));
        let write_meta = BuiltinTool::WriteFile.metadata(&json!({"path":"x"}));
        let shell_meta = BuiltinTool::Shell.metadata(&json!({"command":"rm -rf x"}));
        assert_eq!(
            policy
                .decide(
                    PermissionMode::Safe,
                    &read_meta,
                    "read_file",
                    &json!({}),
                    &cwd
                )
                .decision,
            PermissionDecision::Allow
        );
        assert_eq!(
            policy
                .decide(
                    PermissionMode::Safe,
                    &write_meta,
                    "write_file",
                    &json!({"path":"x"}),
                    &cwd
                )
                .decision,
            PermissionDecision::Deny
        );
        assert_eq!(
            policy
                .decide(
                    PermissionMode::Ask,
                    &write_meta,
                    "write_file",
                    &json!({"path":"x"}),
                    &cwd
                )
                .decision,
            PermissionDecision::Ask
        );
        assert_eq!(
            policy
                .decide(
                    PermissionMode::Auto,
                    &shell_meta,
                    "shell",
                    &json!({"command":"rm -rf x"}),
                    &cwd
                )
                .decision,
            PermissionDecision::Deny
        );
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
            vec![
                "list_files",
                "read_file",
                "write_file",
                "shell",
                "grep",
                "edit",
                "glob"
            ]
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
        let preview = truncate_text_with_metadata(text, 3);
        assert!(preview.truncated);
        assert_eq!(preview.original_bytes, 6);
        assert_eq!(preview.preview_bytes, 3);
    }
}
