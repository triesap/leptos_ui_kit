use std::{
    io,
    path::{Path, PathBuf},
};

use cap_fs_ext::MetadataExt;
use cap_std::fs::{Dir, Metadata};

use crate::path_safety::PlanningContext;
use crate::{CodegenError, PreservedFileMode};

use super::fs::{
    DirectoryEndpoint, ExactDirectoryObservation, ExactFileObservation, FsOps, HardLinkEndpoint,
    ImmutablePublicationOutcome, ParentSyncKind,
};
use super::journal::{
    ExactDirectoryStateV2, FinalizationLeaseV2, JournalDirectoryV2, JournalEntryV2,
    JournalModelError, JournalOperationV2, JournalPhaseV2, JournalSnapshotV2,
    PartialEnvelopeHeaderV2, PartialRecordBindingV2, PresenceV2, ProjectBindingV2, RecordBindingV2,
    TransactionId, WorkspaceBootstrapBindingV2, WorkspaceBootstrapEnvelopeV2,
    WorkspaceBootstrapIntentBindingV2, WorkspaceBootstrapIntentEnvelopeV2, bootstrap_intent_name,
    bootstrap_owner_name, canonical_root_hash, transaction_directory_name,
};
use super::lock::{DEFAULT_KIT_WRITE_LOCK_PATH, WriteLock};
use super::runtime::{
    EntropyPurpose, JournalRecordKind, TransactionOutcome, TransactionRuntime, TransitionKey,
    TransitionWindow,
};
use super::store::{exact_directory, exact_file, exact_file_observation};

const KIT_LOGICAL_PATH: &str = "src/components/ui/_kit";
const KIT_PARENT_LOGICAL_PATH: &str = "src/components/ui";
const JOURNAL_FILE_LIMIT: u64 = 16 * 1024 * 1024;

pub(super) struct ImmutableJournalStore {
    runtime: TransactionRuntime,
    kit_parent: Dir,
    kit: Dir,
    workspace: Dir,
    kit_path: PathBuf,
    workspace_path: PathBuf,
    snapshot: JournalSnapshotV2,
    records: Vec<RecordBindingV2>,
}

