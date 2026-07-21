use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use leptos_ui_kit_registry::{
    CONTRACT_ABI_VERSION, CONTRACT_ID, CONTRACT_REVISION, CargoPlanEntry, DEFAULT_KIT_CONFIG_PATH,
    DesiredItemConfig, KitConfig, LAYER_ORDER, PORTAL_ABI_VERSION, PORTAL_MOUNT_TYPE,
    PRESENCE_ABI_VERSION, RegistryError, ResolvedRegistryItem, WEB_UI_PRIMITIVES_GIT_REV,
    WEB_UI_PRIMITIVES_REQUIREMENT, WEB_UI_PRIMITIVES_VERSION, kit_config_for_write,
    kit_config_to_json, load_built_in_registry_item, normalize_cargo_plan_for_project,
    parse_kit_json_str, read_built_in_registry_source, resolve_built_in_registry_items,
    token_contract_json,
};

use super::{
    KitConfigWriter, built_in_item_id, desired_builtin_item, load_or_empty_lock,
    plan_generated_source_file, plan_init_with_context, planned_or_existing_content,
    planned_or_existing_kit_config_content, ui_exports_for_item, upsert_planned_file,
    upsert_planned_install_lock, upsert_preloaded_planned_file,
};
use crate::digest::hash_bytes;
use crate::patch::{
    ManagedCssRetirement, patch_components_mod_at_path, patch_ui_mod_at_path,
    reconcile_managed_css_blocks_with_retirements_at_path,
};
use crate::path_safety::PlanningContext;
use crate::{
    ChangeKind, ChangeRecord, CodegenError, InstallLock, InstalledFile, InstalledItem,
    InstalledStyleBlock, InstalledThemeIntegration, ManagedCssBlockRole, ManagedCssDependency,
    ManagedCssOperation, PlannedFile, SyncPlan, THEME_CAPABILITY_PATH, TOKEN_CONTRACT_PATH,
    install_lock_path, lock_to_json_at_path, validate_planned_write_paths,
};

#[derive(Debug, Clone)]
pub(crate) struct DesiredStateProjection {
    pub(crate) desired_items: Vec<DesiredItemConfig>,
    pub(crate) resolved_items: Vec<ResolvedRegistryItem>,
    pub(crate) retained_item_ids: BTreeSet<String>,
    pub(crate) retired_item_ids: BTreeSet<String>,
    pub(crate) lock: InstallLock,
    pub(crate) item_ids: Vec<String>,
    pub(crate) cargo_plan: Vec<CargoPlanEntry>,
    pub(crate) css_operations: Vec<ManagedCssOperation>,
    pub(crate) css_dependencies: Vec<ManagedCssDependency>,
    pub(crate) css_retirements: Vec<ManagedCssRetirement>,
}

pub(crate) struct ItemLockTransition<'a> {
    pub(crate) prior: &'a InstallLock,
    pub(crate) output: &'a mut InstallLock,
}

pub(crate) fn project_desired_state(
    config: &KitConfig,
    config_hash: String,
    prior_lock: &InstallLock,
) -> Result<DesiredStateProjection, CodegenError> {
    let requested_names = config
        .items
        .iter()
        .map(|item| item.item_name().to_owned())
        .collect::<Vec<_>>();
    let resolved_items = resolve_built_in_registry_items(&requested_names)?;
    project_desired_state_from_resolved(config, config_hash, prior_lock, resolved_items)
}

