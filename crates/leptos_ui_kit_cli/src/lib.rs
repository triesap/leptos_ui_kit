#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fmt, fs, io,
    path::{Path, PathBuf},
    process::{self, Command},
};

use leptos_ui_kit_codegen::{
    AddPlan, ChangeRecord, CodegenError, CommandStatus, DEFAULT_KIT_LOCK_PATH, Diagnostic,
    DiagnosticLevel, HtmlStylesheetState, InitPlan, InstallLock, InstalledFile, InstalledItem,
    InstalledStyleBlock, SyncPlan, apply_add, apply_init, apply_sync, check_pending_recovery,
    hash_content_bytes, inspect_html_stylesheet, inspect_managed_css_blocks_at_path,
    install_lock_path, parse_install_lock_str_at_path, plan_add, plan_init, plan_sync,
};
use leptos_ui_kit_registry::{
    CargoPlanEntry, ConfigError, DEFAULT_CSS_PATH, DEFAULT_KIT_CONFIG_PATH, DEFAULT_UI_DIR,
    DependencyRequirement, DependencyStatus, DetectionError, InfoOutput, KitConfig, ProjectKind,
    RegistryError, RenderModeContract, RenderModeSelection, ResolvedRegistryItem, SCHEMA_VERSION,
    TOOL_BINARY, TOOL_GIT_URL, TOOL_PACKAGE, ToolConfig, ToolSourceConfig, build_info_output,
    canonical_tool_config, detect_cargo_plan_requirements, kit_config_to_json, load_registry_item,
    normalize_cargo_plan_for_project, read_built_in_registry_source,
    resolve_built_in_registry_items, validate_built_in_registry_health,
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliEnvelope<T>
where
    T: Serialize,
{
    schema_version: &'static str,
    command: String,
    status: &'static str,
    diagnostics: Vec<CliDiagnosticOutput>,
    changes: Vec<CliChangeOutput>,
    data: T,
}

impl<T> CliEnvelope<T>
where
    T: Serialize,
{
    fn new(command: impl Into<String>, status: CommandStatus, data: T) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.into(),
            status: command_status_name(status),
            diagnostics: Vec::new(),
            changes: Vec::new(),
            data,
        }
    }

    fn success(command: impl Into<String>, data: T) -> Self {
        Self::new(command, CommandStatus::Success, data)
    }

    fn with_diagnostics(mut self, diagnostics: &[Diagnostic]) -> Self {
        self.diagnostics = diagnostics.iter().map(CliDiagnosticOutput::from).collect();
        self
    }

    fn with_changes(mut self, changes: &[ChangeRecord]) -> Self {
        self.changes = changes.iter().map(CliChangeOutput::from).collect();
        self
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliDiagnosticOutput {
    level: &'static str,
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggestion: Option<String>,
}

impl From<&Diagnostic> for CliDiagnosticOutput {
    fn from(diagnostic: &Diagnostic) -> Self {
        Self {
            level: match diagnostic.level {
                DiagnosticLevel::Info => "info",
                DiagnosticLevel::Warning => "warning",
                DiagnosticLevel::Error => "error",
            },
            code: diagnostic.code.clone(),
            message: diagnostic.message.clone(),
            path: diagnostic.path.clone(),
            suggestion: diagnostic.suggestion.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliChangeOutput {
    kind: &'static str,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    item: Option<String>,
    tracked: bool,
}

impl From<&ChangeRecord> for CliChangeOutput {
    fn from(change: &ChangeRecord) -> Self {
        Self {
            kind: match change.kind {
                leptos_ui_kit_codegen::ChangeKind::CreateFile => "create_file",
                leptos_ui_kit_codegen::ChangeKind::UpdateFile => "update_file",
                leptos_ui_kit_codegen::ChangeKind::DeleteFile => "delete_file",
                leptos_ui_kit_codegen::ChangeKind::CreateDir => "create_dir",
                leptos_ui_kit_codegen::ChangeKind::UpdateCssBlock => "update_css_block",
                leptos_ui_kit_codegen::ChangeKind::WriteLockFile => "write_lock_file",
            },
            path: change.path.clone(),
            item: change.item.clone(),
            tracked: change.tracked,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CargoRequirementOutput {
    #[serde(rename = "crate")]
    crate_name: String,
    source: CargoSourceOutput,
    features: Vec<String>,
    required: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CargoSourceOutput {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rev: Option<String>,
}

impl From<&CargoPlanEntry> for CargoRequirementOutput {
    fn from(entry: &CargoPlanEntry) -> Self {
        Self {
            crate_name: entry.crate_name.clone(),
            source: CargoSourceOutput {
                kind: match entry.source.kind {
                    leptos_ui_kit_registry::CargoPlanSourceKind::Version => "version",
                    leptos_ui_kit_registry::CargoPlanSourceKind::Git => "git",
                },
                version: entry.source.version.clone(),
                url: entry.source.url.clone(),
                rev: entry.source.rev.clone(),
            },
            features: entry.features.clone(),
            required: entry.required,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AddCommandOutput {
    item_id: String,
    item_name: String,
    content_hash: String,
    dependencies: Vec<CargoRequirementOutput>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncCommandOutput {
    item_ids: Vec<String>,
    dependencies: Vec<CargoRequirementOutput>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InitCommandOutput {}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HelpCommandOutput {
    usage: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InfoCommandOutput {
    project_root: &'static str,
    project_kind: &'static str,
    workspace_mode: &'static str,
    cargo_manifest: String,
    source_root: String,
    index_html: String,
    stylesheet: String,
    render_mode_contract: RenderModeContract,
    render_mode_selection: RenderModeSelection,
    render_mode: Option<&'static str>,
    config_path: Option<String>,
    registry_available: bool,
    installed: Option<InstalledSummaryOutput>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstalledSummaryOutput {
    lock_path: String,
    item_ids: Vec<String>,
    file_paths: Vec<String>,
    style_block_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryItemSourceOutput {
    resolved: RegistryItemOutput,
    sources: Vec<RegistrySourceContent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryItemOutput {
    source_kind: &'static str,
    source_path: String,
    content_hash: String,
    name: String,
    kind: String,
    version: String,
    title: String,
    description: String,
    registry_dependencies: Vec<String>,
    targets: RegistryTargetsOutput,
    cargo_plan: Vec<CargoRequirementOutput>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryTargetsOutput {
    ui_files: Vec<RegistryUiTargetOutput>,
    style_blocks: Vec<RegistryStyleTargetOutput>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryUiTargetOutput {
    source: String,
    path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryStyleTargetOutput {
    source: String,
    id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistrySourceContent {
    path: String,
    kind: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionCommandOutput {
    package: &'static str,
    binary: &'static str,
    version: &'static str,
    schema_version: &'static str,
    source: VersionSourceOutput,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionSourceOutput {
    kind: &'static str,
    url: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    rev: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCommandOutput {
    project_root: &'static str,
    strict: bool,
    check: bool,
    trunk_build: bool,
    checks: Vec<DoctorCheckOutput>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCheckOutput {
    name: String,
    status: DoctorCheckStatus,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorOutput {
    project_root: PathBuf,
    strict: bool,
    check: bool,
    trunk_build: bool,
    checks: Vec<DoctorCheck>,
}

impl DoctorOutput {
    fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DoctorCheckStatus::Fail)
    }

    fn has_warnings(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DoctorCheckStatus::Warning)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCheck {
    name: String,
    status: DoctorCheckStatus,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

impl DoctorCheck {
    fn pass(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorCheckStatus::Pass,
            message: message.into(),
            path: None,
        }
    }

    fn warning(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorCheckStatus::Warning,
            message: message.into(),
            path: None,
        }
    }

    fn fail(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorCheckStatus::Fail,
            message: message.into(),
            path: None,
        }
    }

    fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DoctorCheckStatus {
    Pass,
    Warning,
    Fail,
}

fn command_status_name(status: CommandStatus) -> &'static str {
    match status {
        CommandStatus::Success => "success",
        CommandStatus::NoChange => "no_change",
        CommandStatus::Planned => "planned",
        CommandStatus::Warning => "warning",
        CommandStatus::Error => "error",
        CommandStatus::Conflict => "conflict",
        CommandStatus::Unsupported => "unsupported",
    }
}

#[derive(Debug, Clone)]
struct DoctorRegistrySnapshot {
    requested_names: BTreeSet<String>,
    resolved_names: BTreeSet<String>,
    resolved_order: Vec<String>,
    expected_items: BTreeMap<String, InstalledItem>,
    files_by_path: BTreeMap<String, String>,
    style_blocks_by_id: BTreeMap<String, String>,
    css_path: Option<String>,
    cargo_plan: Vec<CargoPlanEntry>,
    style_dependencies: BTreeSet<(String, String)>,
}

#[derive(Debug)]
enum DoctorLockState {
    Missing {
        logical_path: String,
        path: PathBuf,
    },
    Invalid {
        logical_path: String,
        path: PathBuf,
        message: String,
    },
    Valid {
        logical_path: String,
        path: PathBuf,
        lock: Box<InstallLock>,
    },
}

impl DoctorLockState {
    fn logical_path(&self) -> &str {
        match self {
            Self::Missing { logical_path, .. }
            | Self::Invalid { logical_path, .. }
            | Self::Valid { logical_path, .. } => logical_path,
        }
    }

    fn path(&self) -> &Path {
        match self {
            Self::Missing { path, .. } | Self::Invalid { path, .. } | Self::Valid { path, .. } => {
                path
            }
        }
    }

    fn lock(&self) -> Option<&InstallLock> {
        match self {
            Self::Valid { lock, .. } => Some(lock.as_ref()),
            Self::Missing { .. } | Self::Invalid { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitClass {
    Operational = 1,
    Usage = 2,
    DoctorFailed = 3,
    Conflict = 10,
    UnsafePath = 11,
    RegistryPackage = 12,
}

impl ExitClass {
    const fn code(self) -> i32 {
        self as i32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorCategory {
    Operational,
    Usage,
    Doctor,
    Conflict,
    UnsafePath,
    RegistryPackage,
}

#[derive(Debug)]
struct CliError {
    command: String,
    status: CommandStatus,
    category: ErrorCategory,
    code: &'static str,
    message: Box<str>,
    logical_path: Option<String>,
    suggestion: Option<&'static str>,
    source: Option<Box<dyn std::error::Error>>,
    exit_class: ExitClass,
    json: bool,
    output_emitted: bool,
}

impl CliError {
    fn usage(
        command: impl Into<String>,
        json: bool,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            command: command.into(),
            status: CommandStatus::Unsupported,
            category: ErrorCategory::Usage,
            code,
            message: message.into().into_boxed_str(),
            logical_path: None,
            suggestion: Some("Run the command with --help to inspect supported arguments."),
            source: None,
            exit_class: ExitClass::Usage,
            json,
            output_emitted: false,
        }
    }

    fn operational(
        command: impl Into<String>,
        json: bool,
        code: &'static str,
        message: impl Into<String>,
        logical_path: Option<String>,
        source: Option<Box<dyn std::error::Error>>,
    ) -> Self {
        Self {
            command: command.into(),
            status: CommandStatus::Error,
            category: ErrorCategory::Operational,
            code,
            message: message.into().into_boxed_str(),
            logical_path,
            suggestion: Some("Resolve the reported project or filesystem error and retry."),
            source,
            exit_class: ExitClass::Operational,
            json,
            output_emitted: false,
        }
    }

    fn doctor_failed(json: bool) -> Self {
        Self {
            command: "doctor".to_owned(),
            status: CommandStatus::Error,
            category: ErrorCategory::Doctor,
            code: "doctor.checks_failed",
            message: "doctor checks failed".into(),
            logical_path: None,
            suggestion: Some("Resolve the failing doctor checks and retry."),
            source: None,
            exit_class: ExitClass::DoctorFailed,
            json,
            output_emitted: true,
        }
    }

    fn from_codegen(
        command: &'static str,
        json: bool,
        project_root: &Path,
        action: &str,
        error: CodegenError,
    ) -> Self {
        let (category, status, code, exit_class, logical_path, suggestion) = match &error {
            CodegenError::Registry(_) => (
                ErrorCategory::RegistryPackage,
                CommandStatus::Error,
                "registry.package",
                ExitClass::RegistryPackage,
                None,
                "Verify the built-in registry package and requested item, then retry.",
            ),
            CodegenError::UnsafePath { path, .. } => (
                ErrorCategory::UnsafePath,
                CommandStatus::Error,
                "path.unsafe",
                ExitClass::UnsafePath,
                Some(path.clone()),
                "Use a normalized project-relative path without traversal or symlink escapes.",
            ),
            CodegenError::Config(
                ConfigError::PathMustBeRelative { .. }
                | ConfigError::PathTraversal { .. }
                | ConfigError::UnsafePathSegment { .. }
                | ConfigError::PathOverlap { .. },
            ) => (
                ErrorCategory::UnsafePath,
                CommandStatus::Error,
                "config.unsafe_path",
                ExitClass::UnsafePath,
                Some(DEFAULT_KIT_CONFIG_PATH.to_owned()),
                "Use a normalized project-relative configuration path without overlap.",
            ),
            CodegenError::UnsafePatch { path, .. } => (
                ErrorCategory::Conflict,
                CommandStatus::Conflict,
                "project.patch_conflict",
                ExitClass::Conflict,
                logical_path(project_root, path),
                "Resolve the conflicting app-owned content and retry.",
            ),
            CodegenError::PreimageConflict { path, .. } => (
                ErrorCategory::Conflict,
                CommandStatus::Conflict,
                "project.preimage_conflict",
                ExitClass::Conflict,
                Some(path.clone()),
                "Re-plan after resolving the concurrent project change.",
            ),
            CodegenError::ProjectRootChanged { .. } => (
                ErrorCategory::Conflict,
                CommandStatus::Conflict,
                "project.root_changed",
                ExitClass::Conflict,
                None,
                "Retry from the unchanged project root.",
            ),
            CodegenError::WriteLockContended { path } => (
                ErrorCategory::Conflict,
                CommandStatus::Conflict,
                "transaction.lock_contended",
                ExitClass::Conflict,
                Some(path.clone()),
                "Wait for the active writer to finish and retry.",
            ),
            CodegenError::LegacyWriteLock { path } => (
                ErrorCategory::Conflict,
                CommandStatus::Conflict,
                "transaction.legacy_lock",
                ExitClass::Conflict,
                Some(path.clone()),
                "Verify no older writer is running, remove the legacy lock, and retry.",
            ),
            CodegenError::InvalidCoordinationState { path, .. } => (
                ErrorCategory::Conflict,
                CommandStatus::Conflict,
                "transaction.invalid_coordination_state",
                ExitClass::Conflict,
                Some(path.clone()),
                "Verify no writer is running, repair the coordination state, and retry.",
            ),
            CodegenError::RecoveryRequired { journal_path, .. } => (
                ErrorCategory::Conflict,
                CommandStatus::Conflict,
                "transaction.recovery_required",
                ExitClass::Conflict,
                logical_path(project_root, journal_path),
                "Run the command again after making the project available for recovery.",
            ),
            CodegenError::LockExists(path) => (
                ErrorCategory::Conflict,
                CommandStatus::Conflict,
                "transaction.lock_exists",
                ExitClass::Conflict,
                logical_path(project_root, path),
                "Wait for the active writer to finish and retry.",
            ),
            CodegenError::FilesystemOperation {
                logical_path: path, ..
            } => (
                ErrorCategory::Operational,
                CommandStatus::Error,
                "filesystem.operation",
                ExitClass::Operational,
                Some(path.clone()),
                "Resolve the reported project or filesystem error and retry.",
            ),
            CodegenError::Io { path, .. }
            | CodegenError::LockParse { path, .. }
            | CodegenError::InvalidLock { path, .. } => (
                ErrorCategory::Operational,
                CommandStatus::Error,
                "project.io",
                ExitClass::Operational,
                logical_path(project_root, path),
                "Resolve the reported project or filesystem error and retry.",
            ),
            CodegenError::DuplicatePath(path) => (
                ErrorCategory::Operational,
                CommandStatus::Error,
                "plan.duplicate_path",
                ExitClass::Operational,
                Some(path.clone()),
                "Report the duplicate generated path and retry with a corrected package.",
            ),
            CodegenError::Config(_) | CodegenError::LockSerialize(_) => (
                ErrorCategory::Operational,
                CommandStatus::Error,
                "project.operation",
                ExitClass::Operational,
                None,
                "Resolve the reported project or filesystem error and retry.",
            ),
        };
        let error_message = match &error {
            CodegenError::Registry(error) => registry_error_message(error),
            CodegenError::Config(error) => config_error_message(error),
            _ => error.to_string(),
        };

        Self {
            command: command.to_owned(),
            status,
            category,
            code,
            message: redact_project_root(&format!("{action}: {error_message}"), project_root)
                .into_boxed_str(),
            logical_path,
            suggestion: Some(suggestion),
            source: Some(Box::new(error)),
            exit_class,
            json,
            output_emitted: false,
        }
    }

    fn from_detection(
        command: &'static str,
        json: bool,
        project_root: &Path,
        error: DetectionError,
    ) -> Self {
        let (category, code, exit_class, logical_path, suggestion) = match &error {
            DetectionError::Registry(_) => (
                ErrorCategory::RegistryPackage,
                "registry.package",
                ExitClass::RegistryPackage,
                None,
                "Verify the built-in registry package and retry.",
            ),
            DetectionError::Config(
                ConfigError::PathMustBeRelative { .. }
                | ConfigError::PathTraversal { .. }
                | ConfigError::UnsafePathSegment { .. }
                | ConfigError::PathOverlap { .. },
            ) => (
                ErrorCategory::UnsafePath,
                "config.unsafe_path",
                ExitClass::UnsafePath,
                Some(DEFAULT_KIT_CONFIG_PATH.to_owned()),
                "Use a normalized project-relative configuration path without overlap.",
            ),
            DetectionError::MissingCargoManifest(path) => (
                ErrorCategory::Operational,
                "project.missing_manifest",
                ExitClass::Operational,
                logical_path(project_root, path).or_else(|| Some("Cargo.toml".to_owned())),
                "Run the command from a supported project root.",
            ),
            DetectionError::MissingIndexHtml(path) => (
                ErrorCategory::Operational,
                "project.missing_index",
                ExitClass::Operational,
                logical_path(project_root, path).or_else(|| Some("index.html".to_owned())),
                "Add the required Trunk index.html and retry.",
            ),
            DetectionError::MissingSourceRoot(path) => (
                ErrorCategory::Operational,
                "project.missing_source_root",
                ExitClass::Operational,
                logical_path(project_root, path),
                "Add the configured source root and retry.",
            ),
            DetectionError::Io { path, .. } => (
                ErrorCategory::Operational,
                "project.io",
                ExitClass::Operational,
                logical_path(project_root, path),
                "Resolve the reported project or filesystem error and retry.",
            ),
            DetectionError::CargoTomlParse(_) => (
                ErrorCategory::Operational,
                "project.invalid_manifest",
                ExitClass::Operational,
                Some("Cargo.toml".to_owned()),
                "Correct Cargo.toml and retry.",
            ),
            DetectionError::UnsupportedProject(_) => (
                ErrorCategory::Operational,
                "project.unsupported",
                ExitClass::Operational,
                Some("Cargo.toml".to_owned()),
                "Run the command from a supported single-package Trunk CSR project.",
            ),
            DetectionError::Config(_) => (
                ErrorCategory::Operational,
                "config.invalid",
                ExitClass::Operational,
                Some(DEFAULT_KIT_CONFIG_PATH.to_owned()),
                "Correct the project configuration and retry.",
            ),
        };

        let error_message = match &error {
            DetectionError::Config(error) => config_error_message(error),
            _ => error.to_string(),
        };
        Self {
            command: command.to_owned(),
            status: CommandStatus::Error,
            category,
            code,
            message: redact_project_root(
                &format!("failed to inspect project: {error_message}"),
                project_root,
            )
            .into_boxed_str(),
            logical_path,
            suggestion: Some(suggestion),
            source: Some(Box::new(error)),
            exit_class,
            json,
            output_emitted: false,
        }
    }

    fn from_registry(
        command: &'static str,
        json: bool,
        selector: &str,
        error: RegistryError,
    ) -> Self {
        let logical_path = match &error {
            RegistryError::UnsafePath { path, .. } | RegistryError::DuplicateTarget(path) => {
                safe_logical_locator(path)
            }
            RegistryError::MissingSource(path)
            | RegistryError::Io { path, .. }
            | RegistryError::Parse { path, .. } => path
                .to_str()
                .filter(|path| !Path::new(path).is_absolute())
                .map(str::to_owned),
            RegistryError::BuiltInNotFound(_)
            | RegistryError::LocalRegistryUnsupported(_)
            | RegistryError::InvalidValue { .. }
            | RegistryError::UnknownDependency { .. }
            | RegistryError::DependencyCycle(_)
            | RegistryError::Serialize(_)
            | RegistryError::BuiltInAsset(_) => None,
        };

        Self {
            command: command.to_owned(),
            status: CommandStatus::Error,
            category: ErrorCategory::RegistryPackage,
            code: "registry.load_failed",
            message: format!(
                "failed to load registry item {selector}: {}",
                registry_error_message(&error)
            )
            .into_boxed_str(),
            logical_path,
            suggestion: Some(
                "Verify the built-in registry package and requested item, then retry.",
            ),
            source: Some(Box::new(error)),
            exit_class: ExitClass::RegistryPackage,
            json,
            output_emitted: false,
        }
    }

    fn serialization(command: impl Into<String>, json: bool, message: String) -> Self {
        Self::operational(command, json, "output.serialize", message, None, None)
    }

    fn exit_code(&self) -> i32 {
        self.exit_class.code()
    }

    fn diagnostic(&self) -> Diagnostic {
        let level = match self.category {
            ErrorCategory::Operational
            | ErrorCategory::Usage
            | ErrorCategory::Doctor
            | ErrorCategory::Conflict
            | ErrorCategory::UnsafePath
            | ErrorCategory::RegistryPackage => DiagnosticLevel::Error,
        };
        let mut diagnostic = Diagnostic::new(level, self.code, self.message.to_string());
        if let Some(path) = &self.logical_path {
            diagnostic = diagnostic.with_path(path);
        }
        if let Some(suggestion) = self.suggestion {
            diagnostic = diagnostic.with_suggestion(suggestion);
        }
        diagnostic
    }

    fn render_json(&self) -> String {
        let diagnostics = [self.diagnostic()];
        serde_json::to_string_pretty(
            &CliEnvelope::new(&self.command, self.status, Option::<()>::None)
                .with_diagnostics(&diagnostics),
        )
        .expect("serializing the fixed CLI error envelope cannot fail")
    }

    fn report(&self) {
        if self.output_emitted {
            return;
        }
        if self.json {
            println!("{}", self.render_json());
        } else {
            eprintln!("{self}");
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref()
    }
}

fn logical_path(project_root: &Path, path: &Path) -> Option<String> {
    if path.is_absolute() {
        path.strip_prefix(project_root)
            .ok()
            .and_then(Path::to_str)
            .and_then(safe_logical_locator)
    } else {
        path.to_str().and_then(safe_logical_locator)
    }
}

fn registry_error_message(error: &RegistryError) -> String {
    match error {
        RegistryError::BuiltInNotFound(name) => {
            format!("built-in registry item not found: {name}")
        }
        RegistryError::LocalRegistryUnsupported(source) => {
            format!("local registry sources are not supported: {source}")
        }
        RegistryError::InvalidValue { .. } => "a packaged registry value is invalid".to_owned(),
        RegistryError::UnknownDependency { .. } => {
            "a packaged registry dependency is unknown".to_owned()
        }
        RegistryError::DependencyCycle(_) => {
            "the packaged registry dependency graph contains a cycle".to_owned()
        }
        RegistryError::UnsafePath { .. } => "a packaged registry path is unsafe".to_owned(),
        RegistryError::DuplicateTarget(_) => "a packaged registry target is duplicated".to_owned(),
        RegistryError::Io { .. } => "failed to read a packaged registry asset".to_owned(),
        RegistryError::Parse { .. } => "failed to parse a packaged registry asset".to_owned(),
        RegistryError::MissingSource(_) => "a packaged registry source is missing".to_owned(),
        RegistryError::Serialize(_) => "failed to serialize registry metadata".to_owned(),
        RegistryError::BuiltInAsset(_) => "the packaged registry catalog is invalid".to_owned(),
    }
}

fn config_error_message(error: &ConfigError) -> String {
    match error {
        ConfigError::PathMustBeRelative { .. }
        | ConfigError::PathTraversal { .. }
        | ConfigError::UnsafePathSegment { .. }
        | ConfigError::PathOverlap { .. } => "kit.json contains an unsafe project path".to_owned(),
        ConfigError::Parse(_) => "kit.json is malformed".to_owned(),
        ConfigError::Serialize(_) => "kit.json could not be serialized".to_owned(),
        ConfigError::InvalidValue { field, .. } => {
            format!("kit.json contains an invalid value for {field}")
        }
        ConfigError::MissingToolProvenance { .. } => error.to_string(),
    }
}

pub fn main_entry() {
    let args = normalize_args(env::args_os().skip(1).collect());
    if let Err(error) = run_from_environment(args, env::current_dir) {
        error.report();
        process::exit(error.exit_code());
    }
}

fn run_from_environment(
    args: Vec<OsString>,
    current_dir: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<(), CliError> {
    let command = command_hint(&args);
    let json = json_requested(&args);
    let cwd = if is_directory_independent_invocation(&args) {
        PathBuf::new()
    } else {
        current_dir().map_err(|error| {
            CliError::operational(
                command,
                json,
                "cwd.unavailable",
                format!("failed to acquire current directory: {error}"),
                None,
                Some(Box::new(error)),
            )
        })?
    };
    run(args, &cwd)
}

fn is_directory_independent_invocation(args: &[OsString]) -> bool {
    if args.iter().any(is_help_arg) {
        return true;
    }

    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--cwd") => index += 2,
            Some("--quiet" | "--verbose") => index += 1,
            Some("--version" | "-V") => return true,
            _ => return false,
        }
    }
    false
}

fn normalize_args(mut args: Vec<OsString>) -> Vec<OsString> {
    if args
        .first()
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| arg == "leptos_ui_kit")
    {
        args.remove(0);
    }

    args
}

fn json_requested(args: &[OsString]) -> bool {
    args.iter()
        .any(|arg| arg.to_str().is_some_and(|arg| arg == "--json"))
}

fn command_hint(args: &[OsString]) -> String {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--cwd") => {
                iter.next();
            }
            Some("--quiet" | "--verbose" | "--json") => {}
            Some("--version" | "-V") => return "version".to_owned(),
            Some("--help" | "-h") => return "help".to_owned(),
            Some(command) if !command.starts_with('-') => return command.to_owned(),
            Some(_) | None => {}
        }
    }
    "cli".to_owned()
}

fn run(args: Vec<OsString>, cwd: &Path) -> Result<(), CliError> {
    let command_hint = command_hint(&args);
    let json = json_requested(&args);
    let (args, cwd, _quiet, _verbose) = parse_common_args(args, cwd, &command_hint, json)?;
    let Some(command) = args.first().and_then(|value| value.to_str()) else {
        return Err(CliError::usage(command_hint, json, "cli.usage", usage()));
    };

    if command == "--help" || command == "-h" {
        println!(
            "{}",
            render_help_output("help", &help_text(), json)
                .map_err(|message| CliError::serialization("help", json, message))?
        );
        return Ok(());
    }
    if command == "--version" || command == "-V" {
        return run_version(&args[1..]);
    }
    if args[1..].iter().any(is_help_arg) {
        let help = command_help(command)
            .map_err(|message| CliError::usage(command, json, "cli.unknown_command", message))?;
        println!(
            "{}",
            render_help_output(command, &help, json)
                .map_err(|message| CliError::serialization(command, json, message))?
        );
        return Ok(());
    }

    match command {
        "add" => run_add(&args[1..], &cwd),
        "doctor" => run_doctor(&args[1..], &cwd),
        "info" => run_info(&args[1..], &cwd),
        "init" => run_init(&args[1..], &cwd),
        "sync" => run_sync(&args[1..], &cwd),
        "view" => run_view(&args[1..], &cwd),
        _ => Err(CliError::usage(
            command,
            json,
            "cli.unknown_command",
            usage(),
        )),
    }
}

fn is_help_arg(arg: &OsString) -> bool {
    arg.to_str()
        .is_some_and(|value| value == "--help" || value == "-h")
}

fn parse_common_args(
    args: Vec<OsString>,
    cwd: &Path,
    command: &str,
    json: bool,
) -> Result<(Vec<OsString>, PathBuf, bool, bool), CliError> {
    let mut filtered = Vec::new();
    let mut target_cwd = cwd.to_path_buf();
    let mut quiet = false;
    let mut verbose = false;
    let mut json_flag = false;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--cwd") => {
                let Some(path) = iter.next() else {
                    return Err(CliError::usage(
                        command,
                        json,
                        "cli.missing_argument",
                        "--cwd requires a path",
                    ));
                };
                let path = PathBuf::from(path);
                target_cwd = if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                };
            }
            Some("--quiet") => quiet = true,
            Some("--verbose") => verbose = true,
            Some("--json") => json_flag = true,
            _ => filtered.push(arg),
        }
    }
    if json_flag && !filtered.is_empty() {
        filtered.push(OsString::from("--json"));
    }

    Ok((filtered, target_cwd, quiet, verbose))
}

fn run_version(args: &[OsString]) -> Result<(), CliError> {
    let json = json_requested(args);

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err(CliError::usage(
                "version",
                json,
                "cli.non_utf8_argument",
                "non-utf8 arguments are not supported",
            ));
        };

        match value {
            "--json" => {}
            value if value.starts_with('-') => {
                return Err(CliError::usage(
                    "version",
                    json,
                    "cli.unsupported_flag",
                    format!("unsupported flag for version: {value}"),
                ));
            }
            _ => {
                return Err(CliError::usage(
                    "version",
                    json,
                    "cli.unexpected_argument",
                    "version does not accept positional arguments",
                ));
            }
        }
    }

    println!(
        "{}",
        render_version_output(json)
            .map_err(|message| CliError::serialization("version", json, message))?
    );
    Ok(())
}

fn run_add(args: &[OsString], cwd: &Path) -> Result<(), CliError> {
    let json = json_requested(args);
    let mut dry_run = false;
    let mut item: Option<String> = None;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err(CliError::usage(
                "add",
                json,
                "cli.non_utf8_argument",
                "non-utf8 arguments are not supported",
            ));
        };

        match value {
            "--json" => {}
            "--dry-run" => dry_run = true,
            value if value.starts_with('-') => {
                return Err(CliError::usage(
                    "add",
                    json,
                    "cli.unsupported_flag",
                    format!("unsupported flag for add: {value}"),
                ));
            }
            value => {
                if item.is_some() {
                    return Err(CliError::usage(
                        "add",
                        json,
                        "cli.unexpected_argument",
                        "add accepts exactly one item name",
                    ));
                }

                item = Some(value.to_owned());
            }
        }
    }

    let item = item.ok_or_else(|| {
        CliError::usage(
            "add",
            json,
            "cli.missing_argument",
            "add requires an item name",
        )
    })?;
    let plan = if dry_run {
        plan_add(cwd, &item).map_err(|error| {
            CliError::from_codegen(
                "add",
                json,
                cwd,
                &format!("failed to plan add {item}"),
                error,
            )
        })?
    } else {
        apply_add(cwd, &item).map_err(|error| {
            CliError::from_codegen("add", json, cwd, &format!("failed to add {item}"), error)
        })?
    };
    let status = if dry_run {
        CommandStatus::Planned
    } else if plan.is_empty() {
        CommandStatus::NoChange
    } else {
        CommandStatus::Success
    };

    println!(
        "{}",
        render_add_plan(&plan, json, status)
            .map_err(|message| CliError::serialization("add", json, message))?
    );

    Ok(())
}

fn run_doctor(args: &[OsString], cwd: &Path) -> Result<(), CliError> {
    let json = json_requested(args);
    let mut strict = false;
    let mut check = false;
    let mut trunk_build = false;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err(CliError::usage(
                "doctor",
                json,
                "cli.non_utf8_argument",
                "non-utf8 arguments are not supported",
            ));
        };

        match value {
            "--json" => {}
            "--strict" => strict = true,
            "--check" => check = true,
            "--trunk-build" => trunk_build = true,
            value if value.starts_with('-') => {
                return Err(CliError::usage(
                    "doctor",
                    json,
                    "cli.unsupported_flag",
                    format!("unsupported flag for doctor: {value}"),
                ));
            }
            _ => {
                return Err(CliError::usage(
                    "doctor",
                    json,
                    "cli.unexpected_argument",
                    "doctor does not accept positional arguments",
                ));
            }
        }
    }

    let output = build_doctor_output(cwd, strict, check, trunk_build);
    let status = doctor_status(&output);
    println!(
        "{}",
        render_doctor_output(&output, json, status)
            .map_err(|message| CliError::serialization("doctor", json, message))?
    );
    if output.has_failures() {
        return Err(CliError::doctor_failed(json));
    }

    Ok(())
}

fn run_info(args: &[OsString], cwd: &Path) -> Result<(), CliError> {
    let json = json_requested(args);
    let mut path: Option<PathBuf> = None;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err(CliError::usage(
                "info",
                json,
                "cli.non_utf8_argument",
                "non-utf8 arguments are not supported",
            ));
        };

        match value {
            "--json" => {}
            value if value.starts_with('-') => {
                return Err(CliError::usage(
                    "info",
                    json,
                    "cli.unsupported_flag",
                    format!("unsupported flag for info: {value}"),
                ));
            }
            value => {
                if path.is_some() {
                    return Err(CliError::usage(
                        "info",
                        json,
                        "cli.unexpected_argument",
                        "info accepts at most one path argument",
                    ));
                }

                path = Some(PathBuf::from(value));
            }
        }
    }

    let target = match path {
        Some(path) if path.is_absolute() => path,
        Some(path) => cwd.join(path),
        None => cwd.to_path_buf(),
    };
    let output = build_info_output(&target)
        .map_err(|error| CliError::from_detection("info", json, &target, error))?;

    println!(
        "{}",
        render_info_output(&output, json)
            .map_err(|message| CliError::serialization("info", json, message))?
    );

    Ok(())
}

fn run_init(args: &[OsString], cwd: &Path) -> Result<(), CliError> {
    let json = json_requested(args);
    let mut dry_run = false;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err(CliError::usage(
                "init",
                json,
                "cli.non_utf8_argument",
                "non-utf8 arguments are not supported",
            ));
        };

        match value {
            "--json" => {}
            "--dry-run" => dry_run = true,
            value if value.starts_with('-') => {
                return Err(CliError::usage(
                    "init",
                    json,
                    "cli.unsupported_flag",
                    format!("unsupported flag for init: {value}"),
                ));
            }
            _ => {
                return Err(CliError::usage(
                    "init",
                    json,
                    "cli.unexpected_argument",
                    "init does not accept positional arguments",
                ));
            }
        }
    }

    let plan = if dry_run {
        plan_init(cwd).map_err(|error| {
            CliError::from_codegen("init", json, cwd, "failed to plan init", error)
        })?
    } else {
        apply_init(cwd).map_err(|error| {
            CliError::from_codegen("init", json, cwd, "failed to initialize project", error)
        })?
    };

    let status = if dry_run {
        CommandStatus::Planned
    } else if plan.is_empty() {
        CommandStatus::NoChange
    } else {
        CommandStatus::Success
    };

    println!(
        "{}",
        render_init_plan(&plan, json, status)
            .map_err(|message| CliError::serialization("init", json, message))?
    );

    Ok(())
}

fn run_view(args: &[OsString], cwd: &Path) -> Result<(), CliError> {
    let json = json_requested(args);
    let mut include_source = false;
    let mut registry_source: Option<String> = None;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err(CliError::usage(
                "view",
                json,
                "cli.non_utf8_argument",
                "non-utf8 arguments are not supported",
            ));
        };

        match value {
            "--json" => {}
            "--source" => include_source = true,
            value if value.starts_with('-') => {
                return Err(CliError::usage(
                    "view",
                    json,
                    "cli.unsupported_flag",
                    format!("unsupported flag for view: {value}"),
                ));
            }
            value => {
                if registry_source.is_some() {
                    return Err(CliError::usage(
                        "view",
                        json,
                        "cli.unexpected_argument",
                        "view accepts exactly one registry source",
                    ));
                }

                registry_source = Some(value.to_owned());
            }
        }
    }

    let source = registry_source.ok_or_else(|| {
        CliError::usage(
            "view",
            json,
            "cli.missing_argument",
            "view requires a registry source",
        )
    })?;
    let item = load_registry_item(&source, cwd)
        .map_err(|error| CliError::from_registry("view", json, &source, error))?;

    println!(
        "{}",
        render_registry_item(&item, json, include_source)
            .map_err(|message| CliError::serialization("view", json, message))?
    );

    Ok(())
}

fn run_sync(args: &[OsString], cwd: &Path) -> Result<(), CliError> {
    let json = json_requested(args);
    let mut dry_run = false;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err(CliError::usage(
                "sync",
                json,
                "cli.non_utf8_argument",
                "non-utf8 arguments are not supported",
            ));
        };

        match value {
            "--json" => {}
            "--dry-run" => dry_run = true,
            value if value.starts_with('-') => {
                return Err(CliError::usage(
                    "sync",
                    json,
                    "cli.unsupported_flag",
                    format!("unsupported flag for sync: {value}"),
                ));
            }
            _ => {
                return Err(CliError::usage(
                    "sync",
                    json,
                    "cli.unexpected_argument",
                    "sync does not accept positional arguments",
                ));
            }
        }
    }

    let plan = if dry_run {
        plan_sync(cwd).map_err(|error| {
            CliError::from_codegen("sync", json, cwd, "failed to plan sync", error)
        })?
    } else {
        apply_sync(cwd).map_err(|error| {
            CliError::from_codegen("sync", json, cwd, "failed to sync project", error)
        })?
    };
    let status = if dry_run {
        CommandStatus::Planned
    } else if plan.is_empty() {
        CommandStatus::NoChange
    } else {
        CommandStatus::Success
    };

    println!(
        "{}",
        render_sync_plan(&plan, json, status)
            .map_err(|message| CliError::serialization("sync", json, message))?
    );

    Ok(())
}

fn render_add_plan(plan: &AddPlan, json: bool, status: CommandStatus) -> Result<String, String> {
    if json {
        let output = AddCommandOutput {
            item_id: plan.item_id.clone(),
            item_name: plan.item_name.clone(),
            content_hash: plan.content_hash.clone(),
            dependencies: plan
                .cargo_plan
                .iter()
                .map(CargoRequirementOutput::from)
                .collect(),
        };
        return serde_json::to_string_pretty(
            &CliEnvelope::new("add", status, output)
                .with_changes(&plan.changes)
                .with_diagnostics(&plan.diagnostics),
        )
        .map_err(|error| format!("failed to serialize add plan: {error}"));
    }

    if plan.is_empty() {
        return Ok(format!(
            "add {}: {}",
            plan.item_name,
            unchanged_label(status)
        ));
    }

    let mut output = format!("add {} {} changes:", plan.item_name, change_verb(status));
    for change in &plan.changes {
        output.push_str(&format!("\n- {:?} {}", change.kind, change.path));
    }
    append_cargo_plan_text(&mut output, &plan.cargo_plan);
    Ok(output)
}

fn render_sync_plan(plan: &SyncPlan, json: bool, status: CommandStatus) -> Result<String, String> {
    if json {
        let output = SyncCommandOutput {
            item_ids: plan.item_ids.clone(),
            dependencies: plan
                .cargo_plan
                .iter()
                .map(CargoRequirementOutput::from)
                .collect(),
        };
        return serde_json::to_string_pretty(
            &CliEnvelope::new("sync", status, output)
                .with_changes(&plan.changes)
                .with_diagnostics(&plan.diagnostics),
        )
        .map_err(|error| format!("failed to serialize sync plan: {error}"));
    }

    if plan.is_empty() {
        return Ok(format!("sync: {}", unchanged_label(status)));
    }

    let mut output = format!("sync {} changes:", change_verb(status));
    for change in &plan.changes {
        output.push_str(&format!("\n- {:?} {}", change.kind, change.path));
    }
    append_cargo_plan_text(&mut output, &plan.cargo_plan);
    Ok(output)
}

fn append_cargo_plan_text(output: &mut String, cargo_plan: &[CargoPlanEntry]) {
    if cargo_plan.is_empty() {
        return;
    }

    output.push_str("\nrequired cargo dependencies:");
    for entry in cargo_plan {
        output.push_str(&format!("\n- {}", cargo_plan_entry_label(entry)));
    }
}

fn cargo_plan_entry_label(entry: &CargoPlanEntry) -> String {
    let source = match entry.source.kind {
        leptos_ui_kit_registry::CargoPlanSourceKind::Version => entry
            .source
            .version
            .as_deref()
            .map(|version| format!("version {version}"))
            .unwrap_or_else(|| "version <missing>".to_owned()),
        leptos_ui_kit_registry::CargoPlanSourceKind::Git => {
            let url = entry.source.url.as_deref().unwrap_or("<missing-url>");
            let rev = entry.source.rev.as_deref().unwrap_or("<missing-rev>");
            format!("git {url} rev {rev}")
        }
    };
    let features = if entry.features.is_empty() {
        String::new()
    } else {
        format!(" features [{}]", entry.features.join(", "))
    };

    format!("{} ({source}){features}", entry.crate_name)
}

fn render_init_plan(plan: &InitPlan, json: bool, status: CommandStatus) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(
            &CliEnvelope::new("init", status, InitCommandOutput {}).with_changes(&plan.changes),
        )
        .map_err(|error| format!("failed to serialize init plan: {error}"));
    }

    if plan.is_empty() {
        return Ok(format!("init: {}", unchanged_label(status)));
    }

    let mut output = format!("init {} changes:", change_verb(status));
    for change in &plan.changes {
        output.push_str(&format!("\n- {:?} {}", change.kind, change.path));
    }
    Ok(output)
}

fn change_verb(status: CommandStatus) -> &'static str {
    match status {
        CommandStatus::Planned => "planned",
        CommandStatus::Success => "applied",
        CommandStatus::NoChange => "unchanged",
        CommandStatus::Warning
        | CommandStatus::Error
        | CommandStatus::Conflict
        | CommandStatus::Unsupported => "reported",
    }
}

fn unchanged_label(status: CommandStatus) -> &'static str {
    if status == CommandStatus::Planned {
        "no changes planned"
    } else {
        "no changes"
    }
}

fn render_version_output(json: bool) -> Result<String, String> {
    render_version_output_with_tool(json, canonical_tool_config())
}

fn render_version_output_with_tool(
    json: bool,
    tool: Result<ToolConfig, ConfigError>,
) -> Result<String, String> {
    let output = version_output_with_tool(tool)?;

    if json {
        return serde_json::to_string_pretty(&CliEnvelope::success("version", output))
            .map_err(|error| format!("failed to serialize version output: {error}"));
    }

    Ok(format!("{} {}", output.binary, output.version))
}

fn render_help_output(command: &str, help: &str, json: bool) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(&CliEnvelope::success(
            command,
            HelpCommandOutput {
                usage: help.to_owned(),
            },
        ))
        .map_err(|error| format!("failed to serialize help output: {error}"));
    }
    Ok(help.to_owned())
}