impl ImmutableJournalStore {
    pub(super) fn create<F>(
        context: &PlanningContext,
        lock: &WriteLock,
        runtime: TransactionRuntime,
        operation: JournalOperationV2,
        build: F,
    ) -> Result<Self, CodegenError>
    where
        F: FnOnce(
            &TransactionId,
            &ProjectBindingV2,
        ) -> Result<(Vec<JournalEntryV2>, Vec<JournalDirectoryV2>), CodegenError>,
    {
        lock.validate_context(context)?;
        let mut entropy = [0_u8; 16];
        runtime
            .fill_entropy(EntropyPurpose::TransactionId, &mut entropy)
            .map_err(|source| {
                transaction_io("generate entropy", ".", context.project_root(), source)
            })?;
        let transaction_id = TransactionId::parse(&hex(&entropy)).map_err(model_error_at(
            context.project_root().join(KIT_LOGICAL_PATH),
        ))?;

        let kit_parent = context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
        let kit = context.open_directory(KIT_LOGICAL_PATH)?;
        let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
        let kit_name = Path::new("_kit");
        let kit_before_observation = runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &kit_parent,
                kit_name,
                &kit,
                &kit_path,
            ))
            .map_err(|source| {
                transaction_io("inspect directory", KIT_LOGICAL_PATH, &kit_path, source)
            })?;
        let kit_before =
            exact_directory(&kit_before_observation).map_err(model_error_at(&kit_path))?;

        let root = context.open_pinned_project_root()?;
        let root_metadata = root.dir_metadata().map_err(|source| CodegenError::Io {
            path: context.project_root().to_path_buf(),
            source,
        })?;
        let root_exact = exact_directory_from_metadata(&root_metadata)
            .map_err(model_error_at(context.project_root().to_path_buf()))?;
        let root_hash = canonical_root_hash(&canonical_native_bytes(context.project_root()));

        let lock_path = context.project_root().join(DEFAULT_KIT_WRITE_LOCK_PATH);
        let lock_observation = runtime
            .fs()
            .observe_regular_file(&kit, Path::new(".write.lock"), &lock_path)
            .map_err(|source| {
                transaction_io(
                    "inspect metadata",
                    DEFAULT_KIT_WRITE_LOCK_PATH,
                    &lock_path,
                    source,
                )
            })?;
        if lock_observation.identity != lock.identity() {
            return Err(CodegenError::InvalidCoordinationState {
                path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
                reason: "the journal store observed a different advisory-lock inode".to_owned(),
            });
        }
        let lock_exact = exact_file(&lock_observation).map_err(model_error_at(&lock_path))?;

        let intent_envelope = WorkspaceBootstrapIntentEnvelopeV2::new(
            transaction_id.clone(),
            root_hash.clone(),
            kit_before.clone(),
        )
        .map_err(model_error_at(&kit_path))?;
        let intent_bytes = intent_envelope
            .to_json_bytes()
            .map_err(model_error_at(&kit_path))?;
        let intent_name = bootstrap_intent_name(&transaction_id);
        let intent_path = kit_path.join(&intent_name);
        runtime.observe(TransitionKey::PublishWorkspaceOwnership {
            window: TransitionWindow::Before,
        });
        let intent_observation = write_private_exact(
            runtime.fs(),
            &kit,
            Path::new(&intent_name),
            &intent_path,
            &intent_bytes,
        )?;
        let kit_after_intent_observation = runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &kit_parent,
                kit_name,
                &kit,
                &kit_path,
            ))
            .map_err(|source| {
                transaction_io("inspect directory", KIT_LOGICAL_PATH, &kit_path, source)
            })?;
        sync_exact_parent(
            runtime.fs(),
            DirectoryEndpoint::new(&kit_parent, kit_name, &kit, &kit_path),
            &kit_after_intent_observation,
            &intent_path,
        )?;
        let intent = WorkspaceBootstrapIntentBindingV2::new(
            intent_envelope,
            exact_file(&intent_observation).map_err(model_error_at(&intent_path))?,
        )
        .map_err(model_error_at(&intent_path))?;
        runtime.observe(TransitionKey::PublishWorkspaceOwnership {
            window: TransitionWindow::After,
        });

        let workspace_name = transaction_directory_name(&transaction_id);
        let workspace_path = kit_path.join(&workspace_name);
        runtime.observe(TransitionKey::BootstrapWorkspace {
            window: TransitionWindow::Before,
        });
        let workspace_handle = runtime
            .fs()
            .create_directory_exact(&kit, Path::new(&workspace_name), &workspace_path, 0o700)
            .map_err(|source| {
                transaction_io("create directory", &workspace_name, &workspace_path, source)
            })?;
        let kit_after_observation = runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &kit_parent,
                kit_name,
                &kit,
                &kit_path,
            ))
            .map_err(|source| {
                transaction_io("inspect directory", KIT_LOGICAL_PATH, &kit_path, source)
            })?;
        sync_exact_parent(
            runtime.fs(),
            DirectoryEndpoint::new(&kit_parent, kit_name, &kit, &kit_path),
            &kit_after_observation,
            &workspace_path,
        )?;
        runtime.observe(TransitionKey::BootstrapWorkspace {
            window: TransitionWindow::After,
        });

        let project = ProjectBindingV2::new(
            &transaction_id,
            root_hash,
            root_exact,
            lock_exact,
            exact_directory(&kit_after_intent_observation).map_err(model_error_at(&kit_path))?,
            exact_directory(&kit_after_observation).map_err(model_error_at(&kit_path))?,
            exact_directory(&workspace_handle.observation)
                .map_err(model_error_at(&workspace_path))?,
        )
        .map_err(model_error_at(&workspace_path))?;

        let bootstrap_envelope =
            WorkspaceBootstrapEnvelopeV2::for_project(&transaction_id, &project);
        let bootstrap_bytes = bootstrap_envelope
            .to_json_bytes()
            .map_err(model_error_at(&workspace_path))?;
        let bootstrap_name = bootstrap_owner_name(&transaction_id);
        let bootstrap_path = workspace_path.join(&bootstrap_name);
        let bootstrap_observation = write_private_exact(
            runtime.fs(),
            &workspace_handle.directory,
            Path::new(&bootstrap_name),
            &bootstrap_path,
            &bootstrap_bytes,
        )?;
        let workspace_after_bootstrap = runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &kit,
                Path::new(&workspace_name),
                &workspace_handle.directory,
                &workspace_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "inspect directory",
                    &workspace_name,
                    &workspace_path,
                    source,
                )
            })?;
        sync_exact_parent(
            runtime.fs(),
            DirectoryEndpoint::new(
                &kit,
                Path::new(&workspace_name),
                &workspace_handle.directory,
                &workspace_path,
            ),
            &workspace_after_bootstrap,
            &bootstrap_path,
        )?;
        let bootstrap = WorkspaceBootstrapBindingV2::new(
            &transaction_id,
            &project,
            intent,
            exact_file(&bootstrap_observation).map_err(model_error_at(&bootstrap_path))?,
        )
        .map_err(model_error_at(&bootstrap_path))?;

        let (entries, directories) = build(&transaction_id, &project)?;
        let snapshot = JournalSnapshotV2::new(
            transaction_id,
            operation,
            project,
            bootstrap,
            entries,
            directories,
        )
        .map_err(model_error_at(&workspace_path))?;

        let mut store = Self {
            runtime,
            kit_parent,
            kit,
            workspace: workspace_handle.directory,
            kit_path,
            workspace_path,
            snapshot,
            records: Vec::new(),
        };
        let record = store.publish_snapshot(&store.snapshot.clone())?;
        store.records.push(record);
        Ok(store)
    }

    pub(super) fn runtime(&self) -> &TransactionRuntime {
        &self.runtime
    }

    pub(super) fn snapshot(&self) -> &JournalSnapshotV2 {
        &self.snapshot
    }

    pub(super) fn records(&self) -> &[RecordBindingV2] {
        &self.records
    }

    pub(super) fn publish_successor(
        &mut self,
        successor: JournalSnapshotV2,
    ) -> Result<(), CodegenError> {
        self.snapshot
            .validate_successor(&successor)
            .map_err(model_error_at(&self.workspace_path))?;
        let record = self.publish_snapshot(&successor)?;
        self.records.push(record);
        self.snapshot = successor;
        Ok(())
    }

    pub(super) fn finalize(&self, outcome: TransactionOutcome) -> Result<(), CodegenError> {
        if !self.snapshot.ready_for_finalization() {
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.workspace_path.clone(),
                reason: "transaction journal is not ready for exact finalization".to_owned(),
            });
        }
        let lease = FinalizationLeaseV2::arm(&self.snapshot, self.records.clone(), None)
            .map_err(model_error_at(&self.workspace_path))?;
        self.publish_finalization(&lease, outcome, false)?;

        let intent_name = bootstrap_intent_name(self.snapshot.transaction_id());
        let intent_path = self.kit_path.join(&intent_name);
        self.runtime
            .observe(TransitionKey::RemoveWorkspaceOwnership {
                outcome,
                window: TransitionWindow::Before,
            });
        self.remove_exact_workspace_file(
            &self.kit,
            Path::new(&intent_name),
            &intent_path,
            &exact_file_observation(self.snapshot.bootstrap().intent().exact()),
            &self.kit_parent,
            Path::new("_kit"),
            &self.kit,
            &self.kit_path,
        )?;

        let bootstrap_name = self.snapshot.bootstrap().name();
        let bootstrap_path = self.workspace_path.join(bootstrap_name);
        self.remove_exact_workspace_file(
            &self.workspace,
            Path::new(bootstrap_name),
            &bootstrap_path,
            &exact_file_observation(self.snapshot.bootstrap().exact()),
            &self.kit,
            Path::new(self.workspace_path.file_name().expect("workspace leaf")),
            &self.workspace,
            &self.workspace_path,
        )?;

        for record in self.records.iter().rev() {
            let record_path = self.workspace_path.join(record.name());
            self.runtime.observe(TransitionKey::RemoveJournalHistory {
                outcome,
                kind: JournalRecordKind::Published,
                sequence: record.sequence(),
                window: TransitionWindow::Before,
            });
            self.remove_exact_workspace_file(
                &self.workspace,
                Path::new(record.name()),
                &record_path,
                &exact_file_observation(record.exact()),
                &self.kit,
                Path::new(self.workspace_path.file_name().expect("workspace leaf")),
                &self.workspace,
                &self.workspace_path,
            )?;
            self.runtime.observe(TransitionKey::RemoveJournalHistory {
                outcome,
                kind: JournalRecordKind::Published,
                sequence: record.sequence(),
                window: TransitionWindow::After,
            });
        }
        self.runtime
            .observe(TransitionKey::RemoveWorkspaceOwnership {
                outcome,
                window: TransitionWindow::After,
            });

        let workspace_name = self.workspace_path.file_name().expect("workspace leaf");
        let workspace_before = self
            .runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &self.kit,
                Path::new(workspace_name),
                &self.workspace,
                &self.workspace_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "inspect transaction workspace",
                    KIT_LOGICAL_PATH,
                    &self.workspace_path,
                    source,
                )
            })?;
        let expected_workspace = self.snapshot.project().workspace().exact();
        if workspace_before.identity
            != (
                expected_workspace.identity().device(),
                expected_workspace.identity().inode(),
            )
            || workspace_before.mode.readonly != expected_workspace.mode().readonly()
            || workspace_before.mode.posix_mode != expected_workspace.mode().posix_mode()
        {
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.workspace_path.clone(),
                reason: "transaction workspace changed before exact finalization".to_owned(),
            });
        }
        self.runtime
            .observe(TransitionKey::RemoveTransactionWorkspace {
                outcome,
                window: TransitionWindow::Before,
            });
        self.runtime
            .fs()
            .remove_empty_directory_exact(
                DirectoryEndpoint::new(
                    &self.kit,
                    Path::new(workspace_name),
                    &self.workspace,
                    &self.workspace_path,
                ),
                &workspace_before,
            )
            .map_err(|source| {
                transaction_io(
                    "remove transaction workspace",
                    KIT_LOGICAL_PATH,
                    &self.workspace_path,
                    source,
                )
            })?;
        let kit_after_workspace = self
            .runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &self.kit_parent,
                Path::new("_kit"),
                &self.kit,
                &self.kit_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "inspect directory",
                    KIT_LOGICAL_PATH,
                    &self.kit_path,
                    source,
                )
            })?;
        sync_exact_parent(
            self.runtime.fs(),
            DirectoryEndpoint::new(
                &self.kit_parent,
                Path::new("_kit"),
                &self.kit,
                &self.kit_path,
            ),
            &kit_after_workspace,
            &self.workspace_path,
        )?;
        self.runtime
            .observe(TransitionKey::RemoveTransactionWorkspace {
                outcome,
                window: TransitionWindow::After,
            });

        let closed = lease
            .mark_workspace_removed(
                exact_directory(&kit_after_workspace).map_err(model_error_at(&self.kit_path))?,
            )
            .map_err(model_error_at(&self.kit_path))?;
        self.publish_finalization(&closed, outcome, true)?;

        self.remove_finalization_record(&lease, outcome)?;
        self.remove_finalization_record(&closed, outcome)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn remove_exact_workspace_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        expected: &ExactFileObservation,
        sync_parent_parent: &Dir,
        sync_parent_name: &Path,
        sync_parent: &Dir,
        sync_parent_path: &Path,
    ) -> Result<(), CodegenError> {
        let observed = self
            .runtime
            .fs()
            .observe_regular_file(parent, name, path)
            .map_err(|source| {
                transaction_io(
                    "inspect transaction file",
                    &name.to_string_lossy(),
                    path,
                    source,
                )
            })?;
        if &observed != expected {
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.workspace_path.clone(),
                reason: format!("{} changed before exact finalization", path.display()),
            });
        }
        self.runtime
            .fs()
            .remove_file_exact(parent, name, path, expected)
            .map_err(|source| {
                transaction_io(
                    "remove transaction file",
                    &name.to_string_lossy(),
                    path,
                    source,
                )
            })?;
        let parent_observation = self
            .runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                sync_parent_parent,
                sync_parent_name,
                sync_parent,
                sync_parent_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "inspect transaction parent",
                    &name.to_string_lossy(),
                    sync_parent_path,
                    source,
                )
            })?;
        sync_exact_parent(
            self.runtime.fs(),
            DirectoryEndpoint::new(
                sync_parent_parent,
                sync_parent_name,
                sync_parent,
                sync_parent_path,
            ),
            &parent_observation,
            path,
        )
    }

    fn publish_finalization(
        &self,
        lease: &FinalizationLeaseV2,
        outcome: TransactionOutcome,
        progress: bool,
    ) -> Result<(), CodegenError> {
        let partial_name = lease.partial_name();
        let partial_path = self.kit_path.join(&partial_name);
        let record_name = lease.record_name();
        let record_path = self.kit_path.join(&record_name);
        let key = if progress {
            TransitionKey::PublishFinalizationProgress {
                outcome,
                generation: lease.generation(),
                window: TransitionWindow::Before,
            }
        } else {
            TransitionKey::PublishFinalizationLease {
                outcome,
                generation: lease.generation(),
                window: TransitionWindow::Before,
            }
        };
        self.runtime.observe(key);
        let bytes = lease
            .to_json_bytes()
            .map_err(model_error_at(&record_path))?;
        let partial = write_private_exact(
            self.runtime.fs(),
            &self.kit,
            Path::new(&partial_name),
            &partial_path,
            &bytes,
        )?;
        let kit_observation = self
            .runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &self.kit_parent,
                Path::new("_kit"),
                &self.kit,
                &self.kit_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "inspect directory",
                    KIT_LOGICAL_PATH,
                    &self.kit_path,
                    source,
                )
            })?;
        sync_exact_parent(
            self.runtime.fs(),
            DirectoryEndpoint::new(
                &self.kit_parent,
                Path::new("_kit"),
                &self.kit,
                &self.kit_path,
            ),
            &kit_observation,
            &partial_path,
        )?;
        let published = match self.runtime.fs().publish_immutable(
            HardLinkEndpoint::new(&self.kit, Path::new(&partial_name), &partial_path),
            &partial,
            HardLinkEndpoint::new(&self.kit, Path::new(&record_name), &record_path),
            DirectoryEndpoint::new(
                &self.kit_parent,
                Path::new("_kit"),
                &self.kit,
                &self.kit_path,
            ),
            &kit_observation,
            ParentSyncKind::Journal,
        ) {
            ImmutablePublicationOutcome::Durable { published } => published,
            ImmutablePublicationOutcome::NotPublished { source, .. }
            | ImmutablePublicationOutcome::VisibleDurabilityUnknown { source, .. }
            | ImmutablePublicationOutcome::DurableWithPartialResidual { source, .. } => {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: self.kit_path.clone(),
                    reason: format!(
                        "finalization generation {} requires conservative recovery: {source}",
                        lease.generation()
                    ),
                });
            }
        };
        if published.content_hash != crate::hash_content_bytes(&bytes) {
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.kit_path.clone(),
                reason: "published finalization record does not match its canonical lease bytes"
                    .to_owned(),
            });
        }
        let key = if progress {
            TransitionKey::PublishFinalizationProgress {
                outcome,
                generation: lease.generation(),
                window: TransitionWindow::After,
            }
        } else {
            TransitionKey::PublishFinalizationLease {
                outcome,
                generation: lease.generation(),
                window: TransitionWindow::After,
            }
        };
        self.runtime.observe(key);
        Ok(())
    }

    fn remove_finalization_record(
        &self,
        lease: &FinalizationLeaseV2,
        outcome: TransactionOutcome,
    ) -> Result<(), CodegenError> {
        let name = lease.record_name();
        let path = self.kit_path.join(&name);
        let observed = self
            .runtime
            .fs()
            .observe_regular_file(&self.kit, Path::new(&name), &path)
            .map_err(|source| {
                transaction_io("inspect finalization record", &name, &path, source)
            })?;
        let expected_hash =
            crate::hash_content_bytes(&lease.to_json_bytes().map_err(model_error_at(&path))?);
        if observed.content_hash != expected_hash || observed.mode.posix_mode != private_mode() {
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.kit_path.clone(),
                reason: format!(
                    "finalization record {} changed before cleanup",
                    lease.generation()
                ),
            });
        }
        self.runtime
            .observe(TransitionKey::RemoveFinalizationLease {
                outcome,
                generation: lease.generation(),
                window: TransitionWindow::Before,
            });
        self.runtime
            .fs()
            .remove_file_exact(&self.kit, Path::new(&name), &path, &observed)
            .map_err(|source| transaction_io("remove finalization record", &name, &path, source))?;
        let kit_observation = self
            .runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &self.kit_parent,
                Path::new("_kit"),
                &self.kit,
                &self.kit_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "inspect directory",
                    KIT_LOGICAL_PATH,
                    &self.kit_path,
                    source,
                )
            })?;
        sync_exact_parent(
            self.runtime.fs(),
            DirectoryEndpoint::new(
                &self.kit_parent,
                Path::new("_kit"),
                &self.kit,
                &self.kit_path,
            ),
            &kit_observation,
            &path,
        )?;
        self.runtime
            .observe(TransitionKey::RemoveFinalizationLease {
                outcome,
                generation: lease.generation(),
                window: TransitionWindow::After,
            });
        Ok(())
    }

    fn publish_snapshot(
        &self,
        candidate: &JournalSnapshotV2,
    ) -> Result<RecordBindingV2, CodegenError> {
        let envelope = candidate
            .record_envelope_bytes()
            .map_err(model_error_at(&self.workspace_path))?;
        if envelope.len() as u64 > JOURNAL_FILE_LIMIT {
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.workspace_path.clone(),
                reason: "journal record exceeds the bounded immutable-record limit".to_owned(),
            });
        }
        let partial_name = candidate.partial_name();
        let partial_path = self.workspace_path.join(&partial_name);
        let record_name = candidate.record_name();
        let record_path = self.workspace_path.join(&record_name);
        let sequence = candidate.sequence();
        self.runtime.observe(TransitionKey::PrepareJournalPartial {
            sequence,
            window: TransitionWindow::Before,
        });
        let partial_observation = write_private_exact(
            self.runtime.fs(),
            &self.workspace,
            Path::new(&partial_name),
            &partial_path,
            &envelope,
        )?;
        let workspace_endpoint = DirectoryEndpoint::new(
            &self.kit,
            Path::new(
                self.workspace_path
                    .file_name()
                    .expect("workspace path has a leaf"),
            ),
            &self.workspace,
            &self.workspace_path,
        );
        let workspace_observation = self
            .runtime
            .fs()
            .observe_directory(workspace_endpoint)
            .map_err(|source| {
                transaction_io(
                    "inspect directory",
                    KIT_LOGICAL_PATH,
                    &self.workspace_path,
                    source,
                )
            })?;
        sync_exact_parent(
            self.runtime.fs(),
            workspace_endpoint,
            &workspace_observation,
            &partial_path,
        )?;
        self.runtime.observe(TransitionKey::PrepareJournalPartial {
            sequence,
            window: TransitionWindow::After,
        });

        let header = PartialEnvelopeHeaderV2::for_payload(
            candidate.transaction_id().clone(),
            candidate.project(),
            sequence,
            &candidate
                .to_json_bytes()
                .map_err(model_error_at(&partial_path))?,
        )
        .map_err(model_error_at(&partial_path))?;
        let partial_binding = PartialRecordBindingV2::new(
            candidate,
            exact_file(&partial_observation).map_err(model_error_at(&partial_path))?,
            header,
            &envelope,
        )
        .map_err(model_error_at(&partial_path))?;

        let transition = if matches!(candidate.phase(), JournalPhaseV2::CommitComplete { .. }) {
            TransitionKey::CommitBoundary {
                sequence,
                window: TransitionWindow::Before,
            }
        } else {
            TransitionKey::PublishJournalRecord {
                sequence,
                window: TransitionWindow::Before,
            }
        };
        self.runtime.observe(transition);
        let outcome = self.runtime.fs().publish_immutable(
            HardLinkEndpoint::new(&self.workspace, Path::new(&partial_name), &partial_path),
            &partial_observation,
            HardLinkEndpoint::new(&self.workspace, Path::new(&record_name), &record_path),
            workspace_endpoint,
            &workspace_observation,
            ParentSyncKind::Journal,
        );
        let published = match outcome {
            ImmutablePublicationOutcome::Durable { published } => published,
            ImmutablePublicationOutcome::NotPublished { source, .. }
            | ImmutablePublicationOutcome::VisibleDurabilityUnknown { source, .. }
            | ImmutablePublicationOutcome::DurableWithPartialResidual { source, .. } => {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: self.workspace_path.clone(),
                    reason: format!(
                        "immutable record {sequence} requires conservative recovery after publication: {source}"
                    ),
                });
            }
        };
        let record = candidate
            .expected_record_binding(
                exact_file(&published)
                    .map_err(model_error_at(&record_path))?
                    .identity(),
            )
            .map_err(model_error_at(&record_path))?;
        partial_binding
            .completed_record_binding(candidate)
            .map_err(model_error_at(&record_path))?;
        candidate
            .validate_record_binding(&record)
            .map_err(model_error_at(&record_path))?;
        let transition = if matches!(candidate.phase(), JournalPhaseV2::CommitComplete { .. }) {
            TransitionKey::CommitBoundary {
                sequence,
                window: TransitionWindow::After,
            }
        } else {
            TransitionKey::PublishJournalRecord {
                sequence,
                window: TransitionWindow::After,
            }
        };
        self.runtime.observe(transition);
        Ok(record)
    }
}

