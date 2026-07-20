use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    KIT_SCHEMA_URL, REGISTRY_ITEM_SCHEMA_URL, REGISTRY_SCHEMA_URL, RegistryError, RegistryItem,
    RegistryRoot, ResolvedRegistryTargets, SCHEMA_VERSION, THEME_CONTRACT_SCHEMA_URL,
    THEME_INTEGRATION_SCHEMA_URL, TOKEN_CONTRACT_SCHEMA_URL, ThemeContract, ThemeContractError,
    embedded_assets::{
        AssetProvider, AssetProviderError, EmbeddedAssetKind, embedded_asset_inventory,
        embedded_asset_provider,
    },
    item::{
        parse_registry_item_raw_str, parse_registry_root_raw_str, resolve_registry_targets,
        validate_built_in_registry_items,
    },
    registry_health::{RegistryHealthError, validate_theme_contract_schema_shape},
    theme_contract::parse_theme_contract_str,
};

const REGISTRY_ROOT_PATH: &str = "registry/registry.json";
const THEME_CONTRACT_PATH: &str = "registry/contracts/theme-v1.json";
const TOKENS_MANIFEST_PATH: &str = "registry/foundation/tokens.json";
const TOKENS_CSS_PATH: &str = "registry/styles/tokens.css";
const JSON_SCHEMA_DRAFT_2020_12_URL: &str = "https://json-schema.org/draft/2020-12/schema";
const THEME_CONTRACT_SCHEMA_PATH: &str = "schema/0.9.0-alpha/theme-contract.schema.json";
const SCHEMA_PATHS: [&str; 6] = [
    "schema/0.9.0-alpha/kit.schema.json",
    "schema/0.9.0-alpha/registry-item.schema.json",
    "schema/0.9.0-alpha/registry.schema.json",
    THEME_CONTRACT_SCHEMA_PATH,
    "schema/0.2.0/theme-integration.schema.json",
    "schema/0.2.0/token-contract.schema.json",
];
const KIT_SCHEMA_REQUIRED: &[&str] = &[
    "$schema",
    "schemaVersion",
    "tool",
    "project",
    "leptos",
    "install",
    "styles",
    "registry",
    "items",
];
const REGISTRY_ITEM_SCHEMA_REQUIRED: &[&str] = &[
    "$schema",
    "schemaVersion",
    "name",
    "kind",
    "version",
    "title",
    "description",
    "leptos",
    "files",
    "styles",
    "cargoPlan",
];
const REGISTRY_SCHEMA_REQUIRED: &[&str] =
    &["$schema", "schemaVersion", "name", "compatibility", "items"];
const THEME_CONTRACT_SCHEMA_REQUIRED: &[&str] = &[
    "$schema",
    "schemaVersion",
    "contractVersion",
    "name",
    "tokens",
];
const THEME_INTEGRATION_SCHEMA_REQUIRED: &[&str] = &[
    "$schema",
    "schemaVersion",
    "producer",
    "primitives",
    "contract",
    "stylesheet",
    "layerAbi",
    "portalAbi",
];
const TOKEN_ABI_SCHEMA_REQUIRED: &[&str] = &[
    "$schema",
    "schemaVersion",
    "contractId",
    "abiVersion",
    "revision",
    "dtcgVersion",
    "dtcgProfile",
    "canonicalDigest",
    "tokens",
    "contrastChecks",
    "extensions",
];

struct SchemaContract {
    logical_path: &'static str,
    schema_id: &'static str,
    schema_version: &'static str,
    required: &'static [&'static str],
}

const SCHEMA_CONTRACTS: [SchemaContract; 6] = [
    SchemaContract {
        logical_path: SCHEMA_PATHS[0],
        schema_id: KIT_SCHEMA_URL,
        schema_version: SCHEMA_VERSION,
        required: KIT_SCHEMA_REQUIRED,
    },
    SchemaContract {
        logical_path: SCHEMA_PATHS[1],
        schema_id: REGISTRY_ITEM_SCHEMA_URL,
        schema_version: SCHEMA_VERSION,
        required: REGISTRY_ITEM_SCHEMA_REQUIRED,
    },
    SchemaContract {
        logical_path: SCHEMA_PATHS[2],
        schema_id: REGISTRY_SCHEMA_URL,
        schema_version: SCHEMA_VERSION,
        required: REGISTRY_SCHEMA_REQUIRED,
    },
    SchemaContract {
        logical_path: THEME_CONTRACT_SCHEMA_PATH,
        schema_id: THEME_CONTRACT_SCHEMA_URL,
        schema_version: SCHEMA_VERSION,
        required: THEME_CONTRACT_SCHEMA_REQUIRED,
    },
    SchemaContract {
        logical_path: "schema/0.2.0/theme-integration.schema.json",
        schema_id: THEME_INTEGRATION_SCHEMA_URL,
        schema_version: "1.0.0",
        required: THEME_INTEGRATION_SCHEMA_REQUIRED,
    },
    SchemaContract {
        logical_path: "schema/0.2.0/token-contract.schema.json",
        schema_id: TOKEN_CONTRACT_SCHEMA_URL,
        schema_version: "1.0.0",
        required: TOKEN_ABI_SCHEMA_REQUIRED,
    },
];

#[derive(Debug, Clone)]
struct OwnedAsset {
    kind: EmbeddedAssetKind,
    content: Arc<str>,
}

#[derive(Debug, Clone)]
pub(crate) struct BuiltInRegistryItemSnapshot {
    manifest_path: String,
    item: RegistryItem,
    targets: ResolvedRegistryTargets,
    content_hash: String,
}

impl BuiltInRegistryItemSnapshot {
    pub(crate) fn manifest_path(&self) -> &str {
        &self.manifest_path
    }

    pub(crate) fn item(&self) -> &RegistryItem {
        &self.item
    }

    pub(crate) fn targets(&self) -> &ResolvedRegistryTargets {
        &self.targets
    }

    pub(crate) fn content_hash(&self) -> &str {
        &self.content_hash
    }
}

/// One owned, immutable view of every packaged built-in asset and parsed model.
///
/// Construction consumes provider bytes once. All later registry operations use
/// this snapshot and cannot observe authoring-tree, provider, or filesystem
/// changes.
#[derive(Debug, Clone)]
pub(crate) struct BuiltInRegistrySnapshot {
    assets: BTreeMap<String, OwnedAsset>,
    root: RegistryRoot,
    items: BTreeMap<String, BuiltInRegistryItemSnapshot>,
    theme_contract: ThemeContract,
    schemas: BTreeMap<String, Value>,
}

impl BuiltInRegistrySnapshot {
    pub(crate) fn from_provider(provider: &dyn AssetProvider) -> Result<Self, SnapshotError> {
        let assets = own_exact_catalog(provider)?;
        let root = parse_root(&assets)?;
        let (items, parsed_items) = parse_items(&assets, &root)?;
        validate_exact_asset_ownership(&assets, &root, &parsed_items)?;
        validate_runtime_compatibility_sources(&assets, &parsed_items)?;
        let theme_contract = parse_theme_contract(&assets)?;
        validate_theme_contract_version(&root, &parsed_items, &theme_contract)?;
        validate_theme_css_contract(&assets, &root, &parsed_items, &theme_contract)?;
        validate_built_in_registry_items(&parsed_items).map_err(|source| {
            SnapshotError::InvalidRegistryCatalog {
                logical_path: REGISTRY_ROOT_PATH.to_owned(),
                source,
            }
        })?;
        let schemas = parse_schemas(&assets)?;

        Ok(Self {
            assets,
            root,
            items,
            theme_contract,
            schemas,
        })
    }

    pub(crate) fn root(&self) -> &RegistryRoot {
        &self.root
    }

    pub(crate) fn item(&self, name: &str) -> Option<&BuiltInRegistryItemSnapshot> {
        self.items.get(name)
    }

    pub(crate) fn resolve_items(
        &self,
        names: &[String],
    ) -> Result<Vec<&BuiltInRegistryItemSnapshot>, SnapshotError> {
        let mut closure = BTreeSet::new();
        for name in names {
            self.collect_item_closure(name, &mut closure)?;
        }

        let mut resolved_names = BTreeSet::new();
        let mut resolved = Vec::with_capacity(closure.len());
        while resolved.len() < closure.len() {
            let next = closure.iter().find(|name| {
                !resolved_names.contains(*name)
                    && self.items[*name]
                        .item
                        .registry_dependencies
                        .iter()
                        .all(|dependency| resolved_names.contains(dependency))
            });
            let Some(next) = next else {
                let unresolved = closure
                    .iter()
                    .find(|name| !resolved_names.contains(*name))
                    .expect("an incomplete closure has an unresolved item");
                return Err(SnapshotError::InvalidRegistryCatalog {
                    logical_path: REGISTRY_ROOT_PATH.to_owned(),
                    source: RegistryError::DependencyCycle(unresolved.clone()),
                });
            };
            let next = next.clone();
            resolved_names.insert(next.clone());
            resolved.push(&self.items[&next]);
        }

        Ok(resolved)
    }

