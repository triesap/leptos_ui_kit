use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use leptos_ui_kit_registry::{DEFAULT_KIT_CONFIG_PATH, RegistryItem, canonical_kit_json};

use crate::digest::hash_bytes;
use crate::patch::unsafe_patch;
use crate::path_safety::PlanningContext;
use crate::{
    ChangeKind, ChangeRecord, CodegenError, DEFAULT_KIT_LOCK_PATH, InstallLock, InstalledFile,
    PlannedFile, PlannedFileAction, UiModuleExport, lock_to_json_at_path,
    parse_install_lock_str_at_path,
};

pub(crate) fn read_canonical_install_lock(
    context: &PlanningContext,
    lock_path: &str,
) -> Result<Option<(InstallLock, String)>, CodegenError> {
    let Some(input) = context.read_optional_string(lock_path)? else {
        return Ok(None);
    };
    let path = context.project_root().join(lock_path);
    let lock = parse_install_lock_str_at_path(&input, &path)?;
    let canonical = lock_to_json_at_path(&lock, &path)?;
    if input != canonical {
        return Err(CodegenError::InvalidLock {
            path,
            reason: "install lock must use its canonical JSON serialization".to_owned(),
        });
    }

    Ok(Some((lock, canonical)))
}

pub(crate) fn load_or_empty_lock(
    context: &PlanningContext,
    lock_path: &str,
    config_hash: String,
) -> Result<InstallLock, CodegenError> {
    if let Some((mut lock, _)) = read_canonical_install_lock(context, lock_path)? {
        lock.project.config_hash = config_hash;
        return Ok(lock);
    }

    Ok(InstallLock::empty(config_hash))
}

pub(crate) fn plan_generated_source_file(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    lock: &InstallLock,
    item_id: &str,
    logical_path: &str,
    generated: &str,
) -> Result<(), CodegenError> {
    if let Some(owner) = lock.files_by_path.get(logical_path) {
        if owner != item_id {
            return unsafe_patch(
                logical_path,
                format!("target is already tracked by {owner}"),
            );
        }

        let current = context.read_optional_string(logical_path)?;
        let Some(current) = current else {
            return upsert_planned_file(
                context,
                files,
                changes,
                logical_path,
                generated.to_owned(),
                ChangeKind::CreateFile,
                Some(item_id),
            );
        };
        if current == generated {
            return Ok(());
        }
        let tracked = tracked_file_lock(lock, item_id, logical_path)?;
        if hash_bytes(current.as_bytes()) != tracked.local_hash_at_install {
            return unsafe_patch(logical_path, "tracked target has local edits");
        }
        return upsert_planned_file(
            context,
            files,
            changes,
            logical_path,
            generated.to_owned(),
            ChangeKind::UpdateFile,
            Some(item_id),
        );
    }

    if let Some(current) = context.read_optional_string(logical_path)? {
        if current == generated {
            return Ok(());
        }
        return unsafe_patch(
            logical_path,
            "target is application-owned and differs from the current registry source; reconcile or move it before re-adding the item",
        );
    }

    upsert_planned_file(
        context,
        files,
        changes,
        logical_path,
        generated.to_owned(),
        ChangeKind::CreateFile,
        Some(item_id),
    )
}

pub(crate) fn upsert_planned_file(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    logical_path: &str,
    content: String,
    change_kind: ChangeKind,
    item_id: Option<&str>,
) -> Result<(), CodegenError> {
    if let Some(file) = files.iter_mut().find(|file| file.path == logical_path) {
        if file.content != content {
            file.content = content;
        }
        return Ok(());
    }

    let existing = context.read_optional_string(logical_path)?;
    if existing.as_deref() == Some(content.as_str()) {
        return Ok(());
    }

    let action = if existing.is_some() {
        PlannedFileAction::Update
    } else {
        PlannedFileAction::Create
    };
    files.push(PlannedFile {
        path: logical_path.to_owned(),
        action,
        content,
    });

    let mut change = ChangeRecord::new(change_kind, logical_path, true);
    if let Some(item_id) = item_id {
        change = change.with_item(item_id);
    }
    changes.push(change);
    Ok(())
}

