use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum RegistryError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    BuiltInNotFound(String),
    LocalPathNotFound(PathBuf),
    MissingSource,
    MissingTarget {
        item: String,
        file_path: String,
        kind: RegistryItemType,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "failed to read {}: {source}", path.display()),
            Self::Parse { path, source } => {
                write!(
                    f,
                    "failed to parse registry item {}: {source}",
                    path.display()
                )
            }
            Self::BuiltInNotFound(name) => write!(f, "built-in registry item not found: {name}"),
            Self::LocalPathNotFound(path) => {
                write!(f, "registry item path not found: {}", path.display())
            }
            Self::MissingSource => write!(f, "registry source is required"),
            Self::MissingTarget {
                item,
                file_path,
                kind,
            } => write!(
                f,
                "registry item {item} requires target for {kind} file {file_path}"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistryItemType {
    #[serde(rename = "registry:item")]
    Item,
    #[serde(rename = "registry:block")]
    Block,
    #[serde(rename = "registry:component")]
    Component,
    #[serde(rename = "registry:ui")]
    Ui,
    #[serde(rename = "registry:hook")]
    Hook,
    #[serde(rename = "registry:lib")]
    Lib,
    #[serde(rename = "registry:file")]
    File,
    #[serde(rename = "registry:page")]
    Page,
    #[serde(rename = "registry:theme")]
    Theme,
    #[serde(rename = "registry:style")]
    Style,
    #[serde(rename = "registry:base")]
    Base,
    #[serde(rename = "registry:font")]
    Font,
    #[serde(rename = "registry:example")]
    Example,
    #[serde(rename = "registry:internal")]
    Internal,
}

impl fmt::Display for RegistryItemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Item => "registry:item",
            Self::Block => "registry:block",
            Self::Component => "registry:component",
            Self::Ui => "registry:ui",
            Self::Hook => "registry:hook",
            Self::Lib => "registry:lib",
            Self::File => "registry:file",
            Self::Page => "registry:page",
            Self::Theme => "registry:theme",
            Self::Style => "registry:style",
            Self::Base => "registry:base",
            Self::Font => "registry:font",
            Self::Example => "registry:example",
            Self::Internal => "registry:internal",
        };

        write!(f, "{value}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryItemFile {
    pub path: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(rename = "type")]
    pub kind: RegistryItemType,
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryItem {
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    pub extends: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: RegistryItemType,
    pub title: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub dev_dependencies: Vec<String>,
    #[serde(default)]
    pub registry_dependencies: Vec<String>,
    #[serde(default)]
    pub files: Vec<RegistryItemFile>,
    #[serde(default)]
    pub docs: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistrySourceKind {
    BuiltIn,
    LocalPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRegistryItem {
    pub source_kind: RegistrySourceKind,
    pub source_path: PathBuf,
    pub item: RegistryItem,
}

pub fn parse_registry_item_str(input: &str) -> Result<RegistryItem, serde_json::Error> {
    serde_json::from_str(input)
}

pub fn load_registry_item(source: &str, cwd: &Path) -> Result<ResolvedRegistryItem, RegistryError> {
    if source.trim().is_empty() {
        return Err(RegistryError::MissingSource);
    }

    if looks_like_local_path(source) {
        return load_local_registry_item(&cwd.join(source));
    }

    load_built_in_registry_item(source)
}

pub fn load_local_registry_item(path: &Path) -> Result<ResolvedRegistryItem, RegistryError> {
    if !path.is_file() {
        return Err(RegistryError::LocalPathNotFound(path.to_path_buf()));
    }

    let item = parse_registry_item_file(path)?;

    Ok(ResolvedRegistryItem {
        source_kind: RegistrySourceKind::LocalPath,
        source_path: path.to_path_buf(),
        item,
    })
}

pub fn load_built_in_registry_item(name: &str) -> Result<ResolvedRegistryItem, RegistryError> {
    let path = built_in_registry_root().join(format!("{name}.json"));
    if !path.is_file() {
        return Err(RegistryError::BuiltInNotFound(name.to_owned()));
    }

    let item = parse_registry_item_file(&path)?;

    Ok(ResolvedRegistryItem {
        source_kind: RegistrySourceKind::BuiltIn,
        source_path: path,
        item,
    })
}

fn parse_registry_item_file(path: &Path) -> Result<RegistryItem, RegistryError> {
    let input = fs::read_to_string(path).map_err(|source| RegistryError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let item = parse_registry_item_str(&input).map_err(|source| RegistryError::Parse {
        path: path.to_path_buf(),
        source,
    })?;

    validate_registry_item(&item)?;

    Ok(item)
}

fn validate_registry_item(item: &RegistryItem) -> Result<(), RegistryError> {
    for file in &item.files {
        if matches!(file.kind, RegistryItemType::File | RegistryItemType::Page)
            && file.target.is_none()
        {
            return Err(RegistryError::MissingTarget {
                item: item.name.clone(),
                file_path: file.path.clone(),
                kind: file.kind,
            });
        }
    }

    Ok(())
}

fn looks_like_local_path(source: &str) -> bool {
    source.ends_with(".json")
        || source.contains(std::path::MAIN_SEPARATOR)
        || source.contains('/')
        || source.starts_with('.')
}

fn built_in_registry_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../registry"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::tempdir;

    #[test]
    fn loads_built_in_registry_item() {
        let resolved = load_built_in_registry_item("button").expect("load button");

        assert_eq!(resolved.source_kind, RegistrySourceKind::BuiltIn);
        assert_eq!(resolved.item.name, "button");
        assert_eq!(resolved.item.kind, RegistryItemType::Ui);
    }

    #[test]
    fn loads_local_registry_item() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("card.json");
        fs::write(
            &path,
            r#"{
              "name": "card",
              "type": "registry:ui",
              "title": "Card",
              "files": [
                {
                  "path": "ui/card.rs",
                  "type": "registry:ui"
                }
              ]
            }"#,
        )
        .expect("write item");

        let resolved = load_registry_item("./card.json", dir.path()).expect("load local path");

        assert_eq!(resolved.source_kind, RegistrySourceKind::LocalPath);
        assert_eq!(resolved.item.name, "card");
        assert_eq!(resolved.item.kind, RegistryItemType::Ui);
    }

    #[test]
    fn rejects_registry_file_without_required_target() {
        let error = parse_registry_item_str(
            r#"{
              "name": "broken",
              "type": "registry:file",
              "files": [
                {
                  "path": "styles/theme.css",
                  "type": "registry:file"
                }
              ]
            }"#,
        )
        .expect("parse raw item");

        let error = validate_registry_item(&error).expect_err("validation should fail");

        assert!(matches!(error, RegistryError::MissingTarget { .. }));
    }
}
