use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::fs;

use serde_json::{Value, json};

use crate::{
    BuiltInAssetError, RegistryError, SCHEMA_VERSION, THEME_CONTRACT_NAME,
    THEME_CONTRACT_SCHEMA_URL, THEME_CONTRACT_VERSION, ThemeContractError,
    builtin_registry::{SnapshotError, built_in_registry_snapshot},
    item::built_in_asset_kind,
};

#[cfg(test)]
use crate::{parse_registry_item_str, parse_registry_root_str, parse_theme_contract_str};

const JSON_SCHEMA_DRAFT_2020_12_URL: &str = "https://json-schema.org/draft/2020-12/schema";
const THEME_TOKEN_NAME_PATTERN: &str = "^--kit-[a-z][a-z0-9-]*$";

/// The role of a packaged file involved in built-in registry validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryHealthFileKind {
    RegistryRoot,
    RegistryItem,
    RegistrySource,
    ThemeContract,
    ThemeContractSchema,
    PublicSchema,
}

impl fmt::Display for RegistryHealthFileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RegistryRoot => write!(f, "registry root"),
            Self::RegistryItem => write!(f, "registry item"),
            Self::RegistrySource => write!(f, "registry source"),
            Self::ThemeContract => write!(f, "theme contract"),
            Self::ThemeContractSchema => write!(f, "theme contract schema"),
            Self::PublicSchema => write!(f, "public schema"),
        }
    }
}

/// A failure in the packaged built-in registry health contract.
#[derive(Debug)]
pub enum RegistryHealthError {
    MissingFile {
        kind: RegistryHealthFileKind,
        path: PathBuf,
    },
    ReadFile {
        kind: RegistryHealthFileKind,
        path: PathBuf,
        source: std::io::Error,
    },
    NonUtf8File {
        kind: RegistryHealthFileKind,
        path: PathBuf,
        source: std::string::FromUtf8Error,
    },
    BuiltInAsset {
        kind: RegistryHealthFileKind,
        source: BuiltInAssetError,
    },
    ParseJson {
        kind: RegistryHealthFileKind,
        path: PathBuf,
        source: serde_json::Error,
    },
    InvalidRegistryRoot {
        path: PathBuf,
        source: RegistryError,
    },
    InvalidRegistryItem {
        path: PathBuf,
        source: RegistryError,
    },
    RegistryItemIdentity {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    InvalidRegistryCatalog {
        path: PathBuf,
        source: RegistryError,
    },
    InvalidThemeContract {
        path: PathBuf,
        source: ThemeContractError,
    },
    InvalidThemeContractSchema {
        path: PathBuf,
        pointer: &'static str,
        expected: String,
        actual: String,
    },
    InvalidPublicSchema {
        path: PathBuf,
        pointer: &'static str,
        expected: String,
        actual: String,
    },
    ThemeContractVersionMismatch {
        manifest_path: PathBuf,
        contract_path: PathBuf,
        manifest_version: String,
        contract_version: String,
        expected: String,
    },
}

impl fmt::Display for RegistryHealthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFile { kind, path } => {
                write!(f, "packaged {kind} is missing: {}", path.display())
            }
            Self::ReadFile { kind, path, source } => {
                write!(
                    f,
                    "failed to read packaged {kind} {}: {source}",
                    path.display()
                )
            }
            Self::NonUtf8File { kind, path, .. } => {
                write!(f, "packaged {kind} is not UTF-8: {}", path.display())
            }
            Self::BuiltInAsset { kind, source } => {
                write!(f, "invalid packaged {kind}: {source}")
            }
            Self::ParseJson { kind, path, source } => {
                write!(
                    f,
                    "failed to parse packaged {kind} {}: {source}",
                    path.display()
                )
            }
            Self::InvalidRegistryRoot { path, source } => {
                write!(
                    f,
                    "invalid packaged registry root {}: {source}",
                    path.display()
                )
            }
            Self::InvalidRegistryItem { path, source } => {
                write!(
                    f,
                    "invalid packaged registry item {}: {source}",
                    path.display()
                )
            }
            Self::RegistryItemIdentity {
                path,
                expected,
                actual,
            } => write!(
                f,
                "packaged registry item {} has name {actual}, expected {expected}",
                path.display()
            ),
            Self::InvalidRegistryCatalog { path, source } => write!(
                f,
                "invalid packaged registry catalog {}: {source}",
                path.display()
            ),
            Self::InvalidThemeContract { path, source } => {
                write!(
                    f,
                    "invalid packaged theme contract {}: {source}",
                    path.display()
                )
            }
            Self::InvalidThemeContractSchema {
                path,
                pointer,
                expected,
                actual,
            } => write!(
                f,
                "invalid packaged theme contract schema {} at {pointer}: expected {expected}, got {actual}",
                path.display()
            ),
            Self::InvalidPublicSchema {
                path,
                pointer,
                expected,
                actual,
            } => write!(
                f,
                "invalid packaged public schema {} at {pointer}: expected {expected}, got {actual}",
                path.display()
            ),
            Self::ThemeContractVersionMismatch {
                manifest_path,
                contract_path,
                manifest_version,
                contract_version,
                expected,
            } => write!(
                f,
                "theme contract version mismatch: {} declares {manifest_version}, {} declares {contract_version}, runtime expects {expected}",
                manifest_path.display(),
                contract_path.display()
            ),
        }
    }
}

