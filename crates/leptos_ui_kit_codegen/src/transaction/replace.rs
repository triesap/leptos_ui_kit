use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_std::fs::Dir;

use crate::path_safety::PlanningContext;
#[cfg(test)]
use crate::path_safety::capture_plan_snapshot;
use crate::{
    ChangeKind, ChangeRecord, CodegenError, PathPreimage, PlanSnapshot, PlannedFile,
    validate_planned_write_paths,
};

use super::{
    fs::{CreatedFile, FsOps, HardLinkEndpoint, SystemFs, current_regular_file_identity},
    lock::WriteLock,
};

const AUXILIARY_RANDOM_BYTES: usize = 16;
const AUXILIARY_CREATE_ATTEMPTS: usize = 16;
const STAGE_PREFIX: &str = ".leptos-ui-kit-stage-";
const BACKUP_PREFIX: &str = ".leptos-ui-kit-backup-";

struct AuxiliaryFile {
    name: String,
    path: PathBuf,
}

struct StagedFile {
    logical_path: String,
    target_path: PathBuf,
    target_name: String,
    parent: Dir,
    stage: AuxiliaryFile,
    stage_identity: (u64, u64),
    backup: Option<AuxiliaryFile>,
}

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
    _lock: &WriteLock,
    files: &[PlannedFile],
    changes: &[ChangeRecord],
    snapshot: &PlanSnapshot,
    fs: Arc<dyn FsOps>,
) -> Result<(), CodegenError> {
    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    validate_planned_write_paths(&paths)?;
    validate_preimages(&paths, snapshot)?;
    snapshot.revalidate_all(transaction)?;

    if files.is_empty() {
        return Ok(());
    }

    let ordered = ordered_files(files, changes);
    let mut staged = Vec::with_capacity(ordered.len());
    for file in ordered {
        match stage_bytes(
            transaction,
            &file.path,
            file.content.as_bytes(),
            snapshot,
            fs.as_ref(),
        ) {
            Ok(file) => staged.push(file),
            Err(error) => {
                cleanup_uncommitted_auxiliaries(fs.as_ref(), &staged);
                return Err(error);
            }
        }
    }

    if let Err(error) = snapshot.revalidate_all(transaction) {
        cleanup_uncommitted_auxiliaries(fs.as_ref(), &staged);
        return Err(error);
    }

    for index in 0..staged.len() {
        match backup_file(transaction, &staged[index], snapshot, fs.as_ref()) {
            Ok(backup) => staged[index].backup = backup,
            Err(error) => {
                cleanup_uncommitted_auxiliaries(fs.as_ref(), &staged);
                return Err(error);
            }
        }
    }

    if let Err(error) = snapshot.revalidate_all(transaction) {
        cleanup_uncommitted_auxiliaries(fs.as_ref(), &staged);
        return Err(error);
    }

    for file in &staged {
        commit_staged_file(transaction, file, snapshot, fs.as_ref())?;
    }

    cleanup_successful_backups(fs.as_ref(), &staged)?;
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
    let fs = SystemFs;
    validate_preimages(&[logical_path.to_owned()], &snapshot)?;
    snapshot.revalidate_all(&transaction)?;
    let mut staged = stage_bytes(&transaction, logical_path, content, &snapshot, &fs)?;
    if let Err(error) = snapshot.revalidate_all(&transaction) {
        cleanup_uncommitted_auxiliaries(&fs, std::slice::from_ref(&staged));
        return Err(error);
    }
    match backup_file(&transaction, &staged, &snapshot, &fs) {
        Ok(backup) => staged.backup = backup,
        Err(error) => {
            cleanup_uncommitted_auxiliaries(&fs, std::slice::from_ref(&staged));
            return Err(error);
        }
    }
    if let Err(error) = snapshot.revalidate_all(&transaction) {
        cleanup_uncommitted_auxiliaries(&fs, std::slice::from_ref(&staged));
        return Err(error);
    }
    commit_staged_file(&transaction, &staged, &snapshot, &fs)?;
    cleanup_successful_backups(&fs, std::slice::from_ref(&staged))?;
    drop(lock);
    Ok(())
}

fn validate_preimages(paths: &[String], snapshot: &PlanSnapshot) -> Result<(), CodegenError> {
    for path in paths {
        let Some(preimage) = snapshot.preimage(path) else {
            return Err(CodegenError::PreimageConflict {
                path: path.clone(),
                reason: "planned target has no recorded preimage".to_owned(),
            });
        };
        if let PathPreimage::RegularFile { mode, .. } = preimage
            && mode.readonly
        {
            return Err(CodegenError::PreimageConflict {
                path: path.clone(),
                reason: "planned target is readonly".to_owned(),
            });
        }
    }
    Ok(())
}