fn version_output_with_tool(
    tool: Result<ToolConfig, ConfigError>,
) -> Result<VersionCommandOutput, String> {
    let rev = match tool {
        Ok(tool) => Some(match tool.source {
            ToolSourceConfig::Git { rev, .. } => rev,
        }),
        Err(ConfigError::MissingToolProvenance { .. }) => None,
        Err(error) => return Err(format!("invalid compiled tool provenance: {error}")),
    };

    Ok(VersionCommandOutput {
        package: TOOL_PACKAGE,
        binary: TOOL_BINARY,
        version: env!("CARGO_PKG_VERSION"),
        schema_version: SCHEMA_VERSION,
        source: VersionSourceOutput {
            kind: "git",
            url: TOOL_GIT_URL,
            rev,
        },
    })
}

fn render_info_output(output: &InfoOutput, json: bool) -> Result<String, String> {
    let project_root = &output.detected.project_root;
    let installed_lock = read_info_install_lock(project_root, output.kit_config.as_ref());
    let command_output = InfoCommandOutput {
        project_root: ".",
        project_kind: output.detected.project_kind.as_str(),
        workspace_mode: match output.detected.workspace_mode {
            leptos_ui_kit_registry::WorkspaceMode::SingleCrate => "single-crate",
            leptos_ui_kit_registry::WorkspaceMode::SinglePackageWorkspaceRoot => {
                "single-package-workspace-root"
            }
        },
        cargo_manifest: logical_path(project_root, &output.detected.cargo_manifest_path)
            .unwrap_or_else(|| "Cargo.toml".to_owned()),
        source_root: logical_path(project_root, &output.detected.source_root)
            .unwrap_or_else(|| "src".to_owned()),
        index_html: output
            .detected
            .index_html_path
            .as_ref()
            .and_then(|path| logical_path(project_root, path))
            .unwrap_or_else(|| "not-applicable".to_owned()),
        stylesheet: logical_path(project_root, &output.detected.css_file_path)
            .unwrap_or_else(|| DEFAULT_CSS_PATH.to_owned()),
        render_mode_contract: output.detected.render_mode_contract,
        render_mode_selection: output.detected.render_mode_selection.clone(),
        render_mode: output.detected.render_mode.map(|mode| mode.as_str()),
        config_path: output
            .detected
            .kit_config_path
            .as_ref()
            .and_then(|path| logical_path(project_root, path)),
        registry_available: validate_built_in_registry_health().is_ok(),
        installed: installed_lock.as_ref().map(|lock| InstalledSummaryOutput {
            lock_path: output
                .kit_config
                .as_ref()
                .map(install_lock_path)
                .unwrap_or_else(|| DEFAULT_KIT_LOCK_PATH.to_owned()),
            item_ids: lock.items.keys().cloned().collect(),
            file_paths: lock.files_by_path.keys().cloned().collect(),
            style_block_ids: lock.style_blocks_by_id.keys().cloned().collect(),
        }),
    };

    if json {
        return serde_json::to_string_pretty(&CliEnvelope::success("info", &command_output))
            .map_err(|error| format!("failed to serialize info output: {error}"));
    }

    Ok(format!(
        "project_root: {}\nproject_kind: {}\nworkspace_mode: {:?}\nsource_root: {}\nindex_html: {}\ncss_file: {}\nrender_mode_contract: {:?}\nrender_mode_selection: {:?}\nrender_mode: {}\nregistry_available: {}\ninstalled_lock: {}",
        command_output.project_root,
        command_output.project_kind,
        output.detected.workspace_mode,
        command_output.source_root,
        command_output.index_html,
        command_output.stylesheet,
        command_output.render_mode_contract,
        command_output.render_mode_selection,
        output
            .detected
            .render_mode
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| "unknown".to_owned()),
        command_output.registry_available,
        command_output.installed.is_some()
    ))
}

