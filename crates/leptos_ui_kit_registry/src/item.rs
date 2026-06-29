use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{LEPTOS_ROUTER_VERSION, LEPTOS_VERSION, RenderMode, SCHEMA_VERSION};

pub const REGISTRY_SCHEMA_URL: &str =
    "https://leptos-ui-kit.dev/schema/0.9.0-alpha/registry.schema.json";
pub const REGISTRY_ITEM_SCHEMA_URL: &str =
    "https://leptos-ui-kit.dev/schema/0.9.0-alpha/registry-item.schema.json";

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
    LocalRegistryUnsupported(String),
    InvalidValue {
        field: &'static str,
        expected: String,
        actual: String,
    },
    UnsafePath {
        field: &'static str,
        path: String,
    },
    DuplicateTarget(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "failed to read {}: {source}", path.display()),
            Self::Parse { path, source } => {
                write!(
                    f,
                    "failed to parse registry file {}: {source}",
                    path.display()
                )
            }
            Self::BuiltInNotFound(name) => write!(f, "built-in registry item not found: {name}"),
            Self::LocalRegistryUnsupported(source) => {
                write!(
                    f,
                    "local registry sources are not supported in MVP: {source}"
                )
            }
            Self::InvalidValue {
                field,
                expected,
                actual,
            } => write!(
                f,
                "invalid registry value for {field}: expected {expected}, got {actual}"
            ),
            Self::UnsafePath { field, path } => {
                write!(f, "unsafe registry path for {field}: {path}")
            }
            Self::DuplicateTarget(target) => write!(f, "duplicate registry target: {target}"),
        }
    }
}

impl std::error::Error for RegistryError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryRoot {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: String,
    pub name: String,
    pub items: Vec<RegistryRootItem>,
}