/// Adds the install-lock publication that closes a non-empty command cohort.
///
/// Unlike an ordinary upsert, `force_publication` keeps an exact-content
/// update in the plan. The durable transaction protocol uses that replacement
/// as the unique, final desired-state marker whenever another project file is
/// changing.
pub(crate) fn upsert_planned_install_lock(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    content: String,
    force_publication: bool,
) -> Result<(), CodegenError> {
    let logical_path = Path::new(DEFAULT_KIT_LOCK_PATH);
    let parsed = parse_install_lock_str_at_path(&content, logical_path)?;
    if lock_to_json_at_path(&parsed, logical_path)? != content {
        return Err(CodegenError::InvalidLock {
            path: PathBuf::from(DEFAULT_KIT_LOCK_PATH),
            reason: "planned install lock must use its canonical JSON serialization".to_owned(),
        });
    }

    let target_count = files
        .iter()
        .filter(|file| file.path == DEFAULT_KIT_LOCK_PATH)
        .count();
    if target_count > 1 {
        return Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_LOCK_PATH.to_owned(),
            reason: "the plan contains duplicate install-lock targets".to_owned(),
        });
    }
    if changes.iter().any(|change| {
        change.path == DEFAULT_KIT_LOCK_PATH && change.kind != ChangeKind::WriteLockFile
    }) {
        return Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_LOCK_PATH.to_owned(),
            reason: "the canonical install lock cannot carry another change kind".to_owned(),
        });
    }
    let marker_count = changes
        .iter()
        .filter(|change| change.kind == ChangeKind::WriteLockFile)
        .count();
    if marker_count > 1
        || changes.iter().any(|change| {
            change.kind == ChangeKind::WriteLockFile
                && (change.path != DEFAULT_KIT_LOCK_PATH || !change.tracked)
        })
    {
        return Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_LOCK_PATH.to_owned(),
            reason: "the plan contains an invalid install-lock change marker".to_owned(),
        });
    }
    if target_count == 0 && marker_count == 1 {
        return Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_LOCK_PATH.to_owned(),
            reason: "the install-lock change marker has no planned target".to_owned(),
        });
    }

    let existing = read_canonical_install_lock(context, DEFAULT_KIT_LOCK_PATH)?
        .map(|(_, canonical)| canonical);
    let publication_required =
        target_count == 1 || force_publication || existing.as_deref() != Some(content.as_str());
    if !publication_required {
        return Ok(());
    }

    files.retain(|file| file.path != DEFAULT_KIT_LOCK_PATH);
    files.push(PlannedFile {
        path: DEFAULT_KIT_LOCK_PATH.to_owned(),
        action: if existing.is_some() {
            PlannedFileAction::Update
        } else {
            PlannedFileAction::Create
        },
        content,
    });
    changes.retain(|change| change.kind != ChangeKind::WriteLockFile);
    changes.push(ChangeRecord::new(
        ChangeKind::WriteLockFile,
        DEFAULT_KIT_LOCK_PATH,
        true,
    ));
    Ok(())
}

pub(crate) fn upsert_preloaded_planned_file(
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    logical_path: &str,
    existing: &str,
    content: String,
    change_kind: ChangeKind,
    item_id: Option<&str>,
) -> Result<(), CodegenError> {
    if let Some(file) = files.iter_mut().find(|file| file.path == logical_path) {
        file.content = content;
        return Ok(());
    }
    if existing == content {
        return Ok(());
    }

    files.push(PlannedFile {
        path: logical_path.to_owned(),
        action: PlannedFileAction::Update,
        content,
    });
    let mut change = ChangeRecord::new(change_kind, logical_path, true);
    if let Some(item_id) = item_id {
        change = change.with_item(item_id);
    }
    changes.push(change);
    Ok(())
}

