use std::{collections::BTreeMap, path::Path};

use leptos_ui_kit_registry::{KitConfig, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};

use crate::CodegenError;

pub const DEFAULT_KIT_LOCK_PATH: &str = "src/components/ui/_kit/kit.lock.json";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallLock {
    pub schema_version: String,
    pub kit_version: String,
    pub project: InstallLockProject,
    pub items: BTreeMap<String, InstalledItem>,
    pub files_by_path: BTreeMap<String, String>,
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
        if self.project.crate_root != "." {
            return invalid_lock(path, "project.crateRoot must be .");
        }
        if self.project.kind != "single-crate-trunk-csr" {
            return invalid_lock(path, "project.kind must be single-crate-trunk-csr");
        }
        validate_lock_hash(path, "project.configHash", &self.project.config_hash)?;

        for (key, item) in &self.items {
            if key != &item.id {
                return invalid_lock(path, format!("item key {key} does not match item id"));
            }
            if item.source != "builtin" {
                return invalid_lock(path, "only builtin item lock entries are supported");
            }
            if item.version != SCHEMA_VERSION {
                return invalid_lock(path, format!("item version must be {SCHEMA_VERSION}"));
            }
            validate_lock_hash(path, "items[].contentHash", &item.content_hash)?;
            for file in &item.files {
                validate_lock_hash(path, "items[].files[].generatedHash", &file.generated_hash)?;
                validate_lock_hash(
                    path,
                    "items[].files[].localHashAtInstall",
                    &file.local_hash_at_install,
                )?;
            }
            for block in &item.style_blocks {
                validate_lock_hash(
                    path,
                    "items[].styleBlocks[].generatedHash",
                    &block.generated_hash,
                )?;
            }
        }

        for (file_path, item_id) in &self.files_by_path {
            if !self.items.contains_key(item_id) {
                return invalid_lock(
                    path,
                    format!("filesByPath entry {file_path} references missing item {item_id}"),
                );
            }
        }

        for (block_id, item_id) in &self.style_blocks_by_id {
            if !self.items.contains_key(item_id) {
                return invalid_lock(
                    path,
                    format!("styleBlocksById entry {block_id} references missing item {item_id}"),
                );
            }
        }

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

fn validate_lock_hash(path: &Path, field: &'static str, value: &str) -> Result<(), CodegenError> {
    if value
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        Ok(())
    } else {
        invalid_lock(path, format!("{field} must be a sha256 hash"))
    }
}

fn invalid_lock<T>(path: &Path, reason: impl Into<String>) -> Result<T, CodegenError> {
    Err(CodegenError::InvalidLock {
        path: path.to_path_buf(),
        reason: reason.into(),
    })
}
