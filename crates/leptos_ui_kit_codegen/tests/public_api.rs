#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    error::Error,
    fmt::{Debug, Display},
    fs,
    panic::{RefUnwindSafe, UnwindSafe},
    path::{Path, PathBuf},
};

use leptos_ui_kit_codegen::*;
use leptos_ui_kit_registry::{CargoPlanEntry, KitConfig};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

#[derive(Serialize)]
struct PublicData {
    value: &'static str,
}

#[derive(Debug, Serialize)]
struct DebugData;

#[derive(Clone, Serialize)]
struct CloneData;

#[derive(PartialEq, Serialize)]
struct PartialEqData;

#[derive(PartialEq, Eq, Serialize)]
struct EqData;

fn assert_debug<T: Debug>() {}
fn assert_display<T: Display>() {}
fn assert_error<T: Error>() {}
fn assert_clone<T: Clone>() {}
fn assert_partial_eq<T: PartialEq>() {}
fn assert_eq<T: Eq>() {}
fn assert_copy_eq<T: Copy + Eq>() {}
fn assert_partial_ord<T: PartialOrd>() {}
fn assert_ord<T: Ord>() {}
fn assert_serialize<T: Serialize>() {}
fn assert_send_sync_unpin<T: Send + Sync + Unpin>() {}
fn assert_auto_traits<T: Send + Sync + Unpin + UnwindSafe + RefUnwindSafe>() {}
fn assert_owned_traits<T: Debug + Clone + Eq + Send + Sync + Unpin + UnwindSafe + RefUnwindSafe>() {
}
fn assert_serializable_owned_traits<
    T: Debug + Clone + Eq + Serialize + Send + Sync + Unpin + UnwindSafe + RefUnwindSafe,
>() {
}
fn assert_serde_owned_traits<
    T: Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + UnwindSafe
        + RefUnwindSafe,
>() {
}

#[test]
#[allow(clippy::type_complexity)]
fn root_symbols_and_function_signatures_remain_available_downstream() {
    assert_eq!(
        DEFAULT_KIT_LOCK_PATH,
        "src/components/ui/_kit/kit.lock.json"
    );
    assert_eq!(
        DEFAULT_KIT_WRITE_LOCK_PATH,
        "src/components/ui/_kit/.write.lock"
    );

    let _: fn(&Path) -> Result<InitPlan, CodegenError> = plan_init;
    let _: fn(&Path) -> Result<InitPlan, CodegenError> = apply_init;
    let _: fn(&Path, &str) -> Result<AddPlan, CodegenError> = plan_add;
    let _: fn(&Path, &str) -> Result<AddPlan, CodegenError> = apply_add;
    let _: fn(&Path) -> Result<SyncPlan, CodegenError> = plan_sync;
    let _: fn(&Path) -> Result<SyncPlan, CodegenError> = apply_sync;
    let _: fn(&KitConfig) -> String = install_lock_path;
    let _: fn(&str) -> Result<InstallLock, CodegenError> = parse_install_lock_str;
    let _: fn(&str, &Path) -> Result<InstallLock, CodegenError> = parse_install_lock_str_at_path;
    let _: fn(&InstallLock) -> Result<String, CodegenError> = lock_to_json;
    let _: fn(&InstallLock, &Path) -> Result<String, CodegenError> = lock_to_json_at_path;
    let _: fn(&[u8]) -> String = hash_content_bytes;
    let _: fn(&[String]) -> Result<(), CodegenError> = validate_planned_write_paths;
    let _: fn(&str) -> Result<(), CodegenError> = validate_logical_write_path;
    let _: fn(&Path, &str) -> Result<PathBuf, CodegenError> = validate_project_write_path;
    let _: fn(&Path) -> Result<WriteLock, CodegenError> = WriteLock::acquire;
    let _: fn(&Path, &str, &[u8]) -> Result<(), CodegenError> = write_file_atomic;
    let _: fn(Option<&str>) -> Result<String, CodegenError> = patch_components_mod;
    let _: fn(Option<&str>, &[UiModuleExport]) -> Result<String, CodegenError> = patch_ui_mod;
    let _: fn(&str, &str, &str, Option<&str>) -> Result<String, CodegenError> = patch_css_block;
    let _: fn(&str, &str, &str, &str, Option<&str>) -> Result<String, CodegenError> =
        patch_css_block_at_path;
    let _: fn(&str, &str) -> Result<Option<String>, CodegenError> = extract_managed_css_block;
    let _: fn(&str, &str, &str) -> Result<Option<String>, CodegenError> =
        extract_managed_css_block_at_path;
    let _: fn(&str, &str) -> Result<BTreeMap<String, ManagedCssBlockRange>, CodegenError> =
        inspect_managed_css_blocks_at_path;
    let _: fn(
        &str,
        &str,
        &InstallLock,
        &[ManagedCssOperation],
        &[ManagedCssDependency],
    ) -> Result<String, CodegenError> = reconcile_managed_css_blocks_at_path;
}