pub(crate) fn planned_or_existing_content(
    files: &[PlannedFile],
    context: &PlanningContext,
    logical_path: &str,
) -> Result<Option<String>, CodegenError> {
    if let Some(file) = files.iter().find(|file| file.path == logical_path) {
        return Ok(Some(file.content.clone()));
    }
    context.read_optional_string(logical_path)
}

pub(crate) fn planned_or_existing_kit_config_content(
    context: &PlanningContext,
    files: &[PlannedFile],
) -> Result<String, CodegenError> {
    if let Some(content) = planned_or_existing_content(files, context, DEFAULT_KIT_CONFIG_PATH)? {
        return Ok(content);
    }

    Ok(canonical_kit_json()?)
}

pub(crate) fn tracked_file_lock<'a>(
    lock: &'a InstallLock,
    item_id: &str,
    logical_path: &str,
) -> Result<&'a InstalledFile, CodegenError> {
    let path = PathBuf::from(DEFAULT_KIT_LOCK_PATH);
    let item = lock
        .items
        .get(item_id)
        .ok_or_else(|| CodegenError::InvalidLock {
            path: path.clone(),
            reason: format!("missing item {item_id}"),
        })?;
    item.files
        .iter()
        .find(|file| file.path == logical_path)
        .ok_or_else(|| CodegenError::InvalidLock {
            path: path.clone(),
            reason: format!("missing file lock entry for {logical_path}"),
        })
}

pub(crate) fn ui_exports_for_item(
    item: &RegistryItem,
) -> Result<Vec<UiModuleExport>, CodegenError> {
    let mut exports = BTreeMap::<(String, String), Vec<String>>::new();
    for file in &item.files {
        if file.target.exports.is_empty() {
            continue;
        }
        let (module, path) = ui_export_paths_for_target(&file.target.path)?;
        exports
            .entry((module, path))
            .or_default()
            .extend(file.target.exports.clone());
    }

    let mut output = Vec::new();
    for ((module, path), mut symbols) in exports {
        symbols.sort();
        symbols.dedup();
        output.push(UiModuleExport::with_path(module, path, symbols));
    }
    Ok(output)
}

pub(crate) fn ui_export_paths_for_target(
    target_path: &str,
) -> Result<(String, String), CodegenError> {
    let parts = target_path.split('/').collect::<Vec<_>>();
    let Some(first) = parts.first() else {
        return unsafe_patch("src/components/ui/mod.rs", "missing UI target path");
    };
    let module = if parts.len() == 1 {
        first.trim_end_matches(".rs").to_owned()
    } else {
        (*first).to_owned()
    };

    let mut path_parts = Vec::new();
    if parts.len() == 1 {
        path_parts.push(module.clone());
    } else {
        for part in &parts[..parts.len() - 1] {
            path_parts.push((*part).to_owned());
        }
        let file_stem = parts[parts.len() - 1].trim_end_matches(".rs");
        if file_stem != "mod" {
            path_parts.push(file_stem.to_owned());
        }
    }

    Ok((module, path_parts.join("::")))
}

pub(crate) fn built_in_item_id(item_name: &str) -> String {
    format!("builtin:{item_name}")
}

pub(crate) fn push_file_plan(
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    path: &str,
    action: PlannedFileAction,
    content: String,
    change_kind: ChangeKind,
) {
    files.push(PlannedFile {
        path: path.to_owned(),
        action,
        content,
    });
    changes.push(ChangeRecord::new(change_kind, path, true));
}

pub(crate) fn empty_lock_json(
    config_content: &str,
    state_path: &str,
) -> Result<String, CodegenError> {
    lock_to_json_at_path(
        &InstallLock::empty(hash_bytes(config_content.as_bytes())),
        Path::new(state_path),
    )
}