fn render_registry_item(
    item: &ResolvedRegistryItem,
    json: bool,
    include_source: bool,
) -> Result<String, String> {
    if include_source {
        let output = registry_item_source_output(item)?;
        if json {
            return serde_json::to_string_pretty(&CliEnvelope::success("view", output))
                .map_err(|error| format!("failed to serialize registry item source: {error}"));
        }

        let mut rendered = format!(
            "name: {}\nkind: {}\ncontent_hash: {}",
            output.resolved.name, output.resolved.kind, output.resolved.content_hash
        );
        for source in output.sources {
            rendered.push_str(&format!(
                "\n--- {} ({}) ---\n{}",
                source.path, source.kind, source.content
            ));
        }
        return Ok(rendered);
    }

    let output = registry_item_output(item)?;
    if json {
        return serde_json::to_string_pretty(&CliEnvelope::success("view", &output))
            .map_err(|error| format!("failed to serialize registry item: {error}"));
    }

    Ok(format!(
        "name: {}\nkind: {}\nsource_kind: {}\nsource_path: {}",
        output.name, output.kind, output.source_kind, output.source_path
    ))
}

fn registry_item_source_output(
    item: &ResolvedRegistryItem,
) -> Result<RegistryItemSourceOutput, String> {
    let resolved = registry_item_output(item)?;
    let mut sources = Vec::new();
    for file in &item.targets.ui_files {
        sources.push(RegistrySourceContent {
            path: file.source.clone(),
            kind: "rust".to_owned(),
            content: read_built_in_registry_source(&file.source)
                .map_err(|error| format!("failed to read {}: {error}", file.source))?,
        });
    }
    for style in &item.targets.style_blocks {
        sources.push(RegistrySourceContent {
            path: style.source.clone(),
            kind: "css".to_owned(),
            content: read_built_in_registry_source(&style.source)
                .map_err(|error| format!("failed to read {}: {error}", style.source))?,
        });
    }

    Ok(RegistryItemSourceOutput { resolved, sources })
}

