use std::{
    collections::{BTreeSet, HashSet},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
#[cfg(unix)]
use cap_std::fs::DirBuilder;
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};

use crate::path_safety::PlanningContext;
#[cfg(test)]
use crate::path_safety::capture_plan_snapshot;
use crate::{
    ChangeKind, ChangeRecord, CodegenError, PathPreimage, PlanSnapshot, PlannedFile,
    PlannedFileAction, validate_planned_write_paths,
};

use super::{
    fs::{CreatedFile, FsOps, HardLinkEndpoint, SystemFs, current_regular_file_identity},
    lock::WriteLock,
};

const AUXILIARY_RANDOM_BYTES: usize = 16;
const STAGE_PREFIX: &str = ".leptos-ui-kit-stage-";
const BACKUP_PREFIX: &str = ".leptos-ui-kit-backup-";
const KIT_DIRECTORY_PATH: &str = "src/components/ui/_kit";
const TRANSACTIONS_DIRECTORY_NAME: &str = ".transactions";
const TRANSACTION_JOURNAL_VERSION: u32 = 2;
const TRANSACTION_JOURNAL_PREFIX: &str = "transaction-";
const TRANSACTION_JOURNAL_SUFFIX: &str = ".json";
const JOURNAL_INTENT_PREFIX: &str = "journal-intent-";
const JOURNAL_UPDATE_PREFIX: &str = "journal-update-";

struct AuxiliaryFile {
    name: String,
    path: PathBuf,
    identity: (u64, u64),
    content_hash: String,
    length: u64,
    posix_mode: Option<u32>,
}