    pub(crate) fn registry_source(&self, source: &str) -> Result<&str, SnapshotError> {
        let logical_path = format!("registry/{source}");
        self.asset_text(&logical_path, None)
    }

    pub(crate) fn theme_contract(&self) -> &ThemeContract {
        &self.theme_contract
    }

    pub(crate) fn schema_count(&self) -> usize {
        self.schemas.len()
    }

    #[cfg(test)]
    pub(crate) fn schema(&self, logical_path: &str) -> Option<&Value> {
        self.schemas.get(logical_path)
    }

    fn collect_item_closure(
        &self,
        name: &str,
        seen: &mut BTreeSet<String>,
    ) -> Result<(), SnapshotError> {
        if !seen.insert(name.to_owned()) {
            return Ok(());
        }
        let item = self
            .items
            .get(name)
            .ok_or_else(|| SnapshotError::ItemNotFound(name.to_owned()))?;
        for dependency in &item.item.registry_dependencies {
            self.collect_item_closure(dependency, seen)?;
        }
        Ok(())
    }

    fn asset_text(
        &self,
        logical_path: &str,
        expected_kind: Option<EmbeddedAssetKind>,
    ) -> Result<&str, SnapshotError> {
        let asset = self
            .assets
            .get(logical_path)
            .ok_or_else(|| SnapshotError::MissingAsset {
                logical_path: logical_path.to_owned(),
            })?;
        if let Some(expected) = expected_kind
            && asset.kind != expected
        {
            return Err(SnapshotError::KindMismatch {
                logical_path: logical_path.to_owned(),
                expected,
                actual: asset.kind,
            });
        }
        Ok(&asset.content)
    }
}

#[derive(Debug)]
pub(crate) enum SnapshotError {
    Provider(AssetProviderError),
    MissingAsset {
        logical_path: String,
    },
    UnexpectedAsset {
        logical_path: String,
    },
    DuplicateAsset {
        logical_path: String,
    },
    UnsortedAsset {
        previous: String,
        logical_path: String,
    },
    KindMismatch {
        logical_path: String,
        expected: EmbeddedAssetKind,
        actual: EmbeddedAssetKind,
    },
    ParseJson {
        logical_path: String,
        source: serde_json::Error,
    },
    InvalidRegistryRoot {
        logical_path: String,
        source: RegistryError,
    },
    InvalidRegistryItem {
        logical_path: String,
        source: RegistryError,
    },
    RegistryItemIdentity {
        logical_path: String,
        expected: String,
        actual: String,
    },
    InvalidRegistryCatalog {
        logical_path: String,
        source: RegistryError,
    },
    InvalidThemeContract {
        logical_path: String,
        source: ThemeContractError,
    },
    InvalidThemeContractSchema {
        logical_path: String,
        pointer: &'static str,
        expected: String,
        actual: String,
    },
    ThemeContractVersionMismatch {
        manifest_path: String,
        contract_path: String,
        manifest_version: String,
        contract_version: String,
        expected: String,
    },
    InvalidThemeCss {
        logical_path: String,
        reason: String,
    },
    UnownedRuntimeAsset {
        logical_path: String,
    },
    DuplicateRuntimeAssetReference {
        logical_path: String,
        first_owner: String,
        second_owner: String,
    },
    SerializeItem {
        logical_path: String,
        source: serde_json::Error,
    },
    ItemNotFound(String),
}

impl SnapshotError {
    pub(crate) fn logical_path(&self) -> Option<&str> {
        match self {
            Self::Provider(source) => provider_error_path(source),
            Self::MissingAsset { logical_path }
            | Self::UnexpectedAsset { logical_path }
            | Self::DuplicateAsset { logical_path }
            | Self::KindMismatch { logical_path, .. }
            | Self::ParseJson { logical_path, .. }
            | Self::InvalidRegistryRoot { logical_path, .. }
            | Self::InvalidRegistryItem { logical_path, .. }
            | Self::RegistryItemIdentity { logical_path, .. }
            | Self::InvalidRegistryCatalog { logical_path, .. }
            | Self::InvalidThemeContract { logical_path, .. }
            | Self::InvalidThemeContractSchema { logical_path, .. }
            | Self::InvalidThemeCss { logical_path, .. }
            | Self::UnownedRuntimeAsset { logical_path }
            | Self::DuplicateRuntimeAssetReference { logical_path, .. }
            | Self::SerializeItem { logical_path, .. } => Some(logical_path),
            Self::ThemeContractVersionMismatch { manifest_path, .. } => Some(manifest_path),
            Self::UnsortedAsset { logical_path, .. } => Some(logical_path),
            Self::ItemNotFound(_) => None,
        }
    }
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(source) => source.fmt(formatter),
            Self::MissingAsset { logical_path } => {
                write!(formatter, "built-in asset is missing: {logical_path}")
            }
            Self::UnexpectedAsset { logical_path } => {
                write!(formatter, "unexpected built-in asset: {logical_path}")
            }
            Self::DuplicateAsset { logical_path } => {
                write!(formatter, "duplicate built-in asset: {logical_path}")
            }
            Self::UnsortedAsset {
                previous,
                logical_path,
            } => write!(
                formatter,
                "built-in assets are not strictly sorted: {logical_path} follows {previous}"
            ),
            Self::KindMismatch {
                logical_path,
                expected,
                actual,
            } => write!(
                formatter,
                "built-in asset {logical_path} has kind {actual}, expected {expected}"
            ),
            Self::ParseJson {
                logical_path,
                source,
            } => write!(
                formatter,
                "failed to parse built-in JSON {logical_path}: {source}"
            ),
            Self::InvalidRegistryRoot {
                logical_path,
                source,
            } => write!(
                formatter,
                "invalid built-in registry root {logical_path}: {source}"
            ),
            Self::InvalidRegistryItem {
                logical_path,
                source,
            } => write!(
                formatter,
                "invalid built-in registry item {logical_path}: {source}"
            ),
            Self::RegistryItemIdentity {
                logical_path,
                expected,
                actual,
            } => write!(
                formatter,
                "built-in registry item {logical_path} has name {actual}, expected {expected}"
            ),
            Self::InvalidRegistryCatalog {
                logical_path,
                source,
            } => write!(
                formatter,
                "invalid built-in registry catalog {logical_path}: {source}"
            ),
            Self::InvalidThemeContract {
                logical_path,
                source,
            } => write!(
                formatter,
                "invalid built-in theme contract {logical_path}: {source}"
            ),
            Self::InvalidThemeContractSchema {
                logical_path,
                pointer,
                expected,
                actual,
            } => write!(
                formatter,
                "invalid built-in schema {logical_path} at {pointer}: expected {expected}, got {actual}"
            ),
            Self::ThemeContractVersionMismatch {
                manifest_path,
                contract_path,
                manifest_version,
                contract_version,
                expected,
            } => write!(
                formatter,
                "theme contract version mismatch: {manifest_path} declares {manifest_version}, {contract_path} declares {contract_version}, runtime expects {expected}"
            ),
            Self::InvalidThemeCss {
                logical_path,
                reason,
            } => write!(
                formatter,
                "invalid built-in theme CSS {logical_path}: {reason}"
            ),
            Self::UnownedRuntimeAsset { logical_path } => write!(
                formatter,
                "built-in runtime asset has no manifest owner: {logical_path}"
            ),
            Self::DuplicateRuntimeAssetReference {
                logical_path,
                first_owner,
                second_owner,
            } => write!(
                formatter,
                "built-in runtime asset {logical_path} is referenced by both {first_owner} and {second_owner}"
            ),
            Self::SerializeItem {
                logical_path,
                source,
            } => write!(
                formatter,
                "failed to serialize built-in registry item {logical_path}: {source}"
            ),
            Self::ItemNotFound(name) => {
                write!(formatter, "built-in registry item not found: {name}")
            }
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Provider(source) => Some(source),
            Self::ParseJson { source, .. } | Self::SerializeItem { source, .. } => Some(source),
            Self::InvalidRegistryRoot { source, .. }
            | Self::InvalidRegistryItem { source, .. }
            | Self::InvalidRegistryCatalog { source, .. } => Some(source),
            Self::InvalidThemeContract { source, .. } => Some(source),
            Self::MissingAsset { .. }
            | Self::UnexpectedAsset { .. }
            | Self::DuplicateAsset { .. }
            | Self::UnsortedAsset { .. }
            | Self::KindMismatch { .. }
            | Self::RegistryItemIdentity { .. }
            | Self::InvalidThemeContractSchema { .. }
            | Self::ThemeContractVersionMismatch { .. }
            | Self::InvalidThemeCss { .. }
            | Self::UnownedRuntimeAsset { .. }
            | Self::DuplicateRuntimeAssetReference { .. }
            | Self::ItemNotFound(_) => None,
        }
    }
}

