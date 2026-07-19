use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use cap_fs_ext::{DirExt, MetadataExt};
use cap_std::fs::Dir;

use crate::path_safety::{ObjectIdentity, PlanningContext};
use crate::{
    ChangeKind, ChangeRecord, CodegenError, DEFAULT_KIT_LOCK_PATH, PathPreimage, PlanSnapshot,
    PlannedFile, PlannedFileAction, PreservedFileMode, lock_to_json_at_path,
    parse_install_lock_str_at_path, validate_planned_write_paths,
};

use super::authority::TransactionAuthority;
use super::fs::{
    DirectoryEndpoint, ExactDirectoryObservation, ExactFileMetadataObservation,
    ExactFileObservation, ExactRelocationSource, ExclusiveCreateFailure, ExclusiveFileCopyOutcome,
    FsOps, HardLinkEndpoint, ParentSyncKind,
};
use super::journal::{
    ArtifactOrdinal, CleanupIntentV2, CleanupTargetV2, DirectoryModeV2, DirectoryParentV2,
    DirectoryPublicationObservationV2, DirectoryPublishIntentV2, EntryActionV2, EntryRoleV2,
    ExactDirectoryStateV2, ExactFileStateV2, FileArtifactKindV2, FileModePolicyV2,
    FilePlacementIntentV2, FilePlacementObservationV2, JournalDirectoryV2, JournalEntryV2,
    JournalOperationV2, JournalPhaseV2, OwnedResidualDeleteBindingV2, OwnedResidualObjectV2,
    OwnerArtifactKindV2, PlannedFileStateV2, PreimageV2, PreparationObservationV2,
    PreparationPendingIntentV2, PreparationPlacementIntentV2, PresenceV2, RecordBindingV2,
    ReplacementObservationV2, RollbackIntentV2, Sha256Digest, TransactionId,
};
use super::lock::WriteLock;
use super::recovery_policy::{MutationWorldV2, RecoveryPreflightV2, RecoveryPreparationArtifactV2};
use super::runtime::{
    CleanupObjectKind, PreparationArtifactKind, RollbackAction, TransactionOutcome,
    TransactionRuntime, TransitionKey, TransitionWindow,
};
use super::store::{ActiveRecoveryAdoptionAuthority, LoadedJournal};
use super::store::{exact_directory, exact_file};
use super::writer::{
    ImmutableJournalStore, exact_existing_directory, model_error_at, transaction_io,
};

struct OrderedFile<'a> {
    file: TransactionFile<'a>,
    ordinal: ArtifactOrdinal,
    role: EntryRoleV2,
}

#[derive(Clone, Copy)]
enum TransactionFile<'a> {
    Text(&'a PlannedFile),
    Bytes {
        path: &'a str,
        content: &'a [u8],
        action: PlannedFileAction,
    },
}

impl<'a> TransactionFile<'a> {
    fn path(self) -> &'a str {
        match self {
            Self::Text(file) => &file.path,
            Self::Bytes { path, .. } => path,
        }
    }

    fn content(self) -> &'a [u8] {
        match self {
            Self::Text(file) => file.content.as_bytes(),
            Self::Bytes { content, .. } => content,
        }
    }

    const fn action(self) -> PlannedFileAction {
        match self {
            Self::Text(file) => file.action,
            Self::Bytes { action, .. } => action,
        }
    }
}

struct PreparedFile {
    ordinal: ArtifactOrdinal,
    logical_path: String,
    target_name: String,
    target_path: PathBuf,
    stage_parent: Dir,
    stage_parent_path: PathBuf,
    stage_name: String,
    stage_path: PathBuf,
    stage: ExactFileObservation,
    backup_name: Option<String>,
    backup_path: Option<PathBuf>,
    backup: Option<ExactFileObservation>,
}

pub(crate) fn apply_planned_files_locked(
    context: &PlanningContext,
    lock: &WriteLock,
    files: &[PlannedFile],
    changes: &[ChangeRecord],
    snapshot: &PlanSnapshot,
    operation: JournalOperationV2,
) -> Result<(), CodegenError> {
    apply_exact_transaction(
        context,
        lock,
        files,
        changes,
        snapshot,
        TransactionRuntime::system(),
        operation,
    )
}

pub(super) fn apply_exact_transaction(
    context: &PlanningContext,
    lock: &WriteLock,
    files: &[PlannedFile],
    changes: &[ChangeRecord],
    snapshot: &PlanSnapshot,
    runtime: TransactionRuntime,
    operation: JournalOperationV2,
) -> Result<(), CodegenError> {
    lock.validate_context(context)?;
    let paths = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    validate_planned_write_paths(&paths)?;
    validate_actions(files, snapshot)?;
    snapshot.revalidate_all(context)?;
    let ordered = order_and_validate_lock(files, changes, operation)?;
    if files.is_empty() {
        return Ok(());
    }

    execute_ordered(context, lock, snapshot, runtime, operation, ordered)
}

fn execute_ordered(
    context: &PlanningContext,
    lock: &WriteLock,
    snapshot: &PlanSnapshot,
    runtime: TransactionRuntime,
    operation: JournalOperationV2,
    ordered: Vec<OrderedFile<'_>>,
) -> Result<(), CodegenError> {
    let build_runtime = runtime.clone();
    let mut store = ImmutableJournalStore::create(
        context,
        lock,
        runtime,
        operation,
        |transaction_id, project| {
            let entries = build_entries(
                context,
                build_runtime.fs(),
                snapshot,
                transaction_id,
                &ordered,
            )?;
            let directories = build_directories(
                context,
                build_runtime.fs(),
                snapshot,
                transaction_id,
                project,
                &ordered,
            )?;
            Ok((entries, directories))
        },
    )?;

    let mut prepared_authority = None;
    let execution = (|| {
        preflight_preparation_filesystems(context, lock, &store)?;
        prepare_directories(context, lock, &mut store)?;
        prepared_authority = Some(prepare_files(
            context, lock, snapshot, &ordered, &mut store,
        )?);
        let prepared = prepared_authority
            .as_ref()
            .expect("prepared authority was just installed");
        let record = store
            .records()
            .last()
            .expect("initial journal record exists")
            .clone();
        let prepared_snapshot = store
            .snapshot()
            .mark_prepared(record)
            .map_err(model_error_at(context.project_root()))?;
        store.publish_successor(prepared_snapshot)?;
        snapshot.revalidate_all(context)?;
        commit_files(context, lock, snapshot, &prepared, &mut store)
    })();
    match execution {
        Ok(()) => Ok(()),
        Err(original) => {
            let finish_only = store.snapshot().phase().desired_state_is_irreversible();
            let salvage_error = if !finish_only && let Some(prepared) = prepared_authority.as_ref()
            {
                salvage_exact_unpublished_stages(store.runtime(), prepared).err()
            } else {
                None
            };
            // Never recover from a stale in-memory journal handle. Drop every
            // cached capability and enter the same rediscover -> stable double
            // capture -> preflight -> one-step loop used by later commands.
            // This keeps an ordinary I/O error from bypassing the bounded
            // crash-recovery protocol merely because it happened in-process.
            drop(store);
            let recovery = super::recovery::recover_pending_locked(context, lock);
            match (finish_only, recovery) {
                // Once CommitComplete is durable, successful finish-only
                // reconciliation proves the requested cohort is installed.
                // Returning the pre-reconciliation I/O error here would
                // report failure with a committed tree and no recovery
                // authority, inviting callers to retry a completed command.
                (true, Ok(())) => Ok(()),
                (false, Ok(())) => Err(salvage_error.unwrap_or(original)),
                (_, Err(recovery_required)) => Err(recovery_required),
            }
        }
    }
}

pub fn write_file_atomic(
    project_root: &Path,
    logical_path: &str,
    content: &[u8],
) -> Result<(), CodegenError> {
    if logical_path == DEFAULT_KIT_LOCK_PATH {
        return Err(CodegenError::InvalidCoordinationState {
            path: logical_path.to_owned(),
            reason: "the canonical install lock may only be written as the final entry of an init, add, or sync cohort"
                .to_owned(),
        });
    }
    let context = PlanningContext::open(project_root)?;
    let lock = WriteLock::acquire_with_context(&context)?;
    super::recovery::recover_pending_locked(&context, &lock)?;
    context.observe_path(logical_path)?;
    let snapshot = context.finish_snapshot();
    let preimage =
        snapshot
            .preimage(logical_path)
            .ok_or_else(|| CodegenError::PreimageConflict {
                path: logical_path.to_owned(),
                reason: "atomic target has no exact preimage".to_owned(),
            })?;
    let action = match preimage {
        PathPreimage::Absent => PlannedFileAction::Create,
        PathPreimage::RegularFile { mode, .. } if !mode.readonly => PlannedFileAction::Update,
        PathPreimage::RegularFile { .. } => {
            return Err(CodegenError::PreimageConflict {
                path: logical_path.to_owned(),
                reason: "atomic target is readonly".to_owned(),
            });
        }
    };
    validate_planned_write_paths(&[logical_path.to_owned()])?;
    snapshot.revalidate_all(&context)?;
    let ordered = vec![OrderedFile {
        file: TransactionFile::Bytes {
            path: logical_path,
            content,
            action,
        },
        ordinal: ArtifactOrdinal::new(0).map_err(model_error_at(logical_path))?,
        role: EntryRoleV2::Ordinary,
    }];
    execute_ordered(
        &context,
        &lock,
        &snapshot,
        TransactionRuntime::system(),
        JournalOperationV2::AtomicWrite,
        ordered,
    )
}

fn validate_actions(files: &[PlannedFile], snapshot: &PlanSnapshot) -> Result<(), CodegenError> {
    for file in files {
        let preimage =
            snapshot
                .preimage(&file.path)
                .ok_or_else(|| CodegenError::PreimageConflict {
                    path: file.path.clone(),
                    reason: "planned target has no exact preimage".to_owned(),
                })?;
        match (file.action, preimage) {
            (PlannedFileAction::Create, PathPreimage::Absent) => {}
            (PlannedFileAction::Update, PathPreimage::RegularFile { mode, .. })
                if !mode.readonly => {}
            (PlannedFileAction::Update, PathPreimage::RegularFile { .. }) => {
                return Err(CodegenError::PreimageConflict {
                    path: file.path.clone(),
                    reason: "planned update target is readonly".to_owned(),
                });
            }
            (PlannedFileAction::Create, PathPreimage::RegularFile { .. }) => {
                return Err(CodegenError::PreimageConflict {
                    path: file.path.clone(),
                    reason: "create action requires an absent preimage".to_owned(),
                });
            }
            (PlannedFileAction::Update, PathPreimage::Absent) => {
                return Err(CodegenError::PreimageConflict {
                    path: file.path.clone(),
                    reason: "update action requires a regular-file preimage".to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn order_and_validate_lock<'a>(
    files: &'a [PlannedFile],
    changes: &[ChangeRecord],
    operation: JournalOperationV2,
) -> Result<Vec<OrderedFile<'a>>, CodegenError> {
    if files.is_empty() {
        if changes.is_empty() {
            return Ok(Vec::new());
        }
        return Err(CodegenError::InvalidCoordinationState {
            path: "transaction cohort".to_owned(),
            reason: "an empty mutation cohort cannot report filesystem changes".to_owned(),
        });
    }

    let markers = changes
        .iter()
        .filter(|change| change.kind == ChangeKind::WriteLockFile)
        .collect::<Vec<_>>();
    let selected = match operation {
        JournalOperationV2::Init | JournalOperationV2::Add | JournalOperationV2::Sync => {
            if markers.len() != 1 {
                return Err(CodegenError::InvalidCoordinationState {
                    path: "transaction cohort".to_owned(),
                    reason: "a nonempty init, add, or sync cohort must contain exactly one install-lock change marker"
                        .to_owned(),
                });
            }
            let marker = markers[0];
            if marker.path != DEFAULT_KIT_LOCK_PATH || !marker.tracked {
                return Err(CodegenError::InvalidCoordinationState {
                    path: marker.path.clone(),
                    reason: "the install-lock marker must be tracked and name the canonical kit.lock.json path"
                        .to_owned(),
                });
            }
            if changes.iter().any(|change| {
                change.path == DEFAULT_KIT_LOCK_PATH && change.kind != ChangeKind::WriteLockFile
            }) {
                return Err(CodegenError::InvalidCoordinationState {
                    path: DEFAULT_KIT_LOCK_PATH.to_owned(),
                    reason: "the canonical install lock cannot also carry another change kind"
                        .to_owned(),
                });
            }
            let matching = files
                .iter()
                .filter(|file| file.path == DEFAULT_KIT_LOCK_PATH)
                .collect::<Vec<_>>();
            if matching.len() != 1 {
                return Err(CodegenError::InvalidCoordinationState {
                    path: DEFAULT_KIT_LOCK_PATH.to_owned(),
                    reason: "the install-lock marker must name exactly one cohort target"
                        .to_owned(),
                });
            }
            let path = Path::new(DEFAULT_KIT_LOCK_PATH);
            let parsed = parse_install_lock_str_at_path(&matching[0].content, path)?;
            let canonical = lock_to_json_at_path(&parsed, path)?;
            if matching[0].content != canonical {
                return Err(CodegenError::InvalidCoordinationState {
                    path: DEFAULT_KIT_LOCK_PATH.to_owned(),
                    reason: "the planned install-lock payload is valid but not canonical"
                        .to_owned(),
                });
            }
            Some(DEFAULT_KIT_LOCK_PATH)
        }
        JournalOperationV2::AtomicWrite => {
            if !markers.is_empty() || files.iter().any(|file| file.path == DEFAULT_KIT_LOCK_PATH) {
                return Err(CodegenError::InvalidCoordinationState {
                    path: DEFAULT_KIT_LOCK_PATH.to_owned(),
                    reason: "AtomicWrite cannot select or target the canonical install lock"
                        .to_owned(),
                });
            }
            None
        }
    };

    if let Some(marker) = markers.first()
        && selected.is_some()
        && marker.path != selected.expect("selected install lock")
    {
        return Err(CodegenError::InvalidCoordinationState {
            path: marker.path.clone(),
            reason: "install-lock marker does not name the selected canonical target".to_owned(),
        });
    }
    if selected.is_none() && files.iter().any(|file| file.path == DEFAULT_KIT_LOCK_PATH) {
        return Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_LOCK_PATH.to_owned(),
            reason: "canonical install-lock target is missing its role marker".to_owned(),
        });
    }

    if let Some(selected) = selected
        && files.iter().filter(|file| file.path == selected).count() != 1
    {
        return Err(CodegenError::InvalidCoordinationState {
            path: selected.to_owned(),
            reason: "selected install-lock marker must name exactly one cohort target".to_owned(),
        });
    }

    let mut ordered = files.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        let left_lock = selected == Some(left.path.as_str());
        let right_lock = selected == Some(right.path.as_str());
        left_lock
            .cmp(&right_lock)
            .then_with(|| left.path.cmp(&right.path))
    });
    ordered
        .into_iter()
        .enumerate()
        .map(|(index, file)| {
            Ok(OrderedFile {
                file: TransactionFile::Text(file),
                ordinal: ArtifactOrdinal::new(index as u32).map_err(model_error_at(&file.path))?,
                role: if selected == Some(file.path.as_str()) {
                    EntryRoleV2::InstallLock
                } else {
                    EntryRoleV2::Ordinary
                },
            })
        })
        .collect()
}

fn build_entries(
    context: &PlanningContext,
    fs: &dyn FsOps,
    snapshot: &PlanSnapshot,
    transaction_id: &super::journal::TransactionId,
    ordered: &[OrderedFile<'_>],
) -> Result<Vec<JournalEntryV2>, CodegenError> {
    ordered
        .iter()
        .map(|ordered| {
            let path = ordered.file.path();
            let target = observe_target(
                context,
                fs,
                path,
                snapshot
                    .byte_len(path)
                    .unwrap_or(0)
                    .max(ordered.file.content().len() as u64),
            )?;
            let (action, preimage, mode_policy) =
                match (ordered.file.action(), snapshot.preimage(path), target) {
                    (PlannedFileAction::Create, Some(PathPreimage::Absent), None) => (
                        EntryActionV2::Create,
                        PreimageV2::Absent,
                        FileModePolicyV2::NormalCreateResolveOnStage,
                    ),
                    (
                        PlannedFileAction::Update,
                        Some(PathPreimage::RegularFile { content_hash, mode }),
                        Some(observation),
                    ) if content_hash == &observation.content_hash && mode == &observation.mode => {
                        (
                            EntryActionV2::Replace,
                            PreimageV2::regular(
                                exact_file(&observation).map_err(model_error_at(path))?,
                            ),
                            FileModePolicyV2::PreservePreimage,
                        )
                    }
                    _ => {
                        return Err(CodegenError::PreimageConflict {
                            path: path.to_owned(),
                            reason: "target changed while exact journal intent was constructed"
                                .to_owned(),
                        });
                    }
                };
            JournalEntryV2::new(
                transaction_id,
                ordered.ordinal,
                path,
                action,
                ordered.role,
                preimage,
                PlannedFileStateV2::new(
                    Sha256Digest::parse(&crate::hash_content_bytes(ordered.file.content()))
                        .map_err(model_error_at(path))?,
                    ordered.file.content().len() as u64,
                    mode_policy,
                )
                .map_err(model_error_at(path))?,
            )
            .map_err(model_error_at(path))
        })
        .collect()
}

fn build_directories(
    context: &PlanningContext,
    fs: &dyn FsOps,
    snapshot: &PlanSnapshot,
    transaction_id: &super::journal::TransactionId,
    project: &super::journal::ProjectBindingV2,
    ordered: &[OrderedFile<'_>],
) -> Result<Vec<JournalDirectoryV2>, CodegenError> {
    let mut paths = BTreeSet::new();
    for ordered in ordered {
        paths.extend(logical_parents(ordered.file.path()));
    }
    paths.insert("src/components/ui/_kit/.transactions".to_owned());
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort_by(|left, right| {
        path_depth(left)
            .cmp(&path_depth(right))
            .then_with(|| left.cmp(right))
    });
    paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let ordinal = ArtifactOrdinal::new(index as u32).map_err(model_error_at(&path))?;
            if path == "src/components/ui/_kit/.transactions" {
                return JournalDirectoryV2::existing(
                    ordinal,
                    path,
                    project.workspace_parent_after_workspace().clone(),
                )
                .map_err(model_error_at(context.project_root()));
            }
            if snapshot.directory_identity(&path).is_some() {
                JournalDirectoryV2::existing(
                    ordinal,
                    &path,
                    exact_existing_directory(context, fs, &path)?,
                )
                .map_err(model_error_at(context.project_root()))
            } else {
                JournalDirectoryV2::create(
                    transaction_id,
                    ordinal,
                    &path,
                    DirectoryModeV2::new(false, normal_directory_mode())
                        .map_err(model_error_at(&path))?,
                )
                .map_err(model_error_at(context.project_root()))
            }
        })
        .collect()
}

const TRANSACTION_NAMESPACE_LOGICAL_PATH: &str = "src/components/ui/_kit/.transactions";