struct StagedFile {
    logical_path: String,
    target_path: PathBuf,
    target_name: String,
    parent: Dir,
    stage: AuxiliaryFile,
    planned_hash: String,
    planned_length: u64,
    planned_posix_mode: Option<u32>,
    preimage: JournalPreimage,
    backup: Option<AuxiliaryFile>,
    created_directories: Vec<JournalDirectory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransactionJournalData {
    version: u32,
    transaction_id: String,
    project: JournalProject,
    state: JournalState,
    entries: Vec<JournalEntry>,
    created_directories: Vec<JournalDirectory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JournalProject {
    canonical_root: String,
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum JournalState {
    Intent,
    Preparing { index: usize },
    Prepared,
    Replacing { index: usize },
    Committed { count: usize },
    RollingBack { count: usize },
    Applied,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JournalEntry {
    ordinal: usize,
    logical_path: String,
    action: JournalAction,
    stage_name: String,
    stage_identity: Option<JournalIdentity>,
    stage_hash: Option<String>,
    stage_length: Option<u64>,
    stage_posix_mode: Option<u32>,
    backup_name: Option<String>,
    backup_identity: Option<JournalIdentity>,
    backup_hash: Option<String>,
    backup_length: Option<u64>,
    backup_posix_mode: Option<u32>,
    preimage: JournalPreimage,
    planned_hash: String,
    planned_length: u64,
    planned_posix_mode: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum JournalAction {
    Create,
    Update,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JournalIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JournalDirectory {
    logical_path: String,
    identity: Option<JournalIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum JournalPreimage {
    Absent,
    RegularFile {
        content_hash: String,
        readonly: bool,
        posix_mode: Option<u32>,
    },
}

struct DurableJournal {
    kit_directory: Dir,
    transactions_directory: Dir,
    transactions_path: PathBuf,
    name: String,
    path: PathBuf,
    data: TransactionJournalData,
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
    validate_actions(files, snapshot)?;
    snapshot.revalidate_all(transaction)?;

    if files.is_empty() {
        return Ok(());
    }

    let ordered = ordered_files(files, changes)?;
    let mut journal = DurableJournal::create_intent(transaction, fs.as_ref(), &ordered, snapshot)?;
    let mut staged = Vec::with_capacity(ordered.len());
    for (index, file) in ordered.into_iter().enumerate() {
        journal.data.state = JournalState::Preparing { index };
        if let Err(error) = journal.persist(fs.as_ref()) {
            return Err(rollback_or_recovery_required(
                transaction,
                fs.as_ref(),
                &staged,
                journal,
                0,
                error,
            ));
        }
        let stage_name = journal.data.entries[index].stage_name.clone();
        match stage_bytes(
            transaction,
            &file.path,
            file.content.as_bytes(),
            snapshot,
            fs.as_ref(),
            &stage_name,
        ) {
            Ok(staged_file) => {
                record_stage(&mut journal.data.entries[index], &staged_file);
                merge_created_directories(
                    &mut journal.data.created_directories,
                    &staged_file.created_directories,
                );
                staged.push(staged_file);
                if let Err(error) = journal.persist(fs.as_ref()) {
                    return Err(rollback_or_recovery_required(
                        transaction,
                        fs.as_ref(),
                        &staged,
                        journal,
                        0,
                        error,
                    ));
                }
            }
            Err(error) => {
                return Err(rollback_or_recovery_required(
                    transaction,
                    fs.as_ref(),
                    &staged,
                    journal,
                    0,
                    error,
                ));
            }
        }
    }

    if let Err(error) = snapshot.revalidate_all(transaction) {
        return Err(rollback_or_recovery_required(
            transaction,
            fs.as_ref(),
            &staged,
            journal,
            0,
            error,
        ));
    }

    for index in 0..staged.len() {
        let backup_name = journal.data.entries[index].backup_name.clone();
        match backup_file(
            transaction,
            &staged[index],
            snapshot,
            fs.as_ref(),
            backup_name.as_deref(),
        ) {
            Ok(backup) => {
                staged[index].backup = backup;
                record_backup(&mut journal.data.entries[index], &staged[index]);
                if let Err(error) = journal.persist(fs.as_ref()) {
                    return Err(rollback_or_recovery_required(
                        transaction,
                        fs.as_ref(),
                        &staged,
                        journal,
                        0,
                        error,
                    ));
                }
            }
            Err(error) => {
                return Err(rollback_or_recovery_required(
                    transaction,
                    fs.as_ref(),
                    &staged,
                    journal,
                    0,
                    error,
                ));
            }
        }
    }

    if let Err(error) = snapshot.revalidate_all(transaction) {
        return Err(rollback_or_recovery_required(
            transaction,
            fs.as_ref(),
            &staged,
            journal,
            0,
            error,
        ));
    }

    journal.data.state = JournalState::Prepared;
    if let Err(error) = journal.persist(fs.as_ref()) {
        return Err(rollback_or_recovery_required(
            transaction,
            fs.as_ref(),
            &staged,
            journal,
            0,
            error,
        ));
    }
    commit_staged_cohort(transaction, staged, snapshot, fs.as_ref(), journal)
}

fn commit_staged_cohort(
    transaction: &PlanningContext,
    staged: Vec<StagedFile>,
    snapshot: &PlanSnapshot,
    fs: &dyn FsOps,
    mut journal: DurableJournal,
) -> Result<(), CodegenError> {
    let mut committed = 0;
    for (index, file) in staged.iter().enumerate() {
        journal.data.state = JournalState::Replacing { index };
        if let Err(error) = journal.persist(fs) {
            return Err(rollback_or_recovery_required(
                transaction,
                fs,
                &staged,
                journal,
                committed,
                error,
            ));
        }
        if let Err(error) = commit_staged_file(transaction, file, snapshot, fs) {
            return Err(rollback_or_recovery_required(
                transaction,
                fs,
                &staged,
                journal,
                committed,
                error,
            ));
        }
        committed += 1;
        journal.data.state = JournalState::Committed { count: committed };
        if let Err(error) = journal.persist(fs) {
            return Err(rollback_or_recovery_required(
                transaction,
                fs,
                &staged,
                journal,
                committed,
                error,
            ));
        }
    }

    journal.data.state = JournalState::Applied;
    if let Err(error) = journal.persist(fs) {
        return Err(rollback_or_recovery_required(
            transaction,
            fs,
            &staged,
            journal,
            committed,
            error,
        ));
    }
    finish_successful_transaction(fs, &staged, journal)
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
    let mut journal =
        DurableJournal::create_atomic_intent(&transaction, &fs, logical_path, content, &snapshot)?;
    journal.data.state = JournalState::Preparing { index: 0 };
    journal.persist(&fs)?;
    let stage_name = journal.data.entries[0].stage_name.clone();
    let mut staged = match stage_bytes(
        &transaction,
        logical_path,
        content,
        &snapshot,
        &fs,
        &stage_name,
    ) {
        Ok(staged) => staged,
        Err(error) => {
            return Err(rollback_or_recovery_required(
                &transaction,
                &fs,
                &[],
                journal,
                0,
                error,
            ));
        }
    };
    record_stage(&mut journal.data.entries[0], &staged);
    merge_created_directories(
        &mut journal.data.created_directories,
        &staged.created_directories,
    );
    if let Err(error) = journal.persist(&fs) {
        return Err(rollback_or_recovery_required(
            &transaction,
            &fs,
            std::slice::from_ref(&staged),
            journal,
            0,
            error,
        ));
    }
    if let Err(error) = snapshot.revalidate_all(&transaction) {
        return Err(rollback_or_recovery_required(
            &transaction,
            &fs,
            std::slice::from_ref(&staged),
            journal,
            0,
            error,
        ));
    }
    let backup_name = journal.data.entries[0].backup_name.clone();
    match backup_file(
        &transaction,
        &staged,
        &snapshot,
        &fs,
        backup_name.as_deref(),
    ) {
        Ok(backup) => staged.backup = backup,
        Err(error) => {
            return Err(rollback_or_recovery_required(
                &transaction,
                &fs,
                std::slice::from_ref(&staged),
                journal,
                0,
                error,
            ));
        }
    }
    record_backup(&mut journal.data.entries[0], &staged);
    if let Err(error) = journal.persist(&fs) {
        return Err(rollback_or_recovery_required(
            &transaction,
            &fs,
            std::slice::from_ref(&staged),
            journal,
            0,
            error,
        ));
    }
    if let Err(error) = snapshot.revalidate_all(&transaction) {
        return Err(rollback_or_recovery_required(
            &transaction,
            &fs,
            std::slice::from_ref(&staged),
            journal,
            0,
            error,
        ));
    }
    journal.data.state = JournalState::Prepared;
    if let Err(error) = journal.persist(&fs) {
        return Err(rollback_or_recovery_required(
            &transaction,
            &fs,
            std::slice::from_ref(&staged),
            journal,
            0,
            error,
        ));
    }
    commit_staged_cohort(&transaction, vec![staged], &snapshot, &fs, journal)?;
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

fn validate_actions(files: &[PlannedFile], snapshot: &PlanSnapshot) -> Result<(), CodegenError> {
    for file in files {
        let expected = match snapshot.preimage(&file.path) {
            Some(PathPreimage::Absent) => PlannedFileAction::Create,
            Some(PathPreimage::RegularFile { .. }) => PlannedFileAction::Update,
            None => continue,
        };
        if file.action != expected {
            return Err(CodegenError::PreimageConflict {
                path: file.path.clone(),
                reason: format!(
                    "planned {:?} action disagrees with the observed {:?} preimage action",
                    file.action, expected
                ),
            });
        }
    }
    Ok(())
}

fn ordered_files<'a>(
    files: &'a [PlannedFile],
    changes: &[ChangeRecord],
) -> Result<Vec<&'a PlannedFile>, CodegenError> {
    let lock_paths = changes
        .iter()
        .filter(|change| change.kind == ChangeKind::WriteLockFile)
        .map(|change| change.path.as_str())
        .collect::<Vec<_>>();
    let unique_lock_paths = lock_paths.iter().copied().collect::<BTreeSet<_>>();
    if lock_paths.len() != unique_lock_paths.len() || unique_lock_paths.len() > 1 {
        return Err(CodegenError::PreimageConflict {
            path: "<transaction cohort>".to_owned(),
            reason: "install-lock change records must select at most one unique path".to_owned(),
        });
    }
    if let Some(lock_path) = unique_lock_paths.first()
        && !files.iter().any(|file| file.path == **lock_path)
    {
        return Err(CodegenError::PreimageConflict {
            path: (*lock_path).to_owned(),
            reason: "install-lock change record is not part of the replacement cohort".to_owned(),
        });
    }
    let cohort_contains_install_lock = files
        .iter()
        .any(|file| file.path == crate::DEFAULT_KIT_LOCK_PATH);
    if cohort_contains_install_lock != unique_lock_paths.contains(crate::DEFAULT_KIT_LOCK_PATH) {
        return Err(CodegenError::PreimageConflict {
            path: crate::DEFAULT_KIT_LOCK_PATH.to_owned(),
            reason: "the install lock must have exactly one matching lock-last change record"
                .to_owned(),
        });
    }
    let mut ordered = files.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        let left_is_lock = unique_lock_paths.contains(left.path.as_str());
        let right_is_lock = unique_lock_paths.contains(right.path.as_str());
        left_is_lock
            .cmp(&right_is_lock)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(ordered)
}

fn stage_bytes(
    transaction: &PlanningContext,
    logical_path: &str,
    content: &[u8],
    snapshot: &PlanSnapshot,
    fs: &dyn FsOps,
    stage_name: &str,
) -> Result<StagedFile, CodegenError> {
    let created_directory_paths =
        transaction.ensure_parent_with(logical_path, |directory, created| {
            let path = transaction.project_root().join(directory);
            if created {
                fs.after_create_directory(&path)
            } else {
                fs.before_create_directory(&path)
            }
            .map_err(|source| {
                filesystem_operation_error("prepare parent directory", directory, path, source)
            })
        })?;
    let (parent, target_name) = transaction.open_parent(logical_path)?;
    let target_path = transaction.project_root().join(logical_path);
    let stage_path = target_path
        .parent()
        .expect("validated target has a parent")
        .join(stage_name);
    let creation_mode = 0o600;
    let mut created = fs
        .create_new_file(&parent, Path::new(stage_name), &stage_path, creation_mode)
        .map_err(|source| {
            filesystem_operation_error(
                "create exclusive transaction stage",
                logical_path,
                stage_path.clone(),
                source,
            )
        })?;

    let result = (|| {
        fs.write_handle(&mut created.file, &stage_path, content)
            .map_err(|source| {
                filesystem_operation_error(
                    "write transaction stage",
                    logical_path,
                    stage_path.clone(),
                    source,
                )
            })?;
        apply_publication_mode(
            fs,
            &created,
            logical_path,
            &stage_path,
            snapshot.preimage(logical_path),
        )?;
        fs.sync_handle(&created.file, &stage_path)
            .map_err(|source| {
                filesystem_operation_error(
                    "sync transaction stage",
                    logical_path,
                    stage_path.clone(),
                    source,
                )
            })?;
        Ok(())
    })();
    let stage_identity = created.identity;
    let stage_posix_mode = opened_posix_mode(&created.file).map_err(|source| {
        filesystem_operation_error(
            "inspect transaction stage mode",
            logical_path,
            stage_path.clone(),
            source,
        )
    })?;
    drop(created.file);
    if let Err(error) = result {
        let _ = fs.remove_file(&parent, Path::new(stage_name), &stage_path);
        cleanup_created_directory_paths(transaction, fs, &created_directory_paths);
        return Err(error);
    }

    let created_directories = created_directory_paths
        .iter()
        .map(|logical_path| {
            let directory = transaction.open_directory(logical_path)?;
            let metadata = directory
                .dir_metadata()
                .map_err(|source| CodegenError::Io {
                    path: transaction.project_root().join(logical_path),
                    source,
                })?;
            Ok(JournalDirectory {
                logical_path: logical_path.clone(),
                identity: Some(JournalIdentity {
                    device: MetadataExt::dev(&metadata),
                    inode: MetadataExt::ino(&metadata),
                }),
            })
        })
        .collect::<Result<Vec<_>, CodegenError>>()?;

    let planned_hash = crate::hash_content_bytes(content);
    let preimage = match snapshot
        .preimage(logical_path)
        .expect("staged target has a recorded preimage")
    {
        PathPreimage::Absent => JournalPreimage::Absent,
        PathPreimage::RegularFile { content_hash, mode } => JournalPreimage::RegularFile {
            content_hash: content_hash.clone(),
            readonly: mode.readonly,
            posix_mode: mode.posix_mode,
        },
    };
    let stage = AuxiliaryFile {
        name: stage_name.to_owned(),
        path: stage_path,
        identity: stage_identity,
        content_hash: planned_hash.clone(),
        length: content.len() as u64,
        posix_mode: stage_posix_mode,
    };

    Ok(StagedFile {
        logical_path: logical_path.to_owned(),
        target_path,
        target_name,
        parent,
        stage,
        planned_hash,
        planned_length: content.len() as u64,
        planned_posix_mode: stage_posix_mode,
        preimage,
        backup: None,
        created_directories,
    })
}

fn backup_file(
    transaction: &PlanningContext,
    file: &StagedFile,
    snapshot: &PlanSnapshot,
    fs: &dyn FsOps,
    backup_name: Option<&str>,
) -> Result<Option<AuxiliaryFile>, CodegenError> {
    if matches!(
        snapshot.preimage(&file.logical_path),
        Some(PathPreimage::Absent)
    ) {
        return Ok(None);
    }

    let backup_name = backup_name.ok_or_else(|| CodegenError::InvalidCoordinationState {
        path: file.logical_path.clone(),
        reason: "existing target is missing its transaction-bound backup name".to_owned(),
    })?;
    snapshot.revalidate_path(transaction, &file.logical_path)?;
    fs.before_read_handle(&file.target_path).map_err(|source| {
        filesystem_operation_error(
            "prepare transaction backup read",
            &file.logical_path,
            file.target_path.clone(),
            source,
        )
    })?;
    let mut target_options = OpenOptions::new();
    target_options.read(true);
    target_options.follow(FollowSymlinks::No);
    target_options.nonblock(true);
    let mut target = file
        .parent
        .open_with(&file.target_name, &target_options)
        .map_err(|source| {
            filesystem_operation_error(
                "open transaction backup source",
                &file.logical_path,
                file.target_path.clone(),
                source,
            )
        })?;
    let target_metadata = target.metadata().map_err(|source| {
        filesystem_operation_error(
            "inspect transaction backup source",
            &file.logical_path,
            file.target_path.clone(),
            source,
        )
    })?;
    if !target_metadata.is_file() || target_metadata.file_type().is_symlink() {
        return Err(CodegenError::UnsafePath {
            path: file.logical_path.clone(),
            reason: "transaction backup source is not a no-follow regular file".to_owned(),
        });
    }
    let mut content = Vec::new();
    target.read_to_end(&mut content).map_err(|source| {
        filesystem_operation_error(
            "read transaction backup source",
            &file.logical_path,
            file.target_path.clone(),
            source,
        )
    })?;
    let content_hash = crate::hash_content_bytes(&content);
    let expected = snapshot
        .preimage(&file.logical_path)
        .expect("backup path has a validated preimage");
    let expected_mode = match expected {
        PathPreimage::RegularFile {
            content_hash: expected_hash,
            mode,
        } => {
            if &content_hash != expected_hash {
                return Err(CodegenError::PreimageConflict {
                    path: file.logical_path.clone(),
                    reason: "target bytes changed while copying the recovery backup".to_owned(),
                });
            }
            mode.posix_mode
        }
        PathPreimage::Absent => unreachable!("absent targets do not create backups"),
    };
    let backup_path = file
        .target_path
        .parent()
        .expect("validated target has a parent")
        .join(backup_name);
    let mut created = fs
        .create_new_file(&file.parent, Path::new(backup_name), &backup_path, 0o600)
        .map_err(|source| {
            filesystem_operation_error(
                "create independent transaction backup",
                &file.logical_path,
                backup_path.clone(),
                source,
            )
        })?;
    let result = (|| {
        fs.write_handle(&mut created.file, &backup_path, &content)
            .map_err(|source| {
                filesystem_operation_error(
                    "write independent transaction backup",
                    &file.logical_path,
                    backup_path.clone(),
                    source,
                )
            })?;
        #[cfg(unix)]
        if let Some(mode) = expected_mode {
            fs.set_file_mode(&created.file, &backup_path, mode)
                .map_err(|source| {
                    filesystem_operation_error(
                        "preserve transaction backup mode",
                        &file.logical_path,
                        backup_path.clone(),
                        source,
                    )
                })?;
        }
        fs.sync_handle(&created.file, &backup_path)
            .map_err(|source| {
                filesystem_operation_error(
                    "sync independent transaction backup",
                    &file.logical_path,
                    backup_path.clone(),
                    source,
                )
            })?;
        fs.sync_directory(&file.parent, &file.target_path)
            .map_err(|source| {
                filesystem_operation_error(
                    "sync transaction backup directory",
                    &file.logical_path,
                    file.target_path.clone(),
                    source,
                )
            })
    })();
    let identity = created.identity;
    let posix_mode = opened_posix_mode(&created.file).map_err(|source| {
        filesystem_operation_error(
            "inspect transaction backup mode",
            &file.logical_path,
            backup_path.clone(),
            source,
        )
    })?;
    drop(created.file);
    if let Err(error) = result {
        let _ = fs.remove_file(&file.parent, Path::new(backup_name), &backup_path);
        return Err(error);
    }
    snapshot.revalidate_path(transaction, &file.logical_path)?;
    Ok(Some(AuxiliaryFile {
        name: backup_name.to_owned(),
        path: backup_path,
        identity,
        content_hash,
        length: content.len() as u64,
        posix_mode,
    }))
}

fn commit_staged_file(
    transaction: &PlanningContext,
    file: &StagedFile,
    snapshot: &PlanSnapshot,
    fs: &dyn FsOps,
) -> Result<(), CodegenError> {
    fs.before_final_revalidation(&file.target_path)
        .map_err(|source| {
            filesystem_operation_error(
                "prepare final target validation",
                &file.logical_path,
                file.target_path.clone(),
                source,
            )
        })?;
    snapshot.revalidate_path(transaction, &file.logical_path)?;
    fs.after_final_revalidation(&file.target_path)
        .map_err(|source| {
            filesystem_operation_error(
                "finish final target validation",
                &file.logical_path,
                file.target_path.clone(),
                source,
            )
        })?;
    snapshot.revalidate_path(transaction, &file.logical_path)?;
    validate_auxiliary(&file.parent, &file.stage, &file.logical_path, "stage")?;
    if let Some(backup) = &file.backup {
        validate_auxiliary(&file.parent, backup, &file.logical_path, "backup")?;
    }
    let (commit_parent, target_name) = transaction.open_parent(&file.logical_path)?;
    transaction.ensure_same_directory(&file.logical_path, &file.parent, &commit_parent)?;
    snapshot.revalidate_path(transaction, &file.logical_path)?;
    fs.before_target_publication(&file.target_path)
        .map_err(|source| {
            filesystem_operation_error(
                "prepare atomic target publication",
                &file.logical_path,
                file.target_path.clone(),
                source,
            )
        })?;
    if matches!(
        snapshot.preimage(&file.logical_path),
        Some(PathPreimage::RegularFile { .. })
    ) {
        snapshot.revalidate_path(transaction, &file.logical_path)?;
    }
    match snapshot
        .preimage(&file.logical_path)
        .expect("commit target has a validated preimage")
    {
        PathPreimage::Absent => {
            fs.hard_link(
                &[],
                HardLinkEndpoint::new(&file.parent, Path::new(&file.stage.name), &file.stage.path),
                HardLinkEndpoint::new(&commit_parent, Path::new(&target_name), &file.target_path),
            )
            .map_err(|source| {
                if source.kind() == io::ErrorKind::AlreadyExists {
                    CodegenError::PreimageConflict {
                        path: file.logical_path.clone(),
                        reason: "expected-absent target appeared before no-clobber publication"
                            .to_owned(),
                    }
                } else {
                    filesystem_operation_error(
                        "publish absent target without clobber",
                        &file.logical_path,
                        file.target_path.clone(),
                        source,
                    )
                }
            })?;
        }
        PathPreimage::RegularFile { .. } => {
            fs.rename(
                &file.parent,
                Path::new(&file.stage.name),
                &file.stage.path,
                &commit_parent,
                Path::new(&target_name),
                &file.target_path,
            )
            .map_err(|source| {
                filesystem_operation_error(
                    "replace target",
                    &file.logical_path,
                    file.target_path.clone(),
                    source,
                )
            })?;
        }
    }
    fs.sync_directory(&commit_parent, &file.target_path)
        .map_err(|source| {
            filesystem_operation_error(
                "sync replaced target directory",
                &file.logical_path,
                file.target_path.clone(),
                source,
            )
        })?;
    validate_installed_target(&commit_parent, &target_name, file)?;
    if matches!(
        snapshot.preimage(&file.logical_path),
        Some(PathPreimage::Absent)
    ) {
        validate_auxiliary(&file.parent, &file.stage, &file.logical_path, "stage")?;
        fs.remove_file(&file.parent, Path::new(&file.stage.name), &file.stage.path)
            .map_err(|source| {
                filesystem_operation_error(
                    "remove published target stage link",
                    &file.logical_path,
                    file.stage.path.clone(),
                    source,
                )
            })?;
        fs.sync_directory(&file.parent, &file.target_path)
            .map_err(|source| {
                filesystem_operation_error(
                    "sync published target stage cleanup",
                    &file.logical_path,
                    file.target_path.clone(),
                    source,
                )
            })?;
    }
    Ok(())
}

fn validate_auxiliary(
    parent: &Dir,
    auxiliary: &AuxiliaryFile,
    logical_path: &str,
    kind: &str,
) -> Result<(), CodegenError> {
    let identity =
        current_regular_file_identity(parent, Path::new(&auxiliary.name)).map_err(|source| {
            CodegenError::UnsafePath {
                path: auxiliary.path.display().to_string(),
                reason: format!("transaction {kind} is not a no-follow regular file: {source}"),
            }
        })?;
    if identity != auxiliary.identity {
        return Err(CodegenError::UnsafePath {
            path: auxiliary.path.display().to_string(),
            reason: format!("transaction {kind} changed identity"),
        });
    }
    let observed = read_auxiliary(parent, &auxiliary.name, &auxiliary.path)?;
    if observed.identity != auxiliary.identity
        || observed.content_hash != auxiliary.content_hash
        || observed.length != auxiliary.length
        || observed.posix_mode != auxiliary.posix_mode
    {
        return Err(CodegenError::RecoveryRequired {
            journal_path: auxiliary.path.clone(),
            reason: format!(
                "transaction {kind} for {logical_path} changed content, length, mode, or identity"
            ),
        });
    }
    Ok(())
}

fn validate_installed_target(
    parent: &Dir,
    target_name: &str,
    file: &StagedFile,
) -> Result<(), CodegenError> {
    let observed = read_auxiliary(parent, target_name, &file.target_path)?;
    if observed.content_hash != file.planned_hash
        || observed.length != file.planned_length
        || observed.posix_mode != file.planned_posix_mode
    {
        return Err(CodegenError::PreimageConflict {
            path: file.logical_path.clone(),
            reason: "published target did not retain the planned bytes, length, and mode"
                .to_owned(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedArtifact {
    identity: (u64, u64),
    content_hash: String,
    length: u64,
    posix_mode: Option<u32>,
}

fn read_auxiliary(parent: &Dir, name: &str, path: &Path) -> Result<ObservedArtifact, CodegenError> {
    let mut options = OpenOptions::new();
    options.read(true);
    options.follow(FollowSymlinks::No);
    options.nonblock(true);
    let mut file = parent
        .open_with(name, &options)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let metadata = file.metadata().map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(CodegenError::UnsafePath {
            path: path.display().to_string(),
            reason: "transaction artifact is not a no-follow regular file".to_owned(),
        });
    }
    #[cfg(windows)]
    if cap_fs_ext::OsMetadataExt::file_attributes(&metadata) & 0x0000_0400 != 0 {
        return Err(CodegenError::UnsafePath {
            path: path.display().to_string(),
            reason: "transaction artifact is a Windows reparse point".to_owned(),
        });
    }
    let identity = (MetadataExt::dev(&metadata), MetadataExt::ino(&metadata));
    let length = metadata.len();
    let posix_mode = metadata_posix_mode(&metadata);
    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if content.len() as u64 != length {
        return Err(CodegenError::UnsafePath {
            path: path.display().to_string(),
            reason: "transaction artifact length changed while it was read".to_owned(),
        });
    }
    Ok(ObservedArtifact {
        identity,
        content_hash: crate::hash_content_bytes(&content),
        length,
        posix_mode,
    })
}

fn opened_posix_mode(file: &cap_std::fs::File) -> io::Result<Option<u32>> {
    file.metadata()
        .map(|metadata| metadata_posix_mode(&metadata))
}

fn metadata_posix_mode(metadata: &cap_std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;

        Some(metadata.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn record_stage(entry: &mut JournalEntry, staged: &StagedFile) {
    entry.stage_identity = Some(staged.stage.identity.into());
    entry.stage_hash = Some(staged.stage.content_hash.clone());
    entry.stage_length = Some(staged.stage.length);
    entry.stage_posix_mode = staged.stage.posix_mode;
    entry.planned_posix_mode = staged.planned_posix_mode;
}

fn record_backup(entry: &mut JournalEntry, staged: &StagedFile) {
    if let Some(backup) = &staged.backup {
        entry.backup_identity = Some(backup.identity.into());
        entry.backup_hash = Some(backup.content_hash.clone());
        entry.backup_length = Some(backup.length);
        entry.backup_posix_mode = backup.posix_mode;
    }
}

fn merge_created_directories(
    existing: &mut Vec<JournalDirectory>,
    additional: &[JournalDirectory],
) {
    for directory in additional {
        if let Some(current) = existing
            .iter_mut()
            .find(|current| current.logical_path == directory.logical_path)
        {
            *current = directory.clone();
        } else {
            existing.push(directory.clone());
        }
    }
    existing.sort_by(|left, right| {
        path_depth(&left.logical_path)
            .cmp(&path_depth(&right.logical_path))
            .then_with(|| left.logical_path.cmp(&right.logical_path))
    });
    existing.dedup_by(|left, right| left.logical_path == right.logical_path);
}

impl From<(u64, u64)> for JournalIdentity {
    fn from((device, inode): (u64, u64)) -> Self {
        Self { device, inode }
    }
}

fn transaction_artifact_name(prefix: &str, transaction_id: &str, ordinal: usize) -> String {
    format!("{prefix}{transaction_id}-{ordinal:08x}")
}

struct RecoveryInventory {
    kit_directory: Dir,
    transactions_directory: Dir,
    transactions_path: PathBuf,
    journal_name: String,
    journal_path: PathBuf,
    intent_names: Vec<String>,
    update_names: Vec<String>,
    data: TransactionJournalData,
}

struct OrphanIntentInventory {
    kit_directory: Dir,
    transactions_directory: Dir,
    transactions_path: PathBuf,
    name: String,
    path: PathBuf,
    observed: ObservedArtifact,
}

pub fn check_pending_recovery(project_root: &Path) -> Result<(), CodegenError> {
    let context = PlanningContext::open(project_root)?;
    let (inventory, orphan_intent) = load_recovery_inventory(&context)?;
    if let Some(orphan_intent) = orphan_intent {
        return Err(CodegenError::RecoveryRequired {
            journal_path: orphan_intent.path,
            reason: "an unpublished durable transaction intent must be cleaned by the next mutating command"
                .to_owned(),
        });
    }
    let Some(inventory) = inventory else {
        return Ok(());
    };
    validate_recovery_application_state(&context, &inventory)?;
    Err(CodegenError::RecoveryRequired {
        journal_path: inventory.journal_path,
        reason: "a durable transaction journal must be recovered by the next mutating command"
            .to_owned(),
    })
}

pub(super) fn recover_pending_transaction(
    context: &PlanningContext,
    fs: &dyn FsOps,
) -> Result<(), CodegenError> {
    let (inventory, orphan_intent) = load_recovery_inventory(context)?;
    if let Some(orphan_intent) = orphan_intent {
        cleanup_orphan_intent(fs, orphan_intent)?;
        return Ok(());
    }
    let Some(inventory) = inventory else {
        return Ok(());
    };
    validate_recovery_application_state(context, &inventory)?;

    if matches!(inventory.data.state, JournalState::Applied) {
        for entry in &inventory.data.entries {
            let current = context.inspect_path_uncached(&entry.logical_path)?;
            if !target_matches_planned(&current, entry) {
                return Err(third_state_error(
                    &inventory.journal_path,
                    &entry.logical_path,
                ));
            }
        }
        cleanup_recovery_auxiliaries(context, fs, &inventory)?;
        cleanup_recovery_updates(fs, &inventory)?;
        return recovery_journal(inventory).remove(fs);
    }

    for entry in inventory.data.entries.iter().rev() {
        let current = context.inspect_path_uncached(&entry.logical_path)?;
        if target_matches_preimage(&current, &entry.preimage) {
            continue;
        }
        if !target_matches_planned(&current, entry) {
            return Err(third_state_error(
                &inventory.journal_path,
                &entry.logical_path,
            ));
        }
        let (parent, target_name) = context.open_parent(&entry.logical_path)?;
        let target_path = context.project_root().join(&entry.logical_path);
        let observed_target = read_auxiliary(&parent, &target_name, &target_path)?;
        if entry.stage_identity.is_none()
            || !artifact_matches_record(
                &observed_target,
                entry.stage_identity,
                Some(&entry.planned_hash),
                Some(entry.planned_length),
                entry.planned_posix_mode,
            )
        {
            return Err(third_state_error(
                &inventory.journal_path,
                &entry.logical_path,
            ));
        }
        match &entry.preimage {
            JournalPreimage::Absent => fs
                .remove_file(&parent, Path::new(&target_name), &target_path)
                .map_err(|source| {
                    filesystem_operation_error(
                        "recover absent target",
                        &entry.logical_path,
                        target_path.clone(),
                        source,
                    )
                })?,
            JournalPreimage::RegularFile { .. } => {
                let backup_name = entry
                    .backup_name
                    .as_deref()
                    .expect("validated regular-file journal entry has a backup");
                let backup_path = target_path
                    .parent()
                    .expect("target has a parent")
                    .join(backup_name);
                let observed_backup = read_auxiliary(&parent, backup_name, &backup_path)?;
                if !artifact_matches_record(
                    &observed_backup,
                    entry.backup_identity,
                    entry.backup_hash.as_deref(),
                    entry.backup_length,
                    entry.backup_posix_mode,
                ) {
                    return Err(third_state_error(
                        &inventory.journal_path,
                        &entry.logical_path,
                    ));
                }
                fs.rename(
                    &parent,
                    Path::new(backup_name),
                    &backup_path,
                    &parent,
                    Path::new(&target_name),
                    &target_path,
                )
                .map_err(|source| {
                    filesystem_operation_error(
                        "recover target backup",
                        &entry.logical_path,
                        target_path.clone(),
                        source,
                    )
                })?;
            }
        }
        fs.sync_directory(&parent, &target_path).map_err(|source| {
            filesystem_operation_error(
                "sync recovered target directory",
                &entry.logical_path,
                target_path,
                source,
            )
        })?;
    }

    cleanup_recovery_auxiliaries(context, fs, &inventory)?;
    cleanup_created_directories_strict(context, fs, &inventory.data.created_directories)?;
    cleanup_recovery_updates(fs, &inventory)?;
    recovery_journal(inventory).remove(fs)
}

fn cleanup_recovery_updates(
    fs: &dyn FsOps,
    inventory: &RecoveryInventory,
) -> Result<(), CodegenError> {
    for name in inventory
        .intent_names
        .iter()
        .chain(inventory.update_names.iter())
    {
        let path = inventory.transactions_path.join(name);
        validate_recovery_private_file(&inventory.transactions_directory, name, &path)?;
        match fs.remove_file(&inventory.transactions_directory, Path::new(name), &path) {
            Ok(()) => fs
                .sync_directory(
                    &inventory.transactions_directory,
                    &inventory.transactions_path,
                )
                .map_err(|source| {
                    filesystem_operation_error(
                        "sync recovery journal auxiliary cleanup",
                        "src/components/ui/_kit/.transactions",
                        inventory.transactions_path.clone(),
                        source,
                    )
                })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(filesystem_operation_error(
                    "remove recovery journal update",
                    "src/components/ui/_kit/.transactions",
                    path,
                    source,
                ));
            }
        }
    }
    Ok(())
}

fn cleanup_orphan_intent(
    fs: &dyn FsOps,
    inventory: OrphanIntentInventory,
) -> Result<(), CodegenError> {
    validate_recovery_private_file(
        &inventory.transactions_directory,
        &inventory.name,
        &inventory.path,
    )?;
    let current = read_auxiliary(
        &inventory.transactions_directory,
        &inventory.name,
        &inventory.path,
    )?;
    if current != inventory.observed {
        return Err(CodegenError::RecoveryRequired {
            journal_path: inventory.path,
            reason: "unpublished transaction intent changed before cleanup".to_owned(),
        });
    }
    fs.remove_file(
        &inventory.transactions_directory,
        Path::new(&inventory.name),
        &inventory.path,
    )
    .map_err(|source| {
        filesystem_operation_error(
            "remove unpublished transaction intent",
            "src/components/ui/_kit/.transactions",
            inventory.path.clone(),
            source,
        )
    })?;
    fs.sync_directory(
        &inventory.transactions_directory,
        &inventory.transactions_path,
    )
    .map_err(|source| {
        filesystem_operation_error(
            "sync unpublished transaction intent cleanup",
            "src/components/ui/_kit/.transactions",
            inventory.transactions_path.clone(),
            source,
        )
    })?;
    drop(inventory.transactions_directory);
    fs.remove_dir(
        &inventory.kit_directory,
        Path::new(TRANSACTIONS_DIRECTORY_NAME),
        &inventory.transactions_path,
    )
    .map_err(|source| {
        filesystem_operation_error(
            "remove unpublished transaction directory",
            KIT_DIRECTORY_PATH,
            inventory.transactions_path.clone(),
            source,
        )
    })?;
    fs.sync_directory(&inventory.kit_directory, &inventory.transactions_path)
        .map_err(|source| {
            filesystem_operation_error(
                "sync unpublished transaction directory cleanup",
                KIT_DIRECTORY_PATH,
                inventory.transactions_path,
                source,
            )
        })
}

fn recovery_journal(inventory: RecoveryInventory) -> DurableJournal {
    DurableJournal {
        kit_directory: inventory.kit_directory,
        transactions_directory: inventory.transactions_directory,
        transactions_path: inventory.transactions_path,
        name: inventory.journal_name,
        path: inventory.journal_path,
        data: inventory.data,
    }
}

fn load_recovery_inventory(
    context: &PlanningContext,
) -> Result<(Option<RecoveryInventory>, Option<OrphanIntentInventory>), CodegenError> {
    let kit_directory = match context.open_directory(KIT_DIRECTORY_PATH) {
        Ok(directory) => directory,
        Err(CodegenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok((None, None));
        }
        Err(error) => return Err(error),
    };
    let transactions_path = context
        .project_root()
        .join(KIT_DIRECTORY_PATH)
        .join(TRANSACTIONS_DIRECTORY_NAME);
    let metadata = match kit_directory.symlink_metadata(TRANSACTIONS_DIRECTORY_NAME) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((None, None)),
        Err(source) => {
            return Err(CodegenError::Io {
                path: transactions_path,
                source,
            });
        }
    };
    validate_transaction_directory_metadata(&transactions_path, &metadata)?;
    let transactions_directory = kit_directory
        .open_dir_nofollow(TRANSACTIONS_DIRECTORY_NAME)
        .map_err(|source| CodegenError::UnsafePath {
            path: transactions_path.display().to_string(),
            reason: format!("failed to open transaction directory without following: {source}"),
        })?;
    validate_transaction_directory_metadata(
        &transactions_path,
        &transactions_directory
            .dir_metadata()
            .map_err(|source| CodegenError::Io {
                path: transactions_path.clone(),
                source,
            })?,
    )?;

    let mut journal_names = Vec::new();
    let mut intent_names = Vec::new();
    let mut update_names = Vec::new();
    for entry in transactions_directory
        .entries()
        .map_err(|source| CodegenError::Io {
            path: transactions_path.clone(),
            source,
        })?
    {
        let name = entry
            .map_err(|source| CodegenError::Io {
                path: transactions_path.clone(),
                source,
            })?
            .file_name()
            .into_string()
            .map_err(|name| CodegenError::InvalidCoordinationState {
                path: transactions_path.display().to_string(),
                reason: format!("non-UTF-8 transaction entry: {}", name.to_string_lossy()),
            })?;
        if transaction_journal_name(&name) {
            journal_names.push(name);
        } else if journal_intent_name(&name) {
            intent_names.push(name);
        } else if journal_update_name(&name) {
            update_names.push(name);
        } else {
            return Err(CodegenError::InvalidCoordinationState {
                path: transactions_path.join(&name).display().to_string(),
                reason: "unexpected transaction recovery entry".to_owned(),
            });
        }
    }
    journal_names.sort();
    intent_names.sort();
    update_names.sort();
    if journal_names.is_empty() && intent_names.is_empty() && update_names.is_empty() {
        return Ok((None, None));
    }
    if journal_names.is_empty() {
        if intent_names.len() == 1 && update_names.is_empty() {
            let name = intent_names.pop().expect("one intent name");
            let path = transactions_path.join(&name);
            validate_recovery_private_file(&transactions_directory, &name, &path)?;
            let observed = read_auxiliary(&transactions_directory, &name, &path)?;
            return Ok((
                None,
                Some(OrphanIntentInventory {
                    kit_directory,
                    transactions_directory,
                    transactions_path,
                    name,
                    path,
                    observed,
                }),
            ));
        }
        return Err(CodegenError::InvalidCoordinationState {
            path: transactions_path.display().to_string(),
            reason: "transaction directory has no unambiguous durable primary journal".to_owned(),
        });
    }
    if journal_names.len() != 1 {
        return Err(CodegenError::InvalidCoordinationState {
            path: transactions_path.display().to_string(),
            reason: "multiple durable transaction journals are present".to_owned(),
        });
    }
    let journal_name = journal_names.pop().expect("one journal name");
    let journal_path = transactions_path.join(&journal_name);
    let data = read_and_validate_journal(
        context,
        &transactions_directory,
        &journal_name,
        &journal_path,
    )?;
    for name in &intent_names {
        if !journal_intent_name_for_transaction(name, &data.transaction_id) {
            return Err(CodegenError::InvalidCoordinationState {
                path: transactions_path.join(name).display().to_string(),
                reason: "journal intent is not bound to the active transaction".to_owned(),
            });
        }
        validate_recovery_private_file(
            &transactions_directory,
            name,
            &transactions_path.join(name),
        )?;
    }
    for name in &update_names {
        if !journal_update_name_for_transaction(name, &data.transaction_id) {
            return Err(CodegenError::InvalidCoordinationState {
                path: transactions_path.join(name).display().to_string(),
                reason: "journal update is not bound to the active transaction".to_owned(),
            });
        }
        validate_recovery_private_file(
            &transactions_directory,
            name,
            &transactions_path.join(name),
        )?;
    }
    Ok((
        Some(RecoveryInventory {
            kit_directory,
            transactions_directory,
            transactions_path,
            journal_name,
            journal_path,
            intent_names,
            update_names,
            data,
        }),
        None,
    ))
}

fn read_and_validate_journal(
    context: &PlanningContext,
    directory: &Dir,
    name: &str,
    path: &Path,
) -> Result<TransactionJournalData, CodegenError> {
    validate_recovery_private_file(directory, name, path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    options.follow(FollowSymlinks::No);
    options.nonblock(true);
    let mut file = directory
        .open_with(name, &options)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let metadata = file.metadata().map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > 1024 * 1024 {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: "transaction journal exceeds the one-megabyte limit".to_owned(),
        });
    }
    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let envelope: serde_json::Value = serde_json::from_slice(&content).map_err(|source| {
        CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: format!("invalid transaction journal: {source}"),
        }
    })?;
    let version = envelope
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: "transaction journal has no numeric version".to_owned(),
        })?;
    if version != u64::from(TRANSACTION_JOURNAL_VERSION) {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: format!(
                "unsupported transaction journal version {version}; version-one journals are not reinterpreted as version two"
            ),
        });
    }
    let data: TransactionJournalData = serde_json::from_value(envelope).map_err(|source| {
        CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: format!("invalid transaction journal: {source}"),
        }
    })?;
    validate_journal_data(context, name, path, &data)?;
    Ok(data)
}

fn validate_journal_data(
    context: &PlanningContext,
    name: &str,
    path: &Path,
    data: &TransactionJournalData,
) -> Result<(), CodegenError> {
    if data.version != TRANSACTION_JOURNAL_VERSION {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: format!("unsupported transaction journal version {}", data.version),
        });
    }
    if name
        != format!(
            "{TRANSACTION_JOURNAL_PREFIX}{}{TRANSACTION_JOURNAL_SUFFIX}",
            data.transaction_id
        )
        || !random_suffix(&data.transaction_id)
    {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: "journal filename and transaction identifier disagree".to_owned(),
        });
    }
    let (canonical_root, device, inode) = context.project_identity();
    if data.project.canonical_root != canonical_root.to_string_lossy()
        || data.project.device != device
        || data.project.inode != inode
    {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: "journal belongs to a different project identity".to_owned(),
        });
    }
    if data.entries.is_empty() {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: "transaction journal has an empty cohort".to_owned(),
        });
    }
    let paths = data
        .entries
        .iter()
        .map(|entry| entry.logical_path.clone())
        .collect::<Vec<_>>();
    validate_planned_write_paths(&paths).map_err(|error| {
        CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: format!("journal contains an unsafe cohort: {error}"),
        }
    })?;
    for (ordinal, entry) in data.entries.iter().enumerate() {
        let action_matches = matches!(
            (&entry.action, &entry.preimage),
            (JournalAction::Create, JournalPreimage::Absent)
                | (JournalAction::Update, JournalPreimage::RegularFile { .. })
        );
        let stage_ready = artifact_fields_valid(
            entry.stage_identity,
            entry.stage_hash.as_deref(),
            entry.stage_length,
            entry.stage_posix_mode,
        );
        let backup_ready = artifact_fields_valid(
            entry.backup_identity,
            entry.backup_hash.as_deref(),
            entry.backup_length,
            entry.backup_posix_mode,
        );
        if entry.ordinal != ordinal
            || !transaction_artifact_name_valid(
                &entry.stage_name,
                STAGE_PREFIX,
                &data.transaction_id,
                ordinal,
            )
            || !hash_string(&entry.planned_hash)
            || !action_matches
            || !stage_ready
            || match &entry.preimage {
                JournalPreimage::Absent => {
                    entry.backup_name.is_some()
                        || entry.backup_identity.is_some()
                        || entry.backup_hash.is_some()
                        || entry.backup_length.is_some()
                        || entry.backup_posix_mode.is_some()
                }
                JournalPreimage::RegularFile { content_hash, .. } => {
                    !hash_string(content_hash)
                        || !entry.backup_name.as_deref().is_some_and(|name| {
                            transaction_artifact_name_valid(
                                name,
                                BACKUP_PREFIX,
                                &data.transaction_id,
                                ordinal,
                            )
                        })
                        || !backup_ready
                }
            }
        {
            return Err(CodegenError::InvalidCoordinationState {
                path: path.display().to_string(),
                reason: format!("invalid journal entry for {}", entry.logical_path),
            });
        }
    }
    let stage_ready = data
        .entries
        .iter()
        .map(|entry| entry.stage_identity.is_some())
        .collect::<Vec<_>>();
    let backup_ready = data
        .entries
        .iter()
        .filter(|entry| matches!(entry.preimage, JournalPreimage::RegularFile { .. }))
        .map(|entry| entry.backup_identity.is_some())
        .collect::<Vec<_>>();
    let readiness_is_prefix =
        |readiness: &[bool]| readiness.windows(2).all(|pair| !pair[1] || pair[0]);
    let readiness_valid = match data.state {
        JournalState::Intent => {
            stage_ready.iter().all(|ready| !ready)
                && data.entries.iter().all(|entry| {
                    entry.backup_identity.is_none()
                        && entry.backup_hash.is_none()
                        && entry.backup_length.is_none()
                })
        }
        JournalState::Preparing { .. } => {
            readiness_is_prefix(&stage_ready)
                && readiness_is_prefix(&backup_ready)
                && (data
                    .entries
                    .iter()
                    .all(|entry| entry.backup_identity.is_none())
                    || stage_ready.iter().all(|ready| *ready))
        }
        JournalState::Prepared
        | JournalState::Replacing { .. }
        | JournalState::Committed { .. }
        | JournalState::RollingBack { .. }
        | JournalState::Applied => {
            stage_ready.iter().all(|ready| *ready) && backup_ready.iter().all(|ready| *ready)
        }
    };
    if !readiness_valid {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: "journal artifact readiness is inconsistent with durable state".to_owned(),
        });
    }
    let count = match data.state {
        JournalState::Intent | JournalState::Prepared | JournalState::Applied => None,
        JournalState::Preparing { index } => Some(index),
        JournalState::Replacing { index } => Some(index),
        JournalState::Committed { count } | JournalState::RollingBack { count } => Some(count),
    };
    if count.is_some_and(|count| count > data.entries.len()) {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: "journal progress exceeds its cohort".to_owned(),
        });
    }
    if matches!(data.state, JournalState::Preparing { index } if index >= data.entries.len())
        || matches!(data.state, JournalState::Replacing { index } if index >= data.entries.len())
        || matches!(data.state, JournalState::Committed { count } if count == 0)
    {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.display().to_string(),
            reason: "journal progress is not reachable for its durable state".to_owned(),
        });
    }
    for directory in &data.created_directories {
        if Path::new(&directory.logical_path).is_absolute()
            || Path::new(&directory.logical_path)
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || !data.entries.iter().any(|entry| {
                Path::new(&entry.logical_path)
                    .parent()
                    .is_some_and(|parent| parent.starts_with(&directory.logical_path))
            })
        {
            return Err(CodegenError::InvalidCoordinationState {
                path: path.display().to_string(),
                reason: format!("invalid created-directory entry {}", directory.logical_path),
            });
        }
    }
    Ok(())
}

