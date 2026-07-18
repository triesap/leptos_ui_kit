use std::path::Path;

use leptos_ui_kit_registry::{
    DEFAULT_KIT_CONFIG_PATH, KitConfig, RegistryError, desired_builtin_anchor_item,
    desired_builtin_button_item, desired_builtin_collapsible_item, desired_builtin_dialog_item,
    desired_builtin_field_item, desired_builtin_menu_item, desired_builtin_router_link_item,
    desired_builtin_spinner_item, desired_builtin_status_item, desired_builtin_tabs_item,
    desired_builtin_tokens_item, kit_config_for_write, kit_config_to_json,
    kit_config_with_desired_item, load_built_in_registry_item, parse_kit_json_str,
    resolve_built_in_registry_items,
};

use super::{
    KitConfigWriter, built_in_item_id, plan_init_with_context, plan_sync_from_config,
    planned_or_existing_kit_config_content, prepare_kit_config_write, upsert_planned_file,
};
use crate::path_safety::PlanningContext;
use crate::{AddPlan, ChangeKind, CodegenError, install_lock_path};

pub fn plan_add(project_root: &Path, item_name: &str) -> Result<AddPlan, CodegenError> {
    plan_add_with_config_writer(project_root, item_name, kit_config_for_write)
}

pub(crate) fn plan_add_with_config_writer(
    project_root: &Path,
    item_name: &str,
    config_writer: KitConfigWriter,
) -> Result<AddPlan, CodegenError> {
    let context = PlanningContext::open(project_root)?;
    plan_add_with_context(&context, project_root, item_name, config_writer)
}

pub(crate) fn plan_add_with_context(
    context: &PlanningContext,
    project_root: &Path,
    item_name: &str,
    config_writer: KitConfigWriter,
) -> Result<AddPlan, CodegenError> {
    let item = load_built_in_registry_item(item_name)?;
    let desired_items = resolve_built_in_registry_items(&[item_name.to_owned()])?
        .into_iter()
        .map(|item| desired_builtin_item(&item.item.name))
        .collect::<Result<Vec<_>, _>>()?;
    let item_id = built_in_item_id(&item.item.name);
    let item_name = item.item.name.clone();
    let content_hash = item.content_hash.clone();
    let init_plan = plan_init_with_context(context, project_root, kit_config_to_canonical_json)?;
    let existing_config_content =
        planned_or_existing_kit_config_content(context, &init_plan.files)?;
    let config = parse_kit_json_str(&existing_config_content)?;
    let state_path = install_lock_path(&config);
    let mut files = init_plan
        .files
        .into_iter()
        .filter(|file| file.path != state_path)
        .collect::<Vec<_>>();
    let mut changes = init_plan
        .changes
        .into_iter()
        .filter(|change| change.path != state_path)
        .collect::<Vec<_>>();

    let config = kit_config_with_desired_items(config, desired_items)?;
    let candidate_config_content = kit_config_to_json(&config)?;
    let (config, config_content) = if candidate_config_content == existing_config_content {
        (config, candidate_config_content)
    } else {
        prepare_kit_config_write(config, config_writer)?
    };
    upsert_planned_file(
        context,
        &mut files,
        &mut changes,
        DEFAULT_KIT_CONFIG_PATH,
        config_content.clone(),
        ChangeKind::UpdateFile,
        Some(&item_id),
    )?;

    let sync = plan_sync_from_config(
        context,
        project_root,
        files,
        changes,
        config,
        config_content,
        config_writer,
    )?;

    Ok(AddPlan {
        project_root: sync.project_root,
        item_id,
        item_name,
        content_hash,
        cargo_plan: sync.cargo_plan,
        files: sync.files,
        changes: sync.changes,
        diagnostics: sync.diagnostics,
        lock: sync.lock,
        snapshot: sync.snapshot,
    })
}

fn kit_config_to_canonical_json() -> Result<String, leptos_ui_kit_registry::ConfigError> {
    leptos_ui_kit_registry::canonical_kit_json()
}

pub(crate) fn desired_builtin_item(
    name: &str,
) -> Result<leptos_ui_kit_registry::DesiredItemConfig, RegistryError> {
    match name {
        "anchor" => Ok(desired_builtin_anchor_item()),
        "button" => Ok(desired_builtin_button_item()),
        "collapsible" => Ok(desired_builtin_collapsible_item()),
        "dialog" => Ok(desired_builtin_dialog_item()),
        "field" => Ok(desired_builtin_field_item()),
        "menu" => Ok(desired_builtin_menu_item()),
        "router-link" => Ok(desired_builtin_router_link_item()),
        "spinner" => Ok(desired_builtin_spinner_item()),
        "status" => Ok(desired_builtin_status_item()),
        "tabs" => Ok(desired_builtin_tabs_item()),
        "tokens" => Ok(desired_builtin_tokens_item()),
        _ => Err(RegistryError::BuiltInNotFound(name.to_owned())),
    }
}

fn kit_config_with_desired_items(
    config: KitConfig,
    items: Vec<leptos_ui_kit_registry::DesiredItemConfig>,
) -> Result<KitConfig, CodegenError> {
    let mut config = config;
    for item in items {
        config = kit_config_with_desired_item(config, item)?;
    }
    Ok(config)
}