fn open_transaction_workspace(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &ImmutableJournalStore<'_>,
    mutation_path: &Path,
) -> Result<(Dir, Dir, PathBuf), CodegenError> {
    lock.validate_context(context)?;
    let namespace = store.rebind_parent_for_mutation(
        TRANSACTION_NAMESPACE_LOGICAL_PATH,
        store.snapshot().project().workspace_parent_current(),
        mutation_path,
    )?;
    let workspace_name = store.snapshot().project().workspace().name();
    let workspace_path = context
        .project_root()
        .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
        .join(workspace_name);
    let opened = store
        .runtime()
        .fs()
        .open_directory_exact(
            &namespace,
            Path::new(workspace_name),
            &workspace_path,
            store
                .snapshot()
                .project()
                .workspace()
                .exact()
                .mode()
                .posix_mode()
                .unwrap_or(0o700),
        )
        .map_err(|source| {
            transaction_io(
                "reopen transaction workspace",
                TRANSACTION_NAMESPACE_LOGICAL_PATH,
                &workspace_path,
                source,
            )
        })?;
    if exact_directory(&opened.observation).map_err(model_error_at(&workspace_path))?
        != *store.snapshot().project().workspace().exact()
    {
        return Err(CodegenError::RecoveryRequired {
            journal_path: workspace_path,
            reason: "transaction workspace changed before an owner mutation".to_owned(),
        });
    }
    Ok((namespace, opened.directory, workspace_path))
}

fn sync_transaction_workspace(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &ImmutableJournalStore<'_>,
    mutation_path: &Path,
) -> Result<(), CodegenError> {
    let (namespace, workspace, workspace_path) =
        open_transaction_workspace(context, lock, store, mutation_path)?;
    let observation = store
        .runtime()
        .fs()
        .observe_directory(DirectoryEndpoint::new(
            &namespace,
            Path::new(store.snapshot().project().workspace().name()),
            &workspace,
            &workspace_path,
        ))
        .map_err(|source| {
            transaction_io(
                "observe transaction workspace",
                TRANSACTION_NAMESPACE_LOGICAL_PATH,
                &workspace_path,
                source,
            )
        })?;
    if exact_directory(&observation).map_err(model_error_at(&workspace_path))?
        != *store.snapshot().project().workspace().exact()
    {
        return Err(CodegenError::RecoveryRequired {
            journal_path: workspace_path,
            reason: "transaction workspace changed at its durability barrier".to_owned(),
        });
    }
    store
        .runtime()
        .fs()
        .sync_directory(&workspace, &workspace_path)
        .map_err(|source| {
            transaction_io(
                "sync transaction workspace",
                TRANSACTION_NAMESPACE_LOGICAL_PATH,
                &workspace_path,
                source,
            )
        })
}

fn ensure_same_preparation_filesystem(
    snapshot: &super::journal::JournalSnapshotV2,
    destination_parent: DirectoryParentV2,
    diagnostic_path: &Path,
) -> Result<(), CodegenError> {
    let owner_namespace = snapshot
        .project()
        .workspace()
        .exact()
        .identity()
        .namespace();
    let destination_namespace = model_parent_current(snapshot, destination_parent)?
        .identity()
        .namespace();
    if owner_namespace != destination_namespace {
        return Err(CodegenError::InvalidCoordinationState {
            path: diagnostic_path.display().to_string(),
            reason: "transaction owner workspace and destination parent are on different filesystems; owner-first placement cannot copy across devices"
                .to_owned(),
        });
    }
    Ok(())
}

fn preparation_parent_namespace(
    snapshot: &super::journal::JournalSnapshotV2,
    mut parent: DirectoryParentV2,
) -> Result<u128, CodegenError> {
    for _ in 0..=snapshot.directories().len() {
        match parent {
            DirectoryParentV2::ProjectRoot => {
                return Ok(snapshot.project().root_current().identity().namespace());
            }
            DirectoryParentV2::CoordinationParent => {
                return Ok(snapshot
                    .project()
                    .coordination_parent()
                    .identity()
                    .namespace());
            }
            DirectoryParentV2::TransactionNamespace => {
                return Ok(snapshot
                    .project()
                    .workspace_parent_current()
                    .identity()
                    .namespace());
            }
            DirectoryParentV2::TransactionWorkspace => {
                return Ok(snapshot
                    .project()
                    .workspace()
                    .exact()
                    .identity()
                    .namespace());
            }
            DirectoryParentV2::Cohort { ordinal } => {
                let directory = snapshot
                    .directories()
                    .get(ordinal.get() as usize)
                    .filter(|directory| directory.ordinal() == ordinal)
                    .ok_or_else(|| CodegenError::InvalidCoordinationState {
                        path: "transaction directory cohort".to_owned(),
                        reason: "preparation parent ordinal is outside the exact directory cohort"
                            .to_owned(),
                    })?;
                if let Some(current) = directory.current().as_present() {
                    return Ok(current.identity().namespace());
                }
                let parent_path = immediate_parent(directory.logical_path()).unwrap_or("");
                parent = directory_parent(snapshot, parent_path)?;
            }
        }
    }
    Err(CodegenError::InvalidCoordinationState {
        path: "transaction directory cohort".to_owned(),
        reason: "preparation parent ancestry is cyclic".to_owned(),
    })
}

fn preflight_preparation_filesystems(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &ImmutableJournalStore<'_>,
) -> Result<(), CodegenError> {
    let snapshot = store.snapshot();
    let owner_namespace = snapshot
        .project()
        .workspace()
        .exact()
        .identity()
        .namespace();
    for directory in snapshot.directories().iter().filter(|directory| {
        directory.disposition() == super::journal::DirectoryDispositionV2::Create
    }) {
        let parent_path = immediate_parent(directory.logical_path()).unwrap_or("");
        let parent = directory_parent(snapshot, parent_path)?;
        if preparation_parent_namespace(snapshot, parent)? != owner_namespace {
            return Err(CodegenError::InvalidCoordinationState {
                path: context
                    .project_root()
                    .join(directory.logical_path())
                    .display()
                    .to_string(),
                reason: "full-cohort owner-first preflight found a cross-filesystem directory destination"
                    .to_owned(),
            });
        }
    }
    for entry in snapshot.entries() {
        let parent_path = immediate_parent(entry.logical_path()).unwrap_or("");
        let parent = directory_parent(snapshot, parent_path)?;
        if preparation_parent_namespace(snapshot, parent)? != owner_namespace {
            return Err(CodegenError::InvalidCoordinationState {
                path: context
                    .project_root()
                    .join(entry.logical_path())
                    .display()
                    .to_string(),
                reason:
                    "full-cohort owner-first preflight found a cross-filesystem file destination"
                        .to_owned(),
            });
        }
    }

    let runtime = store.runtime();
    let probe_path = context
        .project_root()
        .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
        .join(snapshot.project().workspace().name());
    let (_namespace, workspace, workspace_path) =
        open_transaction_workspace(context, lock, store, &probe_path)?;
    runtime
        .fs()
        .probe_noreplace_relocation(&workspace, &workspace_path)
        .map_err(|source| CodegenError::InvalidCoordinationState {
            path: workspace_path.display().to_string(),
            reason: format!(
                "owner-first preparation requires an atomic no-replace relocation primitive: {}",
                source.into_io()
            ),
        })?;
    runtime
        .fs()
        .sync_directory(&workspace, &workspace_path)
        .map_err(|source| {
            transaction_io(
                "preflight transaction-workspace durability",
                TRANSACTION_NAMESPACE_LOGICAL_PATH,
                &workspace_path,
                source,
            )
        })?;
    let root = open_directory(context, "")?;
    runtime
        .fs()
        .probe_noreplace_relocation(&root, context.project_root())
        .map_err(|source| CodegenError::InvalidCoordinationState {
            path: context.project_root().display().to_string(),
            reason: format!(
                "owner-first destination filesystem lacks atomic no-replace relocation: {}",
                source.into_io()
            ),
        })?;
    runtime
        .fs()
        .sync_directory(&root, context.project_root())
        .map_err(|source| {
            transaction_io(
                "preflight destination-parent durability",
                "project root",
                context.project_root(),
                source,
            )
        })?;
    Ok(())
}

fn prepare_directories(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &mut ImmutableJournalStore<'_>,
) -> Result<(), CodegenError> {
    let directories = store.snapshot().directories().to_vec();
    for directory in directories.iter().filter(|directory| {
        directory.disposition() == super::journal::DirectoryDispositionV2::Create
    }) {
        let parent_path = immediate_parent(directory.logical_path()).unwrap_or("");
        let destination_parent = directory_parent(store.snapshot(), parent_path)?;
        let target_name = leaf_name(directory.logical_path());
        let target_path = context.project_root().join(directory.logical_path());
        ensure_same_preparation_filesystem(store.snapshot(), destination_parent, &target_path)?;

        let owner_name = directory
            .candidate_name()
            .expect("created directory has a deterministic workspace owner name")
            .to_owned();
        let workspace_path = context
            .project_root()
            .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
            .join(store.snapshot().project().workspace().name());
        let owner_path = workspace_path.join(&owner_name);
        let runtime = store.runtime().clone();
        let armed = store
            .snapshot()
            .arm_owner_creation(store.records().last().expect("record exists").clone())
            .map_err(model_error_at(&owner_path))?;
        store.publish_successor(armed)?;
        let (_namespace, workspace, _) =
            open_transaction_workspace(context, lock, store, &owner_path)?;
        runtime.observe(TransitionKey::OwnerPrepared {
            artifact: PreparationArtifactKind::Directory,
            ordinal: directory.ordinal().get(),
            window: TransitionWindow::Before,
        });
        let owner = runtime
            .fs()
            .create_directory_exact(&workspace, Path::new(&owner_name), &owner_path, 0o700)
            .map_err(|source| {
                transaction_io(
                    "create directory owner",
                    directory.logical_path(),
                    &owner_path,
                    source,
                )
            })?;
        runtime
            .fs()
            .set_directory_mode(
                &owner.directory,
                &owner_path,
                directory.planned_mode().posix_mode().unwrap_or(0o755),
            )
            .map_err(|source| {
                transaction_io(
                    "set directory owner final mode",
                    directory.logical_path(),
                    &owner_path,
                    source,
                )
            })?;
        runtime
            .fs()
            .sync_directory(&owner.directory, &owner_path)
            .map_err(|source| {
                transaction_io(
                    "sync directory owner inode",
                    directory.logical_path(),
                    &owner_path,
                    source,
                )
            })?;
        let owner_observation = runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &workspace,
                Path::new(&owner_name),
                &owner.directory,
                &owner_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "observe directory owner",
                    directory.logical_path(),
                    &owner_path,
                    source,
                )
            })?;
        let owner_inventory = runtime
            .fs()
            .inventory_directory_exact_bounded(
                DirectoryEndpoint::new(
                    &workspace,
                    Path::new(&owner_name),
                    &owner.directory,
                    &owner_path,
                ),
                &owner_observation,
                0,
            )
            .map_err(|source| {
                transaction_io(
                    "inventory directory owner",
                    directory.logical_path(),
                    &owner_path,
                    source,
                )
            })?;
        if !owner_inventory.entries.is_empty() {
            return Err(CodegenError::RecoveryRequired {
                journal_path: owner_path,
                reason: "new directory owner is not exactly empty".to_owned(),
            });
        }
        sync_transaction_workspace(context, lock, store, &owner_path)?;
        let (_namespace, final_workspace, _) =
            open_transaction_workspace(context, lock, store, &owner_path)?;
        let final_owner = runtime
            .fs()
            .open_directory_exact(
                &final_workspace,
                Path::new(&owner_name),
                &owner_path,
                directory.planned_mode().posix_mode().unwrap_or(0o755),
            )
            .map_err(|source| {
                transaction_io(
                    "rebind directory owner at completion boundary",
                    directory.logical_path(),
                    &owner_path,
                    source,
                )
            })?;
        if final_owner.observation != owner_observation {
            return third_state(directory.logical_path(), context);
        }
        runtime
            .fs()
            .inventory_directory_exact_bounded(
                DirectoryEndpoint::new(
                    &final_workspace,
                    Path::new(&owner_name),
                    &final_owner.directory,
                    &owner_path,
                ),
                &final_owner.observation,
                0,
            )
            .map_err(|source| {
                transaction_io(
                    "prove directory owner exactly empty at completion boundary",
                    directory.logical_path(),
                    &owner_path,
                    source,
                )
            })?;
        runtime.observe(TransitionKey::OwnerPrepared {
            artifact: PreparationArtifactKind::Directory,
            ordinal: directory.ordinal().get(),
            window: TransitionWindow::After,
        });
        let owner_exact =
            exact_directory(&final_owner.observation).map_err(model_error_at(&owner_path))?;
        let owner_successor = store
            .snapshot()
            .complete_owner_creation(
                store.records().last().expect("record exists").clone(),
                PreparationObservationV2::DirectoryCandidate {
                    exact: owner_exact.clone(),
                    parent_after: store.snapshot().project().workspace().exact().clone(),
                },
            )
            .map_err(model_error_at(&owner_path))?;
        store.publish_successor(owner_successor)?;

        let parent_before = model_parent_current(store.snapshot(), destination_parent)?.clone();
        let intent = DirectoryPublishIntentV2::new(
            directory.ordinal(),
            &owner_name,
            owner_exact.clone(),
            destination_parent,
            parent_before,
        );
        let armed = store
            .snapshot()
            .arm_directory_publication(
                store.records().last().expect("record exists").clone(),
                intent,
            )
            .map_err(model_error_at(&owner_path))?;
        store.publish_successor(armed)?;

        runtime.observe(TransitionKey::Placement {
            artifact: PreparationArtifactKind::Directory,
            ordinal: directory.ordinal().get(),
            window: TransitionWindow::Before,
        });
        let (_namespace, workspace, _) =
            open_transaction_workspace(context, lock, store, &owner_path)?;
        let parent = rebind_parent_for_mutation(
            context,
            lock,
            runtime.fs(),
            store.snapshot(),
            parent_path,
            &target_path,
        )?;
        if observe_directory_child(runtime.fs(), &parent, Path::new(target_name), &target_path)?
            .is_some()
        {
            return third_state(directory.logical_path(), context);
        }
        let owner = runtime
            .fs()
            .open_directory_exact(
                &workspace,
                Path::new(&owner_name),
                &owner_path,
                directory.planned_mode().posix_mode().unwrap_or(0o755),
            )
            .map_err(|source| {
                transaction_io(
                    "rebind directory owner",
                    directory.logical_path(),
                    &owner_path,
                    source,
                )
            })?;
        if exact_directory(&owner.observation).map_err(model_error_at(&owner_path))? != owner_exact
        {
            return third_state(directory.logical_path(), context);
        }
        runtime
            .fs()
            .inventory_directory_exact_bounded(
                DirectoryEndpoint::new(
                    &workspace,
                    Path::new(&owner_name),
                    &owner.directory,
                    &owner_path,
                ),
                &owner.observation,
                0,
            )
            .map_err(|source| {
                transaction_io(
                    "revalidate exact-empty directory owner",
                    directory.logical_path(),
                    &owner_path,
                    source,
                )
            })?;
        runtime
            .fs()
            .relocate_noreplace(
                &workspace,
                Path::new(&owner_name),
                &owner_path,
                &parent,
                Path::new(target_name),
                &target_path,
                &ExactRelocationSource::EmptyDirectory(owner.observation),
            )
            .map_err(|source| CodegenError::RecoveryRequired {
                journal_path: owner_path.clone(),
                reason: format!(
                    "directory owner placement requires exact recovery: {}",
                    source.into_io()
                ),
            })?;
        runtime
            .fs()
            .sync_directory(&owner.directory, &target_path)
            .map_err(|source| {
                transaction_io(
                    "sync placed directory inode",
                    directory.logical_path(),
                    &target_path,
                    source,
                )
            })?;
        let parent_after = observe_directory_path(context, runtime.fs(), parent_path)?;
        sync_directory_path(
            context,
            lock,
            runtime.fs(),
            store.snapshot(),
            parent_path,
            &parent_after,
            &target_path,
        )?;
        sync_transaction_workspace(context, lock, store, &owner_path)?;
        if observe_directory_child(
            runtime.fs(),
            &workspace,
            Path::new(&owner_name),
            &owner_path,
        )?
        .is_some()
        {
            return third_state(directory.logical_path(), context);
        }
        let placed =
            observe_directory_child(runtime.fs(), &parent, Path::new(target_name), &target_path)?
                .ok_or_else(|| recovery_missing(directory.logical_path(), context))?;
        runtime
            .fs()
            .inventory_directory_exact_bounded(
                DirectoryEndpoint::new(
                    &parent,
                    Path::new(target_name),
                    &owner.directory,
                    &target_path,
                ),
                &placed,
                0,
            )
            .map_err(|source| {
                transaction_io(
                    "verify placed directory remains exactly empty",
                    directory.logical_path(),
                    &target_path,
                    source,
                )
            })?;
        let placed_exact = exact_directory(&placed).map_err(model_error_at(&target_path))?;
        if placed_exact != owner_exact {
            return third_state(directory.logical_path(), context);
        }
        runtime.observe(TransitionKey::Placement {
            artifact: PreparationArtifactKind::Directory,
            ordinal: directory.ordinal().get(),
            window: TransitionWindow::After,
        });
        let completed = store
            .snapshot()
            .complete_directory_publication(
                store.records().last().expect("record exists").clone(),
                DirectoryPublicationObservationV2::new(
                    placed_exact,
                    PresenceV2::Missing,
                    exact_directory(&parent_after).map_err(model_error_at(&target_path))?,
                ),
            )
            .map_err(model_error_at(&target_path))?;
        store.publish_successor(completed)?;
    }
    Ok(())
}

