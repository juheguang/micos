use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

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

pub fn path_has_symlink_component(cwd: &Path, path: &Path) -> Result<bool> {
    let cwd = normalize_path(cwd)?;
    let path = normalize_path(path)?;
    if !path.starts_with(&cwd) {
        bail!("path {} escapes cwd {}", path.display(), cwd.display());
    }

    let Ok(relative) = path.strip_prefix(&cwd) else {
        bail!("path {} escapes cwd {}", path.display(), cwd.display());
    };
    let mut current = cwd;
    for component in relative.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect path {}", current.display()));
            }
        }
    }
    Ok(false)
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
