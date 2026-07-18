use std::{path::Path, sync::Arc};

use crate::{
    ChangeKind, ChangeRecord, CodegenError, PlannedFile, validate_planned_write_paths,
    validate_project_write_path,
};

use super::{
    fs::{FsOps, SystemFs},
    lock::WriteLock,
};

pub(crate) fn apply_planned_files(
    project_root: &Path,
    files: &[PlannedFile],
    changes: &[ChangeRecord],
) -> Result<(), CodegenError> {
    apply_planned_files_with(project_root, files, changes, Arc::new(SystemFs))
}

pub(crate) fn apply_planned_files_with(
    project_root: &Path,
    files: &[PlannedFile],
    changes: &[ChangeRecord],
    fs: Arc<dyn FsOps>,
) -> Result<(), CodegenError> {
    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let lock_paths = lock_file_write_paths(changes);
    validate_planned_write_paths(&paths)?;

    if files.is_empty() {
        return Ok(());
    }

    let _lock = WriteLock::acquire_with(project_root, Arc::clone(&fs))?;

    for file in files
        .iter()
        .filter(|file| !lock_paths.contains(&file.path.as_str()))
    {
        write_file_atomic_with(
            project_root,
            &file.path,
            file.content.as_bytes(),
            fs.as_ref(),
        )?;
    }

    for lock_path in lock_paths {
        if let Some(lock_file) = files.iter().find(|file| file.path == lock_path) {
            write_file_atomic_with(
                project_root,
                &lock_file.path,
                lock_file.content.as_bytes(),
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
    write_file_atomic_with(project_root, logical_path, content, &SystemFs)
}

fn write_file_atomic_with(
    project_root: &Path,
    logical_path: &str,
    content: &[u8],
    fs: &dyn FsOps,
) -> Result<(), CodegenError> {
    let full_path = validate_project_write_path(project_root, logical_path)?;
    if let Some(parent) = full_path.parent() {
        fs.create_dir_all(parent)
            .map_err(|source| CodegenError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
    }

    let temp_path = full_path.with_extension("leptos-ui-kit.tmp");
    fs.write_file(&temp_path, content)
        .map_err(|source| CodegenError::Io {
            path: temp_path.clone(),
            source,
        })?;
    fs.rename(&temp_path, &full_path)
        .map_err(|source| CodegenError::Io {
            path: full_path,
            source,
        })?;
    Ok(())
}

fn lock_file_write_paths(changes: &[ChangeRecord]) -> Vec<&str> {
    changes
        .iter()
        .filter(|change| change.kind == ChangeKind::WriteLockFile)
        .map(|change| change.path.as_str())
        .collect()
}
