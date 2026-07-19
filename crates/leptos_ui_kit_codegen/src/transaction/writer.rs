use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_fs_ext::MetadataExt;
use cap_std::fs::{Dir, Metadata};

use crate::path_safety::{ObjectIdentity, PlanningContext};
use crate::{CodegenError, PreservedFileMode};

use super::authority::TransactionAuthority;
use super::fs::{
    DirectoryEndpoint, ExactDirectoryObservation, ExactFileObservation, ExclusiveCreateFailure,
    FsOps, HardLinkEndpoint, ParentSyncKind,
};
use super::journal::{
    ExactDirectoryStateV2, FinalizationLeaseV2, JournalDirectoryV2, JournalEntryV2,
    JournalModelError, JournalOperationV2, JournalSnapshotV2, ProjectBindingV2, RecordBindingV2,
    TransactionId, ValidatedJournalEnvelopeV2, WorkspaceBootstrapBindingV2,
    WorkspaceBootstrapEnvelopeV2, WorkspaceBootstrapIntentBindingV2,
    WorkspaceBootstrapIntentEnvelopeV2, bootstrap_intent_name, bootstrap_owner_name,
    canonical_root_hash, transaction_directory_name,
};
use super::lock::{DEFAULT_KIT_WRITE_LOCK_PATH, KIT_ADVISORY_LOCK_CONTENT, WriteLock};
use super::runtime::{
    EntropyPurpose, TransactionOutcome, TransactionRuntime, TransitionKey, TransitionWindow,
};
use super::store::{
    ActiveAdoptionSlot, ActiveJournalLoad, ActiveReconciliationDisposition,
    ExactRemovalDisposition, FinalizationPreparationDisposition, FinalizationRecord,
    JournalNamespace, JournalRecoveryStore, JournalStoreCapabilities, JournalStoreError,
    LoadedFinalization, RemovalReconciliation, SnapshotLinkDisposition,
    SnapshotPreparationDisposition, WorkspaceRemovalDisposition, exact_directory, exact_file,
};

const KIT_LOGICAL_PATH: &str = "src/components/ui/_kit/.transactions";
const KIT_PARENT_LOGICAL_PATH: &str = "src/components/ui/_kit";
const KIT_GRANDPARENT_LOGICAL_PATH: &str = "src/components/ui";

pub(super) struct ImmutableJournalStore<'a> {
    context: &'a PlanningContext,
    authority: TransactionAuthority<'a>,
    runtime: TransactionRuntime,
    project_root_path: PathBuf,
    project_root: ExactDirectoryObservation,
    held_write_lock_identity: ObjectIdentity,
    write_lock_path: PathBuf,
    kit_path: PathBuf,
    workspace_path: PathBuf,
    snapshot: JournalSnapshotV2,
    records: Vec<RecordBindingV2>,
}

impl<'a> ImmutableJournalStore<'a> {
    pub(super) fn resume(
        context: &'a PlanningContext,
        lock: &'a WriteLock,
        runtime: TransactionRuntime,
        snapshot: JournalSnapshotV2,
        records: Vec<RecordBindingV2>,
    ) -> Result<Self, CodegenError> {
        lock.validate_context(context)?;
        let root = context.open_pinned_project_root()?;
        let root_metadata = root.dir_metadata().map_err(|source| CodegenError::Io {
            path: context.project_root().to_path_buf(),
            source,
        })?;
        let kit = context.open_directory(KIT_LOGICAL_PATH)?;
        let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
        let workspace_name = transaction_directory_name(snapshot.transaction_id());
        if snapshot.project().workspace().name() != workspace_name {
            return Err(CodegenError::RecoveryRequired {
                journal_path: kit_path,
                reason: "loaded transaction has a non-canonical workspace name".to_owned(),
            });
        }
        let workspace_path = kit_path.join(&workspace_name);
        runtime
            .fs()
            .open_directory_exact(&kit, Path::new(&workspace_name), &workspace_path, 0o700)
            .map_err(|source| {
                transaction_io(
                    "open transaction workspace",
                    KIT_LOGICAL_PATH,
                    &workspace_path,
                    source,
                )
            })?;
        Ok(Self {
            context,
            authority: TransactionAuthority::new(context, lock),
            runtime,
            project_root_path: context.project_root().to_path_buf(),
            project_root: directory_observation(&root_metadata),
            held_write_lock_identity: lock.identity(),
            write_lock_path: context.project_root().join(DEFAULT_KIT_WRITE_LOCK_PATH),
            kit_path,
            workspace_path,
            snapshot,
            records,
        })
    }