fn registry_item_output(item: &ResolvedRegistryItem) -> Result<RegistryItemOutput, String> {
    let source_path = item
        .source_path
        .to_str()
        .and_then(safe_logical_locator)
        .ok_or_else(|| "registry manifest locator is not a safe logical path".to_owned())?;
    let ui_files = item
        .targets
        .ui_files
        .iter()
        .map(|target| {
            Ok(RegistryUiTargetOutput {
                source: safe_logical_locator(&target.source).ok_or_else(|| {
                    "registry source locator is not a safe logical path".to_owned()
                })?,
                path: safe_logical_locator(&target.path).ok_or_else(|| {
                    "registry target locator is not a safe logical path".to_owned()
                })?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let style_blocks = item
        .targets
        .style_blocks
        .iter()
        .map(|target| {
            Ok(RegistryStyleTargetOutput {
                source: safe_logical_locator(&target.source).ok_or_else(|| {
                    "registry stylesheet locator is not a safe logical path".to_owned()
                })?,
                id: target.id.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(RegistryItemOutput {
        source_kind: "built-in",
        source_path,
        content_hash: item.content_hash.clone(),
        name: item.item.name.clone(),
        kind: item.item.kind.to_string(),
        version: item.item.version.clone(),
        title: item.item.title.clone(),
        description: item.item.description.clone(),
        registry_dependencies: item.item.registry_dependencies.clone(),
        targets: RegistryTargetsOutput {
            ui_files,
            style_blocks,
        },
        cargo_plan: item
            .item
            .cargo_plan
            .iter()
            .map(CargoRequirementOutput::from)
            .collect(),
    })
}

fn safe_logical_locator(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path.is_empty()
        || path.starts_with('/')
        || path.as_bytes().get(1) == Some(&b':')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return None;
    }
    Some(path)
}

fn build_doctor_output(cwd: &Path, strict: bool, check: bool, trunk_build: bool) -> DoctorOutput {
    build_doctor_output_with_registry_health(cwd, strict, check, trunk_build, || {
        validate_built_in_registry_health().map_err(|error| error.to_string())
    })
}

fn build_doctor_output_with_registry_health(
    cwd: &Path,
    strict: bool,
    check: bool,
    trunk_build: bool,
    registry_health: impl FnOnce() -> Result<(), String>,
) -> DoctorOutput {
    let mut checks = Vec::new();
    let mut detected_project_kind = None;

    match check_pending_recovery(cwd) {
        Ok(()) => checks.push(DoctorCheck::pass(
            "transaction_recovery",
            "no pending durable transaction recovery",
        )),
        Err(error) => checks.push(DoctorCheck::fail("transaction_recovery", error.to_string())),
    }

    match build_info_output(cwd) {
        Ok(info) => {
            detected_project_kind = Some(info.detected.project_kind);
            checks.push(DoctorCheck::pass(
                "project",
                format!(
                    "supported {} project detected",
                    info.detected.project_kind.as_str()
                ),
            ));
            dependency_check(
                &mut checks,
                strict,
                "dependency.leptos",
                "leptos",
                info.detected.dependency_plan.leptos.status,
            );
            checks.extend(stylesheet_checks(cwd, strict, &info));

            let lock_state = load_doctor_lock(cwd, info.kit_config.as_ref());
            checks.extend(lock_state_checks(cwd, strict, &lock_state));

            if let Some(config) = info.kit_config.as_ref() {
                checks.push(DoctorCheck::pass("config", "kit.json is valid"));
                let names = config
                    .items
                    .iter()
                    .map(|item| item.item_name().to_owned())
                    .collect::<BTreeSet<_>>();
                match resolve_doctor_registry_snapshot(
                    names,
                    &config.install.ui_dir,
                    Some(&config.styles.css),
                    config.project.kind.render_mode_contract(),
                ) {
                    Ok(snapshot) => {
                        checks.extend(config_closure_checks(strict, &snapshot));
                        if let Some(lock) = lock_state.lock() {
                            checks.push(compare_config_hash(cwd, strict, config, lock, &snapshot));
                        }
                        checks.extend(registry_snapshot_checks(
                            cwd,
                            strict,
                            lock_state.lock(),
                            &snapshot,
                        ));
                        checks.extend(registry_dependency_checks(
                            cwd,
                            strict,
                            &snapshot.cargo_plan,
                        ));
                    }
                    Err(error) => checks.push(DoctorCheck::fail(
                        "registry.snapshot",
                        format!("failed to resolve configured registry closure: {error}"),
                    )),
                }
            } else {
                checks.push(strict_check(
                    strict,
                    "config",
                    "kit.json is missing; run leptos_ui_kit init",
                ));

                if !strict && let Some(lock) = lock_state.lock() {
                    let names = lock
                        .items
                        .values()
                        .map(|item| item.name.clone())
                        .collect::<BTreeSet<_>>();
                    let css_path = match fallback_css_path(lock) {
                        Ok(css_path) => Some(css_path),
                        Err(message) => {
                            checks.push(DoctorCheck::warning("registry.snapshot", message));
                            None
                        }
                    };
                    match resolve_doctor_registry_snapshot(
                        names,
                        DEFAULT_UI_DIR,
                        css_path.as_deref(),
                        info.detected.render_mode_contract,
                    ) {
                        Ok(snapshot) => {
                            checks.push(DoctorCheck::warning(
                                "registry.snapshot",
                                "using lock-derived registry closure because kit.json is missing",
                            ));
                            checks.extend(registry_snapshot_checks(
                                cwd,
                                strict,
                                Some(lock),
                                &snapshot,
                            ));
                            checks.extend(registry_dependency_checks(
                                cwd,
                                strict,
                                &snapshot.cargo_plan,
                            ));
                        }
                        Err(error) => checks.push(strict_check(
                            strict,
                            "registry.snapshot",
                            format!("failed to resolve lock-derived registry closure: {error}"),
                        )),
                    }
                }
            }
        }
        Err(error) => {
            checks.push(strict_check(strict, "project", error.to_string()));
        }
    }

    match registry_health() {
        Ok(()) => checks.push(DoctorCheck::pass(
            "registry",
            "built-in registry runtime health is valid",
        )),
        Err(error) => checks.push(DoctorCheck::fail("registry", error.to_string())),
    }

    if check {
        let args = doctor_cargo_check_args(detected_project_kind);
        checks.push(run_command_check("build.cargo_check", cwd, "cargo", args));
    }

    if trunk_build {
        if detected_project_kind == Some(ProjectKind::SingleCrateTrunkCsr) {
            checks.push(run_command_check("build.trunk", cwd, "trunk", &["build"]));
        } else {
            checks.push(DoctorCheck::fail(
                "build.trunk",
                "--trunk-build is supported only for single-crate-trunk-csr projects",
            ));
        }
    }

    DoctorOutput {
        project_root: cwd.to_path_buf(),
        strict,
        check,
        trunk_build,
        checks,
    }
}

fn doctor_cargo_check_args(project_kind: Option<ProjectKind>) -> &'static [&'static str] {
    match project_kind {
        Some(ProjectKind::SingleCrateTrunkCsr | ProjectKind::SingleCrateBrowserHydration) => {
            &["check", "--target", "wasm32-unknown-unknown"]
        }
        Some(ProjectKind::SingleCrateNativeSsr | ProjectKind::SharedLibraryCrate) | None => {
            &["check"]
        }
    }
}

fn resolve_doctor_registry_snapshot(
    requested_names: BTreeSet<String>,
    ui_dir: &str,
    css_path: Option<&str>,
    render_mode_contract: RenderModeContract,
) -> Result<DoctorRegistrySnapshot, String> {
    let sorted_names = requested_names.iter().cloned().collect::<Vec<_>>();
    let resolved =
        resolve_built_in_registry_items(&sorted_names).map_err(|error| error.to_string())?;
    build_doctor_registry_snapshot(
        requested_names,
        ui_dir,
        css_path,
        render_mode_contract,
        resolved,
    )
}

fn build_doctor_registry_snapshot(
    requested_names: BTreeSet<String>,
    ui_dir: &str,
    css_path: Option<&str>,
    render_mode_contract: RenderModeContract,
    resolved: Vec<ResolvedRegistryItem>,
) -> Result<DoctorRegistrySnapshot, String> {
    let mut resolved_names = BTreeSet::new();
    let mut resolved_order = Vec::new();
    let mut expected_items = BTreeMap::new();
    let mut files_by_path = BTreeMap::new();
    let mut style_blocks_by_id = BTreeMap::new();
    let cargo_plan_entries = resolved
        .iter()
        .flat_map(|item| item.item.cargo_plan.iter().cloned())
        .collect::<Vec<_>>();
    let cargo_plan = normalize_cargo_plan_for_project(&cargo_plan_entries, render_mode_contract)
        .map_err(|error| error.to_string())?;
    let style_ids_by_item = resolved
        .iter()
        .map(|item| {
            (
                item.item.name.clone(),
                item.targets
                    .style_blocks
                    .iter()
                    .map(|style| style.id.clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut style_dependencies = BTreeSet::new();

    for item in resolved {
        let item_id = format!("builtin:{}", item.item.name);
        resolved_names.insert(item.item.name.clone());
        resolved_order.push(item.item.name.clone());
        let mut files = Vec::new();
        for target in &item.targets.ui_files {
            let generated =
                read_built_in_registry_source(&target.source).map_err(|error| error.to_string())?;
            let logical_path = format!(
                "{}/{path}",
                ui_dir.trim_end_matches('/'),
                path = target.path
            );
            let generated_hash = hash_content_bytes(generated.as_bytes());
            if files_by_path
                .insert(logical_path.clone(), item_id.clone())
                .is_some()
            {
                return Err(format!(
                    "registry closure has duplicate file target {logical_path}"
                ));
            }
            files.push(InstalledFile {
                path: logical_path,
                kind: "rust".to_owned(),
                generated_hash: generated_hash.clone(),
                local_hash_at_install: generated_hash,
            });
        }

        let mut style_blocks = Vec::new();
        for target in &item.targets.style_blocks {
            let generated =
                read_built_in_registry_source(&target.source).map_err(|error| error.to_string())?;
            if style_blocks_by_id
                .insert(target.id.clone(), item_id.clone())
                .is_some()
            {
                return Err(format!(
                    "registry closure has duplicate managed CSS block {}",
                    target.id
                ));
            }
            style_blocks.push(InstalledStyleBlock {
                css_path: css_path.unwrap_or_default().to_owned(),
                block_id: target.id.clone(),
                generated_hash: hash_content_bytes(generated.as_bytes()),
            });
        }

        for dependency_name in &item.item.registry_dependencies {
            let dependency_style_ids = style_ids_by_item.get(dependency_name).ok_or_else(|| {
                format!(
                    "resolved registry closure is missing dependency {dependency_name} required by {}",
                    item.item.name
                )
            })?;
            let dependent_style_ids = style_ids_by_item.get(&item.item.name).ok_or_else(|| {
                format!(
                    "resolved registry closure is missing style metadata for {}",
                    item.item.name
                )
            })?;
            for dependency_id in dependency_style_ids {
                for dependent_id in dependent_style_ids {
                    style_dependencies.insert((dependency_id.clone(), dependent_id.clone()));
                }
            }
        }

        let installed = InstalledItem {
            id: item_id.clone(),
            name: item.item.name,
            kind: item.item.kind,
            source: "builtin".to_owned(),
            version: item.item.version,
            content_hash: item.content_hash,
            files,
            style_blocks,
        };
        if expected_items.insert(item_id.clone(), installed).is_some() {
            return Err(format!(
                "registry closure contains duplicate item {item_id}"
            ));
        }
    }

    Ok(DoctorRegistrySnapshot {
        requested_names,
        resolved_names,
        resolved_order,
        expected_items,
        files_by_path,
        style_blocks_by_id,
        css_path: css_path.map(str::to_owned),
        cargo_plan,
        style_dependencies,
    })
}

fn load_doctor_lock(cwd: &Path, config: Option<&KitConfig>) -> DoctorLockState {
    let logical_path = config
        .map(install_lock_path)
        .unwrap_or_else(|| DEFAULT_KIT_LOCK_PATH.to_owned());
    let path = cwd.join(&logical_path);
    if !path.is_file() {
        return DoctorLockState::Missing { logical_path, path };
    }
    let input = match fs::read_to_string(&path) {
        Ok(input) => input,
        Err(error) => {
            return DoctorLockState::Invalid {
                logical_path,
                path,
                message: format!("failed to read lock: {error}"),
            };
        }
    };
    match parse_install_lock_str_at_path(&input, Path::new(&logical_path)) {
        Ok(lock) => DoctorLockState::Valid {
            logical_path,
            path,
            lock: Box::new(lock),
        },
        Err(error) => DoctorLockState::Invalid {
            logical_path,
            path,
            message: error.to_string(),
        },
    }
}

fn lock_state_checks(cwd: &Path, strict: bool, state: &DoctorLockState) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    match state {
        DoctorLockState::Missing { logical_path, path } => checks.push(
            strict_check(strict, "lock", format!("{logical_path} is missing"))
                .with_path(path.display().to_string()),
        ),
        DoctorLockState::Invalid { path, message, .. } => {
            checks.push(strict_check(strict, "lock", message).with_path(path.display().to_string()))
        }
        DoctorLockState::Valid { .. } => {
            checks.push(
                DoctorCheck::pass("lock", "install lock is valid")
                    .with_path(state.path().display().to_string()),
            );
            checks.extend(git_metadata_checks(cwd, strict, state.logical_path()));
        }
    }
    checks
}

fn fallback_css_path(lock: &InstallLock) -> Result<String, String> {
    let paths = lock
        .items
        .values()
        .flat_map(|item| item.style_blocks.iter().map(|block| block.css_path.clone()))
        .collect::<BTreeSet<_>>();
    match paths.len() {
        0 => Ok(DEFAULT_CSS_PATH.to_owned()),
        1 => Ok(paths
            .into_iter()
            .next()
            .unwrap_or_else(|| DEFAULT_CSS_PATH.to_owned())),
        _ => Err(format!(
            "lock-derived registry closure spans multiple stylesheet paths [{}]; managed CSS and dependency-order inspection was skipped",
            paths.into_iter().collect::<Vec<_>>().join(", ")
        )),
    }
}

fn run_command_check(name: &str, cwd: &Path, program: &str, args: &[&str]) -> DoctorCheck {
    match Command::new(program).args(args).current_dir(cwd).output() {
        Ok(output) if output.status.success() => {
            DoctorCheck::pass(name, format!("{} {} passed", program, args.join(" ")))
        }
        Ok(output) => DoctorCheck::fail(
            name,
            format!(
                "{} {} failed: {}",
                program,
                args.join(" "),
                command_output_summary(&output.stdout, &output.stderr)
            ),
        ),
        Err(error) => DoctorCheck::fail(
            name,
            format!("failed to run {} {}: {error}", program, args.join(" ")),
        ),
    }
}

fn command_output_summary(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    let summary = if !stderr.is_empty() { stderr } else { stdout };
    if summary.is_empty() {
        "process exited with a non-zero status".to_owned()
    } else {
        summary.chars().take(600).collect()
    }
}

fn dependency_check(
    checks: &mut Vec<DoctorCheck>,
    strict: bool,
    name: &str,
    crate_name: &str,
    status: DependencyStatus,
) {
    match status {
        DependencyStatus::Satisfied => {
            checks.push(DoctorCheck::pass(
                name,
                format!("{crate_name} dependency is satisfied"),
            ));
        }
        DependencyStatus::Missing => checks.push(strict_check(
            strict,
            name,
            format!("{crate_name} dependency is missing"),
        )),
        DependencyStatus::Incompatible => checks.push(strict_check(
            strict,
            name,
            format!("{crate_name} dependency is incompatible"),
        )),
    }
}

fn strict_check(strict: bool, name: impl Into<String>, message: impl Into<String>) -> DoctorCheck {
    if strict {
        DoctorCheck::fail(name, message)
    } else {
        DoctorCheck::warning(name, message)
    }
}

fn config_closure_checks(strict: bool, snapshot: &DoctorRegistrySnapshot) -> Vec<DoctorCheck> {
    if snapshot.requested_names == snapshot.resolved_names {
        return vec![
            DoctorCheck::pass(
                "config_closure",
                "kit.json item membership equals the resolved registry closure",
            )
            .with_path(DEFAULT_KIT_CONFIG_PATH),
        ];
    }

    vec![
        strict_check(
            strict,
            "config_closure",
            set_drift_message(
                "kit.json item membership differs from the resolved registry closure",
                &snapshot.resolved_names,
                &snapshot.requested_names,
            ),
        )
        .with_path(DEFAULT_KIT_CONFIG_PATH),
    ]
}

fn registry_snapshot_checks(
    cwd: &Path,
    strict: bool,
    lock: Option<&InstallLock>,
    snapshot: &DoctorRegistrySnapshot,
) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    checks.extend(lock_snapshot_checks(strict, lock, snapshot));
    checks.extend(installed_file_snapshot_checks(cwd, strict, snapshot));
    checks.extend(managed_css_snapshot_checks(cwd, strict, snapshot));
    checks
}

fn lock_snapshot_checks(
    strict: bool,
    lock: Option<&InstallLock>,
    snapshot: &DoctorRegistrySnapshot,
) -> Vec<DoctorCheck> {
    let Some(lock) = lock else {
        return Vec::new();
    };
    let mut checks = Vec::new();
    if lock.kit_version == SCHEMA_VERSION {
        checks.push(DoctorCheck::pass(
            "lock_metadata",
            "install lock kitVersion matches the registry schema version",
        ));
    } else {
        checks.push(strict_check(
            strict,
            "lock_metadata",
            format!(
                "install lock kitVersion {} must be {SCHEMA_VERSION}",
                lock.kit_version
            ),
        ));
    }
    let expected_ids = snapshot
        .expected_items
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let actual_ids = lock.items.keys().cloned().collect::<BTreeSet<_>>();
    if expected_ids == actual_ids {
        checks.push(DoctorCheck::pass(
            "lock_closure",
            "install lock item membership equals the resolved registry closure",
        ));
    } else {
        checks.push(strict_check(
            strict,
            "lock_closure",
            set_drift_message(
                "install lock item membership differs from the resolved registry closure",
                &expected_ids,
                &actual_ids,
            ),
        ));
    }

    for (item_id, expected) in &snapshot.expected_items {
        let Some(actual) = lock.items.get(item_id) else {
            continue;
        };
        if actual.id == expected.id
            && actual.name == expected.name
            && actual.kind == expected.kind
            && actual.source == expected.source
            && actual.version == expected.version
            && actual.content_hash == expected.content_hash
        {
            checks.push(DoctorCheck::pass(
                "lock_item_metadata",
                format!("installed item metadata for {item_id} matches the registry snapshot"),
            ));
        } else {
            checks.push(strict_check(
                strict,
                "lock_item_metadata",
                format!("installed item metadata for {item_id} differs from the registry snapshot"),
            ));
        }

        let expected_files = installed_file_records(&expected.files);
        let actual_files = installed_file_records(&actual.files);
        if expected_files == actual_files {
            checks.push(DoctorCheck::pass(
                "lock_file_targets",
                format!("installed file targets for {item_id} match the registry snapshot"),
            ));
        } else {
            checks.push(strict_check(
                strict,
                "lock_file_targets",
                record_drift_message(
                    &format!(
                        "installed file targets for {item_id} differ from the registry snapshot"
                    ),
                    &expected_files,
                    &actual_files,
                ),
            ));
        }

        let include_css_path = snapshot.css_path.is_some();
        let expected_styles = installed_style_records(&expected.style_blocks, include_css_path);
        let actual_styles = installed_style_records(&actual.style_blocks, include_css_path);
        if expected_styles == actual_styles {
            checks.push(DoctorCheck::pass(
                "lock_style_targets",
                format!("managed CSS targets for {item_id} match the registry snapshot"),
            ));
        } else {
            checks.push(strict_check(
                strict,
                "lock_style_targets",
                record_drift_message(
                    &format!("managed CSS targets for {item_id} differ from the registry snapshot"),
                    &expected_styles,
                    &actual_styles,
                ),
            ));
        }
    }

    if lock.files_by_path == snapshot.files_by_path {
        checks.push(DoctorCheck::pass(
            "lock_files_by_path",
            "filesByPath exactly matches registry target ownership",
        ));
    } else {
        checks.push(strict_check(
            strict,
            "lock_files_by_path",
            "filesByPath differs from registry target ownership",
        ));
    }
    if lock.style_blocks_by_id == snapshot.style_blocks_by_id {
        checks.push(DoctorCheck::pass(
            "lock_style_blocks_by_id",
            "styleBlocksById exactly matches registry target ownership",
        ));
    } else {
        checks.push(strict_check(
            strict,
            "lock_style_blocks_by_id",
            "styleBlocksById differs from registry target ownership",
        ));
    }

    checks
}

fn installed_file_records(files: &[InstalledFile]) -> Vec<String> {
    let mut records = files
        .iter()
        .map(|file| {
            format!(
                "{}|{}|{}|{}",
                file.path, file.kind, file.generated_hash, file.local_hash_at_install
            )
        })
        .collect::<Vec<_>>();
    records.sort();
    records
}

fn installed_style_records(styles: &[InstalledStyleBlock], include_css_path: bool) -> Vec<String> {
    let mut records = styles
        .iter()
        .map(|style| {
            if include_css_path {
                format!(
                    "{}|{}|{}",
                    style.css_path, style.block_id, style.generated_hash
                )
            } else {
                format!("{}|{}", style.block_id, style.generated_hash)
            }
        })
        .collect::<Vec<_>>();
    records.sort();
    records
}

fn installed_file_snapshot_checks(
    cwd: &Path,
    strict: bool,
    snapshot: &DoctorRegistrySnapshot,
) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    for expected in snapshot.expected_items.values() {
        for file in &expected.files {
            let path = cwd.join(&file.path);
            match fs::read(&path) {
                Ok(content) if hash_content_bytes(&content) == file.generated_hash => checks.push(
                    DoctorCheck::pass(
                        "installed_file",
                        format!("installed file {} matches the registry snapshot", file.path),
                    )
                    .with_path(path.display().to_string()),
                ),
                Ok(_) => checks.push(
                    strict_check(
                        strict,
                        "installed_file",
                        format!(
                            "installed file {} differs from the registry snapshot",
                            file.path
                        ),
                    )
                    .with_path(path.display().to_string()),
                ),
                Err(error) => checks.push(
                    strict_check(
                        strict,
                        "installed_file",
                        format!(
                            "installed file {} is missing or unreadable: {error}",
                            file.path
                        ),
                    )
                    .with_path(path.display().to_string()),
                ),
            }
        }
    }
    checks
}

fn managed_css_snapshot_checks(
    cwd: &Path,
    strict: bool,
    snapshot: &DoctorRegistrySnapshot,
) -> Vec<DoctorCheck> {
    let Some(css_logical_path) = snapshot.css_path.as_deref() else {
        return Vec::new();
    };
    let path = cwd.join(css_logical_path);
    let css = match fs::read_to_string(&path) {
        Ok(css) => css,
        Err(error) => {
            if snapshot.style_blocks_by_id.is_empty() {
                return Vec::new();
            }
            return snapshot
                .style_blocks_by_id
                .keys()
                .map(|block_id| {
                    strict_check(
                        strict,
                        "managed_css",
                        format!(
                            "managed CSS block {block_id} is missing because {} is unreadable: {error}",
                            css_logical_path
                        ),
                    )
                    .with_path(path.display().to_string())
                })
                .collect();
        }
    };
    let ranges = match inspect_managed_css_blocks_at_path(&css, css_logical_path) {
        Ok(ranges) => ranges,
        Err(error) => {
            return vec![
                strict_check(strict, "managed_css", error.to_string())
                    .with_path(path.display().to_string()),
            ];
        }
    };
    let mut checks = Vec::new();
    let expected_ids = snapshot
        .style_blocks_by_id
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let actual_ids = ranges.keys().cloned().collect::<BTreeSet<_>>();
    if expected_ids == actual_ids {
        checks.push(
            DoctorCheck::pass(
                "managed_css_closure",
                "managed CSS block membership equals the resolved registry closure",
            )
            .with_path(path.display().to_string()),
        );
    } else {
        checks.push(
            strict_check(
                strict,
                "managed_css_closure",
                set_drift_message(
                    "managed CSS block membership differs from the resolved registry closure",
                    &expected_ids,
                    &actual_ids,
                ),
            )
            .with_path(path.display().to_string()),
        );
    }

    for expected in snapshot.expected_items.values() {
        for block in &expected.style_blocks {
            let Some(range) = ranges.get(&block.block_id) else {
                checks.push(
                    strict_check(
                        strict,
                        "managed_css",
                        format!("managed CSS block {} is missing", block.block_id),
                    )
                    .with_path(path.display().to_string()),
                );
                continue;
            };
            let current = &css[range.start..range.end];
            if hash_content_bytes(current.as_bytes()) == block.generated_hash {
                checks.push(
                    DoctorCheck::pass(
                        "managed_css",
                        format!(
                            "managed CSS block {} matches the registry snapshot",
                            block.block_id
                        ),
                    )
                    .with_path(path.display().to_string()),
                );
            } else {
                checks.push(
                    strict_check(
                        strict,
                        "managed_css",
                        format!(
                            "managed CSS block {} differs from the registry snapshot",
                            block.block_id
                        ),
                    )
                    .with_path(path.display().to_string()),
                );
            }
        }
    }

    for (dependency_id, dependent_id) in &snapshot.style_dependencies {
        let (Some(dependency), Some(dependent)) =
            (ranges.get(dependency_id), ranges.get(dependent_id))
        else {
            continue;
        };
        if dependency.start < dependent.start {
            checks.push(
                DoctorCheck::pass(
                    "managed_css_order",
                    format!("managed CSS dependency {dependency_id} precedes {dependent_id}"),
                )
                .with_path(path.display().to_string()),
            );
        } else {
            checks.push(
                strict_check(
                    strict,
                    "managed_css_order",
                    format!("managed CSS dependency {dependency_id} must precede {dependent_id}"),
                )
                .with_path(path.display().to_string()),
            );
        }
    }

    checks
}

fn set_drift_message(
    prefix: &str,
    expected: &BTreeSet<String>,
    actual: &BTreeSet<String>,
) -> String {
    let missing = expected.difference(actual).cloned().collect::<Vec<_>>();
    let extra = actual.difference(expected).cloned().collect::<Vec<_>>();
    format!(
        "{prefix}; missing [{}]; extra [{}]",
        missing.join(", "),
        extra.join(", ")
    )
}

fn record_drift_message(prefix: &str, expected: &[String], actual: &[String]) -> String {
    format!(
        "{prefix}; expected [{}]; actual [{}]",
        expected.join(", "),
        actual.join(", ")
    )
}

fn stylesheet_checks(cwd: &Path, strict: bool, info: &InfoOutput) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let css_logical_path = info
        .kit_config
        .as_ref()
        .map(|config| config.styles.css.as_str())
        .unwrap_or(DEFAULT_CSS_PATH);
    let css_path = cwd.join(css_logical_path);

    if css_path.is_file() {
        checks.push(
            DoctorCheck::pass("stylesheet", format!("{css_logical_path} exists"))
                .with_path(css_path.display().to_string()),
        );
    } else {
        checks.push(
            strict_check(
                strict,
                "stylesheet",
                format!("{css_logical_path} is missing; run leptos_ui_kit init or sync"),
            )
            .with_path(css_path.display().to_string()),
        );
    }

    let Some(index_html_path) = info.detected.index_html_path.as_ref() else {
        checks.push(DoctorCheck::pass(
            "stylesheet_link",
            format!(
                "{} does not own a Trunk index.html",
                info.detected.project_kind.as_str()
            ),
        ));
        return checks;
    };

    match fs::read_to_string(index_html_path) {
        Ok(html) => match inspect_html_stylesheet(&html, css_logical_path) {
            Ok(HtmlStylesheetState::Present { .. }) => checks.push(
                DoctorCheck::pass(
                    "stylesheet_link",
                    format!("index.html links {css_logical_path} for Trunk"),
                )
                .with_path(index_html_path.display().to_string()),
            ),
            Ok(HtmlStylesheetState::Missing { .. }) => checks.push(
                strict_check(
                    strict,
                    "stylesheet_link",
                    format!("index.html is missing a Trunk CSS link for {css_logical_path}"),
                )
                .with_path(index_html_path.display().to_string()),
            ),
            Err(error) => checks.push(
                strict_check(
                    strict,
                    "stylesheet_link",
                    format!("index.html cannot be inspected safely: {error}"),
                )
                .with_path(index_html_path.display().to_string()),
            ),
        },
        Err(error) => checks.push(
            strict_check(
                strict,
                "stylesheet_link",
                format!("failed to read index.html: {error}"),
            )
            .with_path(index_html_path.display().to_string()),
        ),
    }

    checks
}

fn registry_dependency_checks(
    cwd: &Path,
    strict: bool,
    cargo_plan: &[CargoPlanEntry],
) -> Vec<DoctorCheck> {
    if cargo_plan.is_empty() {
        return Vec::new();
    }

    match detect_cargo_plan_requirements(cwd, cargo_plan) {
        Ok(requirements) => requirements
            .iter()
            .map(|requirement| registry_dependency_check(strict, requirement))
            .collect(),
        Err(error) => vec![strict_check(
            strict,
            "dependency.registry",
            format!("failed to inspect registry dependency plan: {error}"),
        )],
    }
}

fn registry_dependency_check(strict: bool, requirement: &DependencyRequirement) -> DoctorCheck {
    let name = format!("dependency.registry.{}", requirement.crate_name);
    match requirement.status {
        DependencyStatus::Satisfied => DoctorCheck::pass(
            name,
            format!(
                "{} dependency satisfies registry plan",
                requirement.crate_name
            ),
        ),
        DependencyStatus::Missing if !requirement.required => DoctorCheck::pass(
            name,
            format!(
                "optional {} dependency is not present",
                requirement.crate_name
            ),
        ),
        DependencyStatus::Missing => strict_check(
            strict,
            name,
            format!(
                "{} dependency required by registry plan is missing",
                requirement.crate_name
            ),
        ),
        DependencyStatus::Incompatible => strict_check(
            strict,
            name,
            format!(
                "{} dependency does not satisfy registry plan",
                requirement.crate_name
            ),
        ),
    }
}

fn compare_config_hash(
    cwd: &Path,
    strict: bool,
    config: &KitConfig,
    lock: &InstallLock,
    snapshot: &DoctorRegistrySnapshot,
) -> DoctorCheck {
    let path = cwd.join(DEFAULT_KIT_CONFIG_PATH);
    match fs::read(&path) {
        Ok(content) if hash_content_bytes(&content) == lock.project.config_hash => {
            DoctorCheck::pass("config_hash", "kit.json hash matches install lock")
                .with_path(path.display().to_string())
        }
        Ok(_) if snapshot.requested_names == snapshot.resolved_names => {
            let mut canonical = config.clone();
            canonical.items = snapshot
                .resolved_order
                .iter()
                .filter_map(|name| {
                    config
                        .items
                        .iter()
                        .find(|item| item.item_name() == name)
                        .cloned()
                })
                .collect();
            match kit_config_to_json(&canonical) {
                Ok(content)
                    if hash_content_bytes(content.as_bytes()) == lock.project.config_hash =>
                {
                    DoctorCheck::pass(
                        "config_hash",
                        "kit.json differs only by nonsemantic JSON formatting or item ordering",
                    )
                    .with_path(path.display().to_string())
                }
                Ok(_) | Err(_) => strict_check(
                    strict,
                    "config_hash",
                    "kit.json hash differs from install lock",
                )
                .with_path(path.display().to_string()),
            }
        }
        Ok(_) => strict_check(
            strict,
            "config_hash",
            "kit.json hash differs from install lock",
        )
        .with_path(path.display().to_string()),
        Err(error) => strict_check(
            strict,
            "config_hash",
            format!("failed to read config: {error}"),
        )
        .with_path(path.display().to_string()),
    }
}

fn git_metadata_checks(cwd: &Path, strict: bool, state_logical_path: &str) -> Vec<DoctorCheck> {
    if !is_git_worktree(cwd) {
        return Vec::new();
    }

    let paths = BTreeSet::from([state_logical_path.to_owned()]);
    let mut ignored = Vec::new();
    for path in paths {
        match git_check_ignore(cwd, &path) {
            GitIgnoreStatus::Ignored => ignored.push(path),
            GitIgnoreStatus::NotIgnored => {}
            GitIgnoreStatus::Unknown(message) => {
                return vec![strict_check(strict, "git_metadata", message)];
            }
        }
    }

    if ignored.is_empty() {
        vec![DoctorCheck::pass(
            "git_metadata",
            "installer metadata is not ignored by Git",
        )]
    } else {
        ignored
            .into_iter()
            .map(|path| {
                strict_check(
                    strict,
                    "git_metadata",
                    format!("installer metadata {path} is ignored by Git"),
                )
                .with_path(path)
            })
            .collect()
    }
}

fn is_git_worktree(cwd: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "true")
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GitIgnoreStatus {
    Ignored,
    NotIgnored,
    Unknown(String),
}

fn git_check_ignore(cwd: &Path, path: &str) -> GitIgnoreStatus {
    match Command::new("git")
        .args(["check-ignore", "-q", path])
        .current_dir(cwd)
        .status()
    {
        Ok(status) if status.success() => GitIgnoreStatus::Ignored,
        Ok(status) if status.code() == Some(1) => GitIgnoreStatus::NotIgnored,
        Ok(status) => GitIgnoreStatus::Unknown(format!(
            "failed to check Git ignore status for {path}: exit status {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        )),
        Err(error) => GitIgnoreStatus::Unknown(format!(
            "failed to run git check-ignore for {path}: {error}"
        )),
    }
}

fn doctor_status(output: &DoctorOutput) -> CommandStatus {
    if output.has_failures() {
        CommandStatus::Error
    } else if output.has_warnings() {
        CommandStatus::Warning
    } else {
        CommandStatus::Success
    }
}

fn render_doctor_output(
    output: &DoctorOutput,
    json: bool,
    status: CommandStatus,
) -> Result<String, String> {
    if json {
        let command_output = DoctorCommandOutput {
            project_root: ".",
            strict: output.strict,
            check: output.check,
            trunk_build: output.trunk_build,
            checks: output
                .checks
                .iter()
                .map(|check| DoctorCheckOutput {
                    name: check.name.clone(),
                    status: check.status,
                    message: redact_project_root(&check.message, &output.project_root),
                    path: check
                        .path
                        .as_deref()
                        .and_then(|path| public_path(&output.project_root, path)),
                })
                .collect(),
        };
        let diagnostics = doctor_diagnostics(output);
        return serde_json::to_string_pretty(
            &CliEnvelope::new("doctor", status, command_output).with_diagnostics(&diagnostics),
        )
        .map_err(|error| format!("failed to serialize doctor output: {error}"));
    }

    let mut rendered = String::from("doctor checks:");
    for check in &output.checks {
        rendered.push_str(&format!(
            "\n- {:?} {}: {}",
            check.status,
            check.name,
            redact_project_root(&check.message, &output.project_root)
        ));
    }
    Ok(rendered)
}

fn doctor_diagnostics(output: &DoctorOutput) -> Vec<Diagnostic> {
    output
        .checks
        .iter()
        .filter_map(|check| match check.status {
            DoctorCheckStatus::Pass => None,
            DoctorCheckStatus::Warning => Some((DiagnosticLevel::Warning, check)),
            DoctorCheckStatus::Fail => Some((DiagnosticLevel::Error, check)),
        })
        .map(|(level, check)| {
            let diagnostic = Diagnostic::new(
                level,
                format!("doctor.{}", check.name),
                redact_project_root(&check.message, &output.project_root),
            );
            check
                .path
                .as_deref()
                .and_then(|path| public_path(&output.project_root, path))
                .map_or(diagnostic.clone(), |path| diagnostic.with_path(path))
        })
        .collect()
}

fn public_path(project_root: &Path, path: &str) -> Option<String> {
    let path = Path::new(path);
    if path.is_absolute() {
        logical_path(project_root, path).or_else(|| (path == project_root).then(|| ".".to_owned()))
    } else {
        Some(path.to_string_lossy().replace('\\', "/"))
    }
}

fn redact_project_root(message: &str, project_root: &Path) -> String {
    let root = project_root.to_string_lossy();
    if root.is_empty() || root == "." {
        message.to_owned()
    } else {
        message.replace(root.as_ref(), ".")
    }
}

fn read_info_install_lock(
    project_root: &Path,
    kit_config: Option<&KitConfig>,
) -> Option<InstallLock> {
    let logical_path = kit_config
        .map(install_lock_path)
        .unwrap_or_else(|| DEFAULT_KIT_LOCK_PATH.to_owned());
    let input = fs::read_to_string(project_root.join(&logical_path)).ok()?;
    parse_install_lock_str_at_path(&input, Path::new(&logical_path)).ok()
}

fn usage() -> String {
    "usage: leptos_ui_kit <add|doctor|info|init|sync|view> [--json] [--dry-run] [path-or-source]"
        .to_owned()
}

fn help_text() -> String {
    [
        "leptos_ui_kit",
        "",
        "usage: leptos_ui_kit <command> [options]",
        "",
        "commands:",
        "  info                 inspect a supported Leptos project",
        "  init                 create src/components/ui/_kit/kit.json and kit-managed app files",
        "  view <item>          show a registry item",
        "  add <item>           add a registry item to the app",
        "  sync                 reconcile installed items with src/components/ui/_kit/kit.json",
        "  doctor               validate generated source, CSS, lock metadata, and dependencies",
        "",
        "global options:",
        "  --cwd <path>         run against a different project root",
        "  --quiet              accepted for script compatibility",
        "  --verbose            accepted for script compatibility",
        "  --help               print help",
        "  --version            print version",
    ]
    .join("\n")
}

fn command_help(command: &str) -> Result<String, String> {
    let lines = match command {
        "add" => vec![
            "usage: leptos_ui_kit add <item> [--dry-run] [--json]",
            "",
            "Adds a built-in item to desired state and creates only its required source and CSS.",
        ],
        "doctor" => vec![
            "usage: leptos_ui_kit doctor [--strict] [--check] [--trunk-build] [--json]",
            "",
            "Validates project shape, dependencies, desired state, generated files, managed CSS, and installer metadata.",
        ],
        "info" => vec![
            "usage: leptos_ui_kit info [path] [--json]",
            "",
            "Inspects a supported Leptos CSR, SSR, hydration, or shared-library project.",
        ],
        "init" => vec![
            "usage: leptos_ui_kit init [--dry-run] [--json]",
            "",
            "Creates src/components/ui/_kit/kit.json, src/components/ui/_kit/kit.lock.json, and the minimal app-owned source and CSS files.",
        ],
        "sync" => vec![
            "usage: leptos_ui_kit sync [--dry-run] [--json]",
            "",
            "Reconciles lock metadata and managed CSS with desired items while retaining app-owned Rust and module files.",
        ],
        "view" => vec![
            "usage: leptos_ui_kit view <item> [--source] [--json]",
            "",
            "Shows a built-in registry item and optionally its source files.",
        ],
        _ => return Err(usage()),
    };
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs};

    use leptos_ui_kit_codegen::{extract_managed_css_block, plan_init};
    use leptos_ui_kit_registry::{
        canonical_kit_config, desired_builtin_button_item, desired_builtin_spinner_item,
        desired_builtin_tokens_item, kit_config_to_json, kit_config_with_desired_item,
        load_built_in_registry_item, parse_kit_json_str,
    };
    use tempfile::tempdir;

    const PINNED_BUTTON_CSS: &str =
        include_str!("../tests/fixtures/theme_pre_refactor_06124efa/button.css");
    const PINNED_SPINNER_CSS: &str =
        include_str!("../tests/fixtures/theme_pre_refactor_06124efa/spinner.css");

    const TEST_TOOL_REV: &str = "0123456789abcdef0123456789abcdef01234567";
    const APP_TOKEN_OVERRIDES: &str = r#"
/* application-owned token overrides */
:root {
  --kit-color-primary: rebeccapurple;
  --kit-button-gap: 0.75rem;
}
"#;

    #[test]
    fn info_envelope_json_outputs_detected_project_shape() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
leptos_router = "0.9.0-alpha"
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::create_dir(root.join("styles")).expect("create styles");
        fs::write(root.join("styles/kit.css"), ":root {}\n").expect("write css");
        fs::write(
            root.join("index.html"),
            r#"<!DOCTYPE html>
<html>
  <head>
    <link data-trunk rel="css" href="styles/kit.css" />
  </head>
  <body></body>
</html>
"#,
        )
        .expect("write html");

        let info = build_info_output(root).expect("build info output");
        let output = render_info_output(&info, true).expect("render json");

        assert!(output.contains("\"schemaVersion\": \"0.9.0-alpha\""));
        assert!(output.contains("\"command\": \"info\""));
        assert!(output.contains("\"projectRoot\": \".\""));
        assert!(output.contains("\"projectKind\": \"single-crate-trunk-csr\""));
        assert!(output.contains("\"renderModeContract\""));
        assert!(output.contains("\"renderModeSelection\""));
        assert!(output.contains("\"renderMode\": \"csr\""));
        assert!(output.contains("\"stylesheet\": \"styles/kit.css\""));
        assert!(!output.contains(&root.display().to_string()));
        assert!(!output.contains("\"kitConfig\""));
        assert!(!output.contains("\"installedLock\""));
    }

    #[test]
    fn view_envelope_json_outputs_built_in_registry_item() {
        let item = load_registry_item("button", Path::new(".")).expect("load built-in item");
        let output = render_registry_item(&item, true, false).expect("render json");

        assert!(output.contains("\"schemaVersion\": \"0.9.0-alpha\""));
        assert!(output.contains("\"command\": \"view\""));
        assert!(output.contains("\"name\": \"button\""));
        assert!(output.contains("\"sourceKind\": \"built-in\""));
        assert!(output.contains("\"kind\": \"ui\""));
        assert!(output.contains("\"cargoPlan\""));
        assert!(output.contains("\"source\""));
        assert!(output.contains("\"features\""));
    }

    #[test]
    fn view_envelope_json_outputs_css_only_tokens_item() {
        let item = load_registry_item("tokens", Path::new(".")).expect("load tokens item");
        let output = render_registry_item(&item, true, true).expect("render json");

        assert!(output.contains("\"name\": \"tokens\""));
        assert!(output.contains("\"kind\": \"foundation\""));
        assert!(output.contains("styles/tokens.css"));
        assert!(!output.contains("\"kind\": \"rust\""));
    }

    #[test]
    fn view_source_outputs_registry_source_contents() {
        let item = load_registry_item("button", Path::new(".")).expect("load built-in item");
        let output = render_registry_item(&item, true, true).expect("render json");

        assert!(output.contains("\"sources\""));
        assert!(output.contains("pub fn Button"));
        assert!(output.contains(".kit-button"));
    }

    #[test]
    fn init_dry_run_envelope_json_outputs_planned_changes() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");

        run(
            vec![
                OsString::from("init"),
                OsString::from("--dry-run"),
                OsString::from("--json"),
            ],
            root,
        )
        .expect("run init dry-run");

        let output = render_init_plan(
            &plan_init(root).expect("plan init"),
            true,
            CommandStatus::Planned,
        )
        .expect("render");
        assert!(output.contains("\"command\": \"init\""));
        assert!(output.contains("\"status\": \"planned\""));
        assert!(output.contains("\"path\": \"src/components/ui/_kit/kit.json\""));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&output).expect("init JSON")["data"],
            serde_json::json!({})
        );
        assert!(!output.contains("\"files\""));
        assert!(!output.contains("\"content\""));
        assert!(!root.join(DEFAULT_KIT_CONFIG_PATH).exists());
    }

    #[test]
    fn init_write_creates_files() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");

        run(vec![OsString::from("init")], root).expect("run init");

        assert!(root.join(DEFAULT_KIT_CONFIG_PATH).is_file());
        assert!(root.join(DEFAULT_KIT_LOCK_PATH).is_file());
    }

    #[test]
    fn add_dry_run_envelope_json_outputs_planned_changes_without_writes() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");

        run(
            vec![
                OsString::from("add"),
                OsString::from("button"),
                OsString::from("--dry-run"),
                OsString::from("--json"),
            ],
            root,
        )
        .expect("run add dry-run");

        let output = render_add_plan(
            &plan_add(root, "button").expect("plan add"),
            true,
            CommandStatus::Planned,
        )
        .expect("render add");
        assert!(output.contains("\"command\": \"add\""));
        assert!(output.contains("\"status\": \"planned\""));
        assert!(output.contains("\"itemName\": \"button\""));
        assert!(output.contains("\"dependencies\""));
        assert!(output.contains("\"crate\": \"leptos\""));
        assert!(output.contains("\"path\": \"src/components/ui/button.rs\""));
        assert!(output.contains("\"path\": \"src/components/ui/_kit/kit.lock.json\""));
        assert!(!output.contains("\"files\""));
        assert!(!output.contains("\"lock\""));
        assert!(!output.contains("\"content\""));
        assert_eq!(output.matches("\"changes\"").count(), 1);
        assert_eq!(output.matches("\"diagnostics\"").count(), 1);
        assert!(!root.join("src/components/ui/button.rs").exists());
    }

    #[test]
    fn add_write_installs_button_and_then_reports_no_change() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");

        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");
        assert!(root.join("src/components/ui/button.rs").is_file());
        assert!(root.join(DEFAULT_KIT_LOCK_PATH).is_file());

        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run second add");
        let output = render_add_plan(
            &plan_add(root, "button").expect("plan add"),
            true,
            CommandStatus::NoChange,
        )
        .expect("render add");
        assert!(output.contains("\"status\": \"no_change\""));
    }

    #[test]
    fn sync_dry_run_envelope_json_outputs_declared_button_changes() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");
        write_desired_button_config(root);

        run(
            vec![
                OsString::from("sync"),
                OsString::from("--dry-run"),
                OsString::from("--json"),
            ],
            root,
        )
        .expect("run sync dry-run");

        let output = render_sync_plan(
            &plan_sync(root).expect("plan sync"),
            true,
            CommandStatus::Planned,
        )
        .expect("render sync");
        assert!(output.contains("\"command\": \"sync\""));
        assert!(output.contains("\"status\": \"planned\""));
        assert!(output.contains("\"itemIds\": ["));
        assert!(output.contains("\"builtin:button\""));
        assert!(output.contains("\"dependencies\""));
        assert!(output.contains("\"crate\": \"leptos\""));
        assert!(!output.contains("\"crate\": \"leptos_router\""));
        assert!(output.contains("\"path\": \"src/components/ui/button.rs\""));
        assert!(!output.contains("\"files\""));
        assert!(!output.contains("\"lock\""));
        assert!(!output.contains("\"content\""));
    }

    #[test]
    fn sync_write_installs_declared_button_and_then_reports_no_change() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");
        write_desired_button_config(root);

        run(vec![OsString::from("sync")], root).expect("run sync");
        assert!(root.join("src/components/ui/button.rs").is_file());

        run(vec![OsString::from("sync")], root).expect("run second sync");
        let output = render_sync_plan(
            &plan_sync(root).expect("plan sync"),
            true,
            CommandStatus::NoChange,
        )
        .expect("render sync");
        assert!(output.contains("\"status\": \"no_change\""));
    }

    #[test]
    fn doctor_strict_passes_after_sync_reconciles_button_dependencies() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_doctor_project(root);
        run(vec![OsString::from("init")], root).expect("run init");
        write_desired_button_config(root);

        run(vec![OsString::from("sync")], root).expect("run sync");
        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Success);
        assert!(output.contains("managed CSS block tokens matches the registry snapshot"));
        assert!(
            output.contains("install lock item membership equals the resolved registry closure")
        );
    }

    #[test]
    fn ordinary_and_strict_doctor_fail_on_injected_registry_corruption() {
        let project = tempdir().expect("tempdir");
        for strict in [false, true] {
            let doctor = build_doctor_output_with_registry_health(
                project.path(),
                strict,
                false,
                false,
                || {
                    Err(
                        "invalid built-in theme CSS registry/styles/tokens.css: injected drift"
                            .to_owned(),
                    )
                },
            );
            assert_doctor_check(
                &doctor,
                "registry",
                DoctorCheckStatus::Fail,
                "registry/styles/tokens.css",
            );
            let registry = doctor
                .checks
                .iter()
                .find(|check| check.name == "registry")
                .expect("registry check");
            assert!(registry.message.contains("registry/styles/tokens.css"));
            assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        }
    }

    #[test]
    fn doctor_ignores_retired_application_owned_rust_outside_the_lock() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        let button_path = root.join("src/components/ui/button.rs");
        let module_path = root.join("src/components/ui/mod.rs");
        let button_before = fs::read(&button_path).expect("read button source");
        let module_before = fs::read(&module_path).expect("read UI module");

        let config_path = root.join(DEFAULT_KIT_CONFIG_PATH);
        let mut config =
            parse_kit_json_str(&fs::read_to_string(&config_path).expect("read config"))
                .expect("parse config");
        config.items = vec![
            desired_builtin_tokens_item(),
            desired_builtin_spinner_item(),
        ];
        let config_json = kit_config_to_json(&config).expect("serialize retained config");
        fs::write(&config_path, &config_json).expect("write retained config");

        let mut lock = read_install_lock(root);
        let retired = lock.items.remove("builtin:button").expect("retired button");
        for file in retired.files {
            assert_eq!(
                lock.files_by_path.remove(&file.path).as_deref(),
                Some("builtin:button")
            );
        }
        for block in retired.style_blocks {
            assert_eq!(
                lock.style_blocks_by_id.remove(&block.block_id).as_deref(),
                Some("builtin:button")
            );
        }
        lock.project.config_hash = hash_content_bytes(config_json.as_bytes());
        write_install_lock(root, &lock);

        let css_path = root.join(DEFAULT_CSS_PATH);
        let css = fs::read_to_string(&css_path).expect("read stylesheet");
        fs::write(&css_path, remove_managed_css_block(css, "button")).expect("retire button CSS");

        let doctor = build_doctor_output(root, true, false, false);

        assert_eq!(doctor_status(&doctor), CommandStatus::Success);
        assert_eq!(
            fs::read(button_path).expect("read retained button"),
            button_before
        );
        assert_eq!(
            fs::read(module_path).expect("read retained module"),
            module_before
        );
    }

    #[test]
    fn sync_to_empty_retains_app_source_and_passes_strict_doctor() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        let button_path = root.join("src/components/ui/button.rs");
        let module_path = root.join("src/components/ui/mod.rs");
        let button_before = fs::read(&button_path).expect("read button source");
        let module_before = fs::read(&module_path).expect("read UI module");
        let config_path = root.join(DEFAULT_KIT_CONFIG_PATH);
        let mut config =
            parse_kit_json_str(&fs::read_to_string(&config_path).expect("read config"))
                .expect("parse config");
        config.items.clear();
        fs::write(
            &config_path,
            kit_config_to_json(&config).expect("serialize empty config"),
        )
        .expect("write empty config");

        let first = apply_sync(root).expect("sync to empty");

        assert!(first.lock.items.is_empty());
        assert!(first.lock.files_by_path.is_empty());
        assert!(first.lock.style_blocks_by_id.is_empty());
        assert_eq!(
            fs::read(&button_path).expect("read retained button"),
            button_before
        );
        assert_eq!(
            fs::read(&module_path).expect("read retained UI module"),
            module_before
        );
        assert_strict_doctor_success(root);
        assert!(apply_sync(root).expect("idempotent empty sync").is_empty());
    }

    #[test]
    fn doctor_surfaces_pending_or_invalid_transaction_recovery_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        create_doctor_project(root);
        let transactions = root.join("src/components/ui/_kit/.transactions");
        fs::create_dir_all(&transactions).expect("create transaction directory");
        let journal = transactions.join("transaction-00000000000000000000000000000000.json");
        fs::write(&journal, b"not a valid journal\n").expect("write invalid journal");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&transactions, fs::Permissions::from_mode(0o700))
                .expect("transaction directory mode");
            fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).expect("journal mode");
        }
        let before = fs::read(&journal).expect("journal before doctor");

        let doctor = build_doctor_output(root, false, false, false);

        assert_doctor_check(
            &doctor,
            "transaction_recovery",
            DoctorCheckStatus::Fail,
            "transaction recovery is required",
        );
        assert_eq!(fs::read(journal).expect("journal after doctor"), before);
    }

    #[test]
    fn pinned_button_spinner_migrations_are_canonical_and_strict_doctor_clean() {
        for (css_path, with_overrides) in [
            (DEFAULT_CSS_PATH, false),
            (DEFAULT_CSS_PATH, true),
            ("styles/custom-theme.css", false),
            ("styles/custom-theme.css", true),
        ] {
            let dir = tempdir().expect("tempdir");
            let root = dir.path();
            create_current_button_install(root, css_path);
            reconstruct_pinned_button_install(root, css_path, with_overrides);

            let first = apply_sync(root).unwrap_or_else(|error| {
                panic!(
                    "pinned migration failed for {css_path} (overrides={with_overrides}): {error}"
                )
            });
            assert!(
                !first.is_empty(),
                "pinned migration unexpectedly had no changes for {css_path} (overrides={with_overrides})"
            );

            assert_current_button_install(
                root,
                css_path,
                with_overrides.then_some(APP_TOKEN_OVERRIDES),
            );
            assert_strict_doctor_success(root);
            assert!(
                apply_sync(root).expect("second pinned sync").is_empty(),
                "pinned migration was not idempotent for {css_path} (overrides={with_overrides})"
            );
        }
    }

    #[test]
    fn sync_relocates_a_current_tracked_late_foundation_before_dependents() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);

        let css_path = root.join(DEFAULT_CSS_PATH);
        let css = fs::read_to_string(&css_path).expect("read current css");
        let tokens = managed_css_block(&css, "tokens");
        let mut late = remove_managed_css_block(css, "tokens");
        if !late.ends_with('\n') {
            late.push('\n');
        }
        late.push_str(&tokens);
        late.push_str(APP_TOKEN_OVERRIDES);
        fs::write(&css_path, late).expect("write late tokens css");

        let first = apply_sync(root).expect("relocate tracked tokens");
        assert!(!first.is_empty());
        assert_current_button_install(root, DEFAULT_CSS_PATH, Some(APP_TOKEN_OVERRIDES));
        assert_strict_doctor_success(root);
        assert!(
            apply_sync(root)
                .expect("second late-foundation sync")
                .is_empty()
        );
    }

    #[test]
    fn sync_inserts_one_foundation_for_multiple_current_dependents() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        let css_path = "styles/multi-dependent.css";
        create_current_button_install(root, css_path);

        let stylesheet = root.join(css_path);
        let css = fs::read_to_string(&stylesheet).expect("read current css");
        fs::write(&stylesheet, remove_managed_css_block(css, "tokens"))
            .expect("remove tokens block");
        remove_tokens_from_config_and_lock(root);

        let first = apply_sync(root).expect("insert shared tokens foundation");
        assert!(!first.is_empty());
        assert_current_button_install(root, css_path, None);
        let css = fs::read_to_string(&stylesheet).expect("read migrated css");
        assert_eq!(css.matches("/* leptos-ui-kit:start tokens */").count(), 1);
        assert_strict_doctor_success(root);
        assert!(
            apply_sync(root)
                .expect("second multi-dependent sync")
                .is_empty()
        );
    }

    #[test]
    fn doctor_strict_json_passes_after_init_and_add() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
