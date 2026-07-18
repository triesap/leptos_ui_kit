use std::path::Path;

use leptos_ui_kit_registry::{canonical_kit_json, kit_config_for_write};

use crate::path_safety::PlanningContext;
use crate::planning::{plan_add_with_context, plan_init_with_context, plan_sync_with_context};
use crate::transaction::{
    JournalOperationV2, WriteLock, apply_planned_files_locked, recover_pending_locked,
};
use crate::{AddPlan, CodegenError, InitPlan, SyncPlan};

pub fn apply_init(project_root: &Path) -> Result<InitPlan, CodegenError> {
    let context = PlanningContext::open(project_root)?;
    let lock = WriteLock::acquire_with_context(&context)?;
    recover_pending_locked(&context, &lock)?;
    let plan = plan_init_with_context(&context, project_root, canonical_kit_json)?;
    apply_planned_files_locked(
        &context,
        &lock,
        &plan.files,
        &plan.changes,
        &plan.snapshot,
        JournalOperationV2::Init,
    )?;

    Ok(plan)
}

pub fn apply_add(project_root: &Path, item_name: &str) -> Result<AddPlan, CodegenError> {
    let context = PlanningContext::open(project_root)?;
    let lock = WriteLock::acquire_with_context(&context)?;
    recover_pending_locked(&context, &lock)?;
    let plan = plan_add_with_context(&context, project_root, item_name, kit_config_for_write)?;
    apply_planned_files_locked(
        &context,
        &lock,
        &plan.files,
        &plan.changes,
        &plan.snapshot,
        JournalOperationV2::Add,
    )?;

    Ok(plan)
}

pub fn apply_sync(project_root: &Path) -> Result<SyncPlan, CodegenError> {
    let context = PlanningContext::open(project_root)?;
    let lock = WriteLock::acquire_with_context(&context)?;
    recover_pending_locked(&context, &lock)?;
    let plan = plan_sync_with_context(&context, project_root, kit_config_for_write)?;
    apply_planned_files_locked(
        &context,
        &lock,
        &plan.files,
        &plan.changes,
        &plan.snapshot,
        JournalOperationV2::Sync,
    )?;

    Ok(plan)
}