#[test]
fn public_traits_associated_methods_and_fields_remain_available() {
    assert_debug::<CommandEnvelope<DebugData>>();
    assert_clone::<CommandEnvelope<CloneData>>();
    assert_partial_eq::<CommandEnvelope<PartialEqData>>();
    assert_eq::<CommandEnvelope<EqData>>();
    assert_serialize::<CommandEnvelope<PublicData>>();
    assert_auto_traits::<CommandEnvelope<PublicData>>();
    assert_serializable_owned_traits::<CommandStatus>();
    assert_copy_eq::<CommandStatus>();
    assert_serializable_owned_traits::<Diagnostic>();
    assert_serializable_owned_traits::<DiagnosticLevel>();
    assert_copy_eq::<DiagnosticLevel>();
    assert_serializable_owned_traits::<ChangeRecord>();
    assert_serializable_owned_traits::<ChangeKind>();
    assert_copy_eq::<ChangeKind>();
    assert_debug::<CodegenError>();
    assert_display::<CodegenError>();
    assert_error::<CodegenError>();
    assert_send_sync_unpin::<CodegenError>();
    assert_serializable_owned_traits::<InitPlan>();
    assert_serializable_owned_traits::<AddPlan>();
    assert_serializable_owned_traits::<SyncPlan>();
    assert_serializable_owned_traits::<PlannedFile>();
    assert_serializable_owned_traits::<PlannedFileAction>();
    assert_copy_eq::<PlannedFileAction>();
    assert_serde_owned_traits::<InstallLock>();
    assert_serde_owned_traits::<InstallLockProject>();
    assert_serde_owned_traits::<InstalledItem>();
    assert_serde_owned_traits::<InstalledFile>();
    assert_serde_owned_traits::<InstalledStyleBlock>();
    assert_owned_traits::<ManagedCssBlockRole>();
    assert_copy_eq::<ManagedCssBlockRole>();
    assert_owned_traits::<ManagedCssOperation>();
    assert_owned_traits::<ManagedCssDependency>();
    assert_partial_ord::<ManagedCssDependency>();
    assert_ord::<ManagedCssDependency>();
    assert_owned_traits::<ManagedCssBlockRange>();
    assert_owned_traits::<UiModuleExport>();
    assert_debug::<WriteLock>();
    assert_auto_traits::<WriteLock>();
    let _ = ManagedCssBlockRole::Foundation;
    let _ = ManagedCssBlockRole::Component;

    let _: fn(&InitPlan) -> bool = InitPlan::is_empty;
    let _: fn(&AddPlan) -> bool = AddPlan::is_empty;
    let _: fn(&SyncPlan) -> bool = SyncPlan::is_empty;
    let _: fn(String) -> InstallLock = InstallLock::empty;
    let _: fn(&InstallLock) -> Result<(), CodegenError> = InstallLock::validate;
    let _: fn(&InstallLock, &Path) -> Result<(), CodegenError> = InstallLock::validate_at_path;
    let _ = project_command_fields;
    let _ = project_plan_fields;
    let _ = project_lock_fields;
    let _ = project_css_fields;

    let _ = CommandEnvelope::new("info", CommandStatus::Planned, PublicData { value: "ok" })
        .with_diagnostics(Vec::new())
        .with_changes(Vec::new());
    let _ = CommandEnvelope::success("info", PublicData { value: "ok" });
    let _ = Diagnostic::new(DiagnosticLevel::Warning, "api.warning", "warning")
        .with_path("styles/kit.css")
        .with_suggestion("review the file");
    let _ = ChangeRecord::new(ChangeKind::CreateFile, "styles/kit.css", true)
        .with_item("builtin:tokens");
    let lock = InstallLock::empty(valid_hash('0'));
    lock.validate().expect("valid lock");
    lock.validate_at_path(Path::new(DEFAULT_KIT_LOCK_PATH))
        .expect("valid lock at path");
    let _ = UiModuleExport::new("button", vec!["Button".to_owned()]);
    let nested =
        UiModuleExport::with_path("nested", "nested::root", vec!["NestedButton".to_owned()]);
    assert_eq!(nested.path, "nested::root");
}

