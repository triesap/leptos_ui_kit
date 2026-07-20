use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use leptos_ui_kit_registry::{
    CargoPlanEntry, DEFAULT_KIT_CONFIG_PATH, KitConfig, kit_config_for_write, kit_config_to_json,
    parse_kit_json_str, read_built_in_registry_source, resolve_built_in_registry_items,
};

use super::{
    KitConfigWriter, built_in_item_id, desired_builtin_item, load_or_empty_lock,
    plan_generated_source_file, plan_init_with_context, planned_or_existing_content,
    planned_or_existing_kit_config_content, ui_exports_for_item, upsert_planned_file,
    upsert_preloaded_planned_file,
};
use crate::digest::hash_bytes;
use crate::path_safety::PlanningContext;
use crate::{
    ChangeKind, ChangeRecord, CodegenError, InstallLock, InstalledFile, InstalledItem,
    InstalledStyleBlock, ManagedCssBlockRole, ManagedCssDependency, ManagedCssOperation,
    PlannedFile, SyncPlan, install_lock_path, lock_to_json_at_path, patch_components_mod,
    patch_ui_mod, reconcile_managed_css_blocks_at_path, validate_planned_write_paths,
};

pub fn plan_sync(project_root: &Path) -> Result<SyncPlan, CodegenError> {
    plan_sync_with_config_writer(project_root, kit_config_for_write)
}

pub(crate) fn plan_sync_with_config_writer(
    project_root: &Path,
    config_writer: KitConfigWriter,
) -> Result<SyncPlan, CodegenError> {
    crate::transaction::check_pending_recovery(project_root)?;
    let context = PlanningContext::open(project_root)?;
    plan_sync_with_context(&context, project_root, config_writer)
}

pub(crate) fn plan_sync_with_context(
    context: &PlanningContext,
    project_root: &Path,
    config_writer: KitConfigWriter,
) -> Result<SyncPlan, CodegenError> {
    let init_plan = plan_init_with_context(
        context,
        project_root,
        leptos_ui_kit_registry::canonical_kit_json,
    )?;
    let config_content = planned_or_existing_kit_config_content(context, &init_plan.files)?;
    let config = parse_kit_json_str(&config_content)?;
    let state_path = install_lock_path(&config);
    let files = init_plan
        .files
        .into_iter()
        .filter(|file| file.path != state_path)
        .collect::<Vec<_>>();
    let changes = init_plan
        .changes
        .into_iter()
        .filter(|change| change.path != state_path)
        .collect::<Vec<_>>();

    plan_sync_from_config(
        context,
        project_root,
        files,
        changes,
        config,
        config_content,
        config_writer,
    )
}

pub(crate) fn plan_sync_from_config(
    context: &PlanningContext,
    project_root: &Path,
    mut files: Vec<PlannedFile>,
    mut changes: Vec<ChangeRecord>,
    mut config: KitConfig,
    mut config_content: String,
    config_writer: KitConfigWriter,
) -> Result<SyncPlan, CodegenError> {
    let diagnostics = Vec::new();
    let mut requested_names = config
        .items
        .iter()
        .map(|item| item.item_name().to_owned())
        .collect::<Vec<_>>();
    requested_names.sort();
    let resolved_items = resolve_built_in_registry_items(&requested_names)?;
    let resolved_desired_items = resolved_items
        .iter()
        .map(|item| desired_builtin_item(&item.item.name))
        .collect::<Result<Vec<_>, _>>()?;

    if config.items != resolved_desired_items {
        config.items = resolved_desired_items;
        (config, config_content) = prepare_kit_config_write(config, config_writer)?;
        upsert_planned_file(
            context,
            &mut files,
            &mut changes,
            DEFAULT_KIT_CONFIG_PATH,
            config_content.clone(),
            ChangeKind::UpdateFile,
            None,
        )?;
    }

    let config_hash = hash_bytes(config_content.as_bytes());
    let lock_path = install_lock_path(&config);
    let mut lock = load_or_empty_lock(
        context,
        &lock_path,
        config_hash.clone(),
        config.project.kind,
    )?;
    let prior_lock = lock.clone();
    lock.project.config_hash = config_hash;
    let mut item_ids = Vec::new();
    let mut cargo_plan = Vec::new();
    let mut css_operations = Vec::new();
    let css_dependencies = managed_css_dependencies(&resolved_items);

    for item in &resolved_items {
        let item_id = plan_built_in_item(
            context,
            &mut files,
            &mut changes,
            &mut lock,
            &config,
            item,
            &mut css_operations,
        )?;
        item_ids.push(item_id);
        merge_cargo_plan(&mut cargo_plan, &item.item.cargo_plan);
    }

    plan_managed_stylesheet_batch(
        context,
        &mut files,
        &mut changes,
        &prior_lock,
        &config,
        &css_operations,
        &css_dependencies,
    )?;

    lock.validate_at_path(Path::new(&lock_path))?;
    let lock_json = lock_to_json_at_path(&lock, Path::new(&lock_path))?;
    upsert_planned_file(
        context,
        &mut files,
        &mut changes,
        &lock_path,
        lock_json,
        ChangeKind::WriteLockFile,
        None,
    )?;

    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    validate_planned_write_paths(&paths)?;

    Ok(SyncPlan {
        project_root: project_root.to_path_buf(),
        item_ids,
        cargo_plan,
        files,
        changes,
        diagnostics,
        lock,
        snapshot: context.finish_snapshot(),
    })
}

