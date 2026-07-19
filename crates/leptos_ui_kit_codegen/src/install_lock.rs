use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use leptos_ui_kit_registry::{
    KitConfig, RegistryItemKind, SCHEMA_VERSION, validate_registry_item_name,
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as _, MapAccess, Visitor},
};

use crate::{CodegenError, validate_logical_write_path};

pub const DEFAULT_KIT_LOCK_PATH: &str = "src/components/ui/_kit/kit.lock.json";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallLock {
    pub schema_version: String,
    pub kit_version: String,
    pub project: InstallLockProject,
    #[serde(deserialize_with = "deserialize_string_map_without_duplicates")]
    pub items: BTreeMap<String, InstalledItem>,
    #[serde(deserialize_with = "deserialize_string_map_without_duplicates")]
    pub files_by_path: BTreeMap<String, String>,
    #[serde(deserialize_with = "deserialize_string_map_without_duplicates")]
    pub style_blocks_by_id: BTreeMap<String, String>,
}

impl InstallLock {
    pub fn empty(config_hash: String) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_owned(),
            kit_version: SCHEMA_VERSION.to_owned(),
            project: InstallLockProject {
                config_hash,
                crate_root: ".".to_owned(),
                kind: "single-crate-trunk-csr".to_owned(),
            },
            items: BTreeMap::new(),
            files_by_path: BTreeMap::new(),
            style_blocks_by_id: BTreeMap::new(),
        }
    }

    pub fn validate(&self) -> Result<(), CodegenError> {
        self.validate_at_path(Path::new(DEFAULT_KIT_LOCK_PATH))
    }

    pub fn validate_at_path(&self, path: &Path) -> Result<(), CodegenError> {
        if self.schema_version != SCHEMA_VERSION {
            return invalid_lock(path, format!("schemaVersion must be {SCHEMA_VERSION}"));
        }
        if self.kit_version != SCHEMA_VERSION {
            return invalid_lock(path, format!("kitVersion must be {SCHEMA_VERSION}"));
        }
        if self.project.crate_root != "." {
            return invalid_lock(path, "project.crateRoot must be .");
        }
        if self.project.kind != "single-crate-trunk-csr" {
            return invalid_lock(path, "project.kind must be single-crate-trunk-csr");
        }
        validate_lock_hash(path, "project.configHash", &self.project.config_hash)?;

        let mut item_names = BTreeSet::new();
        let mut expected_files_by_path = BTreeMap::new();
        let mut expected_style_blocks_by_id = BTreeMap::new();
        let mut folded_file_paths = BTreeMap::<String, String>::new();

        for (key, item) in &self.items {
            let item_path = format!("items[{key:?}]");
            if key != &item.id {
                return invalid_lock(
                    path,
                    format!("{item_path}.id must equal its map key {key:?}"),
                );
            }
            validate_registry_item_name(&item.name).map_err(|error| CodegenError::InvalidLock {
                path: path.to_path_buf(),
                reason: format!("{item_path}.name is invalid: {error}"),
            })?;
            let expected_id = format!("builtin:{}", item.name);
            if item.id != expected_id {
                return invalid_lock(
                    path,
                    format!(
                        "{item_path}.id must be {expected_id:?} for item name {:?}",
                        item.name
                    ),
                );
            }
            if !item_names.insert(item.name.as_str()) {
                return invalid_lock(
                    path,
                    format!("{item_path}.name duplicates installed item {:?}", item.name),
                );
            }
            if item.source != "builtin" {
                return invalid_lock(path, format!("{item_path}.source must be \"builtin\""));
            }
            if item.version != SCHEMA_VERSION {
                return invalid_lock(
                    path,
                    format!("{item_path}.version must be {SCHEMA_VERSION}"),
                );
            }
            validate_lock_hash(
                path,
                &format!("{item_path}.contentHash"),
                &item.content_hash,
            )?;
            for (index, file) in item.files.iter().enumerate() {
                let file_path = format!("{item_path}.files[{index}]");
                validate_lock_target_path(path, &format!("{file_path}.path"), &file.path, "rs")?;
                if file.kind != "rust" {
                    return invalid_lock(path, format!("{file_path}.kind must be \"rust\""));
                }
                validate_lock_hash(
                    path,
                    &format!("{file_path}.generatedHash"),
                    &file.generated_hash,
                )?;
                validate_lock_hash(
                    path,
                    &format!("{file_path}.localHashAtInstall"),
                    &file.local_hash_at_install,
                )?;
                if let Some(existing_owner) =
                    expected_files_by_path.insert(file.path.clone(), item.id.clone())
                {
                    return invalid_lock(
                        path,
                        format!(
                            "{file_path}.path {:?} duplicates a file target owned by {existing_owner}",
                            file.path
                        ),
                    );
                }
                let folded = file.path.to_ascii_lowercase();
                if let Some(existing) = folded_file_paths.insert(folded, file.path.clone()) {
                    return invalid_lock(
                        path,
                        format!(
                            "{file_path}.path {:?} collides under ASCII case folding with {existing:?}",
                            file.path
                        ),
                    );
                }
            }
            for (index, block) in item.style_blocks.iter().enumerate() {
                let block_path = format!("{item_path}.styleBlocks[{index}]");
                validate_lock_target_path(
                    path,
                    &format!("{block_path}.cssPath"),
                    &block.css_path,
                    "css",
                )?;
                validate_kebab_name(path, &format!("{block_path}.blockId"), &block.block_id)?;
                validate_lock_hash(
                    path,
                    &format!("{block_path}.generatedHash"),
                    &block.generated_hash,
                )?;
                if let Some(existing_owner) =
                    expected_style_blocks_by_id.insert(block.block_id.clone(), item.id.clone())
                {
                    return invalid_lock(
                        path,
                        format!(
                            "{block_path}.blockId {:?} duplicates a managed CSS block owned by {existing_owner}",
                            block.block_id
                        ),
                    );
                }
            }
            validate_item_target_shape(path, &item_path, item)?;
        }

        validate_reverse_index(
            path,
            "filesByPath",
            &expected_files_by_path,
            &self.files_by_path,
        )?;
        validate_reverse_index(
            path,
            "styleBlocksById",
            &expected_style_blocks_by_id,
            &self.style_blocks_by_id,
        )?;

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallLockProject {
    pub config_hash: String,
    pub crate_root: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledItem {
    pub id: String,
    pub name: String,
    pub kind: RegistryItemKind,
    pub source: String,
    pub version: String,
    pub content_hash: String,
    pub files: Vec<InstalledFile>,
    pub style_blocks: Vec<InstalledStyleBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledFile {
    pub path: String,
    pub kind: String,
    pub generated_hash: String,
    pub local_hash_at_install: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledStyleBlock {
    pub css_path: String,
    pub block_id: String,
    pub generated_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedCssBlockRole {
    Foundation,
    Component,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedCssOperation {
    pub item_id: String,
    pub block_id: String,
    pub role: ManagedCssBlockRole,
    pub generated: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManagedCssDependency {
    pub dependency_block_id: String,
    pub dependent_block_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedCssBlockRange {
    pub start: usize,
    pub end: usize,
}

pub fn install_lock_path(_config: &KitConfig) -> String {
    DEFAULT_KIT_LOCK_PATH.to_owned()
}

pub fn parse_install_lock_str(input: &str) -> Result<InstallLock, CodegenError> {
    parse_install_lock_str_at_path(input, Path::new(DEFAULT_KIT_LOCK_PATH))
}

pub fn parse_install_lock_str_at_path(
    input: &str,
    path: &Path,
) -> Result<InstallLock, CodegenError> {
    let lock: InstallLock =
        serde_json::from_str(input).map_err(|source| CodegenError::LockParse {
            path: path.to_path_buf(),
            source,
        })?;
    lock.validate_at_path(path)?;
    Ok(lock)
}

pub fn lock_to_json(lock: &InstallLock) -> Result<String, CodegenError> {
    lock_to_json_at_path(lock, Path::new(DEFAULT_KIT_LOCK_PATH))
}

pub fn lock_to_json_at_path(lock: &InstallLock, path: &Path) -> Result<String, CodegenError> {
    lock.validate_at_path(path)?;
    let mut output = serde_json::to_string_pretty(lock).map_err(CodegenError::LockSerialize)?;
    output.push('\n');
    Ok(output)
}

fn validate_lock_hash(path: &Path, field: &str, value: &str) -> Result<(), CodegenError> {
    if value.strip_prefix("sha256:").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }) {
        Ok(())
    } else {
        invalid_lock(path, format!("{field} must be a lowercase sha256 hash"))
    }
}

fn validate_lock_target_path(
    lock_path: &Path,
    field: &str,
    value: &str,
    extension: &str,
) -> Result<(), CodegenError> {
    if Path::new(value)
        .extension()
        .and_then(|value| value.to_str())
        != Some(extension)
    {
        return invalid_lock(lock_path, format!("{field} must end in .{extension}"));
    }
    if let Err(error) = validate_logical_write_path(value) {
        return invalid_lock(
            lock_path,
            format!("{field} must be a safe writable project path: {error}"),
        );
    }
    Ok(())
}

fn validate_kebab_name(lock_path: &Path, field: &str, value: &str) -> Result<(), CodegenError> {
    let valid = value.split('-').all(|segment| {
        let mut bytes = segment.bytes();
        bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    });
    if valid {
        Ok(())
    } else {
        invalid_lock(
            lock_path,
            format!("{field} must be an ASCII lowercase kebab-case name beginning with a letter"),
        )
    }
}

fn validate_reverse_index(
    lock_path: &Path,
    index_name: &str,
    expected: &BTreeMap<String, String>,
    actual: &BTreeMap<String, String>,
) -> Result<(), CodegenError> {
    for (target, expected_owner) in expected {
        match actual.get(target) {
            Some(actual_owner) if actual_owner == expected_owner => {}
            Some(actual_owner) => {
                return invalid_lock(
                    lock_path,
                    format!(
                        "{index_name}[{target:?}] is owned by {actual_owner:?}; expected {expected_owner:?}"
                    ),
                );
            }
            None => {
                return invalid_lock(
                    lock_path,
                    format!(
                        "{index_name}[{target:?}] is missing for forward target owned by {expected_owner:?}"
                    ),
                );
            }
        }
    }
    for (target, owner) in actual {
        if !expected.contains_key(target) {
            return invalid_lock(
                lock_path,
                format!("{index_name}[{target:?}] is an extra reverse entry owned by {owner:?}"),
            );
        }
    }
    Ok(())
}

fn validate_item_target_shape(
    lock_path: &Path,
    item_path: &str,
    item: &InstalledItem,
) -> Result<(), CodegenError> {
    match item.kind {
        RegistryItemKind::Ui => {
            if item.style_blocks.len() > 1 {
                return invalid_lock(
                    lock_path,
                    format!("{item_path}.styleBlocks must contain at most one UI style block"),
                );
            }
            if item
                .style_blocks
                .first()
                .is_some_and(|block| block.block_id != item.name)
            {
                return invalid_lock(
                    lock_path,
                    format!(
                        "{item_path}.styleBlocks[0].blockId must equal {:?}",
                        item.name
                    ),
                );
            }
        }
        RegistryItemKind::Foundation => {
            if !item.files.is_empty() {
                return invalid_lock(
                    lock_path,
                    format!("{item_path}.files must be empty for a foundation item"),
                );
            }
            let Some(first) = item.style_blocks.first() else {
                return invalid_lock(
                    lock_path,
                    format!(
                        "{item_path}.styleBlocks must contain at least one foundation style block"
                    ),
                );
            };
            if first.block_id != item.name {
                return invalid_lock(
                    lock_path,
                    format!(
                        "{item_path}.styleBlocks[0].blockId must equal {:?}",
                        item.name
                    ),
                );
            }
            let prefix = format!("{}-", item.name);
            for (index, block) in item.style_blocks.iter().enumerate().skip(1) {
                let Some(suffix) = block.block_id.strip_prefix(&prefix) else {
                    return invalid_lock(
                        lock_path,
                        format!(
                            "{item_path}.styleBlocks[{index}].blockId must begin with {prefix:?}"
                        ),
                    );
                };
                validate_kebab_name(
                    lock_path,
                    &format!("{item_path}.styleBlocks[{index}].blockId suffix"),
                    suffix,
                )?;
            }
        }
    }
    Ok(())
}

fn deserialize_string_map_without_duplicates<'de, D, V>(
    deserializer: D,
) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct UniqueStringMapVisitor<V>(std::marker::PhantomData<V>);

    impl<'de, V> Visitor<'de> for UniqueStringMapVisitor<V>
    where
        V: Deserialize<'de>,
    {
        type Value = BTreeMap<String, V>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object with unique string keys")
        }

        fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut output = BTreeMap::new();
            while let Some((key, value)) = access.next_entry::<String, V>()? {
                if output.insert(key.clone(), value).is_some() {
                    return Err(A::Error::custom(format!("duplicate object key {key:?}")));
                }
            }
            Ok(output)
        }
    }

    deserializer.deserialize_map(UniqueStringMapVisitor(std::marker::PhantomData))
}

fn invalid_lock<T>(path: &Path, reason: impl Into<String>) -> Result<T, CodegenError> {
    Err(CodegenError::InvalidLock {
        path: path.to_path_buf(),
        reason: reason.into(),
    })
}
