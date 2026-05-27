use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

pub const VERIFY_CONFIG_PATH: &str = ".micos/verify.toml";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationCheck {
    pub name: String,
    pub command: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationRunReport {
    pub checks: Vec<VerificationCheckReport>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationCheckReport {
    pub name: String,
    pub command: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub output_preview: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct VerifyFile {
    #[serde(default)]
    checks: Vec<VerifyCheckFile>,
}

#[derive(Clone, Debug, Deserialize)]
struct VerifyCheckFile {
    name: String,
    command: String,
}

pub fn load_verification_checks(cwd: &Path) -> Result<Vec<VerificationCheck>> {
    let path = cwd.join(VERIFY_CONFIG_PATH);
    if !path.exists() {
        if cwd.join("Cargo.toml").exists() {
            return Ok(vec![VerificationCheck {
                name: "test".into(),
                command: "cargo test".into(),
            }]);
        }
        return Ok(Vec::new());
    }

    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let file: VerifyFile =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    if file.checks.is_empty() {
        bail!("verify config must contain at least one [[checks]] entry");
    }

    let mut names = HashSet::new();
    let mut checks = Vec::new();
    for check in file.checks {
        let name = check.name.trim();
        let command = check.command.trim();
        if name.is_empty() {
            bail!("verify check name cannot be empty");
        }
        if command.is_empty() {
            bail!("verify check command cannot be empty: {name}");
        }
        if !names.insert(name.to_string()) {
            bail!("duplicate verify check: {name}");
        }
        checks.push(VerificationCheck {
            name: name.to_string(),
            command: command.to_string(),
        });
    }
    Ok(checks)
}

pub fn select_verification_checks(
    checks: &[VerificationCheck],
    name: Option<&str>,
) -> Result<Vec<VerificationCheck>> {
    let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(checks.to_vec());
    };
    let selected = checks
        .iter()
        .find(|check| check.name == name)
        .cloned()
        .with_context(|| format!("unknown verification check: {name}"))?;
    Ok(vec![selected])
}

pub fn shell_exit_code(output: &str) -> Option<i32> {
    output
        .lines()
        .find_map(|line| line.strip_prefix("status: "))
        .and_then(|status| status.trim().parse::<i32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_dir() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("micos-verify-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn defaults_to_cargo_test_for_rust_project() {
        let cwd = temp_dir();
        std::fs::write(cwd.join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();

        let checks = load_verification_checks(&cwd).unwrap();

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "test");
        assert_eq!(checks[0].command, "cargo test");
    }

    #[test]
    fn parses_verify_config_and_rejects_duplicates() {
        let cwd = temp_dir();
        std::fs::create_dir_all(cwd.join(".micos")).unwrap();
        std::fs::write(
            cwd.join(VERIFY_CONFIG_PATH),
            "[[checks]]\nname=\"test\"\ncommand=\"cargo test\"\n",
        )
        .unwrap();

        let checks = load_verification_checks(&cwd).unwrap();

        assert_eq!(checks[0].name, "test");
        assert_eq!(checks[0].command, "cargo test");

        std::fs::write(
            cwd.join(VERIFY_CONFIG_PATH),
            "[[checks]]\nname=\"test\"\ncommand=\"cargo test\"\n[[checks]]\nname=\"test\"\ncommand=\"cargo build\"\n",
        )
        .unwrap();
        assert!(load_verification_checks(&cwd)
            .unwrap_err()
            .to_string()
            .contains("duplicate verify check"));
    }

    #[test]
    fn selects_named_check_and_parses_exit_code() {
        let checks = vec![
            VerificationCheck {
                name: "test".into(),
                command: "cargo test".into(),
            },
            VerificationCheck {
                name: "build".into(),
                command: "cargo build".into(),
            },
        ];

        let selected = select_verification_checks(&checks, Some("build")).unwrap();

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].command, "cargo build");
        assert_eq!(shell_exit_code("status: 101\nstdout:\n"), Some(101));
    }
}
