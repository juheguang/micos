mod builtin;
mod path;
mod policy;
mod types;

pub use builtin::{BuiltinTool, BuiltinToolRegistry};
pub use path::{resolve_under_cwd, truncate_text};
pub use policy::{is_safe_shell_command, permission_decision};
pub use types::{
    ModePermissionPolicy, PermissionPolicy, Tool, ToolContext, ToolPermissionDecision,
    ToolRegistry, ToolResult, ToolSummary,
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
