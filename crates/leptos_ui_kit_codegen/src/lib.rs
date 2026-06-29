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
    CargoPlanEntry, ComponentsConfig, ConfigError, RegistryError, SCHEMA_VERSION,
    canonical_components_json, components_config_to_json, components_config_with_desired_item,
    desired_builtin_button_item, load_built_in_registry_item, parse_components_json_str,
    read_built_in_registry_source,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    WriteState,
    WriteBaseline,
}

#[derive(Debug)]
pub enum CodegenError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Config(ConfigError),
    Registry(RegistryError),
    StateParse {
        path: PathBuf,
        source: serde_json::Error,
    },
    StateSerialize(serde_json::Error),
    InvalidState {
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
            Self::StateParse { path, source } => {
                write!(f, "failed to parse {}: {source}", path.display())
            }
            Self::StateSerialize(error) => write!(f, "failed to serialize state: {error}"),
            Self::InvalidState { path, reason } => {
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
    pub state: InstallState,
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
    pub state: InstallState,
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
pub struct InstallState {
    pub schema_version: String,
    pub kit_version: String,
    pub project: InstallStateProject,
    pub items: BTreeMap<String, InstalledItem>,
    pub files_by_path: BTreeMap<String, String>,
    pub style_blocks_by_id: BTreeMap<String, String>,
}

impl InstallState {
    pub fn empty(config_hash: String) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_owned(),
            kit_version: SCHEMA_VERSION.to_owned(),
            project: InstallStateProject {
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
        let path = PathBuf::from(".leptos-ui/state.json");
        if self.schema_version != SCHEMA_VERSION {
            return invalid_state(&path, format!("schemaVersion must be {SCHEMA_VERSION}"));
        }
        if self.project.crate_root != "." {
            return invalid_state(&path, "project.crateRoot must be .");
        }
        if self.project.kind != "single-crate-trunk-csr" {
            return invalid_state(&path, "project.kind must be single-crate-trunk-csr");
        }
        validate_state_hash(&path, "project.configHash", &self.project.config_hash)?;

        for (key, item) in &self.items {
            if key != &item.id {
                return invalid_state(&path, format!("item key {key} does not match item id"));
            }
            if item.source != "builtin" {
                return invalid_state(&path, "only builtin item state is supported");
            }
            if item.version != SCHEMA_VERSION {
                return invalid_state(&path, format!("item version must be {SCHEMA_VERSION}"));
            }
            validate_state_hash(&path, "items[].contentHash", &item.content_hash)?;
            for file in &item.files {
                validate_state_hash(&path, "items[].files[].baselineHash", &file.baseline_hash)?;
                validate_state_hash(
                    &path,
                    "items[].files[].localHashAtInstall",
                    &file.local_hash_at_install,
                )?;
            }
            for block in &item.style_blocks {
                validate_state_hash(
                    &path,
                    "items[].styleBlocks[].baselineHash",
                    &block.baseline_hash,
                )?;
            }
        }

        for (file_path, item_id) in &self.files_by_path {
            if !self.items.contains_key(item_id) {
                return invalid_state(
                    &path,
                    format!("filesByPath entry {file_path} references missing item {item_id}"),
                );
            }
        }

        for (block_id, item_id) in &self.style_blocks_by_id {
            if !self.items.contains_key(item_id) {
                return invalid_state(
                    &path,
                    format!("styleBlocksById entry {block_id} references missing item {item_id}"),
                );
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallStateProject {
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
    pub baseline_path: String,
    pub baseline_hash: String,
    pub local_hash_at_install: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledStyleBlock {
    pub css_path: String,
    pub block_id: String,
    pub baseline_path: String,
    pub baseline_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiModuleExport {
    pub module: String,
    pub symbols: Vec<String>,
}

impl UiModuleExport {
    pub fn new(module: impl Into<String>, symbols: Vec<String>) -> Self {
        Self {
            module: module.into(),
            symbols,
        }
    }
}

pub fn plan_init(project_root: &Path) -> Result<InitPlan, CodegenError> {
    let mut files = Vec::new();
    let mut changes = Vec::new();

    plan_components_json(project_root, &mut files, &mut changes)?;
    plan_stylesheet(project_root, &mut files, &mut changes)?;
    plan_index_html(project_root, &mut files, &mut changes)?;
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
    let paths = plan
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    validate_planned_write_paths(&paths)?;

    if plan.is_empty() {
        return Ok(plan);
    }

    let _lock = WriteLock::acquire(project_root)?;

    for file in plan
        .files
        .iter()
        .filter(|file| file.path != ".leptos-ui/state.json")
    {
        write_file_atomic(project_root, &file.path, file.content.as_bytes())?;
    }

    if let Some(state_file) = plan
        .files
        .iter()
        .find(|file| file.path == ".leptos-ui/state.json")
    {
        write_file_atomic(
            project_root,
            &state_file.path,
            state_file.content.as_bytes(),
        )?;
    }

    Ok(plan)
}

pub fn plan_add(project_root: &Path, item_name: &str) -> Result<AddPlan, CodegenError> {
    let item = load_built_in_registry_item(item_name)?;
    let desired_item = match item.item.name.as_str() {
        "button" => desired_builtin_button_item(),
        _ => return Err(RegistryError::BuiltInNotFound(item_name.to_owned()).into()),
    };
    let item_id = built_in_item_id(&item.item.name);
    let item_name = item.item.name.clone();
    let content_hash = item.content_hash.clone();
    let init_plan = plan_init(project_root)?;
    let mut files = init_plan
        .files
        .into_iter()
        .filter(|file| file.path != ".leptos-ui/state.json")
        .collect::<Vec<_>>();
    let mut changes = init_plan
        .changes
        .into_iter()
        .filter(|change| change.path != ".leptos-ui/state.json")
        .collect::<Vec<_>>();

    let config_content = planned_or_existing_content(&files, project_root, "components.json")?
        .unwrap_or(canonical_components_json()?);
    let config = parse_components_json_str(&config_content)?;
    let config = components_config_with_desired_item(config, desired_item)?;
    let config_content = components_config_to_json(&config)?;
    upsert_planned_file(
        project_root,
        &mut files,
        &mut changes,
        "components.json",
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
        state: sync.state,
    })
}

pub fn apply_add(project_root: &Path, item_name: &str) -> Result<AddPlan, CodegenError> {
    let plan = plan_add(project_root, item_name)?;
    apply_planned_files(project_root, &plan.files)?;

    Ok(plan)
}

pub fn plan_sync(project_root: &Path) -> Result<SyncPlan, CodegenError> {
    let init_plan = plan_init(project_root)?;
    let files = init_plan
        .files
        .into_iter()
        .filter(|file| file.path != ".leptos-ui/state.json")
        .collect::<Vec<_>>();
    let changes = init_plan
        .changes
        .into_iter()
        .filter(|change| change.path != ".leptos-ui/state.json")
        .collect::<Vec<_>>();

    let config_content = planned_or_existing_content(&files, project_root, "components.json")?
        .unwrap_or(canonical_components_json()?);
    let config = parse_components_json_str(&config_content)?;

    plan_sync_from_config(project_root, files, changes, config, config_content)
}

pub fn apply_sync(project_root: &Path) -> Result<SyncPlan, CodegenError> {
    let plan = plan_sync(project_root)?;
    apply_planned_files(project_root, &plan.files)?;

    Ok(plan)
}

fn plan_sync_from_config(
    project_root: &Path,
    mut files: Vec<PlannedFile>,
    mut changes: Vec<ChangeRecord>,
    config: ComponentsConfig,
    config_content: String,
) -> Result<SyncPlan, CodegenError> {
    let diagnostics = Vec::new();
    let config_hash = hash_bytes(config_content.as_bytes());
    let mut state = load_or_empty_state(project_root, config_hash.clone())?;
    state.project.config_hash = config_hash;
    let mut item_ids = Vec::new();
    let mut cargo_plan = Vec::new();

    for desired_item in &config.items {
        let item = load_built_in_registry_item(desired_item.item_name())?;
        let item_id =
            plan_built_in_item(project_root, &mut files, &mut changes, &mut state, &item)?;
        item_ids.push(item_id);
        merge_cargo_plan(&mut cargo_plan, &item.item.cargo_plan);
    }

    state.validate()?;
    let state_json = state_to_json(&state)?;
    upsert_planned_file(
        project_root,
        &mut files,
        &mut changes,
        ".leptos-ui/state.json",
        state_json,
        ChangeKind::WriteState,
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
        state,
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
    state: &mut InstallState,
    item: &leptos_ui_kit_registry::ResolvedRegistryItem,
) -> Result<String, CodegenError> {
    let item_id = built_in_item_id(&item.item.name);
    let safe_item_id = safe_item_id(&item_id);
    let mut installed_files = Vec::new();
    let mut installed_style_blocks = Vec::new();

    for ui_file in &item.targets.ui_files {
        let generated = read_built_in_registry_source(&ui_file.source)?;
        let logical_path = format!("src/components/ui/{}", ui_file.path);
        let baseline_path = format!(".leptos-ui/baselines/{safe_item_id}/{}", ui_file.path);
        let generated_hash = hash_bytes(generated.as_bytes());

        plan_generated_source_file(
            project_root,
            files,
            changes,
            state,
            &item_id,
            &logical_path,
            &generated,
        )?;
        upsert_planned_file(
            project_root,
            files,
            changes,
            &baseline_path,
            generated.clone(),
            ChangeKind::WriteBaseline,
            Some(&item_id),
        )?;

        installed_files.push(InstalledFile {
            path: logical_path.clone(),
            kind: "rust".to_owned(),
            baseline_path,
            baseline_hash: generated_hash.clone(),
            local_hash_at_install: generated_hash,
        });
        state.files_by_path.insert(logical_path, item_id.clone());
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
    let patched_ui_mod = patch_ui_mod(ui_mod.as_deref(), &ui_exports_for_item(&item.item.name)?)?;
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
        let css_path = "styles/app.css";
        let baseline_path = format!(".leptos-ui/baselines/{safe_item_id}/{}.css", style.id);
        let baseline = tracked_style_baseline(project_root, state, &item_id, &style.id)?;
        let existing_css = planned_or_existing_content(files, project_root, css_path)?
            .unwrap_or_else(|| ":root {\n  --luk-color-primary: #111827;\n}\n".to_owned());
        let patched_css =
            patch_css_block(&existing_css, &style.id, &generated, baseline.as_deref())?;

        upsert_planned_file(
            project_root,
            files,
            changes,
            css_path,
            patched_css,
            ChangeKind::UpdateCssBlock,
            Some(&item_id),
        )?;
        upsert_planned_file(
            project_root,
            files,
            changes,
            &baseline_path,
            generated.clone(),
            ChangeKind::WriteBaseline,
            Some(&item_id),
        )?;

        installed_style_blocks.push(InstalledStyleBlock {
            css_path: css_path.to_owned(),
            block_id: style.id.clone(),
            baseline_path,
            baseline_hash: hash_bytes(generated.as_bytes()),
        });
        state
            .style_blocks_by_id
            .insert(style.id.clone(), item_id.clone());
    }

    state.items.insert(
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

fn apply_planned_files(project_root: &Path, files: &[PlannedFile]) -> Result<(), CodegenError> {
    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    validate_planned_write_paths(&paths)?;

    if files.is_empty() {
        return Ok(());
    }

    let _lock = WriteLock::acquire(project_root)?;

    for file in files
        .iter()
        .filter(|file| file.path != ".leptos-ui/state.json")
    {
        write_file_atomic(project_root, &file.path, file.content.as_bytes())?;
    }

    if let Some(state_file) = files
        .iter()
        .find(|file| file.path == ".leptos-ui/state.json")
    {
        write_file_atomic(
            project_root,
            &state_file.path,
            state_file.content.as_bytes(),
        )?;
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
            let allowed_internal = component == ".leptos-ui" && components.peek().is_some();
            if !allowed_internal {
                return unsafe_path(path, "hidden paths are rejected except .leptos-ui");
            }
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
        let lock_path = project_root.join(".leptos-ui/lock");
        fs::create_dir_all(project_root.join(".leptos-ui")).map_err(|source| CodegenError::Io {
            path: project_root.join(".leptos-ui"),
            source,
        })?;

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

pub fn parse_install_state_str(input: &str) -> Result<InstallState, CodegenError> {
    let state: InstallState =
        serde_json::from_str(input).map_err(|source| CodegenError::StateParse {
            path: PathBuf::from(".leptos-ui/state.json"),
            source,
        })?;
    state.validate()?;
    Ok(state)
}

pub fn state_to_json(state: &InstallState) -> Result<String, CodegenError> {
    state.validate()?;
    let mut output = serde_json::to_string_pretty(state).map_err(CodegenError::StateSerialize)?;
    output.push('\n');
    Ok(output)
}

pub fn patch_css_block(
    existing: &str,
    block_id: &str,
    block: &str,
    tracked_baseline: Option<&str>,
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

            match tracked_baseline {
                Some(baseline) if current == normalize_managed_css_block(block_id, baseline)? => {
                    let mut output = String::with_capacity(
                        existing.len() + replacement.len().saturating_sub(current.len()),
                    );
                    output.push_str(&existing[..range.start]);
                    output.push_str(&replacement);
                    output.push_str(&existing[range.end..]);
                    Ok(output)
                }
                Some(_) => unsafe_patch(
                    "styles/app.css",
                    format!("managed CSS block {block_id} differs from its tracked baseline"),
                ),
                None => unsafe_patch(
                    "styles/app.css",
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
        for symbol in &export.symbols {
            validate_patch_identifier(
                symbol,
                "UI export symbol",
                Path::new("src/components/ui/mod.rs"),
            )?;
        }

        lines.push(format!("pub mod {};", export.module));
        if !export.symbols.is_empty() {
            lines.push(format!(
                "pub use {}::{{{}}};",
                export.module,
                export.symbols.join(", ")
            ));
        }
    }

    let borrowed = lines.iter().map(String::as_str).collect::<Vec<_>>();
    patch_module_lines(
        existing.unwrap_or_default(),
        "src/components/ui/mod.rs",
        &borrowed,
    )
}

fn load_or_empty_state(
    project_root: &Path,
    config_hash: String,
) -> Result<InstallState, CodegenError> {
    let path = project_root.join(".leptos-ui/state.json");
    if path.is_file() {
        let input = read_to_string(&path)?;
        let mut state = serde_json::from_str::<InstallState>(&input).map_err(|source| {
            CodegenError::StateParse {
                path: path.clone(),
                source,
            }
        })?;
        state.validate()?;
        state.project.config_hash = config_hash;
        return Ok(state);
    }

    Ok(InstallState::empty(config_hash))
}

fn plan_generated_source_file(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    state: &InstallState,
    item_id: &str,
    logical_path: &str,
    generated: &str,
) -> Result<(), CodegenError> {
    if let Some(owner) = state.files_by_path.get(logical_path) {
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
        let baseline = tracked_file_baseline(project_root, state, item_id, logical_path)?;
        if current != baseline {
            return unsafe_patch(
                logical_path,
                "tracked target has local edits that differ from its baseline",
            );
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
        return unsafe_patch(logical_path, "target exists but is not tracked in state");
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

fn tracked_file_baseline(
    project_root: &Path,
    state: &InstallState,
    item_id: &str,
    logical_path: &str,
) -> Result<String, CodegenError> {
    let path = PathBuf::from(".leptos-ui/state.json");
    let item = state
        .items
        .get(item_id)
        .ok_or_else(|| CodegenError::InvalidState {
            path: path.clone(),
            reason: format!("missing item {item_id}"),
        })?;
    let file = item
        .files
        .iter()
        .find(|file| file.path == logical_path)
        .ok_or_else(|| CodegenError::InvalidState {
            path: path.clone(),
            reason: format!("missing file state for {logical_path}"),
        })?;

    read_required_baseline(project_root, &file.baseline_path)
}

fn tracked_style_baseline(
    project_root: &Path,
    state: &InstallState,
    item_id: &str,
    block_id: &str,
) -> Result<Option<String>, CodegenError> {
    let Some(owner) = state.style_blocks_by_id.get(block_id) else {
        return Ok(None);
    };
    if owner != item_id {
        return unsafe_patch(
            "styles/app.css",
            format!("CSS block is already tracked by {owner}"),
        );
    }

    let path = PathBuf::from(".leptos-ui/state.json");
    let item = state
        .items
        .get(item_id)
        .ok_or_else(|| CodegenError::InvalidState {
            path: path.clone(),
            reason: format!("missing item {item_id}"),
        })?;
    let block = item
        .style_blocks
        .iter()
        .find(|block| block.block_id == block_id)
        .ok_or_else(|| CodegenError::InvalidState {
            path: path.clone(),
            reason: format!("missing style block state for {block_id}"),
        })?;

    Ok(Some(read_required_baseline(
        project_root,
        &block.baseline_path,
    )?))
}

fn read_required_baseline(project_root: &Path, logical_path: &str) -> Result<String, CodegenError> {
    let path = project_root.join(logical_path);
    read_optional_to_string(&path)?.ok_or_else(|| CodegenError::InvalidState {
        path,
        reason: "tracked baseline is missing".to_owned(),
    })
}

fn ui_exports_for_item(item_name: &str) -> Result<Vec<UiModuleExport>, CodegenError> {
    match item_name {
        "button" => Ok(vec![UiModuleExport::new(
            "button",
            vec![
                "Button".to_owned(),
                "ButtonSize".to_owned(),
                "ButtonType".to_owned(),
                "ButtonVariant".to_owned(),
            ],
        )]),
        _ => unsafe_patch(
            "src/components/ui/mod.rs",
            format!("no MVP export strategy is defined for {item_name}"),
        ),
    }
}

fn built_in_item_id(item_name: &str) -> String {
    format!("builtin:{item_name}")
}

fn safe_item_id(item_id: &str) -> String {
    item_id.replace(':', "-")
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

pub fn hash_content_bytes(bytes: &[u8]) -> String {
    hash_bytes(bytes)
}

fn validate_state_hash(path: &Path, field: &'static str, value: &str) -> Result<(), CodegenError> {
    if value
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        Ok(())
    } else {
        invalid_state(path, format!("{field} must be a sha256 hash"))
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

fn invalid_state<T>(path: &Path, reason: impl Into<String>) -> Result<T, CodegenError> {
    Err(CodegenError::InvalidState {
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
        "components.json" | "index.html" | "styles/app.css" | "src/components/mod.rs"
    ) || path.starts_with("src/components/ui/")
        || path.starts_with(".leptos-ui/")
}

fn plan_components_json(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
) -> Result<(), CodegenError> {
    let path = project_root.join("components.json");
    if path.is_file() {
        parse_components_json_str(&read_to_string(&path)?)?;
        return Ok(());
    }

    push_file_plan(
        files,
        changes,
        "components.json",
        PlannedFileAction::Create,
        canonical_components_json()?,
        ChangeKind::CreateFile,
    );
    Ok(())
}

fn plan_stylesheet(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
) -> Result<(), CodegenError> {
    let path = project_root.join("styles/app.css");
    if path.is_file() {
        return Ok(());
    }

    push_file_plan(
        files,
        changes,
        "styles/app.css",
        PlannedFileAction::Create,
        ":root {\n  --luk-color-primary: #111827;\n}\n".to_owned(),
        ChangeKind::CreateFile,
    );
    Ok(())
}

fn plan_index_html(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
) -> Result<(), CodegenError> {
    let path = project_root.join("index.html");
    let html = read_to_string(&path)?;
    if html.contains("data-trunk")
        && html.contains("rel=\"css\"")
        && html.contains("styles/app.css")
    {
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

    let mut patched = html;
    patched.insert_str(
        head_end,
        "    <link data-trunk rel=\"css\" href=\"styles/app.css\" />\n",
    );

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
            "styles/app.css",
            format!("managed CSS block {block_id} must contain exactly one start and end marker"),
        );
    }

    let Some(start) = block.find(&start_marker) else {
        return unsafe_patch(
            "styles/app.css",
            format!("managed CSS block {block_id} is missing its start marker"),
        );
    };
    let Some(end) = block.find(&end_marker) else {
        return unsafe_patch(
            "styles/app.css",
            format!("managed CSS block {block_id} is missing its end marker"),
        );
    };
    if start > end {
        return unsafe_patch(
            "styles/app.css",
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
                    "styles/app.css",
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
            "styles/app.css",
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
        if output
            .lines()
            .any(|existing_line| existing_line.trim() == *line)
        {
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

fn validate_css_block_id(block_id: &str) -> Result<(), CodegenError> {
    if block_id.is_empty()
        || !block_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return unsafe_patch(
            "styles/app.css",
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
    let path = project_root.join(".leptos-ui/state.json");
    if path.is_file() {
        return Ok(());
    }

    let content = empty_state_json(project_root, files)?;
    push_file_plan(
        files,
        changes,
        ".leptos-ui/state.json",
        PlannedFileAction::Create,
        content,
        ChangeKind::WriteState,
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

fn empty_state_json(project_root: &Path, files: &[PlannedFile]) -> Result<String, CodegenError> {
    let config_content = planned_or_existing_content(files, project_root, "components.json")?
        .unwrap_or(canonical_components_json()?);
    state_to_json(&InstallState::empty(hash_bytes(config_content.as_bytes())))
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
                        .with_path("components.json")
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
        assert!(plan.files.iter().any(|file| file.path == "components.json"));
        assert!(plan.files.iter().any(|file| file.path == "styles/app.css"));
        assert!(plan.files.iter().any(|file| file.path == "index.html"));
        assert!(
            plan.files
                .iter()
                .any(|file| file.path == "src/components/mod.rs")
        );
        assert!(
            plan.files
                .iter()
                .any(|file| file.path == ".leptos-ui/state.json")
        );
        assert!(!root.join("components.json").exists());
    }

    #[test]
    fn init_plan_rejects_invalid_existing_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(root.join("components.json"), "{\"tailwind\":true}\n").expect("write config");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");

        let error = plan_init(root).expect_err("invalid config should fail");

        assert!(matches!(error, CodegenError::Config(_)));
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
        assert!(root.join("components.json").is_file());
        assert!(root.join("styles/app.css").is_file());
        assert!(root.join("src/components/mod.rs").is_file());
        assert!(root.join("src/components/ui/mod.rs").is_file());
        assert!(root.join(".leptos-ui/state.json").is_file());
        assert!(!root.join(".leptos-ui/lock").exists());
        assert!(
            fs::read_to_string(root.join("index.html"))
                .expect("read index")
                .contains("styles/app.css")
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
    fn state_round_trips_deterministically() {
        let state = InstallState::empty(hash_bytes(b"components"));
        let first = state_to_json(&state).expect("serialize first");
        let parsed = parse_install_state_str(&first).expect("parse state");
        let second = state_to_json(&parsed).expect("serialize second");

        assert_eq!(first, second);
        assert!(first.contains("\"schemaVersion\": \"0.9.0-alpha\""));
        assert!(first.contains("\"configHash\": \"sha256:"));
        assert!(!first.contains("null"));
    }

    #[test]
    fn state_rejects_malformed_hash_fields() {
        let mut state = InstallState::empty("sha256:not-a-real-hash".to_owned());

        let error = state.validate().expect_err("config hash should fail");

        assert!(
            matches!(error, CodegenError::InvalidState { reason, .. } if reason.contains("project.configHash"))
        );

        state.project.config_hash = hash_bytes(b"components");
        state.items.insert(
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

        let error = state.validate().expect_err("content hash should fail");

        assert!(
            matches!(error, CodegenError::InvalidState { reason, .. } if reason.contains("items[].contentHash"))
        );
    }

    #[test]
    fn add_plan_records_exact_baseline_bytes() {
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
        let rust_baseline = plan
            .files
            .iter()
            .find(|file| file.path == ".leptos-ui/baselines/builtin-button/button.rs")
            .expect("rust baseline");
        let css_baseline = plan
            .files
            .iter()
            .find(|file| file.path == ".leptos-ui/baselines/builtin-button/button.css")
            .expect("css baseline");

        assert_eq!(rust_target.content, source);
        assert_eq!(rust_baseline.content, source);
        assert_eq!(css_baseline.content, css);
        assert_eq!(
            plan.state.files_by_path.get("src/components/ui/button.rs"),
            Some(&"builtin:button".to_owned())
        );
        assert_eq!(
            plan.state.style_blocks_by_id.get("button"),
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
        assert!(paths.contains(&"styles/app.css"));
        assert!(paths.contains(&".leptos-ui/baselines/builtin-button/button.rs"));
        assert!(paths.contains(&".leptos-ui/baselines/builtin-button/button.css"));
        assert!(paths.contains(&".leptos-ui/state.json"));
        assert_eq!(plan.cargo_plan.len(), 2);
        assert!(!root.join("src/components/ui/button.rs").exists());
        assert!(
            !root
                .join(".leptos-ui/baselines/builtin-button/button.rs")
                .exists()
        );
    }

    #[test]
    fn add_write_installs_button_state_and_baselines() {
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
            fs::read_to_string(root.join("styles/app.css"))
                .expect("read css")
                .contains("/* leptos-ui-kit:start button */")
        );
        assert!(
            root.join(".leptos-ui/baselines/builtin-button/button.rs")
                .is_file()
        );
        assert!(
            root.join(".leptos-ui/baselines/builtin-button/button.css")
                .is_file()
        );
        let state = parse_install_state_str(
            &fs::read_to_string(root.join(".leptos-ui/state.json")).expect("read state"),
        )
        .expect("parse state");
        assert!(state.items.contains_key("builtin:button"));
        let config = parse_components_json_str(
            &fs::read_to_string(root.join("components.json")).expect("read config"),
        )
        .expect("parse config");
        assert_eq!(config.items.len(), 1);
        assert_eq!(config.items[0].item_name(), "button");
        assert!(!root.join(".leptos-ui/lock").exists());
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
        assert!(paths.contains(&"styles/app.css"));
        assert!(paths.contains(&".leptos-ui/baselines/builtin-button/button.rs"));
        assert!(paths.contains(&".leptos-ui/baselines/builtin-button/button.css"));
        assert!(paths.contains(&".leptos-ui/state.json"));
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

    fn write_desired_button_config(root: &Path) {
        let config = parse_components_json_str(
            &fs::read_to_string(root.join("components.json")).expect("read config"),
        )
        .expect("parse config");
        let config = components_config_with_desired_item(config, desired_builtin_button_item())
            .expect("add desired item");
        fs::write(
            root.join("components.json"),
            components_config_to_json(&config).expect("serialize config"),
        )
        .expect("write config");
    }

    #[test]
    fn css_patcher_appends_managed_block() {
        let existing = ":root {\n  color-scheme: light;\n}\n";
        let block =
            "/* leptos-ui-kit:start button */\n.luk-button {}\n/* leptos-ui-kit:end button */\n";

        let patched = patch_css_block(existing, "button", block, None).expect("patch css");

        assert!(patched.starts_with(existing));
        assert!(patched.contains("/* leptos-ui-kit:start button */"));
        assert!(patched.contains(".luk-button {}"));
        assert!(patched.ends_with("/* leptos-ui-kit:end button */\n"));
    }

    #[test]
    fn css_patcher_is_idempotent_for_existing_matching_block() {
        let block =
            "/* leptos-ui-kit:start button */\n.luk-button {}\n/* leptos-ui-kit:end button */\n";

        let patched = patch_css_block(block, "button", block, None).expect("patch css");

        assert_eq!(patched, block);
    }

    #[test]
    fn css_patcher_replaces_tracked_baseline() {
        let baseline = "/* leptos-ui-kit:start button */\n.luk-button { color: red; }\n/* leptos-ui-kit:end button */\n";
        let next = "/* leptos-ui-kit:start button */\n.luk-button { color: blue; }\n/* leptos-ui-kit:end button */\n";
        let existing = format!("/* app */\n{baseline}.other {{}}\n");

        let patched =
            patch_css_block(&existing, "button", next, Some(baseline)).expect("patch css");

        assert!(patched.contains("color: blue"));
        assert!(!patched.contains("color: red"));
        assert!(patched.contains(".other {}"));
    }

    #[test]
    fn css_block_extractor_requires_exact_managed_markers() {
        let block =
            "/* leptos-ui-kit:start button */\n.luk-button {}\n/* leptos-ui-kit:end button */\n";
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
        let baseline = "/* leptos-ui-kit:start button */\n.luk-button { color: red; }\n/* leptos-ui-kit:end button */\n";
        let edited = "/* leptos-ui-kit:start button */\n.luk-button { color: green; }\n/* leptos-ui-kit:end button */\n";
        let next = "/* leptos-ui-kit:start button */\n.luk-button { color: blue; }\n/* leptos-ui-kit:end button */\n";

        let error =
            patch_css_block(edited, "button", next, Some(baseline)).expect_err("should conflict");

        assert!(matches!(error, CodegenError::UnsafePatch { .. }));
    }

    #[test]
    fn module_patchers_insert_required_exports() {
        let components = patch_components_mod(Some("// existing\n")).expect("patch components");
        let ui = patch_ui_mod(
            Some("// generated exports\n"),
            &[UiModuleExport::new(
                "button",
                vec![
                    "Button".to_owned(),
                    "ButtonSize".to_owned(),
                    "ButtonType".to_owned(),
                    "ButtonVariant".to_owned(),
                ],
            )],
        )
        .expect("patch ui mod");

        assert_eq!(components, "// existing\npub mod ui;\n");
        assert_eq!(
            ui,
            "// generated exports\npub mod button;\npub use button::{Button, ButtonSize, ButtonType, ButtonVariant};\n"
        );
        assert_eq!(
            patch_ui_mod(
                Some(&ui),
                &[UiModuleExport::new(
                    "button",
                    vec![
                        "Button".to_owned(),
                        "ButtonSize".to_owned(),
                        "ButtonType".to_owned(),
                        "ButtonVariant".to_owned(),
                    ],
                )],
            )
            .expect("idempotent enough"),
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
            "components.json".to_owned(),
            "index.html".to_owned(),
            "styles/app.css".to_owned(),
            "src/components/mod.rs".to_owned(),
            "src/components/ui/button.rs".to_owned(),
            ".leptos-ui/state.json".to_owned(),
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
            write_file_atomic(root, "styles/app.css", b":root {}\n").expect("write css");
            assert!(root.join(".leptos-ui/lock").exists());
        }

        assert_eq!(
            fs::read_to_string(root.join("styles/app.css")).expect("read css"),
            ":root {}\n"
        );
        assert!(!root.join(".leptos-ui/lock").exists());
    }
}
