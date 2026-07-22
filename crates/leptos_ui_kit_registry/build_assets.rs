use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

pub const GENERATED_CATALOG_FILE: &str = "embedded_assets.rs";
pub const SNAPSHOT_DIRECTORY: &str = "leptos_ui_kit_embedded_assets";
pub const ASSET_ROOTS: [&str; 2] = ["registry", "schema"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AssetKind {
    Json,
    Rust,
    Css,
}

impl AssetKind {
    const fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Rust => "rs",
            Self::Css => "css",
        }
    }

    const fn generated_variant(self) -> &'static str {
        match self {
            Self::Json => "Json",
            Self::Rust => "Rust",
            Self::Css => "Css",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetSpec {
    pub source_path: &'static str,
    pub logical_path: &'static str,
    pub kind: AssetKind,
}

impl AssetSpec {
    pub const fn new(
        source_path: &'static str,
        logical_path: &'static str,
        kind: AssetKind,
    ) -> Self {
        Self {
            source_path,
            logical_path,
            kind,
        }
    }

    const fn same(path: &'static str, kind: AssetKind) -> Self {
        Self::new(path, path, kind)
    }
}

pub const ASSET_SPECS: [AssetSpec; 104] = [
    AssetSpec::same(
        "registry/contracts/component-customization-v1.json",
        AssetKind::Json,
    ),
    AssetSpec::same("registry/contracts/theme-v1.json", AssetKind::Json),
    AssetSpec::same("registry/foundation/tokens.json", AssetKind::Json),
    AssetSpec::same("registry/registry.json", AssetKind::Json),
    AssetSpec::same("registry/styles/alert.css", AssetKind::Css),
    AssetSpec::same("registry/styles/anchor.css", AssetKind::Css),
    AssetSpec::same("registry/styles/avatar.css", AssetKind::Css),
    AssetSpec::same("registry/styles/badge.css", AssetKind::Css),
    AssetSpec::same("registry/styles/button.css", AssetKind::Css),
    AssetSpec::same("registry/styles/card.css", AssetKind::Css),
    AssetSpec::same("registry/styles/checkbox.css", AssetKind::Css),
    AssetSpec::same("registry/styles/collapsible.css", AssetKind::Css),
    AssetSpec::same("registry/styles/dialog.css", AssetKind::Css),
    AssetSpec::same("registry/styles/field.css", AssetKind::Css),
    AssetSpec::same("registry/styles/menu.css", AssetKind::Css),
    AssetSpec::same("registry/styles/progress.css", AssetKind::Css),
    AssetSpec::same("registry/styles/radio.css", AssetKind::Css),
    AssetSpec::same("registry/styles/separator.css", AssetKind::Css),
    AssetSpec::same("registry/styles/skeleton.css", AssetKind::Css),
    AssetSpec::same("registry/styles/spinner.css", AssetKind::Css),
    AssetSpec::same("registry/styles/status.css", AssetKind::Css),
    AssetSpec::same("registry/styles/switch.css", AssetKind::Css),
    AssetSpec::same("registry/styles/tabs.css", AssetKind::Css),
    AssetSpec::same("registry/styles/tokens.css", AssetKind::Css),
    AssetSpec::same("registry/ui/alert.json", AssetKind::Json),
    AssetSpec::same("registry/ui/alert.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/anchor.json", AssetKind::Json),
    AssetSpec::same("registry/ui/anchor.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/avatar.json", AssetKind::Json),
    AssetSpec::same("registry/ui/avatar.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/badge.json", AssetKind::Json),
    AssetSpec::same("registry/ui/badge.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/button.json", AssetKind::Json),
    AssetSpec::same("registry/ui/button.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/card.json", AssetKind::Json),
    AssetSpec::same("registry/ui/card.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/checkbox.json", AssetKind::Json),
    AssetSpec::same("registry/ui/checkbox.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/collapsible.json", AssetKind::Json),
    AssetSpec::same("registry/ui/collapsible/content.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/collapsible/mod.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/collapsible/root.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/collapsible/trigger.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/dialog.json", AssetKind::Json),
    AssetSpec::same("registry/ui/dialog/close.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/dialog/content.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/dialog/description.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/dialog/mod.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/dialog/root.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/dialog/title.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/dialog/trigger.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field.json", AssetKind::Json),
    AssetSpec::same("registry/ui/field/label.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/message.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/mod.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/native_select.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/required.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/root.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/select_field.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/slot.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/surface.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/text_area.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/text_area_field.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/text_field.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/field/text_input.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/identity.json", AssetKind::Json),
    AssetSpec::same("registry/ui/identity.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/menu.json", AssetKind::Json),
    AssetSpec::same("registry/ui/menu/content.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/menu/item.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/menu/item_indicator.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/menu/mod.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/menu/radio_item.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/menu/root.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/menu/trigger.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/progress.json", AssetKind::Json),
    AssetSpec::same("registry/ui/progress.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/radio.json", AssetKind::Json),
    AssetSpec::same("registry/ui/radio.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/router-link.json", AssetKind::Json),
    AssetSpec::same("registry/ui/router_link.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/separator.json", AssetKind::Json),
    AssetSpec::same("registry/ui/separator.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/skeleton.json", AssetKind::Json),
    AssetSpec::same("registry/ui/skeleton.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/spinner.json", AssetKind::Json),
    AssetSpec::same("registry/ui/spinner.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/status.json", AssetKind::Json),
    AssetSpec::same("registry/ui/status.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/switch.json", AssetKind::Json),
    AssetSpec::same("registry/ui/switch.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/tabs.json", AssetKind::Json),
    AssetSpec::same("registry/ui/tabs/list.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/tabs/mod.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/tabs/panel.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/tabs/root.rs", AssetKind::Rust),
    AssetSpec::same("registry/ui/tabs/trigger.rs", AssetKind::Rust),
    AssetSpec::same(
        "schema/0.2.0/theme-integration.schema.json",
        AssetKind::Json,
    ),
    AssetSpec::same("schema/0.2.0/token-contract.schema.json", AssetKind::Json),
    AssetSpec::same(
        "schema/0.9.0-alpha/component-customization.schema.json",
        AssetKind::Json,
    ),
    AssetSpec::same("schema/0.9.0-alpha/kit.schema.json", AssetKind::Json),
    AssetSpec::same(
        "schema/0.9.0-alpha/registry-item.schema.json",
        AssetKind::Json,
    ),
    AssetSpec::same("schema/0.9.0-alpha/registry.schema.json", AssetKind::Json),
    AssetSpec::same(
        "schema/0.9.0-alpha/theme-contract.schema.json",
        AssetKind::Json,
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedAsset {
    pub source_path: String,
    pub logical_path: String,
    pub kind: AssetKind,
    pub content_hash: String,
    pub byte_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedCatalog {
    pub assets: Vec<GeneratedAsset>,
    pub catalog_hash: String,
    pub rust_source: String,
    pub rerun_paths: BTreeSet<PathBuf>,
    pub generated_path: PathBuf,
    pub snapshot_root: PathBuf,
}

#[derive(Debug)]
pub enum AssetCatalogError {
    DuplicateSource(String),
    DuplicateLogical(String),
    CaseFoldCollision {
        first: String,
        second: String,
    },
    UnsafePath {
        path: String,
        reason: &'static str,
    },
    MissingInput(String),
    UnexpectedInput(String),
    Symlink(String),
    UnexpectedType(String),
    NonUtf8Path(String),
    NonUtf8Content(String),
    InvalidJson {
        path: String,
        source: serde_json::Error,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for AssetCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateSource(path) => write!(formatter, "duplicate asset source: {path}"),
            Self::DuplicateLogical(path) => write!(formatter, "duplicate logical asset: {path}"),
            Self::CaseFoldCollision { first, second } => write!(
                formatter,
                "portable asset path collision between {first} and {second}"
            ),
            Self::UnsafePath { path, reason } => {
                write!(formatter, "unsafe asset path {path:?}: {reason}")
            }
            Self::MissingInput(path) => write!(formatter, "missing asset input: {path}"),
            Self::UnexpectedInput(path) => write!(formatter, "unexpected asset input: {path}"),
            Self::Symlink(path) => write!(formatter, "asset input must not be a symlink: {path}"),
            Self::UnexpectedType(path) => {
                write!(formatter, "asset input has an unexpected file type: {path}")
            }
            Self::NonUtf8Path(parent) => {
                write!(formatter, "asset path below {parent} is not valid UTF-8")
            }
            Self::NonUtf8Content(path) => {
                write!(formatter, "asset content is not valid UTF-8: {path}")
            }
            Self::InvalidJson { path, source } => {
                write!(formatter, "invalid JSON asset {path}: {source}")
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for AssetCatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidJson { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::DuplicateSource(_)
            | Self::DuplicateLogical(_)
            | Self::CaseFoldCollision { .. }
            | Self::UnsafePath { .. }
            | Self::MissingInput(_)
            | Self::UnexpectedInput(_)
            | Self::Symlink(_)
            | Self::UnexpectedType(_)
            | Self::NonUtf8Path(_)
            | Self::NonUtf8Content(_) => None,
        }
    }
}

struct ValidatedAsset {
    generated: GeneratedAsset,
    bytes: Vec<u8>,
}

pub fn generate_asset_catalog(
    package_root: &Path,
    out_dir: &Path,
) -> Result<GeneratedCatalog, AssetCatalogError> {
    generate_asset_catalog_with(package_root, out_dir, &ASSET_SPECS, &ASSET_ROOTS)
}

pub fn generate_asset_catalog_with(
    package_root: &Path,
    out_dir: &Path,
    specs: &[AssetSpec],
    roots: &[&str],
) -> Result<GeneratedCatalog, AssetCatalogError> {
    validate_asset_specs(specs, roots)?;
    let (actual, rerun_paths) = scan_asset_roots(package_root, specs, roots)?;
    let mut validated = Vec::with_capacity(specs.len());

    for spec in specs {
        let path = actual
            .get(spec.source_path)
            .ok_or_else(|| AssetCatalogError::MissingInput(spec.source_path.to_owned()))?;
        let bytes = fs::read(path).map_err(|source| AssetCatalogError::Io {
            operation: "read asset",
            path: path.clone(),
            source,
        })?;
        if std::str::from_utf8(&bytes).is_err() {
            return Err(AssetCatalogError::NonUtf8Content(
                spec.source_path.to_owned(),
            ));
        }
        if spec.kind == AssetKind::Json {
            serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|source| {
                AssetCatalogError::InvalidJson {
                    path: spec.source_path.to_owned(),
                    source,
                }
            })?;
        }
        validated.push(ValidatedAsset {
            generated: GeneratedAsset {
                source_path: spec.source_path.to_owned(),
                logical_path: spec.logical_path.to_owned(),
                kind: spec.kind,
                content_hash: sha256(&bytes),
                byte_len: bytes.len(),
            },
            bytes,
        });
    }

    validated.sort_by(|left, right| {
        left.generated
            .logical_path
            .cmp(&right.generated.logical_path)
    });
    let catalog_hash = catalog_hash(&validated);
    let rust_source = render_catalog(&validated, &catalog_hash);
    let snapshot_root = out_dir.join(SNAPSHOT_DIRECTORY);
    replace_snapshot_directory(&snapshot_root)?;
    for asset in &validated {
        let path = snapshot_root.join(&asset.generated.logical_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| AssetCatalogError::Io {
                operation: "create snapshot directory",
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&path, &asset.bytes).map_err(|source| AssetCatalogError::Io {
            operation: "write asset snapshot",
            path,
            source,
        })?;
    }

    let generated_path = out_dir.join(GENERATED_CATALOG_FILE);
    fs::write(&generated_path, &rust_source).map_err(|source| AssetCatalogError::Io {
        operation: "write generated asset catalog",
        path: generated_path.clone(),
        source,
    })?;

    Ok(GeneratedCatalog {
        assets: validated.into_iter().map(|asset| asset.generated).collect(),
        catalog_hash,
        rust_source,
        rerun_paths,
        generated_path,
        snapshot_root,
    })
}

pub fn validate_asset_specs(specs: &[AssetSpec], roots: &[&str]) -> Result<(), AssetCatalogError> {
    let mut sources = BTreeSet::new();
    let mut logical = BTreeSet::new();
    let mut portable = BTreeMap::<String, String>::new();
    for root in roots {
        validate_portable_path(root)?;
    }
    for spec in specs {
        validate_portable_path(spec.source_path)?;
        validate_portable_path(spec.logical_path)?;
        if !roots.iter().any(|root| {
            spec.source_path == *root || spec.source_path.starts_with(&format!("{root}/"))
        }) {
            return Err(AssetCatalogError::UnsafePath {
                path: spec.source_path.to_owned(),
                reason: "source is outside the declared asset roots",
            });
        }
        for path in [spec.source_path, spec.logical_path] {
            if Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str())
                != Some(spec.kind.extension())
            {
                return Err(AssetCatalogError::UnsafePath {
                    path: path.to_owned(),
                    reason: "extension does not match the declared asset kind",
                });
            }
        }
        if !sources.insert(spec.source_path) {
            return Err(AssetCatalogError::DuplicateSource(
                spec.source_path.to_owned(),
            ));
        }
        if !logical.insert(spec.logical_path) {
            return Err(AssetCatalogError::DuplicateLogical(
                spec.logical_path.to_owned(),
            ));
        }
        for value in [spec.source_path, spec.logical_path] {
            let folded = portable_case_fold(value);
            if let Some(first) = portable.insert(folded, value.to_owned())
                && first != value
            {
                return Err(AssetCatalogError::CaseFoldCollision {
                    first,
                    second: value.to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn scan_asset_roots(
    package_root: &Path,
    specs: &[AssetSpec],
    roots: &[&str],
) -> Result<(BTreeMap<String, PathBuf>, BTreeSet<PathBuf>), AssetCatalogError> {
    let expected_files = specs
        .iter()
        .map(|spec| spec.source_path.to_owned())
        .collect::<BTreeSet<_>>();
    let expected_directories = expected_directories(specs, roots);
    let mut actual = BTreeMap::new();
    let mut rerun_paths = BTreeSet::new();
    let mut portable = BTreeMap::new();

    for root in roots {
        let path = package_root.join(root);
        inspect_root(
            package_root,
            &path,
            root,
            &expected_files,
            &expected_directories,
            &mut actual,
            &mut rerun_paths,
            &mut portable,
        )?;
    }
    for expected in expected_files {
        if !actual.contains_key(&expected) {
            return Err(AssetCatalogError::MissingInput(expected));
        }
    }
    Ok((actual, rerun_paths))
}

#[allow(clippy::too_many_arguments)]
fn inspect_root(
    package_root: &Path,
    path: &Path,
    logical: &str,
    expected_files: &BTreeSet<String>,
    expected_directories: &BTreeSet<String>,
    actual: &mut BTreeMap<String, PathBuf>,
    rerun_paths: &mut BTreeSet<PathBuf>,
    portable: &mut BTreeMap<String, String>,
) -> Result<(), AssetCatalogError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            AssetCatalogError::MissingInput(logical.to_owned())
        } else {
            AssetCatalogError::Io {
                operation: "inspect asset input",
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    rerun_paths.insert(path.to_path_buf());
    if metadata.file_type().is_symlink() {
        return Err(AssetCatalogError::Symlink(logical.to_owned()));
    }
    if !metadata.is_dir() {
        return Err(AssetCatalogError::UnexpectedType(logical.to_owned()));
    }
    record_portable_path(logical, portable)?;
    inspect_directory(
        package_root,
        path,
        logical,
        expected_files,
        expected_directories,
        actual,
        rerun_paths,
        portable,
    )
}

#[allow(clippy::too_many_arguments)]
fn inspect_directory(
    package_root: &Path,
    directory: &Path,
    logical_directory: &str,
    expected_files: &BTreeSet<String>,
    expected_directories: &BTreeSet<String>,
    actual: &mut BTreeMap<String, PathBuf>,
    rerun_paths: &mut BTreeSet<PathBuf>,
    portable: &mut BTreeMap<String, String>,
) -> Result<(), AssetCatalogError> {
    rerun_paths.insert(directory.to_path_buf());
    let mut entries = fs::read_dir(directory)
        .map_err(|source| AssetCatalogError::Io {
            operation: "read asset directory",
            path: directory.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| AssetCatalogError::Io {
            operation: "read asset directory entry",
            path: directory.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(AssetCatalogError::NonUtf8Path(logical_directory.to_owned()));
        };
        let logical = format!("{logical_directory}/{name}");
        validate_portable_path(&logical)?;
        record_portable_path(&logical, portable)?;
        let path = package_root.join(&logical);
        let metadata = fs::symlink_metadata(&path).map_err(|source| AssetCatalogError::Io {
            operation: "inspect asset input",
            path: path.clone(),
            source,
        })?;
        rerun_paths.insert(path.clone());
        if metadata.file_type().is_symlink() {
            return Err(AssetCatalogError::Symlink(logical));
        }
        if metadata.is_dir() {
            if expected_files.contains(&logical) {
                return Err(AssetCatalogError::UnexpectedType(logical));
            }
            if !expected_directories.contains(&logical) {
                return Err(AssetCatalogError::UnexpectedInput(logical));
            }
            inspect_directory(
                package_root,
                &path,
                &logical,
                expected_files,
                expected_directories,
                actual,
                rerun_paths,
                portable,
            )?;
        } else if metadata.is_file() {
            if expected_directories.contains(&logical) {
                return Err(AssetCatalogError::UnexpectedType(logical));
            }
            if !expected_files.contains(&logical) {
                return Err(AssetCatalogError::UnexpectedInput(logical));
            }
            if actual.insert(logical.clone(), path).is_some() {
                return Err(AssetCatalogError::DuplicateSource(logical));
            }
        } else {
            return Err(AssetCatalogError::UnexpectedType(logical));
        }
    }
    Ok(())
}

fn expected_directories(specs: &[AssetSpec], roots: &[&str]) -> BTreeSet<String> {
    let mut directories = roots
        .iter()
        .map(|root| (*root).to_owned())
        .collect::<BTreeSet<_>>();
    for spec in specs {
        let mut current = String::new();
        let mut segments = spec.source_path.split('/').peekable();
        while let Some(segment) = segments.next() {
            if segments.peek().is_none() {
                break;
            }
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(segment);
            directories.insert(current.clone());
        }
    }
    directories
}

fn validate_portable_path(path: &str) -> Result<(), AssetCatalogError> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return Err(unsafe_path(
            path,
            "expected a non-empty forward-slash relative path",
        ));
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.starts_with('.') {
            return Err(unsafe_path(
                path,
                "contains an empty, dot, parent, or hidden segment",
            ));
        }
        if segment.ends_with('.') || segment.ends_with(' ') {
            return Err(unsafe_path(
                path,
                "contains a segment ending in a dot or space",
            ));
        }
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(unsafe_path(path, "contains a non-portable character"));
        }
        let stem = segment
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if is_windows_reserved_name(&stem) {
            return Err(unsafe_path(path, "contains a Windows reserved device name"));
        }
    }
    Ok(())
}

fn unsafe_path(path: &str, reason: &'static str) -> AssetCatalogError {
    AssetCatalogError::UnsafePath {
        path: path.to_owned(),
        reason,
    }
}

fn is_windows_reserved_name(value: &str) -> bool {
    matches!(value, "con" | "prn" | "aux" | "nul" | "clock$")
        || value
            .strip_prefix("com")
            .or_else(|| value.strip_prefix("lpt"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
}

fn portable_case_fold(path: &str) -> String {
    path.to_ascii_lowercase()
}

fn record_portable_path(
    path: &str,
    portable: &mut BTreeMap<String, String>,
) -> Result<(), AssetCatalogError> {
    let folded = portable_case_fold(path);
    if let Some(first) = portable.insert(folded, path.to_owned())
        && first != path
    {
        return Err(AssetCatalogError::CaseFoldCollision {
            first,
            second: path.to_owned(),
        });
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn catalog_hash(assets: &[ValidatedAsset]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"leptos-ui-kit-embedded-catalog-v1\0");
    for asset in assets {
        let path = asset.generated.logical_path.as_bytes();
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path);
        hasher.update((asset.bytes.len() as u64).to_be_bytes());
        hasher.update(&asset.bytes);
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn render_catalog(assets: &[ValidatedAsset], catalog_hash: &str) -> String {
    let mut output = String::from("// @generated by build_assets.rs; do not edit.\n\n");
    output.push_str(&format!(
        "pub(crate) const EMBEDDED_ASSET_COUNT: usize = {};\n",
        assets.len()
    ));
    output.push_str(&format!(
        "pub(crate) const EMBEDDED_CATALOG_HASH: &str = {catalog_hash:?};\n\n"
    ));
    output.push_str("pub(crate) static EMBEDDED_ASSETS: &[EmbeddedAsset] = &[\n");
    for asset in assets {
        output.push_str("    EmbeddedAsset {\n");
        output.push_str(&format!(
            "        logical_path: {:?},\n",
            asset.generated.logical_path
        ));
        output.push_str(&format!(
            "        kind: EmbeddedAssetKind::{},\n",
            asset.generated.kind.generated_variant()
        ));
        output.push_str(&format!(
            "        content: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{SNAPSHOT_DIRECTORY}/{}\")),\n",
            asset.generated.logical_path
        ));
        output.push_str(&format!(
            "        content_hash: {:?},\n",
            asset.generated.content_hash
        ));
        output.push_str("    },\n");
    }
    output.push_str("];\n");
    output
}

fn replace_snapshot_directory(path: &Path) -> Result<(), AssetCatalogError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path).map_err(|source| AssetCatalogError::Io {
                operation: "remove stale snapshot directory",
                path: path.to_path_buf(),
                source,
            })?;
        }
        Ok(_) => {
            fs::remove_file(path).map_err(|source| AssetCatalogError::Io {
                operation: "remove stale snapshot path",
                path: path.to_path_buf(),
                source,
            })?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(AssetCatalogError::Io {
                operation: "inspect stale snapshot path",
                path: path.to_path_buf(),
                source,
            });
        }
    }
    fs::create_dir_all(path).map_err(|source| AssetCatalogError::Io {
        operation: "create snapshot root",
        path: path.to_path_buf(),
        source,
    })
}