impl From<AssetProviderError> for SnapshotError {
    fn from(source: AssetProviderError) -> Self {
        Self::Provider(source)
    }
}

struct BuiltInRegistrySnapshotCell {
    snapshot: OnceLock<BuiltInRegistrySnapshot>,
    initialization: Mutex<()>,
}

impl BuiltInRegistrySnapshotCell {
    const fn new() -> Self {
        Self {
            snapshot: OnceLock::new(),
            initialization: Mutex::new(()),
        }
    }

    fn get_or_try_init(
        &self,
        provider: &dyn AssetProvider,
    ) -> Result<&BuiltInRegistrySnapshot, SnapshotError> {
        if let Some(snapshot) = self.snapshot.get() {
            return Ok(snapshot);
        }

        let _initialization = self
            .initialization
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(snapshot) = self.snapshot.get() {
            return Ok(snapshot);
        }

        let snapshot = BuiltInRegistrySnapshot::from_provider(provider)?;
        Ok(self.snapshot.get_or_init(|| snapshot))
    }
}

static BUILT_IN_SNAPSHOT: BuiltInRegistrySnapshotCell = BuiltInRegistrySnapshotCell::new();

pub(crate) fn built_in_registry_snapshot() -> Result<&'static BuiltInRegistrySnapshot, SnapshotError>
{
    BUILT_IN_SNAPSHOT.get_or_try_init(embedded_asset_provider())
}

fn own_exact_catalog(
    provider: &dyn AssetProvider,
) -> Result<BTreeMap<String, OwnedAsset>, SnapshotError> {
    let mut actual_inventory = BTreeMap::new();
    let mut previous = None::<String>;
    for asset in provider.assets() {
        let asset = asset?;
        if actual_inventory.contains_key(asset.logical_path()) {
            return Err(SnapshotError::DuplicateAsset {
                logical_path: asset.logical_path().to_owned(),
            });
        }
        if let Some(previous) = previous.as_deref()
            && asset.logical_path() < previous
        {
            return Err(SnapshotError::UnsortedAsset {
                previous: previous.to_owned(),
                logical_path: asset.logical_path().to_owned(),
            });
        }
        previous = Some(asset.logical_path().to_owned());
        actual_inventory.insert(asset.logical_path().to_owned(), asset);
    }
    debug_assert_eq!(provider.asset_count(), actual_inventory.len());

    let expected = embedded_asset_inventory().collect::<Vec<_>>();
    for &(logical_path, expected_kind) in &expected {
        let Some(actual) = actual_inventory.get(logical_path) else {
            return Err(SnapshotError::MissingAsset {
                logical_path: logical_path.to_owned(),
            });
        };
        if actual.kind() != expected_kind {
            return Err(SnapshotError::KindMismatch {
                logical_path: logical_path.to_owned(),
                expected: expected_kind,
                actual: actual.kind(),
            });
        }
    }
    let expected_paths = expected
        .iter()
        .map(|(logical_path, _)| *logical_path)
        .collect::<BTreeSet<_>>();
    if let Some(unexpected) = actual_inventory
        .keys()
        .find(|path| !expected_paths.contains(path.as_str()))
    {
        return Err(SnapshotError::UnexpectedAsset {
            logical_path: unexpected.clone(),
        });
    }

    let mut owned = BTreeMap::new();
    for (logical_path, asset) in actual_inventory {
        let content = asset.utf8()?;
        owned.insert(
            logical_path,
            OwnedAsset {
                kind: asset.kind(),
                content: Arc::from(content),
            },
        );
    }
    Ok(owned)
}

fn parse_root(assets: &BTreeMap<String, OwnedAsset>) -> Result<RegistryRoot, SnapshotError> {
    let input = asset_text(assets, REGISTRY_ROOT_PATH, EmbeddedAssetKind::Json)?;
    let root = parse_registry_root_raw_str(input).map_err(|source| SnapshotError::ParseJson {
        logical_path: REGISTRY_ROOT_PATH.to_owned(),
        source,
    })?;
    root.validate()
        .map_err(|source| SnapshotError::InvalidRegistryRoot {
            logical_path: REGISTRY_ROOT_PATH.to_owned(),
            source,
        })?;
    Ok(root)
}

fn parse_items(
    assets: &BTreeMap<String, OwnedAsset>,
    root: &RegistryRoot,
) -> Result<
    (
        BTreeMap<String, BuiltInRegistryItemSnapshot>,
        Vec<RegistryItem>,
    ),
    SnapshotError,
> {
    let mut snapshots = BTreeMap::new();
    let mut parsed = Vec::with_capacity(root.items.len());
    for entry in &root.items {
        let logical_path = format!("registry/{}", entry.path);
        let input = asset_text(assets, &logical_path, EmbeddedAssetKind::Json)?;
        let item =
            parse_registry_item_raw_str(input).map_err(|source| SnapshotError::ParseJson {
                logical_path: logical_path.clone(),
                source,
            })?;
        item.validate()
            .map_err(|source| SnapshotError::InvalidRegistryItem {
                logical_path: logical_path.clone(),
                source,
            })?;
        if item.name != entry.name {
            return Err(SnapshotError::RegistryItemIdentity {
                logical_path,
                expected: entry.name.clone(),
                actual: item.name,
            });
        }
        validate_referenced_sources(assets, &item)?;
        let targets = resolve_registry_targets(&item).map_err(|source| {
            SnapshotError::InvalidRegistryItem {
                logical_path: logical_path.clone(),
                source,
            }
        })?;
        let content_hash = item_content_hash(assets, &logical_path, &item)?;
        parsed.push(item.clone());
        snapshots.insert(
            item.name.clone(),
            BuiltInRegistryItemSnapshot {
                manifest_path: entry.path.clone(),
                item,
                targets,
                content_hash,
            },
        );
    }
    Ok((snapshots, parsed))
}

fn validate_referenced_sources(
    assets: &BTreeMap<String, OwnedAsset>,
    item: &RegistryItem,
) -> Result<(), SnapshotError> {
    for file in &item.files {
        asset_text(
            assets,
            &format!("registry/{}", file.source),
            EmbeddedAssetKind::Rust,
        )?;
    }
    for style in &item.styles {
        asset_text(
            assets,
            &format!("registry/{}", style.source),
            EmbeddedAssetKind::Css,
        )?;
    }
    Ok(())
}

fn validate_exact_asset_ownership(
    assets: &BTreeMap<String, OwnedAsset>,
    root: &RegistryRoot,
    items: &[RegistryItem],
) -> Result<(), SnapshotError> {
    let catalog_manifests = assets
        .iter()
        .filter_map(|(logical_path, asset)| {
            (asset.kind == EmbeddedAssetKind::Json
                && logical_path.starts_with("registry/")
                && logical_path != REGISTRY_ROOT_PATH
                && logical_path != THEME_CONTRACT_PATH)
                .then_some(logical_path.as_str())
        })
        .collect::<BTreeSet<_>>();
    let root_manifests = root
        .items
        .iter()
        .map(|entry| format!("registry/{}", entry.path))
        .collect::<BTreeSet<_>>();
    for logical_path in &catalog_manifests {
        if !root_manifests.contains(*logical_path) {
            return Err(SnapshotError::UnownedRuntimeAsset {
                logical_path: (*logical_path).to_owned(),
            });
        }
    }

    let manifest_by_name = root
        .items
        .iter()
        .map(|entry| (entry.name.as_str(), format!("registry/{}", entry.path)))
        .collect::<BTreeMap<_, _>>();
    let mut source_owners = BTreeMap::<String, String>::new();
    for item in items {
        let owner = manifest_by_name
            .get(item.name.as_str())
            .expect("parsed item identity was checked against the registry root");
        for source in item
            .files
            .iter()
            .map(|file| file.source.as_str())
            .chain(item.styles.iter().map(|style| style.source.as_str()))
        {
            let logical_path = format!("registry/{source}");
            if let Some(first_owner) = source_owners.insert(logical_path.clone(), owner.clone()) {
                return Err(SnapshotError::DuplicateRuntimeAssetReference {
                    logical_path,
                    first_owner,
                    second_owner: owner.clone(),
                });
            }
        }
    }

    for (logical_path, asset) in assets {
        if logical_path.starts_with("registry/")
            && matches!(asset.kind, EmbeddedAssetKind::Rust | EmbeddedAssetKind::Css)
            && !source_owners.contains_key(logical_path)
        {
            return Err(SnapshotError::UnownedRuntimeAsset {
                logical_path: logical_path.clone(),
            });
        }
    }
    Ok(())
}

