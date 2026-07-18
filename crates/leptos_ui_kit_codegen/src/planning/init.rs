use std::path::Path;

use leptos_ui_kit_registry::{
    ConfigError, DEFAULT_KIT_CONFIG_PATH, KitConfig, canonical_kit_json, parse_kit_json_str,
};

use super::{
    empty_lock_json, planned_or_existing_kit_config_content, push_file_plan, read_to_string,
};
use crate::patch::plan_index_html;
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
    let mut files = Vec::new();
    let mut changes = Vec::new();

    plan_kit_json(project_root, &mut files, &mut changes, canonical_config)?;
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

fn plan_kit_json<F>(
    project_root: &Path,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    canonical_config: F,
) -> Result<(), CodegenError>
where
    F: FnOnce() -> Result<String, ConfigError>,
{
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
        canonical_config()?,
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
