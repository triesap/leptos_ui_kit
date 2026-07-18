use std::{
    collections::{BTreeSet, HashSet},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
#[cfg(unix)]
use cap_std::fs::DirBuilder;
use serde::{Deserialize, Serialize};

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
const KIT_DIRECTORY_PATH: &str = "src/components/ui/_kit";
const TRANSACTIONS_DIRECTORY_NAME: &str = ".transactions";
const TRANSACTION_JOURNAL_VERSION: u32 = 1;
const TRANSACTION_JOURNAL_PREFIX: &str = "transaction-";
const TRANSACTION_JOURNAL_SUFFIX: &str = ".json";
const JOURNAL_UPDATE_PREFIX: &str = "journal-update-";

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
    planned_hash: String,
    backup: Option<AuxiliaryFile>,
    created_directories: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransactionJournalData {
    version: u32,
    transaction_id: String,
    project: JournalProject,
    state: JournalState,
    entries: Vec<JournalEntry>,
    created_directories: Vec<String>,
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
    Prepared,
    Replacing { index: usize },
    Committed { count: usize },
    RollingBack { count: usize },
    Applied,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JournalEntry {
    logical_path: String,
    stage_name: String,
    backup_name: Option<String>,
    preimage: JournalPreimage,
    planned_hash: String,
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
                cleanup_prepared_cohort(transaction, fs.as_ref(), &staged);
                return Err(error);
            }
        }
    }

    if let Err(error) = snapshot.revalidate_all(transaction) {
        cleanup_prepared_cohort(transaction, fs.as_ref(), &staged);
        return Err(error);
    }

    for index in 0..staged.len() {
        match backup_file(transaction, &staged[index], snapshot, fs.as_ref()) {
            Ok(backup) => staged[index].backup = backup,
            Err(error) => {
                cleanup_prepared_cohort(transaction, fs.as_ref(), &staged);
                return Err(error);
            }
        }
    }

    if let Err(error) = snapshot.revalidate_all(transaction) {
        cleanup_prepared_cohort(transaction, fs.as_ref(), &staged);
        return Err(error);
    }

    commit_staged_cohort(transaction, staged, snapshot, fs.as_ref())
}