fn validate_runtime_compatibility_sources(
    assets: &BTreeMap<String, OwnedAsset>,
    items: &[RegistryItem],
) -> Result<(), SnapshotError> {
    for logical_path in [
        "registry/ui/dialog/content.rs",
        "registry/ui/menu/content.rs",
    ] {
        let source = asset_text(assets, logical_path, EmbeddedAssetKind::Rust)?;
        for required in [
            "transition_cancel_handler()",
            "animation_cancel_handler()",
            "on:transitioncancel=",
            "on:animationcancel=",
        ] {
            if !source.contains(required) {
                return Err(invalid_runtime_compatibility(
                    logical_path,
                    format!("Presence ABI 2 binding {required:?}"),
                ));
            }
        }
    }

    let dialog = asset_text(
        assets,
        "registry/ui/dialog/content.rs",
        EmbeddedAssetKind::Rust,
    )?;
    for required in ["PortalMount", "<Portal", "portal_mount"] {
        if !dialog.contains(required) {
            return Err(invalid_runtime_compatibility(
                "registry/ui/dialog/content.rs",
                format!("portal ABI 1 binding {required:?}"),
            ));
        }
    }

    for item in items {
        for style in &item.styles {
            let logical_path = format!("registry/{}", style.source);
            let source = asset_text(assets, &logical_path, EmbeddedAssetKind::Css)?;
            let required = if item.name == "tokens" {
                "@layer leptos-ui-kit.tokens, leptos-ui-kit.themes, leptos-ui-kit.components;"
            } else {
                "@layer leptos-ui-kit.components {"
            };
            if source.matches(required).count() != 1 {
                return Err(invalid_runtime_compatibility(
                    &logical_path,
                    format!("exactly one layer ABI 1 declaration {required:?}"),
                ));
            }
        }
    }

    Ok(())
}

fn invalid_runtime_compatibility(logical_path: &str, expected: String) -> SnapshotError {
    SnapshotError::InvalidRegistryCatalog {
        logical_path: logical_path.to_owned(),
        source: RegistryError::InvalidValue {
            field: "compatibility",
            expected,
            actual: "required runtime source binding is missing or duplicated".to_owned(),
        },
    }
}

fn item_content_hash(
    assets: &BTreeMap<String, OwnedAsset>,
    logical_path: &str,
    item: &RegistryItem,
) -> Result<String, SnapshotError> {
    let metadata = serde_json::to_vec(item).map_err(|source| SnapshotError::SerializeItem {
        logical_path: logical_path.to_owned(),
        source,
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"leptos-ui-kit-registry-item-v1\0");
    hasher.update((metadata.len() as u64).to_be_bytes());
    hasher.update(&metadata);
    for source in item
        .files
        .iter()
        .map(|file| file.source.as_str())
        .chain(item.styles.iter().map(|style| style.source.as_str()))
    {
        let content = asset_text(
            assets,
            &format!("registry/{source}"),
            kind_for_source(source),
        )?;
        hasher.update(source.as_bytes());
        hasher.update([0]);
        hasher.update((content.len() as u64).to_be_bytes());
        hasher.update(content.as_bytes());
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn kind_for_source(source: &str) -> EmbeddedAssetKind {
    if source.ends_with(".rs") {
        EmbeddedAssetKind::Rust
    } else {
        EmbeddedAssetKind::Css
    }
}

fn parse_theme_contract(
    assets: &BTreeMap<String, OwnedAsset>,
) -> Result<ThemeContract, SnapshotError> {
    let input = asset_text(assets, THEME_CONTRACT_PATH, EmbeddedAssetKind::Json)?;
    parse_theme_contract_str(input).map_err(|source| SnapshotError::InvalidThemeContract {
        logical_path: THEME_CONTRACT_PATH.to_owned(),
        source,
    })
}

fn validate_theme_contract_version(
    root: &RegistryRoot,
    items: &[RegistryItem],
    contract: &ThemeContract,
) -> Result<(), SnapshotError> {
    let tokens = items
        .iter()
        .find(|item| item.name == "tokens")
        .ok_or_else(|| SnapshotError::InvalidRegistryCatalog {
            logical_path: REGISTRY_ROOT_PATH.to_owned(),
            source: RegistryError::BuiltInNotFound("tokens".to_owned()),
        })?;
    let manifest_path = root
        .items
        .iter()
        .find(|entry| entry.name == "tokens")
        .map(|entry| format!("registry/{}", entry.path))
        .unwrap_or_else(|| "registry/foundation/tokens.json".to_owned());
    let manifest_version = tokens
        .extra
        .get("themeContractVersion")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            tokens
                .extra
                .get("themeContractVersion")
                .map_or_else(|| "<missing>".to_owned(), Value::to_string)
        });

    if manifest_version == crate::THEME_CONTRACT_VERSION
        && contract.contract_version == crate::THEME_CONTRACT_VERSION
    {
        return Ok(());
    }

    Err(SnapshotError::ThemeContractVersionMismatch {
        manifest_path,
        contract_path: THEME_CONTRACT_PATH.to_owned(),
        manifest_version,
        contract_version: contract.contract_version.clone(),
        expected: crate::THEME_CONTRACT_VERSION.to_owned(),
    })
}

fn validate_theme_css_contract(
    assets: &BTreeMap<String, OwnedAsset>,
    root: &RegistryRoot,
    items: &[RegistryItem],
    contract: &ThemeContract,
) -> Result<(), SnapshotError> {
    let tokens_entry = root
        .items
        .iter()
        .find(|entry| entry.name == "tokens")
        .ok_or_else(|| invalid_theme_css(REGISTRY_ROOT_PATH, "missing tokens registry entry"))?;
    if format!("registry/{}", tokens_entry.path) != TOKENS_MANIFEST_PATH {
        return Err(invalid_theme_css(
            REGISTRY_ROOT_PATH,
            format!(
                "tokens registry entry must reference {}, got registry/{}",
                TOKENS_MANIFEST_PATH
                    .strip_prefix("registry/")
                    .expect("tokens manifest path has registry prefix"),
                tokens_entry.path
            ),
        ));
    }

    let tokens = items
        .iter()
        .find(|item| item.name == "tokens")
        .ok_or_else(|| invalid_theme_css(TOKENS_MANIFEST_PATH, "missing tokens manifest"))?;
    if tokens.styles.len() != 1
        || tokens.styles[0].source
            != TOKENS_CSS_PATH
                .strip_prefix("registry/")
                .expect("tokens CSS path has registry prefix")
        || tokens.styles[0].target.id != "tokens"
    {
        return Err(invalid_theme_css(
            TOKENS_MANIFEST_PATH,
            "tokens manifest must own exactly styles/tokens.css as managed block \"tokens\"",
        ));
    }

    let mut all_declarations = Vec::new();
    for (logical_path, asset) in assets {
        if asset.kind != EmbeddedAssetKind::Css {
            continue;
        }
        let masked = mask_css_comments_and_strings(logical_path, &asset.content)?;
        validate_balanced_css_blocks(logical_path, &masked)?;
        let root_occurrences = root_selector_occurrences(&masked);
        let expected_roots = usize::from(logical_path == TOKENS_CSS_PATH);
        if root_occurrences != expected_roots {
            return Err(invalid_theme_css(
                logical_path,
                format!(
                    "expected {expected_roots} built-in :root selector occurrences, found {root_occurrences}"
                ),
            ));
        }
        let exact_root_rules = root_rule_bodies(&masked).len();
        if exact_root_rules != expected_roots {
            return Err(invalid_theme_css(
                logical_path,
                format!(
                    "expected {expected_roots} exact kit-layer :root rules, found {exact_root_rules}"
                ),
            ));
        }
        all_declarations.extend(
            custom_property_declaration_names(&masked)
                .into_iter()
                .map(|name| (logical_path.as_str(), name)),
        );
    }

    let tokens_css = asset_text(assets, TOKENS_CSS_PATH, EmbeddedAssetKind::Css)?;
    let masked_tokens_css = mask_css_comments_and_strings(TOKENS_CSS_PATH, tokens_css)?;
    let root_body = root_rule_bodies(&masked_tokens_css)
        .into_iter()
        .next()
        .expect("the sole root block was established above");
    let declarations =
        parse_root_declarations(TOKENS_CSS_PATH, tokens_css, &masked_tokens_css, root_body)?;

    let color_scheme = declarations
        .iter()
        .filter(|(name, _)| name == "color-scheme")
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    if color_scheme != ["light"] {
        return Err(invalid_theme_css(
            TOKENS_CSS_PATH,
            format!(
                "the sole :root must declare color-scheme: light exactly once, got {color_scheme:?}"
            ),
        ));
    }

    let contract_defaults = contract
        .tokens
        .iter()
        .map(|token| (token.name.as_str(), token.default_value.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut css_defaults = BTreeMap::<&str, &str>::new();
    for (name, value) in &declarations {
        if !name.starts_with("--kit-") {
            continue;
        }
        if css_defaults.insert(name, value).is_some() {
            return Err(invalid_theme_css(
                TOKENS_CSS_PATH,
                format!("duplicate theme token declaration {name}"),
            ));
        }
    }

    if css_defaults != contract_defaults {
        let missing = contract_defaults
            .keys()
            .filter(|name| !css_defaults.contains_key(**name))
            .copied()
            .collect::<Vec<_>>();
        let extra = css_defaults
            .keys()
            .filter(|name| !contract_defaults.contains_key(**name))
            .copied()
            .collect::<Vec<_>>();
        let changed = contract_defaults
            .iter()
            .filter_map(|(name, expected)| {
                css_defaults
                    .get(name)
                    .filter(|actual| *actual != expected)
                    .map(|actual| format!("{name}: expected {expected:?}, got {actual:?}"))
            })
            .collect::<Vec<_>>();
        return Err(invalid_theme_css(
            TOKENS_CSS_PATH,
            format!(
                "theme token defaults differ from the contract; missing={missing:?}, extra={extra:?}, changed={changed:?}"
            ),
        ));
    }

    let mut declared_inventory = all_declarations
        .iter()
        .filter(|(_, name)| contract_defaults.contains_key(name.as_str()))
        .map(|(path, name)| (*path, name.as_str()))
        .collect::<Vec<_>>();
    declared_inventory.sort_unstable();
    let expected_inventory = contract_defaults
        .keys()
        .map(|name| (TOKENS_CSS_PATH, *name))
        .collect::<Vec<_>>();
    if declared_inventory != expected_inventory {
        return Err(invalid_theme_css(
            TOKENS_CSS_PATH,
            format!(
                "theme token declarations must occur exactly once in the sole built-in :root; got {declared_inventory:?}"
            ),
        ));
    }

    Ok(())
}

fn invalid_theme_css(logical_path: &str, reason: impl Into<String>) -> SnapshotError {
    SnapshotError::InvalidThemeCss {
        logical_path: logical_path.to_owned(),
        reason: reason.into(),
    }
}

fn mask_css_comments_and_strings(
    logical_path: &str,
    input: &str,
) -> Result<Vec<u8>, SnapshotError> {
    let bytes = input.as_bytes();
    let mut masked = bytes.to_vec();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"/*") {
            let start = index;
            index += 2;
            while index + 1 < bytes.len() && !bytes[index..].starts_with(b"*/") {
                index += 1;
            }
            if index + 1 == bytes.len() {
                return Err(invalid_theme_css(logical_path, "unterminated CSS comment"));
            }
            index += 2;
            masked[start..index].fill(b' ');
            continue;
        }
        if matches!(bytes[index], b'\'' | b'"') {
            let quote = bytes[index];
            let start = index;
            index += 1;
            let mut closed = false;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                    continue;
                }
                if bytes[index] == quote {
                    index += 1;
                    closed = true;
                    break;
                }
                index += 1;
            }
            if !closed {
                return Err(invalid_theme_css(logical_path, "unterminated CSS string"));
            }
            masked[start..index].fill(b' ');
            continue;
        }
        index += 1;
    }
    Ok(masked)
}

