use std::{path::Path, sync::Arc};

use crate::path_safety::PlanningContext;
#[cfg(test)]
use crate::path_safety::capture_plan_snapshot;
use crate::{
    ChangeKind, ChangeRecord, CodegenError, PathPreimage, PlanSnapshot, PlannedFile,
    validate_planned_write_paths,
};

use super::{
    fs::{FsOps, SystemFs},
    lock::WriteLock,
};

pub(crate) fn apply_planned_files_locked(
    context: &PlanningContext,
    lock: &WriteLock,
    files: &[PlannedFile],
    changes: &[ChangeRecord],
    snapshot: &PlanSnapshot,
) -> Result<(), CodegenError> {
    apply_planned_files_locked_with(context, lock, files, changes, snapshot, Arc::new(SystemFs))
}

#[cfg(test)]
pub(crate) fn apply_planned_files_with(
    project_root: &Path,
    files: &[PlannedFile],
    changes: &[ChangeRecord],
    fs: Arc<dyn FsOps>,
) -> Result<(), CodegenError> {
    let snapshot = capture_plan_snapshot(project_root, files.iter().map(|file| &file.path))?;
    apply_planned_files_with_snapshot(project_root, files, changes, &snapshot, fs)
}

#[cfg(test)]
pub(crate) fn apply_planned_files_with_snapshot(
    project_root: &Path,
    files: &[PlannedFile],
    changes: &[ChangeRecord],
    snapshot: &PlanSnapshot,
    fs: Arc<dyn FsOps>,
) -> Result<(), CodegenError> {
    let transaction = PlanningContext::open(project_root)?;
    let lock = WriteLock::acquire_with_context_and_fs(&transaction, Arc::clone(&fs))?;
    apply_planned_files_locked_with(&transaction, &lock, files, changes, snapshot, fs)
}

fn apply_planned_files_locked_with(
    transaction: &PlanningContext,
    lock: &WriteLock,
    files: &[PlannedFile],
    changes: &[ChangeRecord],
    snapshot: &PlanSnapshot,
    fs: Arc<dyn FsOps>,
) -> Result<(), CodegenError> {
    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let lock_paths = lock_file_write_paths(changes);
    validate_planned_write_paths(&paths)?;
    for path in &paths {
        if snapshot.preimage(path).is_none() {
            return Err(CodegenError::PreimageConflict {
                path: path.clone(),
                reason: "planned target has no recorded preimage".to_owned(),
            });
        }
    }

    snapshot.revalidate_all(transaction)?;

    if files.is_empty() {
        return Ok(());
    }

    for file in files
        .iter()
        .filter(|file| !lock_paths.contains(&file.path.as_str()))
    {
        write_file_atomic_with(
            transaction,
            lock,
            &file.path,
            file.content.as_bytes(),
            snapshot,
            fs.as_ref(),
        )?;
    }

    for lock_path in lock_paths {
        if let Some(lock_file) = files.iter().find(|file| file.path == lock_path) {
            write_file_atomic_with(
                transaction,
                lock,
                &lock_file.path,
                lock_file.content.as_bytes(),
                snapshot,
                fs.as_ref(),
            )?;
        }
    }

    Ok(())
}

pub fn write_file_atomic(
    project_root: &Path,
    logical_path: &str,
    content: &[u8],
) -> Result<(), CodegenError> {
    let transaction = PlanningContext::open(project_root)?;
    let lock = WriteLock::acquire_with_context(&transaction)?;
    transaction.observe_path(logical_path)?;
    let snapshot = transaction.finish_snapshot();
    write_file_atomic_with(
        &transaction,
        &lock,
        logical_path,
        content,
        &snapshot,
        &SystemFs,
    )
}

fn write_file_atomic_with(
    transaction: &PlanningContext,
    _lock: &WriteLock,
    logical_path: &str,
    content: &[u8],
    snapshot: &PlanSnapshot,
    fs: &dyn FsOps,
) -> Result<(), CodegenError> {
    snapshot.revalidate_path(transaction, logical_path)?;
    transaction.ensure_parent(logical_path)?;
    let transaction_root = transaction.project_root();
    let full_path = transaction_root.join(logical_path);

    let temp_path = full_path.with_extension("leptos-ui-kit.tmp");
    let temp_logical = temp_path
        .strip_prefix(transaction_root)
        .expect("temporary is below transaction root")
        .to_str()
        .ok_or_else(|| CodegenError::UnsafePath {
            path: logical_path.to_owned(),
            reason: "temporary path is not UTF-8".to_owned(),
        })?;
    transaction.validate_auxiliary_path(temp_logical)?;
    let (stage_parent, temp_name) = transaction.open_parent(temp_logical)?;
    let temp_file = fs
        .write_file(&stage_parent, Path::new(&temp_name), &temp_path, content)
        .map_err(|source| CodegenError::Io {
            path: temp_path.clone(),
            source,
        })?;
    preserve_preimage_mode(&temp_file, &temp_path, snapshot.preimage(logical_path))?;
    drop(temp_file);
    fs.before_final_revalidation(&full_path)
        .map_err(|source| CodegenError::Io {
            path: full_path.clone(),
            source,
        })?;
    snapshot.revalidate_path(transaction, logical_path)?;
    fs.after_final_revalidation(&full_path)
        .map_err(|source| CodegenError::Io {
            path: full_path.clone(),
            source,
        })?;
    transaction.validate_auxiliary_path(temp_logical)?;
    let (commit_parent, target_name) = transaction.open_parent(logical_path)?;
    transaction.ensure_same_directory(logical_path, &stage_parent, &commit_parent)?;
    snapshot.revalidate_path(transaction, logical_path)?;
    fs.rename(
        &stage_parent,
        Path::new(&temp_name),
        &temp_path,
        &commit_parent,
        Path::new(&target_name),
        &full_path,
    )
    .map_err(|source| CodegenError::Io {
        path: full_path,
        source,
    })?;
    Ok(())
}

fn preserve_preimage_mode(
    temp_file: &cap_std::fs::File,
    temp_path: &Path,
    preimage: Option<&PathPreimage>,
) -> Result<(), CodegenError> {
    let Some(PathPreimage::RegularFile { mode, .. }) = preimage else {
        return Ok(());
    };
    let mut permissions = temp_file
        .metadata()
        .map_err(|source| CodegenError::Io {
            path: temp_path.to_path_buf(),
            source,
        })?
        .permissions();
    #[cfg(unix)]
    if let Some(posix_mode) = mode.posix_mode {
        use cap_std::fs::PermissionsExt;
        permissions.set_mode(posix_mode);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(mode.readonly);
    temp_file
        .set_permissions(permissions)
        .map_err(|source| CodegenError::Io {
            path: temp_path.to_path_buf(),
            source,
        })
}

fn lock_file_write_paths(changes: &[ChangeRecord]) -> Vec<&str> {
    changes
        .iter()
        .filter(|change| change.kind == ChangeKind::WriteLockFile)
        .map(|change| change.path.as_str())
        .collect()
}
