use std::{
    io,
    path::{Path, PathBuf},
};

use cap_fs_ext::MetadataExt;
use cap_std::fs::{Dir, Metadata};

use crate::path_safety::PlanningContext;
use crate::{CodegenError, PreservedFileMode};

use super::authority::TransactionAuthority;
use super::fs::{
    DirectoryEndpoint, ExactDirectoryObservation, ExactFileObservation, ExactObjectIdentity,
    ExactRelocationSource, FsOps, HardLinkEndpoint, ParentSyncKind,
};
use super::journal::{
    ExactDirectoryStateV2, FinalizationLeaseV2, JournalDirectoryV2, JournalEntryV2,
    JournalModelError, JournalOperationV2, JournalSnapshotV2, ProjectBindingV2, RecordBindingV2,
    TransactionId, WorkspaceBootstrapBindingV2, WorkspaceBootstrapEnvelopeV2,
    WorkspaceBootstrapIntentBindingV2, WorkspaceBootstrapIntentEnvelopeV2, bootstrap_intent_name,
    bootstrap_owner_name, canonical_root_hash, retirement_authority_name,
    retirement_namespace_name, transaction_directory_name,
};
use super::lock::{DEFAULT_KIT_WRITE_LOCK_PATH, WriteLock};
use super::runtime::{
    EntropyPurpose, TransactionOutcome, TransactionRuntime, TransitionKey, TransitionWindow,
};
use super::store::{
    ExactRemovalDisposition, FinalizationAdoptionDisposition, FinalizationDisposition,
    FinalizationRecord, JournalNamespace, JournalRecoveryStore, JournalStoreCapabilities,
    JournalStoreError, LoadedFinalization, PublicationDisposition, RemovalReconciliation,
    RemovalWorld, WorkspaceRemovalDisposition, exact_directory, exact_file,
};

const KIT_LOGICAL_PATH: &str = "src/components/ui/_kit/.transactions";
const KIT_PARENT_LOGICAL_PATH: &str = "src/components/ui/_kit";