fn root_rule_bodies(masked: &[u8]) -> Vec<(usize, usize)> {
    let mut bodies = Vec::new();
    for (index, byte) in masked.iter().copied().enumerate() {
        if byte != b'{' {
            continue;
        }
        let prelude_start = masked[..index]
            .iter()
            .rposition(|byte| matches!(byte, b'{' | b'}' | b';'))
            .map_or(0, |delimiter| delimiter + 1);
        if trim_ascii(&masked[prelude_start..index]) != b":root" {
            continue;
        }
        let mut body_depth = 1_usize;
        let mut end = index + 1;
        while end < masked.len() && body_depth != 0 {
            match masked[end] {
                b'{' => body_depth += 1,
                b'}' => body_depth -= 1,
                _ => {}
            }
            end += 1;
        }
        if body_depth == 0 {
            bodies.push((index + 1, end - 1));
        }
    }
    bodies
}

fn validate_balanced_css_blocks(logical_path: &str, masked: &[u8]) -> Result<(), SnapshotError> {
    let mut depth = 0_usize;
    for byte in masked {
        match byte {
            b'{' => depth += 1,
            b'}' if depth == 0 => {
                return Err(invalid_theme_css(
                    logical_path,
                    "unexpected closing CSS block",
                ));
            }
            b'}' => depth -= 1,
            _ => {}
        }
    }
    if depth == 0 {
        Ok(())
    } else {
        Err(invalid_theme_css(
            logical_path,
            format!("{depth} unterminated CSS block(s)"),
        ))
    }
}

fn root_selector_occurrences(masked: &[u8]) -> usize {
    const ROOT: &[u8] = b":root";

    masked
        .windows(ROOT.len())
        .enumerate()
        .filter(|(index, candidate)| {
            candidate.eq_ignore_ascii_case(ROOT)
                && masked.get(index + ROOT.len()).is_none_or(|next| {
                    !next.is_ascii_alphanumeric() && !matches!(next, b'-' | b'_')
                })
        })
        .count()
}

fn custom_property_declaration_names(masked: &[u8]) -> Vec<String> {
    let mut declarations = Vec::new();
    let mut index = 0;
    while index + 6 <= masked.len() {
        if !masked[index..].starts_with(b"--kit-") {
            index += 1;
            continue;
        }
        let start = index;
        index += 6;
        while index < masked.len()
            && (masked[index].is_ascii_alphanumeric() || masked[index] == b'-')
        {
            index += 1;
        }
        let end = index;
        while index < masked.len() && masked[index].is_ascii_whitespace() {
            index += 1;
        }
        if masked.get(index) == Some(&b':') {
            declarations.push(String::from_utf8_lossy(&masked[start..end]).into_owned());
        }
    }
    declarations
}