impl RegistryRoot {
    pub fn validate(&self) -> Result<(), RegistryError> {
        expect_string("$schema", REGISTRY_SCHEMA_URL, &self.schema)?;
        expect_string("schemaVersion", SCHEMA_VERSION, &self.schema_version)?;
        expect_string("name", "leptos-ui-kit", &self.name)?;

        let mut names = BTreeSet::new();
        for item in &self.items {
            validate_item_name(&item.name)?;
            validate_registry_source_path("items[].path", &item.path)?;
            if !names.insert(item.name.clone()) {
                return Err(RegistryError::DuplicateTarget(format!(
                    "item:{}",
                    item.name
                )));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryRootItem {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryItem {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: String,
    pub name: String,
    pub kind: RegistryItemKind,
    pub version: String,
    pub title: String,
    pub description: String,
    pub leptos: RegistryLeptos,
    pub files: Vec<RegistryItemFile>,
    pub styles: Vec<RegistryItemStyle>,
    #[serde(default)]
    pub registry_dependencies: Vec<String>,
    pub cargo_plan: Vec<CargoPlanEntry>,
    #[serde(default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl RegistryItem {
    pub fn validate(&self) -> Result<(), RegistryError> {
        expect_string("$schema", REGISTRY_ITEM_SCHEMA_URL, &self.schema)?;
        expect_string("schemaVersion", SCHEMA_VERSION, &self.schema_version)?;
        validate_item_name(&self.name)?;
        expect_string("version", SCHEMA_VERSION, &self.version)?;
        self.leptos.validate()?;

        let mut targets = BTreeSet::new();
        for file in &self.files {
            file.validate()?;
            if !targets.insert(format!("ui:{}", file.target.path)) {
                return Err(RegistryError::DuplicateTarget(file.target.path.clone()));
            }
        }

        for style in &self.styles {
            style.validate(&self.name)?;
            if !targets.insert(format!("css-block:{}", style.target.id)) {
                return Err(RegistryError::DuplicateTarget(style.target.id.clone()));
            }
        }

        for dependency in &self.registry_dependencies {
            validate_item_name(dependency)?;
        }

        for entry in &self.cargo_plan {
            entry.validate()?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryItemKind {
    Ui,
}

impl fmt::Display for RegistryItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ui => write!(f, "ui"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryLeptos {
    pub version: String,
    pub router_version: String,
    pub render_mode: RenderMode,
}

impl RegistryLeptos {
    fn validate(&self) -> Result<(), RegistryError> {
        expect_string("leptos.version", LEPTOS_VERSION, &self.version)?;
        expect_string(
            "leptos.routerVersion",
            LEPTOS_ROUTER_VERSION,
            &self.router_version,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryItemFile {
    pub source: String,
    pub target: RegistryFileTarget,
}

impl RegistryItemFile {
    fn validate(&self) -> Result<(), RegistryError> {
        validate_registry_source_path("files[].source", &self.source)?;
        self.target.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryFileTarget {
    pub kind: RegistryFileTargetKind,
    pub path: String,
}

impl RegistryFileTarget {
    fn validate(&self) -> Result<(), RegistryError> {
        validate_safe_file_name("files[].target.path", &self.path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryFileTargetKind {
    Ui,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryItemStyle {
    pub source: String,
    pub target: RegistryStyleTarget,
}

impl RegistryItemStyle {
    fn validate(&self, item_name: &str) -> Result<(), RegistryError> {
        validate_registry_source_path("styles[].source", &self.source)?;
        self.target.validate(item_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryStyleTarget {
    pub kind: RegistryStyleTargetKind,
    pub id: String,
}

impl RegistryStyleTarget {
    fn validate(&self, item_name: &str) -> Result<(), RegistryError> {
        expect_string("styles[].target.id", item_name, &self.id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryStyleTargetKind {
    ManagedCssBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CargoPlanEntry {
    #[serde(rename = "crate")]
    pub crate_name: String,
    pub version: String,
    pub required: bool,
}

impl CargoPlanEntry {
    fn validate(&self) -> Result<(), RegistryError> {
        match self.crate_name.as_str() {
            "leptos" => expect_string("cargoPlan[].version", LEPTOS_VERSION, &self.version),
            "leptos_router" => {
                expect_string("cargoPlan[].version", LEPTOS_ROUTER_VERSION, &self.version)
            }
            value => Err(RegistryError::InvalidValue {
                field: "cargoPlan[].crate",
                expected: "leptos or leptos_router".to_owned(),
                actual: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistrySourceKind {
    BuiltIn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRegistryItem {
    pub source_kind: RegistrySourceKind,
    pub source_path: PathBuf,
    pub item: RegistryItem,
}

pub fn parse_registry_root_str(input: &str) -> Result<RegistryRoot, serde_json::Error> {
    serde_json::from_str(input)
}

pub fn parse_registry_item_str(input: &str) -> Result<RegistryItem, serde_json::Error> {
    serde_json::from_str(input)
}

pub fn load_registry_item(
    source: &str,
    _cwd: &Path,
) -> Result<ResolvedRegistryItem, RegistryError> {
    if source.trim().is_empty() {
        return Err(RegistryError::BuiltInNotFound(source.to_owned()));
    }

    if looks_like_local_path(source) {
        return Err(RegistryError::LocalRegistryUnsupported(source.to_owned()));
    }

    load_built_in_registry_item(source)
}

pub fn load_built_in_registry_root() -> Result<RegistryRoot, RegistryError> {
    let path = built_in_registry_root().join("registry.json");
    parse_registry_root_file(&path)
}

pub fn load_built_in_registry_item(name: &str) -> Result<ResolvedRegistryItem, RegistryError> {
    let root = load_built_in_registry_root()?;
    let Some(entry) = root.items.iter().find(|item| item.name == name) else {
        return Err(RegistryError::BuiltInNotFound(name.to_owned()));
    };

    let path = built_in_registry_root().join(&entry.path);
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

fn parse_registry_root_file(path: &Path) -> Result<RegistryRoot, RegistryError> {
    let input = read_to_string(path)?;
    let root = parse_registry_root_str(&input).map_err(|source| RegistryError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    root.validate()?;
    Ok(root)
}

fn parse_registry_item_file(path: &Path) -> Result<RegistryItem, RegistryError> {
    let input = read_to_string(path)?;
    let item = parse_registry_item_str(&input).map_err(|source| RegistryError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    item.validate()?;
    Ok(item)
}

fn read_to_string(path: &Path) -> Result<String, RegistryError> {
    fs::read_to_string(path).map_err(|source| RegistryError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn expect_string(field: &'static str, expected: &str, actual: &str) -> Result<(), RegistryError> {
    if actual == expected {
        Ok(())
    } else {
        Err(RegistryError::InvalidValue {
            field,
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        })
    }
}

fn validate_item_name(value: &str) -> Result<(), RegistryError> {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        Ok(())
    } else {
        Err(RegistryError::InvalidValue {
            field: "name",
            expected: "ASCII lowercase kebab-case item name".to_owned(),
            actual: value.to_owned(),
        })
    }
}

fn validate_registry_source_path(field: &'static str, value: &str) -> Result<(), RegistryError> {
    let path = Path::new(value);
    if path.is_absolute()
        || value.starts_with('.')
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(RegistryError::UnsafePath {
            field,
            path: value.to_owned(),
        });
    }

    Ok(())
}

fn validate_safe_file_name(field: &'static str, value: &str) -> Result<(), RegistryError> {
    if value.is_empty()
        || value.starts_with('.')
        || value.contains('/')
        || value.contains('\\')
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'.'
        })
    {
        return Err(RegistryError::UnsafePath {
            field,
            path: value.to_owned(),
        });
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("registry")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_built_in_registry_root() {
        let root = load_built_in_registry_root().expect("load root");

        assert_eq!(root.schema_version, SCHEMA_VERSION);
        assert_eq!(root.items.len(), 1);
        assert_eq!(root.items[0].name, "button");
    }

    #[test]
    fn loads_built_in_registry_item() {
        let resolved = load_built_in_registry_item("button").expect("load button");

        assert_eq!(resolved.source_kind, RegistrySourceKind::BuiltIn);
        assert_eq!(resolved.item.name, "button");
        assert_eq!(resolved.item.kind, RegistryItemKind::Ui);
        assert_eq!(resolved.item.files[0].target.path, "button.rs");
        assert_eq!(resolved.item.styles[0].target.id, "button");
    }

    #[test]
    fn rejects_local_registry_item_sources() {
        let error =
            load_registry_item("./card.json", Path::new(".")).expect_err("local path should fail");

        assert!(matches!(error, RegistryError::LocalRegistryUnsupported(_)));
    }

    #[test]
    fn rejects_unknown_built_in_item_field() {
        let error = parse_registry_item_str(
            r#"{
              "$schema": "https://leptos-ui-kit.dev/schema/0.9.0-alpha/registry-item.schema.json",
              "schemaVersion": "0.9.0-alpha",
              "name": "button",
              "kind": "ui",
              "version": "0.9.0-alpha",
              "title": "Button",
              "description": "A pure-CSS Leptos button component.",
              "unexpected": true,
              "leptos": {
                "version": "0.9.0-alpha",
                "routerVersion": "0.9.0-alpha",
                "renderMode": "csr"
              },
              "files": [],
              "styles": [],
              "registryDependencies": [],
              "cargoPlan": [],
              "extra": {}
            }"#,
        )
        .expect_err("unknown field should fail");

        assert!(error.is_data());
    }

    #[test]
    fn rejects_unsafe_target_path() {
        let item = parse_registry_item_str(
            r#"{
              "$schema": "https://leptos-ui-kit.dev/schema/0.9.0-alpha/registry-item.schema.json",
              "schemaVersion": "0.9.0-alpha",
              "name": "button",
              "kind": "ui",
              "version": "0.9.0-alpha",
              "title": "Button",
              "description": "A pure-CSS Leptos button component.",
              "leptos": {
                "version": "0.9.0-alpha",
                "routerVersion": "0.9.0-alpha",
                "renderMode": "csr"
              },
              "files": [
                {
                  "source": "ui/button.rs",
                  "target": {
                    "kind": "ui",
                    "path": "../button.rs"
                  }
                }
              ],
              "styles": [],
              "registryDependencies": [],
              "cargoPlan": [],
              "extra": {}
            }"#,
        )
        .expect("parse raw item");

        let error = item.validate().expect_err("unsafe path should fail");

        assert!(matches!(error, RegistryError::UnsafePath { .. }));
    }
}