fn project_command_fields(
    envelope: &CommandEnvelope<PublicData>,
    diagnostic: &Diagnostic,
    change: &ChangeRecord,
) {
    let _: &'static str = envelope.schema_version;
    let _: &String = &envelope.command;
    let _: CommandStatus = envelope.status;
    let _: &Vec<Diagnostic> = &envelope.diagnostics;
    let _: &Vec<ChangeRecord> = &envelope.changes;
    let _: &PublicData = &envelope.data;

    let _: DiagnosticLevel = diagnostic.level;
    let _: &String = &diagnostic.code;
    let _: &String = &diagnostic.message;
    let _: &Option<String> = &diagnostic.path;
    let _: &Option<String> = &diagnostic.suggestion;

    let _: ChangeKind = change.kind;
    let _: &String = &change.path;
    let _: &Option<String> = &change.item;
    let _: bool = change.tracked;
}

fn project_plan_fields(init: &InitPlan, add: &AddPlan, sync: &SyncPlan, file: &PlannedFile) {
    let _: &PathBuf = &init.project_root;
    let _: &Vec<PlannedFile> = &init.files;
    let _: &Vec<ChangeRecord> = &init.changes;

    let _: &PathBuf = &add.project_root;
    let _: &String = &add.item_id;
    let _: &String = &add.item_name;
    let _: &String = &add.content_hash;
    let _: &Vec<CargoPlanEntry> = &add.cargo_plan;
    let _: &Vec<PlannedFile> = &add.files;
    let _: &Vec<ChangeRecord> = &add.changes;
    let _: &Vec<Diagnostic> = &add.diagnostics;
    let _: &InstallLock = &add.lock;

    let _: &PathBuf = &sync.project_root;
    let _: &Vec<String> = &sync.item_ids;
    let _: &Vec<CargoPlanEntry> = &sync.cargo_plan;
    let _: &Vec<PlannedFile> = &sync.files;
    let _: &Vec<ChangeRecord> = &sync.changes;
    let _: &Vec<Diagnostic> = &sync.diagnostics;
    let _: &InstallLock = &sync.lock;

    let _: &String = &file.path;
    let _: PlannedFileAction = file.action;
    let _: &String = &file.content;
}

fn project_lock_fields(
    lock: &InstallLock,
    project: &InstallLockProject,
    item: &InstalledItem,
    installed_file: &InstalledFile,
    style: &InstalledStyleBlock,
) {
    let _: &String = &lock.schema_version;
    let _: &String = &lock.kit_version;
    let _: &InstallLockProject = &lock.project;
    let _: &BTreeMap<String, InstalledItem> = &lock.items;
    let _: &BTreeMap<String, String> = &lock.files_by_path;
    let _: &BTreeMap<String, String> = &lock.style_blocks_by_id;

    let _: &String = &project.config_hash;
    let _: &String = &project.crate_root;
    let _: &String = &project.kind;

    let _: &String = &item.id;
    let _: &String = &item.name;
    let _: &String = &item.source;
    let _: &String = &item.version;
    let _: &String = &item.content_hash;
    let _: &Vec<InstalledFile> = &item.files;
    let _: &Vec<InstalledStyleBlock> = &item.style_blocks;

    let _: &String = &installed_file.path;
    let _: &String = &installed_file.kind;
    let _: &String = &installed_file.generated_hash;
    let _: &String = &installed_file.local_hash_at_install;

    let _: &String = &style.css_path;
    let _: &String = &style.block_id;
    let _: &String = &style.generated_hash;
}

fn project_css_fields(
    operation: &ManagedCssOperation,
    dependency: &ManagedCssDependency,
    range: &ManagedCssBlockRange,
    export: &UiModuleExport,
) {
    let _: &String = &operation.item_id;
    let _: &String = &operation.block_id;
    let _: ManagedCssBlockRole = operation.role;
    let _: &String = &operation.generated;

    let _: &String = &dependency.dependency_block_id;
    let _: &String = &dependency.dependent_block_id;

    let _: usize = range.start;
    let _: usize = range.end;

    let _: &String = &export.module;
    let _: &String = &export.path;
    let _: &Vec<String> = &export.symbols;
}