    pub(super) fn create<F>(
        context: &'a PlanningContext,
        lock: &'a WriteLock,
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
        let authority = TransactionAuthority::new(context, lock);
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
        let kit = lock.open_or_create_transaction_namespace(context, runtime.fs())?;
        let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
        let kit_name = Path::new(".transactions");
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
        let root_observation = directory_observation(&root_metadata);
        let root_exact = exact_directory(&root_observation)
            .map_err(model_error_at(context.project_root().to_path_buf()))?;
        let root_hash = canonical_root_hash(&canonical_native_bytes(context.project_root()));

        let lock_path = context.project_root().join(DEFAULT_KIT_WRITE_LOCK_PATH);
        let lock_observation = runtime
            .fs()
            .observe_regular_file_bounded(
                &kit_parent,
                Path::new(".write.lock"),
                &lock_path,
                KIT_ADVISORY_LOCK_CONTENT.len() as u64,
            )
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
        let kit = authority.rebind_parent_for_mutation(
            runtime.fs(),
            KIT_LOGICAL_PATH,
            &kit_before,
            &intent_path,
        )?;
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
        let kit_after_intent =
            exact_directory(&kit_after_intent_observation).map_err(model_error_at(&kit_path))?;
        let kit = authority.rebind_parent_for_mutation(
            runtime.fs(),
            KIT_LOGICAL_PATH,
            &kit_after_intent,
            &intent_path,
        )?;
        let kit_parent = context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
        authority.validate_lock()?;
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
        let kit = authority.rebind_parent_for_mutation(
            runtime.fs(),
            KIT_LOGICAL_PATH,
            &kit_after_intent,
            &workspace_path,
        )?;
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
        let kit_after =
            exact_directory(&kit_after_observation).map_err(model_error_at(&kit_path))?;
        let kit = authority.rebind_parent_for_mutation(
            runtime.fs(),
            KIT_LOGICAL_PATH,
            &kit_after,
            &workspace_path,
        )?;
        let kit_parent = context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
        authority.validate_lock()?;
        sync_exact_parent(
            runtime.fs(),
            DirectoryEndpoint::new(&kit_parent, kit_name, &kit, &kit_path),
            &kit_after_observation,
            &workspace_path,
        )?;
        runtime.observe(TransitionKey::BootstrapWorkspace {
            window: TransitionWindow::After,
        });

        let kit_grandparent = context.open_directory(KIT_GRANDPARENT_LOGICAL_PATH)?;
        let coordination_parent_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
        let coordination_parent = runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &kit_grandparent,
                Path::new("_kit"),
                &kit_parent,
                &coordination_parent_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "inspect directory",
                    KIT_PARENT_LOGICAL_PATH,
                    &coordination_parent_path,
                    source,
                )
            })?;
        let project = ProjectBindingV2::new(
            &transaction_id,
            root_hash,
            root_exact,
            lock_exact,
            exact_directory(&coordination_parent)
                .map_err(model_error_at(&coordination_parent_path))?,
            kit_after_intent,
            kit_after.clone(),
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
        let kit = authority.rebind_parent_for_mutation(
            runtime.fs(),
            KIT_LOGICAL_PATH,
            &kit_after,
            &bootstrap_path,
        )?;
        let workspace_handle = runtime
            .fs()
            .open_directory_exact(
                &kit,
                Path::new(&workspace_name),
                &workspace_path,
                project
                    .workspace()
                    .exact()
                    .mode()
                    .posix_mode()
                    .unwrap_or(0o700),
            )
            .map_err(|source| {
                transaction_io(
                    "rebind transaction workspace",
                    &workspace_name,
                    &workspace_path,
                    source,
                )
            })?;
        if exact_directory(&workspace_handle.observation)
            .map_err(model_error_at(&workspace_path))?
            != *project.workspace().exact()
        {
            return Err(CodegenError::RecoveryRequired {
                journal_path: workspace_path.clone(),
                reason: "transaction workspace changed before bootstrap-owner creation".to_owned(),
            });
        }
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
        let workspace_after_bootstrap_exact =
            exact_directory(&workspace_after_bootstrap).map_err(model_error_at(&workspace_path))?;
        let kit = authority.rebind_parent_for_mutation(
            runtime.fs(),
            KIT_LOGICAL_PATH,
            &kit_after,
            &bootstrap_path,
        )?;
        let workspace_handle = runtime
            .fs()
            .open_directory_exact(
                &kit,
                Path::new(&workspace_name),
                &workspace_path,
                workspace_after_bootstrap_exact
                    .mode()
                    .posix_mode()
                    .unwrap_or(0o700),
            )
            .map_err(|source| {
                transaction_io(
                    "rebind transaction workspace",
                    &workspace_name,
                    &workspace_path,
                    source,
                )
            })?;
        if exact_directory(&workspace_handle.observation)
            .map_err(model_error_at(&workspace_path))?
            != workspace_after_bootstrap_exact
        {
            return Err(CodegenError::RecoveryRequired {
                journal_path: workspace_path.clone(),
                reason: "transaction workspace changed before bootstrap-owner durability sync"
                    .to_owned(),
            });
        }
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
            context,
            authority: TransactionAuthority::new(context, lock),
            runtime,
            project_root_path: context.project_root().to_path_buf(),
            project_root: root_observation,
            held_write_lock_identity: lock.identity(),
            write_lock_path: lock_path,
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
        self.authority.validate_lock()?;
        self.certify_active_finalization_slot()?;
        let lease = FinalizationLeaseV2::arm(&self.snapshot, self.records.clone(), None)
            .map_err(model_error_at(&self.workspace_path))?;
        let initial = self.publish_finalization_exact(None, &lease, outcome, true)?;

        self.certify_finalization_cleanup_stage(true)?;
        self.remove_exact_with_store("bootstrap intent", true, |store| {
            store
                .remove_bootstrap_intent(&lease, outcome)
                .map_err(strict_store_error)
        })?;
        self.certify_finalization_cleanup_stage(true)?;
        self.remove_exact_with_store("bootstrap owner", true, |store| {
            store
                .remove_bootstrap_owner(&lease, outcome)
                .map_err(strict_store_error)
        })?;
        if let Some(partial) = lease.partial() {
            self.certify_finalization_cleanup_stage(true)?;
            self.remove_exact_with_store("journal partial", true, |store| {
                store
                    .remove_journal_partial(partial, outcome)
                    .map_err(strict_store_error)
            })?;
        }
        for record in self.records.iter().rev() {
            self.certify_finalization_cleanup_stage(true)?;
            self.remove_exact_with_store("journal record", true, |store| {
                store
                    .remove_journal_record(record, outcome)
                    .map_err(strict_store_error)
            })?;
        }
        let expected_workspace = self.snapshot.project().workspace().exact();
        self.certify_finalization_cleanup_stage(true)?;
        let workspace_parent_after =
            self.with_strict_store(true, &self.workspace_path, |store| {
                store
                    .remove_workspace(expected_workspace, outcome)
                    .map_err(strict_store_error)
            })?;
        let workspace_parent_after = match workspace_parent_after {
            WorkspaceRemovalDisposition::Durable {
                workspace_parent_after,
            } => workspace_parent_after,
            WorkspaceRemovalDisposition::ReconcileRequired(reconciliation) => {
                return Err(self.removal_recovery("transaction workspace", &reconciliation));
            }
        };

        let closed = lease
            .mark_workspace_removed(workspace_parent_after)
            .map_err(model_error_at(&self.kit_path))?;
        let loaded = self.certify_finalization_cleanup_stage(false)?;
        let terminal = self.publish_finalization_exact(Some(&loaded), &closed, outcome, false)?;

        // Retire oldest-first so the closed workspace-removed generation is
        // always the last durable authority.  The strict loader accepts only
        // this exact lone generation-one suffix between the two removals.
        self.certify_finalization_cleanup_stage(false)?;
        self.remove_finalization_exact(&initial, outcome)?;
        let namespace = self.with_strict_store(false, &self.kit_path, |store| {
            store.inspect_namespace().map_err(strict_store_error)
        })?;
        let JournalNamespace::Finalizing(remaining) = namespace else {
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.kit_path.clone(),
                reason:
                    "closed finalization authority disappeared while retiring its oldest generation"
                        .to_owned(),
            });
        };
        if remaining.history() != std::slice::from_ref(&terminal) {
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.kit_path.clone(),
                reason: "finalization retirement did not leave the exact lone closed generation-one suffix"
                    .to_owned(),
            });
        }
        // If this last exact unlink completed but only the following
        // observation or sync failed, durable confirmed absence is terminal.
        self.certify_finalization_cleanup_stage(false)?;
        self.remove_finalization_exact(&terminal, outcome)
    }

    fn certify_active_finalization_slot(&self) -> Result<(), CodegenError> {
        let (loaded, certificate) =
            self.with_strict_store(true, &self.workspace_path, |store| {
                let ActiveJournalLoad::Stable(loaded) =
                    store.load_active().map_err(strict_store_error)?
                else {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: self.workspace_path.clone(),
                        reason: "active lineage requires reconciliation before finalization"
                            .to_owned(),
                    });
                };
                let (loaded, certificate) = store
                    .certify_active_adopted_publication(&loaded, ActiveAdoptionSlot::Finalize)
                    .map_err(strict_store_error)?;
                Ok((loaded, certificate))
            })?;
        self.with_strict_store(true, &self.workspace_path, |store| {
            store
                .authorize_active_adopted_publication(
                    &loaded,
                    &certificate,
                    &ActiveAdoptionSlot::Finalize,
                )
                .map_err(strict_store_error)
        })
    }

    fn certify_finalization_cleanup_stage(
        &self,
        workspace_present: bool,
    ) -> Result<LoadedFinalization, CodegenError> {
        self.with_strict_store(workspace_present, &self.kit_path, |store| {
            let JournalNamespace::Finalizing(loaded) =
                store.inspect_namespace().map_err(strict_store_error)?
            else {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: self.kit_path.clone(),
                    reason: "expected an exact finalization cleanup stage".to_owned(),
                });
            };
            if !matches!(
                loaded.reconciliation(),
                Some(super::store::FinalizationWorld::AdoptedPublished { .. })
            ) {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: self.kit_path.clone(),
                    reason: format!(
                        "finalization cleanup mutation requires an adopted-published stage, found {:?}",
                        loaded.reconciliation()
                    ),
                });
            }
            store
                .certify_finalization_world(&loaded)
                .map_err(strict_store_error)?;
            Ok(loaded)
        })
    }

    pub(super) fn rebind_parent_for_mutation(
        &self,
        logical_parent: &str,
        expected_parent: &ExactDirectoryStateV2,
        mutation_path: &Path,
    ) -> Result<Dir, CodegenError> {
        self.authority.rebind_parent_for_mutation(
            self.runtime.fs(),
            logical_parent,
            expected_parent,
            mutation_path,
        )
    }

    fn with_strict_store<T>(
        &self,
        workspace_present: bool,
        mutation_path: &Path,
        action: impl FnOnce(&JournalRecoveryStore<'_>) -> Result<T, CodegenError>,
    ) -> Result<T, CodegenError> {
        let expected_kit = self.snapshot.project().workspace_parent_current();
        let kit = self.authority.rebind_parent_for_mutation(
            self.runtime.fs(),
            KIT_LOGICAL_PATH,
            expected_kit,
            mutation_path,
        )?;
        let kit_parent = self.context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
        self.authority.validate_lock()?;

        let workspace_name = self
            .workspace_path
            .file_name()
            .expect("workspace path has a leaf");
        let workspace = if workspace_present {
            let expected = self.snapshot.project().workspace().exact();
            let opened = self
                .runtime
                .fs()
                .open_directory_exact(
                    &kit,
                    Path::new(workspace_name),
                    &self.workspace_path,
                    expected.mode().posix_mode().unwrap_or(0o700),
                )
                .map_err(|source| {
                    transaction_io(
                        "reopen exact transaction workspace",
                        KIT_LOGICAL_PATH,
                        &self.workspace_path,
                        source,
                    )
                })?;
            let actual = exact_directory(&opened.observation)
                .map_err(model_error_at(&self.workspace_path))?;
            if &actual != expected {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: self.workspace_path.clone(),
                    reason: "transaction workspace changed before a journal-store mutation"
                        .to_owned(),
                });
            }
            Some(opened)
        } else {
            None
        };
        self.authority.validate_lock()?;

        let write_lock =
            HardLinkEndpoint::new(&kit_parent, Path::new(".write.lock"), &self.write_lock_path);
        let workspace_parent = DirectoryEndpoint::new(
            &kit_parent,
            Path::new(".transactions"),
            &kit,
            &self.kit_path,
        );
        let capabilities = match workspace.as_ref() {
            Some(workspace) => JournalStoreCapabilities::active(
                &self.project_root_path,
                self.project_root,
                self.held_write_lock_identity,
                write_lock,
                workspace_parent,
                DirectoryEndpoint::new(
                    &kit,
                    Path::new(workspace_name),
                    &workspace.directory,
                    &self.workspace_path,
                ),
            ),
            None => JournalStoreCapabilities::finalization_only(
                &self.project_root_path,
                self.project_root,
                self.held_write_lock_identity,
                write_lock,
                workspace_parent,
            ),
        };
        let store = JournalRecoveryStore::bind(
            &self.runtime,
            self.snapshot.transaction_id().clone(),
            self.snapshot.project().canonical_root_hash().clone(),
            capabilities,
        )
        .map_err(strict_store_error)?;
        action(&store)
    }

    fn publish_finalization_exact(
        &self,
        previous: Option<&LoadedFinalization>,
        candidate: &FinalizationLeaseV2,
        outcome: TransactionOutcome,
        workspace_present: bool,
    ) -> Result<FinalizationRecord, CodegenError> {
        let mutation_path = self.kit_path.join(candidate.record_name());
        self.with_strict_store(
            workspace_present,
            &mutation_path,
            |store| {
                match store
                    .prepare_finalization_publication(previous, candidate)
                    .map_err(strict_store_error)?
                {
                    FinalizationPreparationDisposition::Durable => Ok(()),
                    FinalizationPreparationDisposition::ReconcileRequired { reconciliation } => {
                        Err(CodegenError::RecoveryRequired {
                            journal_path: self.kit_path.clone(),
                            reason: format!(
                                "finalization partial preparation requires exact recovery after {:?}: {} ({:?})",
                                reconciliation.mutation(),
                                reconciliation.source(),
                                reconciliation.world(),
                            ),
                        })
                    }
                }
            },
        )?;
        let prepared = self.with_strict_store(workspace_present, &mutation_path, |store| {
            match store.inspect_namespace().map_err(strict_store_error)? {
                JournalNamespace::Finalizing(loaded)
                    if loaded
                        .partial()
                        .is_some_and(|partial| partial.lease() == candidate) =>
                {
                    Ok(loaded)
                }
                _ => Err(CodegenError::RecoveryRequired {
                    journal_path: self.kit_path.clone(),
                    reason: "prepared finalization partial did not survive strict rediscovery"
                        .to_owned(),
                }),
            }
        })?;
        self.with_strict_store(workspace_present, &mutation_path, |store| {
            store
                .link_finalization_publication(&prepared)
                .map_err(strict_store_error)
        })?;
        let linked =
            self.with_strict_store(workspace_present, &mutation_path, |store| {
                match store.inspect_namespace().map_err(strict_store_error)? {
                    JournalNamespace::Finalizing(loaded)
                        if matches!(
                            loaded.reconciliation(),
                            Some(super::store::FinalizationWorld::LinkedAliases { generation })
                                if *generation == candidate.generation()
                        ) =>
                    {
                        Ok(loaded)
                    }
                    _ => Err(CodegenError::RecoveryRequired {
                        journal_path: self.kit_path.clone(),
                        reason:
                            "linked finalization publication did not survive strict rediscovery"
                                .to_owned(),
                    }),
                }
            })?;
        self.with_strict_store(workspace_present, &mutation_path, |store| {
            store
                .certify_finalization_publication(&linked)
                .map_err(strict_store_error)
        })?;
        let certified = self.with_strict_store(workspace_present, &mutation_path, |store| {
            match store.inspect_namespace().map_err(strict_store_error)? {
                JournalNamespace::Finalizing(loaded) if loaded == linked => Ok(loaded),
                _ => Err(CodegenError::RecoveryRequired {
                    journal_path: self.kit_path.clone(),
                    reason: "certified finalization aliases changed on immediate rediscovery"
                        .to_owned(),
                }),
            }
        })?;
        let partial =
            certified
                .partial()
                .cloned()
                .ok_or_else(|| CodegenError::RecoveryRequired {
                    journal_path: self.kit_path.clone(),
                    reason: "certified finalization publication has no partial alias".to_owned(),
                })?;
        self.remove_exact_with_store("finalization partial", workspace_present, |store| {
            store
                .remove_finalization_partial(&partial, outcome)
                .map_err(strict_store_error)
        })?;
        let published = self.with_strict_store(workspace_present, &mutation_path, |store| {
            match store.inspect_namespace().map_err(strict_store_error)? {
                JournalNamespace::Finalizing(loaded) => Ok(loaded),
                _ => Err(CodegenError::RecoveryRequired {
                    journal_path: self.kit_path.clone(),
                    reason: "finalization publication disappeared after partial retirement"
                        .to_owned(),
                }),
            }
        })?;
        published
            .history()
            .iter()
            .find(|record| record.lease() == candidate)
            .cloned()
            .ok_or_else(|| CodegenError::RecoveryRequired {
                journal_path: self.kit_path.clone(),
                reason: format!(
                    "finalization for {outcome:?} did not reload the exact published lease"
                ),
            })
    }

    fn remove_exact_with_store(
        &self,
        label: &str,
        workspace_present: bool,
        action: impl FnOnce(&JournalRecoveryStore<'_>) -> Result<ExactRemovalDisposition, CodegenError>,
    ) -> Result<(), CodegenError> {
        let disposition =
            self.with_strict_store(workspace_present, &self.workspace_path, action)?;
        match disposition {
            ExactRemovalDisposition::DurableAbsent => Ok(()),
            ExactRemovalDisposition::ReconcileRequired(reconciliation) => {
                Err(self.removal_recovery(label, &reconciliation))
            }
        }
    }

    fn remove_finalization_exact(
        &self,
        record: &FinalizationRecord,
        outcome: TransactionOutcome,
    ) -> Result<(), CodegenError> {
        let disposition =
            self.with_strict_store(false, &self.kit_path.join(record.name()), |store| {
                store
                    .remove_finalization_record(record, outcome)
                    .map_err(strict_store_error)
            })?;
        match disposition {
            ExactRemovalDisposition::DurableAbsent => Ok(()),
            ExactRemovalDisposition::ReconcileRequired(reconciliation) => {
                Err(self.removal_recovery("finalization record", &reconciliation))
            }
        }
    }

    fn removal_recovery(
        &self,
        label: &str,
        reconciliation: &RemovalReconciliation,
    ) -> CodegenError {
        CodegenError::RecoveryRequired {
            journal_path: self.kit_path.clone(),
            reason: format!("{label} removal requires exact reconciliation: {reconciliation:?}"),
        }
    }

    fn publish_snapshot(
        &self,
        candidate: &JournalSnapshotV2,
    ) -> Result<RecordBindingV2, CodegenError> {
        let expected_envelope = Arc::new(
            ValidatedJournalEnvelopeV2::from_snapshot(candidate.clone())
                .map_err(model_error_at(&self.workspace_path))?,
        );
        self.runtime
            .cache_journal_envelope_name(candidate.partial_name(), Arc::clone(&expected_envelope));
        self.runtime
            .cache_journal_envelope_name(candidate.record_name(), expected_envelope);
        let adoption_slot =
            ActiveAdoptionSlot::append(candidate).map_err(model_error_at(&self.workspace_path))?;
        let (certified_loaded, adopted) =
            self.with_strict_store(true, &self.workspace_path, |store| {
            let loaded = match store.load_active().map_err(strict_store_error)? {
                ActiveJournalLoad::Stable(loaded) => loaded,
                ActiveJournalLoad::ReconciliationRequired(reconciliation) => {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: self.workspace_path.clone(),
                        reason: format!(
                            "journal sequence {} must be reconciled before a successor is appended ({:?})",
                            reconciliation.sequence(),
                            reconciliation.world(),
                        ),
                    });
                }
            };
            if loaded.latest().is_some() {
                let (loaded, adopted) = store
                    .certify_active_adopted_publication(&loaded, adoption_slot.clone())
                    .map_err(strict_store_error)?;
                Ok((loaded, Some(adopted)))
            } else {
                Ok((loaded, None))
            }
        })?;
        let preparation = self.with_strict_store(true, &self.workspace_path, |store| {
            store
                .prepare_snapshot_publication(&certified_loaded, candidate, adopted.as_ref())
                .map_err(strict_store_error)
        })?;
        let prepared = match preparation {
            SnapshotPreparationDisposition::Durable { prepared } => prepared,
            SnapshotPreparationDisposition::ReconcileRequired { reconciliation } => {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: self.workspace_path.clone(),
                    reason: format!(
                        "journal partial preparation at {:?} requires exact recovery after {:?}: {} ({:?})",
                        reconciliation.boundary(),
                        reconciliation.mutation(),
                        reconciliation.source(),
                        reconciliation.world(),
                    ),
                });
            }
        };
        let linking = self.with_strict_store(true, &self.workspace_path, |store| {
            store
                .link_snapshot_publication(&prepared, candidate)
                .map_err(strict_store_error)
        })?;
        let SnapshotLinkDisposition::Linked {
            reconciliation: linked,
        } = linking
        else {
            let SnapshotLinkDisposition::ReconcileRequired { reconciliation } = linking else {
                unreachable!("snapshot link disposition is closed")
            };
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.workspace_path.clone(),
                reason: format!(
                    "journal publication at {:?} requires exact recovery after {:?}: {} ({:?})",
                    reconciliation.boundary(),
                    reconciliation.mutation(),
                    reconciliation.source(),
                    reconciliation.world(),
                ),
            });
        };
        self.with_strict_store(true, &self.workspace_path, |store| {
            store
                .certify_active_publication(&linked)
                .map_err(strict_store_error)
        })?;
        let retired = self.with_strict_store(true, &self.workspace_path, |store| {
            store
                .retire_active_publication_partial(&linked)
                .map_err(strict_store_error)
        })?;
        match retired {
            ActiveReconciliationDisposition::Durable { loaded }
                if loaded.latest() == Some(candidate) && loaded.partial().is_none() =>
            {
                loaded
                    .records()
                    .last()
                    .cloned()
                    .ok_or_else(|| CodegenError::RecoveryRequired {
                        journal_path: self.workspace_path.clone(),
                        reason: "published journal lineage has no final record binding".to_owned(),
                    })
            }
            ActiveReconciliationDisposition::Durable { .. } => {
                Err(CodegenError::RecoveryRequired {
                    journal_path: self.workspace_path.clone(),
                    reason:
                        "retired journal partial did not recapture as the exact published lineage"
                            .to_owned(),
                })
            }
            ActiveReconciliationDisposition::ReconcileRequired { reconciliation } => {
                Err(CodegenError::RecoveryRequired {
                    journal_path: self.workspace_path.clone(),
                    reason: format!(
                        "certified journal partial retirement requires exact recovery after {:?}: {} ({:?})",
                        reconciliation.mutation(),
                        reconciliation.source(),
                        reconciliation.world(),
                    ),
                })
            }
        }
    }
}