fn observe_file_child_optional(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &Path,
    path: &Path,
    max_bytes: u64,
) -> Result<Option<ExactFileObservation>, CodegenError> {
    match fs.observe_regular_file_bounded(parent, name, path, max_bytes) {
        Ok(observation) => Ok(Some(observation)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(transaction_io(
            "observe exact transaction file",
            &name.to_string_lossy(),
            path,
            source,
        )),
    }
}

fn place_file_owner(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &mut ImmutableJournalStore<'_>,
    ordinal: ArtifactOrdinal,
    artifact_kind: FileArtifactKindV2,
    transition_kind: PreparationArtifactKind,
    logical_path: &str,
    owner_name: &str,
    placed_name: &str,
    expected_owner: ExactFileStateV2,
) -> Result<(ExactFileObservation, Dir), CodegenError> {
    let parent_path = immediate_parent(logical_path).unwrap_or("");
    let parent_binding = directory_parent(store.snapshot(), parent_path)?;
    let target_path = context.project_root().join(parent_path).join(placed_name);
    let workspace_path = context
        .project_root()
        .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
        .join(store.snapshot().project().workspace().name());
    let owner_path = workspace_path.join(owner_name);
    let intent = FilePlacementIntentV2::new(
        ordinal,
        artifact_kind,
        owner_name,
        placed_name,
        expected_owner.clone(),
        parent_binding,
        model_parent_current(store.snapshot(), parent_binding)?.clone(),
    );
    let armed = store
        .snapshot()
        .arm_file_placement(
            store.records().last().expect("record exists").clone(),
            intent,
        )
        .map_err(model_error_at(&owner_path))?;
    store.publish_successor(armed)?;

    let runtime = store.runtime().clone();
    runtime.observe(TransitionKey::Placement {
        artifact: transition_kind,
        ordinal: ordinal.get(),
        window: TransitionWindow::Before,
    });
    let (_namespace, workspace, _) = open_transaction_workspace(context, lock, store, &owner_path)?;
    let parent = rebind_parent_for_mutation(
        context,
        lock,
        runtime.fs(),
        store.snapshot(),
        parent_path,
        &target_path,
    )?;
    let owner = observe_file_child_optional(
        runtime.fs(),
        &workspace,
        Path::new(owner_name),
        &owner_path,
        expected_owner.state().byte_len(),
    )?
    .ok_or_else(|| recovery_missing(logical_path, context))?;
    if !file_matches(&owner, &expected_owner, Some(1))
        || observe_file_child_optional(
            runtime.fs(),
            &parent,
            Path::new(placed_name),
            &target_path,
            expected_owner.state().byte_len(),
        )?
        .is_some()
    {
        return third_state(logical_path, context);
    }
    runtime
        .fs()
        .relocate_noreplace(
            &workspace,
            Path::new(owner_name),
            &owner_path,
            &parent,
            Path::new(placed_name),
            &target_path,
            &ExactRelocationSource::File(owner.clone()),
        )
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: owner_path.clone(),
            reason: format!(
                "file owner placement requires exact recovery: {}",
                source.into_io()
            ),
        })?;
    let parent_after = observe_directory_path(context, runtime.fs(), parent_path)?;
    sync_directory_path(
        context,
        lock,
        runtime.fs(),
        store.snapshot(),
        parent_path,
        &parent_after,
        &target_path,
    )?;
    sync_transaction_workspace(context, lock, store, &owner_path)?;
    if observe_file_child_optional(
        runtime.fs(),
        &workspace,
        Path::new(owner_name),
        &owner_path,
        expected_owner.state().byte_len(),
    )?
    .is_some()
    {
        return third_state(logical_path, context);
    }
    let placed = observe_file_child_optional(
        runtime.fs(),
        &parent,
        Path::new(placed_name),
        &target_path,
        expected_owner.state().byte_len(),
    )?
    .ok_or_else(|| recovery_missing(logical_path, context))?;
    let placed_exact = exact_file(&placed).map_err(model_error_at(&target_path))?;
    if placed_exact != expected_owner {
        return third_state(logical_path, context);
    }
    runtime.observe(TransitionKey::Placement {
        artifact: transition_kind,
        ordinal: ordinal.get(),
        window: TransitionWindow::After,
    });
    let completed = store
        .snapshot()
        .complete_file_placement(
            store.records().last().expect("record exists").clone(),
            FilePlacementObservationV2::new(
                placed_exact,
                PresenceV2::Missing,
                exact_directory(&parent_after).map_err(model_error_at(&target_path))?,
            ),
        )
        .map_err(model_error_at(&target_path))?;
    store.publish_successor(completed)?;
    Ok((placed, parent))
}

fn prepare_files(
    context: &PlanningContext,
    lock: &WriteLock,
    snapshot: &PlanSnapshot,
    ordered: &[OrderedFile<'_>],
    store: &mut ImmutableJournalStore<'_>,
) -> Result<Vec<PreparedFile>, CodegenError> {
    let mut prepared = Vec::with_capacity(ordered.len());
    for ordered in ordered {
        let logical_path = ordered.file.path();
        let parent_path = immediate_parent(logical_path).unwrap_or("");
        let parent_binding = directory_parent(store.snapshot(), parent_path)?;
        let target_name = leaf_name(logical_path).to_owned();
        let target_path = context.project_root().join(logical_path);
        ensure_same_preparation_filesystem(store.snapshot(), parent_binding, &target_path)?;
        let entry = store.snapshot().entries()[ordered.ordinal.get() as usize].clone();
        let owner_name = entry.stage().owner_name().to_owned();
        let stage_name = entry.stage().name().to_owned();
        let workspace_path = context
            .project_root()
            .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
            .join(store.snapshot().project().workspace().name());
        let owner_path = workspace_path.join(&owner_name);
        let runtime = store.runtime().clone();
        let armed = store
            .snapshot()
            .arm_owner_creation(store.records().last().expect("record exists").clone())
            .map_err(model_error_at(&owner_path))?;
        store.publish_successor(armed)?;
        let (_namespace, workspace, _) =
            open_transaction_workspace(context, lock, store, &owner_path)?;
        runtime.observe(TransitionKey::OwnerPrepared {
            artifact: PreparationArtifactKind::Stage,
            ordinal: ordered.ordinal.get(),
            window: TransitionWindow::Before,
        });
        let owner = write_stage(
            runtime.fs(),
            &workspace,
            Path::new(&owner_name),
            &owner_path,
            ordered.file.content(),
            stage_owner_final_mode(snapshot.preimage(logical_path).expect("validated preimage")),
        )?;
        sync_transaction_workspace(context, lock, store, &owner_path)?;
        runtime.observe(TransitionKey::OwnerPrepared {
            artifact: PreparationArtifactKind::Stage,
            ordinal: ordered.ordinal.get(),
            window: TransitionWindow::After,
        });
        let owner_exact = exact_file(&owner).map_err(model_error_at(&owner_path))?;
        let successor = store
            .snapshot()
            .complete_owner_creation(
                store.records().last().expect("record exists").clone(),
                PreparationObservationV2::Stage {
                    exact: owner_exact.clone(),
                },
            )
            .map_err(model_error_at(&owner_path))?;
        store.publish_successor(successor)?;
        let (stage, stage_parent) = place_file_owner(
            context,
            lock,
            store,
            ordered.ordinal,
            FileArtifactKindV2::Stage,
            PreparationArtifactKind::Stage,
            logical_path,
            &owner_name,
            &stage_name,
            owner_exact,
        )?;
        let stage_parent_path = context.project_root().join(parent_path);
        let stage_path = stage_parent_path.join(&stage_name);
        prepared.push(PreparedFile {
            ordinal: ordered.ordinal,
            logical_path: logical_path.to_owned(),
            target_name,
            target_path,
            stage_parent,
            stage_parent_path,
            stage_name,
            stage_path,
            stage,
            backup_name: None,
            backup_path: None,
            backup: None,
        });
    }

    for prepared_file in &mut prepared {
        let entry = store.snapshot().entries()[prepared_file.ordinal.get() as usize].clone();
        let Some(backup_artifact) = entry.backup() else {
            continue;
        };
        let owner_name = backup_artifact.owner_name().to_owned();
        let backup_name = backup_artifact.name().to_owned();
        let backup_path = prepared_file
            .target_path
            .parent()
            .expect("target has a parent")
            .join(&backup_name);
        let owner_path = context
            .project_root()
            .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
            .join(store.snapshot().project().workspace().name())
            .join(&owner_name);
        let runtime = store.runtime().clone();
        let parent_path = immediate_parent(&prepared_file.logical_path).unwrap_or("");
        let source_parent = rebind_parent_for_mutation(
            context,
            lock,
            runtime.fs(),
            store.snapshot(),
            parent_path,
            &prepared_file.target_path,
        )?;
        let source = runtime
            .fs()
            .observe_regular_file_bounded(
                &source_parent,
                Path::new(&prepared_file.target_name),
                &prepared_file.target_path,
                entry_file_read_limit(&entry),
            )
            .map_err(|source| {
                transaction_io(
                    "inspect backup source",
                    &prepared_file.logical_path,
                    &prepared_file.target_path,
                    source,
                )
            })?;
        let expected_source = entry
            .current_target()
            .as_present()
            .ok_or_else(|| recovery_missing(&prepared_file.logical_path, context))?;
        if exact_file(&source).map_err(model_error_at(&prepared_file.target_path))?
            != *expected_source
        {
            return third_state(&prepared_file.logical_path, context);
        }
        let armed = store
            .snapshot()
            .arm_owner_creation(store.records().last().expect("record exists").clone())
            .map_err(model_error_at(&owner_path))?;
        store.publish_successor(armed)?;
        let (_namespace, workspace, _) =
            open_transaction_workspace(context, lock, store, &owner_path)?;
        runtime.observe(TransitionKey::OwnerPrepared {
            artifact: PreparationArtifactKind::Backup,
            ordinal: prepared_file.ordinal.get(),
            window: TransitionWindow::Before,
        });
        let copy = match runtime.fs().create_exclusive_copy(
            HardLinkEndpoint::new(
                &source_parent,
                Path::new(&prepared_file.target_name),
                &prepared_file.target_path,
            ),
            &source,
            HardLinkEndpoint::new(&workspace, Path::new(&owner_name), &owner_path),
        ) {
            ExclusiveFileCopyOutcome::CreatedVerified { copy } => copy,
            ExclusiveFileCopyOutcome::NotCreated { source } => {
                return Err(transaction_io(
                    "create backup owner",
                    &prepared_file.logical_path,
                    &owner_path,
                    source,
                ));
            }
            ExclusiveFileCopyOutcome::CreatedUnverified {
                mut created,
                source,
            } => {
                let residual = runtime.fs().observe_created_file_exact(
                    &workspace,
                    Path::new(&owner_name),
                    &owner_path,
                    &mut created,
                    expected_source.state().byte_len(),
                );
                return Err(CodegenError::RecoveryRequired {
                    journal_path: owner_path,
                    reason: match residual {
                        Ok(observation) => format!(
                            "backup-owner creation changed the namespace but did not complete with \
                             verified authority ({source}); recovery retained an exact live-owner \
                             observation: {observation:?}"
                        ),
                        Err(rebind_source) => format!(
                            "backup-owner creation changed the namespace but did not complete with \
                             verified authority ({source}); its live owner could not be rebound \
                             for exact recovery classification ({rebind_source})"
                        ),
                    },
                });
            }
        };
        if copy.source != source {
            return Err(CodegenError::RecoveryRequired {
                journal_path: owner_path,
                reason: "exclusive backup-owner copy returned a source observation that does not \
                         match the journal-certified preimage"
                    .to_owned(),
            });
        }
        runtime
            .fs()
            .sync_handle(&copy.file, &owner_path)
            .map_err(|source| {
                transaction_io(
                    "sync backup owner",
                    &prepared_file.logical_path,
                    &owner_path,
                    source,
                )
            })?;
        sync_transaction_workspace(context, lock, store, &owner_path)?;
        runtime.observe(TransitionKey::OwnerPrepared {
            artifact: PreparationArtifactKind::Backup,
            ordinal: prepared_file.ordinal.get(),
            window: TransitionWindow::After,
        });
        let owner_exact = exact_file(&copy.copy).map_err(model_error_at(&owner_path))?;
        let successor = store
            .snapshot()
            .complete_owner_creation(
                store.records().last().expect("record exists").clone(),
                PreparationObservationV2::Backup {
                    exact: owner_exact.clone(),
                },
            )
            .map_err(model_error_at(&owner_path))?;
        store.publish_successor(successor)?;
        let (backup, _backup_parent) = place_file_owner(
            context,
            lock,
            store,
            prepared_file.ordinal,
            FileArtifactKindV2::Backup,
            PreparationArtifactKind::Backup,
            &prepared_file.logical_path,
            &owner_name,
            &backup_name,
            owner_exact,
        )?;
        prepared_file.backup_name = Some(backup_name);
        prepared_file.backup_path = Some(backup_path);
        prepared_file.backup = Some(backup);
    }
    Ok(prepared)
}

fn salvage_exact_unpublished_stages(
    runtime: &TransactionRuntime,
    prepared: &[PreparedFile],
) -> Result<(), CodegenError> {
    for prepared_file in prepared.iter().rev() {
        let current = match runtime.fs().observe_regular_file_bounded(
            &prepared_file.stage_parent,
            Path::new(&prepared_file.stage_name),
            &prepared_file.stage_path,
            prepared_file.stage.byte_len,
        ) {
            Ok(current) => current,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(transaction_io(
                    "inspect retained stage authority",
                    &prepared_file.logical_path,
                    &prepared_file.stage_path,
                    source,
                ));
            }
        };
        if current != prepared_file.stage {
            continue;
        }

        runtime.observe(TransitionKey::CleanupObject {
            outcome: TransactionOutcome::Rollback,
            kind: CleanupObjectKind::PlacedStage,
            ordinal: prepared_file.ordinal.get(),
            window: TransitionWindow::Before,
        });
        let removal = runtime.fs().remove_file_exact(
            &prepared_file.stage_parent,
            Path::new(&prepared_file.stage_name),
            &prepared_file.stage_path,
            &current,
        );
        if let Err(error) = removal.as_ref()
            && !error.mutation_may_have_completed()
        {
            return Err(transaction_io(
                "remove retained exact stage",
                &prepared_file.logical_path,
                &prepared_file.stage_path,
                std::io::Error::other(error.to_string()),
            ));
        }
        runtime
            .fs()
            .sync_directory(
                &prepared_file.stage_parent,
                &prepared_file.stage_parent_path,
            )
            .map_err(|source| {
                transaction_io(
                    "sync retained stage parent",
                    &prepared_file.logical_path,
                    &prepared_file.stage_parent_path,
                    source,
                )
            })?;
        match prepared_file
            .stage_parent
            .symlink_metadata(Path::new(&prepared_file.stage_name))
        {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: prepared_file.stage_path.clone(),
                    reason: "retained exact stage remains after rollback cleanup".to_owned(),
                });
            }
            Err(source) => {
                return Err(transaction_io(
                    "prove retained stage absent",
                    &prepared_file.logical_path,
                    &prepared_file.stage_path,
                    source,
                ));
            }
        }
        runtime.observe(TransitionKey::CleanupObject {
            outcome: TransactionOutcome::Rollback,
            kind: CleanupObjectKind::PlacedStage,
            ordinal: prepared_file.ordinal.get(),
            window: TransitionWindow::After,
        });
    }
    Ok(())
}

fn commit_files(
    context: &PlanningContext,
    lock: &WriteLock,
    snapshot: &PlanSnapshot,
    prepared: &[PreparedFile],
    store: &mut ImmutableJournalStore,
) -> Result<(), CodegenError> {
    for prepared_file in prepared {
        let runtime = store.runtime().clone();
        runtime
            .fs()
            .before_final_revalidation(&prepared_file.target_path)
            .map_err(|source| {
                transaction_io(
                    "enter final target revalidation",
                    &prepared_file.logical_path,
                    &prepared_file.target_path,
                    source,
                )
            })?;
        snapshot.revalidate_path(context, &prepared_file.logical_path)?;
        // The second modeled race hook fires before another fresh snapshot
        // check. No hook is allowed after this final check and before the
        // capability-relative publication/replace syscall.
        runtime
            .fs()
            .after_final_revalidation(&prepared_file.target_path)
            .map_err(|source| {
                transaction_io(
                    "enter post-hook target revalidation",
                    &prepared_file.logical_path,
                    &prepared_file.target_path,
                    source,
                )
            })?;
        snapshot.revalidate_path(context, &prepared_file.logical_path)?;
        let entry = &store.snapshot().entries()[prepared_file.ordinal.get() as usize];
        let expected_stage =
            entry
                .stage()
                .current()
                .as_present()
                .ok_or_else(|| CodegenError::RecoveryRequired {
                    journal_path: prepared_file.stage_path.clone(),
                    reason: "commit has no durable exact stage authority".to_owned(),
                })?;
        if exact_file(&prepared_file.stage).map_err(model_error_at(&prepared_file.stage_path))?
            != *expected_stage
        {
            return third_state(&prepared_file.logical_path, context);
        }
        let parent_path = immediate_parent(&prepared_file.logical_path).unwrap_or("");
        let parent = rebind_parent_for_mutation(
            context,
            lock,
            runtime.fs(),
            store.snapshot(),
            parent_path,
            &prepared_file.target_path,
        )?;
        match entry.action() {
            EntryActionV2::Create => {
                runtime.observe(TransitionKey::ReplaceTarget {
                    ordinal: prepared_file.ordinal.get(),
                    window: TransitionWindow::Before,
                });
                runtime
                    .fs()
                    .publish_absent(
                        HardLinkEndpoint::new(
                            &parent,
                            Path::new(&prepared_file.stage_name),
                            &prepared_file.stage_path,
                        ),
                        &prepared_file.stage,
                        HardLinkEndpoint::new(
                            &parent,
                            Path::new(&prepared_file.target_name),
                            &prepared_file.target_path,
                        ),
                    )
                    .map_err(|source| {
                        transaction_io(
                            "publish absent target",
                            &prepared_file.logical_path,
                            &prepared_file.target_path,
                            source,
                        )
                    })?
            }
            EntryActionV2::Replace => {
                let target = runtime
                    .fs()
                    .observe_regular_file_bounded(
                        &parent,
                        Path::new(&prepared_file.target_name),
                        &prepared_file.target_path,
                        entry_file_read_limit(&entry),
                    )
                    .map_err(|source| {
                        transaction_io(
                            "inspect replacement target",
                            &prepared_file.logical_path,
                            &prepared_file.target_path,
                            source,
                        )
                    })?;
                let expected_target = entry.current_target().as_present().ok_or_else(|| {
                    CodegenError::RecoveryRequired {
                        journal_path: prepared_file.target_path.clone(),
                        reason: "replacement has no durable exact current-target authority"
                            .to_owned(),
                    }
                })?;
                if exact_file(&target).map_err(model_error_at(&prepared_file.target_path))?
                    != *expected_target
                {
                    return third_state(&prepared_file.logical_path, context);
                }
                runtime.observe(TransitionKey::ReplaceTarget {
                    ordinal: prepared_file.ordinal.get(),
                    window: TransitionWindow::Before,
                });
                runtime
                    .fs()
                    .replace_existing(
                        HardLinkEndpoint::new(
                            &parent,
                            Path::new(&prepared_file.stage_name),
                            &prepared_file.stage_path,
                        ),
                        &prepared_file.stage,
                        HardLinkEndpoint::new(
                            &parent,
                            Path::new(&prepared_file.target_name),
                            &prepared_file.target_path,
                        ),
                        &target,
                    )
                    .map_err(|source| {
                        transaction_io(
                            "replace target",
                            &prepared_file.logical_path,
                            &prepared_file.target_path,
                            source,
                        )
                    })?;
            }
        }
        let parent_state = observe_directory_path(context, runtime.fs(), parent_path)?;
        sync_directory_path(
            context,
            lock,
            runtime.fs(),
            store.snapshot(),
            parent_path,
            &parent_state,
            &prepared_file.target_path,
        )?;
        let verification_parent = rebind_parent_for_mutation(
            context,
            lock,
            runtime.fs(),
            store.snapshot(),
            parent_path,
            &prepared_file.target_path,
        )?;
        let target = runtime
            .fs()
            .observe_regular_file_bounded(
                &verification_parent,
                Path::new(&prepared_file.target_name),
                &prepared_file.target_path,
                entry_file_read_limit(entry),
            )
            .map_err(|source| {
                transaction_io(
                    "verify published target",
                    &prepared_file.logical_path,
                    &prepared_file.target_path,
                    source,
                )
            })?;
        let expected_target_links = match entry.action() {
            EntryActionV2::Create => 2,
            EntryActionV2::Replace => 1,
        };
        if !file_matches(&target, expected_stage, Some(expected_target_links)) {
            return third_state(&prepared_file.logical_path, context);
        }
        let stage = match entry.action() {
            EntryActionV2::Create => {
                let stage = runtime
                    .fs()
                    .observe_regular_file_bounded(
                        &verification_parent,
                        Path::new(&prepared_file.stage_name),
                        &prepared_file.stage_path,
                        entry_file_read_limit(entry),
                    )
                    .map_err(|source| {
                        transaction_io(
                            "verify stage",
                            &prepared_file.logical_path,
                            &prepared_file.stage_path,
                            source,
                        )
                    })?;
                if !file_matches(&stage, expected_stage, Some(2))
                    || stage.identity != target.identity
                    || stage.content_hash != target.content_hash
                    || stage.mode != target.mode
                {
                    return third_state(&prepared_file.logical_path, context);
                }
                PresenceV2::Present(
                    exact_file(&stage).map_err(model_error_at(&prepared_file.stage_path))?,
                )
            }
            EntryActionV2::Replace => {
                match verification_parent.symlink_metadata(Path::new(&prepared_file.stage_name)) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) => return third_state(&prepared_file.logical_path, context),
                    Err(source) => {
                        return Err(transaction_io(
                            "prove replaced stage absent",
                            &prepared_file.logical_path,
                            &prepared_file.stage_path,
                            source,
                        ));
                    }
                }
                PresenceV2::Missing
            }
        };
        runtime.observe(TransitionKey::ReplaceTarget {
            ordinal: prepared_file.ordinal.get(),
            window: TransitionWindow::After,
        });
        let successor = store
            .snapshot()
            .record_replacement_completion(
                store.records().last().expect("record exists").clone(),
                ReplacementObservationV2::new(
                    exact_file(&target).map_err(model_error_at(&prepared_file.target_path))?,
                    stage,
                ),
            )
            .map_err(model_error_at(&prepared_file.target_path))?;
        store.publish_successor(successor)?;
    }

    let commit = store
        .snapshot()
        .enter_commit_complete(store.records().last().expect("record exists").clone())
        .map_err(model_error_at(context.project_root()))?;
    store.publish_successor(commit)?;
    cleanup_commit(context, lock, prepared, store)
}