fn validate_recovery_application_state(
    context: &PlanningContext,
    inventory: &RecoveryInventory,
) -> Result<(), CodegenError> {
    for entry in &inventory.data.entries {
        let current = context.inspect_path_uncached(&entry.logical_path)?;
        if !target_matches_preimage(&current, &entry.preimage)
            && !target_matches_planned(&current, entry)
        {
            return Err(third_state_error(
                &inventory.journal_path,
                &entry.logical_path,
            ));
        }
        let (parent, target_name) = match context.open_parent(&entry.logical_path) {
            Ok(parent) => parent,
            Err(CodegenError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound
                    && entry.stage_identity.is_none()
                    && entry.backup_identity.is_none()
                    && target_matches_preimage(&current, &entry.preimage) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let target_path = context.project_root().join(&entry.logical_path);
        if target_matches_planned(&current, entry) {
            let observed_target = read_auxiliary(&parent, &target_name, &target_path)?;
            if entry.stage_identity.is_none()
                || !artifact_matches_record(
                    &observed_target,
                    entry.stage_identity,
                    Some(&entry.planned_hash),
                    Some(entry.planned_length),
                    entry.planned_posix_mode,
                )
            {
                return Err(third_state_error(
                    &inventory.journal_path,
                    &entry.logical_path,
                ));
            }
        }
        let stage_path = target_path
            .parent()
            .expect("target has a parent")
            .join(&entry.stage_name);
        if let Some(observed) = read_optional_auxiliary(&parent, &entry.stage_name, &stage_path)?
            && entry.stage_identity.is_some()
            && (observed.content_hash != entry.planned_hash
                || observed.length != entry.planned_length
                || entry
                    .planned_posix_mode
                    .is_some_and(|mode| observed.posix_mode != Some(mode))
                || !artifact_matches_record(
                    &observed,
                    entry.stage_identity,
                    entry.stage_hash.as_deref(),
                    entry.stage_length,
                    entry.stage_posix_mode,
                ))
        {
            return Err(third_state_error(
                &inventory.journal_path,
                &entry.logical_path,
            ));
        }
        if let JournalPreimage::RegularFile { content_hash, .. } = &entry.preimage {
            let backup_name = entry
                .backup_name
                .as_deref()
                .expect("validated regular entry has backup");
            let backup_path = target_path
                .parent()
                .expect("target has a parent")
                .join(backup_name);
            let backup = read_optional_auxiliary(&parent, backup_name, &backup_path)?;
            if (entry.backup_identity.is_some()
                && backup.as_ref().is_some_and(|observed| {
                    observed.content_hash != *content_hash
                        || !artifact_matches_record(
                            observed,
                            entry.backup_identity,
                            entry.backup_hash.as_deref(),
                            entry.backup_length,
                            entry.backup_posix_mode,
                        )
                }))
                || (!matches!(inventory.data.state, JournalState::Applied)
                    && target_matches_planned(&current, entry)
                    && backup.is_none())
            {
                return Err(third_state_error(
                    &inventory.journal_path,
                    &entry.logical_path,
                ));
            }
        }
    }
    Ok(())
}

fn cleanup_recovery_auxiliaries(
    context: &PlanningContext,
    fs: &dyn FsOps,
    inventory: &RecoveryInventory,
) -> Result<(), CodegenError> {
    for entry in inventory.data.entries.iter().rev() {
        let (parent, _) = match context.open_parent(&entry.logical_path) {
            Ok(parent) => parent,
            Err(CodegenError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound
                    && entry.stage_identity.is_none()
                    && entry.backup_identity.is_none() =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let target_path = context.project_root().join(&entry.logical_path);
        for (name, identity, hash, length, mode) in [
            (
                Some(entry.stage_name.as_str()),
                entry.stage_identity,
                entry.stage_hash.as_deref(),
                entry.stage_length,
                entry.stage_posix_mode,
            ),
            (
                entry.backup_name.as_deref(),
                entry.backup_identity,
                entry.backup_hash.as_deref(),
                entry.backup_length,
                entry.backup_posix_mode,
            ),
        ] {
            let Some(name) = name else { continue };
            let path = target_path
                .parent()
                .expect("target has a parent")
                .join(name);
            if let Some(observed) = read_optional_auxiliary(&parent, name, &path)?
                && !artifact_matches_record(&observed, identity, hash, length, mode)
            {
                return Err(third_state_error(
                    &inventory.journal_path,
                    &entry.logical_path,
                ));
            }
            match fs.remove_file(&parent, Path::new(name), &path) {
                Ok(()) => fs.sync_directory(&parent, &target_path).map_err(|source| {
                    filesystem_operation_error(
                        "sync recovery auxiliary cleanup",
                        &entry.logical_path,
                        target_path.clone(),
                        source,
                    )
                })?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(filesystem_operation_error(
                        "remove recovery auxiliary",
                        &entry.logical_path,
                        path,
                        source,
                    ));
                }
            }
        }
    }
    Ok(())
}

fn read_optional_auxiliary(
    parent: &Dir,
    name: &str,
    path: &Path,
) -> Result<Option<ObservedArtifact>, CodegenError> {
    match parent.symlink_metadata(name) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CodegenError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    validate_recovery_regular_file(parent, name, path)?;
    read_auxiliary(parent, name, path).map(Some)
}

fn artifact_matches_record(
    observed: &ObservedArtifact,
    identity: Option<JournalIdentity>,
    hash: Option<&str>,
    length: Option<u64>,
    posix_mode: Option<u32>,
) -> bool {
    identity.is_none_or(|identity| observed.identity == (identity.device, identity.inode))
        && hash.is_none_or(|hash| observed.content_hash == hash)
        && length.is_none_or(|length| observed.length == length)
        && posix_mode.is_none_or(|mode| observed.posix_mode == Some(mode))
}

fn validate_recovery_regular_file(
    parent: &Dir,
    name: &str,
    path: &Path,
) -> Result<(), CodegenError> {
    current_regular_file_identity(parent, Path::new(name)).map_err(|source| {
        CodegenError::UnsafePath {
            path: path.display().to_string(),
            reason: format!("recovery entry is not a no-follow regular file: {source}"),
        }
    })?;
    Ok(())
}

fn validate_recovery_private_file(
    parent: &Dir,
    name: &str,
    path: &Path,
) -> Result<(), CodegenError> {
    validate_recovery_regular_file(parent, name, path)?;
    let metadata = parent
        .symlink_metadata(name)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o7777 != 0o600 {
            return Err(CodegenError::InvalidCoordinationState {
                path: path.display().to_string(),
                reason: "recovery entry must have mode 0600".to_owned(),
            });
        }
    }
    Ok(())
}

fn target_matches_preimage(current: &PathPreimage, expected: &JournalPreimage) -> bool {
    match (current, expected) {
        (PathPreimage::Absent, JournalPreimage::Absent) => true,
        (
            PathPreimage::RegularFile { content_hash, mode },
            JournalPreimage::RegularFile {
                content_hash: expected_hash,
                readonly,
                posix_mode,
            },
        ) => {
            content_hash == expected_hash
                && mode.readonly == *readonly
                && mode.posix_mode == *posix_mode
        }
        _ => false,
    }
}

fn target_matches_planned(current: &PathPreimage, entry: &JournalEntry) -> bool {
    matches!(
        current,
        PathPreimage::RegularFile { content_hash, mode }
            if content_hash == &entry.planned_hash
                && entry
                    .planned_posix_mode
                    .is_none_or(|expected| mode.posix_mode == Some(expected))
    )
}

fn third_state_error(journal_path: &Path, logical_path: &str) -> CodegenError {
    CodegenError::RecoveryRequired {
        journal_path: journal_path.to_path_buf(),
        reason: format!(
            "project path {logical_path} is neither its recorded preimage nor planned transaction state; preserve the application edit and journal for manual inspection"
        ),
    }
}

fn transaction_journal_name(name: &str) -> bool {
    name.strip_prefix(TRANSACTION_JOURNAL_PREFIX)
        .and_then(|value| value.strip_suffix(TRANSACTION_JOURNAL_SUFFIX))
        .is_some_and(random_suffix)
}

fn journal_intent_name(name: &str) -> bool {
    journal_auxiliary_name(name, JOURNAL_INTENT_PREFIX)
}

fn journal_update_name(name: &str) -> bool {
    journal_auxiliary_name(name, JOURNAL_UPDATE_PREFIX)
}

fn journal_auxiliary_name(name: &str, prefix: &str) -> bool {
    let Some(suffix) = name.strip_prefix(prefix) else {
        return false;
    };
    let Some((transaction_id, update_id)) = suffix.split_once('-') else {
        return false;
    };
    random_suffix(transaction_id) && random_suffix(update_id)
}

fn journal_intent_name_for_transaction(name: &str, transaction_id: &str) -> bool {
    journal_auxiliary_name_for_transaction(name, JOURNAL_INTENT_PREFIX, transaction_id)
}

fn journal_update_name_for_transaction(name: &str, transaction_id: &str) -> bool {
    journal_auxiliary_name_for_transaction(name, JOURNAL_UPDATE_PREFIX, transaction_id)
}

fn journal_auxiliary_name_for_transaction(name: &str, prefix: &str, transaction_id: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|suffix| suffix.split_once('-'))
        .is_some_and(|(candidate, update_id)| {
            candidate == transaction_id && random_suffix(update_id)
        })
}

fn transaction_artifact_name_valid(
    name: &str,
    prefix: &str,
    transaction_id: &str,
    ordinal: usize,
) -> bool {
    name == transaction_artifact_name(prefix, transaction_id, ordinal)
}

fn artifact_fields_valid(
    identity: Option<JournalIdentity>,
    hash: Option<&str>,
    length: Option<u64>,
    _posix_mode: Option<u32>,
) -> bool {
    match (identity, hash, length) {
        (None, None, None) => true,
        (Some(_), Some(hash), Some(_)) => hash_string(hash),
        _ => false,
    }
}

fn random_suffix(value: &str) -> bool {
    value.len() == AUXILIARY_RANDOM_BYTES * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hash_string(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

struct JournalEntrySpec {
    logical_path: String,
    action: JournalAction,
    planned_hash: String,
    planned_length: u64,
}

fn predeclare_missing_directories(
    transaction: &PlanningContext,
    specs: &[JournalEntrySpec],
) -> Result<Vec<JournalDirectory>, CodegenError> {
    let mut paths = BTreeSet::new();
    for spec in specs {
        let mut current = PathBuf::new();
        let Some(parent) = Path::new(&spec.logical_path).parent() else {
            continue;
        };
        for component in parent.components() {
            let std::path::Component::Normal(component) = component else {
                continue;
            };
            current.push(component);
            let logical_path = current.to_string_lossy().replace('\\', "/");
            match transaction.open_directory(&logical_path) {
                Ok(_) => {}
                Err(CodegenError::Io { source, .. })
                    if source.kind() == io::ErrorKind::NotFound =>
                {
                    paths.insert(logical_path);
                }
                Err(error) => return Err(error),
            }
        }
    }
    Ok(paths
        .into_iter()
        .map(|logical_path| JournalDirectory {
            logical_path,
            identity: None,
        })
        .collect())
}

impl DurableJournal {
    fn create_intent(
        transaction: &PlanningContext,
        fs: &dyn FsOps,
        files: &[&PlannedFile],
        snapshot: &PlanSnapshot,
    ) -> Result<Self, CodegenError> {
        let specs = files
            .iter()
            .map(|file| JournalEntrySpec {
                logical_path: file.path.clone(),
                action: match file.action {
                    PlannedFileAction::Create => JournalAction::Create,
                    PlannedFileAction::Update => JournalAction::Update,
                },
                planned_hash: crate::hash_content_bytes(file.content.as_bytes()),
                planned_length: file.content.len() as u64,
            })
            .collect::<Vec<_>>();
        Self::create_intent_from_specs(transaction, fs, &specs, snapshot)
    }

    fn create_atomic_intent(
        transaction: &PlanningContext,
        fs: &dyn FsOps,
        logical_path: &str,
        content: &[u8],
        snapshot: &PlanSnapshot,
    ) -> Result<Self, CodegenError> {
        let action = match snapshot
            .preimage(logical_path)
            .expect("atomic target has a recorded preimage")
        {
            PathPreimage::Absent => JournalAction::Create,
            PathPreimage::RegularFile { .. } => JournalAction::Update,
        };
        Self::create_intent_from_specs(
            transaction,
            fs,
            &[JournalEntrySpec {
                logical_path: logical_path.to_owned(),
                action,
                planned_hash: crate::hash_content_bytes(content),
                planned_length: content.len() as u64,
            }],
            snapshot,
        )
    }

    fn create_intent_from_specs(
        transaction: &PlanningContext,
        fs: &dyn FsOps,
        specs: &[JournalEntrySpec],
        snapshot: &PlanSnapshot,
    ) -> Result<Self, CodegenError> {
        let kit_directory = transaction.open_directory(KIT_DIRECTORY_PATH)?;
        let transactions_path = transaction
            .project_root()
            .join(KIT_DIRECTORY_PATH)
            .join(TRANSACTIONS_DIRECTORY_NAME);
        let created = match kit_directory.symlink_metadata(TRANSACTIONS_DIRECTORY_NAME) {
            Ok(metadata) => {
                validate_transaction_directory_metadata(&transactions_path, &metadata)?;
                false
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs.before_create_directory(&transactions_path)
                    .map_err(|source| CodegenError::Io {
                        path: transactions_path.clone(),
                        source,
                    })?;
                create_private_transaction_directory(&kit_directory, &transactions_path)?;
                fs.after_create_directory(&transactions_path)
                    .map_err(|source| CodegenError::Io {
                        path: transactions_path.clone(),
                        source,
                    })?;
                true
            }
            Err(source) => {
                return Err(CodegenError::Io {
                    path: transactions_path.clone(),
                    source,
                });
            }
        };
        let transactions_directory = kit_directory
            .open_dir_nofollow(TRANSACTIONS_DIRECTORY_NAME)
            .map_err(|source| CodegenError::UnsafePath {
                path: transactions_path.display().to_string(),
                reason: format!("failed to open transaction directory without following: {source}"),
            })?;
        validate_transaction_directory_metadata(
            &transactions_path,
            &transactions_directory
                .dir_metadata()
                .map_err(|source| CodegenError::Io {
                    path: transactions_path.clone(),
                    source,
                })?,
        )?;
        fs.set_directory_mode(&transactions_directory, &transactions_path, 0o700)
            .map_err(|source| CodegenError::Io {
                path: transactions_path.clone(),
                source,
            })?;
        if created {
            fs.sync_directory(&kit_directory, &transactions_path)
                .map_err(|source| CodegenError::Io {
                    path: transactions_path.clone(),
                    source,
                })?;
        }

        let transaction_id = random_hex(transaction.project_root())?;
        let name =
            format!("{TRANSACTION_JOURNAL_PREFIX}{transaction_id}{TRANSACTION_JOURNAL_SUFFIX}");
        let path = transactions_path.join(&name);
        let (canonical_root, device, inode) = transaction.project_identity();
        let entries = specs
            .iter()
            .enumerate()
            .map(|(ordinal, spec)| {
                let preimage = match snapshot
                    .preimage(&spec.logical_path)
                    .expect("intent path has a validated preimage")
                {
                    PathPreimage::Absent => JournalPreimage::Absent,
                    PathPreimage::RegularFile { content_hash, mode } => {
                        JournalPreimage::RegularFile {
                            content_hash: content_hash.clone(),
                            readonly: mode.readonly,
                            posix_mode: mode.posix_mode,
                        }
                    }
                };
                JournalEntry {
                    ordinal,
                    logical_path: spec.logical_path.clone(),
                    action: spec.action,
                    stage_name: transaction_artifact_name(STAGE_PREFIX, &transaction_id, ordinal),
                    stage_identity: None,
                    stage_hash: None,
                    stage_length: None,
                    stage_posix_mode: None,
                    backup_name: matches!(&preimage, JournalPreimage::RegularFile { .. }).then(
                        || transaction_artifact_name(BACKUP_PREFIX, &transaction_id, ordinal),
                    ),
                    backup_identity: None,
                    backup_hash: None,
                    backup_length: None,
                    backup_posix_mode: None,
                    preimage,
                    planned_hash: spec.planned_hash.clone(),
                    planned_length: spec.planned_length,
                    planned_posix_mode: None,
                }
            })
            .collect();
        let created_directories = predeclare_missing_directories(transaction, specs)?;
        let data = TransactionJournalData {
            version: TRANSACTION_JOURNAL_VERSION,
            transaction_id,
            project: JournalProject {
                canonical_root: canonical_root.to_string_lossy().into_owned(),
                device,
                inode,
            },
            state: JournalState::Intent,
            entries,
            created_directories,
        };
        let journal = Self {
            kit_directory,
            transactions_directory,
            transactions_path,
            name,
            path,
            data,
        };
        if let Err(error) = journal.create_initial(fs) {
            drop(journal.transactions_directory);
            let _ = fs.remove_dir(
                &journal.kit_directory,
                Path::new(TRANSACTIONS_DIRECTORY_NAME),
                &journal.transactions_path,
            );
            return Err(error);
        }
        Ok(journal)
    }

    fn create_initial(&self, fs: &dyn FsOps) -> Result<(), CodegenError> {
        let content = serialize_journal(&self.data, &self.path)?;
        let intent_name = format!(
            "{JOURNAL_INTENT_PREFIX}{}-{}",
            self.data.transaction_id,
            random_hex(&self.path)?
        );
        let intent_path = self.transactions_path.join(&intent_name);
        let mut created = fs
            .create_new_file(
                &self.transactions_directory,
                Path::new(&intent_name),
                &intent_path,
                0o600,
            )
            .map_err(|source| {
                filesystem_operation_error(
                    "create unpublished transaction intent",
                    KIT_DIRECTORY_PATH,
                    intent_path.clone(),
                    source,
                )
            })?;
        let prepare = (|| {
            fs.write_handle(&mut created.file, &intent_path, &content)
                .map_err(|source| {
                    filesystem_operation_error(
                        "write unpublished transaction intent",
                        KIT_DIRECTORY_PATH,
                        intent_path.clone(),
                        source,
                    )
                })?;
            fs.sync_handle(&created.file, &intent_path)
                .map_err(|source| {
                    filesystem_operation_error(
                        "sync unpublished transaction intent",
                        KIT_DIRECTORY_PATH,
                        intent_path.clone(),
                        source,
                    )
                })
        })();
        drop(created.file);
        if let Err(error) = prepare {
            let _ = fs.remove_file(
                &self.transactions_directory,
                Path::new(&intent_name),
                &intent_path,
            );
            return Err(error);
        }
        fs.hard_link(
            &[],
            HardLinkEndpoint::new(
                &self.transactions_directory,
                Path::new(&intent_name),
                &intent_path,
            ),
            HardLinkEndpoint::new(
                &self.transactions_directory,
                Path::new(&self.name),
                &self.path,
            ),
        )
        .map_err(|source| {
            filesystem_operation_error(
                "publish durable transaction intent",
                KIT_DIRECTORY_PATH,
                self.path.clone(),
                source,
            )
        })?;
        fs.sync_directory(&self.transactions_directory, &self.transactions_path)
            .map_err(|source| {
                filesystem_operation_error(
                    "sync durable transaction intent publication",
                    KIT_DIRECTORY_PATH,
                    self.transactions_path.clone(),
                    source,
                )
            })?;
        fs.remove_file(
            &self.transactions_directory,
            Path::new(&intent_name),
            &intent_path,
        )
        .map_err(|source| {
            filesystem_operation_error(
                "remove unpublished transaction intent link",
                KIT_DIRECTORY_PATH,
                intent_path,
                source,
            )
        })?;
        fs.sync_directory(&self.transactions_directory, &self.transactions_path)
            .map_err(|source| {
                filesystem_operation_error(
                    "sync unpublished transaction intent cleanup",
                    KIT_DIRECTORY_PATH,
                    self.transactions_path.clone(),
                    source,
                )
            })
    }

    fn persist(&self, fs: &dyn FsOps) -> Result<(), CodegenError> {
        let content = serialize_journal(&self.data, &self.path)?;
        let update_name = format!(
            "{JOURNAL_UPDATE_PREFIX}{}-{}",
            self.data.transaction_id,
            random_hex(&self.path)?
        );
        let update_path = self.transactions_path.join(&update_name);
        let mut created = fs
            .create_new_file(
                &self.transactions_directory,
                Path::new(&update_name),
                &update_path,
                0o600,
            )
            .map_err(|source| {
                filesystem_operation_error(
                    "create transaction journal update",
                    KIT_DIRECTORY_PATH,
                    update_path.clone(),
                    source,
                )
            })?;
        let prepare = (|| {
            fs.write_handle(&mut created.file, &update_path, &content)
                .map_err(|source| {
                    filesystem_operation_error(
                        "write transaction journal update",
                        KIT_DIRECTORY_PATH,
                        update_path.clone(),
                        source,
                    )
                })?;
            fs.sync_handle(&created.file, &update_path)
                .map_err(|source| {
                    filesystem_operation_error(
                        "sync transaction journal update",
                        KIT_DIRECTORY_PATH,
                        update_path.clone(),
                        source,
                    )
                })
        })();
        drop(created.file);
        if let Err(error) = prepare {
            let _ = fs.remove_file(
                &self.transactions_directory,
                Path::new(&update_name),
                &update_path,
            );
            return Err(error);
        }
        if let Err(source) = fs.rename_journal(
            &self.transactions_directory,
            Path::new(&update_name),
            &update_path,
            &self.transactions_directory,
            Path::new(&self.name),
            &self.path,
        ) {
            let _ = fs.remove_file(
                &self.transactions_directory,
                Path::new(&update_name),
                &update_path,
            );
            return Err(filesystem_operation_error(
                "publish transaction journal update",
                KIT_DIRECTORY_PATH,
                self.path.clone(),
                source,
            ));
        }
        fs.sync_directory(&self.transactions_directory, &self.transactions_path)
            .map_err(|source| {
                filesystem_operation_error(
                    "sync published transaction journal",
                    KIT_DIRECTORY_PATH,
                    self.transactions_path.clone(),
                    source,
                )
            })
    }

    fn remove(self, fs: &dyn FsOps) -> Result<(), CodegenError> {
        fs.remove_file(
            &self.transactions_directory,
            Path::new(&self.name),
            &self.path,
        )
        .map_err(|source| {
            filesystem_operation_error(
                "remove durable transaction journal",
                KIT_DIRECTORY_PATH,
                self.path.clone(),
                source,
            )
        })?;
        fs.sync_directory(&self.transactions_directory, &self.transactions_path)
            .map_err(|source| {
                filesystem_operation_error(
                    "sync removed transaction journal",
                    KIT_DIRECTORY_PATH,
                    self.transactions_path.clone(),
                    source,
                )
            })?;
        drop(self.transactions_directory);
        fs.remove_dir(
            &self.kit_directory,
            Path::new(TRANSACTIONS_DIRECTORY_NAME),
            &self.transactions_path,
        )
        .map_err(|source| {
            filesystem_operation_error(
                "remove empty transaction directory",
                KIT_DIRECTORY_PATH,
                self.transactions_path.clone(),
                source,
            )
        })?;
        fs.sync_directory(&self.kit_directory, &self.transactions_path)
            .map_err(|source| {
                filesystem_operation_error(
                    "sync transaction directory removal",
                    KIT_DIRECTORY_PATH,
                    self.transactions_path,
                    source,
                )
            })
    }
}

fn serialize_journal(data: &TransactionJournalData, path: &Path) -> Result<Vec<u8>, CodegenError> {
    let mut content = serde_json::to_vec_pretty(data).map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidData, source),
    })?;
    content.push(b'\n');
    Ok(content)
}