leptos_router = "0.9.0-alpha"
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");
        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");

        run(
            vec![
                OsString::from("doctor"),
                OsString::from("--strict"),
                OsString::from("--json"),
            ],
            root,
        )
        .expect("run doctor");
        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert!(output.contains("\"command\": \"doctor\""));
        assert!(output.contains("\"status\": \"success\""));
        assert!(output.contains("\"name\": \"registry\""));
        assert!(output.contains("\"status\": \"pass\""));
        assert!(output.contains("\"name\": \"dependency.registry.leptos\""));
        assert!(!output.contains("\"name\": \"dependency.registry.leptos_router\""));
    }

    #[test]
    fn doctor_strict_passes_after_tokens_only_add_without_router() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("add"), OsString::from("tokens")], root).expect("run add tokens");

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Success);
        assert!(output.contains("built-in registry runtime health is valid"));
        assert!(!output.contains("\"name\": \"dependency.leptos_router\""));
        assert!(!output.contains("\"name\": \"dependency.registry."));
        assert!(!root.join("src/components/mod.rs").exists());
        assert!(!root.join("src/components/ui/mod.rs").exists());
        assert!(!root.join("src/components/ui/tokens.rs").exists());
    }

    #[test]
    fn custom_components_root_converges_and_passes_strict_doctor() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        let mut config = canonical_kit_config().expect("canonical config");
        config.install.ui_dir = "src/widgets/kit_ui".to_owned();
        config.install.ui_mod = "src/widgets/kit_ui/mod.rs".to_owned();
        config.install.components_mod = "src/widgets/mod.rs".to_owned();
        config.styles.css = "styles/custom.css".to_owned();
        let config_path = root.join(DEFAULT_KIT_CONFIG_PATH);
        fs::create_dir_all(config_path.parent().expect("config parent"))
            .expect("create config parent");
        fs::write(
            &config_path,
            kit_config_to_json(&config).expect("serialize custom config"),
        )
        .expect("write custom config");

        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run custom add");
        assert!(root.join("src/widgets/kit_ui/button.rs").is_file());
        assert!(root.join("src/widgets/kit_ui/spinner.rs").is_file());
        assert!(root.join("src/widgets/kit_ui/mod.rs").is_file());
        assert!(root.join("src/widgets/mod.rs").is_file());
        assert!(!root.join("src/components/ui/button.rs").exists());
        assert_strict_doctor_success(root);

        let button_path = root.join("src/widgets/kit_ui/button.rs");
        let button_before = fs::read(&button_path).expect("read custom button");
        let mut retained =
            parse_kit_json_str(&fs::read_to_string(&config_path).expect("read custom config"))
                .expect("parse custom config");
        retained.items = vec![desired_builtin_spinner_item()];
        fs::write(
            &config_path,
            kit_config_to_json(&retained).expect("serialize retained config"),
        )
        .expect("write retained config");

        run(vec![OsString::from("sync")], root).expect("run custom sync");
        assert_eq!(
            fs::read(&button_path).expect("read retained custom button"),
            button_before
        );
        assert_strict_doctor_success(root);
        assert!(apply_sync(root).expect("idempotent custom sync").is_empty());
    }

    #[test]
    fn doctor_diagnostics_preserve_each_duplicate_check_path() {
        let output = DoctorOutput {
            project_root: PathBuf::from("."),
            strict: true,
            check: false,
            trunk_build: false,
            checks: vec![
                DoctorCheck::warning("style_block", "first").with_path("first.css"),
                DoctorCheck::warning("style_block", "second").with_path("second.css"),
            ],
        };
        let diagnostics = doctor_diagnostics(&output);

        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].path.as_deref(), Some("first.css"));
        assert_eq!(diagnostics[1].path.as_deref(), Some("second.css"));
    }

    #[test]
    fn doctor_strict_fails_when_desired_item_is_not_installed() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_doctor_project(root);
        run(vec![OsString::from("init")], root).expect("run init");
        write_desired_button_config(root);

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert!(output.contains("\"code\": \"doctor.config_closure\""));
        assert!(output.contains("missing [spinner, tokens]"));
        assert!(output.contains("\"code\": \"doctor.lock_closure\""));
    }

    #[test]
    fn doctor_strict_fails_when_installed_item_is_not_desired() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_doctor_project(root);
        run(vec![OsString::from("init")], root).expect("run init");
        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");
        write_empty_items_config(root);

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert!(output.contains("\"code\": \"doctor.lock_closure\""));
        assert!(output.contains("extra [builtin:button, builtin:spinner, builtin:tokens]"));
    }

    #[test]
    fn doctor_strict_fails_when_installer_metadata_is_ignored() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_doctor_project(root);
        init_git(root);
        fs::write(
            root.join(".gitignore"),
            "/src/components/ui/_kit/kit.lock.json\n",
        )
        .expect("write gitignore");
        run(vec![OsString::from("init")], root).expect("run init");
        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert!(output.contains("\"code\": \"doctor.git_metadata\""));
        assert!(
            output.contains(
                "installer metadata src/components/ui/_kit/kit.lock.json is ignored by Git"
            )
        );
    }

    #[test]
    fn doctor_strict_rejects_lock_hash_mismatches() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