fn cleanup_commit(
    context: &PlanningContext,
    lock: &WriteLock,
    prepared: &[PreparedFile],
    store: &mut ImmutableJournalStore,
) -> Result<(), CodegenError> {
    let by_ordinal = prepared
        .iter()
        .map(|entry| (entry.ordinal.get(), entry))
        .collect::<BTreeMap<_, _>>();
    while !store.snapshot().cleanup_is_complete() {
        let completed = match store.snapshot().phase() {
            super::journal::JournalPhaseV2::CommitComplete {
                cleanup_completed, ..
            } => *cleanup_completed as usize,
            _ => unreachable!("commit cleanup has commit phase"),
        };
        let target = store.snapshot().cleanup_plans().commit()[completed];
        match target {
            super::journal::CleanupTargetV2::PlacedStage { ordinal }
            | super::journal::CleanupTargetV2::PlacedBackup { ordinal } => {
                let entry = by_ordinal[&ordinal.get()];
                let (name, path, expected, kind) = match target {
                    super::journal::CleanupTargetV2::PlacedStage { .. } => (
                        &entry.stage_name,
                        &entry.stage_path,
                        store.snapshot().entries()[ordinal.get() as usize]
                            .stage()
                            .current()
                            .as_present()
                            .cloned(),
                        CleanupObjectKind::PlacedStage,
                    ),
                    super::journal::CleanupTargetV2::PlacedBackup { .. } => (
                        entry.backup_name.as_ref().expect("backup name"),
                        entry.backup_path.as_ref().expect("backup path"),
                        store.snapshot().entries()[ordinal.get() as usize]
                            .backup()
                            .expect("backup model")
                            .current()
                            .as_present()
                            .cloned(),
                        CleanupObjectKind::PlacedBackup,
                    ),
                    _ => unreachable!(),
                };
                let Some(expected) = expected else {
                    let next = store
                        .snapshot()
                        .advance_cleanup_noop(
                            store.records().last().expect("record exists").clone(),
                        )
                        .map_err(model_error_at(path))?;
                    store.publish_successor(next)?;
                    continue;
                };
                let intent = super::journal::CleanupIntentV2::RemoveFile {
                    target,
                    expected: expected.clone(),
                };
                let armed = store
                    .snapshot()
                    .arm_cleanup(
                        store.records().last().expect("record exists").clone(),
                        intent,
                    )
                    .map_err(model_error_at(path))?;
                store.publish_successor(armed)?;
                let runtime = store.runtime().clone();
                runtime.observe(TransitionKey::CleanupObject {
                    outcome: TransactionOutcome::Commit,
                    kind,
                    ordinal: ordinal.get(),
                    window: TransitionWindow::Before,
                });
                let parent_path = immediate_parent(&entry.logical_path).unwrap_or("");
                let parent = rebind_parent_for_mutation(
                    context,
                    lock,
                    runtime.fs(),
                    store.snapshot(),
                    parent_path,
                    path,
                )?;
                let observed = runtime
                    .fs()
                    .observe_regular_file_bounded(
                        &parent,
                        Path::new(name),
                        path,
                        expected.state().byte_len(),
                    )
                    .map_err(|source| {
                        transaction_io("verify cleanup file", &entry.logical_path, path, source)
                    })?;
                if exact_file(&observed).map_err(model_error_at(path))? != expected {
                    return third_state(&entry.logical_path, context);
                }
                let removal =
                    runtime
                        .fs()
                        .remove_file_exact(&parent, Path::new(name), path, &observed);
                if let Err(error) = removal.as_ref()
                    && !error.mutation_may_have_completed()
                {
                    return Err(transaction_io(
                        "remove cleanup file",
                        &entry.logical_path,
                        path,
                        std::io::Error::other(error.to_string()),
                    ));
                }
                let parent_path = immediate_parent(&entry.logical_path).unwrap_or("");
                let parent_state = observe_directory_path(context, runtime.fs(), parent_path)?;
                if let Err(sync_error) = sync_directory_path(
                    context,
                    lock,
                    runtime.fs(),
                    store.snapshot(),
                    parent_path,
                    &parent_state,
                    path,
                ) {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: path.clone(),
                        reason: match removal.as_ref().err() {
                            Some(remove_error) => format!(
                                "cleanup unlink may have completed ({remove_error}); its parent durability barrier also failed: {sync_error}"
                            ),
                            None => format!(
                                "cleanup unlink completed, but its parent durability barrier failed: {sync_error}"
                            ),
                        },
                    });
                }
                let parent = rebind_parent_for_mutation(
                    context,
                    lock,
                    runtime.fs(),
                    store.snapshot(),
                    parent_path,
                    path,
                )?;
                match parent.symlink_metadata(Path::new(name)) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) => return third_state(&entry.logical_path, context),
                    Err(source) => {
                        return Err(transaction_io(
                            "prove cleanup file absent",
                            &entry.logical_path,
                            path,
                            source,
                        ));
                    }
                }
                runtime.observe(TransitionKey::CleanupObject {
                    outcome: TransactionOutcome::Commit,
                    kind,
                    ordinal: ordinal.get(),
                    window: TransitionWindow::After,
                });
                let next = store
                    .snapshot()
                    .complete_cleanup(store.records().last().expect("record exists").clone(), None)
                    .map_err(model_error_at(path))?;
                store.publish_successor(next)?;
            }
            super::journal::CleanupTargetV2::OwnedStage { .. }
            | super::journal::CleanupTargetV2::OwnedBackup { .. }
            | super::journal::CleanupTargetV2::OwnedDirectory { .. }
            | super::journal::CleanupTargetV2::CreatedDirectory { .. } => {
                let next = store
                    .snapshot()
                    .advance_cleanup_noop(store.records().last().expect("record exists").clone())
                    .map_err(model_error_at(context.project_root()))?;
                store.publish_successor(next)?;
            }
        }
    }
    store.finalize(TransactionOutcome::Commit)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BoundedRecoveryStep {
    Advanced,
    BarrierCertified(RecoveryBarrierCertificate),
    ReadyForFinalization(TransactionOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecoveryBarrierCertificate {
    transaction_id: TransactionId,
    latest_record: RecordBindingV2,
    slot: RecoveryBarrierSlot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecoveryBarrierSlot {
    PendingOwnerCreation {
        intent: super::journal::OwnerCreationIntentV2,
        residual: Option<OwnedResidualDeleteBindingV2>,
    },
    OwnerDiscard {
        binding: OwnedResidualDeleteBindingV2,
    },
    Placement {
        intent: PreparationPlacementIntentV2,
        artifact: RecoveryPreparationArtifactV2,
    },
    ForwardReplacement {
        ordinal: ArtifactOrdinal,
        action: EntryActionV2,
    },
    Rollback {
        intent: RollbackIntentV2,
    },
    Cleanup {
        outcome: TransactionOutcome,
        intent: CleanupIntentV2,
    },
    ExactSnapshot {
        phase: JournalPhaseV2,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecoveryAdoptionPlan {
    Successor { slot: RecoveryBarrierSlot },
    CombinedOperationBarrier { slot: RecoveryBarrierSlot },
}

impl RecoveryAdoptionPlan {
    const fn slot(&self) -> &RecoveryBarrierSlot {
        match self {
            Self::Successor { slot } | Self::CombinedOperationBarrier { slot } => slot,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryAdoptionPlanKind {
    Successor,
    CombinedOperationBarrier,
}

const fn recovery_adoption_plan_kind(preflight: &RecoveryPreflightV2) -> RecoveryAdoptionPlanKind {
    match preflight {
        RecoveryPreflightV2::PendingOwnerCreation { .. }
        | RecoveryPreflightV2::PendingPlacement {
            world: MutationWorldV2::Before,
            ..
        }
        | RecoveryPreflightV2::ExactSnapshot => RecoveryAdoptionPlanKind::Successor,
        RecoveryPreflightV2::PendingOwnerDiscard { .. }
        | RecoveryPreflightV2::PendingPlacement {
            world: MutationWorldV2::After,
            ..
        }
        | RecoveryPreflightV2::ForwardReplacementCompleted { .. }
        | RecoveryPreflightV2::PendingRollback { .. }
        | RecoveryPreflightV2::PendingCleanup { .. } => {
            RecoveryAdoptionPlanKind::CombinedOperationBarrier
        }
    }
}

/// Derives the one engine action that both same-pass journal adoption and any
/// later after-world barrier certificate must bind. Before and after worlds of
/// the same in-flight mutation intentionally map to the same slot; the world
/// itself is revalidated separately before successor authorization.
pub(super) fn derive_recovery_barrier_slot(
    loaded: &LoadedJournal,
    preflight: &RecoveryPreflightV2,
    context: &PlanningContext,
) -> Result<RecoveryBarrierSlot, CodegenError> {
    let latest = loaded
        .latest()
        .ok_or_else(|| CodegenError::RecoveryRequired {
            journal_path: recovery_journal_path(context),
            reason: "recovery-slot derivation has no latest immutable journal snapshot".to_owned(),
        })?;
    match preflight {
        RecoveryPreflightV2::PendingOwnerCreation { residual } => {
            let JournalPhaseV2::Preparing {
                pending: Some(PreparationPendingIntentV2::CreateOwner(intent)),
                ..
            } = latest.phase()
            else {
                return recovery_preflight_mismatch(
                    context,
                    "owner-creation preflight does not match the durable phase",
                );
            };
            Ok(RecoveryBarrierSlot::PendingOwnerCreation {
                intent: intent.clone(),
                residual: residual.clone(),
            })
        }
        RecoveryPreflightV2::PendingOwnerDiscard { .. } => {
            let JournalPhaseV2::Preparing {
                pending: Some(PreparationPendingIntentV2::DiscardOwner(binding)),
                ..
            } = latest.phase()
            else {
                return recovery_preflight_mismatch(
                    context,
                    "owner-discard preflight does not match the durable phase",
                );
            };
            Ok(RecoveryBarrierSlot::OwnerDiscard {
                binding: binding.clone(),
            })
        }
        RecoveryPreflightV2::PendingPlacement {
            ordinal, artifact, ..
        } => {
            let JournalPhaseV2::Preparing {
                pending: Some(PreparationPendingIntentV2::PlaceOwner(intent)),
                ..
            } = latest.phase()
            else {
                return recovery_preflight_mismatch(
                    context,
                    "placement preflight does not match the durable phase",
                );
            };
            if intent.ordinal() != *ordinal {
                return recovery_preflight_mismatch(
                    context,
                    "placement preflight does not match the durable intent",
                );
            }
            Ok(RecoveryBarrierSlot::Placement {
                intent: intent.clone(),
                artifact: *artifact,
            })
        }
        RecoveryPreflightV2::ForwardReplacementCompleted { ordinal, .. } => {
            let next = match latest.phase() {
                JournalPhaseV2::Prepared => 0,
                JournalPhaseV2::Replacing { committed } => *committed as usize,
                _ => {
                    return recovery_preflight_mismatch(
                        context,
                        "forward-replacement preflight does not match the durable phase",
                    );
                }
            };
            let entry =
                latest
                    .entries()
                    .get(next)
                    .ok_or_else(|| CodegenError::RecoveryRequired {
                        journal_path: recovery_journal_path(context),
                        reason: "forward-replacement preflight exceeds the immutable entry cohort"
                            .to_owned(),
                    })?;
            if entry.ordinal() != *ordinal {
                return recovery_preflight_mismatch(
                    context,
                    "forward-replacement preflight has the wrong deterministic ordinal",
                );
            }
            Ok(RecoveryBarrierSlot::ForwardReplacement {
                ordinal: *ordinal,
                action: entry.action(),
            })
        }
        RecoveryPreflightV2::PendingRollback { ordinal, .. } => {
            let JournalPhaseV2::RollingBack {
                pending: Some(intent),
                ..
            } = latest.phase()
            else {
                return recovery_preflight_mismatch(
                    context,
                    "pending-rollback preflight does not match the durable phase",
                );
            };
            if intent.ordinal() != *ordinal {
                return recovery_preflight_mismatch(
                    context,
                    "pending-rollback preflight has the wrong deterministic ordinal",
                );
            }
            Ok(RecoveryBarrierSlot::Rollback {
                intent: intent.clone(),
            })
        }
        RecoveryPreflightV2::PendingCleanup { target, .. } => {
            let (outcome, pending) = match latest.phase() {
                JournalPhaseV2::RollbackComplete { pending, .. } => {
                    (TransactionOutcome::Rollback, pending)
                }
                JournalPhaseV2::CommitComplete { pending, .. } => {
                    (TransactionOutcome::Commit, pending)
                }
                _ => {
                    return recovery_preflight_mismatch(
                        context,
                        "pending-cleanup preflight does not match a terminal durable phase",
                    );
                }
            };
            let intent = pending
                .as_ref()
                .ok_or_else(|| CodegenError::RecoveryRequired {
                    journal_path: recovery_journal_path(context),
                    reason: "pending-cleanup preflight has no durable cleanup intent".to_owned(),
                })?;
            if intent.target() != *target {
                return recovery_preflight_mismatch(
                    context,
                    "pending-cleanup preflight has the wrong deterministic target",
                );
            }
            Ok(RecoveryBarrierSlot::Cleanup {
                outcome,
                intent: intent.clone(),
            })
        }
        RecoveryPreflightV2::ExactSnapshot => Ok(RecoveryBarrierSlot::ExactSnapshot {
            phase: latest.phase().clone(),
        }),
    }
}

pub(super) fn derive_recovery_adoption_plan(
    loaded: &LoadedJournal,
    preflight: &RecoveryPreflightV2,
    context: &PlanningContext,
) -> Result<RecoveryAdoptionPlan, CodegenError> {
    let slot = derive_recovery_barrier_slot(loaded, preflight, context)?;
    Ok(match recovery_adoption_plan_kind(preflight) {
        RecoveryAdoptionPlanKind::Successor => RecoveryAdoptionPlan::Successor { slot },
        RecoveryAdoptionPlanKind::CombinedOperationBarrier => {
            RecoveryAdoptionPlan::CombinedOperationBarrier { slot }
        }
    })
}

fn recovery_barrier_certificate(
    loaded: &LoadedJournal,
    slot: RecoveryBarrierSlot,
    context: &PlanningContext,
) -> Result<RecoveryBarrierCertificate, CodegenError> {
    let latest = loaded
        .latest()
        .ok_or_else(|| CodegenError::RecoveryRequired {
            journal_path: recovery_journal_path(context),
            reason: "barrier certification has no latest immutable journal snapshot".to_owned(),
        })?;
    let latest_record = loaded
        .records()
        .last()
        .ok_or_else(|| CodegenError::RecoveryRequired {
            journal_path: recovery_journal_path(context),
            reason: "barrier certification has no latest immutable record binding".to_owned(),
        })?;
    latest
        .validate_record_binding(latest_record)
        .map_err(model_error_at(context.project_root()))?;
    Ok(RecoveryBarrierCertificate {
        transaction_id: latest.transaction_id().clone(),
        latest_record: latest_record.clone(),
        slot,
    })
}

fn authorize_recovery_barrier(
    supplied: Option<&RecoveryBarrierCertificate>,
    expected: &RecoveryBarrierCertificate,
    after_world: bool,
    context: &PlanningContext,
) -> Result<bool, CodegenError> {
    let Some(supplied) = supplied else {
        return Ok(false);
    };
    if supplied != expected {
        return Err(CodegenError::RecoveryRequired {
            journal_path: recovery_journal_path(context),
            reason: "ephemeral recovery barrier certificate does not match the exact transaction, latest immutable record, and pending slot after rediscovery"
                .to_owned(),
        });
    }
    if !after_world {
        return Err(CodegenError::RecoveryRequired {
            journal_path: recovery_journal_path(context),
            reason: "matching ephemeral recovery barrier certificate rediscovered a non-after filesystem world"
                .to_owned(),
        });
    }
    Ok(true)
}

fn reject_unexpected_recovery_barrier(
    supplied: Option<&RecoveryBarrierCertificate>,
    context: &PlanningContext,
    step: &str,
) -> Result<(), CodegenError> {
    if supplied.is_some() {
        return Err(CodegenError::RecoveryRequired {
            journal_path: recovery_journal_path(context),
            reason: format!(
                "ephemeral recovery barrier certificate cannot authorize the rediscovered {step} step"
            ),
        });
    }
    Ok(())
}

/// Applies exactly one policy-approved recovery step.
///
/// The caller owns rediscovery, stable double capture, and global preflight.
/// This boundary revalidates the lock, resumes the exact immutable lineage,
/// and then performs at most one journal successor publication or one armed
/// filesystem durability transition. A fresh rediscovery plus the matching
/// volatile durability proof is required before a later invocation may
/// publish that mutation's completion successor. This function never enters
/// the unbounded normal-execution rollback or cleanup loops.
pub(super) fn recover_loaded_transaction_step(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: TransactionRuntime,
    loaded: &LoadedJournal,
    preflight: RecoveryPreflightV2,
    adoption_authority: Option<ActiveRecoveryAdoptionAuthority<RecoveryAdoptionPlan>>,
    barrier_certificate: Option<&RecoveryBarrierCertificate>,
) -> Result<BoundedRecoveryStep, CodegenError> {
    let plan = derive_recovery_adoption_plan(loaded, &preflight, context)?;
    let latest = loaded
        .latest()
        .ok_or_else(|| CodegenError::RecoveryRequired {
            journal_path: recovery_journal_path(context),
            reason: "recovery adoption has no latest immutable journal snapshot".to_owned(),
        })?;
    let latest_record = loaded
        .records()
        .last()
        .ok_or_else(|| CodegenError::RecoveryRequired {
            journal_path: recovery_journal_path(context),
            reason: "recovery adoption has no latest immutable record binding".to_owned(),
        })?;
    match (&adoption_authority, barrier_certificate) {
        (Some(authority), None)
            if authority.authorizes(latest.transaction_id(), latest_record, &plan) => {}
        (None, Some(_)) => {}
        (Some(_), None) => {
            return Err(CodegenError::RecoveryRequired {
                journal_path: recovery_journal_path(context),
                reason: "single-use recovery adoption authority does not bind the exact transaction, latest record, and recovery plan"
                    .to_owned(),
            });
        }
        (Some(_), Some(_)) => {
            return Err(CodegenError::RecoveryRequired {
                journal_path: recovery_journal_path(context),
                reason: "recovery cannot combine a same-pass adoption authority with an outer barrier certificate"
                    .to_owned(),
            });
        }
        (None, None) => {
            return Err(CodegenError::RecoveryRequired {
                journal_path: recovery_journal_path(context),
                reason: "recovery requires either same-pass adoption authority or one exact outer barrier certificate"
                    .to_owned(),
            });
        }
    }

    let used_adoption_authority = adoption_authority.is_some();
    let step = recover_loaded_transaction_step_inner(
        context,
        lock,
        runtime,
        loaded,
        preflight,
        plan.slot().clone(),
        barrier_certificate,
    )?;
    if used_adoption_authority {
        match (&plan, &step) {
            (
                RecoveryAdoptionPlan::CombinedOperationBarrier { .. },
                BoundedRecoveryStep::BarrierCertified(_),
            )
            | (
                RecoveryAdoptionPlan::Successor { .. },
                BoundedRecoveryStep::Advanced | BoundedRecoveryStep::ReadyForFinalization(_),
            ) => {}
            (RecoveryAdoptionPlan::CombinedOperationBarrier { .. }, _) => {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: recovery_journal_path(context),
                    reason: "combined journal-and-operation adoption did not issue its sole exact recovery barrier certificate"
                        .to_owned(),
                });
            }
            (RecoveryAdoptionPlan::Successor { .. }, BoundedRecoveryStep::BarrierCertified(_)) => {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: recovery_journal_path(context),
                    reason: "journal-successor recovery unexpectedly attempted to issue an operation barrier certificate"
                        .to_owned(),
                });
            }
        }
    } else if matches!(step, BoundedRecoveryStep::BarrierCertified(_)) {
        return Err(CodegenError::RecoveryRequired {
            journal_path: recovery_journal_path(context),
            reason:
                "an outer recovery barrier certificate attempted to produce another certificate"
                    .to_owned(),
        });
    }
    Ok(step)
}

fn recover_loaded_transaction_step_inner(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: TransactionRuntime,
    loaded: &LoadedJournal,
    preflight: RecoveryPreflightV2,
    recovery_slot: RecoveryBarrierSlot,
    barrier_certificate: Option<&RecoveryBarrierCertificate>,
) -> Result<BoundedRecoveryStep, CodegenError> {
    let latest = loaded
        .latest()
        .ok_or_else(|| CodegenError::RecoveryRequired {
            journal_path: context.project_root().join("src/components/ui/_kit"),
            reason: "transaction bootstrap has no durable sequence-zero journal record".to_owned(),
        })?;
    if let Some(certificate) = barrier_certificate {
        let latest_record = loaded.records().last().ok_or_else(|| {
            CodegenError::RecoveryRequired {
                journal_path: recovery_journal_path(context),
                reason: "ephemeral recovery barrier certificate rediscovered a lineage with no latest immutable record"
                    .to_owned(),
            }
        })?;
        if certificate.transaction_id != *latest.transaction_id()
            || certificate.latest_record != *latest_record
        {
            return Err(CodegenError::RecoveryRequired {
                journal_path: recovery_journal_path(context),
                reason: "ephemeral recovery barrier certificate rediscovered a substituted transaction or latest immutable record"
                    .to_owned(),
            });
        }
    }
    if loaded.partial().is_some() {
        return Err(CodegenError::RecoveryRequired {
            journal_path: context.project_root().join("src/components/ui/_kit"),
            reason: "a complete journal partial must be published or reconciled before recovery"
                .to_owned(),
        });
    }
    let mut store = ImmutableJournalStore::resume(
        context,
        lock,
        runtime,
        latest.clone(),
        loaded.records().to_vec(),
    )?;
    lock.validate_context(context)?;

    match preflight {
        RecoveryPreflightV2::PendingOwnerCreation { residual } => {
            reject_unexpected_recovery_barrier(
                barrier_certificate,
                context,
                "pending-owner-creation",
            )?;
            let successor = match residual {
                Some(binding) => store
                    .snapshot()
                    .arm_owner_discard(
                        store.records().last().expect("record exists").clone(),
                        binding,
                    )
                    .map_err(model_error_at(context.project_root()))?,
                None => store
                    .snapshot()
                    .cancel_owner_creation(store.records().last().expect("record exists").clone())
                    .map_err(model_error_at(context.project_root()))?,
            };
            store.publish_successor(successor)?;
            Ok(BoundedRecoveryStep::Advanced)
        }
        RecoveryPreflightV2::PendingOwnerDiscard { world } => {
            let JournalPhaseV2::Preparing {
                pending: Some(PreparationPendingIntentV2::DiscardOwner(binding)),
                ..
            } = store.snapshot().phase().clone()
            else {
                return recovery_preflight_mismatch(
                    context,
                    "owner-discard preflight does not match the durable phase",
                );
            };
            let certificate = recovery_barrier_certificate(loaded, recovery_slot.clone(), context)?;
            if authorize_recovery_barrier(
                barrier_certificate,
                &certificate,
                world == MutationWorldV2::After,
                context,
            )? {
                let successor = store
                    .snapshot()
                    .complete_owner_discard(store.records().last().expect("record exists").clone())
                    .map_err(model_error_at(context.project_root()))?;
                store.publish_successor(successor)?;
                return Ok(BoundedRecoveryStep::Advanced);
            }
            if world == MutationWorldV2::Before {
                discard_owned_residual(context, lock, &store, &binding)?;
            } else {
                let owner_path = context
                    .project_root()
                    .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
                    .join(store.snapshot().project().workspace().name())
                    .join(binding.owner().owner_name());
                sync_transaction_workspace(context, lock, &store, &owner_path)?;
                require_discarded_owner_absent(context, lock, &store, &binding)?;
                let artifact = match binding.owner().artifact() {
                    OwnerArtifactKindV2::Directory => PreparationArtifactKind::Directory,
                    OwnerArtifactKindV2::Stage => PreparationArtifactKind::Stage,
                    OwnerArtifactKindV2::Backup => PreparationArtifactKind::Backup,
                };
                store.runtime().observe(TransitionKey::DiscardOwner {
                    artifact,
                    ordinal: binding.owner().ordinal().get(),
                    window: TransitionWindow::After,
                });
            }
            Ok(BoundedRecoveryStep::BarrierCertified(certificate))
        }
        RecoveryPreflightV2::PendingPlacement {
            ordinal,
            artifact,
            world,
        } => {
            let JournalPhaseV2::Preparing {
                pending: Some(PreparationPendingIntentV2::PlaceOwner(intent)),
                ..
            } = store.snapshot().phase().clone()
            else {
                return recovery_preflight_mismatch(
                    context,
                    "placement preflight does not match the durable phase",
                );
            };
            if intent.ordinal() != ordinal {
                return recovery_preflight_mismatch(
                    context,
                    "placement preflight does not match the durable intent",
                );
            }
            let certificate = recovery_barrier_certificate(loaded, recovery_slot.clone(), context)?;
            let completion_authorized = authorize_recovery_barrier(
                barrier_certificate,
                &certificate,
                world == MutationWorldV2::After,
                context,
            )?;
            reconcile_pending_placement(
                context,
                lock,
                &mut store,
                intent,
                artifact,
                world,
                completion_authorized,
            )?;
            if world == MutationWorldV2::Before || completion_authorized {
                Ok(BoundedRecoveryStep::Advanced)
            } else {
                Ok(BoundedRecoveryStep::BarrierCertified(certificate))
            }
        }
        RecoveryPreflightV2::ForwardReplacementCompleted { ordinal, world } => {
            let next = match store.snapshot().phase() {
                JournalPhaseV2::Prepared => 0,
                JournalPhaseV2::Replacing { committed } => *committed as usize,
                _ => {
                    return recovery_preflight_mismatch(
                        context,
                        "forward-replacement preflight does not match the durable phase",
                    );
                }
            };
            if store
                .snapshot()
                .entries()
                .get(next)
                .map(|entry| entry.ordinal())
                != Some(ordinal)
            {
                return recovery_preflight_mismatch(
                    context,
                    "forward-replacement preflight has the wrong deterministic ordinal",
                );
            }
            let entry = store.snapshot().entries()[next].clone();
            let certificate = recovery_barrier_certificate(loaded, recovery_slot.clone(), context)?;
            if authorize_recovery_barrier(
                barrier_certificate,
                &certificate,
                world == MutationWorldV2::After,
                context,
            )? {
                reconcile_unrecorded_replacement(context, lock, &mut store)?;
                Ok(BoundedRecoveryStep::Advanced)
            } else {
                certify_unrecorded_replacement_durable(context, lock, &store, &entry)?;
                Ok(BoundedRecoveryStep::BarrierCertified(certificate))
            }
        }
        RecoveryPreflightV2::PendingRollback { ordinal, world } => {
            let JournalPhaseV2::RollingBack {
                pending: Some(intent),
                ..
            } = store.snapshot().phase().clone()
            else {
                return recovery_preflight_mismatch(
                    context,
                    "pending-rollback preflight does not match the durable phase",
                );
            };
            if intent.ordinal() != ordinal {
                return recovery_preflight_mismatch(
                    context,
                    "pending-rollback preflight has the wrong deterministic ordinal",
                );
            }
            let certificate = recovery_barrier_certificate(loaded, recovery_slot.clone(), context)?;
            if authorize_recovery_barrier(
                barrier_certificate,
                &certificate,
                world == MutationWorldV2::After,
                context,
            )? {
                let entry = &store.snapshot().entries()[ordinal.get() as usize];
                let successor = store
                    .snapshot()
                    .complete_rollback(store.records().last().expect("record exists").clone())
                    .map_err(model_error_at(entry.logical_path()))?;
                store.publish_successor(successor)?;
                return Ok(BoundedRecoveryStep::Advanced);
            }
            complete_pending_rollback(context, lock, &store, intent)?;
            Ok(BoundedRecoveryStep::BarrierCertified(certificate))
        }
        RecoveryPreflightV2::PendingCleanup { target, world } => {
            let (outcome, pending) = match store.snapshot().phase() {
                JournalPhaseV2::RollbackComplete { pending, .. } => {
                    (TransactionOutcome::Rollback, pending.clone())
                }
                JournalPhaseV2::CommitComplete { pending, .. } => {
                    (TransactionOutcome::Commit, pending.clone())
                }
                _ => {
                    return recovery_preflight_mismatch(
                        context,
                        "pending-cleanup preflight does not match a terminal durable phase",
                    );
                }
            };
            let intent = pending.ok_or_else(|| CodegenError::RecoveryRequired {
                journal_path: recovery_journal_path(context),
                reason: "pending-cleanup preflight has no durable cleanup intent".to_owned(),
            })?;
            if intent.target() != target {
                return recovery_preflight_mismatch(
                    context,
                    "pending-cleanup preflight has the wrong deterministic target",
                );
            }
            let certificate = recovery_barrier_certificate(loaded, recovery_slot.clone(), context)?;
            let completion_authorized = authorize_recovery_barrier(
                barrier_certificate,
                &certificate,
                world == MutationWorldV2::After,
                context,
            )?;
            let parent_after = execute_cleanup_intent(
                context,
                lock,
                &store,
                outcome,
                &intent,
                !completion_authorized,
            )?;
            if !completion_authorized {
                return Ok(BoundedRecoveryStep::BarrierCertified(certificate));
            }
            let successor = store
                .snapshot()
                .complete_cleanup(
                    store.records().last().expect("record exists").clone(),
                    parent_after,
                )
                .map_err(model_error_at(context.project_root()))?;
            store.publish_successor(successor)?;
            Ok(BoundedRecoveryStep::Advanced)
        }
        RecoveryPreflightV2::ExactSnapshot => {
            reject_unexpected_recovery_barrier(barrier_certificate, context, "exact-snapshot")?;
            recover_exact_snapshot_step(context, lock, &mut store)
        }
    }
}

fn recover_exact_snapshot_step(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &mut ImmutableJournalStore<'_>,
) -> Result<BoundedRecoveryStep, CodegenError> {
    match store.snapshot().phase().clone() {
        JournalPhaseV2::Preparing {
            pending: Some(PreparationPendingIntentV2::CreateOwner(_)),
            ..
        } => {
            let successor = store
                .snapshot()
                .cancel_owner_creation(store.records().last().expect("record exists").clone())
                .map_err(model_error_at(context.project_root()))?;
            store.publish_successor(successor)?;
            Ok(BoundedRecoveryStep::Advanced)
        }
        JournalPhaseV2::Preparing {
            pending: Some(PreparationPendingIntentV2::DiscardOwner(_)),
            ..
        } => {
            let successor = store
                .snapshot()
                .complete_owner_discard(store.records().last().expect("record exists").clone())
                .map_err(model_error_at(context.project_root()))?;
            store.publish_successor(successor)?;
            Ok(BoundedRecoveryStep::Advanced)
        }
        JournalPhaseV2::Preparing {
            pending: Some(PreparationPendingIntentV2::PlaceOwner(_)),
            ..
        } => recovery_preflight_mismatch(
            context,
            "exact-snapshot preflight omitted a pending artifact-placement intent",
        ),
        JournalPhaseV2::Preparing { pending: None, .. }
        | JournalPhaseV2::Prepared
        | JournalPhaseV2::Replacing { .. } => {
            let successor = store
                .snapshot()
                .begin_rollback(store.records().last().expect("record exists").clone())
                .map_err(model_error_at(context.project_root()))?;
            store.publish_successor(successor)?;
            Ok(BoundedRecoveryStep::Advanced)
        }
        JournalPhaseV2::RollingBack {
            pending: Some(_), ..
        } => recovery_preflight_mismatch(
            context,
            "exact-snapshot preflight omitted a pending rollback intent",
        ),
        JournalPhaseV2::RollingBack {
            next: 0,
            pending: None,
        } => {
            let successor = store
                .snapshot()
                .finish_rollback_targets(store.records().last().expect("record exists").clone())
                .map_err(model_error_at(context.project_root()))?;
            store.publish_successor(successor)?;
            Ok(BoundedRecoveryStep::Advanced)
        }
        JournalPhaseV2::RollingBack {
            next,
            pending: None,
        } => {
            recover_rollback_cursor_step(context, lock, store, next)?;
            Ok(BoundedRecoveryStep::Advanced)
        }
        JournalPhaseV2::RollbackComplete {
            pending: Some(_), ..
        }
        | JournalPhaseV2::CommitComplete {
            pending: Some(_), ..
        } => recovery_preflight_mismatch(
            context,
            "exact-snapshot preflight omitted a pending cleanup intent",
        ),
        JournalPhaseV2::RollbackComplete {
            cleanup_completed, ..
        } => recover_cleanup_cursor_step(
            context,
            store,
            TransactionOutcome::Rollback,
            cleanup_completed,
        ),
        JournalPhaseV2::CommitComplete {
            cleanup_completed, ..
        } => recover_cleanup_cursor_step(
            context,
            store,
            TransactionOutcome::Commit,
            cleanup_completed,
        ),
    }
}

fn recover_rollback_cursor_step(
    context: &PlanningContext,
    _lock: &WriteLock,
    store: &mut ImmutableJournalStore<'_>,
    next: u32,
) -> Result<(), CodegenError> {
    let entry = store.snapshot().entries()[(next - 1) as usize].clone();
    match (entry.action(), entry.current_target(), entry.preimage()) {
        (EntryActionV2::Create, PresenceV2::Missing, PreimageV2::Absent) => {
            if observe_target(
                context,
                store.runtime().fs(),
                entry.logical_path(),
                entry_file_read_limit(&entry),
            )?
            .is_some()
            {
                return third_state(entry.logical_path(), context);
            }
            let successor = store
                .snapshot()
                .advance_rollback_noop(store.records().last().expect("record exists").clone())
                .map_err(model_error_at(entry.logical_path()))?;
            store.publish_successor(successor)
        }
        (EntryActionV2::Create, PresenceV2::Present(target), PreimageV2::Absent) => arm_rollback(
            store,
            RollbackIntentV2::RemoveCreatedTarget {
                ordinal: entry.ordinal(),
                expected_target: target.clone(),
            },
            entry.logical_path(),
        ),
        (
            EntryActionV2::Replace,
            PresenceV2::Present(target),
            PreimageV2::Regular { exact: preimage },
        ) if target == preimage => {
            let actual = observe_target(
                context,
                store.runtime().fs(),
                entry.logical_path(),
                entry_file_read_limit(&entry),
            )?
            .ok_or_else(|| recovery_missing(entry.logical_path(), context))?;
            if !file_matches(&actual, preimage, None) {
                return third_state(entry.logical_path(), context);
            }
            let successor = store
                .snapshot()
                .advance_rollback_noop(store.records().last().expect("record exists").clone())
                .map_err(model_error_at(entry.logical_path()))?;
            store.publish_successor(successor)
        }
        (EntryActionV2::Replace, PresenceV2::Present(target), PreimageV2::Regular { .. }) => {
            let backup = entry
                .backup()
                .and_then(|backup| backup.current().as_present())
                .ok_or_else(|| CodegenError::RecoveryRequired {
                    journal_path: context.project_root().join(entry.logical_path()),
                    reason: "rollback requires an exact independent backup".to_owned(),
                })?;
            arm_rollback(
                store,
                RollbackIntentV2::RestoreBackup {
                    ordinal: entry.ordinal(),
                    expected_target: target.clone(),
                    expected_backup: backup.clone(),
                },
                entry.logical_path(),
            )
        }
        _ => third_state(entry.logical_path(), context),
    }
}

fn recover_cleanup_cursor_step(
    context: &PlanningContext,
    store: &mut ImmutableJournalStore<'_>,
    outcome: TransactionOutcome,
    completed: u32,
) -> Result<BoundedRecoveryStep, CodegenError> {
    if store.snapshot().cleanup_is_complete() {
        return Ok(BoundedRecoveryStep::ReadyForFinalization(outcome));
    }
    let plan = match outcome {
        TransactionOutcome::Commit => store.snapshot().cleanup_plans().commit(),
        TransactionOutcome::Rollback => store.snapshot().cleanup_plans().rollback(),
    };
    let target =
        plan.get(completed as usize)
            .copied()
            .ok_or_else(|| CodegenError::RecoveryRequired {
                journal_path: recovery_journal_path(context),
                reason: "cleanup cursor exceeds its immutable cleanup plan".to_owned(),
            })?;
    let successor = match cleanup_intent_for(store.snapshot(), target)? {
        Some(intent) => store
            .snapshot()
            .arm_cleanup(
                store.records().last().expect("record exists").clone(),
                intent,
            )
            .map_err(model_error_at(context.project_root()))?,
        None => store
            .snapshot()
            .advance_cleanup_noop(store.records().last().expect("record exists").clone())
            .map_err(model_error_at(context.project_root()))?,
    };
    store.publish_successor(successor)?;
    Ok(BoundedRecoveryStep::Advanced)
}

fn recovery_preflight_mismatch<T>(
    context: &PlanningContext,
    reason: &str,
) -> Result<T, CodegenError> {
    Err(CodegenError::RecoveryRequired {
        journal_path: recovery_journal_path(context),
        reason: format!("recovery preflight became stale: {reason}"),
    })
}

fn recovery_journal_path(context: &PlanningContext) -> PathBuf {
    context.project_root().join("src/components/ui/_kit")
}

fn discard_owned_residual(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &ImmutableJournalStore<'_>,
    binding: &OwnedResidualDeleteBindingV2,
) -> Result<(), CodegenError> {
    let owner = binding.owner();
    let name = owner.owner_name();
    let ordinal = owner.ordinal();
    let transition_artifact = match owner.artifact() {
        OwnerArtifactKindV2::Directory => PreparationArtifactKind::Directory,
        OwnerArtifactKindV2::Stage => PreparationArtifactKind::Stage,
        OwnerArtifactKindV2::Backup => PreparationArtifactKind::Backup,
    };
    let workspace_path = context
        .project_root()
        .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
        .join(store.snapshot().project().workspace().name());
    let owner_path = workspace_path.join(name);
    let (_namespace, workspace, _) = open_transaction_workspace(context, lock, store, &owner_path)?;
    let removal = match binding.object() {
        OwnedResidualObjectV2::File(expected) => {
            let expected_observation = ExactFileMetadataObservation {
                identity: ObjectIdentity::from_u128(
                    expected.identity().namespace(),
                    expected.identity().object(),
                ),
                byte_len: expected.byte_len(),
                mode: PreservedFileMode {
                    readonly: expected.readonly(),
                    posix_mode: expected.posix_mode(),
                },
                link_count: Some(expected.link_count()),
            };
            let observed = store
                .runtime()
                .fs()
                .observe_regular_file_metadata(
                    &workspace,
                    Path::new(name),
                    &owner_path,
                    expected.byte_len(),
                )
                .map_err(|source| {
                    transaction_io(
                        "rebind metadata-bound file residual",
                        &format!("owner ordinal {}", ordinal.get()),
                        &owner_path,
                        source,
                    )
                })?;
            if observed != expected_observation {
                return third_state("owned file residual", context);
            }
            store.runtime().observe(TransitionKey::DiscardOwner {
                artifact: transition_artifact,
                ordinal: ordinal.get(),
                window: TransitionWindow::Before,
            });
            store.runtime().fs().remove_file_metadata_exact(
                &workspace,
                Path::new(name),
                &owner_path,
                &expected_observation,
            )
        }
        OwnedResidualObjectV2::Directory(expected) => {
            let opened = store
                .runtime()
                .fs()
                .open_directory_exact(
                    &workspace,
                    Path::new(name),
                    &owner_path,
                    expected.exact().mode().posix_mode().unwrap_or(0o755),
                )
                .map_err(|source| {
                    transaction_io(
                        "rebind owned directory residual",
                        &format!("owner ordinal {}", ordinal.get()),
                        &owner_path,
                        source,
                    )
                })?;
            if exact_directory(&opened.observation).map_err(model_error_at(&owner_path))?
                != *expected.exact()
                || opened.observation.link_count != Some(expected.link_count())
            {
                return third_state("owned directory residual", context);
            }
            let inventory = store
                .runtime()
                .fs()
                .inventory_directory_exact_bounded(
                    DirectoryEndpoint::new(
                        &workspace,
                        Path::new(name),
                        &opened.directory,
                        &owner_path,
                    ),
                    &opened.observation,
                    0,
                )
                .map_err(|source| {
                    transaction_io(
                        "inventory owned directory residual",
                        &format!("owner ordinal {}", ordinal.get()),
                        &owner_path,
                        source,
                    )
                })?;
            debug_assert!(inventory.entries.is_empty());
            store.runtime().observe(TransitionKey::DiscardOwner {
                artifact: transition_artifact,
                ordinal: ordinal.get(),
                window: TransitionWindow::Before,
            });
            store.runtime().fs().remove_empty_directory_exact(
                DirectoryEndpoint::new(&workspace, Path::new(name), &opened.directory, &owner_path),
                &opened.observation,
            )
        }
    };

    // Exact removal can report an error after the expected object was already
    // unlinked (for example, when the controlled name is immediately
    // substituted). The durable DiscardOwner intent makes that uncertainty
    // recoverable: always cross the workspace durability barrier, then
    // classify the canonical name. Missing is the exact after-world; any
    // substitute is preserved and blocks successor publication.
    if let Err(sync_error) = sync_transaction_workspace(context, lock, store, &owner_path) {
        return Err(CodegenError::RecoveryRequired {
            journal_path: owner_path,
            reason: match removal.as_ref().err() {
                Some(remove_error) => format!(
                    "owned residual unlink may have completed ({remove_error}); its workspace durability barrier also failed: {sync_error}"
                ),
                None => format!(
                    "owned residual unlink completed, but its workspace durability barrier failed: {sync_error}"
                ),
            },
        });
    }
    match require_discarded_owner_absent(context, lock, store, binding) {
        Ok(()) => {
            store.runtime().observe(TransitionKey::DiscardOwner {
                artifact: transition_artifact,
                ordinal: ordinal.get(),
                window: TransitionWindow::After,
            });
            Ok(())
        }
        Err(absence_error) => Err(CodegenError::RecoveryRequired {
            journal_path: owner_path,
            reason: match removal.err() {
                Some(remove_error) => format!(
                    "owned residual unlink returned {remove_error}; the post-sync canonical name is not the exact absent after-world: {absence_error}"
                ),
                None => format!(
                    "owned residual unlink completed, but the post-sync canonical name is not the exact absent after-world: {absence_error}"
                ),
            },
        }),
    }
}

fn require_discarded_owner_absent(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &ImmutableJournalStore<'_>,
    binding: &OwnedResidualDeleteBindingV2,
) -> Result<(), CodegenError> {
    let name = binding.owner().owner_name();
    let owner_path = context
        .project_root()
        .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
        .join(store.snapshot().project().workspace().name())
        .join(name);
    let (_namespace, workspace, _) = open_transaction_workspace(context, lock, store, &owner_path)?;
    match workspace.symlink_metadata(Path::new(name)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => third_state("discarded owner residual", context),
        Err(source) => Err(transaction_io(
            "prove discarded owner absence",
            name,
            &owner_path,
            source,
        )),
    }
}

fn reconcile_pending_placement(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &mut ImmutableJournalStore<'_>,
    intent: PreparationPlacementIntentV2,
    artifact: RecoveryPreparationArtifactV2,
    world: MutationWorldV2,
    completion_authorized: bool,
) -> Result<(), CodegenError> {
    if world == MutationWorldV2::Before {
        let transition_kind = match (&intent, artifact) {
            (PreparationPlacementIntentV2::File(file), RecoveryPreparationArtifactV2::Stage)
                if file.artifact() == FileArtifactKindV2::Stage =>
            {
                PreparationArtifactKind::Stage
            }
            (PreparationPlacementIntentV2::File(file), RecoveryPreparationArtifactV2::Backup)
                if file.artifact() == FileArtifactKindV2::Backup =>
            {
                PreparationArtifactKind::Backup
            }
            (
                PreparationPlacementIntentV2::Directory(_),
                RecoveryPreparationArtifactV2::Directory,
            ) => PreparationArtifactKind::Directory,
            _ => {
                return recovery_preflight_mismatch(
                    context,
                    "placement cancellation artifact disagrees with its durable intent",
                );
            }
        };
        store.runtime().observe(TransitionKey::CancelPlacement {
            artifact: transition_kind,
            ordinal: intent.ordinal().get(),
            window: TransitionWindow::Before,
        });
        let successor = store
            .snapshot()
            .cancel_preparation_placement(store.records().last().expect("record exists").clone())
            .map_err(model_error_at(context.project_root()))?;
        store.publish_successor(successor)?;
        store.runtime().observe(TransitionKey::CancelPlacement {
            artifact: transition_kind,
            ordinal: intent.ordinal().get(),
            window: TransitionWindow::After,
        });
        return Ok(());
    }

    match intent {
        PreparationPlacementIntentV2::File(intent) => {
            let transition_kind = match (intent.artifact(), artifact) {
                (FileArtifactKindV2::Stage, RecoveryPreparationArtifactV2::Stage) => {
                    PreparationArtifactKind::Stage
                }
                (FileArtifactKindV2::Backup, RecoveryPreparationArtifactV2::Backup) => {
                    PreparationArtifactKind::Backup
                }
                _ => {
                    return recovery_preflight_mismatch(
                        context,
                        "file placement preflight artifact disagrees with its durable intent",
                    );
                }
            };
            let entry = &store.snapshot().entries()[intent.ordinal().get() as usize];
            let logical_path = entry.logical_path().to_owned();
            let parent_path = immediate_parent(&logical_path).unwrap_or("").to_owned();
            let target_path = context
                .project_root()
                .join(&parent_path)
                .join(intent.placed_name());
            let owner_path = context
                .project_root()
                .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
                .join(store.snapshot().project().workspace().name())
                .join(intent.owner_name());
            let runtime = store.runtime().clone();
            if !completion_authorized {
                runtime.observe(TransitionKey::Placement {
                    artifact: transition_kind,
                    ordinal: intent.ordinal().get(),
                    window: TransitionWindow::Before,
                });
            }
            let (_namespace, workspace, _) =
                open_transaction_workspace(context, lock, store, &owner_path)?;
            let parent = rebind_parent_for_mutation(
                context,
                lock,
                runtime.fs(),
                store.snapshot(),
                &parent_path,
                &target_path,
            )?;
            let parent_after = observe_directory_path(context, runtime.fs(), &parent_path)?;
            if !completion_authorized {
                sync_directory_path(
                    context,
                    lock,
                    runtime.fs(),
                    store.snapshot(),
                    &parent_path,
                    &parent_after,
                    &target_path,
                )?;
                sync_transaction_workspace(context, lock, store, &owner_path)?;
            }
            if observe_file_child_optional(
                runtime.fs(),
                &workspace,
                Path::new(intent.owner_name()),
                &owner_path,
                intent.expected_owner().state().byte_len(),
            )?
            .is_some()
            {
                return third_state(&logical_path, context);
            }
            let placed = observe_file_child_optional(
                runtime.fs(),
                &parent,
                Path::new(intent.placed_name()),
                &target_path,
                intent.expected_owner().state().byte_len(),
            )?
            .ok_or_else(|| recovery_missing(&logical_path, context))?;
            let placed_exact = exact_file(&placed).map_err(model_error_at(&target_path))?;
            if &placed_exact != intent.expected_owner() {
                return third_state(&logical_path, context);
            }
            if !completion_authorized {
                runtime.observe(TransitionKey::Placement {
                    artifact: transition_kind,
                    ordinal: intent.ordinal().get(),
                    window: TransitionWindow::After,
                });
                return Ok(());
            }
            let successor = store
                .snapshot()
                .complete_file_placement(
                    store.records().last().expect("record exists").clone(),
                    FilePlacementObservationV2::new(
                        placed_exact,
                        PresenceV2::Missing,
                        exact_directory(&parent_after).map_err(model_error_at(&target_path))?,
                    ),
                )
                .map_err(model_error_at(&target_path))?;
            store.publish_successor(successor)
        }
        PreparationPlacementIntentV2::Directory(intent) => {
            if artifact != RecoveryPreparationArtifactV2::Directory {
                return recovery_preflight_mismatch(
                    context,
                    "directory placement preflight artifact disagrees with its durable intent",
                );
            }
            let directory = store.snapshot().directories()[intent.ordinal().get() as usize].clone();
            let parent_path = immediate_parent(directory.logical_path()).unwrap_or("");
            let target_name = leaf_name(directory.logical_path());
            let target_path = context.project_root().join(directory.logical_path());
            let owner_path = context
                .project_root()
                .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
                .join(store.snapshot().project().workspace().name())
                .join(intent.owner_name());
            let runtime = store.runtime().clone();
            if !completion_authorized {
                runtime.observe(TransitionKey::Placement {
                    artifact: PreparationArtifactKind::Directory,
                    ordinal: intent.ordinal().get(),
                    window: TransitionWindow::Before,
                });
            }
            let (_namespace, workspace, _) =
                open_transaction_workspace(context, lock, store, &owner_path)?;
            let parent = rebind_parent_for_mutation(
                context,
                lock,
                runtime.fs(),
                store.snapshot(),
                parent_path,
                &target_path,
            )?;
            let placed = runtime
                .fs()
                .open_directory_exact(
                    &parent,
                    Path::new(target_name),
                    &target_path,
                    directory.planned_mode().posix_mode().unwrap_or(0o755),
                )
                .map_err(|source| {
                    transaction_io(
                        "rebind placed recovery directory",
                        directory.logical_path(),
                        &target_path,
                        source,
                    )
                })?;
            runtime
                .fs()
                .inventory_directory_exact_bounded(
                    DirectoryEndpoint::new(
                        &parent,
                        Path::new(target_name),
                        &placed.directory,
                        &target_path,
                    ),
                    &placed.observation,
                    0,
                )
                .map_err(|source| {
                    transaction_io(
                        "prove recovered directory placement remains exactly empty",
                        directory.logical_path(),
                        &target_path,
                        source,
                    )
                })?;
            if !completion_authorized {
                runtime
                    .fs()
                    .sync_directory(&placed.directory, &target_path)
                    .map_err(|source| {
                        transaction_io(
                            "sync placed recovery directory inode",
                            directory.logical_path(),
                            &target_path,
                            source,
                        )
                    })?;
            }
            let parent_after = observe_directory_path(context, runtime.fs(), parent_path)?;
            if !completion_authorized {
                sync_directory_path(
                    context,
                    lock,
                    runtime.fs(),
                    store.snapshot(),
                    parent_path,
                    &parent_after,
                    &target_path,
                )?;
                sync_transaction_workspace(context, lock, store, &owner_path)?;
            }
            if observe_directory_child(
                runtime.fs(),
                &workspace,
                Path::new(intent.owner_name()),
                &owner_path,
            )?
            .is_some()
            {
                return third_state(directory.logical_path(), context);
            }
            let placed_exact =
                exact_directory(&placed.observation).map_err(model_error_at(&target_path))?;
            if &placed_exact != intent.expected_owner() {
                return third_state(directory.logical_path(), context);
            }
            if !completion_authorized {
                runtime.observe(TransitionKey::Placement {
                    artifact: PreparationArtifactKind::Directory,
                    ordinal: intent.ordinal().get(),
                    window: TransitionWindow::After,
                });
                return Ok(());
            }
            // Rebind and inventory at the successor boundary. The earlier
            // observation authorized the durability pass, but it cannot also
            // authorize journal progress after an intervening child insertion.
            let final_parent = rebind_parent_for_mutation(
                context,
                lock,
                runtime.fs(),
                store.snapshot(),
                parent_path,
                &target_path,
            )?;
            let final_placed = runtime
                .fs()
                .open_directory_exact(
                    &final_parent,
                    Path::new(target_name),
                    &target_path,
                    directory.planned_mode().posix_mode().unwrap_or(0o755),
                )
                .map_err(|source| {
                    transaction_io(
                        "rebind placed directory at completion boundary",
                        directory.logical_path(),
                        &target_path,
                        source,
                    )
                })?;
            let final_placed_exact =
                exact_directory(&final_placed.observation).map_err(model_error_at(&target_path))?;
            if &final_placed_exact != intent.expected_owner() {
                return third_state(directory.logical_path(), context);
            }
            runtime
                .fs()
                .inventory_directory_exact_bounded(
                    DirectoryEndpoint::new(
                        &final_parent,
                        Path::new(target_name),
                        &final_placed.directory,
                        &target_path,
                    ),
                    &final_placed.observation,
                    0,
                )
                .map_err(|source| {
                    transaction_io(
                        "prove placed directory exactly empty at completion boundary",
                        directory.logical_path(),
                        &target_path,
                        source,
                    )
                })?;
            let final_parent_after = observe_directory_path(context, runtime.fs(), parent_path)?;
            let successor = store
                .snapshot()
                .complete_directory_publication(
                    store.records().last().expect("record exists").clone(),
                    DirectoryPublicationObservationV2::new(
                        final_placed_exact,
                        PresenceV2::Missing,
                        exact_directory(&final_parent_after)
                            .map_err(model_error_at(&target_path))?,
                    ),
                )
                .map_err(model_error_at(&target_path))?;
            store.publish_successor(successor)
        }
    }
}

fn reconcile_unrecorded_replacement(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &mut ImmutableJournalStore,
) -> Result<(), CodegenError> {
    lock.validate_context(context)?;
    let committed = match store.snapshot().phase() {
        JournalPhaseV2::Prepared => 0,
        JournalPhaseV2::Replacing { committed } => *committed as usize,
        _ => return Ok(()),
    };
    if committed >= store.snapshot().entries().len() {
        return Ok(());
    }
    let entry = store.snapshot().entries()[committed].clone();
    let observation = observe_unrecorded_replacement_after_world(context, lock, store, &entry)?;
    let successor = store
        .snapshot()
        .record_replacement_completion(
            store.records().last().expect("record exists").clone(),
            observation,
        )
        .map_err(model_error_at(entry.logical_path()))?;
    store.publish_successor(successor)
}

fn certify_unrecorded_replacement_durable(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &ImmutableJournalStore<'_>,
    entry: &JournalEntryV2,
) -> Result<(), CodegenError> {
    let parent_path = immediate_parent(entry.logical_path()).unwrap_or("");
    let target_path = context.project_root().join(entry.logical_path());
    let parent_before = observe_directory_path(context, store.runtime().fs(), parent_path)?;
    sync_directory_path(
        context,
        lock,
        store.runtime().fs(),
        store.snapshot(),
        parent_path,
        &parent_before,
        &target_path,
    )?;
    observe_unrecorded_replacement_after_world(context, lock, store, entry)?;
    store.runtime().observe(TransitionKey::ReplaceTarget {
        ordinal: entry.ordinal().get(),
        window: TransitionWindow::After,
    });
    Ok(())
}

fn observe_unrecorded_replacement_after_world(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &ImmutableJournalStore<'_>,
    entry: &JournalEntryV2,
) -> Result<ReplacementObservationV2, CodegenError> {
    let parent_path = immediate_parent(entry.logical_path()).unwrap_or("");
    let target_path = context.project_root().join(entry.logical_path());
    let stage_path = context
        .project_root()
        .join(parent_path)
        .join(entry.stage().name());
    let parent = rebind_parent_for_mutation(
        context,
        lock,
        store.runtime().fs(),
        store.snapshot(),
        parent_path,
        &target_path,
    )?;
    let stage_exact =
        entry
            .stage()
            .current()
            .as_present()
            .ok_or_else(|| CodegenError::RecoveryRequired {
                journal_path: target_path.clone(),
                reason: "unrecorded replacement has no exact stage authority".to_owned(),
            })?;
    let target = store
        .runtime()
        .fs()
        .observe_regular_file_bounded(
            &parent,
            Path::new(leaf_name(entry.logical_path())),
            &target_path,
            stage_exact.state().byte_len(),
        )
        .map_err(|source| {
            transaction_io(
                "prove recovered replacement target",
                entry.logical_path(),
                &target_path,
                source,
            )
        })?;
    match entry.action() {
        EntryActionV2::Create => {
            let stage = store
                .runtime()
                .fs()
                .observe_regular_file_bounded(
                    &parent,
                    Path::new(entry.stage().name()),
                    &stage_path,
                    stage_exact.state().byte_len(),
                )
                .map_err(|source| {
                    transaction_io(
                        "prove recovered create stage alias",
                        entry.logical_path(),
                        &stage_path,
                        source,
                    )
                })?;
            if !file_matches(&target, stage_exact, Some(2))
                || !file_matches(&stage, stage_exact, Some(2))
                || target.identity != stage.identity
                || target.content_hash != stage.content_hash
                || target.mode != stage.mode
            {
                return third_state(entry.logical_path(), context);
            }
            Ok(ReplacementObservationV2::new(
                exact_file(&target).map_err(model_error_at(&target_path))?,
                PresenceV2::Present(exact_file(&stage).map_err(model_error_at(&stage_path))?),
            ))
        }
        EntryActionV2::Replace => {
            if !file_matches(&target, stage_exact, Some(1)) {
                return third_state(entry.logical_path(), context);
            }
            match parent.symlink_metadata(Path::new(entry.stage().name())) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => return third_state(entry.logical_path(), context),
                Err(source) => {
                    return Err(transaction_io(
                        "prove recovered replacement stage absent",
                        entry.logical_path(),
                        &stage_path,
                        source,
                    ));
                }
            }
            Ok(ReplacementObservationV2::new(
                exact_file(&target).map_err(model_error_at(&target_path))?,
                PresenceV2::Missing,
            ))
        }
    }
}

fn arm_rollback(
    store: &mut ImmutableJournalStore,
    intent: RollbackIntentV2,
    path: &str,
) -> Result<(), CodegenError> {
    let successor = store
        .snapshot()
        .arm_rollback(
            store.records().last().expect("record exists").clone(),
            intent,
        )
        .map_err(model_error_at(path))?;
    store.publish_successor(successor)
}

fn complete_pending_rollback(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &ImmutableJournalStore,
    intent: RollbackIntentV2,
) -> Result<(), CodegenError> {
    let entry = store.snapshot().entries()[intent.ordinal().get() as usize].clone();
    let parent_path = immediate_parent(entry.logical_path()).unwrap_or("");
    let target_path = context.project_root().join(entry.logical_path());
    let parent = rebind_parent_for_mutation(
        context,
        lock,
        store.runtime().fs(),
        store.snapshot(),
        parent_path,
        &target_path,
    )?;
    match &intent {
        RollbackIntentV2::RemoveCreatedTarget {
            expected_target, ..
        } => {
            let target = match store.runtime().fs().observe_regular_file_bounded(
                &parent,
                Path::new(leaf_name(entry.logical_path())),
                &target_path,
                expected_target.state().byte_len(),
            ) {
                Ok(target) => Some(target),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(transaction_io(
                        "inspect created rollback target",
                        entry.logical_path(),
                        &target_path,
                        source,
                    ));
                }
            };
            let mut removal_error = None;
            if let Some(target) = target {
                if !file_matches(&target, expected_target, None) {
                    return third_state(entry.logical_path(), context);
                }
                store.runtime().observe(TransitionKey::RollbackTarget {
                    action: RollbackAction::RemoveCreatedTarget,
                    ordinal: entry.ordinal().get(),
                    window: TransitionWindow::Before,
                });
                let removal = store.runtime().fs().remove_file_exact(
                    &parent,
                    Path::new(leaf_name(entry.logical_path())),
                    &target_path,
                    &target,
                );
                match removal {
                    Ok(()) => {}
                    Err(error) if !error.mutation_may_have_completed() => {
                        return Err(transaction_io(
                            "remove created target",
                            entry.logical_path(),
                            &target_path,
                            std::io::Error::other(error),
                        ));
                    }
                    Err(error) => removal_error = Some(error),
                }
            }
            let parent_after = observe_directory_path(context, store.runtime().fs(), parent_path)?;
            if let Err(sync_error) = sync_directory_path(
                context,
                lock,
                store.runtime().fs(),
                store.snapshot(),
                parent_path,
                &parent_after,
                &target_path,
            ) {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: target_path.clone(),
                    reason: match removal_error.as_ref() {
                        Some(remove_error) => format!(
                            "created-target rollback unlink may have completed ({remove_error}); its parent durability barrier also failed: {sync_error}"
                        ),
                        None => format!(
                            "created-target rollback reached its after-world, but its parent durability barrier failed: {sync_error}"
                        ),
                    },
                });
            }
            let parent = rebind_parent_for_mutation(
                context,
                lock,
                store.runtime().fs(),
                store.snapshot(),
                parent_path,
                &target_path,
            )?;
            match parent.symlink_metadata(Path::new(leaf_name(entry.logical_path()))) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => return third_state(entry.logical_path(), context),
                Err(source) => {
                    return Err(transaction_io(
                        "prove created rollback target absent",
                        entry.logical_path(),
                        &target_path,
                        source,
                    ));
                }
            }
            let stage_path = context
                .project_root()
                .join(parent_path)
                .join(entry.stage().name());
            let stage = observe_file_child_optional(
                store.runtime().fs(),
                &parent,
                Path::new(entry.stage().name()),
                &stage_path,
                expected_target.state().byte_len(),
            )?
            .ok_or_else(|| recovery_missing(entry.logical_path(), context))?;
            if !file_matches(&stage, expected_target, Some(1)) {
                return third_state(entry.logical_path(), context);
            }
            store.runtime().observe(TransitionKey::RollbackTarget {
                action: RollbackAction::RemoveCreatedTarget,
                ordinal: entry.ordinal().get(),
                window: TransitionWindow::After,
            });
        }
        RollbackIntentV2::RestoreBackup {
            expected_target,
            expected_backup,
            ..
        } => {
            let backup = entry.backup().expect("replace backup");
            let backup_path = context.project_root().join(parent_path).join(backup.name());
            let target = match store.runtime().fs().observe_regular_file_bounded(
                &parent,
                Path::new(leaf_name(entry.logical_path())),
                &target_path,
                expected_target
                    .state()
                    .byte_len()
                    .max(expected_backup.state().byte_len()),
            ) {
                Ok(target) => Some(target),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(transaction_io(
                        "inspect replaced rollback target",
                        entry.logical_path(),
                        &target_path,
                        source,
                    ));
                }
            };
            let backup_live = match store.runtime().fs().observe_regular_file_bounded(
                &parent,
                Path::new(backup.name()),
                &backup_path,
                expected_backup.state().byte_len(),
            ) {
                Ok(observation) => Some(observation),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(transaction_io(
                        "inspect rollback backup",
                        entry.logical_path(),
                        &backup_path,
                        source,
                    ));
                }
            };
            match (target, backup_live) {
                (Some(target), Some(backup_live))
                    if file_matches(&target, expected_target, None)
                        && file_matches(&backup_live, expected_backup, None) =>
                {
                    store.runtime().observe(TransitionKey::RollbackTarget {
                        action: RollbackAction::RestoreBackup,
                        ordinal: entry.ordinal().get(),
                        window: TransitionWindow::Before,
                    });
                    store
                        .runtime()
                        .fs()
                        .replace_existing(
                            HardLinkEndpoint::new(&parent, Path::new(backup.name()), &backup_path),
                            &backup_live,
                            HardLinkEndpoint::new(
                                &parent,
                                Path::new(leaf_name(entry.logical_path())),
                                &target_path,
                            ),
                            &target,
                        )
                        .map_err(|source| {
                            transaction_io(
                                "restore rollback backup",
                                entry.logical_path(),
                                &target_path,
                                source,
                            )
                        })?;
                }
                (Some(target), None) if file_matches(&target, expected_backup, None) => {}
                _ => return third_state(entry.logical_path(), context),
            }
            let parent_after = observe_directory_path(context, store.runtime().fs(), parent_path)?;
            sync_directory_path(
                context,
                lock,
                store.runtime().fs(),
                store.snapshot(),
                parent_path,
                &parent_after,
                &target_path,
            )?;
            let restored = observe_file_child_optional(
                store.runtime().fs(),
                &parent,
                Path::new(leaf_name(entry.logical_path())),
                &target_path,
                expected_backup.state().byte_len(),
            )?
            .ok_or_else(|| recovery_missing(entry.logical_path(), context))?;
            if !file_matches(&restored, expected_backup, None)
                || observe_file_child_optional(
                    store.runtime().fs(),
                    &parent,
                    Path::new(backup.name()),
                    &backup_path,
                    expected_backup.state().byte_len(),
                )?
                .is_some()
            {
                return third_state(entry.logical_path(), context);
            }
            store.runtime().observe(TransitionKey::RollbackTarget {
                action: RollbackAction::RestoreBackup,
                ordinal: entry.ordinal().get(),
                window: TransitionWindow::After,
            });
        }
    }
    Ok(())
}