pub(super) struct ImmutableJournalStore<'a> {
    context: &'a PlanningContext,
    authority: TransactionAuthority<'a>,
    runtime: TransactionRuntime,
    project_root_path: PathBuf,
    project_root: ExactDirectoryObservation,
    held_coordination_parent_identity: ExactObjectIdentity,
    held_write_lock_identity: ExactObjectIdentity,
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
        let _workspace = runtime
            .fs()
            .open_directory_exact(&kit, Path::new(&workspace_name), &workspace_path, 0o700)
            .map_err(|source| {
                transaction_io(
                    "open transaction workspace",
                    KIT_LOGICAL_PATH,
                    &workspace_path,
                    source,
                )
            })?
            .directory;
        Ok(Self {
            context,
            authority: TransactionAuthority::new(context, lock),
            runtime,
            project_root_path: context.project_root().to_path_buf(),
            project_root: directory_observation(&root_metadata),
            held_coordination_parent_identity: lock.coordination_parent_identity(),
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
        let coordination_grandparent = context.open_directory("src/components/ui")?;
        let coordination_parent_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
        let coordination_parent_observation = runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &coordination_grandparent,
                Path::new("_kit"),
                &kit_parent,
                &coordination_parent_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "inspect coordination parent",
                    KIT_PARENT_LOGICAL_PATH,
                    &coordination_parent_path,
                    source,
                )
            })?;
        let coordination_parent_exact = exact_directory(&coordination_parent_observation)
            .map_err(model_error_at(&coordination_parent_path))?;
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
            .observe_regular_file(&kit_parent, Path::new(".write.lock"), &lock_path)
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
            root_exact.clone(),
            coordination_parent_exact.clone(),
            lock_exact.clone(),
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

        let project = ProjectBindingV2::new(
            &transaction_id,
            root_hash,
            root_exact,
            coordination_parent_exact,
            lock_exact,
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
            held_coordination_parent_identity: lock.coordination_parent_identity(),
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
        self.authority.validate_lock()?;
        let lease = FinalizationLeaseV2::arm(&self.snapshot, self.records.clone(), None)
            .map_err(model_error_at(&self.workspace_path))?;
        let initial = self.publish_finalization_exact(None, &lease, outcome, true)?;

        self.runtime
            .observe(TransitionKey::RemoveWorkspaceOwnership {
                outcome,
                window: TransitionWindow::Before,
            });
        self.remove_exact_with_store("bootstrap intent", true, |store| {
            store
                .remove_bootstrap_intent(&lease, outcome)
                .map_err(strict_store_error)
        })?;
        self.remove_exact_with_store("bootstrap owner", true, |store| {
            store
                .remove_bootstrap_owner(&lease, outcome)
                .map_err(strict_store_error)
        })?;
        if let Some(partial) = lease.partial() {
            self.remove_exact_with_store("journal partial", true, |store| {
                store
                    .remove_journal_partial(partial, outcome)
                    .map_err(strict_store_error)
            })?;
        }
        for record in self.records.iter().rev() {
            self.remove_exact_with_store("journal record", true, |store| {
                store
                    .remove_journal_record(record, outcome)
                    .map_err(strict_store_error)
            })?;
        }
        self.runtime
            .observe(TransitionKey::RemoveWorkspaceOwnership {
                outcome,
                window: TransitionWindow::After,
            });

        let expected_workspace = self.snapshot.project().workspace().exact();
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
        let loaded = self.with_strict_store(false, &self.kit_path, |store| {
            store.inspect_namespace().map_err(strict_store_error)
        })?;
        let JournalNamespace::Finalizing(loaded) = loaded else {
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.kit_path.clone(),
                reason: "generation-zero finalization authority disappeared before its workspace-removed successor"
                    .to_owned(),
            });
        };
        let terminal = self.publish_finalization_exact(Some(&loaded), &closed, outcome, false)?;

        // Retire oldest-first so the closed workspace-removed generation is
        // always the last durable authority.  The strict loader accepts only
        // this exact lone generation-one suffix between the two removals.
        self.remove_finalization_exact(&initial, outcome, false)?;
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
        self.retire_terminal_namespace(&terminal, outcome)
    }

    fn retire_terminal_namespace(
        &self,
        terminal: &FinalizationRecord,
        outcome: TransactionOutcome,
    ) -> Result<(), CodegenError> {
        let alias_name = retirement_authority_name(terminal.lease().transaction_id());
        let alias_path = self
            .context
            .project_root()
            .join(KIT_PARENT_LOGICAL_PATH)
            .join(&alias_name);
        let tombstone_name = terminal.name();
        let tombstone_path = self.kit_path.join(tombstone_name);

        self.authority.validate_lock()?;
        let transactions = self.context.open_directory(KIT_LOGICAL_PATH)?;
        let coordination_parent = self.context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
        let tombstone = self
            .runtime
            .fs()
            .observe_regular_file(&transactions, Path::new(tombstone_name), &tombstone_path)
            .map_err(|source| {
                transaction_io(
                    "inspect terminal finalization lease",
                    tombstone_name,
                    &tombstone_path,
                    source,
                )
            })?;
        if exact_file(&tombstone).map_err(model_error_at(&tombstone_path))? != *terminal.exact() {
            return Err(CodegenError::RecoveryRequired {
                journal_path: tombstone_path,
                reason: "terminal finalization lease changed before retirement authority was armed"
                    .to_owned(),
            });
        }

        self.runtime.observe(TransitionKey::ArmRetirementAuthority {
            outcome,
            window: TransitionWindow::Before,
        });
        self.runtime
            .fs()
            .hard_link(
                &[],
                HardLinkEndpoint::new(&transactions, Path::new(tombstone_name), &tombstone_path),
                HardLinkEndpoint::new(&coordination_parent, Path::new(&alias_name), &alias_path),
            )
            .map_err(|source| {
                transaction_io(
                    "arm terminal retirement authority",
                    &alias_name,
                    &alias_path,
                    source,
                )
            })?;
        let alias =
            self.observe_retirement_alias(&coordination_parent, &alias_name, &alias_path)?;
        let linked_tombstone = self
            .runtime
            .fs()
            .observe_regular_file(&transactions, Path::new(tombstone_name), &tombstone_path)
            .map_err(|source| {
                transaction_io(
                    "reinspect terminal finalization lease",
                    tombstone_name,
                    &tombstone_path,
                    source,
                )
            })?;
        if alias.identity != linked_tombstone.identity
            || alias.content_hash != linked_tombstone.content_hash
            || alias.byte_len != linked_tombstone.byte_len
            || alias.mode != linked_tombstone.mode
            || alias.link_count != Some(2)
            || linked_tombstone.link_count != Some(2)
        {
            return Err(CodegenError::RecoveryRequired {
                journal_path: alias_path,
                reason: "terminal retirement authority is not the exact two-link tombstone alias"
                    .to_owned(),
            });
        }
        self.sync_coordination_parent(&coordination_parent, &alias_path)?;
        self.runtime.observe(TransitionKey::ArmRetirementAuthority {
            outcome,
            window: TransitionWindow::After,
        });

        let transactions_observation =
            self.observe_transactions(&coordination_parent, &transactions)?;
        let transactions_endpoint = DirectoryEndpoint::new(
            &coordination_parent,
            Path::new(".transactions"),
            &transactions,
            &self.kit_path,
        );
        let transactions_inventory = self
            .runtime
            .fs()
            .inventory_directory_exact_bounded(transactions_endpoint, &transactions_observation, 1)
            .map_err(|source| {
                transaction_io(
                    "inventory terminal transaction namespace",
                    KIT_LOGICAL_PATH,
                    &self.kit_path,
                    source,
                )
            })?;
        if transactions_inventory.entries.len() != 1
            || transactions_inventory.entries[0].name != tombstone_name
            || transactions_inventory.entries[0].identity != linked_tombstone.identity
        {
            return Err(CodegenError::RecoveryRequired {
                journal_path: self.kit_path.clone(),
                reason: "terminal transaction namespace does not contain only its exact finalization lease"
                    .to_owned(),
            });
        }
        let retirement_name = retirement_namespace_name(terminal.lease().transaction_id());
        let retirement_path = self
            .context
            .project_root()
            .join(KIT_PARENT_LOGICAL_PATH)
            .join(&retirement_name);
        self.runtime
            .observe(TransitionKey::MoveTransactionNamespaceToRetirement {
                outcome,
                window: TransitionWindow::Before,
            });
        self.runtime
            .fs()
            .relocate_noreplace(
                &coordination_parent,
                Path::new(".transactions"),
                &self.kit_path,
                &coordination_parent,
                Path::new(&retirement_name),
                &retirement_path,
                &ExactRelocationSource::Directory(transactions_inventory.clone()),
            )
            .map_err(|error| {
                transaction_io(
                    "move transaction namespace to retirement",
                    KIT_LOGICAL_PATH,
                    &retirement_path,
                    error.into_io(),
                )
            })?;
        self.sync_coordination_parent(&coordination_parent, &retirement_path)?;
        let retirement_endpoint = DirectoryEndpoint::new(
            &coordination_parent,
            Path::new(&retirement_name),
            &transactions,
            &retirement_path,
        );
        let retirement_observation = self
            .runtime
            .fs()
            .observe_directory(retirement_endpoint)
            .map_err(|source| {
                transaction_io(
                    "inspect retiring transaction namespace",
                    &retirement_name,
                    &retirement_path,
                    source,
                )
            })?;
        let retirement_inventory = self
            .runtime
            .fs()
            .inventory_directory_exact_bounded(retirement_endpoint, &retirement_observation, 1)
            .map_err(|source| {
                transaction_io(
                    "inventory retiring transaction namespace",
                    &retirement_name,
                    &retirement_path,
                    source,
                )
            })?;
        if retirement_inventory != transactions_inventory {
            return Err(CodegenError::RecoveryRequired {
                journal_path: retirement_path.clone(),
                reason: "retirement move did not preserve the exact transaction namespace"
                    .to_owned(),
            });
        }
        self.runtime
            .observe(TransitionKey::MoveTransactionNamespaceToRetirement {
                outcome,
                window: TransitionWindow::After,
            });

        let retiring_tombstone_path = retirement_path.join(tombstone_name);

        self.runtime
            .observe(TransitionKey::RemoveFinalizationLease {
                outcome,
                generation: terminal.lease().generation(),
                window: TransitionWindow::Before,
            });
        self.runtime
            .fs()
            .remove_file_exact(
                &transactions,
                Path::new(tombstone_name),
                &retiring_tombstone_path,
                &linked_tombstone,
            )
            .map_err(|source| {
                transaction_io(
                    "remove terminal finalization lease",
                    tombstone_name,
                    &retiring_tombstone_path,
                    source,
                )
            })?;
        let transactions_observation = self
            .runtime
            .fs()
            .observe_directory(retirement_endpoint)
            .map_err(|source| {
                transaction_io(
                    "reinspect retiring transaction namespace",
                    &retirement_name,
                    &retirement_path,
                    source,
                )
            })?;
        sync_exact_parent(
            self.runtime.fs(),
            retirement_endpoint,
            &transactions_observation,
            &retiring_tombstone_path,
        )?;
        self.runtime
            .observe(TransitionKey::RemoveFinalizationLease {
                outcome,
                generation: terminal.lease().generation(),
                window: TransitionWindow::After,
            });

        let inventory = self
            .runtime
            .fs()
            .inventory_directory_exact(retirement_endpoint, &transactions_observation)
            .map_err(|source| {
                transaction_io(
                    "inventory retiring transaction namespace",
                    &retirement_name,
                    &retirement_path,
                    source,
                )
            })?;
        if !inventory.entries.is_empty()
            || exact_directory(&transactions_observation)
                .map_err(model_error_at(&retirement_path))?
                != *terminal.lease().workspace_parent_current()
        {
            return Err(CodegenError::RecoveryRequired {
                journal_path: retirement_path.clone(),
                reason: "retiring transaction namespace is not the expected exact empty identity"
                    .to_owned(),
            });
        }
        self.runtime
            .observe(TransitionKey::RetireTransactionNamespace {
                outcome,
                window: TransitionWindow::Before,
            });
        self.runtime
            .fs()
            .remove_empty_directory_exact(retirement_endpoint, &transactions_observation)
            .map_err(|source| {
                transaction_io(
                    "retire transaction namespace",
                    &retirement_name,
                    &retirement_path,
                    source,
                )
            })?;
        self.sync_coordination_parent(&coordination_parent, &retirement_path)?;
        for (name, path) in [
            (".transactions", &self.kit_path),
            (retirement_name.as_str(), &retirement_path),
        ] {
            match coordination_parent.symlink_metadata(name) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: path.clone(),
                        reason: "a transaction namespace remained visible after exact retirement"
                            .to_owned(),
                    });
                }
                Err(source) => {
                    return Err(CodegenError::Io {
                        path: path.clone(),
                        source,
                    });
                }
            }
        }
        self.runtime
            .observe(TransitionKey::RetireTransactionNamespace {
                outcome,
                window: TransitionWindow::After,
            });

        let coordination_parent = self.context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
        let alias =
            self.observe_retirement_alias(&coordination_parent, &alias_name, &alias_path)?;
        if alias.identity != linked_tombstone.identity || alias.link_count != Some(1) {
            return Err(CodegenError::RecoveryRequired {
                journal_path: alias_path.clone(),
                reason: "terminal retirement authority changed before its exact removal".to_owned(),
            });
        }
        self.runtime
            .observe(TransitionKey::RemoveRetirementAuthority {
                outcome,
                window: TransitionWindow::Before,
            });
        self.runtime
            .fs()
            .remove_file_exact(
                &coordination_parent,
                Path::new(&alias_name),
                &alias_path,
                &alias,
            )
            .map_err(|source| {
                transaction_io(
                    "remove terminal retirement authority",
                    &alias_name,
                    &alias_path,
                    source,
                )
            })?;
        self.sync_coordination_parent(&coordination_parent, &alias_path)?;
        self.runtime
            .observe(TransitionKey::RemoveRetirementAuthority {
                outcome,
                window: TransitionWindow::After,
            });
        Ok(())
    }

    fn observe_transactions(
        &self,
        coordination_parent: &Dir,
        transactions: &Dir,
    ) -> Result<ExactDirectoryObservation, CodegenError> {
        self.runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                coordination_parent,
                Path::new(".transactions"),
                transactions,
                &self.kit_path,
            ))
            .map_err(|source| {
                transaction_io(
                    "inspect transaction namespace",
                    KIT_LOGICAL_PATH,
                    &self.kit_path,
                    source,
                )
            })
    }

    fn observe_retirement_alias(
        &self,
        coordination_parent: &Dir,
        name: &str,
        path: &Path,
    ) -> Result<ExactFileObservation, CodegenError> {
        self.runtime
            .fs()
            .observe_regular_file(coordination_parent, Path::new(name), path)
            .map_err(|source| {
                transaction_io("inspect terminal retirement authority", name, path, source)
            })
    }

    fn sync_coordination_parent(
        &self,
        coordination_parent: &Dir,
        mutation_path: &Path,
    ) -> Result<(), CodegenError> {
        self.authority.validate_lock()?;
        self.runtime
            .fs()
            .sync_directory(coordination_parent, mutation_path)
            .map_err(|source| {
                transaction_io(
                    "sync coordination parent",
                    KIT_PARENT_LOGICAL_PATH,
                    mutation_path,
                    source,
                )
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
                self.held_coordination_parent_identity,
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
                self.held_coordination_parent_identity,
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
        let disposition = self.with_strict_store(
            workspace_present,
            &self.kit_path.join(candidate.record_name()),
            |store| {
                store
                    .publish_finalization(previous, candidate)
                    .map_err(strict_store_error)
            },
        )?;
        match disposition {
            FinalizationDisposition::Durable { record } => Ok(record),
            FinalizationDisposition::DurableResidual { .. }
            | FinalizationDisposition::ReconcileRequired { .. } => {
                let adoption = self.with_strict_store(
                    workspace_present,
                    &self.kit_path.join(candidate.record_name()),
                    |store| {
                        let namespace = store.inspect_namespace().map_err(strict_store_error)?;
                        let JournalNamespace::Finalizing(loaded) = namespace else {
                            return Err(CodegenError::RecoveryRequired {
                                journal_path: self.kit_path.clone(),
                                reason:
                                    "linked finalization publication disappeared before adoption"
                                        .to_owned(),
                            });
                        };
                        store
                            .adopt_finalization_publication(&loaded, outcome)
                            .map_err(strict_store_error)
                    },
                )?;
                match adoption {
                    FinalizationAdoptionDisposition::Durable { loaded, record }
                        if record.lease() == candidate
                            && loaded.history().iter().any(|current| current == &record) =>
                    {
                        Ok(record)
                    }
                    FinalizationAdoptionDisposition::Durable { .. } => {
                        Err(CodegenError::RecoveryRequired {
                            journal_path: self.kit_path.clone(),
                            reason: "adopted finalization publication reloaded a different lease"
                                .to_owned(),
                        })
                    }
                    FinalizationAdoptionDisposition::ReconcileRequired { reconciliation } => {
                        Err(CodegenError::RecoveryRequired {
                            journal_path: self.kit_path.clone(),
                            reason: format!(
                                "finalization for {outcome:?} requires exact adoption reconciliation: {reconciliation:?}"
                            ),
                        })
                    }
                }
            }
        }
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
        last_authority: bool,
    ) -> Result<(), CodegenError> {
        let disposition =
            self.with_strict_store(false, &self.kit_path.join(record.name()), |store| {
                store
                    .remove_finalization_record(record, outcome)
                    .map_err(strict_store_error)
            })?;
        match disposition {
            ExactRemovalDisposition::DurableAbsent => Ok(()),
            ExactRemovalDisposition::ReconcileRequired(reconciliation)
                if last_authority && matches!(reconciliation.world(), RemovalWorld::Missing) =>
            {
                Ok(())
            }
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
        let disposition = self.with_strict_store(true, &self.workspace_path, |store| {
            store
                .publish_snapshot(candidate)
                .map_err(strict_store_error)
        })?;
        match disposition {
            PublicationDisposition::Durable { record } => Ok(record),
            PublicationDisposition::DurableFinishOnlyResidual { reconciliation } => {
                Err(CodegenError::RecoveryRequired {
                    journal_path: self.workspace_path.clone(),
                    reason: format!(
                        "the irreversible journal record is durable with {:?} durability, but its publication residual must be reconciled before finalization at {:?}: {} ({:?})",
                        reconciliation.durability(),
                        reconciliation.mutation(),
                        reconciliation.source(),
                        reconciliation.world(),
                    ),
                })
            }
            PublicationDisposition::ReconcileRequired { reconciliation } => {
                Err(CodegenError::RecoveryRequired {
                    journal_path: self.workspace_path.clone(),
                    reason: format!(
                        "journal publication at {:?} requires exact recovery after {:?} with {:?} durability: {} ({:?})",
                        reconciliation.boundary(),
                        reconciliation.mutation(),
                        reconciliation.durability(),
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

fn directory_observation(metadata: &Metadata) -> ExactDirectoryObservation {
    ExactDirectoryObservation {
        identity: ExactObjectIdentity::from_unix(
            MetadataExt::dev(metadata),
            MetadataExt::ino(metadata),
        ),
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