fn parse_root_declarations(
    logical_path: &str,
    original: &str,
    masked: &[u8],
    (start, end): (usize, usize),
) -> Result<Vec<(String, String)>, SnapshotError> {
    let mut declarations = Vec::new();
    let mut segment_start = start;
    let mut parentheses = 0_usize;
    for index in start..=end {
        let byte = if index == end {
            b';'
        } else {
            masked.get(index).copied().unwrap_or(b';')
        };
        match byte {
            b'(' => parentheses += 1,
            b')' if parentheses > 0 => parentheses -= 1,
            b'{' | b'}' => {
                return Err(invalid_theme_css(
                    logical_path,
                    "nested blocks are not allowed in the built-in :root rule",
                ));
            }
            b';' if parentheses == 0 || index == end => {
                let segment_end = if index == end && byte != b';' {
                    index + 1
                } else {
                    index
                };
                let segment = trim_ascii(&masked[segment_start..segment_end]);
                if !segment.is_empty() {
                    let colon = segment
                        .iter()
                        .position(|byte| *byte == b':')
                        .ok_or_else(|| {
                            invalid_theme_css(
                                logical_path,
                                format!(
                                    "malformed declaration {:?}",
                                    String::from_utf8_lossy(segment)
                                ),
                            )
                        })?;
                    let absolute = segment.as_ptr() as usize - masked.as_ptr() as usize;
                    let name = original[absolute..absolute + colon].trim().to_owned();
                    let value = original[absolute + colon + 1..absolute + segment.len()]
                        .trim()
                        .to_owned();
                    if name.is_empty() || value.is_empty() {
                        return Err(invalid_theme_css(
                            logical_path,
                            format!("empty name or value in declaration {name:?}"),
                        ));
                    }
                    declarations.push((name, value));
                }
                segment_start = index + 1;
            }
            _ => {}
        }
    }
    if parentheses != 0 {
        return Err(invalid_theme_css(
            logical_path,
            "unbalanced parentheses in the built-in :root rule",
        ));
    }
    Ok(declarations)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

fn parse_schemas(
    assets: &BTreeMap<String, OwnedAsset>,
) -> Result<BTreeMap<String, Value>, SnapshotError> {
    let mut schemas = BTreeMap::new();
    for contract in &SCHEMA_CONTRACTS {
        let input = asset_text(assets, contract.logical_path, EmbeddedAssetKind::Json)?;
        let schema = serde_json::from_str(input).map_err(|source| SnapshotError::ParseJson {
            logical_path: contract.logical_path.to_owned(),
            source,
        })?;
        validate_schema_contract(contract, &schema)?;
        schemas.insert(contract.logical_path.to_owned(), schema);
    }
    let theme_schema = schemas
        .get(THEME_CONTRACT_SCHEMA_PATH)
        .expect("the exact schema inventory includes the theme schema");
    validate_theme_contract_schema_shape(Path::new(THEME_CONTRACT_SCHEMA_PATH), theme_schema)
        .map_err(|source| match source {
            RegistryHealthError::InvalidThemeContractSchema {
                pointer,
                expected,
                actual,
                ..
            } => SnapshotError::InvalidThemeContractSchema {
                logical_path: THEME_CONTRACT_SCHEMA_PATH.to_owned(),
                pointer,
                expected,
                actual,
            },
            other => SnapshotError::InvalidThemeContractSchema {
                logical_path: THEME_CONTRACT_SCHEMA_PATH.to_owned(),
                pointer: "/",
                expected: "the theme contract schema shape".to_owned(),
                actual: other.to_string(),
            },
        })?;
    Ok(schemas)
}

fn validate_schema_contract(
    contract: &SchemaContract,
    schema: &Value,
) -> Result<(), SnapshotError> {
    expect_schema_value(
        contract.logical_path,
        schema,
        "/$schema",
        &Value::String(JSON_SCHEMA_DRAFT_2020_12_URL.to_owned()),
    )?;
    expect_schema_value(
        contract.logical_path,
        schema,
        "/$id",
        &Value::String(contract.schema_id.to_owned()),
    )?;
    expect_schema_value(
        contract.logical_path,
        schema,
        "/type",
        &Value::String("object".to_owned()),
    )?;
    expect_schema_value(
        contract.logical_path,
        schema,
        "/additionalProperties",
        &Value::Bool(false),
    )?;
    expect_schema_string_set(
        contract.logical_path,
        schema,
        "/required",
        contract.required,
    )?;
    let properties = schema
        .pointer("/properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            invalid_schema_contract(
                contract.logical_path,
                "/properties",
                "an object containing every required property",
                schema.pointer("/properties"),
            )
        })?;
    for property in contract.required {
        if !properties.contains_key(*property) {
            return Err(invalid_schema_contract(
                contract.logical_path,
                "/properties",
                &format!("a definition for required property {property:?}"),
                schema.pointer("/properties"),
            ));
        }
    }
    expect_schema_value(
        contract.logical_path,
        schema,
        "/properties/$schema/const",
        &Value::String(contract.schema_id.to_owned()),
    )?;
    expect_schema_value(
        contract.logical_path,
        schema,
        "/properties/schemaVersion/const",
        &Value::String(contract.schema_version.to_owned()),
    )
}

fn expect_schema_value(
    logical_path: &str,
    schema: &Value,
    pointer: &'static str,
    expected: &Value,
) -> Result<(), SnapshotError> {
    let actual = schema.pointer(pointer);
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(invalid_schema_contract(
            logical_path,
            pointer,
            &expected.to_string(),
            actual,
        ))
    }
}

fn expect_schema_string_set(
    logical_path: &str,
    schema: &Value,
    pointer: &'static str,
    expected: &[&str],
) -> Result<(), SnapshotError> {
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    let actual_value = schema.pointer(pointer);
    let actual_values = actual_value.and_then(Value::as_array);
    let actual = actual_values.and_then(|values| {
        values
            .iter()
            .map(Value::as_str)
            .collect::<Option<BTreeSet<_>>>()
    });
    if actual.as_ref() == Some(&expected)
        && actual_values.is_some_and(|values| values.len() == expected.len())
    {
        Ok(())
    } else {
        Err(invalid_schema_contract(
            logical_path,
            pointer,
            &format!("the exact string set {expected:?}"),
            actual_value,
        ))
    }
}

fn invalid_schema_contract(
    logical_path: &str,
    pointer: &'static str,
    expected: &str,
    actual: Option<&Value>,
) -> SnapshotError {
    SnapshotError::InvalidThemeContractSchema {
        logical_path: logical_path.to_owned(),
        pointer,
        expected: expected.to_owned(),
        actual: actual.map_or_else(|| "<missing>".to_owned(), Value::to_string),
    }
}

fn asset_text<'a>(
    assets: &'a BTreeMap<String, OwnedAsset>,
    logical_path: &str,
    expected: EmbeddedAssetKind,
) -> Result<&'a str, SnapshotError> {
    let asset = assets
        .get(logical_path)
        .ok_or_else(|| SnapshotError::MissingAsset {
            logical_path: logical_path.to_owned(),
        })?;
    if asset.kind != expected {
        return Err(SnapshotError::KindMismatch {
            logical_path: logical_path.to_owned(),
            expected,
            actual: asset.kind,
        });
    }
    Ok(&asset.content)
}