fn cleanup_intent_for(
    snapshot: &super::journal::JournalSnapshotV2,
    target: CleanupTargetV2,
) -> Result<Option<CleanupIntentV2>, CodegenError> {
    let intent = match target {
        CleanupTargetV2::OwnedStage { ordinal } => snapshot.entries()[ordinal.get() as usize]
            .stage()
            .owner_current()
            .as_present()
            .map(|expected| CleanupIntentV2::RemoveFile {
                target,
                expected: expected.clone(),
            }),
        CleanupTargetV2::PlacedStage { ordinal } => snapshot.entries()[ordinal.get() as usize]
            .stage()
            .current()
            .as_present()
            .map(|expected| CleanupIntentV2::RemoveFile {
                target,
                expected: expected.clone(),
            }),
        CleanupTargetV2::OwnedBackup { ordinal } => snapshot.entries()[ordinal.get() as usize]
            .backup()
            .and_then(|backup| backup.owner_current().as_present())
            .map(|expected| CleanupIntentV2::RemoveFile {
                target,
                expected: expected.clone(),
            }),
        CleanupTargetV2::PlacedBackup { ordinal } => snapshot.entries()[ordinal.get() as usize]
            .backup()
            .and_then(|backup| backup.current().as_present())
            .map(|expected| CleanupIntentV2::RemoveFile {
                target,
                expected: expected.clone(),
            }),
        CleanupTargetV2::OwnedDirectory { ordinal }
        | CleanupTargetV2::CreatedDirectory { ordinal } => {
            let directory = &snapshot.directories()[ordinal.get() as usize];
            let expected = match target {
                CleanupTargetV2::OwnedDirectory { .. } => {
                    directory.candidate_current().as_present()
                }
                CleanupTargetV2::CreatedDirectory { .. } => directory.current().as_present(),
                _ => unreachable!(),
            };
            expected.map(|expected| {
                let parent = if matches!(target, CleanupTargetV2::OwnedDirectory { .. }) {
                    DirectoryParentV2::TransactionWorkspace
                } else {
                    let logical_parent = immediate_parent(directory.logical_path()).unwrap_or("");
                    directory_parent(snapshot, logical_parent)
                        .expect("validated directory parent exists")
                };
                let parent_before = model_parent_current(snapshot, parent)
                    .expect("validated parent is present")
                    .clone();
                CleanupIntentV2::RemoveDirectory {
                    target,
                    expected: expected.clone(),
                    parent,
                    parent_before,
                }
            })
        }
    };
    Ok(intent)
}

