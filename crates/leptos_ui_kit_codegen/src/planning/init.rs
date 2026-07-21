use std::path::Path;

use leptos_ui_kit_registry::{
    ConfigError, DEFAULT_KIT_CONFIG_PATH, KitConfig, canonical_kit_json, parse_kit_json_str,
};

use super::{
    empty_lock_json, planned_or_existing_kit_config_content, push_file_plan,
    read_canonical_install_lock, upsert_planned_install_lock,
};
use crate::patch::{patch_components_mod_at_path, plan_index_html};
use crate::path_safety::PlanningContext;
use crate::{
    ChangeKind, ChangeRecord, CodegenError, InitPlan, PlannedFile, PlannedFileAction,
    install_lock_path,
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
    plan_project_prerequisites_with_context(context, project_root, canonical_config, true)
}

pub(crate) fn plan_add_prerequisites_with_context<F>(
    context: &PlanningContext,
    project_root: &Path,
    canonical_config: F,
) -> Result<InitPlan, CodegenError>
where
    F: FnOnce() -> Result<String, ConfigError>,
{
    plan_project_prerequisites_with_context(context, project_root, canonical_config, false)
}

fn plan_project_prerequisites_with_context<F>(
    context: &PlanningContext,
    project_root: &Path,
    canonical_config: F,
    include_component_modules: bool,
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
    if include_component_modules {
        plan_component_modules(context, &mut files, &mut changes, &config)?;
    }
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
        String::new(),
        ChangeKind::CreateFile,
    );
    Ok(())
}

fn plan_component_modules(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    config: &KitConfig,
) -> Result<(), CodegenError> {
    let components_mod_path = config.install.components_mod.as_str();
    let ui_mod_path = config.install.ui_mod.as_str();
    let ui_module_name = config
        .install
        .ui_dir
        .rsplit('/')
        .next()
        .expect("validated UI directory has a final segment");
    let components_mod = context.read_optional_string(components_mod_path)?;
    if let Some(existing) = components_mod {
        let patched =
            patch_components_mod_at_path(Some(&existing), components_mod_path, ui_module_name)?;
        if patched != existing {
            push_file_plan(
                files,
                changes,
                components_mod_path,
                PlannedFileAction::Update,
                patched,
                ChangeKind::UpdateFile,
            );
        }
    } else {
        push_file_plan(
            files,
            changes,
            components_mod_path,
            PlannedFileAction::Create,
            patch_components_mod_at_path(None, components_mod_path, ui_module_name)?,
            ChangeKind::CreateFile,
        );
    }

    if context.read_optional_string(ui_mod_path)?.is_none() {
        push_file_plan(
            files,
            changes,
            ui_mod_path,
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
    let content = match read_canonical_install_lock(context, &state_path)? {
        Some((_, canonical)) => canonical,
        None => empty_lock_json(&config_content, &state_path)?,
    };
    let force_publication = !files.is_empty();
    upsert_planned_install_lock(context, files, changes, content, force_publication)
}
