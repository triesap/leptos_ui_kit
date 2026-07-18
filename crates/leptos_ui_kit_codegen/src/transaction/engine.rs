use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use cap_fs_ext::MetadataExt;
use cap_std::fs::Dir;

use crate::path_safety::PlanningContext;
use crate::{
    ChangeKind, ChangeRecord, CodegenError, PathPreimage, PlanSnapshot, PlannedFile,
    PlannedFileAction, validate_planned_write_paths,
};

use super::fs::{
    DirectoryEndpoint, DirectoryPublicationOutcome, ExactDirectoryObservation,
    ExactFileObservation, FsOps, HardLinkEndpoint, ParentSyncKind,
};
use super::journal::{
    ArtifactOrdinal, DirectoryModeV2, DirectoryParentV2, DirectoryPublicationObservationV2,
    DirectoryPublishIntentV2, EntryActionV2, EntryRoleV2, FileModePolicyV2, JournalDirectoryV2,
    JournalEntryV2, JournalOperationV2, PlannedFileStateV2, PreimageV2, PreparationObservationV2,
    PresenceV2, ReplacementObservationV2, Sha256Digest,
};
use super::lock::WriteLock;
use super::runtime::{
    CleanupObjectKind, RollbackAction, TransactionOutcome, TransactionRuntime, TransitionKey,
    TransitionWindow,
};
use super::store::{exact_directory, exact_file};
use super::writer::{
    ImmutableJournalStore, exact_existing_directory, model_error_at, transaction_io,
};

struct OrderedFile<'a> {
    file: &'a PlannedFile,
    ordinal: ArtifactOrdinal,
    role: EntryRoleV2,
}

struct PreparedFile {
    ordinal: ArtifactOrdinal,
    logical_path: String,
    parent: Dir,
    target_name: String,
    target_path: PathBuf,
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
    if files.is_empty() {
        return Ok(());
    }

    let ordered = order_and_validate_lock(files, changes, operation)?;
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

    prepare_directories(context, &mut store)?;
    let prepared = prepare_files(context, snapshot, &ordered, &mut store)?;
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