fn execute_cleanup_intent(
    context: &PlanningContext,
    lock: &WriteLock,
    store: &ImmutableJournalStore,
    outcome: TransactionOutcome,
    intent: &CleanupIntentV2,
    perform_durability_transition: bool,
) -> Result<Option<ExactDirectoryStateV2>, CodegenError> {
    match intent {
        CleanupIntentV2::RemoveFile { target, expected } => {
            let (entry, name, path, kind, parent_path) =
                cleanup_file_location(context, store.snapshot(), *target)?;
            let owner = matches!(
                target,
                CleanupTargetV2::OwnedStage { .. } | CleanupTargetV2::OwnedBackup { .. }
            );
            let parent = if owner {
                open_transaction_workspace(context, lock, store, &path)?.1
            } else {
                rebind_parent_for_mutation(
                    context,
                    lock,
                    store.runtime().fs(),
                    store.snapshot(),
                    &parent_path,
                    &path,
                )?
            };
            let mut removal_error = None;
            match store.runtime().fs().observe_regular_file_bounded(
                &parent,
                Path::new(name),
                &path,
                expected.state().byte_len(),
            ) {
                Ok(observed)
                    if perform_durability_transition && file_matches(&observed, expected, None) =>
                {
                    store.runtime().observe(TransitionKey::CleanupObject {
                        outcome,
                        kind,
                        ordinal: entry.ordinal().get(),
                        window: TransitionWindow::Before,
                    });
                    let removal = store.runtime().fs().remove_file_exact(
                        &parent,
                        Path::new(name),
                        &path,
                        &observed,
                    );
                    match removal {
                        Ok(()) => {}
                        Err(error) if !error.mutation_may_have_completed() => {
                            return Err(transaction_io(
                                "remove transaction cleanup file",
                                entry.logical_path(),
                                &path,
                                std::io::Error::other(error),
                            ));
                        }
                        Err(error) => removal_error = Some(error),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return third_state(entry.logical_path(), context),
            }
            if perform_durability_transition {
                let sync_result = if owner {
                    sync_transaction_workspace(context, lock, store, &path)
                } else {
                    let parent_after =
                        observe_directory_path(context, store.runtime().fs(), &parent_path)?;
                    sync_directory_path(
                        context,
                        lock,
                        store.runtime().fs(),
                        store.snapshot(),
                        &parent_path,
                        &parent_after,
                        &path,
                    )
                };
                if let Err(sync_error) = sync_result {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: path.clone(),
                        reason: match removal_error.as_ref() {
                            Some(remove_error) => format!(
                                "transaction cleanup unlink may have completed ({remove_error}); its parent durability barrier also failed: {sync_error}"
                            ),
                            None => format!(
                                "transaction cleanup reached its after-world, but its parent durability barrier failed: {sync_error}"
                            ),
                        },
                    });
                }
            }
            let parent = if owner {
                open_transaction_workspace(context, lock, store, &path)?.1
            } else {
                rebind_parent_for_mutation(
                    context,
                    lock,
                    store.runtime().fs(),
                    store.snapshot(),
                    &parent_path,
                    &path,
                )?
            };
            match parent.symlink_metadata(Path::new(name)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => return third_state(entry.logical_path(), context),
                Err(source) => {
                    return Err(transaction_io(
                        "prove transaction cleanup file absent",
                        entry.logical_path(),
                        &path,
                        source,
                    ));
                }
            }
            if perform_durability_transition {
                store.runtime().observe(TransitionKey::CleanupObject {
                    outcome,
                    kind,
                    ordinal: entry.ordinal().get(),
                    window: TransitionWindow::After,
                });
            }
            Ok(None)
        }
        CleanupIntentV2::RemoveDirectory {
            target, expected, ..
        } => {
            let ordinal = match target {
                CleanupTargetV2::OwnedDirectory { ordinal }
                | CleanupTargetV2::CreatedDirectory { ordinal } => *ordinal,
                _ => unreachable!("directory cleanup intent has a directory target"),
            };
            let directory = &store.snapshot().directories()[ordinal.get() as usize];
            let logical_parent_path = immediate_parent(directory.logical_path()).unwrap_or("");
            let owner = matches!(target, CleanupTargetV2::OwnedDirectory { .. });
            let (name, path, kind) = match target {
                CleanupTargetV2::OwnedDirectory { .. } => (
                    directory.candidate_name().expect("candidate name"),
                    context
                        .project_root()
                        .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
                        .join(store.snapshot().project().workspace().name())
                        .join(directory.candidate_name().expect("candidate name")),
                    CleanupObjectKind::OwnedDirectory,
                ),
                CleanupTargetV2::CreatedDirectory { .. } => (
                    leaf_name(directory.logical_path()),
                    context.project_root().join(directory.logical_path()),
                    CleanupObjectKind::CreatedDirectory,
                ),
                _ => unreachable!(),
            };
            let parent = if owner {
                open_transaction_workspace(context, lock, store, &path)?.1
            } else {
                rebind_parent_for_mutation(
                    context,
                    lock,
                    store.runtime().fs(),
                    store.snapshot(),
                    logical_parent_path,
                    &path,
                )?
            };
            let mut removal_error = None;
            match observe_directory_child(store.runtime().fs(), &parent, Path::new(name), &path)? {
                Some(observed)
                    if perform_durability_transition && directory_matches(&observed, expected) =>
                {
                    let opened = store
                        .runtime()
                        .fs()
                        .open_directory_exact(
                            &parent,
                            Path::new(name),
                            &path,
                            expected.mode().posix_mode().unwrap_or(0o755),
                        )
                        .map_err(|source| {
                            transaction_io(
                                "open cleanup directory",
                                directory.logical_path(),
                                &path,
                                source,
                            )
                        })?;
                    store.runtime().observe(TransitionKey::CleanupObject {
                        outcome,
                        kind,
                        ordinal: ordinal.get(),
                        window: TransitionWindow::Before,
                    });
                    let removal = store.runtime().fs().remove_empty_directory_exact(
                        DirectoryEndpoint::new(&parent, Path::new(name), &opened.directory, &path),
                        &observed,
                    );
                    match removal {
                        Ok(()) => {}
                        Err(error) if !error.mutation_may_have_completed() => {
                            return Err(transaction_io(
                                "remove cleanup directory",
                                directory.logical_path(),
                                &path,
                                std::io::Error::other(error),
                            ));
                        }
                        Err(error) => removal_error = Some(error),
                    }
                }
                None => {}
                _ => {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: path.clone(),
                        reason: "cleanup directory is a third-state object".to_owned(),
                    });
                }
            }
            let parent_after = if owner {
                if perform_durability_transition {
                    if let Err(sync_error) = sync_transaction_workspace(context, lock, store, &path)
                    {
                        return Err(CodegenError::RecoveryRequired {
                            journal_path: path.clone(),
                            reason: match removal_error.as_ref() {
                                Some(remove_error) => format!(
                                    "cleanup directory removal may have completed ({remove_error}); its workspace durability barrier also failed: {sync_error}"
                                ),
                                None => format!(
                                    "cleanup directory reached its after-world, but its workspace durability barrier failed: {sync_error}"
                                ),
                            },
                        });
                    }
                }
                store.snapshot().project().workspace().exact().clone()
            } else {
                let observed =
                    observe_directory_path(context, store.runtime().fs(), logical_parent_path)?;
                if perform_durability_transition {
                    if let Err(sync_error) = sync_directory_path(
                        context,
                        lock,
                        store.runtime().fs(),
                        store.snapshot(),
                        logical_parent_path,
                        &observed,
                        &path,
                    ) {
                        return Err(CodegenError::RecoveryRequired {
                            journal_path: path.clone(),
                            reason: match removal_error.as_ref() {
                                Some(remove_error) => format!(
                                    "cleanup directory removal may have completed ({remove_error}); its parent durability barrier also failed: {sync_error}"
                                ),
                                None => format!(
                                    "cleanup directory reached its after-world, but its parent durability barrier failed: {sync_error}"
                                ),
                            },
                        });
                    }
                }
                exact_directory(&observed).map_err(model_error_at(directory.logical_path()))?
            };
            let parent = if owner {
                open_transaction_workspace(context, lock, store, &path)?.1
            } else {
                rebind_parent_for_mutation(
                    context,
                    lock,
                    store.runtime().fs(),
                    store.snapshot(),
                    logical_parent_path,
                    &path,
                )?
            };
            match parent.symlink_metadata(Path::new(name)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => return third_state(directory.logical_path(), context),
                Err(source) => {
                    return Err(transaction_io(
                        "prove cleanup directory absent",
                        directory.logical_path(),
                        &path,
                        source,
                    ));
                }
            }
            if perform_durability_transition {
                store.runtime().observe(TransitionKey::CleanupObject {
                    outcome,
                    kind,
                    ordinal: ordinal.get(),
                    window: TransitionWindow::After,
                });
            }
            Ok(Some(parent_after))
        }
    }
}

