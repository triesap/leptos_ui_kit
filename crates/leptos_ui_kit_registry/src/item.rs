use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{LEPTOS_ROUTER_VERSION, LEPTOS_VERSION, RenderMode, SCHEMA_VERSION};

pub const REGISTRY_SCHEMA_URL: &str =
    "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/registry.schema.json";
pub const REGISTRY_ITEM_SCHEMA_URL: &str =
    "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/registry-item.schema.json";

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
    UnknownDependency {
        item: String,
        dependency: String,
    },
    DependencyCycle(String),
    MissingSource(PathBuf),
    Serialize(serde_json::Error),
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
            Self::UnknownDependency { item, dependency } => {
                write!(
                    f,
                    "registry item {item} depends on unknown item {dependency}"
                )
            }
            Self::DependencyCycle(item) => {
                write!(f, "registry dependency cycle includes item {item}")
            }
            Self::MissingSource(path) => {
                write!(f, "registry source file missing: {}", path.display())
            }
            Self::Serialize(error) => write!(f, "failed to serialize registry metadata: {error}"),
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
    pub content_hash: String,
    pub targets: ResolvedRegistryTargets,
    pub item: RegistryItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRegistryTargets {
    pub ui_files: Vec<ResolvedUiTarget>,
    pub style_blocks: Vec<ResolvedStyleBlockTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedUiTarget {
    pub source: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedStyleBlockTarget {
    pub source: String,
    pub id: String,
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
    validate_registry_graph(std::slice::from_ref(&item))?;
    let targets = resolve_registry_targets(&item)?;
    let content_hash = registry_item_content_hash(&item, &built_in_registry_root())?;

    Ok(ResolvedRegistryItem {
        source_kind: RegistrySourceKind::BuiltIn,
        source_path: path,
        content_hash,
        targets,
        item,
    })
}

pub fn read_built_in_registry_source(source: &str) -> Result<String, RegistryError> {
    validate_registry_source_path("source", source)?;
    read_to_string(&built_in_registry_root().join(source))
}

pub fn resolve_built_in_registry_items(
    names: &[String],
) -> Result<Vec<ResolvedRegistryItem>, RegistryError> {
    let root = load_built_in_registry_root()?;
    let mut items = Vec::new();

    for name in names {
        let Some(entry) = root.items.iter().find(|item| item.name == *name) else {
            return Err(RegistryError::BuiltInNotFound(name.clone()));
        };
        let path = built_in_registry_root().join(&entry.path);
        items.push(parse_registry_item_file(&path)?);
    }

    let order = validate_registry_graph(&items)?;
    order
        .into_iter()
        .map(|name| load_built_in_registry_item(&name))
        .collect()
}

pub fn validate_registry_graph(items: &[RegistryItem]) -> Result<Vec<String>, RegistryError> {
    let mut by_name = BTreeMap::new();
    let mut targets = BTreeSet::new();

    for item in items {
        item.validate()?;
        if by_name.insert(item.name.clone(), item).is_some() {
            return Err(RegistryError::DuplicateTarget(format!(
                "item:{}",
                item.name
            )));
        }

        for file in &item.files {
            if !targets.insert(format!("ui:{}", file.target.path)) {
                return Err(RegistryError::DuplicateTarget(file.target.path.clone()));
            }
        }

        for style in &item.styles {
            if !targets.insert(format!("css-block:{}", style.target.id)) {
                return Err(RegistryError::DuplicateTarget(style.target.id.clone()));
            }
        }
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();

    for item in items {
        visit_item(
            item.name.as_str(),
            &by_name,
            &mut visiting,
            &mut visited,
            &mut order,
        )?;
    }

    Ok(order)
}

fn visit_item(
    name: &str,
    by_name: &BTreeMap<String, &RegistryItem>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    order: &mut Vec<String>,
) -> Result<(), RegistryError> {
    if visited.contains(name) {
        return Ok(());
    }

    if !visiting.insert(name.to_owned()) {
        return Err(RegistryError::DependencyCycle(name.to_owned()));
    }

    let Some(item) = by_name.get(name) else {
        return Err(RegistryError::BuiltInNotFound(name.to_owned()));
    };

    for dependency in &item.registry_dependencies {
        if !by_name.contains_key(dependency) {
            return Err(RegistryError::UnknownDependency {
                item: item.name.clone(),
                dependency: dependency.clone(),
            });
        }
        visit_item(dependency, by_name, visiting, visited, order)?;
    }

    visiting.remove(name);
    visited.insert(name.to_owned());
    order.push(name.to_owned());
    Ok(())
}

pub fn resolve_registry_targets(
    item: &RegistryItem,
) -> Result<ResolvedRegistryTargets, RegistryError> {
    item.validate()?;
    Ok(ResolvedRegistryTargets {
        ui_files: item
            .files
            .iter()
            .map(|file| ResolvedUiTarget {
                source: file.source.clone(),
                path: file.target.path.clone(),
            })
            .collect(),
        style_blocks: item
            .styles
            .iter()
            .map(|style| ResolvedStyleBlockTarget {
                source: style.source.clone(),
                id: style.target.id.clone(),
            })
            .collect(),
    })
}

pub fn registry_item_content_hash(
    item: &RegistryItem,
    registry_root: &Path,
) -> Result<String, RegistryError> {
    item.validate()?;
    let mut hasher = Sha256::new();
    let metadata = serde_json::to_vec(item).map_err(RegistryError::Serialize)?;

    hasher.update(b"leptos-ui-kit-registry-item-v1\0");
    hasher.update((metadata.len() as u64).to_be_bytes());
    hasher.update(&metadata);

    for file in &item.files {
        update_hash_with_source(&mut hasher, registry_root, &file.source)?;
    }

    for style in &item.styles {
        update_hash_with_source(&mut hasher, registry_root, &style.source)?;
    }

    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn update_hash_with_source(
    hasher: &mut Sha256,
    registry_root: &Path,
    source: &str,
) -> Result<(), RegistryError> {
    validate_registry_source_path("source", source)?;
    let path = registry_root.join(source);
    if !path.is_file() {
        return Err(RegistryError::MissingSource(path));
    }
    let bytes = fs::read(&path).map_err(|source| RegistryError::Io {
        path: path.clone(),
        source,
    })?;

    hasher.update(source.as_bytes());
    hasher.update([0]);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(&bytes);
    Ok(())
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
        assert!(resolved.content_hash.starts_with("sha256:"));
        assert_eq!(resolved.targets.ui_files[0].path, "button.rs");
        assert_eq!(resolved.targets.style_blocks[0].id, "button");
    }

    #[test]
    fn reads_built_in_registry_source() {
        let source = read_built_in_registry_source("ui/button.rs").expect("read source");

        assert!(source.contains("pub fn Button"));
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
              "$schema": "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/registry-item.schema.json",
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
              "$schema": "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/registry-item.schema.json",
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

    #[test]
    fn registry_hash_is_stable_for_built_in_item() {
        let first = load_built_in_registry_item("button").expect("load first");
        let second = load_built_in_registry_item("button").expect("load second");

        assert_eq!(first.content_hash, second.content_hash);
    }

    #[test]
    fn button_source_and_css_are_mvp_pure_css() {
        let root = built_in_registry_root();
        let source = fs::read_to_string(root.join("ui/button.rs")).expect("read button source");
        let css = fs::read_to_string(root.join("styles/button.css")).expect("read button css");

        assert!(source.contains("use leptos::prelude::*;"));
        assert!(source.contains("#[component]"));
        assert!(source.contains("pub fn Button"));
        assert!(source.contains("Children"));
        assert!(source.contains("<button"));
        assert!(!source.contains("leptos_router"));
        assert!(!source.contains("tailwind"));
        assert!(css.contains(".luk-button"));
        assert!(css.contains("--luk-focus-ring"));
        assert!(css.contains(":focus-visible"));
        assert!(!css.contains("@import"));
    }

    #[test]
    fn graph_validates_registry_dependency_order() {
        let dependency = item_with_name_and_target("base", "base.rs", "base", &[]);
        let dependent = item_with_name_and_target("button", "button.rs", "button", &["base"]);

        let order = validate_registry_graph(&[dependent, dependency]).expect("graph");

        assert_eq!(order, vec!["base".to_owned(), "button".to_owned()]);
    }

    #[test]
    fn graph_rejects_unknown_registry_dependencies() {
        let item = item_with_name_and_target("button", "button.rs", "button", &["missing"]);

        let error = validate_registry_graph(&[item]).expect_err("unknown dependency should fail");

        assert!(matches!(error, RegistryError::UnknownDependency { .. }));
    }

    #[test]
    fn graph_rejects_registry_dependency_cycles() {
        let first = item_with_name_and_target("first", "first.rs", "first", &["second"]);
        let second = item_with_name_and_target("second", "second.rs", "second", &["first"]);

        let error = validate_registry_graph(&[first, second]).expect_err("cycle should fail");

        assert!(matches!(error, RegistryError::DependencyCycle(_)));
    }

    #[test]
    fn rejects_duplicate_registry_targets() {
        let first = item_with_name_and_target("first", "button.rs", "first", &[]);
        let second = item_with_name_and_target("second", "button.rs", "second", &[]);

        let error = validate_registry_graph(&[first, second]).expect_err("duplicate should fail");

        assert!(matches!(error, RegistryError::DuplicateTarget(_)));
    }

    fn item_with_name_and_target(
        name: &str,
        file_target: &str,
        style_id: &str,
        dependencies: &[&str],
    ) -> RegistryItem {
        RegistryItem {
            schema: REGISTRY_ITEM_SCHEMA_URL.to_owned(),
            schema_version: SCHEMA_VERSION.to_owned(),
            name: name.to_owned(),
            kind: RegistryItemKind::Ui,
            version: SCHEMA_VERSION.to_owned(),
            title: name.to_owned(),
            description: name.to_owned(),
            leptos: RegistryLeptos {
                version: LEPTOS_VERSION.to_owned(),
                router_version: LEPTOS_ROUTER_VERSION.to_owned(),
                render_mode: RenderMode::Csr,
            },
            files: vec![RegistryItemFile {
                source: format!("ui/{file_target}"),
                target: RegistryFileTarget {
                    kind: RegistryFileTargetKind::Ui,
                    path: file_target.to_owned(),
                },
            }],
            styles: vec![RegistryItemStyle {
                source: format!("styles/{style_id}.css"),
                target: RegistryStyleTarget {
                    kind: RegistryStyleTargetKind::ManagedCssBlock,
                    id: style_id.to_owned(),
                },
            }],
            registry_dependencies: dependencies
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            cargo_plan: vec![],
            extra: BTreeMap::new(),
        }
    }
}