impl std::error::Error for RegistryHealthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadFile { source, .. } => Some(source),
            Self::NonUtf8File { source, .. } => Some(source),
            Self::BuiltInAsset { source, .. } => Some(source),
            Self::ParseJson { source, .. } => Some(source),
            Self::InvalidRegistryRoot { source, .. }
            | Self::InvalidRegistryItem { source, .. }
            | Self::InvalidRegistryCatalog { source, .. } => Some(source),
            Self::InvalidThemeContract { source, .. } => Some(source),
            Self::MissingFile { .. }
            | Self::RegistryItemIdentity { .. }
            | Self::InvalidThemeContractSchema { .. }
            | Self::InvalidPublicSchema { .. }
            | Self::ThemeContractVersionMismatch { .. } => None,
        }
    }
}

/// Validates every runtime-relevant file in the packaged built-in registry.
#[allow(
    clippy::result_large_err,
    reason = "the stable public health error preserves typed paths, versions, and source diagnostics"
)]
pub fn validate_built_in_registry_health() -> Result<(), RegistryHealthError> {
    let snapshot = built_in_registry_snapshot().map_err(snapshot_health_error)?;
    debug_assert_eq!(snapshot.schema_count(), 4);
    Ok(())
}

#[cfg(test)]
pub(crate) fn validate_built_in_registry_health_at(
    registry_root: &Path,
) -> Result<(), RegistryHealthError> {
    validate_built_in_registry_health_with_schema_at(
        registry_root,
        &registry_root.join("contracts/theme-contract.schema.json"),
    )
}