fn random_hex(path: &Path) -> Result<String, CodegenError> {
    let mut random = [0_u8; AUXILIARY_RANDOM_BYTES];
    getrandom::fill(&mut random).map_err(|error| CodegenError::Io {
        path: path.to_path_buf(),
        source: io::Error::other(format!("generate random transaction identifier: {error}")),
    })?;
    let mut value = String::with_capacity(random.len() * 2);
    for byte in random {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(value)
}

fn create_private_transaction_directory(parent: &Dir, path: &Path) -> Result<(), CodegenError> {
    #[cfg(unix)]
    {
        use cap_std::fs::DirBuilderExt;

        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        parent
            .create_dir_with(TRANSACTIONS_DIRECTORY_NAME, &builder)
            .map_err(|source| CodegenError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
    #[cfg(not(unix))]
    {
        parent
            .create_dir(TRANSACTIONS_DIRECTORY_NAME)
            .map_err(|source| CodegenError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
}

fn validate_transaction_directory_metadata(
    path: &Path,
    metadata: &cap_std::fs::Metadata,
) -> Result<(), CodegenError> {
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(CodegenError::UnsafePath {
            path: path.display().to_string(),
            reason: "transaction coordination path is not a no-follow directory".to_owned(),
        });
    }
    #[cfg(windows)]
    if cap_fs_ext::OsMetadataExt::file_attributes(metadata) & 0x0000_0400 != 0 {
        return Err(CodegenError::UnsafePath {
            path: path.display().to_string(),
            reason: "transaction coordination path is a Windows reparse point".to_owned(),
        });
    }
    Ok(())
}

fn path_depth(path: &str) -> usize {
    Path::new(path).components().count()
}

fn apply_publication_mode(
    fs: &dyn FsOps,
    created: &CreatedFile,
    logical_path: &str,
    stage_path: &Path,
    preimage: Option<&PathPreimage>,
) -> Result<(), CodegenError> {
    #[cfg(unix)]
    let posix_mode = match preimage {
        Some(PathPreimage::RegularFile { mode, .. }) => mode.posix_mode,
        Some(PathPreimage::Absent) => Some(0o644),
        None => None,
    };
    #[cfg(unix)]
    if let Some(posix_mode) = posix_mode {
        fs.set_file_mode(&created.file, stage_path, posix_mode)
            .map_err(|source| {
                filesystem_operation_error(
                    "set transaction stage publication mode",
                    logical_path,
                    stage_path.to_path_buf(),
                    source,
                )
            })?;
    }
    #[cfg(not(unix))]
    let _ = (fs, created, logical_path, stage_path, preimage);
    Ok(())
}

fn rollback_or_recovery_required(
    transaction: &PlanningContext,
    fs: &dyn FsOps,
    staged: &[StagedFile],
    mut journal: DurableJournal,
    committed: usize,
    original: CodegenError,
) -> CodegenError {
    journal.data.state = JournalState::RollingBack { count: committed };
    let journal_update = journal.persist(fs);
    let preserve_unattributed_edit = matches!(
        &original,
        CodegenError::PreimageConflict { .. }
            | CodegenError::UnsafePath { .. }
            | CodegenError::ProjectRootChanged { .. }
    );
    let rollback = rollback_transaction(
        transaction,
        fs,
        staged,
        &journal.data.created_directories,
        preserve_unattributed_edit,
    );
    if rollback.is_ok() {
        let journal_path = journal.path.clone();
        match journal.remove(fs) {
            Ok(()) => original,
            Err(cleanup) => CodegenError::RecoveryRequired {
                journal_path,
                reason: format!(
                    "application content was rolled back but journal cleanup failed: {cleanup}; original failure: {original}"
                ),
            },
        }
    } else {
        CodegenError::RecoveryRequired {
            journal_path: journal.path.clone(),
            reason: format!(
                "rollback failed after {original}: {}; durable-state update: {}",
                rollback.expect_err("rollback branch"),
                journal_update
                    .err()
                    .map_or_else(|| "recorded".to_owned(), |error| error.to_string())
            ),
        }
    }
}

fn rollback_transaction(
    transaction: &PlanningContext,
    fs: &dyn FsOps,
    staged: &[StagedFile],
    created_directories: &[JournalDirectory],
    preserve_unattributed_edit: bool,
) -> Result<(), CodegenError> {
    for file in staged.iter().rev() {
        let current = match transaction.inspect_path_uncached(&file.logical_path) {
            Ok(current) => current,
            Err(_) if preserve_unattributed_edit => continue,
            Err(error) => return Err(error),
        };
        if target_matches_preimage(&current, &file.preimage) {
            continue;
        }
        if !matches!(
            current,
            PathPreimage::RegularFile { ref content_hash, ref mode }
                if content_hash == &file.planned_hash
                    && file
                        .planned_posix_mode
                        .is_none_or(|expected| mode.posix_mode == Some(expected))
        ) {
            if preserve_unattributed_edit {
                continue;
            }
            return Err(CodegenError::RecoveryRequired {
                journal_path: file.target_path.clone(),
                reason: format!(
                    "cannot roll back third-state application path {}",
                    file.logical_path
                ),
            });
        }
        match &file.backup {
            Some(backup) => {
                validate_auxiliary(&file.parent, backup, &file.logical_path, "backup")?;
                fs.rename(
                    &file.parent,
                    Path::new(&backup.name),
                    &backup.path,
                    &file.parent,
                    Path::new(&file.target_name),
                    &file.target_path,
                )
                .map_err(|source| {
                    filesystem_operation_error(
                        "restore target backup",
                        &file.logical_path,
                        file.target_path.clone(),
                        source,
                    )
                })?;
                fs.sync_directory(&file.parent, &file.target_path)
                    .map_err(|source| {
                        filesystem_operation_error(
                            "sync restored target directory",
                            &file.logical_path,
                            file.target_path.clone(),
                            source,
                        )
                    })?;
            }
            None => {
                fs.remove_file(
                    &file.parent,
                    Path::new(&file.target_name),
                    &file.target_path,
                )
                .map_err(|source| {
                    filesystem_operation_error(
                        "remove newly created target during rollback",
                        &file.logical_path,
                        file.target_path.clone(),
                        source,
                    )
                })?;
                fs.sync_directory(&file.parent, &file.target_path)
                    .map_err(|source| {
                        filesystem_operation_error(
                            "sync removed rollback target directory",
                            &file.logical_path,
                            file.target_path.clone(),
                            source,
                        )
                    })?;
            }
        }
    }
    cleanup_auxiliaries_strict(fs, staged)?;
    cleanup_created_directories_strict(transaction, fs, created_directories)
}

fn cleanup_created_directory_paths(
    transaction: &PlanningContext,
    fs: &dyn FsOps,
    directories: &[String],
) {
    let mut unique = directories.iter().cloned().collect::<HashSet<_>>();
    let mut directories = unique.drain().collect::<Vec<_>>();
    directories.sort_by(|left, right| {
        path_depth(right)
            .cmp(&path_depth(left))
            .then_with(|| right.cmp(left))
    });
    for logical_path in directories {
        let Ok((parent, name)) = transaction.open_parent(&logical_path) else {
            continue;
        };
        let _ = fs.remove_dir(
            &parent,
            Path::new(&name),
            &transaction.project_root().join(&logical_path),
        );
    }
}

fn cleanup_auxiliaries_strict(fs: &dyn FsOps, staged: &[StagedFile]) -> Result<(), CodegenError> {
    for file in staged.iter().rev() {
        for auxiliary in [file.backup.as_ref(), Some(&file.stage)]
            .into_iter()
            .flatten()
        {
            if let Some(observed) =
                read_optional_auxiliary(&file.parent, &auxiliary.name, &auxiliary.path)?
                && (observed.identity != auxiliary.identity
                    || observed.content_hash != auxiliary.content_hash
                    || observed.length != auxiliary.length
                    || observed.posix_mode != auxiliary.posix_mode)
            {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: auxiliary.path.clone(),
                    reason: format!(
                        "refusing to remove substituted transaction artifact for {}",
                        file.logical_path
                    ),
                });
            }
            match fs.remove_file(&file.parent, Path::new(&auxiliary.name), &auxiliary.path) {
                Ok(()) => fs
                    .sync_directory(&file.parent, &file.target_path)
                    .map_err(|source| {
                        filesystem_operation_error(
                            "sync transaction auxiliary cleanup",
                            &file.logical_path,
                            file.target_path.clone(),
                            source,
                        )
                    })?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(filesystem_operation_error(
                        "remove transaction auxiliary",
                        &file.logical_path,
                        auxiliary.path.clone(),
                        source,
                    ));
                }
            }
        }
    }
    Ok(())
}

fn cleanup_created_directories_strict(
    transaction: &PlanningContext,
    fs: &dyn FsOps,
    directories: &[JournalDirectory],
) -> Result<(), CodegenError> {
    let mut directories = directories.to_vec();
    directories.sort_by(|left, right| {
        path_depth(&right.logical_path)
            .cmp(&path_depth(&left.logical_path))
            .then_with(|| right.logical_path.cmp(&left.logical_path))
    });
    directories.dedup_by(|left, right| left.logical_path == right.logical_path);
    for directory in directories {
        let path = transaction.project_root().join(&directory.logical_path);
        let current = match transaction.open_directory(&directory.logical_path) {
            Ok(current) => current,
            Err(CodegenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => return Err(error),
        };
        let metadata = current.dir_metadata().map_err(|source| CodegenError::Io {
            path: path.clone(),
            source,
        })?;
        let Some(identity) = directory.identity else {
            return Err(CodegenError::RecoveryRequired {
                journal_path: path,
                reason: format!(
                    "transaction-created directory {} has no durably recorded identity",
                    directory.logical_path
                ),
            });
        };
        if (MetadataExt::dev(&metadata), MetadataExt::ino(&metadata))
            != (identity.device, identity.inode)
        {
            return Err(CodegenError::RecoveryRequired {
                journal_path: path,
                reason: format!(
                    "refusing to remove substituted transaction-created directory {}",
                    directory.logical_path
                ),
            });
        }
        drop(current);
        let (parent, name) = transaction.open_parent(&directory.logical_path)?;
        match fs.remove_dir(&parent, Path::new(&name), &path) {
            Ok(()) => fs.sync_directory(&parent, &path).map_err(|source| {
                filesystem_operation_error(
                    "sync transaction-created directory removal",
                    &directory.logical_path,
                    path.clone(),
                    source,
                )
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(filesystem_operation_error(
                    "remove transaction-created directory",
                    directory.logical_path,
                    path,
                    source,
                ));
            }
        }
    }
    Ok(())
}

fn finish_successful_transaction(
    fs: &dyn FsOps,
    staged: &[StagedFile],
    journal: DurableJournal,
) -> Result<(), CodegenError> {
    let journal_path = journal.path.clone();
    if let Err(error) = cleanup_successful_backups(fs, staged) {
        return Err(CodegenError::RecoveryRequired {
            journal_path,
            reason: format!(
                "transaction is durably committed but finish-only cleanup failed: {error}"
            ),
        });
    }
    journal
        .remove(fs)
        .map_err(|error| CodegenError::RecoveryRequired {
            journal_path,
            reason: format!("transaction is durably committed but journal cleanup failed: {error}"),
        })
}

fn cleanup_successful_backups(fs: &dyn FsOps, staged: &[StagedFile]) -> Result<(), CodegenError> {
    for file in staged.iter().rev() {
        if let Some(backup) = &file.backup {
            validate_auxiliary(&file.parent, backup, &file.logical_path, "backup")?;
            fs.remove_file(&file.parent, Path::new(&backup.name), &backup.path)
                .map_err(|source| {
                    filesystem_operation_error(
                        "remove committed target backup",
                        &file.logical_path,
                        backup.path.clone(),
                        source,
                    )
                })?;
            fs.sync_directory(&file.parent, &file.target_path)
                .map_err(|source| {
                    filesystem_operation_error(
                        "sync committed backup cleanup",
                        &file.logical_path,
                        file.target_path.clone(),
                        source,
                    )
                })?;
        }
    }
    Ok(())
}

fn filesystem_operation_error(
    operation: &'static str,
    logical_path: impl Into<String>,
    path: PathBuf,
    source: io::Error,
) -> CodegenError {
    CodegenError::FilesystemOperation {
        operation,
        logical_path: logical_path.into(),
        path,
        source,
    }
}