fn ordered_files<'a>(files: &'a [PlannedFile], changes: &[ChangeRecord]) -> Vec<&'a PlannedFile> {
    let lock_paths = changes
        .iter()
        .filter(|change| change.kind == ChangeKind::WriteLockFile)
        .map(|change| change.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut ordered = files.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        let left_is_lock = lock_paths.contains(left.path.as_str());
        let right_is_lock = lock_paths.contains(right.path.as_str());
        left_is_lock
            .cmp(&right_is_lock)
            .then_with(|| left.path.cmp(&right.path))
    });
    ordered
}

fn stage_bytes(
    transaction: &PlanningContext,
    logical_path: &str,
    content: &[u8],
    snapshot: &PlanSnapshot,
    fs: &dyn FsOps,
) -> Result<StagedFile, CodegenError> {
    transaction.ensure_parent(logical_path)?;
    let (parent, target_name) = transaction.open_parent(logical_path)?;
    let target_path = transaction.project_root().join(logical_path);
    let (stage, mut created) =
        create_random_auxiliary(transaction, fs, &parent, logical_path, STAGE_PREFIX, 0o600)?;

    let result = (|| {
        fs.write_handle(&mut created.file, &stage.path, content)
            .map_err(|source| CodegenError::Io {
                path: stage.path.clone(),
                source,
            })?;
        preserve_preimage_mode(fs, &created, &stage.path, snapshot.preimage(logical_path))?;
        fs.sync_handle(&created.file, &stage.path)
            .map_err(|source| CodegenError::Io {
                path: stage.path.clone(),
                source,
            })?;
        Ok(())
    })();
    let stage_identity = created.identity;
    drop(created.file);
    if let Err(error) = result {
        let _ = fs.remove_file(&parent, Path::new(&stage.name), &stage.path);
        return Err(error);
    }

    Ok(StagedFile {
        logical_path: logical_path.to_owned(),
        target_path,
        target_name,
        parent,
        stage,
        stage_identity,
        backup: None,
    })
}

fn backup_file(
    transaction: &PlanningContext,
    file: &StagedFile,
    snapshot: &PlanSnapshot,
    fs: &dyn FsOps,
) -> Result<Option<AuxiliaryFile>, CodegenError> {
    if matches!(
        snapshot.preimage(&file.logical_path),
        Some(PathPreimage::Absent)
    ) {
        return Ok(None);
    }

    snapshot.revalidate_path(transaction, &file.logical_path)?;
    for _ in 0..AUXILIARY_CREATE_ATTEMPTS {
        let backup = random_auxiliary_path(transaction, &file.logical_path, BACKUP_PREFIX)?;
        let endpoint = HardLinkEndpoint::new(&file.parent, Path::new(&backup.name), &backup.path);
        match fs.hard_link(
            &[],
            HardLinkEndpoint::new(
                &file.parent,
                Path::new(&file.target_name),
                &file.target_path,
            ),
            endpoint,
        ) {
            Ok(()) => {
                if let Err(error) = snapshot.revalidate_path(transaction, &file.logical_path) {
                    let _ = fs.remove_file(&file.parent, Path::new(&backup.name), &backup.path);
                    return Err(error);
                }
                return Ok(Some(backup));
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(CodegenError::Io {
                    path: backup.path,
                    source,
                });
            }
        }
    }
    Err(auxiliary_collision_error(&file.target_path, "backup"))
}