pub(crate) fn prepare_kit_config_write(
    config: KitConfig,
    config_writer: KitConfigWriter,
) -> Result<(KitConfig, String), CodegenError> {
    let config = config_writer(config)?;
    let content = kit_config_to_json(&config)?;
    Ok((config, content))
}

fn merge_cargo_plan(plan: &mut Vec<CargoPlanEntry>, entries: &[CargoPlanEntry]) {
    for entry in entries {
        let mut entry = entry.clone();
        entry.features.sort();
        entry.features.dedup();
        if !plan.contains(&entry) {
            plan.push(entry);
        }
    }
    plan.sort();
}

fn managed_css_dependencies(
    items: &[leptos_ui_kit_registry::ResolvedRegistryItem],
) -> Vec<ManagedCssDependency> {
    let style_ids_by_item = items
        .iter()
        .map(|item| {
            (
                item.item.name.as_str(),
                item.targets
                    .style_blocks
                    .iter()
                    .map(|style| style.id.as_str())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut dependencies = BTreeSet::new();

    for item in items {
        let dependent_ids = &style_ids_by_item[item.item.name.as_str()];
        for dependency_name in &item.item.registry_dependencies {
            let Some(dependency_ids) = style_ids_by_item.get(dependency_name.as_str()) else {
                continue;
            };
            for dependency_block_id in dependency_ids {
                for dependent_block_id in dependent_ids {
                    dependencies.insert(ManagedCssDependency {
                        dependency_block_id: (*dependency_block_id).to_owned(),
                        dependent_block_id: (*dependent_block_id).to_owned(),
                    });
                }
            }
        }
    }

    dependencies.into_iter().collect()
}

pub(crate) fn plan_built_in_item(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    lock: &mut InstallLock,
    config: &KitConfig,
    item: &leptos_ui_kit_registry::ResolvedRegistryItem,
    css_operations: &mut Vec<ManagedCssOperation>,
) -> Result<String, CodegenError> {
    let item_id = built_in_item_id(&item.item.name);
    let mut installed_files = Vec::new();
    let mut installed_style_blocks = Vec::new();

    for ui_file in &item.targets.ui_files {
        let generated = read_built_in_registry_source(&ui_file.source)?;
        let logical_path = format!("src/components/ui/{}", ui_file.path);
        let generated_hash = hash_bytes(generated.as_bytes());

        plan_generated_source_file(
            context,
            files,
            changes,
            lock,
            &item_id,
            &logical_path,
            &generated,
        )?;

        installed_files.push(InstalledFile {
            path: logical_path.clone(),
            kind: "rust".to_owned(),
            generated_hash: generated_hash.clone(),
            local_hash_at_install: generated_hash,
        });
        lock.files_by_path.insert(logical_path, item_id.clone());
    }

    if !item.targets.ui_files.is_empty() {
        let components_mod = planned_or_existing_content(files, context, "src/components/mod.rs")?;
        let patched_components_mod = patch_components_mod(components_mod.as_deref())?;
        upsert_planned_file(
            context,
            files,
            changes,
            "src/components/mod.rs",
            patched_components_mod,
            ChangeKind::UpdateFile,
            Some(&item_id),
        )?;

        let ui_mod = planned_or_existing_content(files, context, "src/components/ui/mod.rs")?;
        let patched_ui_mod = patch_ui_mod(ui_mod.as_deref(), &ui_exports_for_item(&item.item)?)?;
        upsert_planned_file(
            context,
            files,
            changes,
            "src/components/ui/mod.rs",
            patched_ui_mod,
            ChangeKind::UpdateFile,
            Some(&item_id),
        )?;
    }

    for style in &item.targets.style_blocks {
        let generated = read_built_in_registry_source(&style.source)?;
        let css_path = config.styles.css.as_str();
        let generated_hash = hash_bytes(generated.as_bytes());
        css_operations.push(ManagedCssOperation {
            item_id: item_id.clone(),
            block_id: style.id.clone(),
            role: match item.item.kind {
                leptos_ui_kit_registry::RegistryItemKind::Foundation => {
                    ManagedCssBlockRole::Foundation
                }
                leptos_ui_kit_registry::RegistryItemKind::Ui => ManagedCssBlockRole::Component,
            },
            generated,
        });

        installed_style_blocks.push(InstalledStyleBlock {
            css_path: css_path.to_owned(),
            block_id: style.id.clone(),
            generated_hash,
        });
        lock.style_blocks_by_id
            .insert(style.id.clone(), item_id.clone());
    }

    lock.items.insert(
        item_id.clone(),
        InstalledItem {
            id: item_id.clone(),
            name: item.item.name.clone(),
            source: "builtin".to_owned(),
            version: item.item.version.clone(),
            content_hash: item.content_hash.clone(),
            files: installed_files,
            style_blocks: installed_style_blocks,
        },
    );
    Ok(item_id)
}

fn plan_managed_stylesheet_batch(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    prior_lock: &InstallLock,
    config: &KitConfig,
    operations: &[ManagedCssOperation],
    dependencies: &[ManagedCssDependency],
) -> Result<(), CodegenError> {
    if operations.is_empty() {
        return Ok(());
    }

    let css_path = config.styles.css.as_str();
    let existing = planned_or_existing_content(files, context, css_path)?.unwrap_or_default();
    let reconciled = reconcile_managed_css_blocks_at_path(
        &existing,
        css_path,
        prior_lock,
        operations,
        dependencies,
    )?;

    upsert_preloaded_planned_file(
        files,
        changes,
        css_path,
        &existing,
        reconciled,
        ChangeKind::UpdateCssBlock,
        None,
    )
}
