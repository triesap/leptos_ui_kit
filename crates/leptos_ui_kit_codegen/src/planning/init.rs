use std::path::Path;

use leptos_ui_kit_registry::{
    ConfigError, DEFAULT_KIT_CONFIG_PATH, KIT_LAYER_ORDER_DECLARATION, KitConfig,
    canonical_kit_json, parse_kit_json_str,
};

use super::{empty_lock_json, planned_or_existing_kit_config_content, push_file_plan};
use crate::patch::plan_index_html;
use crate::path_safety::PlanningContext;
use crate::{
    ChangeKind, ChangeRecord, CodegenError, InitPlan, PlannedFile, PlannedFileAction,
    install_lock_path, patch_components_mod,
};

pub fn plan_init(project_root: &Path) -> Result<InitPlan, CodegenError> {
    plan_init_with_config_provider(project_root, canonical_kit_json)
}

pub(crate) fn plan_init_with_config_provider<F>(
    project_root: &Path,
    canonical_config: F,
) -> Result<InitPlan, CodegenError>
where
    F: FnOnce() -> Result<String, ConfigError>,
{
    crate::transaction::check_pending_recovery(project_root)?;
    let context = PlanningContext::open(project_root)?;
    plan_init_with_context(&context, project_root, canonical_config)
}

pub(crate) fn plan_init_with_context<F>(
    context: &PlanningContext,
    project_root: &Path,
    canonical_config: F,
) -> Result<InitPlan, CodegenError>
where
    F: FnOnce() -> Result<String, ConfigError>,
{
    let mut files = Vec::new();
    let mut changes = Vec::new();

    plan_kit_json(context, &mut files, &mut changes, canonical_config)?;
    let config_content = planned_or_existing_kit_config_content(context, &files)?;
    let config = parse_kit_json_str(&config_content)?;
    plan_stylesheet(context, &mut files, &mut changes, &config)?;
    plan_index_html(context, &mut files, &mut changes, &config)?;
    plan_component_modules(context, &mut files, &mut changes)?;
    plan_empty_state(context, &mut files, &mut changes)?;

    Ok(InitPlan {
        project_root: project_root.to_path_buf(),
        files,
        changes,
        snapshot: context.finish_snapshot(),
    })
}

fn plan_kit_json<F>(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    canonical_config: F,
) -> Result<(), CodegenError>
where
    F: FnOnce() -> Result<String, ConfigError>,
{
    if let Some(content) = context.read_optional_string(DEFAULT_KIT_CONFIG_PATH)? {
        parse_kit_json_str(&content)?;
        return Ok(());
    }

    push_file_plan(
        files,
        changes,
        DEFAULT_KIT_CONFIG_PATH,
        PlannedFileAction::Create,
        canonical_config()?,
        ChangeKind::CreateFile,
    );
    Ok(())
}

fn plan_stylesheet(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    config: &KitConfig,
) -> Result<(), CodegenError> {
    let css_path = config.styles.css.as_str();
    if context.read_optional_string(css_path)?.is_some() {
        return Ok(());
    }

    push_file_plan(
        files,
        changes,
        css_path,
        PlannedFileAction::Create,
        KIT_LAYER_ORDER_DECLARATION.to_owned(),
        ChangeKind::CreateFile,
    );
    Ok(())
}

fn plan_component_modules(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
) -> Result<(), CodegenError> {
    let components_mod = context.read_optional_string("src/components/mod.rs")?;
    if let Some(existing) = components_mod {
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
    } else {
        push_file_plan(
            files,
            changes,
            "src/components/mod.rs",
            PlannedFileAction::Create,
            patch_components_mod(None)?,
            ChangeKind::CreateFile,
        );
    }

    if context
        .read_optional_string("src/components/ui/mod.rs")?
        .is_none()
    {
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

fn plan_empty_state(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
) -> Result<(), CodegenError> {
    let config_content = planned_or_existing_kit_config_content(context, files)?;
    let config = parse_kit_json_str(&config_content)?;
    let state_path = install_lock_path(&config);
    if context.read_optional_string(&state_path)?.is_some() {
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