#[cfg(test)]
fn validate_built_in_registry_health_with_schema_at(
    registry_root: &Path,
    schema_path: &Path,
) -> Result<(), RegistryHealthError> {
    let root_path = registry_root.join("registry.json");
    let root_input = read_utf8(RegistryHealthFileKind::RegistryRoot, &root_path)?;
    let root =
        parse_registry_root_str(&root_input).map_err(|source| RegistryHealthError::ParseJson {
            kind: RegistryHealthFileKind::RegistryRoot,
            path: root_path.clone(),
            source,
        })?;
    root.validate()
        .map_err(|source| RegistryHealthError::InvalidRegistryRoot {
            path: root_path.clone(),
            source,
        })?;

    let mut items = Vec::with_capacity(root.items.len());
    let mut manifest_paths = Vec::with_capacity(root.items.len());
    for entry in &root.items {
        let path = registry_root.join(&entry.path);
        let input = read_utf8(RegistryHealthFileKind::RegistryItem, &path)?;
        let item =
            parse_registry_item_str(&input).map_err(|source| RegistryHealthError::ParseJson {
                kind: RegistryHealthFileKind::RegistryItem,
                path: path.clone(),
                source,
            })?;
        item.validate()
            .map_err(|source| RegistryHealthError::InvalidRegistryItem {
                path: path.clone(),
                source,
            })?;
        if item.name != entry.name {
            return Err(RegistryHealthError::RegistryItemIdentity {
                path,
                expected: entry.name.clone(),
                actual: item.name,
            });
        }
        manifest_paths.push((entry.name.as_str(), registry_root.join(&entry.path)));
        items.push(item);
    }

    let contract_path = registry_root.join("contracts/theme-v1.json");
    let contract_input = read_utf8(RegistryHealthFileKind::ThemeContract, &contract_path)?;
    let contract_raw = serde_json::from_str::<Value>(&contract_input).map_err(|source| {
        RegistryHealthError::ParseJson {
            kind: RegistryHealthFileKind::ThemeContract,
            path: contract_path.clone(),
            source,
        }
    })?;
    let contract = parse_theme_contract_str(&contract_input).map_err(|source| {
        RegistryHealthError::InvalidThemeContract {
            path: contract_path.clone(),
            source,
        }
    })?;

    let schema_input = read_utf8(RegistryHealthFileKind::ThemeContractSchema, schema_path)?;
    let schema = serde_json::from_str::<Value>(&schema_input).map_err(|source| {
        RegistryHealthError::ParseJson {
            kind: RegistryHealthFileKind::ThemeContractSchema,
            path: schema_path.to_path_buf(),
            source,
        }
    })?;
    validate_theme_contract_schema_shape(schema_path, &schema)?;

    let (tokens, tokens_path) = items
        .iter()
        .zip(manifest_paths.iter())
        .find(|(item, _)| item.name == "tokens")
        .map(|(item, (_, path))| (item, path))
        .ok_or_else(|| RegistryHealthError::InvalidRegistryCatalog {
            path: root_path.clone(),
            source: RegistryError::BuiltInNotFound("tokens".to_owned()),
        })?;
    let manifest_version = tokens
        .extra
        .get("themeContractVersion")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| render_value(tokens.extra.get("themeContractVersion")));
    let raw_contract_version = contract_raw
        .get("contractVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if manifest_version != THEME_CONTRACT_VERSION
        || raw_contract_version != THEME_CONTRACT_VERSION
        || contract.contract_version != THEME_CONTRACT_VERSION
    {
        return Err(RegistryHealthError::ThemeContractVersionMismatch {
            manifest_path: tokens_path.clone(),
            contract_path,
            manifest_version,
            contract_version: contract.contract_version,
            expected: THEME_CONTRACT_VERSION.to_owned(),
        });
    }

    crate::item::validate_built_in_registry_items(&items).map_err(|source| {
        RegistryHealthError::InvalidRegistryCatalog {
            path: root_path,
            source,
        }
    })?;

    let mut sources = BTreeSet::new();
    for item in &items {
        sources.extend(item.files.iter().map(|file| file.source.as_str()));
        sources.extend(item.styles.iter().map(|style| style.source.as_str()));
    }
    for source in sources {
        read_utf8(
            RegistryHealthFileKind::RegistrySource,
            &registry_root.join(source),
        )?;
    }

    Ok(())
}

#[cfg(test)]
fn read_utf8(kind: RegistryHealthFileKind, path: &Path) -> Result<String, RegistryHealthError> {
    let bytes = fs::read(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            RegistryHealthError::MissingFile {
                kind,
                path: path.to_path_buf(),
            }
        } else {
            RegistryHealthError::ReadFile {
                kind,
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    String::from_utf8(bytes).map_err(|source| RegistryHealthError::NonUtf8File {
        kind,
        path: path.to_path_buf(),
        source,
    })
}

#[allow(
    clippy::result_large_err,
    reason = "schema validation preserves the stable typed registry health error"
)]
pub(crate) fn validate_theme_contract_schema_shape(
    path: &Path,
    schema: &Value,
) -> Result<(), RegistryHealthError> {
    expect_schema_value(
        path,
        schema,
        "/$schema",
        &json!(JSON_SCHEMA_DRAFT_2020_12_URL),
    )?;
    expect_schema_value(path, schema, "/$id", &json!(THEME_CONTRACT_SCHEMA_URL))?;
    expect_schema_value(path, schema, "/type", &json!("object"))?;
    expect_schema_value(path, schema, "/additionalProperties", &json!(false))?;
    expect_schema_string_set(
        path,
        schema,
        "/required",
        &[
            "$schema",
            "schemaVersion",
            "contractVersion",
            "name",
            "tokens",
        ],
    )?;
    expect_schema_object_keys(
        path,
        schema,
        "/properties",
        &[
            "$schema",
            "schemaVersion",
            "contractVersion",
            "name",
            "tokens",
        ],
    )?;

    for (pointer, expected) in [
        (
            "/properties/$schema/const",
            json!(THEME_CONTRACT_SCHEMA_URL),
        ),
        ("/properties/schemaVersion/const", json!(SCHEMA_VERSION)),
        (
            "/properties/contractVersion/const",
            json!(THEME_CONTRACT_VERSION),
        ),
        ("/properties/name/const", json!(THEME_CONTRACT_NAME)),
        ("/properties/tokens/type", json!("array")),
        ("/properties/tokens/minItems", json!(1)),
        ("/properties/tokens/items/type", json!("object")),
        (
            "/properties/tokens/items/additionalProperties",
            json!(false),
        ),
        (
            "/properties/tokens/items/properties/name/type",
            json!("string"),
        ),
        (
            "/properties/tokens/items/properties/name/pattern",
            json!(THEME_TOKEN_NAME_PATTERN),
        ),
        (
            "/properties/tokens/items/properties/required/type",
            json!("boolean"),
        ),
        (
            "/properties/tokens/items/properties/default/type",
            json!("string"),
        ),
        (
            "/properties/tokens/items/properties/default/minLength",
            json!(1),
        ),
        (
            "/properties/tokens/items/properties/description/type",
            json!("string"),
        ),
        (
            "/properties/tokens/items/properties/description/minLength",
            json!(1),
        ),
    ] {
        expect_schema_value(path, schema, pointer, &expected)?;
    }
    expect_schema_string_set(
        path,
        schema,
        "/properties/tokens/items/required",
        &["name", "category", "required", "default", "description"],
    )?;
    expect_schema_object_keys(
        path,
        schema,
        "/properties/tokens/items/properties",
        &["name", "category", "required", "default", "description"],
    )?;
    expect_schema_string_set(
        path,
        schema,
        "/properties/tokens/items/properties/category/enum",
        &["color", "shape", "elevation", "motion", "state"],
    )
}