leptos_router = "0.9.0-alpha"
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");
        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");

        let lock_path = root.join(DEFAULT_KIT_LOCK_PATH);
        let mut lock: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&lock_path).expect("read lock"))
                .expect("parse lock");
        lock["items"]["builtin:button"]["files"][0]["generatedHash"] =
            serde_json::Value::String(format!("sha256:{}", "0".repeat(64)));
        fs::write(
            &lock_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&lock).expect("serialize lock")
            ),
        )
        .expect("write lock");

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert!(output.contains("\"code\": \"doctor.lock_file_targets\""));
    }

    #[test]
    fn doctor_rejects_duplicate_managed_css_blocks() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
leptos_router = "0.9.0-alpha"
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");
        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");

        let css_path = root.join("styles/kit.css");
        let mut css = fs::read_to_string(&css_path).expect("read css");
        let block = extract_managed_css_block(&css, "button")
            .expect("extract block")
            .expect("button block");
        css.push('\n');
        css.push_str(&block);
        fs::write(&css_path, css).expect("write css");

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert!(output.contains("\"code\": \"doctor.managed_css\""));
        assert!(output.contains("managed CSS block button markers are ambiguous"));
    }

    #[test]
    fn doctor_rejects_self_consistent_tokens_removal_from_project_state() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);

        let css_path = root.join(DEFAULT_CSS_PATH);
        let css = fs::read_to_string(&css_path).expect("read stylesheet");
        fs::write(&css_path, remove_managed_css_block(css, "tokens")).expect("remove tokens CSS");
        remove_tokens_from_config_and_lock(root);

        let strict = build_doctor_output(root, true, false, false);
        assert_eq!(doctor_status(&strict), CommandStatus::Error);
        assert_doctor_check(
            &strict,
            "config_closure",
            DoctorCheckStatus::Fail,
            "missing [tokens]",
        );
        assert_doctor_check(
            &strict,
            "lock_closure",
            DoctorCheckStatus::Fail,
            "missing [builtin:tokens]",
        );
        assert_doctor_check(
            &strict,
            "managed_css_closure",
            DoctorCheckStatus::Fail,
            "missing [tokens]",
        );
        assert_doctor_check(
            &strict,
            "managed_css",
            DoctorCheckStatus::Fail,
            "managed CSS block tokens is missing",
        );

        let ordinary = build_doctor_output(root, false, false, false);
        assert_eq!(doctor_status(&ordinary), CommandStatus::Warning);
        assert_doctor_check(
            &ordinary,
            "config_closure",
            DoctorCheckStatus::Warning,
            "missing [tokens]",
        );
    }

    #[test]
    fn doctor_rejects_consistently_removed_registry_targets_and_indexes() {
        for target_kind in ["file", "style"] {
            let dir = tempdir().expect("tempdir");
            let root = dir.path();
            create_current_button_install(root, DEFAULT_CSS_PATH);
            let mut lock = read_install_lock(root);
            let button = lock
                .items
                .get_mut("builtin:button")
                .expect("button lock item");

            match target_kind {
                "file" => {
                    let removed = button.files.pop().expect("button file target");
                    assert_eq!(
                        lock.files_by_path.remove(&removed.path).as_deref(),
                        Some("builtin:button")
                    );
                }
                "style" => {
                    let removed = button.style_blocks.pop().expect("button style target");
                    assert_eq!(
                        lock.style_blocks_by_id.remove(&removed.block_id).as_deref(),
                        Some("builtin:button")
                    );
                }
                _ => unreachable!(),
            }
            write_install_lock(root, &lock);

            let doctor = build_doctor_output(root, true, false, false);
            assert_eq!(doctor_status(&doctor), CommandStatus::Error);
            let (target_check, index_check) = if target_kind == "file" {
                ("lock_file_targets", "lock_files_by_path")
            } else {
                ("lock_style_targets", "lock_style_blocks_by_id")
            };
            assert_doctor_check(
                &doctor,
                target_check,
                DoctorCheckStatus::Fail,
                "differ from the registry snapshot",
            );
            assert_doctor_check(
                &doctor,
                index_check,
                DoctorCheckStatus::Fail,
                "differs from registry target ownership",
            );
        }
    }

    #[test]
    fn doctor_rejects_extra_duplicate_and_misowned_lock_targets() {
        for mutation in [
            "extra_file",
            "duplicate_file",
            "misowned_file",
            "extra_style",
            "duplicate_style",
            "misowned_style",
        ] {
            let dir = tempdir().expect("tempdir");
            let root = dir.path();
            create_current_button_install(root, DEFAULT_CSS_PATH);
            let mut lock = read_install_lock(root);

            match mutation {
                "extra_file" => {
                    let generated_hash = format!("sha256:{}", "1".repeat(64));
                    lock.items
                        .get_mut("builtin:button")
                        .expect("button item")
                        .files
                        .push(InstalledFile {
                            path: "src/components/ui/extra.rs".to_owned(),
                            kind: "rust".to_owned(),
                            generated_hash: generated_hash.clone(),
                            local_hash_at_install: generated_hash,
                        });
                    lock.files_by_path.insert(
                        "src/components/ui/extra.rs".to_owned(),
                        "builtin:button".to_owned(),
                    );
                }
                "duplicate_file" => {
                    let duplicate = lock.items["builtin:button"].files[0].clone();
                    lock.items
                        .get_mut("builtin:button")
                        .expect("button item")
                        .files
                        .push(duplicate);
                }
                "misowned_file" => {
                    let path = lock.items["builtin:spinner"].files[0].path.clone();
                    lock.files_by_path.insert(path, "builtin:button".to_owned());
                }
                "extra_style" => {
                    lock.items
                        .get_mut("builtin:button")
                        .expect("button item")
                        .style_blocks
                        .push(InstalledStyleBlock {
                            css_path: DEFAULT_CSS_PATH.to_owned(),
                            block_id: "extra".to_owned(),
                            generated_hash: format!("sha256:{}", "4".repeat(64)),
                        });
                    lock.style_blocks_by_id
                        .insert("extra".to_owned(), "builtin:button".to_owned());
                }
                "duplicate_style" => {
                    let duplicate = lock.items["builtin:button"].style_blocks[0].clone();
                    lock.items
                        .get_mut("builtin:button")
                        .expect("button item")
                        .style_blocks
                        .push(duplicate);
                }
                "misowned_style" => {
                    lock.style_blocks_by_id
                        .insert("spinner".to_owned(), "builtin:button".to_owned());
                }
                _ => unreachable!(),
            }
            write_install_lock(root, &lock);

            let doctor = build_doctor_output(root, true, false, false);
            assert_eq!(doctor_status(&doctor), CommandStatus::Error);
            let (check, message) = match mutation {
                "extra_file" => ("lock_file_targets", "differ from the registry snapshot"),
                "duplicate_file" => ("lock", ".path \"src/components/ui/button.rs\" duplicates"),
                "misowned_file" => (
                    "lock",
                    "filesByPath[\"src/components/ui/spinner.rs\"] is owned",
                ),
                "extra_style" => (
                    "lock",
                    "styleBlocks must contain at most one UI style block",
                ),
                "duplicate_style" => ("lock", ".blockId \"button\" duplicates"),
                "misowned_style" => ("lock", "styleBlocksById[\"spinner\"] is owned"),
                _ => unreachable!(),
            };
            assert_doctor_check(&doctor, check, DoctorCheckStatus::Fail, message);
        }
    }

    #[test]
    fn doctor_rejects_stale_item_and_lock_metadata() {
        for mutation in ["wrong_version", "stale_content", "stale_kind"] {
            let dir = tempdir().expect("tempdir");
            let root = dir.path();
            create_current_button_install(root, DEFAULT_CSS_PATH);
            let mut lock = read_install_lock(root);
            match mutation {
                "wrong_version" => lock.kit_version = "0.8.0".to_owned(),
                "stale_content" => {
                    lock.items
                        .get_mut("builtin:button")
                        .expect("button item")
                        .content_hash = format!("sha256:{}", "2".repeat(64));
                }
                "stale_kind" => {
                    lock.items
                        .get_mut("builtin:tokens")
                        .expect("tokens item")
                        .kind = leptos_ui_kit_registry::RegistryItemKind::Ui;
                }
                _ => unreachable!(),
            }
            write_install_lock(root, &lock);

            let doctor = build_doctor_output(root, true, false, false);
            assert_eq!(doctor_status(&doctor), CommandStatus::Error);
            match mutation {
                "wrong_version" => assert_doctor_check(
                    &doctor,
                    "lock",
                    DoctorCheckStatus::Fail,
                    "kitVersion must be",
                ),
                "stale_content" => assert_doctor_check(
                    &doctor,
                    "lock_item_metadata",
                    DoctorCheckStatus::Fail,
                    "builtin:button differs",
                ),
                "stale_kind" => assert_doctor_check(
                    &doctor,
                    "lock_item_metadata",
                    DoctorCheckStatus::Fail,
                    "builtin:tokens differs",
                ),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn codegen_and_doctor_use_the_same_install_lock_verdict() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        let mut lock = read_install_lock(root);
        lock.files_by_path.remove("src/components/ui/button.rs");
        let direct_reason = match lock
            .validate()
            .expect_err("missing reverse index must fail")
        {
            leptos_ui_kit_codegen::CodegenError::InvalidLock { reason, .. } => reason,
            error => panic!("unexpected direct validator error: {error}"),
        };
        write_install_lock(root, &lock);

        let doctor = build_doctor_output(root, true, false, false);

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert_doctor_check(&doctor, "lock", DoctorCheckStatus::Fail, &direct_reason);
    }

    #[test]
    fn doctor_rejects_each_managed_css_dependency_order_inversion() {
        for (block_id, expected_message) in [
            ("tokens", "managed CSS dependency tokens must precede"),
            (
                "spinner",
                "managed CSS dependency spinner must precede button",
            ),
        ] {
            let dir = tempdir().expect("tempdir");
            let root = dir.path();
            create_current_button_install(root, DEFAULT_CSS_PATH);
            move_managed_css_block_to_end(root, DEFAULT_CSS_PATH, block_id);

            let doctor = build_doctor_output(root, true, false, false);
            assert_eq!(doctor_status(&doctor), CommandStatus::Error);
            assert_doctor_check(
                &doctor,
                "managed_css_order",
                DoctorCheckStatus::Fail,
                expected_message,
            );
            assert!(doctor.checks.iter().any(|check| {
                check.name == "managed_css"
                    && check.status == DoctorCheckStatus::Pass
                    && check.message.contains(block_id)
            }));
        }
    }

    #[test]
    fn doctor_treats_config_item_order_as_nonsemantic() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        let config_path = root.join(DEFAULT_KIT_CONFIG_PATH);
        let mut config =
            parse_kit_json_str(&fs::read_to_string(&config_path).expect("read installed config"))
                .expect("parse installed config");
        config.items.reverse();
        fs::write(
            &config_path,
            kit_config_to_json(&config).expect("serialize reordered config"),
        )
        .expect("write reordered config");

        let doctor = build_doctor_output(root, true, false, false);
        assert_eq!(doctor_status(&doctor), CommandStatus::Success);
        assert_doctor_check(
            &doctor,
            "config_closure",
            DoctorCheckStatus::Pass,
            "equals the resolved registry closure",
        );
        assert_doctor_check(
            &doctor,
            "config_hash",
            DoctorCheckStatus::Pass,
            "nonsemantic JSON formatting or item ordering",
        );
    }

    #[test]
    fn doctor_tokens_only_cargo_plan_does_not_fall_back_to_stale_lock_items() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_doctor_project(root);
        apply_init(root).expect("init project");
        apply_add(root, "tokens").expect("install tokens");

        let router = load_built_in_registry_item("router-link").expect("load router-link");
        let mut lock = read_install_lock(root);
        lock.items.insert(
            "builtin:router-link".to_owned(),
            InstalledItem {
                id: "builtin:router-link".to_owned(),
                name: "router-link".to_owned(),
                kind: leptos_ui_kit_registry::RegistryItemKind::Ui,
                source: "builtin".to_owned(),
                version: router.item.version,
                content_hash: router.content_hash,
                files: Vec::new(),
                style_blocks: Vec::new(),
            },
        );
        write_install_lock(root, &lock);

        let doctor = build_doctor_output(root, true, false, false);
        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert_doctor_check(
            &doctor,
            "lock_closure",
            DoctorCheckStatus::Fail,
            "extra [builtin:router-link]",
        );
        assert!(
            !doctor
                .checks
                .iter()
                .any(|check| check.name.starts_with("dependency.registry."))
        );
    }

    #[test]
    fn doctor_router_link_cargo_plan_requires_router_from_resolved_closure() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_doctor_project(root);
        apply_init(root).expect("init project");
        apply_add(root, "router-link").expect("install router-link closure");

        let doctor = build_doctor_output(root, true, false, false);
        assert_eq!(doctor_status(&doctor), CommandStatus::Success);
        assert_doctor_check(
            &doctor,
            "dependency.registry.leptos_router",
            DoctorCheckStatus::Pass,
            "satisfies registry plan",
        );
        assert!(
            !doctor
                .checks
                .iter()
                .any(|check| check.name == "dependency.registry.web_ui_primitives")
        );
    }

    #[test]
    fn doctor_missing_config_fallback_is_ordinary_only() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        fs::remove_file(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("remove config");

        let ordinary = build_doctor_output(root, false, false, false);
        assert_eq!(doctor_status(&ordinary), CommandStatus::Warning);
        assert_doctor_check(
            &ordinary,
            "registry.snapshot",
            DoctorCheckStatus::Warning,
            "using lock-derived registry closure",
        );

        let strict = build_doctor_output(root, true, false, false);
        assert_eq!(doctor_status(&strict), CommandStatus::Error);
        assert_doctor_check(
            &strict,
            "config",
            DoctorCheckStatus::Fail,
            "kit.json is missing",
        );
        assert!(
            !strict
                .checks
                .iter()
                .any(|check| check.name == "registry.snapshot")
        );
    }

    #[test]
    fn doctor_malformed_config_never_falls_back_to_lock() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        fs::write(root.join(DEFAULT_KIT_CONFIG_PATH), "{\n").expect("write malformed config");

        let ordinary = build_doctor_output(root, false, false, false);
        assert_eq!(doctor_status(&ordinary), CommandStatus::Warning);
        assert_doctor_check(
            &ordinary,
            "project",
            DoctorCheckStatus::Warning,
            "failed to parse kit.json",
        );
        assert!(
            !ordinary
                .checks
                .iter()
                .any(|check| check.name == "registry.snapshot")
        );

        let strict = build_doctor_output(root, true, false, false);
        assert_eq!(doctor_status(&strict), CommandStatus::Error);
        assert_doctor_check(
            &strict,
            "project",
            DoctorCheckStatus::Fail,
            "failed to parse kit.json",
        );
    }

    #[test]
    fn doctor_malformed_lock_is_warning_ordinary_and_failure_strict() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        fs::write(root.join(DEFAULT_KIT_LOCK_PATH), "{\n").expect("write malformed lock");

        let ordinary = build_doctor_output(root, false, false, false);
        assert_eq!(doctor_status(&ordinary), CommandStatus::Warning);
        assert_doctor_check(
            &ordinary,
            "lock",
            DoctorCheckStatus::Warning,
            "failed to parse",
        );

        let strict = build_doctor_output(root, true, false, false);
        assert_eq!(doctor_status(&strict), CommandStatus::Error);
        assert_doctor_check(&strict, "lock", DoctorCheckStatus::Fail, "failed to parse");
    }

    #[test]
    fn doctor_local_file_drift_warns_ordinary_and_fails_strict() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        let path = root.join("src/components/ui/button.rs");
        let mut source = fs::read_to_string(&path).expect("read installed button");
        source.push_str("\n// local edit\n");
        fs::write(&path, source).expect("write local edit");

        let ordinary = build_doctor_output(root, false, false, false);
        assert_eq!(doctor_status(&ordinary), CommandStatus::Warning);
        assert_doctor_check(
            &ordinary,
            "installed_file",
            DoctorCheckStatus::Warning,
            "differs from the registry snapshot",
        );

        let strict = build_doctor_output(root, true, false, false);
        assert_eq!(doctor_status(&strict), CommandStatus::Error);
        assert_doctor_check(
            &strict,
            "installed_file",
            DoctorCheckStatus::Fail,
            "differs from the registry snapshot",
        );
    }

    #[test]
    fn doctor_lock_fallback_resolution_errors_are_not_swallowed() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        let mut lock = read_install_lock(root);
        lock.items.insert(
            "builtin:missing-item".to_owned(),
            InstalledItem {
                id: "builtin:missing-item".to_owned(),
                name: "missing-item".to_owned(),
                kind: leptos_ui_kit_registry::RegistryItemKind::Ui,
                source: "builtin".to_owned(),
                version: SCHEMA_VERSION.to_owned(),
                content_hash: format!("sha256:{}", "3".repeat(64)),
                files: Vec::new(),
                style_blocks: Vec::new(),
            },
        );
        write_install_lock(root, &lock);
        fs::remove_file(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("remove config");

        let ordinary = build_doctor_output(root, false, false, false);
        assert_eq!(doctor_status(&ordinary), CommandStatus::Warning);
        assert_doctor_check(
            &ordinary,
            "registry.snapshot",
            DoctorCheckStatus::Warning,
            "failed to resolve lock-derived registry closure",
        );

        let strict = build_doctor_output(root, true, false, false);
        assert_eq!(doctor_status(&strict), CommandStatus::Error);
        assert!(
            !strict
                .checks
                .iter()
                .any(|check| check.name == "registry.snapshot")
        );
    }

    #[test]
    fn doctor_ambiguous_lock_only_stylesheets_warn_and_skip_css_inspection() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        let mut lock = read_install_lock(root);
        lock.items
            .get_mut("builtin:button")
            .expect("button item")
            .style_blocks[0]
            .css_path = "styles/other.css".to_owned();
        write_install_lock(root, &lock);
        fs::remove_file(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("remove config");

        let doctor = build_doctor_output(root, false, false, false);
        assert_eq!(doctor_status(&doctor), CommandStatus::Warning);
        assert_doctor_check(
            &doctor,
            "registry.snapshot",
            DoctorCheckStatus::Warning,
            "spans multiple stylesheet paths",
        );
        assert!(!doctor.checks.iter().any(|check| {
            check.name == "managed_css"
                || check.name == "managed_css_closure"
                || check.name == "managed_css_order"
        }));
    }

    #[test]
    fn init_and_doctor_share_the_exact_html_stylesheet_verdict() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_current_button_install(root, DEFAULT_CSS_PATH);
        let cases = [
            (
                "present",
                "<HTML><HeAd><LiNk DATA-TRUNK REL='preload CSS' HREF='styles/kit&#46;css'></HeAd></HTML>",
                "present",
            ),
            (
                "missing",
                "<html><head></head><body></body></html>",
                "missing",
            ),
            (
                "body-only",
                "<html><head></head><body><link data-trunk rel=\"css\" href=\"styles/kit.css\"></body></html>",
                "unsafe",
            ),
            (
                "duplicate",
                "<head><link data-trunk rel=\"css\" href=\"styles/kit.css\"><link data-trunk rel=\"css\" href=\"styles/kit.css\"></head>",
                "unsafe",
            ),
            (
                "malformed",
                "<head><link data-trunk rel=\"css\" href=\"styles/kit.css></head>",
                "unsafe",
            ),
        ];

        for (name, html, expected) in cases {
            fs::write(root.join("index.html"), html).expect("write index");
            let plan = plan_init(root);
            let doctor = build_doctor_output(root, true, false, false);
            match expected {
                "present" => {
                    assert!(
                        !plan
                            .expect("present link plan")
                            .files
                            .iter()
                            .any(|file| file.path == "index.html"),
                        "{name}"
                    );
                    assert_doctor_check(
                        &doctor,
                        "stylesheet_link",
                        DoctorCheckStatus::Pass,
                        "links",
                    );
                }
                "missing" => {
                    assert!(
                        plan.expect("missing link is patchable")
                            .files
                            .iter()
                            .any(|file| file.path == "index.html"),
                        "{name}"
                    );
                    assert_doctor_check(
                        &doctor,
                        "stylesheet_link",
                        DoctorCheckStatus::Fail,
                        "missing",
                    );
                }
                "unsafe" => {
                    assert!(
                        matches!(
                            plan,
                            Err(leptos_ui_kit_codegen::CodegenError::UnsafePatch { .. })
                        ),
                        "{name}"
                    );
                    assert_doctor_check(
                        &doctor,
                        "stylesheet_link",
                        DoctorCheckStatus::Fail,
                        "cannot be inspected safely",
                    );
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn unsupported_flags_return_usage_errors() {
        let error = run(
            vec![
                OsString::from("view"),
                OsString::from("button"),
                OsString::from("--tailwind"),
            ],
            Path::new("."),
        )
        .expect_err("tailwind flag should be unsupported");

        assert_eq!(error.command, "view");
        assert_eq!(error.category, ErrorCategory::Usage);
        assert_eq!(error.code, "cli.unsupported_flag");
        assert_eq!(error.exit_code(), 2);
        assert!(error.message.contains("unsupported flag for view"));
    }

    #[test]
    fn help_and_version_flags_return_success() {
        run(vec![OsString::from("--help")], Path::new(".")).expect("top-level help");
        run(vec![OsString::from("--version")], Path::new(".")).expect("version");
        run(
            vec![OsString::from("--version"), OsString::from("--json")],
            Path::new("."),
        )
        .expect("json version");
        run(
            vec![OsString::from("sync"), OsString::from("--help")],
            Path::new("."),
        )
        .expect("command help");
    }

    #[test]
    fn help_and_version_do_not_require_a_process_directory() {
        let unavailable = || {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "process directory was removed",
            ))
        };

        run_from_environment(vec![OsString::from("--help")], unavailable)
            .expect("help without cwd");
        run_from_environment(vec![OsString::from("--version")], unavailable)
            .expect("version without cwd");
        run_from_environment(
            vec![OsString::from("doctor"), OsString::from("--help")],
            unavailable,
        )
        .expect("command help without cwd");
    }

    #[test]
    fn project_commands_report_typed_current_directory_failures() {
        let error = run_from_environment(vec![OsString::from("info")], || {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cwd unavailable",
            ))
        })
        .expect_err("info should require cwd");

        assert_eq!(error.command, "info");
        assert_eq!(error.category, ErrorCategory::Operational);
        assert_eq!(error.code, "cwd.unavailable");
        assert_eq!(error.exit_code(), 1);
        assert!(error.source.is_some());
        assert!(
            error
                .to_string()
                .contains("failed to acquire current directory")
        );
    }

    #[test]
    fn info_resolves_relative_paths_against_effective_global_cwd() {
        let dir = tempdir().expect("tempdir");
        let effective_cwd = dir.path().join("effective");
        fs::create_dir(&effective_cwd).expect("create effective cwd");

        let relative = run(
            vec![OsString::from("info"), OsString::from("nested")],
            &effective_cwd,
        )
        .expect_err("missing relative project should fail");
        assert_eq!(relative.command, "info");
        assert_eq!(relative.code, "project.missing_manifest");
        assert_eq!(relative.logical_path.as_deref(), Some("Cargo.toml"));

        let absolute_target = dir.path().join("absolute");
        let absolute = run(
            vec![
                OsString::from("info"),
                absolute_target.as_os_str().to_owned(),
            ],
            &effective_cwd,
        )
        .expect_err("missing absolute project should fail");
        assert_eq!(absolute.command, "info");
        assert_eq!(absolute.code, "project.missing_manifest");
        assert_eq!(absolute.logical_path.as_deref(), Some("Cargo.toml"));
    }

    #[test]
    fn version_outputs_known_tool_provenance_in_human_and_json_modes() {
        let human = render_version_output_with_tool(false, Ok(test_tool_config(TEST_TOOL_REV)))
            .expect("render human version");
        assert_eq!(human, "leptos_ui_kit 0.1.0");

        let output = render_version_output_with_tool(true, Ok(test_tool_config(TEST_TOOL_REV)))
            .expect("render JSON version");
        let value = serde_json::from_str::<serde_json::Value>(&output).expect("parse version JSON");

        assert_eq!(
            value,
            serde_json::json!({
                "schemaVersion": "0.9.0-alpha",
                "command": "version",
                "status": "success",
                "diagnostics": [],
                "changes": [],
                "data": {
                    "package": "leptos_ui_kit_cli",
                    "binary": "leptos_ui_kit",
                    "version": "0.1.0",
                    "schemaVersion": "0.9.0-alpha",
                    "source": {
                        "kind": "git",
                        "url": "https://github.com/triesap/leptos_ui_kit",
                        "rev": TEST_TOOL_REV
                    }
                }
            })
        );
        assert_eq!(value["command"], "version");
        assert_eq!(value["status"], "success");
        assert_eq!(value["schemaVersion"], "0.9.0-alpha");
        assert_eq!(value["data"]["package"], "leptos_ui_kit_cli");
        assert_eq!(value["data"]["binary"], "leptos_ui_kit");
        assert_eq!(value["data"]["version"], "0.1.0");
        assert_eq!(value["data"]["schemaVersion"], "0.9.0-alpha");
        assert_eq!(value["data"]["source"]["kind"], "git");
        assert_eq!(
            value["data"]["source"]["url"],
            "https://github.com/triesap/leptos_ui_kit"
        );
        assert_eq!(value["data"]["source"].get("resolution"), None);
        assert_eq!(value["data"]["source"]["rev"], TEST_TOOL_REV);
    }

    #[test]
    fn version_outputs_unavailable_provenance_honestly_in_human_and_json_modes() {
        let human = render_version_output_with_tool(false, missing_tool_provenance())
            .expect("render human version without provenance");
        assert_eq!(human, "leptos_ui_kit 0.1.0");

        let output = render_version_output_with_tool(true, missing_tool_provenance())
            .expect("render JSON version without provenance");
        let value = serde_json::from_str::<serde_json::Value>(&output).expect("parse version JSON");
        assert_eq!(value["command"], "version");
        assert_eq!(value["status"], "success");
        assert_eq!(value["data"]["source"]["kind"], "git");
        assert_eq!(
            value["data"]["source"]["url"],
            "https://github.com/triesap/leptos_ui_kit"
        );
        assert_eq!(value["data"]["source"].get("rev"), None);
    }

    #[test]
    fn version_rejects_invalid_compiled_provenance_without_fabricating_a_revision() {
        let error = render_version_output_with_tool(
            true,
            Err(ConfigError::InvalidValue {
                field: "tool.source.rev",
                expected: "40-character git commit hash",
                actual: "short".to_owned(),
            }),
        )
        .expect_err("invalid provenance must fail");

        assert!(error.contains("invalid compiled tool provenance"));
        assert!(error.contains("tool.source.rev"));
    }

    #[test]
    fn version_rejects_unknown_flags() {
        let error = run(
            vec![OsString::from("--version"), OsString::from("--source")],
            Path::new("."),
        )
        .expect_err("version flag should be unsupported");

        assert_eq!(error.category, ErrorCategory::Usage);
        assert_eq!(error.code, "cli.unsupported_flag");
        assert!(error.message.contains("unsupported flag for version"));
    }

    fn test_tool_config(rev: &str) -> ToolConfig {
        ToolConfig {
            package: TOOL_PACKAGE.to_owned(),
            binary: TOOL_BINARY.to_owned(),
            source: ToolSourceConfig::Git {
                url: TOOL_GIT_URL.to_owned(),
                rev: rev.to_owned(),
            },
        }
    }

    fn missing_tool_provenance() -> Result<ToolConfig, ConfigError> {
        Err(ConfigError::MissingToolProvenance {
            package: TOOL_PACKAGE,
            binary: TOOL_BINARY,
        })
    }

    #[test]
    fn common_flags_are_accepted_before_dispatch() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