fn write_private_exact(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<ExactFileObservation, CodegenError> {
    let mut created = fs
        .create_new_file(parent, name, path, 0o600)
        .map_err(|source| transaction_io("create file", &name.to_string_lossy(), path, source))?;
    fs.set_file_mode(&created.file, path, 0o600)
        .map_err(|source| transaction_io("set mode", &name.to_string_lossy(), path, source))?;
    fs.write_handle(&mut created.file, path, bytes)
        .map_err(|source| transaction_io("write", &name.to_string_lossy(), path, source))?;
    fs.flush_file(&created.file, path)
        .map_err(|source| transaction_io("flush", &name.to_string_lossy(), path, source))?;
    fs.sync_handle(&created.file, path)
        .map_err(|source| transaction_io("sync", &name.to_string_lossy(), path, source))?;
    let observation = fs
        .observe_regular_file(parent, name, path)
        .map_err(|source| transaction_io("verify", &name.to_string_lossy(), path, source))?;
    if observation.identity != created.identity {
        return Err(CodegenError::RecoveryRequired {
            journal_path: path.to_path_buf(),
            reason: "exclusive transaction file changed identity before durable publication"
                .to_owned(),
        });
    }
    Ok(observation)
}

fn sync_exact_parent(
    fs: &dyn FsOps,
    endpoint: DirectoryEndpoint<'_>,
    expected: &ExactDirectoryObservation,
    mutation_path: &Path,
) -> Result<(), CodegenError> {
    fs.sync_parent(endpoint, expected, ParentSyncKind::Journal)
        .map_err(|source| transaction_io("sync parent directory", ".", mutation_path, source))
}

fn exact_directory_from_metadata(
    metadata: &Metadata,
) -> Result<ExactDirectoryStateV2, JournalModelError> {
    exact_directory(&ExactDirectoryObservation {
        identity: (MetadataExt::dev(metadata), MetadataExt::ino(metadata)),
        mode: preserved_mode(metadata),
        link_count: Some(MetadataExt::nlink(metadata)),
    })
}

pub(super) fn exact_existing_directory(
    context: &PlanningContext,
    fs: &dyn FsOps,
    logical_path: &str,
) -> Result<ExactDirectoryStateV2, CodegenError> {
    let (parent_path, name) = logical_path
        .rsplit_once('/')
        .map_or(("", logical_path), |(parent, name)| (parent, name));
    let parent = if parent_path.is_empty() {
        context.open_pinned_project_root()?
    } else {
        context.open_directory(parent_path)?
    };
    let directory = context.open_directory(logical_path)?;
    let native_path = context.project_root().join(logical_path);
    let observation = fs
        .observe_directory(DirectoryEndpoint::new(
            &parent,
            Path::new(name),
            &directory,
            &native_path,
        ))
        .map_err(|source| {
            transaction_io("inspect directory", logical_path, &native_path, source)
        })?;
    exact_directory(&observation).map_err(model_error_at(native_path))
}

#[cfg(unix)]
fn preserved_mode(metadata: &Metadata) -> PreservedFileMode {
    use cap_std::fs::PermissionsExt;
    PreservedFileMode {
        readonly: metadata.permissions().readonly(),
        posix_mode: Some(metadata.permissions().mode() & 0o7777),
    }
}

#[cfg(not(unix))]
fn preserved_mode(metadata: &Metadata) -> PreservedFileMode {
    PreservedFileMode {
        readonly: metadata.permissions().readonly(),
        posix_mode: None,
    }
}

#[cfg(unix)]
fn canonical_native_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn canonical_native_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn canonical_native_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(unix)]
const fn private_mode() -> Option<u32> {
    Some(0o600)
}

#[cfg(not(unix))]
const fn private_mode() -> Option<u32> {
    None
}

pub(super) fn model_error_at(
    path: impl Into<PathBuf>,
) -> impl FnOnce(JournalModelError) -> CodegenError {
    let path = path.into();
    move |error| CodegenError::RecoveryRequired {
        journal_path: path,
        reason: error.reason().to_owned(),
    }
}

pub(super) fn transaction_io(
    operation: &'static str,
    logical_path: &str,
    path: &Path,
    source: io::Error,
) -> CodegenError {
    CodegenError::FilesystemOperation {
        operation,
        logical_path: logical_path.to_owned(),
        path: path.to_path_buf(),
        source,
    }
}