#[allow(
    clippy::result_large_err,
    reason = "schema validation preserves the stable typed registry health error"
)]
fn expect_schema_value(
    path: &Path,
    schema: &Value,
    pointer: &'static str,
    expected: &Value,
) -> Result<(), RegistryHealthError> {
    let actual = schema.pointer(pointer);
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(schema_shape_error(
            path,
            pointer,
            expected.to_string(),
            render_value(actual),
        ))
    }
}

#[allow(
    clippy::result_large_err,
    reason = "schema validation preserves the stable typed registry health error"
)]
fn expect_schema_string_set(
    path: &Path,
    schema: &Value,
    pointer: &'static str,
    expected: &[&str],
) -> Result<(), RegistryHealthError> {
    let actual_value = schema.pointer(pointer);
    let actual = actual_value.and_then(Value::as_array).and_then(|values| {
        values
            .iter()
            .map(Value::as_str)
            .collect::<Option<BTreeSet<_>>>()
            .map(|set| (values.len(), set))
    });
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual
        .as_ref()
        .is_some_and(|(length, set)| *length == expected.len() && set == &expected)
    {
        Ok(())
    } else {
        Err(schema_shape_error(
            path,
            pointer,
            format!("the exact string set {expected:?}"),
            render_value(actual_value),
        ))
    }
}

#[allow(
    clippy::result_large_err,
    reason = "schema validation preserves the stable typed registry health error"
)]
fn expect_schema_object_keys(
    path: &Path,
    schema: &Value,
    pointer: &'static str,
    expected: &[&str],
) -> Result<(), RegistryHealthError> {
    let actual_value = schema.pointer(pointer);
    let actual = actual_value
        .and_then(Value::as_object)
        .map(|object| object.keys().map(String::as_str).collect::<BTreeSet<_>>());
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual.as_ref() == Some(&expected) {
        Ok(())
    } else {
        Err(schema_shape_error(
            path,
            pointer,
            format!("the exact property set {expected:?}"),
            render_value(actual_value),
        ))
    }
}

fn schema_shape_error(
    path: &Path,
    pointer: &'static str,
    expected: String,
    actual: String,
) -> RegistryHealthError {
    RegistryHealthError::InvalidThemeContractSchema {
        path: path.to_path_buf(),
        pointer,
        expected,
        actual,
    }
}

fn render_value(value: Option<&Value>) -> String {
    value.map_or_else(|| "<missing>".to_owned(), Value::to_string)
}

#[cfg(test)]
fn built_in_registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("registry")
}

#[cfg(test)]
fn built_in_theme_contract_schema_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/0.9.0-alpha/theme-contract.schema.json")
}