fn cleanup_file_location<'a>(
    context: &PlanningContext,
    snapshot: &'a super::journal::JournalSnapshotV2,
    target: CleanupTargetV2,
) -> Result<
    (
        &'a JournalEntryV2,
        &'a str,
        PathBuf,
        CleanupObjectKind,
        String,
    ),
    CodegenError,
> {
    let (ordinal, kind) = match target {
        CleanupTargetV2::OwnedStage { ordinal } => (ordinal, CleanupObjectKind::OwnedStage),
        CleanupTargetV2::PlacedStage { ordinal } => (ordinal, CleanupObjectKind::PlacedStage),
        CleanupTargetV2::OwnedBackup { ordinal } => (ordinal, CleanupObjectKind::OwnedBackup),
        CleanupTargetV2::PlacedBackup { ordinal } => (ordinal, CleanupObjectKind::PlacedBackup),
        _ => unreachable!("file cleanup target"),
    };
    let entry = &snapshot.entries()[ordinal.get() as usize];
    let logical_parent = immediate_parent(entry.logical_path()).unwrap_or("");
    let owner = matches!(
        target,
        CleanupTargetV2::OwnedStage { .. } | CleanupTargetV2::OwnedBackup { .. }
    );
    let name = match target {
        CleanupTargetV2::OwnedStage { .. } => entry.stage().owner_name(),
        CleanupTargetV2::PlacedStage { .. } => entry.stage().name(),
        CleanupTargetV2::OwnedBackup { .. } => entry.backup().expect("backup").owner_name(),
        CleanupTargetV2::PlacedBackup { .. } => entry.backup().expect("backup").name(),
        _ => unreachable!(),
    };
    Ok((
        entry,
        name,
        if owner {
            context
                .project_root()
                .join(TRANSACTION_NAMESPACE_LOGICAL_PATH)
                .join(snapshot.project().workspace().name())
                .join(name)
        } else {
            context.project_root().join(logical_parent).join(name)
        },
        kind,
        if owner {
            TRANSACTION_NAMESPACE_LOGICAL_PATH.to_owned()
        } else {
            logical_parent.to_owned()
        },
    ))
}