#[test]
fn public_serialized_names_and_enum_values_remain_stable() {
    let envelope = CommandEnvelope::success("info", PublicData { value: "ok" })
        .with_diagnostics(vec![Diagnostic::new(
            DiagnosticLevel::Info,
            "api.info",
            "ready",
        )])
        .with_changes(vec![ChangeRecord::new(
            ChangeKind::UpdateCssBlock,
            "styles/kit.css",
            true,
        )]);
    let envelope = serde_json::to_value(envelope).expect("serialize envelope");
    assert_eq!(envelope["schemaVersion"], "0.9.0-alpha");
    assert_eq!(envelope["command"], "info");
    assert_eq!(envelope["status"], "success");
    assert_eq!(envelope["diagnostics"][0]["level"], "info");
    assert_eq!(envelope["diagnostics"][0]["code"], "api.info");
    assert_eq!(envelope["diagnostics"][0]["message"], "ready");
    assert!(envelope["diagnostics"][0].get("path").is_none());
    assert!(envelope["diagnostics"][0].get("suggestion").is_none());
    assert_eq!(envelope["changes"][0]["kind"], "update_css_block");
    assert_eq!(envelope["changes"][0]["path"], "styles/kit.css");
    assert_eq!(envelope["changes"][0]["tracked"], true);
    assert!(envelope["changes"][0].get("item").is_none());
    assert_eq!(envelope["data"]["value"], "ok");

    let diagnostic = serde_json::to_value(
        Diagnostic::new(DiagnosticLevel::Warning, "api.warning", "review")
            .with_path("styles/kit.css")
            .with_suggestion("resolve the conflict"),
    )
    .expect("serialize diagnostic optionals");
    assert_eq!(diagnostic["path"], "styles/kit.css");
    assert_eq!(diagnostic["suggestion"], "resolve the conflict");

    let change = serde_json::to_value(
        ChangeRecord::new(ChangeKind::CreateFile, "styles/kit.css", true)
            .with_item("builtin:tokens"),
    )
    .expect("serialize change item");
    assert_eq!(change["item"], "builtin:tokens");

    let statuses = [
        (CommandStatus::Success, "success"),
        (CommandStatus::NoChange, "no_change"),
        (CommandStatus::Planned, "planned"),
        (CommandStatus::Warning, "warning"),
        (CommandStatus::Error, "error"),
        (CommandStatus::Conflict, "conflict"),
        (CommandStatus::Unsupported, "unsupported"),
    ];
    for (value, expected) in statuses {
        assert_eq!(serde_json::to_value(value).expect("status"), expected);
    }
    for (value, expected) in [
        (DiagnosticLevel::Info, "info"),
        (DiagnosticLevel::Warning, "warning"),
        (DiagnosticLevel::Error, "error"),
    ] {
        assert_eq!(serde_json::to_value(value).expect("level"), expected);
    }
    for (value, expected) in [
        (ChangeKind::CreateFile, "create_file"),
        (ChangeKind::UpdateFile, "update_file"),
        (ChangeKind::DeleteFile, "delete_file"),
        (ChangeKind::CreateDir, "create_dir"),
        (ChangeKind::UpdateCssBlock, "update_css_block"),
        (ChangeKind::WriteLockFile, "write_lock_file"),
    ] {
        assert_eq!(serde_json::to_value(value).expect("change kind"), expected);
    }
    for (value, expected) in [
        (PlannedFileAction::Create, "create"),
        (PlannedFileAction::Update, "update"),
    ] {
        assert_eq!(serde_json::to_value(value).expect("file action"), expected);
    }

    let value = serde_json::to_value(valid_empty_lock()).expect("serialize lock");
    assert_object_contains_keys(
        &value,
        &[
            "schemaVersion",
            "kitVersion",
            "project",
            "items",
            "filesByPath",
            "styleBlocksById",
        ],
    );
    assert_object_contains_keys(&value["project"], &["configHash", "crateRoot", "kind"]);
}