leptos_router = "0.9.0-alpha"
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::create_dir(root.join("styles")).expect("create styles");
        fs::write(root.join("styles/kit.css"), ":root {}\n").expect("write css");
        fs::write(
            root.join("index.html"),
            r#"<!DOCTYPE html>
<html>
  <head>
    <link data-trunk rel="css" href="styles/kit.css" />
  </head>
  <body></body>
</html>
"#,
        )
        .expect("write index");

        run(
            vec![
                OsString::from("--cwd"),
                root.as_os_str().to_owned(),
                OsString::from("--quiet"),
                OsString::from("--verbose"),
                OsString::from("info"),
                OsString::from("--json"),
            ],
            Path::new("."),
        )
        .expect("run info with common flags");
    }

    #[test]
    fn exit_code_mapping_matches_contract() {
        let usage = CliError::usage(
            "view",
            false,
            "cli.unsupported_flag",
            "wording is irrelevant",
        );
        let doctor = CliError::doctor_failed(true);
        let root = Path::new("/project");
        let conflict = CliError::from_codegen(
            "add",
            false,
            root,
            "different wording",
            CodegenError::UnsafePatch {
                path: root.join("src/components/ui/button.rs"),
                reason: "different detail".to_owned(),
            },
        );
        let unsafe_path = CliError::from_codegen(
            "init",
            true,
            root,
            "different wording",
            CodegenError::UnsafePath {
                path: "../evil.rs".to_owned(),
                reason: "different detail".to_owned(),
            },
        );
        let registry = CliError::from_registry(
            "view",
            true,
            "nope",
            RegistryError::BuiltInNotFound("nope".to_owned()),
        );
        let operational = CliError::operational(
            "info",
            false,
            "project.inspect",
            "different wording",
            None,
            None,
        );

        assert_eq!(usage.exit_code(), 2);
        assert_eq!(doctor.exit_code(), 3);
        assert_eq!(conflict.exit_code(), 10);
        assert_eq!(unsafe_path.exit_code(), 11);
        assert_eq!(registry.exit_code(), 12);
        assert_eq!(operational.exit_code(), 1);
        assert_eq!(conflict.category, ErrorCategory::Conflict);
        assert_eq!(
            conflict.logical_path.as_deref(),
            Some("src/components/ui/button.rs")
        );
        assert!(conflict.suggestion.is_some());
        assert!(conflict.source.is_some());
        assert_eq!(unsafe_path.category, ErrorCategory::UnsafePath);
        assert_eq!(registry.category, ErrorCategory::RegistryPackage);
    }

    #[test]
    fn registry_diagnostics_never_expose_physical_locators() {
        for path in [
            "/private/build/registry/item.json",
            r"C:\private\build\registry\item.json",
            "../registry/item.json",
            "registry/../item.json",
        ] {
            let error = CliError::from_registry(
                "view",
                true,
                "button",
                RegistryError::UnsafePath {
                    field: "targets.uiFiles[].source",
                    path: path.to_owned(),
                },
            );
            let output = error.render_json();

            assert!(!output.contains(path), "{path}");
            assert!(!output.contains("private"), "{output}");
            assert_eq!(error.logical_path, None);
        }

        let logical = CliError::from_registry(
            "view",
            true,
            "button",
            RegistryError::DuplicateTarget("ui/button.rs".to_owned()),
        );
        assert_eq!(logical.logical_path.as_deref(), Some("ui/button.rs"));

        let packaged = CliError::from_codegen(
            "add",
            true,
            Path::new("/project"),
            "failed to load item",
            CodegenError::Registry(RegistryError::Io {
                path: PathBuf::from("/private/build/out/registry/item.json"),
                source: io::Error::new(io::ErrorKind::NotFound, "missing"),
            }),
        );
        let output = packaged.render_json();
        assert!(!output.contains("/private/build"), "{output}");
        assert!(output.contains("failed to read a packaged registry asset"));
    }

    #[test]
    fn human_change_wording_distinguishes_planned_applied_and_unchanged() {
        assert_eq!(change_verb(CommandStatus::Planned), "planned");
        assert_eq!(change_verb(CommandStatus::Success), "applied");
        assert_eq!(
            unchanged_label(CommandStatus::Planned),
            "no changes planned"
        );
        assert_eq!(unchanged_label(CommandStatus::NoChange), "no changes");
    }

    #[test]
    fn doctor_strict_failure_returns_doctor_error() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        let error = run(
            vec![
                OsString::from("doctor"),
                OsString::from("--strict"),
                OsString::from("--json"),
            ],
            root,
        )
        .expect_err("doctor should fail");

        assert_eq!(error.command, "doctor");
        assert_eq!(error.category, ErrorCategory::Doctor);
        assert_eq!(error.code, "doctor.checks_failed");
        assert_eq!(error.message.as_ref(), "doctor checks failed");
        assert_eq!(error.exit_code(), 3);
        assert!(error.output_emitted);
    }

    #[test]
    fn doctor_command_check_reports_missing_tools() {
        let dir = tempdir().expect("tempdir");
        let check = run_command_check(
            "build.fake",
            dir.path(),
            "leptos_ui_kit_definitely_missing_tool",
            &["build"],
        );

        assert_eq!(check.status, DoctorCheckStatus::Fail);
        assert!(check.message.contains("failed to run"));
    }

    #[test]
    fn doctor_selects_the_build_target_from_the_project_contract() {
        for kind in [
            ProjectKind::SingleCrateTrunkCsr,
            ProjectKind::SingleCrateBrowserHydration,
        ] {
            assert_eq!(
                doctor_cargo_check_args(Some(kind)),
                ["check", "--target", "wasm32-unknown-unknown"]
            );
        }
        for kind in [
            ProjectKind::SingleCrateNativeSsr,
            ProjectKind::SharedLibraryCrate,
        ] {
            assert_eq!(doctor_cargo_check_args(Some(kind)), ["check"]);
        }
        assert_eq!(doctor_cargo_check_args(None), ["check"]);
    }

    fn assert_doctor_check(
        doctor: &DoctorOutput,
        name: &str,
        status: DoctorCheckStatus,
        message: &str,
    ) {
        assert!(
            doctor.checks.iter().any(|check| {
                check.name == name && check.status == status && check.message.contains(message)
            }),
            "missing doctor check {name:?} with status {status:?} and message containing {message:?}; checks:\n{:#?}",
            doctor.checks
        );
    }

    fn move_managed_css_block_to_end(root: &Path, css_path: &str, block_id: &str) {
        let path = root.join(css_path);
        let css = fs::read_to_string(&path).expect("read stylesheet");
        let block = managed_css_block(&css, block_id);
        let mut reordered = remove_managed_css_block(css, block_id);
        if !reordered.ends_with('\n') {
            reordered.push('\n');
        }
        reordered.push_str(&block);
        fs::write(path, reordered).expect("write reordered stylesheet");
    }

    fn create_doctor_project(root: &Path) {
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
leptos_router = "0.9.0-alpha"
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
    }

    fn create_current_button_install(root: &Path, css_path: &str) {
        create_doctor_project(root);
        if css_path != DEFAULT_CSS_PATH {
            let mut config = canonical_kit_config().expect("canonical config");
            config.styles.css = css_path.to_owned();
            let config_path = root.join(DEFAULT_KIT_CONFIG_PATH);
            fs::create_dir_all(config_path.parent().expect("config parent"))
                .expect("create config parent");
            fs::write(
                config_path,
                kit_config_to_json(&config).expect("serialize custom config"),
            )
            .expect("write custom config");
        }

        apply_init(root).expect("initialize migration project");
        apply_add(root, "button").expect("install current button closure");
    }

    fn reconstruct_pinned_button_install(root: &Path, css_path: &str, with_overrides: bool) {
        assert_eq!(
            hash_content_bytes(PINNED_BUTTON_CSS.as_bytes()),
            "sha256:b9414172fc55c4d62e8b4ccd21c9c5d6427729e2ed30e2d5e1c5b808945dee46"
        );
        assert_eq!(
            hash_content_bytes(PINNED_SPINNER_CSS.as_bytes()),
            "sha256:736f9458ba25973db7371e02732ee9f87e02fe7d9e6686e94d76f52cfc26cd6d"
        );

        let stylesheet = root.join(css_path);
        let css = fs::read_to_string(&stylesheet).expect("read current stylesheet");
        let css = remove_managed_css_block(css, "tokens");
        let css = replace_managed_css_block(css, "spinner", PINNED_SPINNER_CSS);
        let mut css = replace_managed_css_block(css, "button", PINNED_BUTTON_CSS);
        if with_overrides {
            css.push_str(APP_TOKEN_OVERRIDES);
        }
        fs::write(&stylesheet, css).expect("write pinned stylesheet");

        remove_tokens_from_config_and_lock(root);
        let mut lock = read_install_lock(root);
        for (item_id, block_id, generated) in [
            ("builtin:spinner", "spinner", PINNED_SPINNER_CSS),
            ("builtin:button", "button", PINNED_BUTTON_CSS),
        ] {
            let item = lock.items.get_mut(item_id).expect("pinned lock item");
            let block = item
                .style_blocks
                .iter_mut()
                .find(|block| block.block_id == block_id)
                .expect("pinned lock style block");
            assert_eq!(block.css_path, css_path);
            block.generated_hash = hash_content_bytes(generated.as_bytes());
        }
        write_install_lock(root, &lock);
    }

    fn remove_tokens_from_config_and_lock(root: &Path) {
        let config_path = root.join(DEFAULT_KIT_CONFIG_PATH);
        let mut config =
            parse_kit_json_str(&fs::read_to_string(&config_path).expect("read installed config"))
                .expect("parse installed config");
        let old_len = config.items.len();
        config.items.retain(|item| item.item_name() != "tokens");
        assert_eq!(config.items.len() + 1, old_len);
        let config_json = kit_config_to_json(&config).expect("serialize legacy config");
        fs::write(&config_path, &config_json).expect("write legacy config");

        let mut lock = read_install_lock(root);
        assert!(lock.items.remove("builtin:tokens").is_some());
        assert_eq!(
            lock.style_blocks_by_id.remove("tokens").as_deref(),
            Some("builtin:tokens")
        );
        lock.project.config_hash = hash_content_bytes(config_json.as_bytes());
        write_install_lock(root, &lock);
    }

    fn managed_css_block(css: &str, block_id: &str) -> String {
        extract_managed_css_block(css, block_id)
            .unwrap_or_else(|error| panic!("inspect managed CSS block {block_id}: {error}"))
            .unwrap_or_else(|| panic!("missing managed CSS block {block_id}"))
    }

    fn remove_managed_css_block(mut css: String, block_id: &str) -> String {
        let block = managed_css_block(&css, block_id);
        let start = css.find(&block).expect("managed block source range");
        css.replace_range(start..start + block.len(), "");
        css
    }

    fn replace_managed_css_block(mut css: String, block_id: &str, replacement: &str) -> String {
        let block = managed_css_block(&css, block_id);
        let start = css.find(&block).expect("managed block source range");
        css.replace_range(start..start + block.len(), replacement);
        css
    }

    fn read_install_lock(root: &Path) -> InstallLock {
        let input =
            fs::read_to_string(root.join(DEFAULT_KIT_LOCK_PATH)).expect("read install lock");
        parse_install_lock_str_at_path(&input, Path::new(DEFAULT_KIT_LOCK_PATH))
            .expect("parse install lock")
    }

    fn write_install_lock(root: &Path, lock: &InstallLock) {
        let mut content =
            serde_json::to_string_pretty(lock).expect("serialize install-lock fixture");
        content.push('\n');
        fs::write(root.join(DEFAULT_KIT_LOCK_PATH), content).expect("write install lock");
    }

    fn current_registry_style(block_id: &str) -> String {
        let item = load_built_in_registry_item(block_id).expect("load style registry item");
        let target = item
            .targets
            .style_blocks
            .iter()
            .find(|target| target.id == block_id)
            .expect("registry style target");
        read_built_in_registry_source(&target.source).expect("read registry style source")
    }

    fn assert_current_button_install(root: &Path, css_path: &str, override_css: Option<&str>) {
        let css = fs::read_to_string(root.join(css_path)).expect("read migrated stylesheet");
        for block_id in ["tokens", "spinner", "button"] {
            assert_eq!(
                managed_css_block(&css, block_id),
                current_registry_style(block_id),
                "managed block {block_id} is not current"
            );
        }

        let tokens_at = css
            .find("/* leptos-ui-kit:start tokens */")
            .expect("tokens marker");
        let spinner_at = css
            .find("/* leptos-ui-kit:start spinner */")
            .expect("spinner marker");
        let button_at = css
            .find("/* leptos-ui-kit:start button */")
            .expect("button marker");
        assert!(tokens_at < spinner_at, "tokens must precede spinner");
        assert!(tokens_at < button_at, "tokens must precede button");
        assert!(spinner_at < button_at, "spinner must precede button");

        match override_css {
            Some(override_css) => {
                assert_eq!(css.matches(override_css).count(), 1);
                let override_at = css.find(override_css).expect("application override");
                assert!(
                    button_at < override_at,
                    "application overrides must remain last"
                );
            }
            None => assert!(!css.contains("application-owned token overrides")),
        }

        let config_input =
            fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read migrated config");
        let config = parse_kit_json_str(&config_input).expect("parse migrated config");
        assert_eq!(config.styles.css, css_path);
        assert_eq!(
            config
                .items
                .iter()
                .map(|item| item.item_name())
                .collect::<Vec<_>>(),
            ["tokens", "spinner", "button"]
        );

        let lock = read_install_lock(root);
        assert_eq!(
            lock.project.config_hash,
            hash_content_bytes(config_input.as_bytes())
        );
        assert_complete_button_lock(root, css_path, &css, &lock);
    }

    fn assert_complete_button_lock(root: &Path, css_path: &str, css: &str, lock: &InstallLock) {
        assert_eq!(
            lock.items.keys().map(String::as_str).collect::<Vec<_>>(),
            ["builtin:button", "builtin:spinner", "builtin:tokens"]
        );

        let mut expected_files_by_path = BTreeMap::new();
        let mut expected_styles_by_id = BTreeMap::new();
        for (item_id, item) in &lock.items {
            let registry =
                load_built_in_registry_item(&item.name).expect("load installed registry item");
            assert_eq!(item.id, *item_id);
            assert_eq!(item.source, "builtin");
            assert_eq!(item.version, SCHEMA_VERSION);
            assert_eq!(item.content_hash, registry.content_hash);

            let files = item
                .files
                .iter()
                .map(|file| (file.path.as_str(), file))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(files.len(), registry.targets.ui_files.len());
            for target in &registry.targets.ui_files {
                let logical_path = format!("src/components/ui/{}", target.path);
                let file = files
                    .get(logical_path.as_str())
                    .unwrap_or_else(|| panic!("missing lock target {logical_path}"));
                let generated = read_built_in_registry_source(&target.source)
                    .expect("read registry Rust source");
                let generated_hash = hash_content_bytes(generated.as_bytes());
                assert_eq!(file.kind, "rust");
                assert_eq!(file.generated_hash, generated_hash);
                assert_eq!(file.local_hash_at_install, generated_hash);
                assert_eq!(
                    hash_content_bytes(
                        &fs::read(root.join(&file.path)).expect("read installed Rust source")
                    ),
                    generated_hash
                );
                assert!(
                    expected_files_by_path
                        .insert(file.path.clone(), item_id.clone())
                        .is_none()
                );
            }

            let style_blocks = item
                .style_blocks
                .iter()
                .map(|block| (block.block_id.as_str(), block))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(style_blocks.len(), registry.targets.style_blocks.len());
            for target in &registry.targets.style_blocks {
                let block = style_blocks
                    .get(target.id.as_str())
                    .unwrap_or_else(|| panic!("missing lock style target {}", target.id));
                let generated = read_built_in_registry_source(&target.source)
                    .expect("read registry CSS source");
                assert_eq!(block.css_path, css_path);
                assert_eq!(
                    block.generated_hash,
                    hash_content_bytes(generated.as_bytes())
                );
                assert_eq!(managed_css_block(css, &target.id), generated);
                assert!(
                    expected_styles_by_id
                        .insert(block.block_id.clone(), item_id.clone())
                        .is_none()
                );
            }
        }

        assert_eq!(lock.files_by_path, expected_files_by_path);
        assert_eq!(lock.style_blocks_by_id, expected_styles_by_id);
    }

    fn assert_strict_doctor_success(root: &Path) {
        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");
        assert_eq!(
            doctor_status(&doctor),
            CommandStatus::Success,
            "strict doctor was not clean:\n{output}"
        );
    }

    fn init_git(root: &Path) {
        let output = Command::new("git")
            .arg("init")
            .current_dir(root)
            .output()
            .expect("run git init");

        assert!(
            output.status.success(),
            "git init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_desired_button_config(root: &Path) {
        let config = parse_kit_json_str(
            &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
        )
        .expect("parse config");
        let config = kit_config_with_desired_item(config, desired_builtin_button_item())
            .expect("add desired item");
        fs::write(
            root.join(DEFAULT_KIT_CONFIG_PATH),
            kit_config_to_json(&config).expect("serialize config"),
        )
        .expect("write config");
    }

    fn write_empty_items_config(root: &Path) {
        let mut config = parse_kit_json_str(
            &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
        )
        .expect("parse config");
        config.items.clear();
        fs::write(
            root.join(DEFAULT_KIT_CONFIG_PATH),
            kit_config_to_json(&config).expect("serialize config"),
        )
        .expect("write config");
    }
}
