use std::path::Path;

use crate::transaction::apply_planned_files;
use crate::{AddPlan, CodegenError, InitPlan, SyncPlan, plan_add, plan_init, plan_sync};

pub fn apply_init(project_root: &Path) -> Result<InitPlan, CodegenError> {
    let plan = plan_init(project_root)?;
    apply_planned_files(project_root, &plan.files, &plan.changes)?;

    Ok(plan)
}

pub fn apply_add(project_root: &Path, item_name: &str) -> Result<AddPlan, CodegenError> {
    let plan = plan_add(project_root, item_name)?;
    apply_planned_files(project_root, &plan.files, &plan.changes)?;

    Ok(plan)
}

pub fn apply_sync(project_root: &Path) -> Result<SyncPlan, CodegenError> {
    let plan = plan_sync(project_root)?;
    apply_planned_files(project_root, &plan.files, &plan.changes)?;

    Ok(plan)
}