#[test]
fn public_plan_and_nested_lock_serialized_names_remain_stable() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path();
    fs::create_dir_all(root.join("src")).expect("create source directory");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");

    let init = plan_init(root).expect("plan init");
    let sync = plan_sync(root).expect("plan sync");
    let add = plan_add(root, "button").expect("plan add");

    let init = serde_json::to_value(init).expect("serialize init plan");
    assert_object_contains_keys(&init, &["projectRoot", "files", "changes"]);
    let planned_file = init["files"]
        .as_array()
        .and_then(|files| files.first())
        .expect("planned file");
    assert_object_contains_keys(planned_file, &["path", "action", "content"]);

    let sync = serde_json::to_value(sync).expect("serialize sync plan");
    assert_object_contains_keys(
        &sync,
        &[
            "projectRoot",
            "itemIds",
            "cargoPlan",
            "files",
            "changes",
            "diagnostics",
            "lock",
        ],
    );

    let add = serde_json::to_value(add).expect("serialize add plan");
    assert_object_contains_keys(
        &add,
        &[
            "projectRoot",
            "itemId",
            "itemName",
            "contentHash",
            "cargoPlan",
            "files",
            "changes",
            "diagnostics",
            "lock",
        ],
    );
    let item = &add["lock"]["items"]["builtin:button"];
    assert_object_contains_keys(
        item,
        &[
            "id",
            "name",
            "source",
            "version",
            "contentHash",
            "files",
            "styleBlocks",
        ],
    );
    assert_object_contains_keys(
        &item["files"][0],
        &["path", "kind", "generatedHash", "localHashAtInstall"],
    );
    assert_object_contains_keys(
        &item["styleBlocks"][0],
        &["cssPath", "blockId", "generatedHash"],
    );
}

#[test]
fn public_error_variants_and_conversion_bounds_remain_constructible() {
    fn assert_from<T, U>()
    where
        U: From<T>,
    {
    }
    assert_from::<leptos_ui_kit_registry::ConfigError, CodegenError>();
    assert_from::<leptos_ui_kit_registry::RegistryError, CodegenError>();

    let config = CodegenError::Config(
        leptos_ui_kit_registry::parse_kit_json_str("{}").expect_err("incomplete config must fail"),
    );
    assert!(matches!(config, CodegenError::Config(_)));
    let registry = CodegenError::Registry(
        leptos_ui_kit_registry::load_built_in_registry_item("not-a-built-in")
            .expect_err("unknown item must fail"),
    );
    assert!(matches!(registry, CodegenError::Registry(_)));

    let path = PathBuf::from("kit.json");
    let io = CodegenError::Io {
        path: path.clone(),
        source: std::io::Error::other("io"),
    };
    assert!(matches!(io, CodegenError::Io { path: value, source: _ } if value == path));
    let parse = CodegenError::LockParse {
        path: path.clone(),
        source: serde_json::from_str::<Value>("{").expect_err("invalid json"),
    };
    assert!(matches!(parse, CodegenError::LockParse { path: value, source: _ } if value == path));
    let serialize =
        CodegenError::LockSerialize(serde_json::from_str::<Value>("{").expect_err("invalid json"));
    assert!(matches!(serialize, CodegenError::LockSerialize(_)));
    let invalid = CodegenError::InvalidLock {
        path: path.clone(),
        reason: "invalid".to_owned(),
    };
    assert!(
        matches!(invalid, CodegenError::InvalidLock { path: value, reason } if value == path && reason == "invalid")
    );
    let patch = CodegenError::UnsafePatch {
        path: path.clone(),
        reason: "unsafe".to_owned(),
    };
    assert!(
        matches!(patch, CodegenError::UnsafePatch { path: value, reason } if value == path && reason == "unsafe")
    );
    let unsafe_path = CodegenError::UnsafePath {
        path: "../escape".to_owned(),
        reason: "unsafe".to_owned(),
    };
    assert!(
        matches!(unsafe_path, CodegenError::UnsafePath { path, reason } if path == "../escape" && reason == "unsafe")
    );
    assert!(matches!(
        CodegenError::DuplicatePath("dup".to_owned()),
        CodegenError::DuplicatePath(path) if path == "dup"
    ));
    assert!(matches!(
        CodegenError::LockExists(path.clone()),
        CodegenError::LockExists(value) if value == path
    ));
}

fn valid_empty_lock() -> InstallLock {
    InstallLock::empty(valid_hash('0'))
}

fn valid_hash(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

fn assert_object_contains_keys(value: &Value, expected: &[&str]) {
    let object = value.as_object().expect("object");
    for key in expected {
        assert!(object.contains_key(*key), "missing serialized key {key}");
    }
}