    commit_files(context, snapshot, &prepared, &mut store)
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
    let markers = changes
        .iter()
        .filter(|change| change.kind == ChangeKind::WriteLockFile)
        .map(|change| change.path.as_str())
        .collect::<Vec<_>>();
    if markers.len() > 1 {
        return Err(CodegenError::InvalidCoordinationState {
            path: "transaction cohort".to_owned(),
            reason: "multiple install-lock change markers are not permitted".to_owned(),
        });
    }
    if operation != JournalOperationV2::AtomicWrite && markers.len() != 1 {
        return Err(CodegenError::InvalidCoordinationState {
            path: "transaction cohort".to_owned(),
            reason: "mutating command cohort must select exactly one install lock".to_owned(),
        });
    }
    let selected = markers.first().copied();
    if let Some(selected) = selected {
        let matches = files.iter().filter(|file| file.path == selected).count();
        if matches != 1 {
            return Err(CodegenError::InvalidCoordinationState {
                path: selected.to_owned(),
                reason: "selected install-lock marker must name exactly one cohort target"
                    .to_owned(),
            });
        }
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
                file,
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
            let target = observe_target(context, fs, &ordered.file.path)?;
            let (action, preimage, mode_policy) = match (
                ordered.file.action,
                snapshot.preimage(&ordered.file.path),
                target,
            ) {
                (PlannedFileAction::Create, Some(PathPreimage::Absent), None) => (
                    EntryActionV2::Create,
                    PreimageV2::Absent,
                    FileModePolicyV2::NormalCreateResolveOnStage,
                ),
                (
                    PlannedFileAction::Update,
                    Some(PathPreimage::RegularFile { content_hash, mode }),
                    Some(observation),
                ) if content_hash == &observation.content_hash && mode == &observation.mode => (
                    EntryActionV2::Replace,
                    PreimageV2::regular(
                        exact_file(&observation).map_err(model_error_at(&ordered.file.path))?,
                    ),
                    FileModePolicyV2::PreservePreimage,
                ),
                _ => {
                    return Err(CodegenError::PreimageConflict {
                        path: ordered.file.path.clone(),
                        reason: "target changed while exact journal intent was constructed"
                            .to_owned(),
                    });
                }
            };
            JournalEntryV2::new(
                transaction_id,
                ordered.ordinal,
                &ordered.file.path,
                action,
                ordered.role,
                preimage,
                PlannedFileStateV2::new(
                    Sha256Digest::parse(&crate::hash_content_bytes(
                        ordered.file.content.as_bytes(),
                    ))
                    .map_err(model_error_at(&ordered.file.path))?,
                    ordered.file.content.len() as u64,
                    mode_policy,
                )
                .map_err(model_error_at(&ordered.file.path))?,
            )
            .map_err(model_error_at(&ordered.file.path))
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
        paths.extend(logical_parents(&ordered.file.path));
    }
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
            if path == "src/components/ui/_kit" {
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

fn prepare_directories(
    context: &PlanningContext,
    store: &mut ImmutableJournalStore,
) -> Result<(), CodegenError> {
    let directories = store.snapshot().directories().to_vec();
    for directory in directories.iter().filter(|directory| {
        directory.disposition() == super::journal::DirectoryDispositionV2::Create
    }) {
        let parent_path = immediate_parent(directory.logical_path()).unwrap_or("");
        let target_name = leaf_name(directory.logical_path());
        let parent = open_directory(context, parent_path)?;
        let candidate_name = directory
            .candidate_name()
            .expect("created directory has a candidate name")
            .to_owned();
        let candidate_path = context
            .project_root()
            .join(parent_path)
            .join(&candidate_name);
        let runtime = store.runtime().clone();
        runtime.observe(TransitionKey::CreateDirectoryCandidate {
            ordinal: directory.ordinal().get(),
            window: TransitionWindow::Before,
        });
        let candidate = runtime
            .fs()
            .create_directory_exact(&parent, Path::new(&candidate_name), &candidate_path, 0o700)
            .map_err(|source| {
                transaction_io(
                    "create directory candidate",
                    directory.logical_path(),
                    &candidate_path,
                    source,
                )
            })?;
        let parent_after = observe_directory_path(context, runtime.fs(), parent_path)?;
        sync_directory_path(
            context,
            runtime.fs(),
            parent_path,
            &parent_after,
            &candidate_path,
        )?;
        runtime.observe(TransitionKey::CreateDirectoryCandidate {
            ordinal: directory.ordinal().get(),
            window: TransitionWindow::After,
        });
        let current_record = store.records().last().expect("record exists").clone();
        let candidate_snapshot = store
            .snapshot()
            .adopt_next_preparation(
                current_record,
                PreparationObservationV2::DirectoryCandidate {
                    exact: exact_directory(&candidate.observation)
                        .map_err(model_error_at(&candidate_path))?,
                    parent_after: exact_directory(&parent_after)
                        .map_err(model_error_at(&candidate_path))?,
                },
            )
            .map_err(model_error_at(&candidate_path))?;
        store.publish_successor(candidate_snapshot)?;

        let live = store
            .snapshot()
            .directories()
            .iter()
            .find(|candidate| candidate.ordinal() == directory.ordinal())
            .expect("journal directory remains present");
        let parent_binding = directory_parent(store.snapshot(), parent_path)?;
        let parent_before = model_parent_current(store.snapshot(), parent_binding)?.clone();
        let intent = DirectoryPublishIntentV2::new(
            directory.ordinal(),
            &candidate_name,
            live.candidate_current()
                .as_present()
                .expect("candidate is recorded")
                .clone(),
            parent_binding,
            parent_before,
        );
        let armed = store
            .snapshot()
            .arm_directory_publication(
                store.records().last().expect("record exists").clone(),
                intent.clone(),
            )
            .map_err(model_error_at(&candidate_path))?;
        store.publish_successor(armed)?;

        runtime
            .fs()
            .set_directory_mode(&candidate.directory, &candidate_path, 0o755)
            .map_err(|source| {
                transaction_io(
                    "set directory mode",
                    directory.logical_path(),
                    &candidate_path,
                    source,
                )
            })?;
        let ready = runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &parent,
                Path::new(&candidate_name),
                &candidate.directory,
                &candidate_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "verify directory candidate",
                    directory.logical_path(),
                    &candidate_path,
                    source,
                )
            })?;
        let inventory = runtime
            .fs()
            .inventory_directory_exact(
                DirectoryEndpoint::new(
                    &parent,
                    Path::new(&candidate_name),
                    &candidate.directory,
                    &candidate_path,
                ),
                &ready,
            )
            .map_err(|source| {
                transaction_io(
                    "inventory directory candidate",
                    directory.logical_path(),
                    &candidate_path,
                    source,
                )
            })?;
        runtime.observe(TransitionKey::PublishDirectoryCandidate {
            ordinal: directory.ordinal().get(),
            window: TransitionWindow::Before,
        });
        let target_path = context.project_root().join(directory.logical_path());
        let parent_parent_path = immediate_parent(parent_path).unwrap_or("");
        let parent_parent = open_directory(context, parent_parent_path)?;
        let parent_name = if parent_path.is_empty() {
            Path::new(".")
        } else {
            Path::new(leaf_name(parent_path))
        };
        let outcome = runtime.fs().publish_directory_absent(
            DirectoryEndpoint::new(
                &parent,
                Path::new(&candidate_name),
                &candidate.directory,
                &candidate_path,
            ),
            &inventory,
            DirectoryEndpoint::new(
                &parent_parent,
                parent_name,
                &parent,
                &context.project_root().join(parent_path),
            ),
            &parent_after,
            Path::new(target_name),
            &target_path,
        );
        let published = match outcome {
            DirectoryPublicationOutcome::Durable { published } => published,
            DirectoryPublicationOutcome::NotPublished { source, .. }
            | DirectoryPublicationOutcome::VisibleDurabilityUnknown { source, .. } => {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: candidate_path,
                    reason: format!("directory publication requires recovery: {source}"),
                });
            }
        };
        let parent_after_publish = observe_directory_path(context, runtime.fs(), parent_path)?;
        runtime.observe(TransitionKey::PublishDirectoryCandidate {
            ordinal: directory.ordinal().get(),
            window: TransitionWindow::After,
        });
        let completed = store
            .snapshot()
            .complete_directory_publication(
                store.records().last().expect("record exists").clone(),
                DirectoryPublicationObservationV2::new(
                    exact_directory(&published.directory).map_err(model_error_at(&target_path))?,
                    PresenceV2::Missing,
                    exact_directory(&parent_after_publish).map_err(model_error_at(&target_path))?,
                ),
            )
            .map_err(model_error_at(&target_path))?;
        store.publish_successor(completed)?;
    }
    Ok(())
}

