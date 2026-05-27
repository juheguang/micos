use serde_json::Value;

pub(super) fn suggest_approval_rule(tool: &str, arguments: &Value) -> String {
    match tool {
        "write_file" => suggest_write_file_rule(arguments),
        "shell" => suggest_shell_rule(arguments),
        _ => tool.to_string(),
    }
}

fn suggest_write_file_rule(arguments: &Value) -> String {
    let path = arguments
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .trim_start_matches("./");
    if path.is_empty() || path.contains(['(', ')']) {
        return "write_file".into();
    }
    let first = path.split('/').next().unwrap_or(path);
    if matches!(first, "src" | "tests" | "docs" | "assets") && path.contains('/') {
        format!("write_file({first}/*)")
    } else {
        format!("write_file({path})")
    }
}

fn suggest_shell_rule(arguments: &Value) -> String {
    let command = arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if command.is_empty() || command.contains(['(', ')']) {
        return "shell".into();
    }
    if command.starts_with("git status") {
        return "shell(git status*)".into();
    }
    if command.starts_with("git diff") {
        return "shell(git diff*)".into();
    }
    format!("shell({command})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn suggests_permission_rules_for_common_tools() {
        assert_eq!(
            suggest_approval_rule("write_file", &json!({"path":"src/agent.rs"})),
            "write_file(src/*)"
        );
        assert_eq!(
            suggest_approval_rule("write_file", &json!({"path":"README.md"})),
            "write_file(README.md)"
        );
        assert_eq!(
            suggest_approval_rule("shell", &json!({"command":"git diff -- src/agent.rs"})),
            "shell(git diff*)"
        );
        assert_eq!(
            suggest_approval_rule("shell", &json!({"command":"cargo test"})),
            "shell(cargo test)"
        );
    }
}
