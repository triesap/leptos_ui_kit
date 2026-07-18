mod add;
mod files;
mod init;
mod sync;

pub(crate) use add::desired_builtin_item;
pub use add::plan_add;
#[cfg(test)]
pub(crate) use add::plan_add_with_config_writer;
pub(crate) use add::plan_add_with_context;
pub(crate) use files::*;
pub use init::plan_init;
#[cfg(test)]
pub(crate) use init::plan_init_with_config_provider;
pub(crate) use init::plan_init_with_context;
pub use sync::plan_sync;
pub(crate) use sync::plan_sync_with_context;
#[cfg(test)]
pub(crate) use sync::{plan_built_in_item, plan_sync_with_config_writer};
pub(crate) use sync::{plan_sync_from_config, prepare_kit_config_write};

use std::path::PathBuf;

use leptos_ui_kit_registry::{CargoPlanEntry, ConfigError, KitConfig};
use serde::Serialize;

use crate::PlanSnapshot;
use crate::{ChangeRecord, Diagnostic, InstallLock};

pub(crate) type KitConfigWriter = fn(KitConfig) -> Result<KitConfig, ConfigError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitPlan {
    pub project_root: PathBuf,
    pub files: Vec<PlannedFile>,
    pub changes: Vec<ChangeRecord>,
    #[serde(skip)]
    pub snapshot: PlanSnapshot,
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
    #[serde(skip)]
    pub snapshot: PlanSnapshot,
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
    #[serde(skip)]
    pub snapshot: PlanSnapshot,
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