fn observe_directory_child(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &Path,
    path: &Path,
) -> Result<Option<ExactDirectoryObservation>, CodegenError> {
    let directory = match parent.open_dir_nofollow(name) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(transaction_io(
                "open recovery directory",
                &name.to_string_lossy(),
                path,
                source,
            ));
        }
    };
    fs.observe_directory(DirectoryEndpoint::new(parent, name, &directory, path))
        .map(Some)
        .map_err(|source| {
            transaction_io(
                "inspect recovery directory",
                &name.to_string_lossy(),
                path,
                source,
            )
        })
}

fn directory_matches(
    observed: &ExactDirectoryObservation,
    expected: &ExactDirectoryStateV2,
) -> bool {
    observed.identity
        == ObjectIdentity::from_u128(
            expected.identity().namespace(),
            expected.identity().object(),
        )
        && observed.mode.readonly == expected.mode().readonly()
        && observed.mode.posix_mode == expected.mode().posix_mode()
}

fn file_matches(
    observed: &ExactFileObservation,
    expected: &ExactFileStateV2,
    link_count: Option<u64>,
) -> bool {
    observed.identity
        == ObjectIdentity::from_u128(
            expected.identity().namespace(),
            expected.identity().object(),
        )
        && observed.content_hash == expected.state().content_hash().as_str()
        && observed.byte_len == expected.state().byte_len()
        && observed.mode.readonly == expected.state().readonly()
        && observed.mode.posix_mode == expected.state().posix_mode()
        && observed.link_count == Some(link_count.unwrap_or(expected.link_count()))
}

fn third_state<T>(path: &str, context: &PlanningContext) -> Result<T, CodegenError> {
    Err(CodegenError::RecoveryRequired {
        journal_path: context.project_root().join("src/components/ui/_kit"),
        reason: format!(
            "{path} is a third-state application or transaction object; recovery preserved it and all journal evidence"
        ),
    })
}

fn recovery_missing(path: &str, context: &PlanningContext) -> CodegenError {
    CodegenError::RecoveryRequired {
        journal_path: context.project_root().join("src/components/ui/_kit"),
        reason: format!("{path} is missing at an exact rollback boundary"),
    }
}

fn stage_owner_final_mode(preimage: &PathPreimage) -> PreservedFileMode {
    match preimage {
        PathPreimage::Absent => PreservedFileMode {
            readonly: false,
            posix_mode: if cfg!(unix) { Some(0o644) } else { None },
        },
        PathPreimage::RegularFile { mode, .. } => *mode,
    }
}

fn write_stage(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &Path,
    path: &Path,
    bytes: &[u8],
    final_mode: PreservedFileMode,
) -> Result<ExactFileObservation, CodegenError> {
    let mut created = match fs
        .create_new_file(parent, name, path, 0o600)
        .bind_empty(fs, parent, name, path)
    {
        Ok(created) => created,
        Err(ExclusiveCreateFailure::NotCreated(source)) => {
            return Err(transaction_io(
                "create stage",
                &name.to_string_lossy(),
                path,
                source,
            ));
        }
        Err(ExclusiveCreateFailure::CreatedUnverified { created, source }) => {
            let _owner_capability = created;
            return Err(CodegenError::RecoveryRequired {
                journal_path: path.to_path_buf(),
                reason: format!(
                    "exclusive stage creation changed the namespace but its live owner capability \
                     could not be rebound: {source}"
                ),
            });
        }
    };
    fs.write_handle(&mut created.file, path, bytes)
        .map_err(|source| transaction_io("write stage", &name.to_string_lossy(), path, source))?;
    fs.flush_file(&created.file, path)
        .map_err(|source| transaction_io("flush stage", &name.to_string_lossy(), path, source))?;
    fs.set_preserved_file_mode(&created.file, path, final_mode)
        .map_err(|source| {
            transaction_io("set stage mode", &name.to_string_lossy(), path, source)
        })?;
    fs.sync_handle(&created.file, path)
        .map_err(|source| transaction_io("sync stage", &name.to_string_lossy(), path, source))?;
    fs.observe_created_file_exact(parent, name, path, &mut created, bytes.len() as u64)
        .map_err(|source| {
            transaction_io(
                "verify stage through its live owner handle",
                &name.to_string_lossy(),
                path,
                source,
            )
        })
}

fn observe_target(
    context: &PlanningContext,
    fs: &dyn FsOps,
    logical_path: &str,
    max_bytes: u64,
) -> Result<Option<ExactFileObservation>, CodegenError> {
    let parent_path = immediate_parent(logical_path).unwrap_or("");
    let Ok(parent) = open_directory(context, parent_path) else {
        return Ok(None);
    };
    let path = context.project_root().join(logical_path);
    match fs.observe_regular_file_bounded(
        &parent,
        Path::new(leaf_name(logical_path)),
        &path,
        max_bytes,
    ) {
        Ok(observation) => Ok(Some(observation)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(transaction_io(
            "inspect target",
            logical_path,
            &path,
            source,
        )),
    }
}

fn entry_file_read_limit(entry: &JournalEntryV2) -> u64 {
    let mut max_bytes = entry.planned().byte_len();
    if let PreimageV2::Regular { exact } = entry.preimage() {
        max_bytes = max_bytes.max(exact.state().byte_len());
    }
    if let Some(current) = entry.current_target().as_present() {
        max_bytes = max_bytes.max(current.state().byte_len());
    }
    if let Some(current) = entry.stage().current().as_present() {
        max_bytes = max_bytes.max(current.state().byte_len());
    }
    if let Some(current) = entry
        .backup()
        .and_then(|backup| backup.current().as_present())
    {
        max_bytes = max_bytes.max(current.state().byte_len());
    }
    max_bytes
}

fn observe_directory_path(
    context: &PlanningContext,
    fs: &dyn FsOps,
    logical_path: &str,
) -> Result<ExactDirectoryObservation, CodegenError> {
    if logical_path.is_empty() {
        let directory = context.open_pinned_project_root()?;
        let metadata = directory
            .dir_metadata()
            .map_err(|source| CodegenError::Io {
                path: context.project_root().to_path_buf(),
                source,
            })?;
        return Ok(ExactDirectoryObservation {
            identity: ObjectIdentity::from_u64(
                MetadataExt::dev(&metadata),
                MetadataExt::ino(&metadata),
            ),
            mode: preserved_directory_mode(&metadata),
            link_count: Some(MetadataExt::nlink(&metadata)),
        });
    }
    let directory = open_directory(context, logical_path)?;
    let parent_path = immediate_parent(logical_path).unwrap_or("");
    let parent = open_directory(context, parent_path)?;
    let path = context.project_root().join(logical_path);
    fs.observe_directory(DirectoryEndpoint::new(
        &parent,
        Path::new(leaf_name(logical_path)),
        &directory,
        &path,
    ))
    .map_err(|source| transaction_io("inspect directory", logical_path, &path, source))
}

fn sync_directory_path(
    context: &PlanningContext,
    lock: &WriteLock,
    fs: &dyn FsOps,
    snapshot: &super::journal::JournalSnapshotV2,
    logical_path: &str,
    expected: &ExactDirectoryObservation,
    mutation: &Path,
) -> Result<(), CodegenError> {
    if logical_path.is_empty() {
        let directory =
            rebind_parent_for_mutation(context, lock, fs, snapshot, logical_path, mutation)?;
        let actual = observe_directory_path(context, fs, logical_path)?;
        if &actual != expected {
            return Err(CodegenError::RecoveryRequired {
                journal_path: context.project_root().to_path_buf(),
                reason: "project root changed before directory durability sync".to_owned(),
            });
        }
        return fs
            .sync_directory(&directory, context.project_root())
            .map_err(|source| {
                transaction_io("sync parent directory", logical_path, mutation, source)
            });
    }
    let parent_path = immediate_parent(logical_path).unwrap_or("");
    // The endpoint carries two directory capabilities. Rebind the outer
    // parent first and the directory being synced last so the latter is the
    // freshest authority at the mutation boundary.
    let parent = rebind_parent_for_mutation(context, lock, fs, snapshot, parent_path, mutation)?;
    let directory =
        rebind_parent_for_mutation(context, lock, fs, snapshot, logical_path, mutation)?;
    let path = context.project_root().join(logical_path);
    fs.sync_parent(
        DirectoryEndpoint::new(
            &parent,
            Path::new(leaf_name(logical_path)),
            &directory,
            &path,
        ),
        expected,
        ParentSyncKind::Target,
    )
    .map_err(|source| transaction_io("sync parent directory", logical_path, mutation, source))
}

fn directory_parent(
    snapshot: &super::journal::JournalSnapshotV2,
    parent_path: &str,
) -> Result<DirectoryParentV2, CodegenError> {
    if parent_path.is_empty() {
        return Ok(DirectoryParentV2::ProjectRoot);
    }
    if parent_path == "src/components/ui/_kit" {
        return Ok(DirectoryParentV2::CoordinationParent);
    }
    if parent_path == TRANSACTION_NAMESPACE_LOGICAL_PATH {
        return Ok(DirectoryParentV2::TransactionNamespace);
    }
    snapshot
        .directories()
        .iter()
        .find(|directory| directory.logical_path() == parent_path)
        .map(|directory| DirectoryParentV2::Cohort {
            ordinal: directory.ordinal(),
        })
        .ok_or_else(|| CodegenError::InvalidCoordinationState {
            path: parent_path.to_owned(),
            reason: "journal directory parent is outside the exact cohort".to_owned(),
        })
}

fn model_parent_current(
    snapshot: &super::journal::JournalSnapshotV2,
    parent: DirectoryParentV2,
) -> Result<&super::journal::ExactDirectoryStateV2, CodegenError> {
    match parent {
        DirectoryParentV2::ProjectRoot => Ok(snapshot.project().root_current()),
        DirectoryParentV2::CoordinationParent => Ok(snapshot.project().coordination_parent()),
        DirectoryParentV2::TransactionNamespace => {
            Ok(snapshot.project().workspace_parent_current())
        }
        DirectoryParentV2::TransactionWorkspace => Ok(snapshot.project().workspace().exact()),
        DirectoryParentV2::Cohort { ordinal } => snapshot
            .directories()
            .get(ordinal.get() as usize)
            .and_then(|directory| directory.current().as_present())
            .ok_or_else(|| CodegenError::InvalidCoordinationState {
                path: "transaction directory parent".to_owned(),
                reason: "journal parent is not durably present".to_owned(),
            }),
    }
}

fn open_directory(context: &PlanningContext, logical_path: &str) -> Result<Dir, CodegenError> {
    if logical_path.is_empty() {
        context.open_pinned_project_root()
    } else {
        context.open_directory(logical_path)
    }
}

fn rebind_parent_for_mutation(
    context: &PlanningContext,
    lock: &WriteLock,
    fs: &dyn FsOps,
    snapshot: &super::journal::JournalSnapshotV2,
    logical_parent: &str,
    mutation_path: &Path,
) -> Result<Dir, CodegenError> {
    let binding = directory_parent(snapshot, logical_parent)?;
    let expected = model_parent_current(snapshot, binding)?;
    TransactionAuthority::new(context, lock).rebind_parent_for_mutation(
        fs,
        logical_parent,
        expected,
        mutation_path,
    )
}

fn logical_parents(path: &str) -> Vec<String> {
    let mut parents = Vec::new();
    let mut current = String::new();
    let components = path.split('/').collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(component);
        parents.push(current.clone());
    }
    parents
}

fn immediate_parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

fn leaf_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn path_depth(path: &str) -> usize {
    path.split('/').count()
}

#[cfg(unix)]
fn normal_directory_mode() -> Option<u32> {
    Some(0o755)
}

#[cfg(unix)]
fn preserved_directory_mode(metadata: &cap_std::fs::Metadata) -> crate::PreservedFileMode {
    use cap_std::fs::PermissionsExt;
    crate::PreservedFileMode {
        readonly: metadata.permissions().readonly(),
        posix_mode: Some(metadata.permissions().mode() & 0o7777),
    }
}

#[cfg(not(unix))]
fn preserved_directory_mode(metadata: &cap_std::fs::Metadata) -> crate::PreservedFileMode {
    crate::PreservedFileMode {
        readonly: metadata.permissions().readonly(),
        posix_mode: None,
    }
}

#[cfg(not(unix))]
fn normal_directory_mode() -> Option<u32> {
    None
}

#[cfg(test)]
mod barrier_certificate_tests {
    use tempfile::TempDir;

    use super::{
        RecoveryAdoptionPlanKind, RecoveryBarrierCertificate, RecoveryBarrierSlot,
        authorize_recovery_barrier, recovery_adoption_plan_kind,
    };
    use crate::path_safety::PlanningContext;
    use crate::transaction::journal::{
        ArtifactOrdinal, CleanupTargetV2, EntryActionV2, ExactFileStateV2, FileStateV2,
        ObjectIdentityV2, RecordBindingV2, Sha256Digest, TransactionId,
    };
    use crate::transaction::recovery_policy::{
        MutationWorldV2, RecoveryPreflightV2, RecoveryPreparationArtifactV2,
    };

    fn record(identity: u64) -> RecordBindingV2 {
        RecordBindingV2::new(
            7,
            "txn-test-00000000000000000007.json",
            ExactFileStateV2::new(
                ObjectIdentityV2::new(9, identity),
                FileStateV2::new(
                    Sha256Digest::parse(&format!("sha256:{}", "0".repeat(64))).unwrap(),
                    1,
                    false,
                    Some(0o600),
                )
                .unwrap(),
                1,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn certificate(transaction: &str, identity: u64) -> RecoveryBarrierCertificate {
        RecoveryBarrierCertificate {
            transaction_id: TransactionId::parse(transaction).unwrap(),
            latest_record: record(identity),
            slot: RecoveryBarrierSlot::ForwardReplacement {
                ordinal: ArtifactOrdinal::new(3).unwrap(),
                action: EntryActionV2::Create,
            },
        }
    }

    #[test]
    fn same_slot_cannot_consume_a_certificate_from_another_lineage() {
        let temporary = TempDir::new().unwrap();
        let context = PlanningContext::open(temporary.path()).unwrap();
        let expected = certificate("4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a", 11);
        let other_transaction = certificate("5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b", 11);
        let other_record = certificate("4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a", 12);

        assert!(
            authorize_recovery_barrier(Some(&other_transaction), &expected, true, &context,)
                .is_err()
        );
        assert!(
            authorize_recovery_barrier(Some(&other_record), &expected, true, &context).is_err()
        );

        let different_action = RecoveryBarrierCertificate {
            transaction_id: expected.transaction_id.clone(),
            latest_record: expected.latest_record.clone(),
            slot: RecoveryBarrierSlot::ForwardReplacement {
                ordinal: ArtifactOrdinal::new(3).unwrap(),
                action: EntryActionV2::Replace,
            },
        };
        assert!(
            authorize_recovery_barrier(Some(&different_action), &expected, true, &context).is_err()
        );

        let different_ordinal = RecoveryBarrierCertificate {
            transaction_id: expected.transaction_id.clone(),
            latest_record: expected.latest_record.clone(),
            slot: RecoveryBarrierSlot::ForwardReplacement {
                ordinal: ArtifactOrdinal::new(4).unwrap(),
                action: EntryActionV2::Create,
            },
        };
        assert!(
            authorize_recovery_barrier(Some(&different_ordinal), &expected, true, &context)
                .is_err()
        );
    }

    #[test]
    fn fresh_after_world_requires_one_certificate_before_successor_authorization() {
        let temporary = TempDir::new().unwrap();
        let context = PlanningContext::open(temporary.path()).unwrap();
        let expected = certificate("4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a", 11);

        assert_eq!(
            authorize_recovery_barrier(None, &expected, true, &context).unwrap(),
            false
        );
        assert_eq!(
            authorize_recovery_barrier(Some(&expected), &expected, true, &context).unwrap(),
            true
        );
    }

    #[test]
    fn recovery_preflight_maps_exhaustively_to_successor_or_combined_barrier() {
        let ordinal = ArtifactOrdinal::new(3).unwrap();
        for preflight in [
            RecoveryPreflightV2::PendingOwnerCreation { residual: None },
            RecoveryPreflightV2::PendingPlacement {
                ordinal,
                artifact: RecoveryPreparationArtifactV2::Stage,
                world: MutationWorldV2::Before,
            },
            RecoveryPreflightV2::ExactSnapshot,
        ] {
            assert_eq!(
                recovery_adoption_plan_kind(&preflight),
                RecoveryAdoptionPlanKind::Successor
            );
        }

        for preflight in [
            RecoveryPreflightV2::PendingOwnerDiscard {
                world: MutationWorldV2::Before,
            },
            RecoveryPreflightV2::PendingOwnerDiscard {
                world: MutationWorldV2::After,
            },
            RecoveryPreflightV2::PendingPlacement {
                ordinal,
                artifact: RecoveryPreparationArtifactV2::Stage,
                world: MutationWorldV2::After,
            },
            RecoveryPreflightV2::ForwardReplacementCompleted {
                ordinal,
                world: MutationWorldV2::Before,
            },
            RecoveryPreflightV2::ForwardReplacementCompleted {
                ordinal,
                world: MutationWorldV2::After,
            },
            RecoveryPreflightV2::PendingRollback {
                ordinal,
                world: MutationWorldV2::Before,
            },
            RecoveryPreflightV2::PendingRollback {
                ordinal,
                world: MutationWorldV2::After,
            },
            RecoveryPreflightV2::PendingCleanup {
                target: CleanupTargetV2::PlacedStage { ordinal },
                world: MutationWorldV2::Before,
            },
            RecoveryPreflightV2::PendingCleanup {
                target: CleanupTargetV2::PlacedStage { ordinal },
                world: MutationWorldV2::After,
            },
        ] {
            assert_eq!(
                recovery_adoption_plan_kind(&preflight),
                RecoveryAdoptionPlanKind::CombinedOperationBarrier
            );
        }
    }
}