fn provider_error_path(error: &AssetProviderError) -> Option<&str> {
    match error {
        AssetProviderError::InvalidLogicalPath { logical_path, .. }
        | AssetProviderError::Missing { logical_path }
        | AssetProviderError::KindMismatch { logical_path, .. }
        | AssetProviderError::NonUtf8 { logical_path, .. }
        | AssetProviderError::HashMismatch { logical_path, .. } => Some(logical_path),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    use serde_json::Value;

    use super::{BuiltInRegistrySnapshot, BuiltInRegistrySnapshotCell, SnapshotError};
    use crate::embedded_assets::{
        AssetIter, AssetProvider, AssetProviderError, AssetView, EmbeddedAssetKind,
        InMemoryAssetProvider,
    };

    struct CountingProvider {
        inner: InMemoryAssetProvider,
        enumerations: AtomicUsize,
        lookups: AtomicUsize,
        content_views: AtomicUsize,
    }

    #[derive(Clone, Copy)]
    enum InventoryMutation {
        Duplicate,
        Unsorted,
    }

    struct MutatedInventoryProvider {
        inner: InMemoryAssetProvider,
        mutation: InventoryMutation,
    }

    impl MutatedInventoryProvider {
        fn new(mutation: InventoryMutation) -> Self {
            Self {
                inner: InMemoryAssetProvider::from_embedded(),
                mutation,
            }
        }
    }

    impl AssetProvider for MutatedInventoryProvider {
        fn asset_count(&self) -> usize {
            self.inner.asset_count()
        }

        fn asset(&self, logical_path: &str) -> Result<AssetView<'_>, AssetProviderError> {
            self.inner.asset(logical_path)
        }

        fn assets(&self) -> AssetIter<'_> {
            let mut assets = self.inner.assets().collect::<Vec<_>>();
            match self.mutation {
                InventoryMutation::Duplicate => {
                    assets.insert(1, assets[0].clone());
                }
                InventoryMutation::Unsorted => assets.swap(0, 1),
            }
            Box::new(assets.into_iter())
        }
    }

    impl CountingProvider {
        fn new() -> Self {
            Self {
                inner: InMemoryAssetProvider::from_embedded(),
                enumerations: AtomicUsize::new(0),
                lookups: AtomicUsize::new(0),
                content_views: AtomicUsize::new(0),
            }
        }
    }

    impl AssetProvider for CountingProvider {
        fn asset_count(&self) -> usize {
            self.inner.asset_count()
        }

        fn asset(&self, logical_path: &str) -> Result<AssetView<'_>, AssetProviderError> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            self.inner.asset(logical_path)
        }

        fn assets(&self) -> AssetIter<'_> {
            self.enumerations.fetch_add(1, Ordering::SeqCst);
            Box::new(self.inner.assets().inspect(|_| {
                self.content_views.fetch_add(1, Ordering::SeqCst);
            }))
        }
    }

    #[test]
    fn snapshot_owns_all_runtime_data_after_the_provider_is_dropped() {
        let provider = InMemoryAssetProvider::from_embedded();
        let snapshot = BuiltInRegistrySnapshot::from_provider(&provider).expect("snapshot");
        drop(provider);

        assert_eq!(snapshot.root().name, "leptos-ui-kit");
        assert!(
            snapshot
                .registry_source("ui/button.rs")
                .unwrap()
                .contains("pub fn Button")
        );
        assert_eq!(
            snapshot.item("button").unwrap().manifest_path(),
            "ui/button.json"
        );
        assert_eq!(snapshot.theme_contract().contract_version, "1");
        assert!(
            snapshot
                .schema("schema/0.9.0-alpha/kit.schema.json")
                .is_some()
        );
    }

    #[test]
    fn snapshot_cache_reads_the_provider_once_for_repeated_health_and_item_access() {
        let provider = CountingProvider::new();
        let expected_content_views = provider.inner.asset_count();
        let snapshot_cell = BuiltInRegistrySnapshotCell::new();

        for _ in 0..4 {
            let snapshot = snapshot_cell
                .get_or_try_init(&provider)
                .expect("cached snapshot");
            assert_eq!(snapshot.root().name, "leptos-ui-kit");
            assert_eq!(snapshot.item("button").unwrap().item().name, "button");
            assert_eq!(
                snapshot
                    .resolve_items(&["button".to_owned()])
                    .expect("resolve cached item")
                    .last()
                    .expect("button closure")
                    .item()
                    .name,
                "button"
            );
            assert!(snapshot.registry_source("ui/button.rs").is_ok());
            assert_eq!(snapshot.theme_contract().contract_version, "1");
            assert_eq!(snapshot.schema_count(), 6);
        }
        assert_eq!(provider.enumerations.load(Ordering::SeqCst), 1);
        assert_eq!(provider.lookups.load(Ordering::SeqCst), 0);
        assert_eq!(
            provider.content_views.load(Ordering::SeqCst),
            expected_content_views
        );
    }

    #[test]
    fn concurrent_first_callers_construct_one_snapshot() {
        const CALLERS: usize = 8;

        let provider = Arc::new(CountingProvider::new());
        let expected_content_views = provider.inner.asset_count();
        let snapshot_cell = Arc::new(BuiltInRegistrySnapshotCell::new());
        let barrier = Arc::new(Barrier::new(CALLERS));
        let callers = (0..CALLERS)
            .map(|_| {
                let provider = Arc::clone(&provider);
                let snapshot_cell = Arc::clone(&snapshot_cell);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let snapshot = snapshot_cell
                        .get_or_try_init(provider.as_ref())
                        .expect("snapshot");
                    snapshot as *const BuiltInRegistrySnapshot as usize
                })
            })
            .collect::<Vec<_>>();
        let snapshot_addresses = callers
            .into_iter()
            .map(|caller| caller.join().expect("snapshot caller"))
            .collect::<Vec<_>>();

        assert!(
            snapshot_addresses
                .iter()
                .all(|address| *address == snapshot_addresses[0])
        );
        assert_eq!(provider.enumerations.load(Ordering::SeqCst), 1);
        assert_eq!(provider.lookups.load(Ordering::SeqCst), 0);
        assert_eq!(
            provider.content_views.load(Ordering::SeqCst),
            expected_content_views
        );
    }

    #[test]
    fn duplicate_and_unsorted_inventories_have_distinct_fault_classes() {
        let duplicate = MutatedInventoryProvider::new(InventoryMutation::Duplicate);
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&duplicate),
            Err(SnapshotError::DuplicateAsset { logical_path })
                if logical_path == "registry/contracts/theme-v1.json"
        ));

        let unsorted = MutatedInventoryProvider::new(InventoryMutation::Unsorted);
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&unsorted),
            Err(SnapshotError::UnsortedAsset {
                previous,
                logical_path,
            }) if previous == "registry/foundation/tokens.json"
                && logical_path == "registry/contracts/theme-v1.json"
        ));
    }

    #[test]
    fn every_runtime_asset_class_reports_missing_inputs_by_logical_path() {
        for logical_path in [
            "registry/registry.json",
            "registry/ui/button.json",
            "registry/contracts/theme-v1.json",
            "schema/0.9.0-alpha/kit.schema.json",
            "schema/0.9.0-alpha/registry-item.schema.json",
            "schema/0.9.0-alpha/registry.schema.json",
            "schema/0.9.0-alpha/theme-contract.schema.json",
            "registry/ui/button.rs",
            "registry/styles/button.css",
        ] {
            let mut provider = InMemoryAssetProvider::from_embedded();
            provider.remove(logical_path).expect("remove fixture asset");
            assert!(matches!(
                BuiltInRegistrySnapshot::from_provider(&provider),
                Err(SnapshotError::MissingAsset { logical_path: actual })
                    if actual == logical_path
            ));
        }
    }

    #[test]
    fn malformed_contract_and_every_schema_fail_with_logical_paths() {
        let mut contract = InMemoryAssetProvider::from_embedded();
        contract
            .set_bytes("registry/contracts/theme-v1.json", b"{")
            .unwrap();
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&contract),
            Err(SnapshotError::InvalidThemeContract { logical_path, .. })
                if logical_path == "registry/contracts/theme-v1.json"
        ));

        for logical_path in [
            "schema/0.9.0-alpha/kit.schema.json",
            "schema/0.9.0-alpha/registry-item.schema.json",
            "schema/0.9.0-alpha/registry.schema.json",
            "schema/0.9.0-alpha/theme-contract.schema.json",
        ] {
            let mut provider = InMemoryAssetProvider::from_embedded();
            provider.set_bytes(logical_path, b"{").unwrap();
            assert!(matches!(
                BuiltInRegistrySnapshot::from_provider(&provider),
                Err(SnapshotError::ParseJson { logical_path: actual, .. })
                    if actual == logical_path
            ));
        }
    }

    #[test]
    fn every_schema_rejects_syntactically_valid_identity_corruption() {
        for logical_path in super::SCHEMA_PATHS {
            let mut provider = InMemoryAssetProvider::from_embedded();
            mutate_json(&mut provider, logical_path, |schema| {
                schema["$id"] = Value::String("https://invalid.example/schema.json".to_owned());
            });
            assert!(matches!(
                BuiltInRegistrySnapshot::from_provider(&provider),
                Err(SnapshotError::InvalidThemeContractSchema {
                    logical_path: actual,
                    ..
                }) if actual == logical_path
            ));
        }
    }

    #[test]
    fn root_must_own_every_catalog_manifest() {
        let mut provider = InMemoryAssetProvider::from_embedded();
        mutate_json(&mut provider, super::REGISTRY_ROOT_PATH, |root| {
            root["items"]
                .as_array_mut()
                .expect("registry root items")
                .retain(|entry| entry["name"] != "button");
        });

        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&provider),
            Err(SnapshotError::UnownedRuntimeAsset { logical_path })
                if logical_path == "registry/ui/button.json"
        ));
    }

    #[test]
    fn every_catalog_rust_and_css_source_requires_exactly_one_owner() {
        for (field, expected) in [
            ("files", "registry/ui/button.rs"),
            ("styles", "registry/styles/button.css"),
        ] {
            let mut provider = InMemoryAssetProvider::from_embedded();
            mutate_json(&mut provider, "registry/ui/button.json", |item| {
                item[field] = Value::Array(Vec::new());
            });
            assert!(matches!(
                BuiltInRegistrySnapshot::from_provider(&provider),
                Err(SnapshotError::UnownedRuntimeAsset { logical_path })
                    if logical_path == expected
            ));
        }

        let mut duplicate = InMemoryAssetProvider::from_embedded();
        mutate_json(&mut duplicate, "registry/ui/button.json", |item| {
            let files = item["files"].as_array_mut().expect("button files");
            let mut second = files[0].clone();
            second["target"]["path"] = Value::String("button_copy.rs".to_owned());
            files.push(second);
        });
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&duplicate),
            Err(SnapshotError::DuplicateRuntimeAssetReference {
                logical_path,
                first_owner,
                second_owner,
            }) if logical_path == "registry/ui/button.rs"
                && first_owner == "registry/ui/button.json"
                && second_owner == "registry/ui/button.json"
        ));
    }

    #[test]
    fn snapshot_reports_logical_missing_kind_utf8_hash_and_json_failures() {
        let mut missing = InMemoryAssetProvider::from_embedded();
        missing.remove("registry/registry.json").unwrap();
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&missing),
            Err(SnapshotError::MissingAsset { logical_path })
                if logical_path == "registry/registry.json"
        ));

        let mut kind = InMemoryAssetProvider::from_embedded();
        kind.set_kind("registry/styles/button.css", EmbeddedAssetKind::Rust)
            .unwrap();
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&kind),
            Err(SnapshotError::KindMismatch { logical_path, .. })
                if logical_path == "registry/styles/button.css"
        ));

        let mut non_utf8 = InMemoryAssetProvider::from_embedded();
        non_utf8.set_bytes("registry/ui/button.rs", [0xff]).unwrap();
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&non_utf8),
            Err(SnapshotError::Provider(source))
                if source.to_string().contains("registry/ui/button.rs")
        ));

        let mut hash = InMemoryAssetProvider::from_embedded();
        hash.set_declared_hash("registry/registry.json", "sha256:wrong")
            .unwrap();
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&hash),
            Err(SnapshotError::Provider(source))
                if source.to_string().contains("registry/registry.json")
        ));

        let mut json = InMemoryAssetProvider::from_embedded();
        json.set_bytes("registry/ui/button.json", b"{").unwrap();
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&json),
            Err(SnapshotError::ParseJson { logical_path, .. })
                if logical_path == "registry/ui/button.json"
        ));
    }

    #[test]
    fn source_bytes_and_item_hash_share_the_owned_snapshot() {
        let baseline =
            BuiltInRegistrySnapshot::from_provider(&InMemoryAssetProvider::from_embedded())
                .expect("baseline");
        let mut changed = InMemoryAssetProvider::from_embedded();
        let replacement = "// changed fixture\npub fn Button() {}\n";
        changed
            .set_bytes("registry/ui/button.rs", replacement.as_bytes())
            .unwrap();
        let changed = BuiltInRegistrySnapshot::from_provider(&changed).expect("changed snapshot");

        assert_eq!(
            changed.registry_source("ui/button.rs").unwrap(),
            replacement
        );
        assert_ne!(
            baseline.item("button").unwrap().content_hash(),
            changed.item("button").unwrap().content_hash()
        );
    }

    #[test]
    fn snapshot_enforces_exact_theme_contract_css_defaults() {
        for (case, mutate, expected_fragment) in [
            (
                "missing",
                ("  --kit-color-canvas: #f8fafc;\n", ""),
                "missing=[\"--kit-color-canvas\"]",
            ),
            (
                "changed",
                ("--kit-color-canvas: #f8fafc", "--kit-color-canvas: #000000"),
                "changed=[\"--kit-color-canvas:",
            ),
            (
                "extra",
                (
                    "  color-scheme: light;\n",
                    "  color-scheme: light;\n  --kit-color-extra: currentColor;\n",
                ),
                "extra=[\"--kit-color-extra\"]",
            ),
            (
                "duplicate",
                (
                    "  --kit-color-canvas: #f8fafc;\n",
                    "  --kit-color-canvas: #f8fafc;\n  --kit-color-canvas: #f8fafc;\n",
                ),
                "duplicate theme token declaration --kit-color-canvas",
            ),
        ] {
            let mut provider = InMemoryAssetProvider::from_embedded();
            let css = provider
                .utf8_asset(super::TOKENS_CSS_PATH, EmbeddedAssetKind::Css)
                .expect("read tokens CSS");
            let changed = css.replacen(mutate.0, mutate.1, 1);
            assert_ne!(changed, css, "{case} mutation must change the fixture");
            provider
                .set_bytes(super::TOKENS_CSS_PATH, changed.into_bytes())
                .expect("replace tokens CSS");

            let error = BuiltInRegistrySnapshot::from_provider(&provider)
                .expect_err("theme CSS drift must reject the snapshot");
            assert!(
                matches!(
                    &error,
                    SnapshotError::InvalidThemeCss { logical_path, .. }
                        if logical_path == super::TOKENS_CSS_PATH
                ),
                "{case}: {error}"
            );
            assert!(
                error.to_string().contains(expected_fragment),
                "{case}: {error}"
            );
        }
    }

    #[test]
    fn snapshot_requires_one_built_in_root_and_one_declaration_location() {
        let mut duplicate_root = InMemoryAssetProvider::from_embedded();
        let css = duplicate_root
            .utf8_asset(super::TOKENS_CSS_PATH, EmbeddedAssetKind::Css)
            .expect("read tokens CSS");
        duplicate_root
            .set_bytes(
                super::TOKENS_CSS_PATH,
                format!("{css}\n:root {{ color-scheme: light; }}\n").into_bytes(),
            )
            .expect("append duplicate root");
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&duplicate_root),
            Err(SnapshotError::InvalidThemeCss { logical_path, reason })
                if logical_path == super::TOKENS_CSS_PATH
                    && reason.contains("expected 1 built-in :root selector occurrences")
        ));

        let mut component_root = InMemoryAssetProvider::from_embedded();
        let path = "registry/styles/button.css";
        let css = component_root
            .utf8_asset(path, EmbeddedAssetKind::Css)
            .expect("read component CSS");
        component_root
            .set_bytes(
                path,
                format!("{css}\n:root {{ --kit-color-canvas: #f8fafc; }}\n").into_bytes(),
            )
            .expect("append component root");
        assert!(matches!(
            BuiltInRegistrySnapshot::from_provider(&component_root),
            Err(SnapshotError::InvalidThemeCss { logical_path, reason })
                if logical_path == path
                    && reason.contains("expected 0 built-in :root selector occurrences")
        ));
    }

    #[test]
    fn theme_css_scanner_rejects_nested_selector_list_and_unbalanced_roots() {
        for (path, suffix, expected) in [
            (
                "registry/styles/button.css",
                "\n@media (min-width: 1px) { :root { color-scheme: light; } }\n",
                "expected 0 built-in :root selector occurrences",
            ),
            (
                "registry/styles/button.css",
                "\n:root, .application { color-scheme: light; }\n",
                "expected 0 built-in :root selector occurrences",
            ),
            (
                "registry/styles/button.css",
                "\n.application {\n",
                "unterminated CSS block",
            ),
        ] {
            let mut provider = InMemoryAssetProvider::from_embedded();
            let css = provider
                .utf8_asset(path, EmbeddedAssetKind::Css)
                .expect("read component CSS");
            provider
                .set_bytes(path, format!("{css}{suffix}").into_bytes())
                .expect("append invalid CSS");

            assert!(matches!(
                BuiltInRegistrySnapshot::from_provider(&provider),
                Err(SnapshotError::InvalidThemeCss { logical_path, reason })
                    if logical_path == path && reason.contains(expected)
            ));
        }
    }

    #[test]
    fn theme_css_scanner_ignores_comments_strings_and_variable_uses() {
        let mut provider = InMemoryAssetProvider::from_embedded();
        let path = "registry/styles/button.css";
        let css = provider
            .utf8_asset(path, EmbeddedAssetKind::Css)
            .expect("read button CSS");
        provider
            .set_bytes(
                path,
                format!(
                    "{css}\n/* :root {{ --kit-fake: red; }} */\n.fake::before {{ content: \":root {{ --kit-string: red; }}\"; color: var(--kit-color-text); }}\n"
                )
                .into_bytes(),
            )
            .expect("append ignored CSS syntax");

        BuiltInRegistrySnapshot::from_provider(&provider)
            .expect("comments, strings, and var uses are not declarations");
    }

    fn mutate_json(
        provider: &mut InMemoryAssetProvider,
        logical_path: &str,
        mutate: impl FnOnce(&mut Value),
    ) {
        let input = provider
            .utf8_asset(logical_path, EmbeddedAssetKind::Json)
            .expect("read JSON fixture");
        let mut value = serde_json::from_str::<Value>(input).expect("parse JSON fixture");
        mutate(&mut value);
        provider
            .set_bytes(
                logical_path,
                serde_json::to_vec_pretty(&value).expect("serialize JSON fixture"),
            )
            .expect("replace JSON fixture");
    }
}