fn prepare_files(
    context: &PlanningContext,
    snapshot: &PlanSnapshot,
    ordered: &[OrderedFile<'_>],
    store: &mut ImmutableJournalStore,
) -> Result<Vec<PreparedFile>, CodegenError> {
    let mut prepared = Vec::with_capacity(ordered.len());
    for ordered in ordered {
        let parent_path = immediate_parent(&ordered.file.path).unwrap_or("");
        let parent = open_directory(context, parent_path)?;
        let target_name = leaf_name(&ordered.file.path).to_owned();
        let target_path = context.project_root().join(&ordered.file.path);
        let entry = &store.snapshot().entries()[ordered.ordinal.get() as usize];
        let stage_name = entry.stage().name().to_owned();
        let stage_path = context.project_root().join(parent_path).join(&stage_name);
        let runtime = store.runtime().clone();
        runtime.observe(TransitionKey::PrepareStage {
            ordinal: ordered.ordinal.get(),
            window: TransitionWindow::Before,
        });
        let stage = write_stage(
            runtime.fs(),
            &parent,
            Path::new(&stage_name),
            &stage_path,
            ordered.file.content.as_bytes(),
            snapshot
                .preimage(&ordered.file.path)
                .expect("validated preimage"),
        )?;
        let parent_state = observe_directory_path(context, runtime.fs(), parent_path)?;
        sync_directory_path(
            context,
            runtime.fs(),
            parent_path,
            &parent_state,
            &stage_path,
        )?;
        runtime.observe(TransitionKey::PrepareStage {
            ordinal: ordered.ordinal.get(),
            window: TransitionWindow::After,
        });
        let successor = store
            .snapshot()
            .adopt_next_preparation(
                store.records().last().expect("record exists").clone(),
                PreparationObservationV2::Stage {
                    exact: exact_file(&stage).map_err(model_error_at(&stage_path))?,
                },
            )
            .map_err(model_error_at(&stage_path))?;
        store.publish_successor(successor)?;
        prepared.push(PreparedFile {
            ordinal: ordered.ordinal,
            logical_path: ordered.file.path.clone(),
            parent,
            target_name,
            target_path,
            stage_name,
            stage_path,
            stage,
            backup_name: None,
            backup_path: None,
            backup: None,
        });
    }

    for prepared_file in &mut prepared {
        let entry = &store.snapshot().entries()[prepared_file.ordinal.get() as usize];
        let Some(backup_artifact) = entry.backup() else {
            continue;
        };
        let backup_name = backup_artifact.name().to_owned();
        let backup_path = prepared_file
            .target_path
            .parent()
            .expect("target has a parent")
            .join(&backup_name);
        let source = observe_target(context, store.runtime().fs(), &prepared_file.logical_path)?
            .expect("replace preimage exists");
        let runtime = store.runtime().clone();
        runtime.observe(TransitionKey::PrepareBackup {
            ordinal: prepared_file.ordinal.get(),
            window: TransitionWindow::Before,
        });
        let copy = runtime
            .fs()
            .create_exclusive_copy(
                HardLinkEndpoint::new(
                    &prepared_file.parent,
                    Path::new(&prepared_file.target_name),
                    &prepared_file.target_path,
                ),
                &source,
                HardLinkEndpoint::new(&prepared_file.parent, Path::new(&backup_name), &backup_path),
            )
            .map_err(|source| {
                transaction_io(
                    "create backup",
                    &prepared_file.logical_path,
                    &backup_path,
                    source,
                )
            })?;
        store
            .runtime()
            .fs()
            .flush_file(&copy.file, &backup_path)
            .map_err(|source| {
                transaction_io(
                    "flush backup",
                    &prepared_file.logical_path,
                    &backup_path,
                    source,
                )
            })?;
        let parent_path = immediate_parent(&prepared_file.logical_path).unwrap_or("");
        let parent_state = observe_directory_path(context, runtime.fs(), parent_path)?;
        sync_directory_path(
            context,
            runtime.fs(),
            parent_path,
            &parent_state,
            &backup_path,
        )?;
        runtime.observe(TransitionKey::PrepareBackup {
            ordinal: prepared_file.ordinal.get(),
            window: TransitionWindow::After,
        });
        let successor = store
            .snapshot()
            .adopt_next_preparation(
                store.records().last().expect("record exists").clone(),
                PreparationObservationV2::Backup {
                    exact: exact_file(&copy.copy).map_err(model_error_at(&backup_path))?,
                },
            )
            .map_err(model_error_at(&backup_path))?;
        store.publish_successor(successor)?;
        prepared_file.backup_name = Some(backup_name);
        prepared_file.backup_path = Some(backup_path);
        prepared_file.backup = Some(copy.copy);
    }
    Ok(prepared)
}

fn commit_files(
    context: &PlanningContext,
    snapshot: &PlanSnapshot,
    prepared: &[PreparedFile],
    store: &mut ImmutableJournalStore,
) -> Result<(), CodegenError> {
    for prepared_file in prepared {
        let runtime = store.runtime().clone();
        runtime.observe(TransitionKey::ReplaceTarget {
            ordinal: prepared_file.ordinal.get(),
            window: TransitionWindow::Before,
        });
        snapshot.revalidate_path(context, &prepared_file.logical_path)?;
        let entry = &store.snapshot().entries()[prepared_file.ordinal.get() as usize];
        match entry.action() {
            EntryActionV2::Create => runtime
                .fs()
                .publish_absent(
                    HardLinkEndpoint::new(
                        &prepared_file.parent,
                        Path::new(&prepared_file.stage_name),
                        &prepared_file.stage_path,
                    ),
                    &prepared_file.stage,
                    HardLinkEndpoint::new(
                        &prepared_file.parent,
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
                })?,
            EntryActionV2::Replace => {
                let target = observe_target(context, runtime.fs(), &prepared_file.logical_path)?
                    .expect("replace target exists");
                runtime
                    .fs()
                    .replace_existing(
                        HardLinkEndpoint::new(
                            &prepared_file.parent,
                            Path::new(&prepared_file.stage_name),
                            &prepared_file.stage_path,
                        ),
                        &prepared_file.stage,
                        HardLinkEndpoint::new(
                            &prepared_file.parent,
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
        let parent_path = immediate_parent(&prepared_file.logical_path).unwrap_or("");
        let parent_state = observe_directory_path(context, runtime.fs(), parent_path)?;
        sync_directory_path(
            context,
            runtime.fs(),
            parent_path,
            &parent_state,
            &prepared_file.target_path,
        )?;
        let target = observe_target(context, runtime.fs(), &prepared_file.logical_path)?
            .ok_or_else(|| CodegenError::RecoveryRequired {
                journal_path: prepared_file.target_path.clone(),
                reason: "published target disappeared before exact verification".to_owned(),
            })?;
        let stage = match entry.action() {
            EntryActionV2::Create => PresenceV2::Present(
                exact_file(
                    &runtime
                        .fs()
                        .observe_regular_file(
                            &prepared_file.parent,
                            Path::new(&prepared_file.stage_name),
                            &prepared_file.stage_path,
                        )
                        .map_err(|source| {
                            transaction_io(
                                "verify stage",
                                &prepared_file.logical_path,
                                &prepared_file.stage_path,
                                source,
                            )
                        })?,
                )
                .map_err(model_error_at(&prepared_file.stage_path))?,
            ),
            EntryActionV2::Replace => PresenceV2::Missing,
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
    cleanup_commit(context, prepared, store)
}

fn cleanup_commit(
    context: &PlanningContext,
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
            super::journal::CleanupTargetV2::Stage { ordinal }
            | super::journal::CleanupTargetV2::Backup { ordinal } => {
                let entry = by_ordinal[&ordinal.get()];
                let (name, path, expected, kind) = match target {
                    super::journal::CleanupTargetV2::Stage { .. } => (
                        &entry.stage_name,
                        &entry.stage_path,
                        store.snapshot().entries()[ordinal.get() as usize]
                            .stage()
                            .current()
                            .as_present(),
                        CleanupObjectKind::Stage,
                    ),
                    super::journal::CleanupTargetV2::Backup { .. } => (
                        entry.backup_name.as_ref().expect("backup name"),
                        entry.backup_path.as_ref().expect("backup path"),
                        store.snapshot().entries()[ordinal.get() as usize]
                            .backup()
                            .expect("backup model")
                            .current()
                            .as_present(),
                        CleanupObjectKind::Backup,
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
                let observed = runtime
                    .fs()
                    .observe_regular_file(&entry.parent, Path::new(name), path)
                    .map_err(|source| {
                        transaction_io("verify cleanup file", &entry.logical_path, path, source)
                    })?;
                runtime
                    .fs()
                    .remove_file_exact(&entry.parent, Path::new(name), path, &observed)
                    .map_err(|source| {
                        transaction_io("remove cleanup file", &entry.logical_path, path, source)
                    })?;
                let parent_path = immediate_parent(&entry.logical_path).unwrap_or("");
                let parent_state = observe_directory_path(context, runtime.fs(), parent_path)?;
                sync_directory_path(context, runtime.fs(), parent_path, &parent_state, path)?;
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
            super::journal::CleanupTargetV2::DirectoryCandidate { .. }
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

fn write_stage(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &Path,
    path: &Path,
    bytes: &[u8],
    preimage: &PathPreimage,
) -> Result<ExactFileObservation, CodegenError> {
    let mut created = fs
        .create_new_file(parent, name, path, 0o666)
        .map_err(|source| transaction_io("create stage", &name.to_string_lossy(), path, source))?;
    if let PathPreimage::RegularFile { mode, .. } = preimage
        && let Some(posix_mode) = mode.posix_mode
    {
        fs.set_file_mode(&created.file, path, posix_mode)
            .map_err(|source| {
                transaction_io("set stage mode", &name.to_string_lossy(), path, source)
            })?;
    }
    fs.write_handle(&mut created.file, path, bytes)
        .map_err(|source| transaction_io("write stage", &name.to_string_lossy(), path, source))?;
    fs.flush_file(&created.file, path)
        .map_err(|source| transaction_io("flush stage", &name.to_string_lossy(), path, source))?;
    fs.sync_handle(&created.file, path)
        .map_err(|source| transaction_io("sync stage", &name.to_string_lossy(), path, source))?;
    let observation = fs
        .observe_regular_file(parent, name, path)
        .map_err(|source| transaction_io("verify stage", &name.to_string_lossy(), path, source))?;
    if observation.identity != created.identity {
        return Err(CodegenError::RecoveryRequired {
            journal_path: path.to_path_buf(),
            reason: "stage identity changed after exclusive creation".to_owned(),
        });
    }
    Ok(observation)
}

fn observe_target(
    context: &PlanningContext,
    fs: &dyn FsOps,
    logical_path: &str,
) -> Result<Option<ExactFileObservation>, CodegenError> {
    let parent_path = immediate_parent(logical_path).unwrap_or("");
    let Ok(parent) = open_directory(context, parent_path) else {
        return Ok(None);
    };
    let path = context.project_root().join(logical_path);
    match fs.observe_regular_file(&parent, Path::new(leaf_name(logical_path)), &path) {
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
            identity: (MetadataExt::dev(&metadata), MetadataExt::ino(&metadata)),
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
    fs: &dyn FsOps,
    logical_path: &str,
    expected: &ExactDirectoryObservation,
    mutation: &Path,
) -> Result<(), CodegenError> {
    if logical_path.is_empty() {
        let directory = context.open_pinned_project_root()?;
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
    let directory = open_directory(context, logical_path)?;
    let parent_path = immediate_parent(logical_path).unwrap_or("");
    let parent = open_directory(context, parent_path)?;
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
