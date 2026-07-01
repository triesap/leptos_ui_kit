#![forbid(unsafe_code)]

//! Code generation and install-planning layer.

use std::{
    collections::BTreeMap,
    fmt, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use leptos_ui_kit_registry::{
    CargoPlanEntry, ConfigError, DEFAULT_KIT_CONFIG_PATH, KitConfig, RegistryError, RegistryItem,
    SCHEMA_VERSION, canonical_kit_json, desired_builtin_button_item,
    desired_builtin_collapsible_item, desired_builtin_dialog_item, desired_builtin_field_item,
    desired_builtin_menu_item, desired_builtin_spinner_item, desired_builtin_status_item,
    desired_builtin_tabs_item, kit_config_to_json, kit_config_with_desired_item,
    load_built_in_registry_item, parse_kit_json_str, read_built_in_registry_source,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEFAULT_KIT_LOCK_PATH: &str = "src/components/ui/_kit/kit.lock.json";
pub const DEFAULT_KIT_WRITE_LOCK_PATH: &str = "src/components/ui/_kit/.write.lock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandEnvelope<T>
where
    T: Serialize,
{
    pub schema_version: &'static str,
    pub command: String,
    pub status: CommandStatus,
    pub diagnostics: Vec<Diagnostic>,
    pub changes: Vec<ChangeRecord>,
    pub data: T,
}

impl<T> CommandEnvelope<T>
where
    T: Serialize,
{
    pub fn new(command: impl Into<String>, status: CommandStatus, data: T) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.into(),
            status,
            diagnostics: Vec::new(),
            changes: Vec::new(),
            data,
        }
    }

    pub fn success(command: impl Into<String>, data: T) -> Self {
        Self::new(command, CommandStatus::Success, data)
    }

    pub fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    pub fn with_changes(mut self, changes: Vec<ChangeRecord>) -> Self {
        self.changes = changes;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Success,
    NoChange,
    Planned,
    Warning,
    Error,
    Conflict,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl Diagnostic {
    pub fn new(
        level: DiagnosticLevel,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            level,
            code: code.into(),
            message: message.into(),
            path: None,
            suggestion: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRecord {
    pub kind: ChangeKind,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    pub tracked: bool,
}

impl ChangeRecord {
    pub fn new(kind: ChangeKind, path: impl Into<String>, tracked: bool) -> Self {
        Self {
            kind,
            path: path.into(),
            item: None,
            tracked,
        }
    }

    pub fn with_item(mut self, item: impl Into<String>) -> Self {
        self.item = Some(item.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    CreateFile,
    UpdateFile,
    DeleteFile,
    CreateDir,
    UpdateCssBlock,
    WriteLockFile,
}

#[derive(Debug)]
pub enum CodegenError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Config(ConfigError),
    Registry(RegistryError),
    LockParse {
        path: PathBuf,
        source: serde_json::Error,
    },
    LockSerialize(serde_json::Error),
    InvalidLock {
        path: PathBuf,
        reason: String,
    },
    UnsafePatch {
        path: PathBuf,
        reason: String,
    },
    UnsafePath {
        path: String,
        reason: String,
    },
    DuplicatePath(String),
    LockExists(PathBuf),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "failed to read {}: {source}", path.display()),
            Self::Config(error) => write!(f, "{error}"),
            Self::Registry(error) => write!(f, "{error}"),
            Self::LockParse { path, source } => {
                write!(f, "failed to parse {}: {source}", path.display())
            }
            Self::LockSerialize(error) => write!(f, "failed to serialize lock: {error}"),
            Self::InvalidLock { path, reason } => {
                write!(f, "invalid {}: {reason}", path.display())
            }
            Self::UnsafePatch { path, reason } => {
                write!(f, "cannot safely patch {}: {reason}", path.display())
            }
            Self::UnsafePath { path, reason } => {
                write!(f, "unsafe write path {path}: {reason}")
            }
            Self::DuplicatePath(path) => write!(f, "duplicate planned write path: {path}"),
            Self::LockExists(path) => write!(f, "write lock already exists: {}", path.display()),
        }
    }
}

impl std::error::Error for CodegenError {}

impl From<ConfigError> for CodegenError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

impl From<RegistryError> for CodegenError {
    fn from(value: RegistryError) -> Self {
        Self::Registry(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitPlan {
    pub project_root: PathBuf,
    pub files: Vec<PlannedFile>,
    pub changes: Vec<ChangeRecord>,
}

impl InitPlan {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddPlan {
    pub project_root: PathBuf,
    pub item_id: String,
    pub item_name: String,
    pub content_hash: String,
    pub cargo_plan: Vec<CargoPlanEntry>,
    pub files: Vec<PlannedFile>,
    pub changes: Vec<ChangeRecord>,
    pub diagnostics: Vec<Diagnostic>,
    pub lock: InstallLock,
}

impl AddPlan {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlan {
    pub project_root: PathBuf,
    pub item_ids: Vec<String>,
    pub cargo_plan: Vec<CargoPlanEntry>,
    pub files: Vec<PlannedFile>,
    pub changes: Vec<ChangeRecord>,
    pub diagnostics: Vec<Diagnostic>,
    pub lock: InstallLock,
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedFile {
    pub path: String,
    pub action: PlannedFileAction,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannedFileAction {
    Create,
    Update,
}

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

pub fn install_lock_path(_config: &KitConfig) -> String {
    DEFAULT_KIT_LOCK_PATH.to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiModuleExport {
    pub module: String,
    pub path: String,
    pub symbols: Vec<String>,
}

impl UiModuleExport {
    pub fn new(module: impl Into<String>, symbols: Vec<String>) -> Self {
        let module = module.into();
        Self {
            path: module.clone(),
            module,
            symbols,
        }
    }

    pub fn with_path(
        module: impl Into<String>,
        path: impl Into<String>,
        symbols: Vec<String>,
    ) -> Self {
        Self {
            module: module.into(),
            path: path.into(),
            symbols,
        }
    }
}

pub fn plan_init(project_root: &Path) -> Result<InitPlan, CodegenError> {
    let mut files = Vec::new();
    let mut changes = Vec::new();

    plan_kit_json(project_root, &mut files, &mut changes)?;
    let config_content = planned_or_existing_kit_config_content(project_root, &files)?;
    let config = parse_kit_json_str(&config_content)?;
    plan_stylesheet(project_root, &mut files, &mut changes, &config)?;
    plan_index_html(project_root, &mut files, &mut changes, &config)?;
    plan_component_modules(project_root, &mut files, &mut changes)?;
    plan_empty_state(project_root, &mut files, &mut changes)?;

    Ok(InitPlan {
        project_root: project_root.to_path_buf(),
        files,
        changes,
    })
}

pub fn apply_init(project_root: &Path) -> Result<InitPlan, CodegenError> {
    let plan = plan_init(project_root)?;
    apply_planned_files(project_root, &plan.files, &plan.changes)?;

    Ok(plan)
}

pub fn plan_add(project_root: &Path, item_name: &str) -> Result<AddPlan, CodegenError> {
    let item = load_built_in_registry_item(item_name)?;
    let desired_item = match item.item.name.as_str() {
        "button" => desired_builtin_button_item(),
        "collapsible" => desired_builtin_collapsible_item(),
        "dialog" => desired_builtin_dialog_item(),
        "field" => desired_builtin_field_item(),
        "menu" => desired_builtin_menu_item(),
        "spinner" => desired_builtin_spinner_item(),
        "status" => desired_builtin_status_item(),
        "tabs" => desired_builtin_tabs_item(),
        _ => return Err(RegistryError::BuiltInNotFound(item_name.to_owned()).into()),
    };
    let item_id = built_in_item_id(&item.item.name);
    let item_name = item.item.name.clone();
    let content_hash = item.content_hash.clone();
    let init_plan = plan_init(project_root)?;
    let config_content = planned_or_existing_kit_config_content(project_root, &init_plan.files)?;
    let config = parse_kit_json_str(&config_content)?;
    let state_path = install_lock_path(&config);
    let mut files = init_plan
        .files
        .into_iter()
        .filter(|file| file.path != state_path)
        .collect::<Vec<_>>();
    let mut changes = init_plan
        .changes
        .into_iter()
        .filter(|change| change.path != state_path)
        .collect::<Vec<_>>();

    let config = kit_config_with_desired_item(config, desired_item)?;
    let config_content = kit_config_to_json(&config)?;
    upsert_planned_file(
        project_root,
        &mut files,
        &mut changes,
        DEFAULT_KIT_CONFIG_PATH,
        config_content.clone(),
        ChangeKind::UpdateFile,
        Some(&item_id),
    )?;

    let sync = plan_sync_from_config(project_root, files, changes, config, config_content)?;

    Ok(AddPlan {
        project_root: sync.project_root,
        item_id,
        item_name,
        content_hash,
        cargo_plan: sync.cargo_plan,
        files: sync.files,
        changes: sync.changes,
        diagnostics: sync.diagnostics,
        lock: sync.lock,
    })
}

pub fn apply_add(project_root: &Path, item_name: &str) -> Result<AddPlan, CodegenError> {
    let plan = plan_add(project_root, item_name)?;
    apply_planned_files(project_root, &plan.files, &plan.changes)?;

    Ok(plan)
}

pub fn plan_sync(project_root: &Path) -> Result<SyncPlan, CodegenError> {
    let init_plan = plan_init(project_root)?;
    let config_content = planned_or_existing_kit_config_content(project_root, &init_plan.files)?;
    let config = parse_kit_json_str(&config_content)?;
    let state_path = install_lock_path(&config);
    let files = init_plan
        .files
        .into_iter()
        .filter(|file| file.path != state_path)
        .collect::<Vec<_>>();
    let changes = init_plan
        .changes
        .into_iter()
        .filter(|change| change.path != state_path)
        .collect::<Vec<_>>();

    plan_sync_from_config(project_root, files, changes, config, config_content)
}

pub fn apply_sync(project_root: &Path) -> Result<SyncPlan, CodegenError> {
    let plan = plan_sync(project_root)?;
    apply_planned_files(project_root, &plan.files, &plan.changes)?;

    Ok(plan)
}

fn plan_sync_from_config(
    project_root: &Path,
    mut files: Vec<PlannedFile>,
    mut changes: Vec<ChangeRecord>,
    config: KitConfig,
    config_content: String,
) -> Result<SyncPlan, CodegenError> {
    let diagnostics = Vec::new();
    let config_hash = hash_bytes(config_content.as_bytes());
    let lock_path = install_lock_path(&config);
    let mut lock = load_or_empty_lock(project_root, &lock_path, config_hash.clone())?;
    lock.project.config_hash = config_hash;
    let mut item_ids = Vec::new();
    let mut cargo_plan = Vec::new();

    for desired_item in &config.items {
        let item = load_built_in_registry_item(desired_item.item_name())?;
        let item_id = plan_built_in_item(
            project_root,
            &mut files,
            &mut changes,
            &mut lock,
            &config,
            &item,
        )?;
        item_ids.push(item_id);
        merge_cargo_plan(&mut cargo_plan, &item.item.cargo_plan);
    }

    lock.validate_at_path(Path::new(&lock_path))?;
    let lock_json = lock_to_json_at_path(&lock, Path::new(&lock_path))?;
    upsert_planned_file(
        project_root,
        &mut files,
        &mut changes,
        &lock_path,
        lock_json,
        ChangeKind::WriteLockFile,
        None,
    )?;

    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    validate_planned_write_paths(&paths)?;

    Ok(SyncPlan {
        project_root: project_root.to_path_buf(),
        item_ids,
        cargo_plan,
        files,
        changes,
        diagnostics,
        lock,
    })
}

fn merge_cargo_plan(plan: &mut Vec<CargoPlanEntry>, entries: &[CargoPlanEntry]) {
    for entry in entries {
        let mut entry = entry.clone();
        entry.features.sort();
        entry.features.dedup();
        if !plan.contains(&entry) {
            plan.push(entry);
        }
    }
    plan.sort();
}

fn plan_built_in_item(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    lock: &mut InstallLock,
    config: &KitConfig,
    item: &leptos_ui_kit_registry::ResolvedRegistryItem,
) -> Result<String, CodegenError> {
    let item_id = built_in_item_id(&item.item.name);
    let mut installed_files = Vec::new();
    let mut installed_style_blocks = Vec::new();

    for ui_file in &item.targets.ui_files {
        let generated = read_built_in_registry_source(&ui_file.source)?;
        let logical_path = format!("src/components/ui/{}", ui_file.path);
        let generated_hash = hash_bytes(generated.as_bytes());

        plan_generated_source_file(
            project_root,
            files,
            changes,
            lock,
            &item_id,
            &logical_path,
            &generated,
        )?;

        installed_files.push(InstalledFile {
            path: logical_path.clone(),
            kind: "rust".to_owned(),
            generated_hash: generated_hash.clone(),
            local_hash_at_install: generated_hash,
        });
        lock.files_by_path.insert(logical_path, item_id.clone());
    }

    let components_mod = planned_or_existing_content(files, project_root, "src/components/mod.rs")?;
    let patched_components_mod = patch_components_mod(components_mod.as_deref())?;
    upsert_planned_file(
        project_root,
        files,
        changes,
        "src/components/mod.rs",
        patched_components_mod,
        ChangeKind::UpdateFile,
        Some(&item_id),
    )?;

    let ui_mod = planned_or_existing_content(files, project_root, "src/components/ui/mod.rs")?;
    let patched_ui_mod = patch_ui_mod(ui_mod.as_deref(), &ui_exports_for_item(&item.item)?)?;
    upsert_planned_file(
        project_root,
        files,
        changes,
        "src/components/ui/mod.rs",
        patched_ui_mod,
        ChangeKind::UpdateFile,
        Some(&item_id),
    )?;

    for style in &item.targets.style_blocks {
        let generated = read_built_in_registry_source(&style.source)?;
        let css_path = config.styles.css.as_str();
        let generated_hash = hash_bytes(generated.as_bytes());
        let tracked_hash = tracked_style_generated_hash(lock, &item_id, &style.id)?;
        let existing_css =
            planned_or_existing_content(files, project_root, css_path)?.unwrap_or_default();
        let patched_css = patch_css_block(
            &existing_css,
            &style.id,
            &generated,
            tracked_hash.as_deref(),
        )?;

        upsert_planned_file(
            project_root,
            files,
            changes,
            css_path,
            patched_css,
            ChangeKind::UpdateCssBlock,
            Some(&item_id),
        )?;

        installed_style_blocks.push(InstalledStyleBlock {
            css_path: css_path.to_owned(),
            block_id: style.id.clone(),
            generated_hash,
        });
        lock.style_blocks_by_id
            .insert(style.id.clone(), item_id.clone());
    }

    lock.items.insert(
        item_id.clone(),
        InstalledItem {
            id: item_id.clone(),
            name: item.item.name.clone(),
            source: "builtin".to_owned(),
            version: item.item.version.clone(),
            content_hash: item.content_hash.clone(),
            files: installed_files,
            style_blocks: installed_style_blocks,
        },
    );
    Ok(item_id)
}

fn apply_planned_files(
    project_root: &Path,
    files: &[PlannedFile],
    changes: &[ChangeRecord],
) -> Result<(), CodegenError> {
    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let lock_paths = lock_file_write_paths(changes);
    validate_planned_write_paths(&paths)?;

    if files.is_empty() {
        return Ok(());
    }

    let _lock = WriteLock::acquire(project_root)?;

    for file in files
        .iter()
        .filter(|file| !lock_paths.contains(&file.path.as_str()))
    {
        write_file_atomic(project_root, &file.path, file.content.as_bytes())?;
    }

    for lock_path in lock_paths {
        if let Some(lock_file) = files.iter().find(|file| file.path == lock_path) {
            write_file_atomic(project_root, &lock_file.path, lock_file.content.as_bytes())?;
        }
    }

    Ok(())
}

pub fn validate_planned_write_paths(paths: &[String]) -> Result<(), CodegenError> {
    let mut seen = std::collections::BTreeSet::new();
    for path in paths {
        validate_logical_write_path(path)?;
        let folded = path.to_ascii_lowercase();
        if !seen.insert(folded) {
            return Err(CodegenError::DuplicatePath(path.clone()));
        }
    }
    Ok(())
}

pub fn validate_project_write_path(
    project_root: &Path,
    logical_path: &str,
) -> Result<PathBuf, CodegenError> {
    validate_logical_write_path(logical_path)?;
    let root = project_root
        .canonicalize()
        .map_err(|source| CodegenError::Io {
            path: project_root.to_path_buf(),
            source,
        })?;
    let full_path = project_root.join(logical_path);
    let existing_parent =
        nearest_existing_parent(&full_path).ok_or_else(|| CodegenError::UnsafePath {
            path: logical_path.to_owned(),
            reason: "no existing parent within project".to_owned(),
        })?;
    let canonical_parent = existing_parent
        .canonicalize()
        .map_err(|source| CodegenError::Io {
            path: existing_parent,
            source,
        })?;

    if !canonical_parent.starts_with(&root) {
        return Err(CodegenError::UnsafePath {
            path: logical_path.to_owned(),
            reason: "path escapes project root through symlink".to_owned(),
        });
    }

    Ok(full_path)
}

pub fn validate_logical_write_path(path: &str) -> Result<(), CodegenError> {
    if path.is_empty() {
        return unsafe_path(path, "path is empty");
    }
    if path.starts_with('/')
        || path.starts_with("//")
        || path.starts_with("\\\\")
        || path.as_bytes().get(1) == Some(&b':')
    {
        return unsafe_path(path, "absolute paths and platform prefixes are rejected");
    }
    if !path.is_ascii() {
        return unsafe_path(path, "path must be ASCII");
    }
    if path.contains('\\') {
        return unsafe_path(path, "backslashes are rejected");
    }

    let mut components = path.split('/').peekable();
    while let Some(component) = components.next() {
        if component.is_empty() || component == "." {
            return unsafe_path(path, "empty or current-dir segments are rejected");
        }
        if component == ".." {
            return unsafe_path(path, "parent traversal is rejected");
        }
        if component.starts_with('.') {
            return unsafe_path(path, "hidden paths are rejected");
        }
        if !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return unsafe_path(path, "file name contains unsafe characters");
        }
    }

    if is_allowed_write_path(path) {
        Ok(())
    } else {
        unsafe_path(path, "path is outside the MVP write allow-list")
    }
}

#[derive(Debug)]
pub struct WriteLock {
    path: PathBuf,
}

impl WriteLock {
    pub fn acquire(project_root: &Path) -> Result<Self, CodegenError> {
        let lock_path = project_root.join(DEFAULT_KIT_WRITE_LOCK_PATH);
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|source| CodegenError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                file.write_all(b"locked\n")
                    .map_err(|source| CodegenError::Io {
                        path: lock_path.clone(),
                        source,
                    })?;
                Ok(Self { path: lock_path })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(CodegenError::LockExists(lock_path))
            }
            Err(source) => Err(CodegenError::Io {
                path: lock_path,
                source,
            }),
        }
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn write_file_atomic(
    project_root: &Path,
    logical_path: &str,
    content: &[u8],
) -> Result<(), CodegenError> {
    let full_path = validate_project_write_path(project_root, logical_path)?;
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).map_err(|source| CodegenError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let temp_path = full_path.with_extension("leptos-ui-kit.tmp");
    fs::write(&temp_path, content).map_err(|source| CodegenError::Io {
        path: temp_path.clone(),
        source,
    })?;
    fs::rename(&temp_path, &full_path).map_err(|source| CodegenError::Io {
        path: full_path,
        source,
    })?;
    Ok(())
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

pub fn patch_css_block(
    existing: &str,
    block_id: &str,
    block: &str,
    tracked_generated_hash: Option<&str>,
) -> Result<String, CodegenError> {
    validate_css_block_id(block_id)?;
    let replacement = normalize_managed_css_block(block_id, block)?;
    let existing_block = find_managed_css_block(existing, block_id)?;

    match existing_block {
        Some(range) => {
            let current = &existing[range.clone()];
            if current == replacement {
                return Ok(existing.to_owned());
            }

            match tracked_generated_hash {
                Some(hash) if hash_bytes(current.as_bytes()) == hash => {
                    let mut output = String::with_capacity(
                        existing.len() + replacement.len().saturating_sub(current.len()),
                    );
                    output.push_str(&existing[..range.start]);
                    output.push_str(&replacement);
                    output.push_str(&existing[range.end..]);
                    Ok(output)
                }
                Some(_) => unsafe_patch(
                    "styles/kit.css",
                    format!("managed CSS block {block_id} has local edits"),
                ),
                None => unsafe_patch(
                    "styles/kit.css",
                    format!("managed CSS block {block_id} already exists but is not tracked"),
                ),
            }
        }
        None => {
            let mut output = existing.to_owned();
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            if !output.trim().is_empty() {
                output.push('\n');
            }
            output.push_str(&replacement);
            Ok(output)
        }
    }
}

pub fn extract_managed_css_block(
    existing: &str,
    block_id: &str,
) -> Result<Option<String>, CodegenError> {
    validate_css_block_id(block_id)?;
    Ok(find_managed_css_block(existing, block_id)?.map(|range| existing[range].to_owned()))
}

pub fn patch_components_mod(existing: Option<&str>) -> Result<String, CodegenError> {
    patch_module_lines(
        existing.unwrap_or_default(),
        "src/components/mod.rs",
        &["pub mod ui;"],
    )
}

pub fn patch_ui_mod(
    existing: Option<&str>,
    exports: &[UiModuleExport],
) -> Result<String, CodegenError> {
    let mut lines = Vec::new();

    for export in exports {
        validate_patch_identifier(
            &export.module,
            "UI module name",
            Path::new("src/components/ui/mod.rs"),
        )?;
        validate_module_path(
            &export.path,
            "UI export path",
            Path::new("src/components/ui/mod.rs"),
        )?;
        for symbol in &export.symbols {
            validate_patch_identifier(
                symbol,
                "UI export symbol",
                Path::new("src/components/ui/mod.rs"),
            )?;
        }

        lines.push(format!("pub mod {};", export.module));
        if !export.symbols.is_empty() {
            if let [symbol] = export.symbols.as_slice() {
                lines.push(format!("pub use {}::{};", export.path, symbol));
            } else {
                lines.push(format!(
                    "pub use {}::{{{}}};",
                    export.path,
                    export.symbols.join(", ")
                ));
            }
        }
    }

    let borrowed = lines.iter().map(String::as_str).collect::<Vec<_>>();
    patch_module_lines(
        existing.unwrap_or_default(),
        "src/components/ui/mod.rs",
        &borrowed,
    )
}

fn load_or_empty_lock(
    project_root: &Path,
    lock_path: &str,
    config_hash: String,
) -> Result<InstallLock, CodegenError> {
    let path = project_root.join(lock_path);
    if path.is_file() {
        let input = read_to_string(&path)?;
        let mut lock = serde_json::from_str::<InstallLock>(&input).map_err(|source| {
            CodegenError::LockParse {
                path: path.clone(),
                source,
            }
        })?;
        lock.validate_at_path(Path::new(lock_path))?;
        lock.project.config_hash = config_hash;
        return Ok(lock);
    }

    Ok(InstallLock::empty(config_hash))
}

fn plan_generated_source_file(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    lock: &InstallLock,
    item_id: &str,
    logical_path: &str,
    generated: &str,
) -> Result<(), CodegenError> {
    if let Some(owner) = lock.files_by_path.get(logical_path) {
        if owner != item_id {
            return unsafe_patch(
                logical_path,
                format!("target is already tracked by {owner}"),
            );
        }

        let current = read_optional_to_string(&project_root.join(logical_path))?;
        let Some(current) = current else {
            return upsert_planned_file(
                project_root,
                files,
                changes,
                logical_path,
                generated.to_owned(),
                ChangeKind::CreateFile,
                Some(item_id),
            );
        };
        if current == generated {
            return Ok(());
        }
        let tracked = tracked_file_lock(lock, item_id, logical_path)?;
        if hash_bytes(current.as_bytes()) != tracked.local_hash_at_install {
            return unsafe_patch(logical_path, "tracked target has local edits");
        }
        return upsert_planned_file(
            project_root,
            files,
            changes,
            logical_path,
            generated.to_owned(),
            ChangeKind::UpdateFile,
            Some(item_id),
        );
    }

    if project_root.join(logical_path).is_file() {
        return unsafe_patch(logical_path, "target exists but is not tracked in lock");
    }

    upsert_planned_file(
        project_root,
        files,
        changes,
        logical_path,
        generated.to_owned(),
        ChangeKind::CreateFile,
        Some(item_id),
    )
}

fn upsert_planned_file(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    logical_path: &str,
    content: String,
    change_kind: ChangeKind,
    item_id: Option<&str>,
) -> Result<(), CodegenError> {
    if let Some(file) = files.iter_mut().find(|file| file.path == logical_path) {
        if file.content != content {
            file.content = content;
        }
        return Ok(());
    }

    let existing = read_optional_to_string(&project_root.join(logical_path))?;
    if existing.as_deref() == Some(content.as_str()) {
        return Ok(());
    }

    let action = if existing.is_some() {
        PlannedFileAction::Update
    } else {
        PlannedFileAction::Create
    };
    files.push(PlannedFile {
        path: logical_path.to_owned(),
        action,
        content,
    });

    let mut change = ChangeRecord::new(change_kind, logical_path, true);
    if let Some(item_id) = item_id {
        change = change.with_item(item_id);
    }
    changes.push(change);
    Ok(())
}

fn planned_or_existing_content(
    files: &[PlannedFile],
    project_root: &Path,
    logical_path: &str,
) -> Result<Option<String>, CodegenError> {
    if let Some(file) = files.iter().find(|file| file.path == logical_path) {
        return Ok(Some(file.content.clone()));
    }
    read_optional_to_string(&project_root.join(logical_path))
}

fn planned_or_existing_kit_config_content(
    project_root: &Path,
    files: &[PlannedFile],
) -> Result<String, CodegenError> {
    if let Some(content) =
        planned_or_existing_content(files, project_root, DEFAULT_KIT_CONFIG_PATH)?
    {
        return Ok(content);
    }

    Ok(canonical_kit_json()?)
}

fn lock_file_write_paths(changes: &[ChangeRecord]) -> Vec<&str> {
    changes
        .iter()
        .filter(|change| change.kind == ChangeKind::WriteLockFile)
        .map(|change| change.path.as_str())
        .collect()
}

fn read_optional_to_string(path: &Path) -> Result<Option<String>, CodegenError> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CodegenError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn tracked_file_lock<'a>(
    lock: &'a InstallLock,
    item_id: &str,
    logical_path: &str,
) -> Result<&'a InstalledFile, CodegenError> {
    let path = PathBuf::from(DEFAULT_KIT_LOCK_PATH);
    let item = lock
        .items
        .get(item_id)
        .ok_or_else(|| CodegenError::InvalidLock {
            path: path.clone(),
            reason: format!("missing item {item_id}"),
        })?;
    item.files
        .iter()
        .find(|file| file.path == logical_path)
        .ok_or_else(|| CodegenError::InvalidLock {
            path: path.clone(),
            reason: format!("missing file lock entry for {logical_path}"),
        })
}

fn tracked_style_generated_hash(
    lock: &InstallLock,
    item_id: &str,
    block_id: &str,
) -> Result<Option<String>, CodegenError> {
    let Some(owner) = lock.style_blocks_by_id.get(block_id) else {
        return Ok(None);
    };
    if owner != item_id {
        return unsafe_patch(
            "styles/kit.css",
            format!("CSS block is already tracked by {owner}"),
        );
    }

    let path = PathBuf::from(DEFAULT_KIT_LOCK_PATH);
    let item = lock
        .items
        .get(item_id)
        .ok_or_else(|| CodegenError::InvalidLock {
            path: path.clone(),
            reason: format!("missing item {item_id}"),
        })?;
    let block = item
        .style_blocks
        .iter()
        .find(|block| block.block_id == block_id)
        .ok_or_else(|| CodegenError::InvalidLock {
            path: path.clone(),
            reason: format!("missing style block lock entry for {block_id}"),
        })?;

    Ok(Some(block.generated_hash.clone()))
}

fn ui_exports_for_item(item: &RegistryItem) -> Result<Vec<UiModuleExport>, CodegenError> {
    let mut exports = BTreeMap::<(String, String), Vec<String>>::new();
    for file in &item.files {
        if file.target.exports.is_empty() {
            continue;
        }
        let (module, path) = ui_export_paths_for_target(&file.target.path)?;
        exports
            .entry((module, path))
            .or_default()
            .extend(file.target.exports.clone());
    }

    let mut output = Vec::new();
    for ((module, path), mut symbols) in exports {
        symbols.sort();
        symbols.dedup();
        output.push(UiModuleExport::with_path(module, path, symbols));
    }
    Ok(output)
}

fn ui_export_paths_for_target(target_path: &str) -> Result<(String, String), CodegenError> {
    let parts = target_path.split('/').collect::<Vec<_>>();
    let Some(first) = parts.first() else {
        return unsafe_patch("src/components/ui/mod.rs", "missing UI target path");
    };
    let module = if parts.len() == 1 {
        first.trim_end_matches(".rs").to_owned()
    } else {
        (*first).to_owned()
    };

    let mut path_parts = Vec::new();
    if parts.len() == 1 {
        path_parts.push(module.clone());
    } else {
        for part in &parts[..parts.len() - 1] {
            path_parts.push((*part).to_owned());
        }
        let file_stem = parts[parts.len() - 1].trim_end_matches(".rs");
        if file_stem != "mod" {
            path_parts.push(file_stem.to_owned());
        }
    }

    Ok((module, path_parts.join("::")))
}

fn built_in_item_id(item_name: &str) -> String {
    format!("builtin:{item_name}")
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

pub fn hash_content_bytes(bytes: &[u8]) -> String {
    hash_bytes(bytes)
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

fn unsafe_path<T>(path: &str, reason: &str) -> Result<T, CodegenError> {
    Err(CodegenError::UnsafePath {
        path: path.to_owned(),
        reason: reason.to_owned(),
    })
}

fn unsafe_patch<T>(path: impl Into<PathBuf>, reason: impl Into<String>) -> Result<T, CodegenError> {
    Err(CodegenError::UnsafePatch {
        path: path.into(),
        reason: reason.into(),
    })
}

fn invalid_lock<T>(path: &Path, reason: impl Into<String>) -> Result<T, CodegenError> {
    Err(CodegenError::InvalidLock {
        path: path.to_path_buf(),
        reason: reason.into(),
    })
}

fn nearest_existing_parent(path: &Path) -> Option<PathBuf> {
    let mut candidate = path.parent()?;
    loop {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        candidate = candidate.parent()?;
    }
}

fn is_allowed_write_path(path: &str) -> bool {
    matches!(
        path,
        DEFAULT_KIT_CONFIG_PATH | DEFAULT_KIT_LOCK_PATH | "index.html" | "src/components/mod.rs"
    ) || is_allowed_stylesheet_path(path)
        || path.starts_with("src/components/ui/")
}

fn is_allowed_stylesheet_path(path: &str) -> bool {
    path.starts_with("styles/") && path.ends_with(".css")
}

fn plan_kit_json(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
) -> Result<(), CodegenError> {
    let path = project_root.join(DEFAULT_KIT_CONFIG_PATH);
    if path.is_file() {
        parse_kit_json_str(&read_to_string(&path)?)?;
        return Ok(());
    }

    push_file_plan(
        files,
        changes,
        DEFAULT_KIT_CONFIG_PATH,
        PlannedFileAction::Create,
        canonical_kit_json()?,
        ChangeKind::CreateFile,
    );
    Ok(())
}

fn plan_stylesheet(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    config: &KitConfig,
) -> Result<(), CodegenError> {
    let css_path = config.styles.css.as_str();
    let path = project_root.join(css_path);
    if path.is_file() {
        return Ok(());
    }

    push_file_plan(
        files,
        changes,
        css_path,
        PlannedFileAction::Create,
        String::new(),
        ChangeKind::CreateFile,
    );
    Ok(())
}

fn plan_index_html(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    config: &KitConfig,
) -> Result<(), CodegenError> {
    let path = project_root.join("index.html");
    let html = read_to_string(&path)?;
    let css_path = config.styles.css.as_str();
    if contains_trunk_css_link(&html, css_path) {
        return Ok(());
    }

    let Some(head_end) = html.find("</head>") else {
        return Err(CodegenError::UnsafePatch {
            path,
            reason: "missing </head> marker".to_owned(),
        });
    };

    if html.matches("<head").count() != 1 || html.matches("</head>").count() != 1 {
        return Err(CodegenError::UnsafePatch {
            path,
            reason: "ambiguous head element".to_owned(),
        });
    }

    let insert_at = first_head_trunk_css_link_index(&html, head_end).unwrap_or(head_end);
    let indent = line_indent_at(&html, insert_at).unwrap_or("    ");
    let link = format!("{indent}<link data-trunk rel=\"css\" href=\"{css_path}\" />\n");

    let mut patched = html;
    patched.insert_str(insert_at, &link);

    push_file_plan(
        files,
        changes,
        "index.html",
        PlannedFileAction::Update,
        patched,
        ChangeKind::UpdateFile,
    );
    Ok(())
}

fn contains_trunk_css_link(html: &str, css_path: &str) -> bool {
    html.lines().any(|line| {
        line.contains("data-trunk")
            && line.contains("rel=\"css\"")
            && line.contains(&format!("href=\"{css_path}\""))
    })
}

fn first_head_trunk_css_link_index(html: &str, head_end: usize) -> Option<usize> {
    let mut offset = 0;
    for line in html.split_inclusive('\n') {
        if offset >= head_end {
            return None;
        }
        if line.contains("data-trunk") && line.contains("rel=\"css\"") {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

fn line_indent_at(html: &str, index: usize) -> Option<&str> {
    let line = html.get(index..)?.lines().next()?;
    let indent_len = line
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    line.get(..indent_len)
}

fn plan_component_modules(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
) -> Result<(), CodegenError> {
    let components_mod = project_root.join("src/components/mod.rs");
    if !components_mod.is_file() {
        push_file_plan(
            files,
            changes,
            "src/components/mod.rs",
            PlannedFileAction::Create,
            patch_components_mod(None)?,
            ChangeKind::CreateFile,
        );
    } else {
        let existing = read_to_string(&components_mod)?;
        let patched = patch_components_mod(Some(&existing))?;
        if patched != existing {
            push_file_plan(
                files,
                changes,
                "src/components/mod.rs",
                PlannedFileAction::Update,
                patched,
                ChangeKind::UpdateFile,
            );
        }
    }

    let ui_mod = project_root.join("src/components/ui/mod.rs");
    if !ui_mod.is_file() {
        push_file_plan(
            files,
            changes,
            "src/components/ui/mod.rs",
            PlannedFileAction::Create,
            String::new(),
            ChangeKind::CreateFile,
        );
    }

    Ok(())
}

fn normalize_managed_css_block(block_id: &str, block: &str) -> Result<String, CodegenError> {
    let start_marker = css_start_marker(block_id);
    let end_marker = css_end_marker(block_id);

    if block.matches(&start_marker).count() != 1 || block.matches(&end_marker).count() != 1 {
        return unsafe_patch(
            "styles/kit.css",
            format!("managed CSS block {block_id} must contain exactly one start and end marker"),
        );
    }

    let Some(start) = block.find(&start_marker) else {
        return unsafe_patch(
            "styles/kit.css",
            format!("managed CSS block {block_id} is missing its start marker"),
        );
    };
    let Some(end) = block.find(&end_marker) else {
        return unsafe_patch(
            "styles/kit.css",
            format!("managed CSS block {block_id} is missing its end marker"),
        );
    };
    if start > end {
        return unsafe_patch(
            "styles/kit.css",
            format!("managed CSS block {block_id} markers are reversed"),
        );
    }

    let mut normalized = block.trim_matches('\n').to_owned();
    normalized.push('\n');
    Ok(normalized)
}

fn find_managed_css_block(
    existing: &str,
    block_id: &str,
) -> Result<Option<std::ops::Range<usize>>, CodegenError> {
    let start_marker = css_start_marker(block_id);
    let end_marker = css_end_marker(block_id);
    let start_count = existing.matches(&start_marker).count();
    let end_count = existing.matches(&end_marker).count();

    match (start_count, end_count) {
        (0, 0) => Ok(None),
        (1, 1) => {
            let start = existing.find(&start_marker).expect("count confirmed start");
            let end_start = existing.find(&end_marker).expect("count confirmed end");
            if start > end_start {
                return unsafe_patch(
                    "styles/kit.css",
                    format!("managed CSS block {block_id} markers are reversed"),
                );
            }
            let mut end = end_start + end_marker.len();
            if existing[end..].starts_with('\n') {
                end += 1;
            }
            Ok(Some(start..end))
        }
        _ => unsafe_patch(
            "styles/kit.css",
            format!("managed CSS block {block_id} markers are ambiguous"),
        ),
    }
}

fn css_start_marker(block_id: &str) -> String {
    format!("/* leptos-ui-kit:start {block_id} */")
}

fn css_end_marker(block_id: &str) -> String {
    format!("/* leptos-ui-kit:end {block_id} */")
}

fn patch_module_lines(
    existing: &str,
    logical_path: &str,
    required_lines: &[&str],
) -> Result<String, CodegenError> {
    let mut output = existing.to_owned();

    for line in required_lines {
        if line.trim() != *line || line.is_empty() {
            return unsafe_patch(logical_path, "module patch line must be normalized");
        }
        if module_line_exists(&output, line)? {
            continue;
        }
        if detects_private_module_conflict(&output, line) {
            return unsafe_patch(
                logical_path,
                format!("private module declaration conflicts with required line `{line}`"),
            );
        }
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(line);
        output.push('\n');
    }

    Ok(output)
}

fn module_line_exists(existing: &str, required_line: &str) -> Result<bool, CodegenError> {
    if existing
        .lines()
        .any(|existing_line| existing_line.trim() == required_line)
    {
        return Ok(true);
    }

    let Some((path, symbols)) = parse_grouped_pub_use(required_line)? else {
        return Ok(false);
    };

    let marker = format!("pub use {path}::{{");
    let mut offset = 0;
    while let Some(relative_start) = existing[offset..].find(&marker) {
        let start = offset + relative_start + marker.len();
        let Some(relative_end) = existing[start..].find("};") else {
            return Ok(false);
        };
        let end = start + relative_end;
        if grouped_pub_use_contains(&existing[start..end], &symbols) {
            return Ok(true);
        }
        offset = end + 2;
    }

    Ok(false)
}

fn parse_grouped_pub_use(required_line: &str) -> Result<Option<(&str, Vec<&str>)>, CodegenError> {
    let Some(body) = required_line
        .strip_prefix("pub use ")
        .and_then(|line| line.strip_suffix("};"))
    else {
        return Ok(None);
    };
    let Some((path, symbols)) = body.split_once("::{") else {
        return Ok(None);
    };
    validate_module_path(
        path,
        "UI export path",
        Path::new("src/components/ui/mod.rs"),
    )?;
    let symbols = symbols
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .collect::<Vec<_>>();
    if symbols.is_empty() {
        return Ok(None);
    }
    for symbol in &symbols {
        validate_patch_identifier(
            symbol,
            "UI export symbol",
            Path::new("src/components/ui/mod.rs"),
        )?;
    }

    Ok(Some((path, symbols)))
}

fn grouped_pub_use_contains(existing_symbols: &str, required_symbols: &[&str]) -> bool {
    let existing_symbols = existing_symbols
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .collect::<Vec<_>>();

    required_symbols
        .iter()
        .all(|symbol| existing_symbols.iter().any(|existing| existing == symbol))
}

fn detects_private_module_conflict(existing: &str, required_line: &str) -> bool {
    let Some(module_name) = required_line
        .strip_prefix("pub mod ")
        .and_then(|line| line.strip_suffix(';'))
    else {
        return false;
    };
    let private_line = format!("mod {module_name};");
    existing
        .lines()
        .any(|existing_line| existing_line.trim() == private_line)
}

fn validate_patch_identifier(value: &str, label: &str, path: &Path) -> Result<(), CodegenError> {
    if value.is_empty()
        || value.as_bytes()[0].is_ascii_digit()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return unsafe_patch(
            path,
            format!("{label} must be a Rust-style ASCII identifier"),
        );
    }
    Ok(())
}

fn validate_module_path(value: &str, label: &str, path: &Path) -> Result<(), CodegenError> {
    if value.is_empty() || value.contains(":::") {
        return unsafe_patch(path, format!("{label} must be a Rust module path"));
    }

    for segment in value.split("::") {
        validate_patch_identifier(segment, label, path)?;
    }

    Ok(())
}

fn validate_css_block_id(block_id: &str) -> Result<(), CodegenError> {
    if block_id.is_empty()
        || !block_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return unsafe_patch(
            "styles/kit.css",
            "CSS block id must be lowercase ASCII, digits, or hyphens",
        );
    }
    Ok(())
}

fn plan_empty_state(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
) -> Result<(), CodegenError> {
    let config_content = planned_or_existing_kit_config_content(project_root, files)?;
    let config = parse_kit_json_str(&config_content)?;
    let state_path = install_lock_path(&config);
    let path = project_root.join(&state_path);
    if path.is_file() {
        return Ok(());
    }

    let content = empty_lock_json(&config_content, &state_path)?;
    push_file_plan(
        files,
        changes,
        &state_path,
        PlannedFileAction::Create,
        content,
        ChangeKind::WriteLockFile,
    );
    Ok(())
}

fn push_file_plan(
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    path: &str,
    action: PlannedFileAction,
    content: String,
    change_kind: ChangeKind,
) {
    files.push(PlannedFile {
        path: path.to_owned(),
        action,
        content,
    });
    changes.push(ChangeRecord::new(change_kind, path, true));
}

fn empty_lock_json(config_content: &str, state_path: &str) -> Result<String, CodegenError> {
    lock_to_json_at_path(
        &InstallLock::empty(hash_bytes(config_content.as_bytes())),
        Path::new(state_path),
    )
}

fn read_to_string(path: &Path) -> Result<String, CodegenError> {
    fs::read_to_string(path).map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct DemoData {
        value: &'static str,
    }

    #[test]
    fn serializes_diagnostics_and_change_records_in_json_envelope() {
        let envelope =
            CommandEnvelope::new("add", CommandStatus::Planned, DemoData { value: "ok" })
                .with_diagnostics(vec![
                    Diagnostic::new(DiagnosticLevel::Warning, "demo.warning", "heads up")
                        .with_path(DEFAULT_KIT_CONFIG_PATH)
                        .with_suggestion("Run leptos_ui_kit init."),
                ])
                .with_changes(vec![
                    ChangeRecord::new(ChangeKind::CreateFile, "src/components/ui/button.rs", true)
                        .with_item("builtin:button"),
                ]);

        let json = serde_json::to_string(&envelope).expect("serialize envelope");

        assert!(json.contains(r#""schemaVersion":"0.9.0-alpha""#));
        assert!(json.contains(r#""command":"add""#));
        assert!(json.contains(r#""status":"planned""#));
        assert!(json.contains(r#""level":"warning""#));
        assert!(json.contains(r#""kind":"create_file""#));
        assert!(json.contains(r#""data":{"value":"ok"}"#));
    }

    #[test]
    fn init_plan_creates_missing_project_files_without_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");

        let plan = plan_init(root).expect("plan init");

        assert_eq!(plan.files.len(), 6);
        assert!(
            plan.files
                .iter()
                .any(|file| file.path == DEFAULT_KIT_CONFIG_PATH)
        );
        assert!(plan.files.iter().any(|file| file.path == "styles/kit.css"));
        assert!(plan.files.iter().any(|file| file.path == "index.html"));
        assert!(
            plan.files
                .iter()
                .any(|file| file.path == "src/components/mod.rs")
        );
        assert!(
            plan.files
                .iter()
                .any(|file| file.path == DEFAULT_KIT_LOCK_PATH)
        );
        assert!(!root.join(DEFAULT_KIT_CONFIG_PATH).exists());
    }

    #[test]
    fn init_plan_rejects_invalid_existing_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_kit_config(root, "{\"tailwind\":true}\n");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");

        let error = plan_init(root).expect_err("invalid config should fail");

        assert!(matches!(error, CodegenError::Config(_)));
    }

    #[test]
    fn init_plan_uses_configured_stylesheet_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        let config = canonical_kit_json().expect("canonical config").replace(
            "\"css\": \"styles/kit.css\"",
            "\"css\": \"styles/custom.css\"",
        );
        write_kit_config(root, config);
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");

        let plan = plan_init(root).expect("plan init");

        assert!(
            plan.files
                .iter()
                .any(|file| file.path == "styles/custom.css")
        );
        assert!(!plan.files.iter().any(|file| file.path == "styles/kit.css"));
        let index = plan
            .files
            .iter()
            .find(|file| file.path == "index.html")
            .expect("index plan");
        assert!(index.content.contains("styles/custom.css"));
    }

    #[test]
    fn init_write_creates_expected_files_and_releases_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");

        let plan = apply_init(root).expect("apply init");

        assert!(!plan.is_empty());
        assert!(root.join(DEFAULT_KIT_CONFIG_PATH).is_file());
        assert!(root.join("styles/kit.css").is_file());
        assert!(root.join("src/components/mod.rs").is_file());
        assert!(root.join("src/components/ui/mod.rs").is_file());
        assert!(root.join(DEFAULT_KIT_LOCK_PATH).is_file());
        assert!(!root.join(DEFAULT_KIT_WRITE_LOCK_PATH).exists());
        assert!(
            fs::read_to_string(root.join("index.html"))
                .expect("read index")
                .contains("styles/kit.css")
        );
    }

    #[test]
    fn init_write_is_idempotent_when_files_are_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");

        apply_init(root).expect("first init");
        let second = apply_init(root).expect("second init");

        assert!(second.is_empty());
    }

    #[test]
    fn lock_round_trips_deterministically() {
        let lock = InstallLock::empty(hash_bytes(b"components"));
        let first = lock_to_json(&lock).expect("serialize first");
        let parsed = parse_install_lock_str(&first).expect("parse lock");
        let second = lock_to_json(&parsed).expect("serialize second");

        assert_eq!(first, second);
        assert!(first.contains("\"schemaVersion\": \"0.9.0-alpha\""));
        assert!(first.contains("\"configHash\": \"sha256:"));
        assert!(!first.contains("null"));
    }

    #[test]
    fn lock_rejects_malformed_hash_fields() {
        let mut lock = InstallLock::empty("sha256:not-a-real-hash".to_owned());

        let error = lock.validate().expect_err("config hash should fail");

        assert!(
            matches!(error, CodegenError::InvalidLock { reason, .. } if reason.contains("project.configHash"))
        );

        lock.project.config_hash = hash_bytes(b"components");
        lock.items.insert(
            "builtin:button".to_owned(),
            InstalledItem {
                id: "builtin:button".to_owned(),
                name: "button".to_owned(),
                source: "builtin".to_owned(),
                version: SCHEMA_VERSION.to_owned(),
                content_hash: "missing-prefix".to_owned(),
                files: Vec::new(),
                style_blocks: Vec::new(),
            },
        );

        let error = lock.validate().expect_err("content hash should fail");

        assert!(
            matches!(error, CodegenError::InvalidLock { reason, .. } if reason.contains("items[].contentHash"))
        );
    }

    #[test]
    fn add_plan_records_generated_hashes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        apply_init(root).expect("init");

        let plan = plan_add(root, "button").expect("plan add");
        let source = read_built_in_registry_source("ui/button.rs").expect("registry source");
        let css = read_built_in_registry_source("styles/button.css").expect("registry css");
        let rust_target = plan
            .files
            .iter()
            .find(|file| file.path == "src/components/ui/button.rs")
            .expect("rust target");
        let installed_file = &plan.lock.items["builtin:button"].files[0];
        let installed_block = &plan.lock.items["builtin:button"].style_blocks[0];

        assert_eq!(rust_target.content, source);
        assert_eq!(installed_file.generated_hash, hash_bytes(source.as_bytes()));
        assert_eq!(
            installed_file.local_hash_at_install,
            hash_bytes(source.as_bytes())
        );
        assert_eq!(installed_block.generated_hash, hash_bytes(css.as_bytes()));
        assert_eq!(
            plan.lock.files_by_path.get("src/components/ui/button.rs"),
            Some(&"builtin:button".to_owned())
        );
        assert_eq!(
            plan.lock.style_blocks_by_id.get("button"),
            Some(&"builtin:button".to_owned())
        );
        assert_eq!(plan.cargo_plan.len(), 2);
        assert!(
            plan.cargo_plan
                .iter()
                .any(|entry| entry.crate_name == "leptos" && entry.features == ["csr".to_owned()])
        );
    }

    #[test]
    fn add_plan_reports_button_changes_without_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        apply_init(root).expect("init");

        let plan = plan_add(root, "button").expect("plan add");
        let paths = plan
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();

        assert!(paths.contains(&"src/components/ui/button.rs"));
        assert!(paths.contains(&"src/components/ui/mod.rs"));
        assert!(paths.contains(&"styles/kit.css"));
        assert!(paths.contains(&DEFAULT_KIT_LOCK_PATH));
        assert_eq!(plan.cargo_plan.len(), 2);
        assert!(!root.join("src/components/ui/button.rs").exists());
        assert!(root.join(DEFAULT_KIT_LOCK_PATH).is_file());
    }

    #[test]
    fn add_plan_uses_configured_stylesheet_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        let config = canonical_kit_json().expect("canonical config").replace(
            "\"css\": \"styles/kit.css\"",
            "\"css\": \"styles/custom.css\"",
        );
        write_kit_config(root, config);
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        apply_init(root).expect("init");

        let plan = plan_add(root, "button").expect("plan add");

        assert!(
            plan.files
                .iter()
                .any(|file| file.path == "styles/custom.css")
        );
        assert!(!plan.files.iter().any(|file| file.path == "styles/kit.css"));
        assert_eq!(
            plan.lock.items["builtin:button"].style_blocks[0].css_path,
            "styles/custom.css"
        );
    }

    #[test]
    fn item_planner_supports_nested_ui_targets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        apply_init(root).expect("init");
        let mut files = Vec::new();
        let mut changes = Vec::new();
        let mut lock = InstallLock::empty(hash_bytes(b"components"));
        let config = parse_kit_json_str(
            &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
        )
        .expect("parse config");
        let item = nested_registry_item();

        let item_id = plan_built_in_item(root, &mut files, &mut changes, &mut lock, &config, &item)
            .expect("plan item");
        let paths = files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        let ui_mod = files
            .iter()
            .find(|file| file.path == "src/components/ui/mod.rs")
            .expect("ui mod");

        assert_eq!(item_id, "builtin:nested");
        assert!(paths.contains(&"src/components/ui/nested/root.rs"));
        assert!(
            ui_mod
                .content
                .contains("pub use nested::root::{NestedButton};")
        );
        assert_eq!(
            lock.files_by_path.get("src/components/ui/nested/root.rs"),
            Some(&"builtin:nested".to_owned())
        );
    }

    #[test]
    fn add_write_installs_button_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        apply_init(root).expect("init");

        let plan = apply_add(root, "button").expect("apply add");

        assert!(!plan.is_empty());
        assert!(root.join("src/components/ui/button.rs").is_file());
        assert!(
            fs::read_to_string(root.join("src/components/ui/mod.rs"))
                .expect("read ui mod")
                .contains("pub use button::{Button, ButtonSize, ButtonType, ButtonVariant};")
        );
        assert!(
            fs::read_to_string(root.join("styles/kit.css"))
                .expect("read css")
                .contains("/* leptos-ui-kit:start button */")
        );
        let lock = parse_install_lock_str_at_path(
            &fs::read_to_string(root.join(DEFAULT_KIT_LOCK_PATH)).expect("read lock"),
            Path::new(DEFAULT_KIT_LOCK_PATH),
        )
        .expect("parse lock");
        assert!(lock.items.contains_key("builtin:button"));
        let config = parse_kit_json_str(
            &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
        )
        .expect("parse config");
        assert_eq!(config.items.len(), 1);
        assert_eq!(config.items[0].item_name(), "button");
        assert!(!root.join(DEFAULT_KIT_WRITE_LOCK_PATH).exists());
    }

    #[test]
    fn sync_plan_installs_declared_button_without_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        apply_init(root).expect("init");
        write_desired_button_config(root);

        let plan = plan_sync(root).expect("plan sync");
        let paths = plan
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();

        assert!(paths.contains(&"src/components/ui/button.rs"));
        assert!(paths.contains(&"src/components/ui/mod.rs"));
        assert!(paths.contains(&"styles/kit.css"));
        assert!(paths.contains(&DEFAULT_KIT_LOCK_PATH));
        assert!(!root.join("src/components/ui/button.rs").exists());
        assert_eq!(plan.item_ids, vec!["builtin:button".to_owned()]);
    }

    #[test]
    fn sync_write_is_idempotent_when_declared_button_is_current() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        apply_init(root).expect("init");
        write_desired_button_config(root);

        let first = apply_sync(root).expect("first sync");
        let second = apply_sync(root).expect("second sync");

        assert!(!first.is_empty());
        assert!(second.is_empty());
        assert!(root.join("src/components/ui/button.rs").is_file());
    }

    #[test]
    fn add_write_is_idempotent_when_tracked_files_are_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        apply_init(root).expect("init");

        apply_add(root, "button").expect("first add");
        let second = apply_add(root, "button").expect("second add");

        assert!(second.is_empty());
    }

    #[test]
    fn add_plan_rejects_untracked_existing_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        apply_init(root).expect("init");
        fs::write(root.join("src/components/ui/button.rs"), "// local\n").expect("write local");

        let error = plan_add(root, "button").expect_err("untracked target should conflict");

        assert!(matches!(error, CodegenError::UnsafePatch { .. }));
    }

    #[test]
    fn add_plan_rejects_tracked_local_edits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        apply_init(root).expect("init");
        apply_add(root, "button").expect("add");
        fs::write(root.join("src/components/ui/button.rs"), "// edited\n").expect("edit source");

        let error = plan_add(root, "button").expect_err("tracked edit should conflict");

        assert!(matches!(error, CodegenError::UnsafePatch { .. }));
    }

    fn write_kit_config(root: &Path, config: impl AsRef<[u8]>) {
        let path = root.join(DEFAULT_KIT_CONFIG_PATH);
        fs::create_dir_all(path.parent().expect("kit config parent")).expect("create kit dir");
        fs::write(path, config).expect("write config");
    }

    fn write_desired_button_config(root: &Path) {
        let config = parse_kit_json_str(
            &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
        )
        .expect("parse config");
        let config = kit_config_with_desired_item(config, desired_builtin_button_item())
            .expect("add desired item");
        write_kit_config(root, kit_config_to_json(&config).expect("serialize config"));
    }

    #[test]
    fn css_patcher_appends_managed_block() {
        let existing = ":root {\n  color-scheme: light;\n}\n";
        let block =
            "/* leptos-ui-kit:start button */\n.kit-button {}\n/* leptos-ui-kit:end button */\n";

        let patched = patch_css_block(existing, "button", block, None).expect("patch css");

        assert!(patched.starts_with(existing));
        assert!(patched.contains("/* leptos-ui-kit:start button */"));
        assert!(patched.contains(".kit-button {}"));
        assert!(patched.ends_with("/* leptos-ui-kit:end button */\n"));
    }

    #[test]
    fn css_patcher_is_idempotent_for_existing_matching_block() {
        let block =
            "/* leptos-ui-kit:start button */\n.kit-button {}\n/* leptos-ui-kit:end button */\n";

        let patched = patch_css_block(block, "button", block, None).expect("patch css");

        assert_eq!(patched, block);
    }

    #[test]
    fn css_patcher_replaces_tracked_generated_block() {
        let previous = "/* leptos-ui-kit:start button */\n.kit-button { color: red; }\n/* leptos-ui-kit:end button */\n";
        let next = "/* leptos-ui-kit:start button */\n.kit-button { color: blue; }\n/* leptos-ui-kit:end button */\n";
        let existing = format!("/* app */\n{previous}.other {{}}\n");

        let previous_hash = hash_bytes(previous.as_bytes());
        let patched =
            patch_css_block(&existing, "button", next, Some(&previous_hash)).expect("patch css");

        assert!(patched.contains("color: blue"));
        assert!(!patched.contains("color: red"));
        assert!(patched.contains(".other {}"));
    }

    #[test]
    fn css_block_extractor_requires_exact_managed_markers() {
        let block =
            "/* leptos-ui-kit:start button */\n.kit-button {}\n/* leptos-ui-kit:end button */\n";
        let css = format!(":root {{}}\n\n{block}.app {{}}\n");

        let extracted = extract_managed_css_block(&css, "button").expect("extract block");

        assert_eq!(extracted, Some(block.to_owned()));
        assert_eq!(
            extract_managed_css_block(":root {}\n", "button").expect("missing block"),
            None
        );
        assert!(extract_managed_css_block(&format!("{block}{block}"), "button").is_err());
    }

    #[test]
    fn css_patcher_rejects_edited_tracked_block() {
        let previous = "/* leptos-ui-kit:start button */\n.kit-button { color: red; }\n/* leptos-ui-kit:end button */\n";
        let edited = "/* leptos-ui-kit:start button */\n.kit-button { color: green; }\n/* leptos-ui-kit:end button */\n";
        let next = "/* leptos-ui-kit:start button */\n.kit-button { color: blue; }\n/* leptos-ui-kit:end button */\n";

        let previous_hash = hash_bytes(previous.as_bytes());
        let error = patch_css_block(edited, "button", next, Some(&previous_hash))
            .expect_err("should conflict");

        assert!(matches!(error, CodegenError::UnsafePatch { .. }));
    }

    #[test]
    fn module_patchers_insert_required_exports() {
        let components = patch_components_mod(Some("// existing\n")).expect("patch components");
        let ui = patch_ui_mod(
            Some("// generated exports\n"),
            &[
                UiModuleExport::new(
                    "button",
                    vec![
                        "Button".to_owned(),
                        "ButtonSize".to_owned(),
                        "ButtonType".to_owned(),
                        "ButtonVariant".to_owned(),
                    ],
                ),
                UiModuleExport::with_path(
                    "collapsible",
                    "collapsible::root",
                    vec!["CollapsibleRoot".to_owned()],
                ),
            ],
        )
        .expect("patch ui mod");

        assert_eq!(components, "// existing\npub mod ui;\n");
        assert_eq!(
            ui,
            "// generated exports\npub mod button;\npub use button::{Button, ButtonSize, ButtonType, ButtonVariant};\npub mod collapsible;\npub use collapsible::root::{CollapsibleRoot};\n"
        );
        assert_eq!(
            patch_ui_mod(
                Some(&ui),
                &[
                    UiModuleExport::new(
                        "button",
                        vec![
                            "Button".to_owned(),
                            "ButtonSize".to_owned(),
                            "ButtonType".to_owned(),
                            "ButtonVariant".to_owned(),
                        ],
                    ),
                    UiModuleExport::with_path(
                        "collapsible",
                        "collapsible::root",
                        vec!["CollapsibleRoot".to_owned()],
                    ),
                ],
            )
            .expect("idempotent enough"),
            ui
        );
    }

    #[test]
    fn ui_module_patcher_accepts_formatted_grouped_exports() {
        let existing = "pub mod menu;\npub use menu::{\n    MenuContent, MenuDirection, MenuItem, MenuItemIndicator, MenuItemKind, MenuLoop, MenuRoot,\n    MenuTrigger,\n};\n";
        let patched = patch_ui_mod(
            Some(existing),
            &[UiModuleExport::new(
                "menu",
                vec![
                    "MenuContent".to_owned(),
                    "MenuDirection".to_owned(),
                    "MenuItem".to_owned(),
                    "MenuItemIndicator".to_owned(),
                    "MenuItemKind".to_owned(),
                    "MenuLoop".to_owned(),
                    "MenuRoot".to_owned(),
                    "MenuTrigger".to_owned(),
                ],
            )],
        )
        .expect("formatted grouped export should be idempotent");

        assert_eq!(patched, existing);
    }

    #[test]
    fn ui_module_patcher_emits_rustfmt_stable_single_exports() {
        let ui = patch_ui_mod(
            Some("// generated exports\n"),
            &[UiModuleExport::new("spinner", vec!["Spinner".to_owned()])],
        )
        .expect("patch ui mod");

        assert_eq!(
            ui,
            "// generated exports\npub mod spinner;\npub use spinner::Spinner;\n"
        );
        assert_eq!(
            patch_ui_mod(
                Some(&ui),
                &[UiModuleExport::new("spinner", vec!["Spinner".to_owned()])],
            )
            .expect("formatted single export should be idempotent"),
            ui
        );
    }

    #[test]
    fn component_module_patcher_rejects_private_conflict() {
        let error = patch_components_mod(Some("mod ui;\n")).expect_err("private conflict");

        assert!(matches!(error, CodegenError::UnsafePatch { .. }));
    }

    #[test]
    fn path_safety_accepts_mvp_paths() {
        let paths = vec![
            DEFAULT_KIT_CONFIG_PATH.to_owned(),
            "index.html".to_owned(),
            "styles/kit.css".to_owned(),
            "src/components/mod.rs".to_owned(),
            "src/components/ui/button.rs".to_owned(),
            "src/components/ui/nested/root.rs".to_owned(),
            DEFAULT_KIT_LOCK_PATH.to_owned(),
        ];

        validate_planned_write_paths(&paths).expect("paths should pass");
    }

    #[test]
    fn path_safety_rejects_unsafe_paths() {
        for path in [
            "../evil.rs",
            "/tmp/evil.rs",
            "C:\\evil.rs",
            "\\\\server\\share\\evil.rs",
            ".hidden",
            "src/components/ui/../../evil.rs",
            "src/components/ui/Button Rs",
            "src/lib.rs",
        ] {
            assert!(validate_logical_write_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn path_safety_rejects_casefold_duplicate_paths() {
        let paths = vec![
            "src/components/ui/button.rs".to_owned(),
            "src/components/ui/Button.rs".to_owned(),
        ];

        let error = validate_planned_write_paths(&paths).expect_err("duplicate should fail");

        assert!(matches!(error, CodegenError::DuplicatePath(_)));
    }

    #[cfg(unix)]
    #[test]
    fn path_safety_rejects_symlink_escape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src/components")).expect("create components");
        let outside = tempfile::tempdir().expect("outside");
        std::os::unix::fs::symlink(outside.path(), root.join("src/components/ui"))
            .expect("symlink");

        let error = validate_project_write_path(root, "src/components/ui/button.rs")
            .expect_err("symlink escape should fail");

        assert!(matches!(error, CodegenError::UnsafePath { .. }));
    }

    #[test]
    fn transaction_lock_fails_when_lock_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let _first = WriteLock::acquire(root).expect("first lock");
        let error = WriteLock::acquire(root).expect_err("second lock should fail");

        assert!(matches!(error, CodegenError::LockExists(_)));
    }

    #[test]
    fn transaction_atomic_write_creates_file_and_releases_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("styles")).expect("styles");

        {
            let _lock = WriteLock::acquire(root).expect("lock");
            write_file_atomic(root, "styles/kit.css", b":root {}\n").expect("write css");
            assert!(root.join(DEFAULT_KIT_WRITE_LOCK_PATH).exists());
        }

        assert_eq!(
            fs::read_to_string(root.join("styles/kit.css")).expect("read css"),
            ":root {}\n"
        );
        assert!(!root.join(DEFAULT_KIT_WRITE_LOCK_PATH).exists());
    }

    fn nested_registry_item() -> leptos_ui_kit_registry::ResolvedRegistryItem {
        leptos_ui_kit_registry::ResolvedRegistryItem {
            source_kind: leptos_ui_kit_registry::RegistrySourceKind::BuiltIn,
            source_path: PathBuf::from("registry/ui/nested.json"),
            content_hash: hash_bytes(b"nested"),
            targets: leptos_ui_kit_registry::ResolvedRegistryTargets {
                ui_files: vec![leptos_ui_kit_registry::ResolvedUiTarget {
                    source: "ui/button.rs".to_owned(),
                    path: "nested/root.rs".to_owned(),
                }],
                style_blocks: Vec::new(),
            },
            item: leptos_ui_kit_registry::RegistryItem {
                schema: leptos_ui_kit_registry::REGISTRY_ITEM_SCHEMA_URL.to_owned(),
                schema_version: leptos_ui_kit_registry::SCHEMA_VERSION.to_owned(),
                name: "nested".to_owned(),
                kind: leptos_ui_kit_registry::RegistryItemKind::Ui,
                version: leptos_ui_kit_registry::SCHEMA_VERSION.to_owned(),
                title: "Nested".to_owned(),
                description: "Nested".to_owned(),
                leptos: leptos_ui_kit_registry::RegistryLeptos {
                    version: leptos_ui_kit_registry::LEPTOS_VERSION.to_owned(),
                    router_version: leptos_ui_kit_registry::LEPTOS_ROUTER_VERSION.to_owned(),
                    render_mode: leptos_ui_kit_registry::RenderMode::Csr,
                },
                accessibility: leptos_ui_kit_registry::RegistryAccessibility::default(),
                files: vec![leptos_ui_kit_registry::RegistryItemFile {
                    source: "ui/button.rs".to_owned(),
                    target: leptos_ui_kit_registry::RegistryFileTarget {
                        kind: leptos_ui_kit_registry::RegistryFileTargetKind::Ui,
                        path: "nested/root.rs".to_owned(),
                        exports: vec!["NestedButton".to_owned()],
                    },
                }],
                styles: Vec::new(),
                registry_dependencies: Vec::new(),
                cargo_plan: Vec::new(),
                extra: BTreeMap::new(),
            },
        }
    }
}
