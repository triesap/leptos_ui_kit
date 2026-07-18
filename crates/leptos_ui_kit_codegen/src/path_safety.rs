use std::path::{Path, PathBuf};

use leptos_ui_kit_registry::DEFAULT_KIT_CONFIG_PATH;

use crate::{CodegenError, DEFAULT_KIT_LOCK_PATH};

pub fn validate_planned_write_paths(paths: &[String]) -> Result<(), CodegenError> {
    let mut seen = std::collections::BTreeSet::new();
    for path in paths {
        validate_logical_write_path(path)?;
        let folded = path.to_ascii_lowercase();
        if !seen.insert(folded) {
            return Err(CodegenError::DuplicatePath(path.clone()));
        }
    }
    Ok(())
}

pub fn validate_project_write_path(
    project_root: &Path,
    logical_path: &str,
) -> Result<PathBuf, CodegenError> {
    validate_logical_write_path(logical_path)?;
    let root = project_root
        .canonicalize()
        .map_err(|source| CodegenError::Io {
            path: project_root.to_path_buf(),
            source,
        })?;
    let full_path = project_root.join(logical_path);
    let existing_parent =
        nearest_existing_parent(&full_path).ok_or_else(|| CodegenError::UnsafePath {
            path: logical_path.to_owned(),
            reason: "no existing parent within project".to_owned(),
        })?;
    let canonical_parent = existing_parent
        .canonicalize()
        .map_err(|source| CodegenError::Io {
            path: existing_parent,
            source,
        })?;

    if !canonical_parent.starts_with(&root) {
        return Err(CodegenError::UnsafePath {
            path: logical_path.to_owned(),
            reason: "path escapes project root through symlink".to_owned(),
        });
    }

    Ok(full_path)
}

pub fn validate_logical_write_path(path: &str) -> Result<(), CodegenError> {
    if path.is_empty() {
        return unsafe_path(path, "path is empty");
    }
    if path.starts_with('/')
        || path.starts_with("//")
        || path.starts_with("\\\\")
        || path.as_bytes().get(1) == Some(&b':')
    {
        return unsafe_path(path, "absolute paths and platform prefixes are rejected");
    }
    if !path.is_ascii() {
        return unsafe_path(path, "path must be ASCII");
    }
    if path.contains('\\') {
        return unsafe_path(path, "backslashes are rejected");
    }

    for component in path.split('/') {
        if component.is_empty() || component == "." {
            return unsafe_path(path, "empty or current-dir segments are rejected");
        }
        if component == ".." {
            return unsafe_path(path, "parent traversal is rejected");
        }
        if component.starts_with('.') {
            return unsafe_path(path, "hidden paths are rejected");
        }
        if !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return unsafe_path(path, "file name contains unsafe characters");
        }
    }

    if is_allowed_write_path(path) {
        Ok(())
    } else {
        unsafe_path(path, "path is outside the MVP write allow-list")
    }
}

fn unsafe_path<T>(path: &str, reason: &str) -> Result<T, CodegenError> {
    Err(CodegenError::UnsafePath {
        path: path.to_owned(),
        reason: reason.to_owned(),
    })
}

fn nearest_existing_parent(path: &Path) -> Option<PathBuf> {
    let mut candidate = path.parent()?;
    loop {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        candidate = candidate.parent()?;
    }
}

fn is_allowed_write_path(path: &str) -> bool {
    matches!(
        path,
        DEFAULT_KIT_CONFIG_PATH | DEFAULT_KIT_LOCK_PATH | "index.html" | "src/components/mod.rs"
    ) || is_allowed_stylesheet_path(path)
        || path.starts_with("src/components/ui/")
}

fn is_allowed_stylesheet_path(path: &str) -> bool {
    path.starts_with("styles/") && path.ends_with(".css")
}
