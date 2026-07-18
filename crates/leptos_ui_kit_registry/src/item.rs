use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    LEPTOS_ROUTER_VERSION, LEPTOS_VERSION, RenderMode, SCHEMA_VERSION, THEME_CONTRACT_VERSION,
};

pub const WEB_UI_PRIMITIVES_VERSION: &str = "0.1.0";

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
        let mut paths = BTreeSet::new();
        for item in &self.items {
            validate_item_name(&item.name)?;
            validate_registry_source_path_with_extension("items[].path", &item.path, "json")?;
            if !names.insert(item.name.clone()) {
                return Err(RegistryError::DuplicateTarget(format!(
                    "item:{}",
                    item.name
                )));
            }
            if !paths.insert(item.path.clone()) {
                return Err(RegistryError::InvalidValue {
                    field: "items[].path",
                    expected: "deduplicated registry item paths".to_owned(),
                    actual: item.path.clone(),
                });
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
    #[serde(default)]
    pub accessibility: RegistryAccessibility,
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
        validate_non_empty_string("title", &self.title)?;
        validate_non_empty_string("description", &self.description)?;
        self.leptos.validate()?;
        self.accessibility.validate()?;

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

        let mut dependencies = BTreeSet::new();
        for dependency in &self.registry_dependencies {
            validate_item_name(dependency)?;
            if !dependencies.insert(dependency) {
                return Err(RegistryError::InvalidValue {
                    field: "registryDependencies",
                    expected: "deduplicated registry item names".to_owned(),
                    actual: dependency.clone(),
                });
            }
        }

        for entry in &self.cargo_plan {
            entry.validate()?;
        }

        match self.kind {
            RegistryItemKind::Ui => Ok(()),
            RegistryItemKind::Foundation => {
                if !self.files.is_empty() {
                    return Err(RegistryError::InvalidValue {
                        field: "files",
                        expected: "no Rust UI files for a foundation item".to_owned(),
                        actual: format!("{} file targets", self.files.len()),
                    });
                }
                if self.styles.is_empty() {
                    return Err(RegistryError::InvalidValue {
                        field: "styles",
                        expected: "at least one managed CSS style for a foundation item".to_owned(),
                        actual: "empty array".to_owned(),
                    });
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryItemKind {
    Ui,
    Foundation,
}

impl fmt::Display for RegistryItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ui => write!(f, "ui"),
            Self::Foundation => write!(f, "foundation"),
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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryAccessibility {
    #[serde(default)]
    pub behaviors: Vec<RegistryAccessibilityBehavior>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl RegistryAccessibility {
    fn validate(&self) -> Result<(), RegistryError> {
        let mut behaviors = BTreeSet::new();
        for behavior in &self.behaviors {
            behavior.validate()?;
            if !behaviors.insert(&behavior.name) {
                return Err(RegistryError::InvalidValue {
                    field: "accessibility.behaviors[].name",
                    expected: "deduplicated behavior names".to_owned(),
                    actual: behavior.name.clone(),
                });
            }
        }
        for note in &self.notes {
            validate_non_empty_string("accessibility.notes[]", note)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryAccessibilityBehavior {
    pub name: String,
    pub required: bool,
}

impl RegistryAccessibilityBehavior {
    fn validate(&self) -> Result<(), RegistryError> {
        validate_kebab_name("accessibility.behaviors[].name", &self.name)
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
        validate_registry_source_path_with_extension("files[].source", &self.source, "rs")?;
        self.target.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryFileTarget {
    pub kind: RegistryFileTargetKind,
    pub path: String,
    #[serde(default)]
    pub exports: Vec<String>,
}

impl RegistryFileTarget {
    fn validate(&self) -> Result<(), RegistryError> {
        validate_ui_target_path("files[].target.path", &self.path)?;
        validate_export_symbols("files[].target.exports", &self.exports)
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
        validate_registry_source_path_with_extension("styles[].source", &self.source, "css")?;
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CargoPlanEntry {
    #[serde(rename = "crate")]
    pub crate_name: String,
    pub source: CargoPlanSource,
    #[serde(default)]
    pub features: Vec<String>,
    pub required: bool,
}

impl CargoPlanEntry {
    fn validate(&self) -> Result<(), RegistryError> {
        self.source.validate()?;
        validate_features("cargoPlan[].features", &self.features)?;

        match self.crate_name.as_str() {
            "leptos" => {
                self.source
                    .expect_version("cargoPlan[].source.version", LEPTOS_VERSION)?;
                expect_features("cargoPlan[].features", &["csr"], &self.features)
            }
            "leptos_router" => {
                self.source
                    .expect_version("cargoPlan[].source.version", LEPTOS_ROUTER_VERSION)?;
                expect_features("cargoPlan[].features", &[], &self.features)
            }
            "web_ui_primitives" => {
                self.source
                    .expect_version("cargoPlan[].source.version", WEB_UI_PRIMITIVES_VERSION)?;
                expect_features("cargoPlan[].features", &["leptos"], &self.features)
            }
            value => Err(RegistryError::InvalidValue {
                field: "cargoPlan[].crate",
                expected: "leptos, leptos_router, or web_ui_primitives".to_owned(),
                actual: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CargoPlanSource {
    pub kind: CargoPlanSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
}

impl CargoPlanSource {
    pub fn version(version: impl Into<String>) -> Self {
        Self {
            kind: CargoPlanSourceKind::Version,
            version: Some(version.into()),
            url: None,
            rev: None,
        }
    }

    pub fn git(url: impl Into<String>, rev: impl Into<String>) -> Self {
        Self {
            kind: CargoPlanSourceKind::Git,
            version: None,
            url: Some(url.into()),
            rev: Some(rev.into()),
        }
    }

    fn validate(&self) -> Result<(), RegistryError> {
        match self.kind {
            CargoPlanSourceKind::Version => {
                if self.url.is_some() || self.rev.is_some() {
                    return Err(RegistryError::InvalidValue {
                        field: "cargoPlan[].source",
                        expected: "version source without url or rev".to_owned(),
                        actual: format!("{self:?}"),
                    });
                }
                if self.version.as_deref().is_none_or(str::is_empty) {
                    return Err(RegistryError::InvalidValue {
                        field: "cargoPlan[].source.version",
                        expected: "non-empty version".to_owned(),
                        actual: String::new(),
                    });
                }
                Ok(())
            }
            CargoPlanSourceKind::Git => {
                if self.version.is_some() {
                    return Err(RegistryError::InvalidValue {
                        field: "cargoPlan[].source",
                        expected: "git source without version".to_owned(),
                        actual: format!("{self:?}"),
                    });
                }
                if self.url.as_deref().is_none_or(str::is_empty) {
                    return Err(RegistryError::InvalidValue {
                        field: "cargoPlan[].source.url",
                        expected: "non-empty git url".to_owned(),
                        actual: String::new(),
                    });
                }
                let rev = self.rev.as_deref().unwrap_or_default();
                validate_git_rev("cargoPlan[].source.rev", rev)
            }
        }
    }

    fn expect_version(&self, field: &'static str, expected: &str) -> Result<(), RegistryError> {
        match (self.kind, self.version.as_deref()) {
            (CargoPlanSourceKind::Version, Some(version)) => {
                expect_string(field, expected, version)
            }
            _ => Err(RegistryError::InvalidValue {
                field,
                expected: expected.to_owned(),
                actual: format!("{self:?}"),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoPlanSourceKind {
    Version,
    Git,
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
    let root = parse_registry_root_file(&path)?;
    validate_built_in_registry_catalog(&root)?;
    Ok(root)
}

pub fn load_built_in_registry_item(name: &str) -> Result<ResolvedRegistryItem, RegistryError> {
    let root = load_built_in_registry_root()?;
    let (item, path) = parse_built_in_item_from_root(&root, name)?;
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    collect_built_in_item_closure(&root, name, &mut seen, &mut items)?;
    validate_registry_graph(&items)?;
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
    let mut seen = BTreeSet::new();

    for name in names {
        collect_built_in_item_closure(&root, name, &mut seen, &mut items)?;
    }

    let order = validate_registry_graph(&items)?;
    order
        .into_iter()
        .map(|name| load_built_in_registry_item(&name))
        .collect()
}

fn parse_built_in_item_from_root(
    root: &RegistryRoot,
    name: &str,
) -> Result<(RegistryItem, PathBuf), RegistryError> {
    let Some(entry) = root.items.iter().find(|item| item.name == name) else {
        return Err(RegistryError::BuiltInNotFound(name.to_owned()));
    };

    let path = built_in_registry_root().join(&entry.path);
    if !path.is_file() {
        return Err(RegistryError::BuiltInNotFound(name.to_owned()));
    }

    let item = parse_registry_item_file(&path)?;
    validate_registry_root_item_identity(entry, &item)?;

    Ok((item, path))
}

fn validate_built_in_registry_catalog(root: &RegistryRoot) -> Result<(), RegistryError> {
    let mut items = Vec::with_capacity(root.items.len());
    for entry in &root.items {
        items.push(parse_built_in_item_from_root(root, &entry.name)?.0);
    }

    validate_built_in_registry_items(&items)
}

pub(crate) fn validate_built_in_registry_items(
    items: &[RegistryItem],
) -> Result<(), RegistryError> {
    validate_registry_graph(items)?;

    let tokens = items
        .iter()
        .find(|item| item.name == "tokens")
        .ok_or_else(|| RegistryError::BuiltInNotFound("tokens".to_owned()))?;
    validate_built_in_tokens_item(tokens)?;

    let router_link = items
        .iter()
        .find(|item| item.name == "router-link")
        .ok_or_else(|| RegistryError::BuiltInNotFound("router-link".to_owned()))?;
    if router_link.registry_dependencies != ["anchor"] {
        return Err(RegistryError::InvalidValue {
            field: "router-link.registryDependencies",
            expected: "anchor".to_owned(),
            actual: router_link.registry_dependencies.join(", "),
        });
    }

    for item in items {
        let direct_tokens_count = item
            .registry_dependencies
            .iter()
            .filter(|dependency| dependency.as_str() == "tokens")
            .count();
        let expected_count = usize::from(item.name != "tokens" && !item.styles.is_empty());
        if direct_tokens_count != expected_count {
            return Err(RegistryError::InvalidValue {
                field: "registryDependencies",
                expected: if expected_count == 1 {
                    "exactly one direct tokens dependency for a styled non-token item".to_owned()
                } else {
                    "no direct tokens dependency for tokens or an unstyled item".to_owned()
                },
                actual: format!(
                    "{} direct tokens dependencies on {}",
                    direct_tokens_count, item.name
                ),
            });
        }
    }

    Ok(())
}

fn validate_built_in_tokens_item(item: &RegistryItem) -> Result<(), RegistryError> {
    let mut extra = BTreeMap::new();
    extra.insert(
        "themeContractVersion".to_owned(),
        serde_json::Value::String(THEME_CONTRACT_VERSION.to_owned()),
    );
    let expected = RegistryItem {
        schema: REGISTRY_ITEM_SCHEMA_URL.to_owned(),
        schema_version: SCHEMA_VERSION.to_owned(),
        name: "tokens".to_owned(),
        kind: RegistryItemKind::Foundation,
        version: SCHEMA_VERSION.to_owned(),
        title: "Semantic Tokens".to_owned(),
        description: "The shared semantic CSS token foundation for all styled leptos_ui_kit items."
            .to_owned(),
        leptos: RegistryLeptos {
            version: LEPTOS_VERSION.to_owned(),
            router_version: LEPTOS_ROUTER_VERSION.to_owned(),
            render_mode: RenderMode::Csr,
        },
        accessibility: RegistryAccessibility::default(),
        files: Vec::new(),
        styles: vec![RegistryItemStyle {
            source: "styles/tokens.css".to_owned(),
            target: RegistryStyleTarget {
                kind: RegistryStyleTargetKind::ManagedCssBlock,
                id: "tokens".to_owned(),
            },
        }],
        registry_dependencies: Vec::new(),
        cargo_plan: Vec::new(),
        extra,
    };

    if item == &expected {
        Ok(())
    } else {
        Err(RegistryError::InvalidValue {
            field: "built-in tokens manifest",
            expected: format!("{expected:?}"),
            actual: format!("{item:?}"),
        })
    }
}

fn collect_built_in_item_closure(
    root: &RegistryRoot,
    name: &str,
    seen: &mut BTreeSet<String>,
    items: &mut Vec<RegistryItem>,
) -> Result<(), RegistryError> {
    if !seen.insert(name.to_owned()) {
        return Ok(());
    }

    let (item, _) = parse_built_in_item_from_root(root, name)?;
    for dependency in &item.registry_dependencies {
        collect_built_in_item_closure(root, dependency, seen, items)?;
    }
    items.push(item);
    Ok(())
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
    validate_kebab_name("name", value)
}

fn validate_kebab_name(field: &'static str, value: &str) -> Result<(), RegistryError> {
    let mut bytes = value.bytes();
    let valid = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');

    if valid {
        Ok(())
    } else {
        Err(RegistryError::InvalidValue {
            field,
            expected: "ASCII lowercase kebab-case name beginning with a letter".to_owned(),
            actual: value.to_owned(),
        })
    }
}

fn validate_registry_source_path(field: &'static str, value: &str) -> Result<(), RegistryError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || value.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
        })
        || value
            .split('/')
            .any(|segment| segment.is_empty() || segment.starts_with('.'))
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(RegistryError::UnsafePath {
            field,
            path: value.to_owned(),
        });
    }

    Ok(())
}

fn validate_registry_source_path_with_extension(
    field: &'static str,
    value: &str,
    extension: &str,
) -> Result<(), RegistryError> {
    validate_registry_source_path(field, value)?;
    if Path::new(value)
        .extension()
        .and_then(|value| value.to_str())
        != Some(extension)
    {
        return Err(RegistryError::UnsafePath {
            field,
            path: value.to_owned(),
        });
    }

    Ok(())
}

fn validate_registry_root_item_identity(
    entry: &RegistryRootItem,
    item: &RegistryItem,
) -> Result<(), RegistryError> {
    expect_string("items[].name", &entry.name, &item.name)
}

fn validate_ui_target_path(field: &'static str, value: &str) -> Result<(), RegistryError> {
    let path = Path::new(value);
    if value == "mod.rs"
        || value.is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || !value.ends_with(".rs")
    {
        return Err(RegistryError::UnsafePath {
            field,
            path: value.to_owned(),
        });
    }

    let mut segments = value.split('/').collect::<Vec<_>>();
    let Some(file_name) = segments.pop() else {
        return Err(RegistryError::UnsafePath {
            field,
            path: value.to_owned(),
        });
    };
    let Some(module_name) = file_name.strip_suffix(".rs") else {
        return Err(RegistryError::UnsafePath {
            field,
            path: value.to_owned(),
        });
    };

    for segment in segments {
        validate_rust_module_segment(field, segment).map_err(|_| RegistryError::UnsafePath {
            field,
            path: value.to_owned(),
        })?;
    }

    if module_name != "mod" {
        validate_rust_module_segment(field, module_name).map_err(|_| {
            RegistryError::UnsafePath {
                field,
                path: value.to_owned(),
            }
        })?;
    }

    Ok(())
}

fn validate_export_symbols(field: &'static str, symbols: &[String]) -> Result<(), RegistryError> {
    let mut seen = BTreeSet::new();
    for symbol in symbols {
        validate_rust_identifier(field, symbol)?;
        if !seen.insert(symbol) {
            return Err(RegistryError::InvalidValue {
                field,
                expected: "deduplicated Rust export symbols".to_owned(),
                actual: symbol.clone(),
            });
        }
    }
    Ok(())
}

fn validate_rust_identifier(field: &'static str, value: &str) -> Result<(), RegistryError> {
    if value.is_empty()
        || value.as_bytes()[0].is_ascii_digit()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(RegistryError::InvalidValue {
            field,
            expected: "ASCII Rust identifier".to_owned(),
            actual: value.to_owned(),
        });
    }

    Ok(())
}

fn validate_rust_module_segment(field: &'static str, value: &str) -> Result<(), RegistryError> {
    if value.is_empty()
        || value.as_bytes()[0].is_ascii_digit()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(RegistryError::InvalidValue {
            field,
            expected: "ASCII lowercase Rust module segment".to_owned(),
            actual: value.to_owned(),
        });
    }

    Ok(())
}

fn validate_non_empty_string(field: &'static str, value: &str) -> Result<(), RegistryError> {
    if value.trim().is_empty() {
        return Err(RegistryError::InvalidValue {
            field,
            expected: "non-empty string".to_owned(),
            actual: value.to_owned(),
        });
    }
    Ok(())
}

fn validate_features(field: &'static str, features: &[String]) -> Result<(), RegistryError> {
    let mut seen = BTreeSet::new();
    for feature in features {
        if feature.is_empty()
            || !feature.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'_' | b'-' | b'+' | b'.' | b'/' | b':')
            })
        {
            return Err(RegistryError::InvalidValue {
                field,
                expected: "ASCII Cargo feature names".to_owned(),
                actual: feature.clone(),
            });
        }

        if !seen.insert(feature) {
            return Err(RegistryError::InvalidValue {
                field,
                expected: "deduplicated Cargo feature names".to_owned(),
                actual: feature.clone(),
            });
        }
    }

    Ok(())
}

fn expect_features(
    field: &'static str,
    expected: &[&str],
    actual: &[String],
) -> Result<(), RegistryError> {
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    let actual = actual.iter().map(String::as_str).collect::<BTreeSet<_>>();

    if actual == expected {
        Ok(())
    } else {
        Err(RegistryError::InvalidValue {
            field,
            expected: expected.into_iter().collect::<Vec<_>>().join(", "),
            actual: actual.into_iter().collect::<Vec<_>>().join(", "),
        })
    }
}

fn validate_git_rev(field: &'static str, rev: &str) -> Result<(), RegistryError> {
    if (7..=40).contains(&rev.len()) && rev.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(RegistryError::InvalidValue {
            field,
            expected: "7 to 40 hex characters".to_owned(),
            actual: rev.to_owned(),
        })
    }
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
    use std::process::Command;

    use super::*;
    use web_ui_primitives::{
        core::{Direction, MenuLoop, MenuModel},
        leptos::{
            DomAttribute, DomAttributeValue,
            attrs::{
                MenuItemAttrs, MenuItemKind as AttrsMenuItemKind, MenuTriggerAttrs,
                menu_item_attrs, menu_item_indicator_attrs, menu_trigger_attrs,
            },
        },
    };

    #[test]
    fn loads_built_in_registry_root() {
        let root = load_built_in_registry_root().expect("load root");

        assert_eq!(root.schema_version, SCHEMA_VERSION);
        let entries = root
            .items
            .iter()
            .map(|item| (item.name.as_str(), item.path.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            [
                ("anchor", "ui/anchor.json"),
                ("button", "ui/button.json"),
                ("collapsible", "ui/collapsible.json"),
                ("dialog", "ui/dialog.json"),
                ("field", "ui/field.json"),
                ("menu", "ui/menu.json"),
                ("router-link", "ui/router-link.json"),
                ("spinner", "ui/spinner.json"),
                ("status", "ui/status.json"),
                ("tabs", "ui/tabs.json"),
                ("tokens", "foundation/tokens.json"),
            ]
        );
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
    fn built_in_rust_sources_are_rustfmt_clean() {
        let root = load_built_in_registry_root().expect("load root");

        for entry in root.items {
            let item =
                parse_registry_item_file(&built_in_registry_root().join(entry.path)).expect("item");
            for file in item.files {
                if !file.source.ends_with(".rs") {
                    continue;
                }

                let path = built_in_registry_root().join(file.source);
                let output = Command::new("rustfmt")
                    .args(["--edition", "2024", "--config", "newline_style=Unix"])
                    .arg("--check")
                    .arg(&path)
                    .output()
                    .expect("run rustfmt");

                assert!(
                    output.status.success(),
                    "registry source {} is not rustfmt-clean\nstdout:\n{}\nstderr:\n{}",
                    path.display(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
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
    fn accepts_web_ui_primitives_version_cargo_plan_entry() {
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
              "files": [],
              "styles": [],
              "registryDependencies": [],
              "cargoPlan": [
                {
                  "crate": "web_ui_primitives",
                  "source": {
                    "kind": "version",
                    "version": "0.1.0"
                  },
                  "features": ["leptos"],
                  "required": true
                }
              ],
              "extra": {}
            }"#,
        )
        .expect("parse item");

        item.validate().expect("validate item");
    }

    #[test]
    fn rejects_unknown_cargo_plan_source_field() {
        let error = parse_registry_item_str(
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
              "files": [],
              "styles": [],
              "registryDependencies": [],
              "cargoPlan": [
                {
                  "crate": "web_ui_primitives",
                  "source": {
                    "kind": "git",
                    "url": "https://github.com/triesap/web_ui_primitives",
                    "rev": "6c764c035b4f6e3bce63e1f8619e25b36b45cb81",
                    "branch": "main"
                  },
                  "features": ["leptos"],
                  "required": true
                }
              ],
              "extra": {}
            }"#,
        )
        .expect_err("unknown source field should fail");

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
        let item = load_built_in_registry_item("button").expect("load button");

        assert!(source.contains("use leptos::prelude::*;"));
        assert!(source.contains("#[component]"));
        assert!(source.contains("#[allow(dead_code)]"));
        assert!(source.contains("use super::{Spinner, SpinnerMode};"));
        assert!(source.contains("pub fn Button"));
        assert!(source.contains("Children"));
        assert!(source.contains("<button"));
        assert!(source.contains("type=button_type.as_str()"));
        assert!(source.contains("loading: Signal<bool>"));
        assert!(source.contains("loading_label: String"));
        assert!(source.contains("disabled.get() || loading.get()"));
        assert!(source.contains("disabled=move || disabled_state.get()"));
        assert!(source.contains("aria-busy=move || loading.get().then_some(\"true\")"));
        assert!(source.contains("SpinnerMode::Decorative"));
        assert!(source.contains("kit-button-content"));
        assert!(source.contains("Self::Button => \"button\""));
        assert!(source.contains("Self::Submit => \"submit\""));
        assert!(source.contains("Self::Reset => \"reset\""));
        assert!(!source.contains("leptos_router"));
        assert!(!source.contains("tailwind"));
        assert!(css.contains(".kit-button"));
        assert!(css.contains("--kit-focus-ring"));
        assert!(css.contains("--kit-color-primary-hover"));
        assert!(css.contains("--kit-button-disabled-opacity"));
        assert!(css.contains("--kit-button-lg-min-height"));
        assert!(css.contains("--kit-button-spinner-size"));
        assert!(css.contains(".kit-button-content[data-loading]"));
        assert!(css.contains(":focus-visible"));
        assert!(!css.contains("@import"));
        assert!(!css.contains(":root"));
        assert!(!css.contains('#'));
        assert!(css.contains("var(--kit-button-border-width, var(--kit-border-width))"));
        assert!(css.contains("var(--kit-button-disabled-opacity, var(--kit-disabled-opacity))"));
        assert!(css.contains("var(--kit-button-transition-duration, var(--kit-duration-fast))"));
        assert!(css.contains("var(--kit-button-transition-timing, var(--kit-easing-standard))"));
        assert!(css.contains("var(--kit-color-ghost, transparent)"));
        assert!(css.contains("var(--kit-color-ghost-hover, var(--kit-color-surface-hover))"));
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(|behavior| behavior.name == "native-button-semantics" && behavior.required)
        );
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(|behavior| behavior.name == "loading-busy-state" && behavior.required)
        );
        assert_eq!(item.item.registry_dependencies, ["tokens", "spinner"]);
        assert_eq!(
            item.item.files[0].target.exports,
            ["Button", "ButtonSize", "ButtonType", "ButtonVariant"]
        );
    }

    #[test]
    fn anchor_source_and_css_encode_native_anchor_contract() {
        let root = built_in_registry_root();
        let source = fs::read_to_string(root.join("ui/anchor.rs")).expect("read anchor source");
        let css = fs::read_to_string(root.join("styles/anchor.css")).expect("read anchor css");
        let item = load_built_in_registry_item("anchor").expect("load anchor");

        assert!(source.contains("pub enum AnchorTarget"));
        assert!(source.contains("pub fn Anchor"));
        assert!(source.contains("<a"));
        assert!(source.contains("href=href"));
        assert!(source.contains("target=target_attr"));
        assert!(source.contains("rel=rel_attr"));
        assert!(source.contains("Self::Blank => Some(\"_blank\")"));
        assert!(source.contains("rel.or_else(|| target.default_rel().map(str::to_owned))"));
        assert!(source.contains("noopener noreferrer"));
        assert!(!source.contains("leptos_router"));
        assert!(
            item.item
                .cargo_plan
                .iter()
                .all(|dependency| dependency.crate_name != "leptos_router")
        );
        assert!(css.contains(".kit-anchor"));
        assert!(css.contains("--kit-anchor-color"));
        assert!(css.contains(":focus-visible"));
        assert!(!css.contains(":root"));
        assert!(css.contains("var(--kit-anchor-color, var(--kit-color-link))"));
        assert!(css.contains("var(--kit-anchor-color-hover, var(--kit-color-link-hover))"));
        assert!(css.contains("var(--kit-anchor-focus-outline-color, var(--kit-focus-ring))"));
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(|behavior| behavior.name == "external-target-rel-safety" && behavior.required)
        );
        assert_eq!(
            item.item.files[0].target.exports,
            ["Anchor", "AnchorTarget"]
        );
    }

    #[test]
    fn router_link_source_reuses_anchor_style_contract() {
        let root = built_in_registry_root();
        let source =
            fs::read_to_string(root.join("ui/router_link.rs")).expect("read router link source");
        let item = load_built_in_registry_item("router-link").expect("load router link");
        let resolved = resolve_built_in_registry_items(&["router-link".to_owned()])
            .expect("resolve router link");
        let resolved_names = resolved
            .iter()
            .map(|item| item.item.name.as_str())
            .collect::<Vec<_>>();

        assert!(source.contains("use leptos_router::components::A;"));
        assert!(source.contains("pub fn RouterLink"));
        assert!(source.contains("<A"));
        assert!(source.contains("attr:class=class"));
        assert!(source.contains("href=href"));
        assert!(source.contains("class_with_base(\"kit-anchor\", &class)"));
        assert!(!source.contains("AnchorTarget"));
        assert!(!source.contains("starts_with"));
        assert_eq!(
            item.item.accessibility.behaviors[0].name,
            "router-link-semantics"
        );
        assert_eq!(item.item.registry_dependencies, ["anchor"]);
        assert!(item.item.styles.is_empty());
        assert_eq!(item.item.files[0].target.exports, ["RouterLink"]);
        assert_eq!(resolved_names, ["tokens", "anchor", "router-link"]);
    }

    #[test]
    fn built_in_router_dependency_metadata_matches_router_source_usage() {
        for name in [
            "button",
            "field",
            "spinner",
            "status",
            "collapsible",
            "dialog",
            "menu",
            "tabs",
        ] {
            let item = load_built_in_registry_item(name).expect("load built-in item");

            assert!(
                item.item
                    .cargo_plan
                    .iter()
                    .all(|dependency| dependency.crate_name != "leptos_router"),
                "{name} must not require leptos_router"
            );
        }

        let router_link = load_built_in_registry_item("router-link").expect("load router-link");

        assert!(
            router_link
                .item
                .cargo_plan
                .iter()
                .any(|dependency| dependency.crate_name == "leptos_router")
        );
    }

    #[test]
    fn collapsible_css_uses_property_local_theme_fallbacks() {
        let root = built_in_registry_root();
        let css =
            fs::read_to_string(root.join("styles/collapsible.css")).expect("read collapsible css");

        assert!(!css.contains(":root"));
        assert!(!css.contains('#'));
        assert!(css.contains("var(--kit-collapsible-trigger-background, transparent)"));
        assert!(
            css.contains("var(--kit-collapsible-trigger-border-color, var(--kit-color-border))")
        );
        assert!(css.contains("var(--kit-collapsible-trigger-color, var(--kit-color-text))"));
        assert!(css.contains(
            "var(--kit-collapsible-trigger-disabled-opacity, var(--kit-disabled-opacity))"
        ));
        assert!(css.contains("var(--kit-collapsible-trigger-focus-ring, var(--kit-focus-ring))"));
        assert!(css.contains("var(--kit-collapsible-trigger-radius, var(--kit-radius-md))"));
    }

    #[test]
    fn form_field_source_and_css_encode_label_message_structure() {
        let root = built_in_registry_root();
        let root_source =
            fs::read_to_string(root.join("ui/field/root.rs")).expect("read field root source");
        let slot_source =
            fs::read_to_string(root.join("ui/field/slot.rs")).expect("read field slot source");
        let label_source =
            fs::read_to_string(root.join("ui/field/label.rs")).expect("read field label source");
        let message_source = fs::read_to_string(root.join("ui/field/message.rs"))
            .expect("read field message source");
        let required_source = fs::read_to_string(root.join("ui/field/required.rs"))
            .expect("read field required source");
        let surface_source = fs::read_to_string(root.join("ui/field/surface.rs"))
            .expect("read field surface source");
        let input_source =
            fs::read_to_string(root.join("ui/field/text_input.rs")).expect("read input source");
        let textarea_source =
            fs::read_to_string(root.join("ui/field/text_area.rs")).expect("read textarea source");
        let text_field_source = fs::read_to_string(root.join("ui/field/text_field.rs"))
            .expect("read text field source");
        let text_area_field_source = fs::read_to_string(root.join("ui/field/text_area_field.rs"))
            .expect("read text area field source");
        let select_source =
            fs::read_to_string(root.join("ui/field/native_select.rs")).expect("read select source");
        let select_field_source = fs::read_to_string(root.join("ui/field/select_field.rs"))
            .expect("read select field source");
        let css = fs::read_to_string(root.join("styles/field.css")).expect("read field css");
        let item = load_built_in_registry_item("field").expect("load field");

        assert!(root_source.contains("pub fn FieldRoot"));
        assert!(root_source.contains("control_id"));
        assert!(root_source.contains("message_id"));
        assert!(root_source.contains("message_ids"));
        assert!(root_source.contains("next_message_id"));
        assert!(root_source.contains("register_message_id"));
        assert!(root_source.contains("unregister_message_id"));
        assert!(root_source.contains("described_by_signal"));
        assert!(root_source.contains("resolved_described_by"));
        assert!(root_source.contains("message_ids.join(\" \")"));
        assert!(root_source.contains("required_signal"));
        assert!(root_source.contains("data-required"));
        assert!(slot_source.contains("pub struct FieldSlot"));
        assert!(slot_source.contains("impl<F, V> From<F> for FieldSlot"));
        assert!(slot_source.contains("pub fn empty() -> Self"));
        assert!(slot_source.contains("pub fn render(&self) -> AnyView"));
        assert!(label_source.contains("pub fn FieldLabel"));
        assert!(label_source.contains("<label"));
        assert!(label_source.contains("for=control_id"));
        assert!(message_source.contains("pub fn FieldMessage"));
        assert!(message_source.contains("id=message_id"));
        assert!(message_source.contains("context.register_message_id(message_id.clone())"));
        assert!(message_source.contains("on_cleanup"));
        assert!(
            message_source.contains("cleanup_context.unregister_message_id(&cleanup_message_id)")
        );
        assert!(required_source.contains("pub fn FieldRequired"));
        assert!(required_source.contains("FieldRequired must be used inside FieldRoot"));
        assert!(required_source.contains("aria-hidden=\"true\""));
        assert!(surface_source.contains("pub fn FieldSurface"));
        assert!(surface_source.contains("data-invalid"));
        assert!(input_source.contains("pub fn TextInput"));
        assert!(input_source.contains("TextInputType"));
        assert!(input_source.contains("context.required_signal()"));
        assert!(input_source.contains("let described_by = resolved_described_by"));
        assert!(input_source.contains("required=move || required.get()"));
        assert!(input_source.contains("disabled=move || disabled.get()"));
        assert!(input_source.contains("aria-describedby=move || described_by.get()"));
        assert!(input_source.contains("aria-invalid=move || data_state(invalid.get())"));
        assert!(textarea_source.contains("pub fn TextArea"));
        assert!(textarea_source.contains("context.required_signal()"));
        assert!(textarea_source.contains("let described_by = resolved_described_by"));
        assert!(textarea_source.contains("required=move || required.get()"));
        assert!(textarea_source.contains("disabled=move || disabled.get()"));
        assert!(textarea_source.contains("aria-describedby=move || described_by.get()"));
        assert!(textarea_source.contains("aria-invalid=move || data_state(invalid.get())"));
        assert!(text_field_source.contains("pub fn TextField"));
        assert!(text_field_source.contains("FieldRoot"));
        assert!(text_field_source.contains("FieldSurface"));
        assert!(text_field_source.contains("FieldLabel"));
        assert!(text_field_source.contains("TextInput"));
        assert!(text_field_source.contains("#[prop(into)] id: String"));
        assert!(text_field_source.contains("#[prop(into)] name: String"));
        assert!(text_field_source.contains("#[prop(into)] value: Signal<String>"));
        assert!(text_field_source.contains("message: Option<Signal<Option<String>>>"));
        assert!(text_field_source.contains(
            "#[prop(optional, into, default = FieldSlot::empty())] label_action: FieldSlot"
        ));
        assert!(text_field_source.contains("label_action_for_render.render()"));
        assert!(text_area_field_source.contains("pub fn TextAreaField"));
        assert!(text_area_field_source.contains("FieldRoot"));
        assert!(text_area_field_source.contains("FieldSurface"));
        assert!(text_area_field_source.contains("FieldLabel"));
        assert!(text_area_field_source.contains("TextArea"));
        assert!(text_area_field_source.contains("#[prop(into)] id: String"));
        assert!(text_area_field_source.contains("#[prop(into)] name: String"));
        assert!(text_area_field_source.contains("#[prop(into)] value: Signal<String>"));
        assert!(text_area_field_source.contains("message: Option<Signal<Option<String>>>"));
        assert!(text_area_field_source.contains(
            "#[prop(optional, into, default = FieldSlot::empty())] label_action: FieldSlot"
        ));
        assert!(text_area_field_source.contains("label_action_for_render.render()"));
        assert!(select_source.contains("pub fn NativeSelect"));
        assert!(select_source.contains("pub fn SelectIcon"));
        assert!(select_source.contains("context.required_signal()"));
        assert!(select_source.contains("let described_by = resolved_described_by"));
        assert!(select_source.contains("required=move || required.get()"));
        assert!(select_source.contains("disabled=move || disabled.get()"));
        assert!(select_source.contains("aria-describedby=move || described_by.get()"));
        assert!(select_source.contains("aria-invalid=move || data_state(invalid.get())"));
        assert!(select_field_source.contains("pub fn SelectField"));
        assert!(select_field_source.contains("NativeSelect"));
        assert!(select_field_source.contains("SelectIcon"));
        assert!(select_field_source.contains("FieldSlot"));
        assert!(!select_field_source.contains("SelectFieldSlot"));
        assert!(select_field_source.contains("#[prop(into)] selected_label: Signal<String>"));
        assert!(select_field_source.contains(
            "#[prop(optional, into, default = FieldSlot::empty())] label_action: FieldSlot"
        ));
        assert!(
            select_field_source
                .contains("#[prop(optional, into, default = FieldSlot::empty())] icon: FieldSlot")
        );
        assert!(select_field_source.contains("label_action_for_render.render()"));
        assert!(select_field_source.contains("icon_for_render.is_present()"));
        assert!(select_field_source.contains("children: Children"));
        assert!(css.contains(".kit-field"));
        assert!(css.contains(".kit-field-label"));
        assert!(css.contains(".kit-field-label-row"));
        assert!(css.contains(".kit-field-surface"));
        assert!(css.contains(".kit-field-control"));
        assert!(css.contains(".kit-native-select"));
        assert!(css.contains(".kit-select-field-native"));
        assert!(css.contains(".kit-select-field-value-row"));
        assert!(css.contains(".kit-select-field-value"));
        assert!(css.contains(".kit-select-icon"));
        assert!(css.contains(".kit-field-message"));
        assert!(css.contains("--kit-field-required-color"));
        assert!(!css.contains(":root"));
        assert!(!css.contains('#'));
        assert!(css.contains("var(--kit-field-control-background, var(--kit-color-surface))"));
        assert!(css.contains("var(--kit-field-control-border-color, var(--kit-color-border))"));
        assert!(css.contains("var(--kit-field-control-focus-ring, var(--kit-focus-ring))"));
        assert!(css.contains("var(--kit-field-message-color, var(--kit-color-text-muted))"));
        assert!(css.contains("var(--kit-field-required-color, var(--kit-color-danger))"));
        assert!(css.contains(
            "--kit-field-surface-background,\n    var(--kit-field-control-background, var(--kit-color-surface))"
        ));
        assert!(css.contains(
            "--kit-field-surface-radius,\n    var(--kit-field-control-radius, var(--kit-radius-md))"
        ));
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(|behavior| behavior.name == "label-control-association" && behavior.required)
        );
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(
                    |behavior| behavior.name == "field-required-state-propagation"
                        && behavior.required
                )
        );
        assert_eq!(
            item.item.files[0].target.exports,
            [
                "FieldLabel",
                "FieldMessage",
                "FieldRequired",
                "FieldRoot",
                "FieldSlot",
                "FieldSurface",
                "NativeSelect",
                "SelectField",
                "SelectIcon",
                "TextArea",
                "TextAreaField",
                "TextField",
                "TextInput",
                "TextInputType"
            ]
        );
    }

    #[test]
    fn menu_source_encodes_controlled_checked_state() {
        let root = built_in_registry_root();
        let root_source =
            fs::read_to_string(root.join("ui/menu/root.rs")).expect("read menu root source");
        let trigger_source =
            fs::read_to_string(root.join("ui/menu/trigger.rs")).expect("read menu trigger source");
        let content_source =
            fs::read_to_string(root.join("ui/menu/content.rs")).expect("read menu content source");
        let item_source =
            fs::read_to_string(root.join("ui/menu/item.rs")).expect("read menu item source");
        let radio_item_source = fs::read_to_string(root.join("ui/menu/radio_item.rs"))
            .expect("read menu radio item source");
        let indicator_source = fs::read_to_string(root.join("ui/menu/item_indicator.rs"))
            .expect("read menu indicator source");
        let css = fs::read_to_string(root.join("styles/menu.css")).expect("read menu css");
        let item = load_built_in_registry_item("menu").expect("load menu");

        assert!(root_source.contains("checked_index: Option<Signal<Option<usize>>>"));
        assert!(root_source.contains("trigger_ref: NodeRef<html::Button>"));
        assert!(root_source.contains("model_snapshot"));
        assert!(root_source.contains("apply_controlled_checked_untracked"));
        assert!(root_source.contains("DomAttribute"));
        assert!(root_source.contains("attr_string"));
        assert!(root_source.contains("attr_bool"));
        assert!(root_source.contains("matches!(attr.value(), DomAttributeValue::Bool(true))"));
        assert!(!root_source.contains("DomAttributeValue::String(_) => true"));
        assert!(trigger_source.contains("menu_trigger_attrs"));
        assert!(trigger_source.contains("<button"));
        assert!(trigger_source.contains("node_ref=node_ref"));
        assert!(trigger_source.contains("type=\"button\""));
        assert!(trigger_source.contains("disabled=move || disabled.get()"));
        assert!(trigger_source.contains("aria-expanded=move || attr_string"));
        assert!(trigger_source.contains("aria-controls=move || attr_string"));
        assert!(trigger_source.contains("data-state=move || attr_string"));
        assert!(trigger_source.contains("event.stop_propagation()"));
        assert!(!trigger_source.contains("suppress_click"));
        assert!(!trigger_source.contains("event.prevent_default()"));
        assert!(content_source.contains("pub enum MenuContentSide"));
        assert!(content_source.contains("pub enum MenuContentAlign"));
        assert!(content_source.contains("side: MenuContentSide"));
        assert!(content_source.contains("align: MenuContentAlign"));
        assert!(content_source.contains("spacing: f64"));
        assert!(content_source.contains("viewport_padding: f64"));
        assert!(content_source.contains("on_pointer_down_outside"));
        assert!(content_source.contains("on_focus_outside"));
        assert!(content_source.contains("target_is_trigger"));
        assert!(content_source.contains("use_menu_placement_with_node_refs"));
        assert!(content_source.contains("MenuPlacementOptions::new"));
        assert!(content_source.contains("style=move || style_placement.style()"));
        assert!(content_source.contains("data-side=move || side_placement.data_side()"));
        assert!(content_source.contains("data-align=move || align_placement.data_align()"));
        assert!(item_source.contains("MenuItemKind::Radio"));
        assert!(item_source.contains("checked_is_controlled"));
        assert!(item_source.contains("label: Option<Signal<String>>"));
        assert!(item_source.contains("set_label(index, label.get())"));
        assert!(item_source.contains("model_snapshot"));
        assert!(item_source.contains("menu_item_attrs"));
        assert!(item_source.contains("<button"));
        assert!(item_source.contains("type=\"button\""));
        assert!(item_source.contains("role=move || attr_string"));
        assert!(item_source.contains("tabindex=move || attr_string"));
        assert!(item_source.contains("disabled=move || attr_bool"));
        assert!(item_source.contains("aria-checked=move || attr_string"));
        assert!(item_source.contains("aria-disabled=move || attr_string"));
        assert!(item_source.contains("data-highlighted=move || data_attr"));
        assert!(item_source.contains("data-disabled=move || data_attr"));
        assert!(item_source.contains("MenuItemAttrs::new().kind(kind.as_attrs_kind())"));
        assert!(radio_item_source.contains("pub fn MenuRadioItem"));
        assert!(radio_item_source.contains("kind=MenuItemKind::Radio"));
        assert!(radio_item_source.contains("<MenuItemIndicator index=index"));
        assert!(radio_item_source.contains("{move || label_for_text.get()}"));
        assert!(indicator_source.contains("model_snapshot"));
        assert!(indicator_source.contains("menu_item_indicator_attrs"));
        assert!(indicator_source.contains("hidden=move || attr_bool"));
        assert!(indicator_source.contains("data-state=move || attr_string"));
        assert!(css.contains("position: fixed;"));
        assert!(css.contains("overflow: auto;"));
        assert!(css.contains(".kit-menu-content[data-state=\"closed\"][data-side=\"bottom\"]"));
        assert!(css.contains(".kit-menu-content[data-state=\"closed\"][data-side=\"top\"]"));
        assert!(css.contains(".kit-menu-content[data-state=\"closed\"][data-side=\"right\"]"));
        assert!(css.contains(".kit-menu-content[data-state=\"closed\"][data-side=\"left\"]"));
        assert!(css.contains(".kit-menu-radio-item-label"));
        assert!(css.contains(".kit-menu-item-indicator[hidden]"));
        assert!(css.contains("display: none;"));
        assert!(!css.contains(":root"));
        assert!(!css.contains('#'));
        assert!(
            css.contains("var(--kit-menu-content-background, var(--kit-color-surface-raised))")
        );
        assert!(css.contains("var(--kit-menu-content-elevation, var(--kit-shadow-md))"));
        assert!(css.contains(
            "var(--kit-menu-item-background-highlighted, var(--kit-color-surface-hover))"
        ));
        assert!(css.contains("--kit-menu-content-translate-x: 0;"));
        assert!(css.contains("--kit-menu-content-translate-y: 0;"));
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(|behavior| behavior.name == "controlled-checked-item-state"
                    && behavior.required)
        );
        assert_eq!(
            item.item.files[0].target.exports,
            [
                "MenuContent",
                "MenuContentAlign",
                "MenuContentSide",
                "MenuDirection",
                "MenuItem",
                "MenuItemIndicator",
                "MenuItemKind",
                "MenuLoop",
                "MenuRadioItem",
                "MenuRoot",
                "MenuTrigger"
            ]
        );
    }

    #[test]
    fn menu_attrs_expose_checked_indicator_state() {
        let mut model = MenuModel::with_loop(2, MenuLoop::Wrap);
        model.set_checked(Some(0));

        let active_attrs = menu_item_indicator_attrs(&model, 0);
        let inactive_attrs = menu_item_indicator_attrs(&model, 1);

        assert_eq!(bool_attr(&active_attrs, "hidden"), Some(false));
        assert_eq!(string_attr(&active_attrs, "data-state"), Some("checked"));
        assert_eq!(bool_attr(&inactive_attrs, "hidden"), Some(true));
        assert_eq!(
            string_attr(&inactive_attrs, "data-state"),
            Some("unchecked")
        );
    }

    #[test]
    fn menu_attrs_expose_trigger_and_item_open_state() {
        let mut model = MenuModel::with_loop(2, MenuLoop::Wrap);

        let closed_attrs = menu_trigger_attrs(
            &model,
            MenuTriggerAttrs::new().controls_id("locale-menu-content"),
        );
        assert_eq!(string_attr(&closed_attrs, "aria-expanded"), Some("false"));
        assert_eq!(string_attr(&closed_attrs, "data-state"), Some("closed"));
        assert_eq!(
            string_attr(&closed_attrs, "aria-controls"),
            Some("locale-menu-content")
        );

        model.set_open(true);
        model.focus_index(Some(1));
        model.set_checked(Some(1));

        let open_attrs = menu_trigger_attrs(
            &model,
            MenuTriggerAttrs::new().controls_id("locale-menu-content"),
        );
        let focused_item_attrs = menu_item_attrs(
            &model,
            1,
            MenuItemAttrs::new().kind(AttrsMenuItemKind::Radio),
        );

        assert_eq!(string_attr(&open_attrs, "aria-expanded"), Some("true"));
        assert_eq!(string_attr(&open_attrs, "data-state"), Some("open"));
        assert_eq!(
            string_attr(&focused_item_attrs, "role"),
            Some("menuitemradio")
        );
        assert_eq!(string_attr(&focused_item_attrs, "tabindex"), Some("0"));
        assert_eq!(
            string_attr(&focused_item_attrs, "aria-checked"),
            Some("true")
        );
        assert_eq!(
            bool_attr(&focused_item_attrs, "data-highlighted"),
            Some(true)
        );
    }

    #[test]
    fn menu_model_keyboard_contract_closes_and_selects() {
        let mut model = MenuModel::with_loop(3, MenuLoop::Wrap);
        model.set_disabled(1, true);
        model.set_open(true);

        assert_eq!(model.focus_by_key("ArrowDown", Direction::Ltr), Some(2));
        assert_eq!(model.activate_index(2), Some(2));
        assert!(!model.open());
        assert_eq!(model.focused(), None);

        model.set_open(true);
        assert!(model.close_by_key("Escape"));
        assert!(!model.open());
    }

    #[test]
    fn spinner_source_and_css_encode_status_loading_indicator() {
        let root = built_in_registry_root();
        let source = fs::read_to_string(root.join("ui/spinner.rs")).expect("read spinner source");
        let css = fs::read_to_string(root.join("styles/spinner.css")).expect("read spinner css");
        let item = load_built_in_registry_item("spinner").expect("load spinner");

        assert!(source.contains("pub fn Spinner"));
        assert!(source.contains("pub enum SpinnerMode"));
        assert!(source.contains("role=mode.role()"));
        assert!(source.contains("aria-hidden=mode.aria_hidden()"));
        assert!(source.contains("Self::Decorative"));
        assert!(source.contains("kit-spinner-mark"));
        assert!(source.contains("kit-spinner-label"));
        assert!(css.contains(".kit-spinner"));
        assert!(css.contains("@keyframes kit-spinner-rotate"));
        assert!(css.contains("--kit-spinner-animation-duration"));
        assert!(!css.contains(":root"));
        assert!(css.contains("var(--kit-spinner-color, currentColor)"));
        assert!(css.contains("color-mix(in srgb, currentColor 20%, transparent)"));
        assert!(css.contains("border-radius: var(--kit-radius-full)"));
        assert!(css.contains("var(--kit-spinner-animation-duration, 900ms)"));
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(|behavior| behavior.name == "status-role" && behavior.required)
        );
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(|behavior| behavior.name == "decorative-mode" && behavior.required)
        );
        assert_eq!(
            item.item.files[0].target.exports,
            ["Spinner", "SpinnerMode"]
        );
    }

    #[test]
    fn status_source_and_css_encode_live_region_contract() {
        let root = built_in_registry_root();
        let source = fs::read_to_string(root.join("ui/status.rs")).expect("read status source");
        let css = fs::read_to_string(root.join("styles/status.css")).expect("read status css");
        let item = load_built_in_registry_item("status").expect("load status");

        assert!(source.contains("pub enum StatusRole"));
        assert!(source.contains("pub enum StatusPoliteness"));
        assert!(source.contains("pub fn Status"));
        assert!(source.contains("role=role.as_str()"));
        assert!(source.contains("aria-live=politeness.as_str()"));
        assert!(source.contains("aria-atomic=if atomic"));
        assert!(css.contains(".kit-status"));
        assert!(css.contains("--kit-status-color"));
        assert!(!css.contains(":root"));
        assert!(css.contains("color: var(--kit-status-color, var(--kit-color-text))"));
        assert!(css.contains("font-size: var(--kit-status-font-size, 1rem)"));
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(|behavior| behavior.name == "live-region-role" && behavior.required)
        );
        assert_eq!(
            item.item.files[0].target.exports,
            ["Status", "StatusPoliteness", "StatusRole"]
        );
    }

    #[test]
    fn tabs_source_declares_keyboard_accessibility_contract() {
        let root = built_in_registry_root();
        let trigger =
            fs::read_to_string(root.join("ui/tabs/trigger.rs")).expect("read tabs trigger source");
        let css = fs::read_to_string(root.join("styles/tabs.css")).expect("read tabs css");
        let item = load_built_in_registry_item("tabs").expect("load tabs");

        assert!(trigger.contains("on:keydown"));
        assert!(trigger.contains("focus_by_key"));
        assert!(trigger.contains("activate_focused"));
        assert!(trigger.contains("focus_trigger"));
        assert!(!css.contains(":root"));
        assert!(!css.contains('#'));
        assert!(css.contains("var(--kit-tabs-panel-background, transparent)"));
        assert!(css.contains("var(--kit-tabs-panel-color, inherit)"));
        assert!(
            css.contains("var(--kit-tabs-trigger-background-active, var(--kit-color-surface))")
        );
        assert!(
            css.contains(
                "var(--kit-tabs-trigger-background-hover, var(--kit-color-surface-hover))"
            )
        );
        assert!(css.contains("var(--kit-tabs-trigger-border-color, var(--kit-color-border))"));
        assert!(css.contains("var(--kit-tabs-trigger-color, var(--kit-color-text))"));
        assert!(
            css.contains("var(--kit-tabs-trigger-color-inactive, var(--kit-color-text-secondary))")
        );
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(|behavior| behavior.name == "keyboard-focus-policy" && behavior.required)
        );
    }

    #[test]
    fn dialog_source_declares_overlay_accessibility_contract() {
        let root = built_in_registry_root();
        let content = fs::read_to_string(root.join("ui/dialog/content.rs"))
            .expect("read dialog content source");
        let css = fs::read_to_string(root.join("styles/dialog.css")).expect("read dialog css");
        let item = load_built_in_registry_item("dialog").expect("load dialog");

        assert!(content.contains("use_dialog_layer_with_node_ref"));
        assert!(content.contains("aria-labelledby"));
        assert!(content.contains("aria-describedby"));
        assert!(content.contains("DialogLayerOptions"));
        assert!(content.contains("PortalMount"));
        assert!(content.contains("#[prop(optional)] portal_mount: Option<PortalMount>"));
        assert!(content.contains("if let Some(portal_mount) = portal_mount.clone()"));
        assert!(content.contains("<Portal mount=portal_mount>"));
        assert!(content.contains("<Portal>"));
        assert!(!css.contains(":root"));
        assert!(!css.contains('#'));
        assert!(css.contains("var(--kit-dialog-background, var(--kit-color-surface-raised))"));
        assert!(css.contains("var(--kit-dialog-color, var(--kit-color-text))"));
        assert!(css.contains("var(--kit-dialog-description-color, var(--kit-color-text-muted))"));
        assert!(css.contains("var(--kit-dialog-elevation, var(--kit-shadow-lg))"));
        assert!(css.contains("var(--kit-dialog-trigger-background, transparent)"));
        assert!(css.contains("var(--kit-dialog-close-background, transparent)"));
        assert!(
            item.item
                .accessibility
                .behaviors
                .iter()
                .any(|behavior| behavior.name == "modal-focus-trap" && behavior.required)
        );
        assert_eq!(
            item.item.files[0].target.exports,
            [
                "DialogClose",
                "DialogContent",
                "DialogContentRole",
                "DialogDescription",
                "DialogRoot",
                "DialogTitle",
                "DialogTrigger"
            ]
        );
    }

    #[test]
    fn accepts_nested_ui_target_paths() {
        let mut item = item_with_name_and_target("nested", "nested/mod.rs", "nested", &[]);
        item.files[0].target.exports = vec!["Nested".to_owned()];

        item.validate().expect("nested target should validate");
    }

    #[test]
    fn rejects_unsafe_or_noncanonical_ui_target_paths() {
        for path in [
            "mod.rs",
            "nested/../root.rs",
            "nested/Root.rs",
            ".hidden/root.rs",
            "nested/root.txt",
        ] {
            let item = item_with_name_and_target("nested", path, "nested", &[]);
            let error = item.validate().expect_err("target path should fail");

            assert!(matches!(error, RegistryError::UnsafePath { .. }), "{path}");
        }
    }

    #[test]
    fn rejects_item_names_without_a_lowercase_letter_prefix() {
        for name in ["", "-button", "1button", "Button", "button_name"] {
            let item = item_with_name_and_target(name, "button.rs", name, &[]);
            let error = item.validate().expect_err("item name should fail");

            assert!(
                matches!(error, RegistryError::InvalidValue { field: "name", .. }),
                "{name:?}"
            );
        }
    }

    #[test]
    fn rejects_blank_item_titles_and_descriptions() {
        let mut blank_title = item_with_name_and_target("button", "button.rs", "button", &[]);
        blank_title.title = " \n\t".to_owned();
        assert!(matches!(
            blank_title.validate(),
            Err(RegistryError::InvalidValue { field: "title", .. })
        ));

        let mut blank_description = item_with_name_and_target("button", "button.rs", "button", &[]);
        blank_description.description = " \n\t".to_owned();
        assert!(matches!(
            blank_description.validate(),
            Err(RegistryError::InvalidValue {
                field: "description",
                ..
            })
        ));
    }

    #[test]
    fn rejects_accessibility_behavior_names_without_a_lowercase_letter_prefix() {
        for name in ["1behavior", "-behavior"] {
            let mut item = item_with_name_and_target("button", "button.rs", "button", &[]);
            item.accessibility.behaviors = vec![RegistryAccessibilityBehavior {
                name: name.to_owned(),
                required: true,
            }];

            assert!(
                matches!(
                    item.validate(),
                    Err(RegistryError::InvalidValue {
                        field: "accessibility.behaviors[].name",
                        ..
                    })
                ),
                "{name:?}"
            );
        }
    }

    #[test]
    fn rejects_unsafe_or_wrong_type_registry_source_paths() {
        for source in [
            "",
            "/ui/button.rs",
            "ui/button",
            "ui/button.css",
            "../ui/button.rs",
            "ui\\button.rs",
            "ui//button.rs",
            "ui/./button.rs",
            "ui/.hidden/button.rs",
        ] {
            let mut item = item_with_name_and_target("button", "button.rs", "button", &[]);
            item.files[0].source = source.to_owned();

            assert!(
                matches!(item.validate(), Err(RegistryError::UnsafePath { .. })),
                "{source}"
            );
        }

        for source in [
            "",
            "/styles/button.css",
            "styles/button",
            "styles/button.rs",
            "styles/../button.css",
            "styles/.hidden/button.css",
        ] {
            let mut item = item_with_name_and_target("button", "button.rs", "button", &[]);
            item.styles[0].source = source.to_owned();

            assert!(
                matches!(item.validate(), Err(RegistryError::UnsafePath { .. })),
                "{source}"
            );
        }
    }

    #[test]
    fn registry_root_rejects_unsafe_non_json_and_duplicate_paths() {
        for path in [
            "",
            "/ui/button.json",
            "ui/button",
            "ui/button.css",
            "ui\\button.json",
            "ui//button.json",
            "ui/./button.json",
            "ui/../button.json",
            ".hidden/button.json",
        ] {
            let root = registry_root_with_items(vec![RegistryRootItem {
                name: "button".to_owned(),
                path: path.to_owned(),
            }]);
            assert!(
                matches!(
                    root.validate(),
                    Err(RegistryError::UnsafePath {
                        field: "items[].path",
                        ..
                    })
                ),
                "{path:?}"
            );
        }

        let root = registry_root_with_items(vec![
            RegistryRootItem {
                name: "button".to_owned(),
                path: "ui/shared.json".to_owned(),
            },
            RegistryRootItem {
                name: "spinner".to_owned(),
                path: "ui/shared.json".to_owned(),
            },
        ]);
        assert!(matches!(
            root.validate(),
            Err(RegistryError::InvalidValue {
                field: "items[].path",
                ..
            })
        ));

        let duplicate_name = registry_root_with_items(vec![
            RegistryRootItem {
                name: "button".to_owned(),
                path: "ui/button.json".to_owned(),
            },
            RegistryRootItem {
                name: "button".to_owned(),
                path: "ui/another-button.json".to_owned(),
            },
        ]);
        assert!(matches!(
            duplicate_name.validate(),
            Err(RegistryError::DuplicateTarget(_))
        ));
    }

    #[test]
    fn rejects_duplicate_registry_dependencies() {
        let item =
            item_with_name_and_target("button", "button.rs", "button", &["tokens", "tokens"]);

        assert!(matches!(
            item.validate(),
            Err(RegistryError::InvalidValue {
                field: "registryDependencies",
                ..
            })
        ));
    }

    #[test]
    fn rejects_registry_root_entry_name_that_differs_from_manifest_name() {
        let entry = RegistryRootItem {
            name: "button".to_owned(),
            path: "ui/button.json".to_owned(),
        };
        let item = item_with_name_and_target("spinner", "spinner.rs", "spinner", &[]);

        assert!(matches!(
            validate_registry_root_item_identity(&entry, &item),
            Err(RegistryError::InvalidValue {
                field: "items[].name",
                ..
            })
        ));
    }

    #[test]
    fn foundation_items_require_styles_and_forbid_ui_files() {
        foundation_item()
            .validate()
            .expect("foundation CSS item should validate");

        let mut no_styles = foundation_item();
        no_styles.styles.clear();
        assert!(matches!(
            no_styles.validate(),
            Err(RegistryError::InvalidValue {
                field: "styles",
                ..
            })
        ));

        let mut with_ui_file = foundation_item();
        with_ui_file.files = item_with_name_and_target("ui", "ui.rs", "ui", &[]).files;
        assert!(matches!(
            with_ui_file.validate(),
            Err(RegistryError::InvalidValue { field: "files", .. })
        ));
    }

    #[test]
    fn public_item_schema_declares_foundation_invariants() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schema/0.9.0-alpha/registry-item.schema.json");
        let schema = serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(path).expect("read schema"),
        )
        .expect("parse schema");

        assert_eq!(
            schema["properties"]["kind"]["enum"],
            serde_json::json!(["ui", "foundation"])
        );
        assert_eq!(
            schema["allOf"][0]["then"]["properties"]["files"]["maxItems"],
            serde_json::json!(0)
        );
        assert_eq!(
            schema["allOf"][0]["then"]["properties"]["styles"]["minItems"],
            serde_json::json!(1)
        );
    }

    #[test]
    fn public_registry_schemas_declare_structural_integrity_constraints() {
        let schema_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/0.9.0-alpha");
        let root_schema = serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(schema_root.join("registry.schema.json"))
                .expect("read registry schema"),
        )
        .expect("parse registry schema");
        let item_schema = serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(schema_root.join("registry-item.schema.json"))
                .expect("read registry item schema"),
        )
        .expect("parse registry item schema");

        assert_eq!(
            root_schema["properties"]["items"]["uniqueItems"],
            serde_json::json!(true)
        );
        assert_eq!(
            root_schema["properties"]["items"]["items"]["properties"]["name"]["pattern"],
            serde_json::json!("^[a-z][a-z0-9-]*$")
        );
        assert!(
            root_schema["properties"]["items"]["items"]["properties"]["path"]["pattern"]
                .as_str()
                .expect("root item path pattern")
                .ends_with("\\.json$")
        );
        assert_eq!(
            item_schema["properties"]["title"]["pattern"],
            serde_json::json!("\\S")
        );
        assert_eq!(
            item_schema["properties"]["description"]["pattern"],
            serde_json::json!("\\S")
        );
        assert!(
            item_schema["properties"]["files"]["items"]["properties"]["source"]["pattern"]
                .as_str()
                .expect("Rust source pattern")
                .ends_with("\\.rs$")
        );
        assert!(
            item_schema["properties"]["styles"]["items"]["properties"]["source"]["pattern"]
                .as_str()
                .expect("CSS source pattern")
                .ends_with("\\.css$")
        );
        assert_eq!(
            item_schema["properties"]["registryDependencies"]["uniqueItems"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn graph_validates_registry_dependency_order() {
        let dependency = item_with_name_and_target("base", "base.rs", "base", &[]);
        let dependent = item_with_name_and_target("button", "button.rs", "button", &["base"]);

        let order = validate_registry_graph(&[dependent, dependency]).expect("graph");

        assert_eq!(order, vec!["base".to_owned(), "button".to_owned()]);
    }

    #[test]
    fn exactly_styled_non_token_items_depend_directly_on_tokens() {
        let root = load_built_in_registry_root().expect("load root");
        let mut styled = Vec::new();

        for entry in root.items {
            let item = load_built_in_registry_item(&entry.name).expect("load item");
            let direct_tokens = item
                .item
                .registry_dependencies
                .iter()
                .filter(|dependency| dependency.as_str() == "tokens")
                .count();
            let should_depend = item.item.name != "tokens" && !item.item.styles.is_empty();
            assert_eq!(
                direct_tokens,
                usize::from(should_depend),
                "{}",
                item.item.name
            );
            if should_depend {
                styled.push(item.item.name);
            }
        }

        assert_eq!(
            styled,
            [
                "anchor",
                "button",
                "collapsible",
                "dialog",
                "field",
                "menu",
                "spinner",
                "status",
                "tabs",
            ]
        );
    }

    #[test]
    fn tokens_manifest_exactly_matches_the_theme_contract_version() {
        let tokens = load_built_in_registry_item("tokens").expect("load tokens");
        let contract = crate::load_built_in_theme_contract().expect("load theme contract");

        validate_built_in_tokens_item(&tokens.item).expect("validate exact tokens manifest");
        assert_eq!(contract.contract_version, THEME_CONTRACT_VERSION);
        assert_eq!(
            tokens.item.extra.get("themeContractVersion"),
            Some(&serde_json::json!(contract.contract_version))
        );
    }

    #[test]
    fn built_in_catalog_rejects_tokens_or_dependency_shape_drift() {
        let root_path = built_in_registry_root().join("registry.json");
        let root = parse_registry_root_file(&root_path).expect("parse root without catalog check");
        let items = root
            .items
            .iter()
            .map(|entry| {
                parse_built_in_item_from_root(&root, &entry.name)
                    .expect("parse item")
                    .0
            })
            .collect::<Vec<_>>();

        let mut missing_styled_dependency = items.clone();
        missing_styled_dependency
            .iter_mut()
            .find(|item| item.name == "anchor")
            .expect("anchor")
            .registry_dependencies
            .clear();
        assert!(validate_built_in_registry_items(&missing_styled_dependency).is_err());

        let mut redundant_unstyled_dependency = items.clone();
        redundant_unstyled_dependency
            .iter_mut()
            .find(|item| item.name == "router-link")
            .expect("router-link")
            .registry_dependencies
            .push("tokens".to_owned());
        assert!(validate_built_in_registry_items(&redundant_unstyled_dependency).is_err());

        let mut wrong_tokens_version = items;
        wrong_tokens_version
            .iter_mut()
            .find(|item| item.name == "tokens")
            .expect("tokens")
            .extra
            .insert(
                "themeContractVersion".to_owned(),
                serde_json::json!("wrong"),
            );
        assert!(validate_built_in_registry_items(&wrong_tokens_version).is_err());
    }

    #[test]
    fn button_resolution_is_tokens_spinner_then_button() {
        let items = resolve_built_in_registry_items(&["button".to_owned()])
            .expect("resolve button dependencies");
        let names = items
            .iter()
            .map(|item| item.item.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["tokens", "spinner", "button"]);
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

    fn string_attr<'a>(attrs: &'a [DomAttribute], name: &str) -> Option<&'a str> {
        attrs.iter().find_map(|attr| {
            if attr.name() != name {
                return None;
            }
            match attr.value() {
                DomAttributeValue::String(value) => Some(value.as_str()),
                DomAttributeValue::Bool(_) => None,
            }
        })
    }

    fn bool_attr(attrs: &[DomAttribute], name: &str) -> Option<bool> {
        attrs.iter().find_map(|attr| {
            if attr.name() != name {
                return None;
            }
            match attr.value() {
                DomAttributeValue::String(_) => None,
                DomAttributeValue::Bool(value) => Some(*value),
            }
        })
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
            accessibility: RegistryAccessibility::default(),
            files: vec![RegistryItemFile {
                source: format!("ui/{file_target}"),
                target: RegistryFileTarget {
                    kind: RegistryFileTargetKind::Ui,
                    path: file_target.to_owned(),
                    exports: Vec::new(),
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

    fn registry_root_with_items(items: Vec<RegistryRootItem>) -> RegistryRoot {
        RegistryRoot {
            schema: REGISTRY_SCHEMA_URL.to_owned(),
            schema_version: SCHEMA_VERSION.to_owned(),
            name: "leptos-ui-kit".to_owned(),
            items,
        }
    }

    fn foundation_item() -> RegistryItem {
        let mut item = item_with_name_and_target("foundation", "unused.rs", "foundation", &[]);
        item.kind = RegistryItemKind::Foundation;
        item.files.clear();
        item
    }
}