fn commit_staged_file(
    transaction: &PlanningContext,
    file: &StagedFile,
    snapshot: &PlanSnapshot,
    fs: &dyn FsOps,
) -> Result<(), CodegenError> {
    fs.before_final_revalidation(&file.target_path)
        .map_err(|source| CodegenError::Io {
            path: file.target_path.clone(),
            source,
        })?;
    snapshot.revalidate_path(transaction, &file.logical_path)?;
    fs.after_final_revalidation(&file.target_path)
        .map_err(|source| CodegenError::Io {
            path: file.target_path.clone(),
            source,
        })?;
    snapshot.revalidate_path(transaction, &file.logical_path)?;
    let actual_stage_identity =
        current_regular_file_identity(&file.parent, Path::new(&file.stage.name)).map_err(
            |source| CodegenError::UnsafePath {
                path: file.stage.path.display().to_string(),
                reason: format!("transaction stage changed before commit: {source}"),
            },
        )?;
    if actual_stage_identity != file.stage_identity {
        return Err(CodegenError::UnsafePath {
            path: file.stage.path.display().to_string(),
            reason: "transaction stage changed identity before commit".to_owned(),
        });
    }
    let (commit_parent, target_name) = transaction.open_parent(&file.logical_path)?;
    transaction.ensure_same_directory(&file.logical_path, &file.parent, &commit_parent)?;
    snapshot.revalidate_path(transaction, &file.logical_path)?;
    fs.rename(
        &file.parent,
        Path::new(&file.stage.name),
        &file.stage.path,
        &commit_parent,
        Path::new(&target_name),
        &file.target_path,
    )
    .map_err(|source| CodegenError::Io {
        path: file.target_path.clone(),
        source,
    })
}

fn create_random_auxiliary(
    transaction: &PlanningContext,
    fs: &dyn FsOps,
    expected_parent: &Dir,
    logical_target: &str,
    prefix: &str,
    mode: u32,
) -> Result<(AuxiliaryFile, CreatedFile), CodegenError> {
    for _ in 0..AUXILIARY_CREATE_ATTEMPTS {
        let auxiliary = random_auxiliary_path(transaction, logical_target, prefix)?;
        match fs.create_new_file(
            expected_parent,
            Path::new(&auxiliary.name),
            &auxiliary.path,
            mode,
        ) {
            Ok(created) => return Ok((auxiliary, created)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(CodegenError::Io {
                    path: auxiliary.path,
                    source,
                });
            }
        }
    }
    Err(auxiliary_collision_error(
        &transaction.project_root().join(logical_target),
        "stage",
    ))
}

fn random_auxiliary_path(
    transaction: &PlanningContext,
    logical_target: &str,
    prefix: &str,
) -> Result<AuxiliaryFile, CodegenError> {
    let mut random = [0_u8; AUXILIARY_RANDOM_BYTES];
    getrandom::fill(&mut random).map_err(|error| CodegenError::Io {
        path: transaction.project_root().join(logical_target),
        source: io::Error::other(format!("generate random transaction filename: {error}")),
    })?;
    let mut name = String::with_capacity(prefix.len() + random.len() * 2);
    name.push_str(prefix);
    for byte in random {
        use std::fmt::Write as _;
        write!(&mut name, "{byte:02x}").expect("writing to String cannot fail");
    }
    let parent = Path::new(logical_target)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let path = transaction.project_root().join(parent).join(&name);
    Ok(AuxiliaryFile { name, path })
}

fn preserve_preimage_mode(
    fs: &dyn FsOps,
    created: &CreatedFile,
    stage_path: &Path,
    preimage: Option<&PathPreimage>,
) -> Result<(), CodegenError> {
    let Some(PathPreimage::RegularFile { mode, .. }) = preimage else {
        return Ok(());
    };
    #[cfg(unix)]
    if let Some(posix_mode) = mode.posix_mode {
        fs.set_file_mode(&created.file, stage_path, posix_mode)
            .map_err(|source| CodegenError::Io {
                path: stage_path.to_path_buf(),
                source,
            })?;
    }
    #[cfg(not(unix))]
    let _ = (fs, created, stage_path, mode);
    Ok(())
}

fn cleanup_uncommitted_auxiliaries(fs: &dyn FsOps, staged: &[StagedFile]) {
    for file in staged.iter().rev() {
        if let Some(backup) = &file.backup {
            let _ = fs.remove_file(&file.parent, Path::new(&backup.name), &backup.path);
        }
        let _ = fs.remove_file(&file.parent, Path::new(&file.stage.name), &file.stage.path);
    }
}

fn cleanup_successful_backups(fs: &dyn FsOps, staged: &[StagedFile]) -> Result<(), CodegenError> {
    for file in staged.iter().rev() {
        if let Some(backup) = &file.backup {
            fs.remove_file(&file.parent, Path::new(&backup.name), &backup.path)
                .map_err(|source| CodegenError::Io {
                    path: backup.path.clone(),
                    source,
                })?;
        }
    }
    Ok(())
}

fn auxiliary_collision_error(target: &Path, kind: &str) -> CodegenError {
    CodegenError::Io {
        path: target.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("could not allocate an exclusive random {kind} filename"),
        ),
    }
}