fn snapshot_health_error(error: SnapshotError) -> RegistryHealthError {
    match error {
        SnapshotError::Provider(source) => health_asset_error(source.into()),
        SnapshotError::MissingAsset { logical_path } => {
            health_asset_error(BuiltInAssetError::Missing {
                logical_path: logical_path.into(),
            })
        }
        SnapshotError::UnexpectedAsset { logical_path } => {
            health_asset_error(BuiltInAssetError::Unexpected {
                logical_path: logical_path.into(),
            })
        }
        SnapshotError::DuplicateAsset { logical_path } => {
            health_asset_error(BuiltInAssetError::Duplicate {
                logical_path: logical_path.into(),
            })
        }
        SnapshotError::UnsortedAsset {
            previous,
            logical_path,
        } => health_asset_error(BuiltInAssetError::Unsorted {
            previous: previous.into(),
            logical_path: logical_path.into(),
        }),
        SnapshotError::KindMismatch {
            logical_path,
            expected,
            actual,
        } => health_asset_error(BuiltInAssetError::KindMismatch {
            logical_path: logical_path.into(),
            expected: built_in_asset_kind(expected),
            actual: built_in_asset_kind(actual),
        }),
        SnapshotError::UnownedRuntimeAsset { logical_path } => {
            health_asset_error(BuiltInAssetError::UnownedRuntimeAsset {
                logical_path: logical_path.into(),
            })
        }
        SnapshotError::DuplicateRuntimeAssetReference {
            logical_path,
            first_owner,
            second_owner,
        } => health_asset_error(BuiltInAssetError::DuplicateRuntimeAssetReference {
            logical_path: logical_path.into(),
            first_owner,
            second_owner,
        }),
        SnapshotError::ParseJson {
            logical_path,
            source,
        } => RegistryHealthError::ParseJson {
            kind: file_kind_for_logical_path(&logical_path),
            path: logical_path.into(),
            source,
        },
        SnapshotError::InvalidRegistryRoot {
            logical_path,
            source,
        } => RegistryHealthError::InvalidRegistryRoot {
            path: logical_path.into(),
            source,
        },
        SnapshotError::InvalidRegistryItem {
            logical_path,
            source,
        } => RegistryHealthError::InvalidRegistryItem {
            path: logical_path.into(),
            source,
        },
        SnapshotError::RegistryItemIdentity {
            logical_path,
            expected,
            actual,
        } => RegistryHealthError::RegistryItemIdentity {
            path: logical_path.into(),
            expected,
            actual,
        },
        SnapshotError::InvalidRegistryCatalog {
            logical_path,
            source,
        } => RegistryHealthError::InvalidRegistryCatalog {
            path: logical_path.into(),
            source,
        },
        SnapshotError::InvalidThemeContract {
            logical_path,
            source: ThemeContractError::Parse(source),
        } => RegistryHealthError::ParseJson {
            kind: RegistryHealthFileKind::ThemeContract,
            path: logical_path.into(),
            source,
        },
        SnapshotError::InvalidThemeContract {
            logical_path,
            source,
        } => RegistryHealthError::InvalidThemeContract {
            path: logical_path.into(),
            source,
        },
        SnapshotError::InvalidThemeContractSchema {
            logical_path,
            pointer,
            expected,
            actual,
        } => {
            if logical_path == THEME_CONTRACT_SCHEMA_LOGICAL_PATH {
                RegistryHealthError::InvalidThemeContractSchema {
                    path: logical_path.into(),
                    pointer,
                    expected,
                    actual,
                }
            } else {
                RegistryHealthError::InvalidPublicSchema {
                    path: logical_path.into(),
                    pointer,
                    expected,
                    actual,
                }
            }
        }
        SnapshotError::ThemeContractVersionMismatch {
            manifest_path,
            contract_path,
            manifest_version,
            contract_version,
            expected,
        } => RegistryHealthError::ThemeContractVersionMismatch {
            manifest_path: manifest_path.into(),
            contract_path: contract_path.into(),
            manifest_version,
            contract_version,
            expected,
        },
        SnapshotError::SerializeItem {
            logical_path,
            source,
        } => RegistryHealthError::InvalidRegistryCatalog {
            path: logical_path.into(),
            source: RegistryError::Serialize(source),
        },
        SnapshotError::ItemNotFound(name) => RegistryHealthError::InvalidRegistryCatalog {
            path: REGISTRY_ROOT_LOGICAL_PATH.into(),
            source: RegistryError::BuiltInNotFound(name),
        },
    }
}

fn health_asset_error(source: BuiltInAssetError) -> RegistryHealthError {
    let logical_path = source.logical_path().to_string_lossy().into_owned();
    let kind = file_kind_for_logical_path(&logical_path);
    match source {
        BuiltInAssetError::Missing { logical_path } => RegistryHealthError::MissingFile {
            kind,
            path: logical_path,
        },
        source => RegistryHealthError::BuiltInAsset { kind, source },
    }
}