fn project_desired_state_from_resolved(
    config: &KitConfig,
    config_hash: String,
    prior_lock: &InstallLock,
    resolved_items: Vec<ResolvedRegistryItem>,
) -> Result<DesiredStateProjection, CodegenError> {
    let desired_items = resolved_items
        .iter()
        .map(|item| desired_builtin_item(&item.item.name))
        .collect::<Result<Vec<_>, _>>()?;
    let item_ids = resolved_items
        .iter()
        .map(|item| built_in_item_id(&item.item.name))
        .collect::<Vec<_>>();
    let desired_item_ids = item_ids.iter().cloned().collect::<BTreeSet<_>>();
    let prior_item_ids = prior_lock.items.keys().cloned().collect::<BTreeSet<_>>();
    let retained_item_ids: BTreeSet<String> = desired_item_ids
        .intersection(&prior_item_ids)
        .cloned()
        .collect();
    let retired_item_ids: BTreeSet<String> = prior_item_ids
        .difference(&desired_item_ids)
        .cloned()
        .collect();
    let mut lock = InstallLock::empty_for_project(config_hash, config.project.kind);
    let mut cargo_plan_entries = Vec::new();
    let mut css_operations = Vec::new();
    let mut css_retirements = Vec::new();

    for item_id in &retired_item_ids {
        let prior_item = &prior_lock.items[item_id];
        let current_item = match load_built_in_registry_item(&prior_item.name) {
            Ok(item) => Some(item),
            Err(RegistryError::BuiltInNotFound(_)) => None,
            Err(error) => return Err(error.into()),
        };
        for block in &prior_item.style_blocks {
            let current_generated = current_item
                .as_ref()
                .and_then(|item| {
                    item.targets
                        .style_blocks
                        .iter()
                        .find(|style| style.id == block.block_id)
                })
                .map(|style| read_built_in_registry_source(&style.source))
                .transpose()?;
            css_retirements.push(ManagedCssRetirement {
                item_id: item_id.clone(),
                block_id: block.block_id.clone(),
                current_generated,
            });
        }
    }

    for item in &resolved_items {
        let item_id = built_in_item_id(&item.item.name);
        let mut installed_files = Vec::new();
        let mut installed_style_blocks = Vec::new();

        for ui_file in &item.targets.ui_files {
            let generated = read_built_in_registry_source(&ui_file.source)?;
            let logical_path = format!("{}/{}", config.install.ui_dir, ui_file.path);
            let generated_hash = hash_bytes(generated.as_bytes());
            installed_files.push(InstalledFile {
                path: logical_path.clone(),
                kind: "rust".to_owned(),
                generated_hash: generated_hash.clone(),
                local_hash_at_install: generated_hash,
            });
            lock.files_by_path.insert(logical_path, item_id.clone());
        }

        for style in &item.targets.style_blocks {
            let generated = read_built_in_registry_source(&style.source)?;
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
                css_path: config.styles.css.clone(),
                block_id: style.id.clone(),
                generated_hash,
            });
            lock.style_blocks_by_id
                .insert(style.id.clone(), item_id.clone());
        }

        lock.items.insert(
            item_id.clone(),
            InstalledItem {
                id: item_id,
                name: item.item.name.clone(),
                kind: item.item.kind,
                source: "builtin".to_owned(),
                version: item.item.version.clone(),
                content_hash: item.content_hash.clone(),
                files: installed_files,
                style_blocks: installed_style_blocks,
            },
        );
        cargo_plan_entries.extend(item.item.cargo_plan.iter().cloned());
    }

    let cargo_plan = normalize_cargo_plan_for_project(
        &cargo_plan_entries,
        config.project.kind.render_mode_contract(),
    )?;
    let css_dependencies = managed_css_dependencies(&resolved_items);
    Ok(DesiredStateProjection {
        desired_items,
        resolved_items,
        retained_item_ids,
        retired_item_ids,
        lock,
        item_ids,
        cargo_plan,
        css_operations,
        css_dependencies,
        css_retirements,
    })
}

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
    let original_config_hash = hash_bytes(config_content.as_bytes());
    let lock_path = install_lock_path(&config);
    let prior_lock = load_or_empty_lock(context, &lock_path, original_config_hash.clone())?;
    let mut desired_projection = project_desired_state(&config, original_config_hash, &prior_lock)?;

    if config.items != desired_projection.desired_items {
        config.items = desired_projection.desired_items.clone();
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
    desired_projection.lock.project.config_hash = config_hash.clone();
    debug_assert_eq!(desired_projection.desired_items, config.items);
    debug_assert_eq!(
        desired_projection.retained_item_ids.len() + desired_projection.retired_item_ids.len(),
        prior_lock.items.len()
    );
    debug_assert_eq!(desired_projection.lock.project.config_hash, config_hash);
    debug_assert_eq!(
        desired_projection.css_retirements.len(),
        desired_projection
            .retired_item_ids
            .iter()
            .map(|item_id| prior_lock.items[item_id].style_blocks.len())
            .sum::<usize>()
    );
    let mut lock = desired_projection.lock.clone();
    let mut item_ids = Vec::new();
    let cargo_plan = desired_projection.cargo_plan.clone();
    let mut css_operations = Vec::new();
    let css_dependencies = desired_projection.css_dependencies.clone();

    for item in &desired_projection.resolved_items {
        let item_id = plan_built_in_item(
            context,
            &mut files,
            &mut changes,
            ItemLockTransition {
                prior: &prior_lock,
                output: &mut lock,
            },
            &config,
            item,
            &mut css_operations,
        )?;
        item_ids.push(item_id);
    }
    debug_assert_eq!(item_ids, desired_projection.item_ids);
    debug_assert_eq!(css_operations, desired_projection.css_operations);

    plan_managed_stylesheet_batch_with_retirements(
        context,
        &mut files,
        &mut changes,
        &prior_lock,
        &config,
        ManagedStylesheetProjection {
            operations: &css_operations,
            dependencies: &css_dependencies,
            retirements: &desired_projection.css_retirements,
        },
    )?;

    install_theme_integration(context, &mut files, &mut changes, &mut lock, &config)?;

    lock.validate_at_path(Path::new(&lock_path))?;
    let lock_json = lock_to_json_at_path(&lock, Path::new(&lock_path))?;
    let force_lock_publication = !files.is_empty();
    upsert_planned_install_lock(
        context,
        &mut files,
        &mut changes,
        lock_json,
        force_lock_publication,
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

fn install_theme_integration(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    lock: &mut InstallLock,
    config: &KitConfig,
) -> Result<(), CodegenError> {
    let contract = token_contract_json().map_err(|reason| CodegenError::InvalidLock {
        path: TOKEN_CONTRACT_PATH.into(),
        reason,
    })?;
    let contract_value: serde_json::Value =
        serde_json::from_str(&contract).map_err(|source| CodegenError::LockParse {
            path: TOKEN_CONTRACT_PATH.into(),
            source,
        })?;
    let contract_canonical_digest = contract_value["canonicalDigest"]
        .as_str()
        .ok_or_else(|| CodegenError::InvalidLock {
            path: TOKEN_CONTRACT_PATH.into(),
            reason: "token contract omits canonicalDigest".to_owned(),
        })?
        .to_owned();
    let contract_bytes_digest = hash_bytes(contract.as_bytes());
    let stylesheet =
        planned_or_existing_content(files, context, &config.styles.css)?.ok_or_else(|| {
            CodegenError::InvalidLock {
                path: config.styles.css.clone().into(),
                reason: "theme integration requires the installed kit stylesheet".to_owned(),
            }
        })?;
    let stylesheet_bytes_digest = hash_bytes(stylesheet.as_bytes());
    let capability = serde_json::json!({
        "$schema": "https://triesap.github.io/leptos_ui_kit/schema/0.2.0/theme-integration.schema.json",
        "schemaVersion": "1.0.0",
        "producer": {
            "package": "leptos_ui_kit_cli",
            "version": env!("CARGO_PKG_VERSION"),
            "repository": "https://github.com/triesap/leptos_ui_kit",
            "checksum": null
        },
        "primitives": {
            "package": "web_ui_primitives",
            "requirement": WEB_UI_PRIMITIVES_REQUIREMENT,
            "version": WEB_UI_PRIMITIVES_VERSION,
            "checksum": format!("git:{WEB_UI_PRIMITIVES_GIT_REV}"),
            "presenceAbi": PRESENCE_ABI_VERSION
        },
        "contract": {
            "path": "token-contract.json",
            "contractId": CONTRACT_ID,
            "abiVersion": CONTRACT_ABI_VERSION,
            "revision": CONTRACT_REVISION,
            "canonicalDigest": contract_canonical_digest,
            "installedBytesDigest": contract_bytes_digest
        },
        "stylesheet": {
            "path": config.styles.css,
            "installedBytesDigest": stylesheet_bytes_digest
        },
        "layerAbi": {
            "version": 1,
            "order": LAYER_ORDER
        },
        "portalAbi": {
            "version": PORTAL_ABI_VERSION,
            "mountType": PORTAL_MOUNT_TYPE,
            "bodyHost": true
        }
    });
    let capability =
        serde_json::to_string_pretty(&capability).map_err(CodegenError::LockSerialize)?;
    let capability = format!("{capability}\n");
    let capability_bytes_digest = hash_bytes(capability.as_bytes());

    upsert_planned_file(
        context,
        files,
        changes,
        TOKEN_CONTRACT_PATH,
        contract,
        ChangeKind::CreateFile,
        None,
    )?;
    upsert_planned_file(
        context,
        files,
        changes,
        THEME_CAPABILITY_PATH,
        capability,
        ChangeKind::CreateFile,
        None,
    )?;
    lock.theme_integration = Some(InstalledThemeIntegration {
        producer_package: "leptos_ui_kit_cli".to_owned(),
        producer_version: env!("CARGO_PKG_VERSION").to_owned(),
        producer_checksum: None,
        primitives_package: "web_ui_primitives".to_owned(),
        primitives_requirement: WEB_UI_PRIMITIVES_REQUIREMENT.to_owned(),
        primitives_version: WEB_UI_PRIMITIVES_VERSION.to_owned(),
        primitives_checksum: format!("git:{WEB_UI_PRIMITIVES_GIT_REV}"),
        presence_abi_version: PRESENCE_ABI_VERSION,
        contract_path: TOKEN_CONTRACT_PATH.to_owned(),
        contract_id: CONTRACT_ID.to_owned(),
        contract_abi_version: CONTRACT_ABI_VERSION,
        contract_revision: CONTRACT_REVISION,
        contract_canonical_digest,
        contract_bytes_digest,
        capability_path: THEME_CAPABILITY_PATH.to_owned(),
        capability_bytes_digest,
        stylesheet_path: config.styles.css.clone(),
        stylesheet_bytes_digest,
        layer_abi_version: 1,
        layer_order: LAYER_ORDER.map(str::to_owned).to_vec(),
        portal_abi_version: PORTAL_ABI_VERSION,
        portal_mount_type: PORTAL_MOUNT_TYPE.to_owned(),
        portal_body_host: true,
    });
    Ok(())
}

pub(crate) fn prepare_kit_config_write(
    config: KitConfig,
    config_writer: KitConfigWriter,
) -> Result<(KitConfig, String), CodegenError> {
    let config = config_writer(config)?;
    let content = kit_config_to_json(&config)?;
    Ok((config, content))
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
    locks: ItemLockTransition<'_>,
    config: &KitConfig,
    item: &leptos_ui_kit_registry::ResolvedRegistryItem,
    css_operations: &mut Vec<ManagedCssOperation>,
) -> Result<String, CodegenError> {
    let item_id = built_in_item_id(&item.item.name);
    let mut installed_files = Vec::new();
    let mut installed_style_blocks = Vec::new();

    for ui_file in &item.targets.ui_files {
        let generated = read_built_in_registry_source(&ui_file.source)?;
        let logical_path = format!("{}/{}", config.install.ui_dir, ui_file.path);
        let generated_hash = hash_bytes(generated.as_bytes());

        plan_generated_source_file(
            context,
            files,
            changes,
            locks.prior,
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
        locks
            .output
            .files_by_path
            .insert(logical_path, item_id.clone());
    }

    if !item.targets.ui_files.is_empty() {
        let components_mod_path = config.install.components_mod.as_str();
        let ui_mod_path = config.install.ui_mod.as_str();
        let ui_module_name = config
            .install
            .ui_dir
            .rsplit('/')
            .next()
            .expect("validated UI directory has a final segment");
        let components_mod = planned_or_existing_content(files, context, components_mod_path)?;
        let components_mod_change = if components_mod.is_some() {
            ChangeKind::UpdateFile
        } else {
            ChangeKind::CreateFile
        };
        let patched_components_mod = patch_components_mod_at_path(
            components_mod.as_deref(),
            components_mod_path,
            ui_module_name,
        )?;
        upsert_planned_file(
            context,
            files,
            changes,
            components_mod_path,
            patched_components_mod,
            components_mod_change,
            Some(&item_id),
        )?;

        let ui_mod = planned_or_existing_content(files, context, ui_mod_path)?;
        let ui_mod_change = if ui_mod.is_some() {
            ChangeKind::UpdateFile
        } else {
            ChangeKind::CreateFile
        };
        let patched_ui_mod = patch_ui_mod_at_path(
            ui_mod.as_deref(),
            &ui_exports_for_item(&item.item)?,
            ui_mod_path,
        )?;
        upsert_planned_file(
            context,
            files,
            changes,
            ui_mod_path,
            patched_ui_mod,
            ui_mod_change,
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
        locks
            .output
            .style_blocks_by_id
            .insert(style.id.clone(), item_id.clone());
    }

    locks.output.items.insert(
        item_id.clone(),
        InstalledItem {
            id: item_id.clone(),
            name: item.item.name.clone(),
            kind: item.item.kind,
            source: "builtin".to_owned(),
            version: item.item.version.clone(),
            content_hash: item.content_hash.clone(),
            files: installed_files,
            style_blocks: installed_style_blocks,
        },
    );
    Ok(item_id)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ManagedStylesheetProjection<'a> {
    pub(crate) operations: &'a [ManagedCssOperation],
    pub(crate) dependencies: &'a [ManagedCssDependency],
    pub(crate) retirements: &'a [ManagedCssRetirement],
}

pub(crate) fn plan_managed_stylesheet_batch_with_retirements(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    prior_lock: &InstallLock,
    config: &KitConfig,
    projection: ManagedStylesheetProjection<'_>,
) -> Result<(), CodegenError> {
    if projection.operations.is_empty() && projection.retirements.is_empty() {
        return Ok(());
    }
    let css_path = config.styles.css.as_str();
    let existing = planned_or_existing_content(files, context, css_path)?.unwrap_or_default();
    let reconciled = reconcile_managed_css_blocks_with_retirements_at_path(
        &existing,
        css_path,
        prior_lock,
        projection.operations,
        projection.dependencies,
        projection.retirements,
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

#[cfg(test)]
pub(crate) fn plan_desired_ownership_cohort(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    prior_lock: &InstallLock,
    config: &KitConfig,
    projection: &DesiredStateProjection,
) -> Result<InstallLock, CodegenError> {
    for item in &projection.resolved_items {
        let item_id = built_in_item_id(&item.item.name);
        for ui_file in &item.targets.ui_files {
            let generated = read_built_in_registry_source(&ui_file.source)?;
            let logical_path = format!("{}/{}", config.install.ui_dir, ui_file.path);
            plan_generated_source_file(
                context,
                files,
                changes,
                prior_lock,
                &item_id,
                &logical_path,
                &generated,
            )?;
        }
    }

    plan_managed_stylesheet_batch_with_retirements(
        context,
        files,
        changes,
        prior_lock,
        config,
        ManagedStylesheetProjection {
            operations: &projection.css_operations,
            dependencies: &projection.css_dependencies,
            retirements: &projection.css_retirements,
        },
    )?;

    let output_lock = projection.lock.clone();
    let lock_path = install_lock_path(config);
    output_lock.validate_at_path(Path::new(&lock_path))?;
    let lock_json = lock_to_json_at_path(&output_lock, Path::new(&lock_path))?;
    let force_lock_publication = !files.is_empty();
    upsert_planned_install_lock(context, files, changes, lock_json, force_lock_publication)?;
    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    validate_planned_write_paths(&paths)?;
    Ok(output_lock)
}
