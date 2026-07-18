use std::{
    collections::BTreeSet,
    fmt, fs,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};

use crate::{
    RegistryError, SCHEMA_VERSION, THEME_CONTRACT_NAME, THEME_CONTRACT_SCHEMA_URL,
    THEME_CONTRACT_VERSION, ThemeContractError, parse_registry_item_str, parse_registry_root_str,
    parse_theme_contract_str,
};

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
}

impl fmt::Display for RegistryHealthFileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RegistryRoot => write!(f, "registry root"),
            Self::RegistryItem => write!(f, "registry item"),
            Self::RegistrySource => write!(f, "registry source"),
            Self::ThemeContract => write!(f, "theme contract"),
            Self::ThemeContractSchema => write!(f, "theme contract schema"),
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
            Self::ParseJson { source, .. } => Some(source),
            Self::InvalidRegistryRoot { source, .. }
            | Self::InvalidRegistryItem { source, .. }
            | Self::InvalidRegistryCatalog { source, .. } => Some(source),
            Self::InvalidThemeContract { source, .. } => Some(source),
            Self::MissingFile { .. }
            | Self::RegistryItemIdentity { .. }
            | Self::InvalidThemeContractSchema { .. }
            | Self::ThemeContractVersionMismatch { .. } => None,
        }
    }
}

/// Validates every runtime-relevant file in the packaged built-in registry.
pub fn validate_built_in_registry_health() -> Result<(), RegistryHealthError> {
    validate_built_in_registry_health_at(&built_in_registry_root())
}

pub(crate) fn validate_built_in_registry_health_at(
    registry_root: &Path,
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

    let schema_path = registry_root.join("contracts/theme-contract.schema.json");
    let schema_input = read_utf8(RegistryHealthFileKind::ThemeContractSchema, &schema_path)?;
    let schema = serde_json::from_str::<Value>(&schema_input).map_err(|source| {
        RegistryHealthError::ParseJson {
            kind: RegistryHealthFileKind::ThemeContractSchema,
            path: schema_path.clone(),
            source,
        }
    })?;
    validate_theme_contract_schema_shape(&schema_path, &schema)?;

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

fn validate_theme_contract_schema_shape(
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

fn built_in_registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("registry")
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::{
        RegistryHealthError, RegistryHealthFileKind, built_in_registry_root,
        validate_built_in_registry_health, validate_built_in_registry_health_at,
    };

    fn health_fixture() -> (TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().expect("create registry health fixture");
        let root = temp.path().join("registry");
        copy_directory(&built_in_registry_root(), &root);
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