fn write_private_exact(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<ExactFileObservation, CodegenError> {
    let mut created = match fs
        .create_new_file(parent, name, path, 0o600)
        .bind_empty(fs, parent, name, path)
    {
        Ok(created) => created,
        Err(ExclusiveCreateFailure::NotCreated(source)) => {
            return Err(transaction_io(
                "create file",
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
                    "exclusive transaction-file creation changed the namespace but its live owner \
                     capability could not be rebound: {source}"
                ),
            });
        }
    };
    fs.set_file_mode(&created.file, path, 0o600)
        .map_err(|source| transaction_io("set mode", &name.to_string_lossy(), path, source))?;
    fs.write_handle(&mut created.file, path, bytes)
        .map_err(|source| transaction_io("write", &name.to_string_lossy(), path, source))?;
    fs.flush_file(&created.file, path)
        .map_err(|source| transaction_io("flush", &name.to_string_lossy(), path, source))?;
    fs.sync_handle(&created.file, path)
        .map_err(|source| transaction_io("sync", &name.to_string_lossy(), path, source))?;
    fs.observe_created_file_exact(parent, name, path, &mut created, bytes.len() as u64)
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: path.to_path_buf(),
            reason: format!(
                "exclusive transaction file could not be verified through its live owner handle \
                 after durable population: {source}"
            ),
        })
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

fn directory_observation(metadata: &Metadata) -> ExactDirectoryObservation {
    ExactDirectoryObservation {
        identity: ObjectIdentity::from_u64(MetadataExt::dev(metadata), MetadataExt::ino(metadata)),
        mode: preserved_mode(metadata),
        link_count: Some(MetadataExt::nlink(metadata)),
    }
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

fn strict_store_error(error: JournalStoreError) -> CodegenError {
    CodegenError::RecoveryRequired {
        journal_path: error.path().to_path_buf(),
        reason: error.reason(),
    }
}
