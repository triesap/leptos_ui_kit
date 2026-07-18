#![forbid(unsafe_code)]

//! Code generation and install-planning layer.

mod command;
mod digest;
mod error;
mod install_lock;
mod orchestration;
mod patch;
mod path_safety;
mod planning;
mod transaction;

#[doc(inline)]
pub use command::{
    ChangeKind, ChangeRecord, CommandEnvelope, CommandStatus, Diagnostic, DiagnosticLevel,
};
#[doc(inline)]
pub use digest::hash_content_bytes;
#[doc(inline)]
pub use error::CodegenError;
#[doc(inline)]
pub use install_lock::{
    DEFAULT_KIT_LOCK_PATH, InstallLock, InstallLockProject, InstalledFile, InstalledItem,
    InstalledStyleBlock, ManagedCssBlockRange, ManagedCssBlockRole, ManagedCssDependency,
    ManagedCssOperation, install_lock_path, lock_to_json, lock_to_json_at_path,
    parse_install_lock_str, parse_install_lock_str_at_path,
};
#[doc(inline)]
pub use orchestration::{apply_add, apply_init, apply_sync};
#[doc(inline)]
pub use patch::{
    extract_managed_css_block, extract_managed_css_block_at_path,
    inspect_managed_css_blocks_at_path, patch_components_mod, patch_css_block,
    patch_css_block_at_path, patch_ui_mod, reconcile_managed_css_blocks_at_path,
};
#[doc(inline)]
pub use path_safety::{
    validate_logical_write_path, validate_planned_write_paths, validate_project_write_path,
};
#[doc(inline)]
pub use planning::{
    AddPlan, InitPlan, PlannedFile, PlannedFileAction, SyncPlan, UiModuleExport, plan_add,
    plan_init, plan_sync,
};
#[doc(inline)]
pub use transaction::{DEFAULT_KIT_WRITE_LOCK_PATH, WriteLock, write_file_atomic};

#[cfg(test)]
use digest::hash_bytes;
#[cfg(test)]
use planning::{
    built_in_item_id, desired_builtin_item, plan_add_with_config_writer, plan_built_in_item,
    plan_init_with_config_provider, plan_sync_with_config_writer,
};
#[cfg(test)]
use transaction::{FaultFs, FsEvent, FsOperation, apply_planned_files_with};

#[cfg(test)]
mod tests;