const REGISTRY_ROOT_LOGICAL_PATH: &str = "registry/registry.json";
const THEME_CONTRACT_SCHEMA_LOGICAL_PATH: &str = "schema/0.9.0-alpha/theme-contract.schema.json";

fn file_kind_for_logical_path(logical_path: &str) -> RegistryHealthFileKind {
    match logical_path {
        REGISTRY_ROOT_LOGICAL_PATH => RegistryHealthFileKind::RegistryRoot,
        "registry/contracts/theme-v1.json" => RegistryHealthFileKind::ThemeContract,
        THEME_CONTRACT_SCHEMA_LOGICAL_PATH => RegistryHealthFileKind::ThemeContractSchema,
        path if path.starts_with("schema/") => RegistryHealthFileKind::PublicSchema,
        path if path.ends_with(".rs") || path.ends_with(".css") => {
            RegistryHealthFileKind::RegistrySource
        }
        _ => RegistryHealthFileKind::RegistryItem,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::{
        RegistryHealthError, RegistryHealthFileKind, built_in_registry_root,
        built_in_theme_contract_schema_path, snapshot_health_error,
        validate_built_in_registry_health, validate_built_in_registry_health_at,
    };
    use crate::{
        BuiltInAssetError, BuiltInAssetKind,
        builtin_registry::BuiltInRegistrySnapshot,
        embedded_assets::{EmbeddedAssetKind, InMemoryAssetProvider},
        load_built_in_registry_item,
    };

    fn health_fixture() -> (TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().expect("create registry health fixture");
        let root = temp.path().join("registry");
        copy_directory(&built_in_registry_root(), &root);
        fs::copy(
            built_in_theme_contract_schema_path(),
            root.join("contracts/theme-contract.schema.json"),
        )
        .expect("copy package theme contract schema");
        (temp, root)
    }

    fn copy_directory(source: &Path, target: &Path) {
        fs::create_dir_all(target).expect("create fixture directory");
        for entry in fs::read_dir(source).expect("read source fixture directory") {
            let entry = entry.expect("read source fixture entry");
            let source_path = entry.path();
            let target_path = target.join(entry.file_name());
            if source_path.is_dir() {
                copy_directory(&source_path, &target_path);
            } else {
                fs::copy(&source_path, &target_path).expect("copy source fixture file");
            }
        }
    }

    fn rewrite_json(path: &Path, mutate: impl FnOnce(&mut Value)) {
        let mut value =
            serde_json::from_slice::<Value>(&fs::read(path).expect("read JSON fixture"))
                .expect("parse JSON fixture");
        mutate(&mut value);
        fs::write(
            path,
            serde_json::to_vec_pretty(&value).expect("serialize JSON fixture"),
        )
        .expect("write JSON fixture");
    }

    #[test]
    fn validates_the_packaged_registry_health_contract() {
        validate_built_in_registry_health().expect("built-in registry should be healthy");
    }

    #[test]
    fn health_adapter_preserves_embedded_kind_and_inventory_faults() {
        let mut wrong_kind = InMemoryAssetProvider::from_embedded();
        wrong_kind
            .set_kind("registry/styles/button.css", EmbeddedAssetKind::Rust)
            .expect("replace asset kind");
        let injected = BuiltInRegistrySnapshot::from_provider(&wrong_kind)
            .expect_err("wrong kind must reject the snapshot");
        let error = snapshot_health_error(injected);
        assert!(matches!(
            &error,
            RegistryHealthError::BuiltInAsset {
                kind: RegistryHealthFileKind::RegistrySource,
                source: BuiltInAssetError::KindMismatch {
                    logical_path,
                    expected: BuiltInAssetKind::Css,
                    actual: BuiltInAssetKind::Rust,
                },
            } if logical_path == Path::new("registry/styles/button.css")
        ));
        assert!(!matches!(error, RegistryHealthError::ReadFile { .. }));

        let mut unexpected = InMemoryAssetProvider::from_embedded();
        unexpected
            .insert(
                "registry/ui/unlisted.rs",
                EmbeddedAssetKind::Rust,
                b"pub fn Unlisted() {}\n",
            )
            .expect("insert unexpected asset");
        let injected = BuiltInRegistrySnapshot::from_provider(&unexpected)
            .expect_err("unexpected inventory must reject the snapshot");
        assert!(matches!(
            snapshot_health_error(injected),
            RegistryHealthError::BuiltInAsset {
                kind: RegistryHealthFileKind::RegistrySource,
                source: BuiltInAssetError::Unexpected { logical_path },
            } if logical_path == Path::new("registry/ui/unlisted.rs")
        ));
    }

    #[test]
    fn health_adapter_classifies_non_theme_schema_faults_accurately() {
        let mut provider = InMemoryAssetProvider::from_embedded();
        provider
            .set_bytes("schema/0.9.0-alpha/kit.schema.json", b"{}")
            .expect("replace schema bytes");
        let injected = BuiltInRegistrySnapshot::from_provider(&provider)
            .expect_err("invalid kit schema must reject the snapshot");

        let error = snapshot_health_error(injected);
        assert!(matches!(
            &error,
            RegistryHealthError::InvalidPublicSchema {
                path,
                pointer,
                ..
            }
                if path == Path::new("schema/0.9.0-alpha/kit.schema.json")
                    && *pointer == "/$schema"
        ));
        assert!(error.to_string().contains("public schema"));
        assert!(!error.to_string().contains("theme contract schema"));
        assert!(!matches!(error, RegistryHealthError::ReadFile { .. }));
    }

    #[test]
    fn health_adapter_preserves_malformed_contract_json_class_and_locator() {
        let mut provider = InMemoryAssetProvider::from_embedded();
        provider
            .set_bytes("registry/contracts/theme-v1.json", b"{")
            .expect("replace contract bytes");
        let injected = BuiltInRegistrySnapshot::from_provider(&provider)
            .expect_err("malformed contract must reject the snapshot");

        let error = snapshot_health_error(injected);
        assert!(matches!(
            &error,
            RegistryHealthError::ParseJson {
                kind: RegistryHealthFileKind::ThemeContract,
                path,
                ..
            } if path == Path::new("registry/contracts/theme-v1.json")
        ));
        assert!(
            error
                .to_string()
                .contains("registry/contracts/theme-v1.json")
        );
        assert!(!matches!(error, RegistryHealthError::ReadFile { .. }));
    }

    #[test]
    fn health_adapter_preserves_theme_contract_version_mismatch_class_and_locators() {
        let mut provider = InMemoryAssetProvider::from_embedded();
        let mut tokens = load_built_in_registry_item("tokens")
            .expect("load template tokens")
            .item;
        tokens.extra.insert(
            "themeContractVersion".to_owned(),
            Value::String("2".to_owned()),
        );
        provider
            .set_bytes(
                "registry/foundation/tokens.json",
                serde_json::to_vec(&tokens).expect("serialize tokens drift"),
            )
            .expect("replace tokens manifest");
        let injected = BuiltInRegistrySnapshot::from_provider(&provider)
            .expect_err("version drift must reject the snapshot");

        assert!(matches!(
            snapshot_health_error(injected),
            RegistryHealthError::ThemeContractVersionMismatch {
                manifest_path,
                contract_path,
                manifest_version,
                contract_version,
                expected,
            } if manifest_path == Path::new("registry/foundation/tokens.json")
                && contract_path == Path::new("registry/contracts/theme-v1.json")
                && manifest_version == "2"
                && contract_version == "1"
                && expected == "1"
        ));
    }

    #[test]
    fn reports_invalid_registry_roots_manifests_and_catalogs() {
        let (_temp, root) = health_fixture();
        let path = root.join("registry.json");
        rewrite_json(&path, |registry| {
            registry["name"] = json!("wrong-registry");
        });
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::InvalidRegistryRoot {
                path: error_path,
                ..
            }) if error_path == path
        ));

        let (_temp, root) = health_fixture();
        let path = root.join("ui/button.json");
        fs::write(&path, b"{").expect("write malformed item manifest");
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::ParseJson {
                kind: RegistryHealthFileKind::RegistryItem,
                path: error_path,
                ..
            }) if error_path == path
        ));

        let (_temp, root) = health_fixture();
        let path = root.join("ui/button.json");
        rewrite_json(&path, |button| {
            button["registryDependencies"] = json!(["tokens", "missing-item"]);
        });
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::InvalidRegistryCatalog {
                path: error_path,
                ..
            }) if error_path == root.join("registry.json")
        ));
    }

    #[test]
    fn reports_missing_malformed_and_semantically_invalid_contracts() {
        let (_temp, root) = health_fixture();
        let path = root.join("contracts/theme-v1.json");
        fs::remove_file(&path).expect("remove contract");
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::MissingFile {
                kind: RegistryHealthFileKind::ThemeContract,
                path: error_path,
            }) if error_path == path
        ));

        let (_temp, root) = health_fixture();
        let path = root.join("contracts/theme-v1.json");
        fs::write(&path, b"{").expect("write malformed contract");
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::ParseJson {
                kind: RegistryHealthFileKind::ThemeContract,
                path: error_path,
                ..
            }) if error_path == path
        ));

        let (_temp, root) = health_fixture();
        let path = root.join("contracts/theme-v1.json");
        rewrite_json(&path, |contract| {
            contract["name"] = json!("wrong-theme");
        });
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::InvalidThemeContract {
                path: error_path,
                ..
            }) if error_path == path
        ));

        let (_temp, root) = health_fixture();
        let path = root.join("contracts/theme-v1.json");
        rewrite_json(&path, |contract| {
            contract["contractVersion"] = json!("2");
        });
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::InvalidThemeContract {
                path: error_path,
                ..
            }) if error_path == path
        ));
    }

    #[test]
    fn reports_missing_and_malformed_theme_contract_schemas() {
        let (_temp, root) = health_fixture();
        let path = root.join("contracts/theme-contract.schema.json");
        fs::remove_file(&path).expect("remove schema");
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::MissingFile {
                kind: RegistryHealthFileKind::ThemeContractSchema,
                path: error_path,
            }) if error_path == path
        ));

        let (_temp, root) = health_fixture();
        let path = root.join("contracts/theme-contract.schema.json");
        fs::write(&path, b"not JSON").expect("write malformed schema");
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::ParseJson {
                kind: RegistryHealthFileKind::ThemeContractSchema,
                path: error_path,
                ..
            }) if error_path == path
        ));
    }

    #[test]
    fn reports_wrong_schema_draft_identity_and_core_shape() {
        for (pointer, replacement, expected_pointer) in [
            (
                "/$schema",
                Some(json!("https://json-schema.org/draft-07/schema")),
                "/$schema",
            ),
            (
                "/$id",
                Some(json!("https://example.invalid/theme.schema.json")),
                "/$id",
            ),
            ("/properties/contractVersion", None, "/properties"),
            (
                "/properties/tokens/type",
                Some(json!("object")),
                "/properties/tokens/type",
            ),
            (
                "/required",
                Some(json!([
                    "$schema",
                    "schemaVersion",
                    "contractVersion",
                    "name",
                    "tokens",
                    "tokens"
                ])),
                "/required",
            ),
        ] {
            let (_temp, root) = health_fixture();
            let path = root.join("contracts/theme-contract.schema.json");
            rewrite_json(&path, |schema| {
                if let Some(replacement) = replacement {
                    *schema
                        .pointer_mut(pointer)
                        .expect("schema mutation pointer") = replacement;
                } else {
                    schema["properties"]
                        .as_object_mut()
                        .expect("schema properties")
                        .remove("contractVersion");
                }
            });
            assert!(matches!(
                validate_built_in_registry_health_at(&root),
                Err(RegistryHealthError::InvalidThemeContractSchema {
                    path: error_path,
                    pointer: error_pointer,
                    ..
                }) if error_path == path && error_pointer == expected_pointer
            ));
        }
    }

    #[test]
    fn reports_tokens_manifest_contract_version_drift() {
        let (_temp, root) = health_fixture();
        let path = root.join("foundation/tokens.json");
        rewrite_json(&path, |tokens| {
            tokens["extra"]["themeContractVersion"] = json!("2");
        });

        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::ThemeContractVersionMismatch {
                manifest_path,
                manifest_version,
                contract_version,
                expected,
                ..
            }) if manifest_path == path
                && manifest_version == "2"
                && contract_version == "1"
                && expected == "1"
        ));
    }

    #[test]
    fn reports_missing_and_non_utf8_referenced_sources() {
        let (_temp, root) = health_fixture();
        let path = root.join("styles/button.css");
        fs::remove_file(&path).expect("remove referenced source");
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::MissingFile {
                kind: RegistryHealthFileKind::RegistrySource,
                path: error_path,
            }) if error_path == path
        ));

        let (_temp, root) = health_fixture();
        let path = root.join("ui/button.rs");
        fs::write(&path, [0xff, 0xfe, 0xfd]).expect("write non-UTF-8 source");
        assert!(matches!(
            validate_built_in_registry_health_at(&root),
            Err(RegistryHealthError::NonUtf8File {
                kind: RegistryHealthFileKind::RegistrySource,
                path: error_path,
                ..
            }) if error_path == path
        ));
    }
}