fn commit_staged_cohort(
    transaction: &PlanningContext,
    staged: Vec<StagedFile>,
    snapshot: &PlanSnapshot,
    fs: &dyn FsOps,
) -> Result<(), CodegenError> {
    let mut journal = match DurableJournal::create(transaction, fs, &staged, snapshot) {
        Ok(journal) => journal,
        Err(error) => {
            cleanup_prepared_cohort(transaction, fs, &staged);
            return Err(error);
        }
    };
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
    let mut staged = stage_bytes(&transaction, logical_path, content, &snapshot, &fs)?;
    if let Err(error) = snapshot.revalidate_all(&transaction) {
        cleanup_prepared_cohort(&transaction, &fs, std::slice::from_ref(&staged));
        return Err(error);
    }
    match backup_file(&transaction, &staged, &snapshot, &fs) {
        Ok(backup) => staged.backup = backup,
        Err(error) => {
            cleanup_prepared_cohort(&transaction, &fs, std::slice::from_ref(&staged));
            return Err(error);
        }
    }
    if let Err(error) = snapshot.revalidate_all(&transaction) {
        cleanup_prepared_cohort(&transaction, &fs, std::slice::from_ref(&staged));
        return Err(error);
    }
    commit_staged_cohort(&transaction, vec![staged], &snapshot, &fs)?;
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
    let created_directories =
        transaction.ensure_parent_with(logical_path, |directory, created| {
            let path = transaction.project_root().join(directory);
            if created {
                fs.after_create_directory(&path)
            } else {
                fs.before_create_directory(&path)
            }
            .map_err(|source| CodegenError::Io { path, source })
        })?;
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
        cleanup_created_directories(transaction, fs, &created_directories);
        return Err(error);
    }

    Ok(StagedFile {
        logical_path: logical_path.to_owned(),
        target_path,
        target_name,
        parent,
        stage,
        stage_identity,
        planned_hash: crate::hash_content_bytes(content),
        backup: None,
        created_directories,
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

impl DurableJournal {
    fn create(
        transaction: &PlanningContext,
        fs: &dyn FsOps,
        staged: &[StagedFile],
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
        let mut created_directories = staged
            .iter()
            .flat_map(|file| file.created_directories.iter().cloned())
            .collect::<Vec<_>>();
        created_directories.sort_by(|left, right| {
            path_depth(left)
                .cmp(&path_depth(right))
                .then_with(|| left.cmp(right))
        });
        created_directories.dedup();
        let entries = staged
            .iter()
            .map(|file| {
                let preimage = match snapshot
                    .preimage(&file.logical_path)
                    .expect("staged path has a validated preimage")
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
                    logical_path: file.logical_path.clone(),
                    stage_name: file.stage.name.clone(),
                    backup_name: file.backup.as_ref().map(|backup| backup.name.clone()),
                    preimage,
                    planned_hash: file.planned_hash.clone(),
                }
            })
            .collect();
        let data = TransactionJournalData {
            version: TRANSACTION_JOURNAL_VERSION,
            transaction_id,
            project: JournalProject {
                canonical_root: canonical_root.to_string_lossy().into_owned(),
                device,
                inode,
            },
            state: JournalState::Prepared,
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
        let mut created = fs
            .create_new_file(
                &self.transactions_directory,
                Path::new(&self.name),
                &self.path,
                0o600,
            )
            .map_err(|source| CodegenError::Io {
                path: self.path.clone(),
                source,
            })?;
        let result = (|| {
            fs.write_handle(&mut created.file, &self.path, &content)
                .map_err(|source| CodegenError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            fs.sync_handle(&created.file, &self.path)
                .map_err(|source| CodegenError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            fs.sync_directory(&self.transactions_directory, &self.transactions_path)
                .map_err(|source| CodegenError::Io {
                    path: self.transactions_path.clone(),
                    source,
                })
        })();
        drop(created.file);
        if result.is_err() {
            let _ = fs.remove_file(
                &self.transactions_directory,
                Path::new(&self.name),
                &self.path,
            );
        }
        result
    }

    fn persist(&self, fs: &dyn FsOps) -> Result<(), CodegenError> {
        let content = serialize_journal(&self.data, &self.path)?;
        let update_name = format!("{JOURNAL_UPDATE_PREFIX}{}", random_hex(&self.path)?);
        let update_path = self.transactions_path.join(&update_name);
        let mut created = fs
            .create_new_file(
                &self.transactions_directory,
                Path::new(&update_name),
                &update_path,
                0o600,
            )
            .map_err(|source| CodegenError::Io {
                path: update_path.clone(),
                source,
            })?;
        let prepare = (|| {
            fs.write_handle(&mut created.file, &update_path, &content)
                .map_err(|source| CodegenError::Io {
                    path: update_path.clone(),
                    source,
                })?;
            fs.sync_handle(&created.file, &update_path)
                .map_err(|source| CodegenError::Io {
                    path: update_path.clone(),
                    source,
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
            return Err(CodegenError::Io {
                path: self.path.clone(),
                source,
            });
        }
        fs.sync_directory(&self.transactions_directory, &self.transactions_path)
            .map_err(|source| CodegenError::Io {
                path: self.transactions_path.clone(),
                source,
            })
    }

    fn remove(self, fs: &dyn FsOps) -> Result<(), CodegenError> {
        fs.remove_file(
            &self.transactions_directory,
            Path::new(&self.name),
            &self.path,
        )
        .map_err(|source| CodegenError::Io {
            path: self.path.clone(),
            source,
        })?;
        fs.sync_directory(&self.transactions_directory, &self.transactions_path)
            .map_err(|source| CodegenError::Io {
                path: self.transactions_path.clone(),
                source,
            })?;
        drop(self.transactions_directory);
        fs.remove_dir(
            &self.kit_directory,
            Path::new(TRANSACTIONS_DIRECTORY_NAME),
            &self.transactions_path,
        )
        .map_err(|source| CodegenError::Io {
            path: self.transactions_path.clone(),
            source,
        })?;
        fs.sync_directory(&self.kit_directory, &self.transactions_path)
            .map_err(|source| CodegenError::Io {
                path: self.transactions_path,
                source,
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
    let rollback = rollback_transaction(transaction, fs, staged, committed);
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
    committed: usize,
) -> Result<(), CodegenError> {
    for file in staged.iter().take(committed).rev() {
        match &file.backup {
            Some(backup) => fs
                .rename(
                    &file.parent,
                    Path::new(&backup.name),
                    &backup.path,
                    &file.parent,
                    Path::new(&file.target_name),
                    &file.target_path,
                )
                .map_err(|source| CodegenError::Io {
                    path: file.target_path.clone(),
                    source,
                })?,
            None => fs
                .remove_file(
                    &file.parent,
                    Path::new(&file.target_name),
                    &file.target_path,
                )
                .map_err(|source| CodegenError::Io {
                    path: file.target_path.clone(),
                    source,
                })?,
        }
    }
    cleanup_auxiliaries_strict(fs, staged)?;
    let created_directories = staged
        .iter()
        .flat_map(|file| file.created_directories.iter().cloned())
        .collect::<Vec<_>>();
    cleanup_created_directories_strict(transaction, fs, &created_directories)
}

fn cleanup_prepared_cohort(transaction: &PlanningContext, fs: &dyn FsOps, staged: &[StagedFile]) {
    cleanup_uncommitted_auxiliaries(fs, staged);
    let created_directories = staged
        .iter()
        .flat_map(|file| file.created_directories.iter().cloned())
        .collect::<Vec<_>>();
    cleanup_created_directories(transaction, fs, &created_directories);
}

fn cleanup_created_directories(
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
            match fs.remove_file(&file.parent, Path::new(&auxiliary.name), &auxiliary.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(CodegenError::Io {
                        path: auxiliary.path.clone(),
                        source,
                    });
                }
            }
        }
    }
    Ok(())
}

fn cleanup_created_directories_strict(
    transaction: &PlanningContext,
    fs: &dyn FsOps,
    directories: &[String],
) -> Result<(), CodegenError> {
    let mut unique = directories.iter().cloned().collect::<HashSet<_>>();
    let mut directories = unique.drain().collect::<Vec<_>>();
    directories.sort_by(|left, right| {
        path_depth(right)
            .cmp(&path_depth(left))
            .then_with(|| right.cmp(left))
    });
    for logical_path in directories {
        let (parent, name) = transaction.open_parent(&logical_path)?;
        let path = transaction.project_root().join(&logical_path);
        match fs.remove_dir(&parent, Path::new(&name), &path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(CodegenError::Io { path, source }),
        }
    }
    Ok(())
}

fn finish_successful_transaction(
    fs: &dyn FsOps,
    staged: &[StagedFile],
    journal: DurableJournal,
) -> Result<(), CodegenError> {
    cleanup_successful_backups(fs, staged)?;
    journal.remove(fs)
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
