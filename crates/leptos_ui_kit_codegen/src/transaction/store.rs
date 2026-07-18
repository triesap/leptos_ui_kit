#![forbid(unsafe_code)]

//! Capability-relative persistence for the immutable transaction journal.
//!
//! This module deliberately stops at the persistence boundary.  It validates
//! namespaces, reads canonical envelopes, publishes immutable records, and
//! exposes exact removal primitives.  Choosing transaction, rollback, or
//! recovery actions belongs to the transaction engine.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, io,
    path::{Component, Path, PathBuf},
};

use cap_std::fs::Dir;
use serde::Deserialize;

use super::fs::{
    DirectoryEndpoint, ExactDirectoryEntry, ExactDirectoryEntryKind, ExactDirectoryInventory,
    ExactDirectoryObservation, ExactFileObservation, ExactFileRead, ExactIdentitySupport, FsOps,
    HardLinkEndpoint, ImmutablePublicationOutcome, ParentSyncKind, exact_identity_support,
};
use super::journal::{
    DirectoryModeV2, ExactDirectoryStateV2, ExactFileStateV2, FileStateV2, FinalizationFileKindV2,
    FinalizationLeaseV2, FinalizationOutcomeV2, JournalFileKindV2, JournalModelError,
    JournalPhaseV2, JournalSnapshotV2, ObjectIdentityV2, PartialEnvelopeHeaderV2,
    PartialRecordBindingV2, RecordBindingV2, Sha256Digest, TransactionId,
    WorkspaceBootstrapBindingV2, WorkspaceBootstrapEnvelopeV2, WorkspaceBootstrapIntentBindingV2,
    WorkspaceBootstrapIntentEnvelopeV2, bootstrap_intent_name, bootstrap_owner_name,
    parse_bootstrap_intent_name, parse_bootstrap_owner_name, parse_finalization_file_name,
    parse_journal_file_name, parse_transaction_directory_name, transaction_directory_name,
};
use super::runtime::{
    JournalRecordKind, TransactionOutcome, TransactionRuntime, TransitionKey, TransitionWindow,
};
use crate::PreservedFileMode;

const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const MAX_CONTROL_ENVELOPE_BYTES: u64 = 1024 * 1024;
const MAX_RECORD_ENVELOPE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_NAMESPACE_ENTRIES: usize = 16_384;
const MAX_RECORDS: usize = 100_000;
const WRITE_LOCK_NAME: &str = ".write.lock";
const TRANSACTION_PREFIX: &str = "transaction-v2-";
const BOOTSTRAP_PREFIX: &str = "bootstrap-v2-";
const BOOTSTRAP_INTENT_PREFIX: &str = "bootstrap-intent-v2-";
const FINALIZATION_PREFIX: &str = "finalization-v2-";

/// A failure before the store made a filesystem mutation, or a failure while
/// taking a stable, bounded observation.  Mutation uncertainty is represented
/// by the typed outcome enums below instead of being collapsed into this type.
#[derive(Debug)]
pub(super) struct JournalStoreError {
    path: PathBuf,
    kind: JournalStoreErrorKind,
}

#[derive(Debug)]
enum JournalStoreErrorKind {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Invalid {
        reason: String,
    },
    Unsupported {
        reason: String,
    },
}

impl JournalStoreError {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn reason(&self) -> String {
        match &self.kind {
            JournalStoreErrorKind::Io { operation, source } => {
                format!("could not {operation}: {source}")
            }
            JournalStoreErrorKind::Invalid { reason }
            | JournalStoreErrorKind::Unsupported { reason } => reason.clone(),
        }
    }

    fn invalid(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: JournalStoreErrorKind::Invalid {
                reason: reason.into(),
            },
        }
    }

    fn unsupported(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: JournalStoreErrorKind::Unsupported {
                reason: reason.into(),
            },
        }
    }

    fn io(path: impl Into<PathBuf>, operation: &'static str, source: io::Error) -> Self {
        Self {
            path: path.into(),
            kind: JournalStoreErrorKind::Io { operation, source },
        }
    }

    fn model(path: impl Into<PathBuf>, source: JournalModelError) -> Self {
        Self::invalid(path, source.reason())
    }
}

impl fmt::Display for JournalStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path.display(), self.reason())
    }
}

impl Error for JournalStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            JournalStoreErrorKind::Io { source, .. } => Some(source),
            JournalStoreErrorKind::Invalid { .. } | JournalStoreErrorKind::Unsupported { .. } => {
                None
            }
        }
    }
}

/// All capabilities used by the store are supplied by the lock-owning caller.
/// `path` fields are diagnostic only; filesystem access always uses the pinned
/// `Dir` handles and one-component child names in these endpoints.
#[derive(Clone, Copy)]
pub(super) struct JournalStoreCapabilities<'a> {
    project_root_path: &'a Path,
    project_root: ExactDirectoryObservation,
    held_write_lock_identity: (u64, u64),
    write_lock: HardLinkEndpoint<'a>,
    workspace_parent: DirectoryEndpoint<'a>,
    workspace: Option<DirectoryEndpoint<'a>>,
}

impl<'a> JournalStoreCapabilities<'a> {
    pub(super) fn active(
        project_root_path: &'a Path,
        project_root: ExactDirectoryObservation,
        held_write_lock_identity: (u64, u64),
        write_lock: HardLinkEndpoint<'a>,
        workspace_parent: DirectoryEndpoint<'a>,
        workspace: DirectoryEndpoint<'a>,
    ) -> Self {
        Self {
            project_root_path,
            project_root,
            held_write_lock_identity,
            write_lock,
            workspace_parent,
            workspace: Some(workspace),
        }
    }

    pub(super) fn finalization_only(
        project_root_path: &'a Path,
        project_root: ExactDirectoryObservation,
        held_write_lock_identity: (u64, u64),
        write_lock: HardLinkEndpoint<'a>,
        workspace_parent: DirectoryEndpoint<'a>,
    ) -> Self {
        Self {
            project_root_path,
            project_root,
            held_write_lock_identity,
            write_lock,
            workspace_parent,
            workspace: None,
        }
    }
}

/// Immutable store authority for exactly one project and transaction.
pub(super) struct JournalRecoveryStore<'a> {
    runtime: &'a TransactionRuntime,
    transaction_id: TransactionId,
    canonical_root_hash: Sha256Digest,
    capabilities: JournalStoreCapabilities<'a>,
}

impl<'a> JournalRecoveryStore<'a> {
    pub(super) fn bind(
        runtime: &'a TransactionRuntime,
        transaction_id: TransactionId,
        canonical_root_hash: Sha256Digest,
        capabilities: JournalStoreCapabilities<'a>,
    ) -> Result<Self, JournalStoreError> {
        if exact_identity_support() != ExactIdentitySupport::Complete {
            return Err(JournalStoreError::unsupported(
                capabilities.workspace_parent.path,
                "the current platform cannot represent the complete filesystem identities required by journal-v2",
            ));
        }
        require_child_name(capabilities.write_lock.name, capabilities.write_lock.path)?;
        require_child_name(
            capabilities.workspace_parent.name,
            capabilities.workspace_parent.path,
        )?;
        if capabilities.write_lock.name != Path::new(WRITE_LOCK_NAME) {
            return Err(JournalStoreError::invalid(
                capabilities.write_lock.path,
                "journal authority must be bound to the persistent .write.lock child",
            ));
        }
        if let Some(workspace) = capabilities.workspace {
            require_child_name(workspace.name, workspace.path)?;
            if workspace.name != Path::new(&transaction_directory_name(&transaction_id)) {
                return Err(JournalStoreError::invalid(
                    workspace.path,
                    "workspace capability name is not bound to the transaction identifier",
                ));
            }
        }
        Ok(Self {
            runtime,
            transaction_id,
            canonical_root_hash,
            capabilities,
        })
    }

    pub(super) fn transaction_id(&self) -> &TransactionId {
        &self.transaction_id
    }

    pub(super) fn inspect_namespace(&self) -> Result<JournalNamespace, JournalStoreError> {
        let capture = self.capture_authority(false)?;
        let parent = &capture.parent_namespace;
        if !parent.finalization.is_empty() || parent.finalization_partial.is_some() {
            return self
                .load_finalization_from_capture(&capture)
                .map(JournalNamespace::Finalizing);
        }
        match (&parent.workspace, &parent.bootstrap_intent) {
            (None, None) => {
                if self.capabilities.workspace.is_some() {
                    return Err(JournalStoreError::invalid(
                        self.capabilities.workspace_parent.path,
                        "a workspace capability was supplied for an empty journal namespace",
                    ));
                }
                self.recapture_matches(&capture, false)?;
                Ok(JournalNamespace::Empty)
            }
            (Some(_), Some(_)) => {
                let workspace = capture.workspace_namespace.as_ref().ok_or_else(|| {
                    JournalStoreError::invalid(
                        self.capabilities.workspace_parent.path,
                        "bootstrap namespace requires the exact workspace capability to be rebound",
                    )
                })?;
                if workspace.bootstrap_owner.is_none() {
                    return Err(JournalStoreError::invalid(
                        self.workspace_path(),
                        "bootstrap namespace is missing its exact owner envelope",
                    ));
                }
                if workspace.published.is_empty() && workspace.partial.is_none() {
                    self.validate_bootstrap_syntax(&capture, workspace)?;
                    self.recapture_matches(&capture, false)?;
                    Ok(JournalNamespace::Bootstrap(LoadedBootstrap {
                        lineage: LoadedJournal {
                            snapshots: Vec::new(),
                            records: Vec::new(),
                            partial: None,
                        },
                    }))
                } else {
                    match self.load_active()? {
                        ActiveJournalLoad::Stable(lineage) => {
                            Ok(JournalNamespace::Active(lineage))
                        }
                        ActiveJournalLoad::ReconciliationRequired(reconciliation) => {
                            Err(JournalStoreError::invalid(
                                self.workspace_path(),
                                format!(
                                    "active namespace requires publication reconciliation at sequence {} ({:?})",
                                    reconciliation.sequence, reconciliation.world
                                ),
                            ))
                        }
                    }
                }
            }
            _ => Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "bootstrap intent and exact transaction workspace must either both exist or both be absent",
            )),
        }
    }

    pub(super) fn load_active(&self) -> Result<ActiveJournalLoad, JournalStoreError> {
        let before = self.capture_authority(true)?;
        let workspace_namespace = before
            .workspace_namespace
            .as_ref()
            .ok_or_else(|| self.missing_workspace_error())?;
        if workspace_namespace.published.len() > MAX_RECORDS {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                format!("journal contains more than {MAX_RECORDS} immutable records"),
            ));
        }

        let publication_overlap = workspace_namespace.partial.as_ref().and_then(|partial| {
            workspace_namespace
                .published
                .get(&partial.sequence)
                .map(|published| (partial, published))
        });
        let overlap_sequence = publication_overlap.map(|(partial, _)| partial.sequence);
        if overlap_sequence.is_some_and(|sequence| {
            workspace_namespace.published.keys().next_back().copied() != Some(sequence)
        }) {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "partial/published overlap is only valid at the newest journal sequence",
            ));
        }

        let mut snapshots: Vec<JournalSnapshotV2> =
            Vec::with_capacity(workspace_namespace.published.len());
        let mut records = Vec::with_capacity(workspace_namespace.published.len());
        let mut identities = BTreeSet::new();

        for (index, (sequence, entry)) in workspace_namespace.published.iter().enumerate() {
            let expected_sequence = index as u64;
            if *sequence != expected_sequence {
                return Err(JournalStoreError::invalid(
                    self.workspace_path().join(&entry.name),
                    format!(
                        "journal lineage is not contiguous: expected sequence {expected_sequence}, found {sequence}"
                    ),
                ));
            }
            let read = self.read_inventory_file(
                self.workspace_directory()?,
                entry,
                MAX_RECORD_ENVELOPE_BYTES,
            )?;
            let exact = exact_file(&read.observation)
                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            require_private_exact_file(&exact, "immutable journal record", &entry.path)?;
            let is_overlap = overlap_sequence == Some(*sequence);
            if (!is_overlap && exact.link_count() != 1)
                || (is_overlap && exact.link_count() != 2)
            {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "journal record link count does not match a stable or linked-publication world",
                ));
            }
            if !identities.insert(read.observation.identity) {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "journal records and bootstrap authorities must have independent identities",
                ));
            }
            let snapshot = JournalSnapshotV2::from_record_envelope_slice(&read.bytes)
                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            if snapshot.transaction_id() != &self.transaction_id
                || snapshot.sequence() != *sequence
                || snapshot.record_name() != entry.name
            {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "record envelope does not match its transaction, sequence, and canonical filename",
                ));
            }
            let record = snapshot
                .expected_record_binding(exact.identity())
                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            snapshot
                .validate_record_binding(&record)
                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            if let Some(previous) = snapshots.last() {
                previous
                    .validate_successor(&snapshot)
                    .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            }
            snapshots.push(snapshot);
            records.push(record);
        }

        let partial = match &workspace_namespace.partial {
            Some(_) if publication_overlap.is_some() => None,
            Some(partial) => {
                let expected_sequence = records.len() as u64;
                if partial.sequence != expected_sequence {
                    return Err(JournalStoreError::invalid(
                        &partial.path,
                        format!(
                            "the sole journal partial must be the next sequence {expected_sequence}"
                        ),
                    ));
                }
                match self.load_completed_partial(partial, snapshots.last())? {
                    PartialLoad::Complete(completed) => Some(completed),
                    PartialLoad::Incomplete(world) => {
                        return Ok(ActiveJournalLoad::ReconciliationRequired(
                            ActiveReconciliation {
                                sequence: partial.sequence,
                                stable_record_count: records.len(),
                                world,
                            },
                        ));
                    }
                }
            }
            None => None,
        };

        let project = snapshots
            .first()
            .map(JournalSnapshotV2::project)
            .or_else(|| partial.as_ref().map(|partial| partial.snapshot().project()));
        if let Some(project) = project {
            let bootstrap = self.load_bootstrap(&before, workspace_namespace, project)?;
            if !identities.insert(file_identity(bootstrap.intent().exact()))
                || !identities.insert(file_identity(bootstrap.exact()))
            {
                return Err(JournalStoreError::invalid(
                    self.workspace_path(),
                    "bootstrap authorities alias an immutable record or one another",
                ));
            }
            for (snapshot, entry) in snapshots.iter().zip(workspace_namespace.published.values()) {
                self.validate_snapshot_authority(snapshot, &before, &bootstrap, &entry.path)?;
            }
            if let Some(completed) = &partial {
                self.validate_snapshot_authority(
                    completed.snapshot(),
                    &before,
                    &bootstrap,
                    &self.workspace_path().join(completed.binding().name()),
                )?;
            }
            self.revalidate_loaded_content(
                &before,
                workspace_namespace,
                &snapshots,
                partial.as_ref(),
                &bootstrap,
            )?;
        } else {
            // Before sequence zero is prepared there is no canonical project
            // binding with which to close the bootstrap-owner relation.  Both
            // files are still parsed canonically and mode/identity checked;
            // candidate publication closes the relation before mutation.
            self.validate_bootstrap_syntax(&before, workspace_namespace)?;
            self.validate_bootstrap_syntax(&before, workspace_namespace)?;
        }

        if let Some((partial_entry, published_entry)) = publication_overlap {
            let world = self.observe_overlap_world(partial_entry, published_entry)?;
            if !matches!(world, ObservedCandidateWorld::LinkedAliases { .. }) {
                return Err(JournalStoreError::invalid(
                    &partial_entry.path,
                    "same-sequence partial/published entries are not an authenticated linked publication",
                ));
            }
            let stable_record_count = usize::try_from(partial_entry.sequence).map_err(|_| {
                JournalStoreError::invalid(
                    &partial_entry.path,
                    "overlap sequence does not fit the process address space",
                )
            })?;
            self.recapture_matches(&before, true)?;
            return Ok(ActiveJournalLoad::ReconciliationRequired(
                ActiveReconciliation {
                    sequence: partial_entry.sequence,
                    stable_record_count,
                    world,
                },
            ));
        }

        self.recapture_matches(&before, true)?;
        Ok(ActiveJournalLoad::Stable(LoadedJournal {
            snapshots,
            records,
            partial,
        }))
    }

    fn revalidate_loaded_content(
        &self,
        capture: &AuthorityCapture,
        namespace: &WorkspaceNamespace,
        snapshots: &[JournalSnapshotV2],
        partial: Option<&CompletedPartial>,
        bootstrap: &WorkspaceBootstrapBindingV2,
    ) -> Result<(), JournalStoreError> {
        let project = snapshots
            .first()
            .map(JournalSnapshotV2::project)
            .or_else(|| partial.map(|partial| partial.snapshot().project()))
            .ok_or_else(|| {
                JournalStoreError::invalid(
                    self.workspace_path(),
                    "content revalidation requires a canonical project binding",
                )
            })?;
        let current_bootstrap = self.load_bootstrap(capture, namespace, project)?;
        if &current_bootstrap != bootstrap {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "bootstrap authority changed during lineage validation",
            ));
        }
        for (snapshot, entry) in snapshots.iter().zip(namespace.published.values()) {
            let reread = self.read_inventory_file(
                self.workspace_directory()?,
                entry,
                MAX_RECORD_ENVELOPE_BYTES,
            )?;
            let reparsed = JournalSnapshotV2::from_record_envelope_slice(&reread.bytes)
                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            if &reparsed != snapshot {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "immutable record content changed during lineage validation",
                ));
            }
        }
        if let Some(expected) = partial {
            let entry = namespace.partial.as_ref().ok_or_else(|| {
                JournalStoreError::invalid(
                    self.workspace_path(),
                    "complete partial disappeared during lineage validation",
                )
            })?;
            let reread = self.read_inventory_file(
                self.workspace_directory()?,
                entry,
                MAX_RECORD_ENVELOPE_BYTES,
            )?;
            if reread.observation != expected.observation
                || reread.bytes
                    != expected
                        .snapshot
                        .record_envelope_bytes()
                        .map_err(|error| JournalStoreError::model(&entry.path, error))?
            {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "complete partial changed during lineage validation",
                ));
            }
        }
        Ok(())
    }

    /// Publishes exactly one canonical successor through the immutable
    /// partial -> hard-link -> parent-sync -> exact-partial-cleanup protocol.
    ///
    /// Once mutation begins, every failure is returned as a typed disposition
    /// carrying the strongest durability knowledge and exact world available.
    pub(super) fn publish_snapshot(
        &self,
        loaded: &LoadedJournal,
        candidate: &JournalSnapshotV2,
    ) -> Result<PublicationDisposition, JournalStoreError> {
        candidate
            .validate()
            .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?;
        if candidate.transaction_id() != &self.transaction_id
            || candidate.project().canonical_root_hash() != &self.canonical_root_hash
        {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "candidate snapshot is not bound to this store transaction and canonical root",
            ));
        }

        let current = match self.load_active()? {
            ActiveJournalLoad::Stable(current) => current,
            ActiveJournalLoad::ReconciliationRequired(reconciliation) => {
                return Ok(PublicationDisposition::ReconcileRequired {
                    reconciliation: PublicationReconciliation {
                        boundary: publication_boundary(candidate),
                        durability: DurabilityKnowledge::VisibilityOrDurabilityUnknown,
                        mutation: StoreMutation::PublishImmutable,
                        world: reconciliation.world,
                        source: io::Error::new(
                            io::ErrorKind::Interrupted,
                            "existing journal publication must be reconciled before appending",
                        ),
                    },
                });
            }
        };
        if &current != loaded {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "loaded journal authority changed before successor publication",
            ));
        }
        if let Some(previous) = current.latest() {
            previous
                .validate_successor(candidate)
                .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?;
        } else if candidate.sequence() != 0 || candidate.previous_record().is_some() {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "an empty immutable lineage accepts only canonical sequence zero",
            ));
        }

        let authority = self.capture_authority(true)?;
        if !authority.parent_namespace.finalization.is_empty()
            || authority.parent_namespace.finalization_partial.is_some()
        {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "cannot append a journal record after finalization authority exists",
            ));
        }
        let workspace_namespace = authority
            .workspace_namespace
            .as_ref()
            .ok_or_else(|| self.missing_workspace_error())?;
        let bootstrap =
            self.load_bootstrap(&authority, workspace_namespace, candidate.project())?;
        self.validate_snapshot_authority(
            candidate,
            &authority,
            &bootstrap,
            &self.workspace_path().join(candidate.partial_name()),
        )?;

        let prepared = match &current.partial {
            Some(partial) => {
                if partial.snapshot != *candidate {
                    return Err(JournalStoreError::invalid(
                        self.workspace_path().join(partial.binding.name()),
                        "existing complete partial is not the requested canonical successor",
                    ));
                }
                PreparedRecord {
                    binding: partial.binding.clone(),
                    observation: partial.observation.clone(),
                }
            }
            None => match self.prepare_snapshot_partial(candidate)? {
                PrepareDisposition::Durable(prepared) => prepared,
                PrepareDisposition::ReconcileRequired(reconciliation) => {
                    return Ok(PublicationDisposition::ReconcileRequired { reconciliation });
                }
            },
        };

        let sequence = candidate.sequence();
        let boundary = publication_boundary(candidate);
        self.runtime
            .observe(publication_transition(boundary, TransitionWindow::Before));
        let partial_name = candidate.partial_name();
        let partial_path = self.workspace_path().join(&partial_name);
        let record_name = candidate.record_name();
        let record_path = self.workspace_path().join(&record_name);
        let workspace = self
            .capabilities
            .workspace
            .ok_or_else(|| self.missing_workspace_error())?;
        let workspace_observation =
            self.runtime
                .fs()
                .observe_directory(workspace)
                .map_err(|source| {
                    JournalStoreError::io(
                        workspace.path,
                        "observe immutable-record publication parent",
                        source,
                    )
                })?;
        let outcome = self.runtime.fs().publish_immutable(
            HardLinkEndpoint::new(workspace.directory, Path::new(&partial_name), &partial_path),
            &prepared.observation,
            HardLinkEndpoint::new(workspace.directory, Path::new(&record_name), &record_path),
            workspace,
            &workspace_observation,
            ParentSyncKind::Journal,
        );
        match outcome {
            ImmutablePublicationOutcome::Durable { published } => {
                let record = self.validate_durable_record(candidate, &published, &record_path)?;
                prepared
                    .binding
                    .completed_record_binding(candidate)
                    .map_err(|error| JournalStoreError::model(&record_path, error))?;
                self.runtime
                    .observe(publication_transition(boundary, TransitionWindow::After));
                Ok(PublicationDisposition::Durable { record })
            }
            ImmutablePublicationOutcome::NotPublished { partial, source } => {
                let world = self.authenticate_publication_world(
                    candidate,
                    partial,
                    None,
                    &prepared.observation,
                );
                Ok(PublicationDisposition::ReconcileRequired {
                    reconciliation: PublicationReconciliation {
                        boundary,
                        durability: DurabilityKnowledge::NotPublished,
                        mutation: StoreMutation::PublishImmutable,
                        world,
                        source,
                    },
                })
            }
            ImmutablePublicationOutcome::VisibleDurabilityUnknown {
                partial,
                published,
                source,
            } => {
                let world = self.authenticate_publication_world(
                    candidate,
                    Some(partial),
                    published,
                    &prepared.observation,
                );
                Ok(PublicationDisposition::ReconcileRequired {
                    reconciliation: PublicationReconciliation {
                        boundary,
                        durability: DurabilityKnowledge::VisibilityOrDurabilityUnknown,
                        mutation: StoreMutation::PublishImmutable,
                        world,
                        source,
                    },
                })
            }
            ImmutablePublicationOutcome::DurableWithPartialResidual {
                last_linked_published,
                last_linked_partial,
                partial_absent_in_process: _,
                source,
            } => {
                let record = candidate
                    .expected_record_binding(ObjectIdentityV2::new(
                        prepared.observation.identity.0,
                        prepared.observation.identity.1,
                    ))
                    .map_err(|error| JournalStoreError::model(&record_path, error))?;
                let reconciliation = PublicationReconciliation {
                    boundary,
                    durability: DurabilityKnowledge::DurableRecord,
                    mutation: StoreMutation::CleanupPublishedPartial,
                    world: self.authenticate_publication_world(
                        candidate,
                        Some(last_linked_partial),
                        Some(last_linked_published),
                        &prepared.observation,
                    ),
                    source,
                };
                if candidate.phase().desired_state_is_irreversible() {
                    Ok(PublicationDisposition::DurableFinishOnlyResidual {
                        record,
                        reconciliation,
                    })
                } else {
                    Ok(PublicationDisposition::ReconcileRequired { reconciliation })
                }
            }
        }
    }

    fn prepare_snapshot_partial(
        &self,
        candidate: &JournalSnapshotV2,
    ) -> Result<PrepareDisposition, JournalStoreError> {
        let envelope = candidate
            .record_envelope_bytes()
            .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?;
        if envelope.len() as u64 > MAX_RECORD_ENVELOPE_BYTES {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                format!(
                    "canonical record exceeds the {MAX_RECORD_ENVELOPE_BYTES}-byte bounded-read limit"
                ),
            ));
        }
        let sequence = candidate.sequence();
        let boundary = publication_boundary(candidate);
        let partial_name = candidate.partial_name();
        let partial_path = self.workspace_path().join(&partial_name);
        let workspace = self
            .capabilities
            .workspace
            .ok_or_else(|| self.missing_workspace_error())?;
        self.runtime.observe(TransitionKey::PrepareJournalPartial {
            sequence,
            window: TransitionWindow::Before,
        });

        let mut created = match self.runtime.fs().create_new_file(
            workspace.directory,
            Path::new(&partial_name),
            &partial_path,
            PRIVATE_FILE_MODE,
        ) {
            Ok(created) => created,
            Err(source) => {
                return Ok(PrepareDisposition::ReconcileRequired(
                    self.prepare_reconciliation(
                        candidate,
                        boundary,
                        StoreMutation::CreatePartial,
                        source,
                    ),
                ));
            }
        };
        if let Err(source) =
            self.runtime
                .fs()
                .set_file_mode(&created.file, &partial_path, PRIVATE_FILE_MODE)
        {
            return Ok(PrepareDisposition::ReconcileRequired(
                self.prepare_reconciliation(
                    candidate,
                    boundary,
                    StoreMutation::SetPartialMode,
                    source,
                ),
            ));
        }
        if let Err(source) =
            self.runtime
                .fs()
                .write_handle(&mut created.file, &partial_path, &envelope)
        {
            return Ok(PrepareDisposition::ReconcileRequired(
                self.prepare_reconciliation(
                    candidate,
                    boundary,
                    StoreMutation::WritePartial,
                    source,
                ),
            ));
        }
        if let Err(source) = self.runtime.fs().flush_file(&created.file, &partial_path) {
            return Ok(PrepareDisposition::ReconcileRequired(
                self.prepare_reconciliation(
                    candidate,
                    boundary,
                    StoreMutation::FlushPartial,
                    source,
                ),
            ));
        }
        if let Err(source) = self.runtime.fs().sync_handle(&created.file, &partial_path) {
            return Ok(PrepareDisposition::ReconcileRequired(
                self.prepare_reconciliation(
                    candidate,
                    boundary,
                    StoreMutation::SyncPartial,
                    source,
                ),
            ));
        }
        let first_read = match self.runtime.fs().read_regular_file_exact(
            workspace.directory,
            Path::new(&partial_name),
            &partial_path,
            MAX_RECORD_ENVELOPE_BYTES,
        ) {
            Ok(read) => read,
            Err(source) => {
                return Ok(PrepareDisposition::ReconcileRequired(
                    self.prepare_reconciliation(
                        candidate,
                        boundary,
                        StoreMutation::VerifyPartial,
                        source,
                    ),
                ));
            }
        };
        if first_read.observation.identity != created.identity || first_read.bytes != envelope {
            return Ok(PrepareDisposition::ReconcileRequired(
                PublicationReconciliation {
                    boundary,
                    durability: DurabilityKnowledge::NotPublished,
                    mutation: StoreMutation::VerifyPartial,
                    world: ObservedCandidateWorld::Conflict {
                        reason:
                            "exclusive partial changed identity or bytes before parent durability"
                                .to_owned(),
                        partial: Some(first_read.observation),
                        published: None,
                    },
                    source: io::Error::new(
                        io::ErrorKind::InvalidData,
                        "exclusive partial verification failed",
                    ),
                },
            ));
        }
        require_private_observation(&first_read.observation, "journal partial", &partial_path)?;
        if first_read.observation.link_count != Some(1) {
            return Err(JournalStoreError::invalid(
                &partial_path,
                "prepared journal partial must have exactly one hard link",
            ));
        }
        let workspace_observation =
            self.runtime
                .fs()
                .observe_directory(workspace)
                .map_err(|source| {
                    JournalStoreError::io(workspace.path, "observe journal-partial parent", source)
                })?;
        if let Err(source) = self.runtime.fs().sync_parent(
            workspace,
            &workspace_observation,
            ParentSyncKind::Journal,
        ) {
            return Ok(PrepareDisposition::ReconcileRequired(
                self.prepare_reconciliation(
                    candidate,
                    boundary,
                    StoreMutation::SyncPartialParent,
                    source,
                ),
            ));
        }
        let durable_read = match self.runtime.fs().read_regular_file_exact(
            workspace.directory,
            Path::new(&partial_name),
            &partial_path,
            MAX_RECORD_ENVELOPE_BYTES,
        ) {
            Ok(read) => read,
            Err(source) => {
                return Ok(PrepareDisposition::ReconcileRequired(
                    self.prepare_reconciliation(
                        candidate,
                        boundary,
                        StoreMutation::VerifyPartial,
                        source,
                    ),
                ));
            }
        };
        if durable_read != first_read {
            return Ok(PrepareDisposition::ReconcileRequired(
                PublicationReconciliation {
                    boundary,
                    durability: DurabilityKnowledge::NotPublished,
                    mutation: StoreMutation::VerifyPartial,
                    world: ObservedCandidateWorld::Conflict {
                        reason: "journal partial changed across its parent durability barrier"
                            .to_owned(),
                        partial: Some(durable_read.observation),
                        published: None,
                    },
                    source: io::Error::new(
                        io::ErrorKind::InvalidData,
                        "durable partial revalidation failed",
                    ),
                },
            ));
        }
        let exact = exact_file(&durable_read.observation)
            .map_err(|error| JournalStoreError::model(&partial_path, error))?;
        let (header, _) = PartialEnvelopeHeaderV2::parse_prefix(&durable_read.bytes)
            .map_err(|error| JournalStoreError::model(&partial_path, error))?;
        let binding = PartialRecordBindingV2::new(candidate, exact, header, &durable_read.bytes)
            .map_err(|error| JournalStoreError::model(&partial_path, error))?;
        self.runtime.observe(TransitionKey::PrepareJournalPartial {
            sequence,
            window: TransitionWindow::After,
        });
        Ok(PrepareDisposition::Durable(PreparedRecord {
            binding,
            observation: durable_read.observation,
        }))
    }

    fn prepare_reconciliation(
        &self,
        candidate: &JournalSnapshotV2,
        boundary: PublicationBoundary,
        mutation: StoreMutation,
        source: io::Error,
    ) -> PublicationReconciliation {
        PublicationReconciliation {
            boundary,
            durability: DurabilityKnowledge::NotPublished,
            mutation,
            world: self.probe_snapshot_world(candidate),
            source,
        }
    }

    fn probe_snapshot_world(&self, candidate: &JournalSnapshotV2) -> ObservedCandidateWorld {
        let Some(workspace) = self.capabilities.workspace else {
            return ObservedCandidateWorld::ObservationUnavailable {
                reason: "no active workspace capability is available".to_owned(),
            };
        };
        let result = (|| -> Result<ObservedCandidateWorld, JournalStoreError> {
            let observation = self
                .runtime
                .fs()
                .observe_directory(workspace)
                .map_err(|source| {
                    JournalStoreError::io(
                        workspace.path,
                        "observe reconciliation workspace",
                        source,
                    )
                })?;
            let inventory = self
                .runtime
                .fs()
                .inventory_directory_exact(workspace, &observation)
                .map_err(|source| {
                    JournalStoreError::io(
                        workspace.path,
                        "inventory reconciliation workspace",
                        source,
                    )
                })?;
            let namespace = self.validate_workspace_namespace(&inventory, true)?;
            let partial = namespace
                .partial
                .as_ref()
                .filter(|entry| entry.sequence == candidate.sequence())
                .map(|entry| {
                    self.read_inventory_file(workspace.directory, entry, MAX_RECORD_ENVELOPE_BYTES)
                })
                .transpose()?;
            let published = namespace
                .published
                .get(&candidate.sequence())
                .map(|entry| {
                    self.read_inventory_file(workspace.directory, entry, MAX_RECORD_ENVELOPE_BYTES)
                })
                .transpose()?;
            let expected = candidate
                .record_envelope_bytes()
                .map_err(|error| JournalStoreError::model(workspace.path, error))?;
            if partial.as_ref().is_some_and(|read| read.bytes != expected)
                || published
                    .as_ref()
                    .is_some_and(|read| read.bytes != expected)
            {
                return Ok(ObservedCandidateWorld::Conflict {
                    reason:
                        "reconciliation path does not contain the candidate's canonical envelope"
                            .to_owned(),
                    partial: partial.map(|read| read.observation),
                    published: published.map(|read| read.observation),
                });
            }
            Ok(classify_candidate_world(
                partial.map(|read| read.observation),
                published.map(|read| read.observation),
            ))
        })();
        result.unwrap_or_else(|error| ObservedCandidateWorld::ObservationUnavailable {
            reason: error.to_string(),
        })
    }

    fn authenticate_publication_world(
        &self,
        candidate: &JournalSnapshotV2,
        partial: Option<ExactFileObservation>,
        published: Option<ExactFileObservation>,
        expected: &ExactFileObservation,
    ) -> ObservedCandidateWorld {
        if partial
            .as_ref()
            .is_some_and(|observation| !same_expected_file_state(observation, expected))
            || published
                .as_ref()
                .is_some_and(|observation| !same_expected_file_state(observation, expected))
        {
            return ObservedCandidateWorld::Conflict {
                reason: format!(
                    "sequence {} publication observation does not match its canonical prepared envelope",
                    candidate.sequence()
                ),
                partial,
                published,
            };
        }
        classify_candidate_world(partial, published)
    }

    fn validate_durable_record(
        &self,
        candidate: &JournalSnapshotV2,
        published: &ExactFileObservation,
        path: &Path,
    ) -> Result<RecordBindingV2, JournalStoreError> {
        let exact = exact_file(published).map_err(|error| JournalStoreError::model(path, error))?;
        require_private_exact_file(&exact, "immutable journal record", path)?;
        if exact.link_count() != 1 {
            return Err(JournalStoreError::invalid(
                path,
                "durable immutable journal record must have exactly one hard link",
            ));
        }
        let record = candidate
            .expected_record_binding(exact.identity())
            .map_err(|error| JournalStoreError::model(path, error))?;
        candidate
            .validate_record_binding(&record)
            .map_err(|error| JournalStoreError::model(path, error))?;
        Ok(record)
    }

    fn capture_authority(
        &self,
        require_active: bool,
    ) -> Result<AuthorityCapture, JournalStoreError> {
        let root = self.capabilities.project_root;
        let write_lock = self
            .runtime
            .fs()
            .read_regular_file_exact(
                self.capabilities.write_lock.parent,
                self.capabilities.write_lock.name,
                self.capabilities.write_lock.path,
                MAX_CONTROL_ENVELOPE_BYTES,
            )
            .map_err(|source| {
                JournalStoreError::io(
                    self.capabilities.write_lock.path,
                    "read the exact persistent write lock",
                    source,
                )
            })?;
        require_private_observation(
            &write_lock.observation,
            "persistent write lock",
            self.capabilities.write_lock.path,
        )?;
        if write_lock.observation.identity != self.capabilities.held_write_lock_identity {
            return Err(JournalStoreError::invalid(
                self.capabilities.write_lock.path,
                "journal authority observed a different inode than the held advisory lock",
            ));
        }
        if write_lock.observation.link_count != Some(1) {
            return Err(JournalStoreError::invalid(
                self.capabilities.write_lock.path,
                "persistent write lock must have exactly one hard link",
            ));
        }

        let parent_observation = self
            .runtime
            .fs()
            .observe_directory(self.capabilities.workspace_parent)
            .map_err(|source| {
                JournalStoreError::io(
                    self.capabilities.workspace_parent.path,
                    "observe the transaction workspace parent",
                    source,
                )
            })?;
        let parent_inventory = self
            .runtime
            .fs()
            .inventory_directory_exact(self.capabilities.workspace_parent, &parent_observation)
            .map_err(|source| {
                JournalStoreError::io(
                    self.capabilities.workspace_parent.path,
                    "inventory the transaction workspace parent",
                    source,
                )
            })?;
        let parent_namespace = self.validate_parent_namespace(&parent_inventory, require_active)?;
        require_inventory_entry_matches_read(
            parent_namespace.write_lock.as_ref().ok_or_else(|| {
                JournalStoreError::invalid(
                    self.capabilities
                        .workspace_parent
                        .path
                        .join(WRITE_LOCK_NAME),
                    "workspace parent inventory is missing the persistent write lock",
                )
            })?,
            &write_lock,
        )?;

        let (workspace_inventory, workspace_namespace) = match self.capabilities.workspace {
            Some(workspace) => {
                let observation =
                    self.runtime
                        .fs()
                        .observe_directory(workspace)
                        .map_err(|source| {
                            JournalStoreError::io(
                                workspace.path,
                                "observe the transaction workspace",
                                source,
                            )
                        })?;
                require_private_directory_observation(
                    &observation,
                    "transaction workspace",
                    workspace.path,
                )?;
                let inventory = self
                    .runtime
                    .fs()
                    .inventory_directory_exact(workspace, &observation)
                    .map_err(|source| {
                        JournalStoreError::io(
                            workspace.path,
                            "inventory the transaction workspace",
                            source,
                        )
                    })?;
                let namespace = self.validate_workspace_namespace(&inventory, require_active)?;
                let parent_workspace = parent_namespace.workspace.as_ref().ok_or_else(|| {
                    JournalStoreError::invalid(
                        workspace.path,
                        "workspace capability exists but its exact parent entry is absent",
                    )
                })?;
                require_directory_entry_matches_observation(parent_workspace, &observation)?;
                (Some(inventory), Some(namespace))
            }
            None => {
                if require_active {
                    return Err(self.missing_workspace_error());
                }
                (None, None)
            }
        };

        Ok(AuthorityCapture {
            root,
            write_lock,
            parent_inventory,
            parent_namespace,
            workspace_inventory,
            workspace_namespace,
        })
    }

    fn recapture_matches(
        &self,
        before: &AuthorityCapture,
        require_active: bool,
    ) -> Result<(), JournalStoreError> {
        let after = self.capture_authority(require_active)?;
        if after.root != before.root
            || after.write_lock != before.write_lock
            || after.parent_inventory != before.parent_inventory
            || after.workspace_inventory != before.workspace_inventory
        {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "journal authority or namespace changed during bounded validation",
            ));
        }
        Ok(())
    }

    fn validate_parent_namespace(
        &self,
        inventory: &ExactDirectoryInventory,
        require_active: bool,
    ) -> Result<ParentNamespace, JournalStoreError> {
        enforce_inventory_bound(inventory, self.capabilities.workspace_parent.path)?;
        let mut namespace = ParentNamespace::default();
        for entry in &inventory.entries {
            let name = entry.name.to_str().ok_or_else(|| {
                JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "workspace-parent inventory contains a non-UTF-8 child name",
                )
            })?;
            let path = self.capabilities.workspace_parent.path.join(name);
            if name == WRITE_LOCK_NAME {
                require_entry_kind(
                    entry,
                    ExactDirectoryEntryKind::RegularFile,
                    &path,
                    "write lock",
                )?;
                require_private_entry(entry, "persistent write lock", &path)?;
                set_once(
                    &mut namespace.write_lock,
                    inventory_entry(entry, path),
                    "write lock",
                    self.capabilities.workspace_parent.path,
                )?;
                continue;
            }
            if name.starts_with(TRANSACTION_PREFIX) {
                let transaction_id = parse_transaction_directory_name(name)
                    .map_err(|error| JournalStoreError::model(&path, error))?;
                self.require_transaction(&transaction_id, &path)?;
                require_entry_kind(
                    entry,
                    ExactDirectoryEntryKind::Directory,
                    &path,
                    "transaction workspace",
                )?;
                require_private_directory_entry(entry, "transaction workspace", &path)?;
                set_once(
                    &mut namespace.workspace,
                    inventory_entry(entry, path),
                    "transaction workspace",
                    self.capabilities.workspace_parent.path,
                )?;
                continue;
            }
            if name.starts_with(BOOTSTRAP_INTENT_PREFIX) {
                let transaction_id = parse_bootstrap_intent_name(name)
                    .map_err(|error| JournalStoreError::model(&path, error))?;
                self.require_transaction(&transaction_id, &path)?;
                require_entry_kind(
                    entry,
                    ExactDirectoryEntryKind::RegularFile,
                    &path,
                    "bootstrap intent",
                )?;
                require_private_entry(entry, "bootstrap intent", &path)?;
                set_once(
                    &mut namespace.bootstrap_intent,
                    inventory_entry(entry, path),
                    "bootstrap intent",
                    self.capabilities.workspace_parent.path,
                )?;
                continue;
            }
            if name.starts_with(FINALIZATION_PREFIX) {
                let finalization = parse_finalization_store_name(name)
                    .map_err(|reason| JournalStoreError::invalid(&path, reason))?;
                self.require_transaction(&finalization.transaction_id, &path)?;
                require_entry_kind(
                    entry,
                    ExactDirectoryEntryKind::RegularFile,
                    &path,
                    "finalization record",
                )?;
                require_private_entry(entry, "finalization record", &path)?;
                let value = inventory_entry(entry, path);
                if finalization.partial {
                    set_once(
                        &mut namespace.finalization_partial,
                        (finalization.generation, value),
                        "finalization partial",
                        self.capabilities.workspace_parent.path,
                    )?;
                } else if namespace
                    .finalization
                    .insert(finalization.generation, value)
                    .is_some()
                {
                    return Err(JournalStoreError::invalid(
                        self.capabilities.workspace_parent.path,
                        "finalization namespace contains multiple current records for one generation",
                    ));
                }
                continue;
            }
            if name.starts_with(BOOTSTRAP_PREFIX) {
                return Err(JournalStoreError::invalid(
                    path,
                    "bootstrap-owner envelopes are only valid inside their exact transaction workspace",
                ));
            }
        }
        if namespace.write_lock.is_none() {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "workspace-parent inventory has no persistent write lock",
            ));
        }
        if require_active && (namespace.workspace.is_none() || namespace.bootstrap_intent.is_none())
        {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "active journal requires exactly one transaction workspace and bootstrap intent",
            ));
        }
        Ok(namespace)
    }

    fn validate_workspace_namespace(
        &self,
        inventory: &ExactDirectoryInventory,
        require_active: bool,
    ) -> Result<WorkspaceNamespace, JournalStoreError> {
        enforce_inventory_bound(inventory, self.workspace_path())?;
        let mut namespace = WorkspaceNamespace::default();
        for entry in &inventory.entries {
            let name = entry.name.to_str().ok_or_else(|| {
                JournalStoreError::invalid(
                    self.workspace_path(),
                    "transaction workspace contains a non-UTF-8 child name",
                )
            })?;
            let path = self.workspace_path().join(name);
            require_entry_kind(
                entry,
                ExactDirectoryEntryKind::RegularFile,
                &path,
                "workspace journal child",
            )?;
            require_private_entry(entry, "workspace journal child", &path)?;
            if name.starts_with(BOOTSTRAP_PREFIX) {
                let transaction_id = parse_bootstrap_owner_name(name)
                    .map_err(|error| JournalStoreError::model(&path, error))?;
                self.require_transaction(&transaction_id, &path)?;
                set_once(
                    &mut namespace.bootstrap_owner,
                    inventory_entry(entry, path),
                    "bootstrap owner",
                    self.workspace_path(),
                )?;
                continue;
            }
            if name.starts_with(TRANSACTION_PREFIX) {
                let parsed = parse_journal_file_name(name)
                    .map_err(|error| JournalStoreError::model(&path, error))?;
                self.require_transaction(parsed.transaction_id(), &path)?;
                let value = InventoryFile {
                    sequence: parsed.sequence(),
                    ..inventory_entry(entry, path)
                };
                match parsed.kind() {
                    JournalFileKindV2::Published => {
                        if namespace
                            .published
                            .insert(parsed.sequence(), value)
                            .is_some()
                        {
                            return Err(JournalStoreError::invalid(
                                self.workspace_path(),
                                "journal namespace contains multiple published records for one sequence",
                            ));
                        }
                    }
                    JournalFileKindV2::Partial => {
                        set_once(
                            &mut namespace.partial,
                            value,
                            "journal partial",
                            self.workspace_path(),
                        )?;
                    }
                }
                continue;
            }
            return Err(JournalStoreError::invalid(
                path,
                "unknown child in the strict transaction-workspace namespace",
            ));
        }
        if require_active && namespace.bootstrap_owner.is_none() {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "active transaction workspace has no bootstrap-owner envelope",
            ));
        }
        Ok(namespace)
    }

    fn load_bootstrap(
        &self,
        capture: &AuthorityCapture,
        workspace: &WorkspaceNamespace,
        project: &super::journal::ProjectBindingV2,
    ) -> Result<WorkspaceBootstrapBindingV2, JournalStoreError> {
        let intent_entry = capture
            .parent_namespace
            .bootstrap_intent
            .as_ref()
            .ok_or_else(|| {
                JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "active transaction has no bootstrap intent",
                )
            })?;
        let intent_read = self.read_inventory_file(
            self.capabilities.workspace_parent.directory,
            intent_entry,
            MAX_CONTROL_ENVELOPE_BYTES,
        )?;
        let intent_envelope: WorkspaceBootstrapIntentEnvelopeV2 =
            serde_json::from_slice(&intent_read.bytes).map_err(|source| {
                JournalStoreError::invalid(
                    &intent_entry.path,
                    format!("invalid workspace-bootstrap intent JSON: {source}"),
                )
            })?;
        let canonical_intent = intent_envelope
            .to_json_bytes()
            .map_err(|error| JournalStoreError::model(&intent_entry.path, error))?;
        if intent_read.bytes != canonical_intent {
            return Err(JournalStoreError::invalid(
                &intent_entry.path,
                "workspace-bootstrap intent bytes are not canonical",
            ));
        }
        let intent_exact = exact_file(&intent_read.observation)
            .map_err(|error| JournalStoreError::model(&intent_entry.path, error))?;
        let intent = WorkspaceBootstrapIntentBindingV2::new(intent_envelope, intent_exact)
            .map_err(|error| JournalStoreError::model(&intent_entry.path, error))?;

        let owner_entry = workspace.bootstrap_owner.as_ref().ok_or_else(|| {
            JournalStoreError::invalid(
                self.workspace_path(),
                "active transaction has no bootstrap-owner envelope",
            )
        })?;
        let owner_read = self.read_inventory_file(
            self.workspace_directory()?,
            owner_entry,
            MAX_CONTROL_ENVELOPE_BYTES,
        )?;
        let owner_envelope: WorkspaceBootstrapEnvelopeV2 =
            serde_json::from_slice(&owner_read.bytes).map_err(|source| {
                JournalStoreError::invalid(
                    &owner_entry.path,
                    format!("invalid workspace-bootstrap owner JSON: {source}"),
                )
            })?;
        let canonical_owner = owner_envelope
            .to_json_bytes()
            .map_err(|error| JournalStoreError::model(&owner_entry.path, error))?;
        if owner_read.bytes != canonical_owner {
            return Err(JournalStoreError::invalid(
                &owner_entry.path,
                "workspace-bootstrap owner bytes are not canonical",
            ));
        }
        WorkspaceBootstrapBindingV2::new(
            &self.transaction_id,
            project,
            intent,
            exact_file(&owner_read.observation)
                .map_err(|error| JournalStoreError::model(&owner_entry.path, error))?,
        )
        .map_err(|error| JournalStoreError::model(&owner_entry.path, error))
    }

    fn validate_bootstrap_syntax(
        &self,
        capture: &AuthorityCapture,
        workspace: &WorkspaceNamespace,
    ) -> Result<(), JournalStoreError> {
        let intent_entry = capture
            .parent_namespace
            .bootstrap_intent
            .as_ref()
            .ok_or_else(|| {
                JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "active transaction has no bootstrap intent",
                )
            })?;
        let intent_read = self.read_inventory_file(
            self.capabilities.workspace_parent.directory,
            intent_entry,
            MAX_CONTROL_ENVELOPE_BYTES,
        )?;
        let intent: WorkspaceBootstrapIntentEnvelopeV2 = serde_json::from_slice(&intent_read.bytes)
            .map_err(|source| {
                JournalStoreError::invalid(
                    &intent_entry.path,
                    format!("invalid workspace-bootstrap intent JSON: {source}"),
                )
            })?;
        if intent_read.bytes
            != intent
                .to_json_bytes()
                .map_err(|error| JournalStoreError::model(&intent_entry.path, error))?
        {
            return Err(JournalStoreError::invalid(
                &intent_entry.path,
                "workspace-bootstrap intent bytes are not canonical",
            ));
        }
        WorkspaceBootstrapIntentBindingV2::new(
            intent,
            exact_file(&intent_read.observation)
                .map_err(|error| JournalStoreError::model(&intent_entry.path, error))?,
        )
        .map_err(|error| JournalStoreError::model(&intent_entry.path, error))?;

        let owner_entry = workspace.bootstrap_owner.as_ref().ok_or_else(|| {
            JournalStoreError::invalid(
                self.workspace_path(),
                "active transaction has no bootstrap-owner envelope",
            )
        })?;
        let owner_read = self.read_inventory_file(
            self.workspace_directory()?,
            owner_entry,
            MAX_CONTROL_ENVELOPE_BYTES,
        )?;
        let owner: WorkspaceBootstrapEnvelopeV2 = serde_json::from_slice(&owner_read.bytes)
            .map_err(|source| {
                JournalStoreError::invalid(
                    &owner_entry.path,
                    format!("invalid workspace-bootstrap owner JSON: {source}"),
                )
            })?;
        if owner_read.bytes
            != owner
                .to_json_bytes()
                .map_err(|error| JournalStoreError::model(&owner_entry.path, error))?
        {
            return Err(JournalStoreError::invalid(
                &owner_entry.path,
                "workspace-bootstrap owner bytes are not canonical",
            ));
        }
        let owner_exact = exact_file(&owner_read.observation)
            .map_err(|error| JournalStoreError::model(&owner_entry.path, error))?;
        require_private_exact_file(&owner_exact, "bootstrap-owner envelope", &owner_entry.path)?;
        if owner_exact.link_count() != 1 {
            return Err(JournalStoreError::invalid(
                &owner_entry.path,
                "bootstrap-owner envelope must have exactly one hard link",
            ));
        }
        Ok(())
    }

    fn validate_snapshot_authority(
        &self,
        snapshot: &JournalSnapshotV2,
        capture: &AuthorityCapture,
        bootstrap: &WorkspaceBootstrapBindingV2,
        path: &Path,
    ) -> Result<(), JournalStoreError> {
        snapshot
            .validate()
            .map_err(|error| JournalStoreError::model(path, error))?;
        if snapshot.transaction_id() != &self.transaction_id
            || snapshot.project().canonical_root_hash() != &self.canonical_root_hash
            || snapshot.bootstrap() != bootstrap
        {
            return Err(JournalStoreError::invalid(
                path,
                "journal snapshot is not bound to this transaction, canonical root, and bootstrap authority",
            ));
        }
        let root = exact_directory(&capture.root).map_err(|error| {
            JournalStoreError::model(self.capabilities.project_root_path, error)
        })?;
        let lock = exact_file(&capture.write_lock.observation)
            .map_err(|error| JournalStoreError::model(self.capabilities.write_lock.path, error))?;
        let parent = exact_directory(&capture.parent_inventory.directory).map_err(|error| {
            JournalStoreError::model(self.capabilities.workspace_parent.path, error)
        })?;
        let workspace = capture
            .workspace_inventory
            .as_ref()
            .ok_or_else(|| self.missing_workspace_error())?;
        let workspace_exact = exact_directory(&workspace.directory)
            .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?;
        if snapshot.project().root_current() != &root
            || snapshot.project().write_lock() != &lock
            || snapshot.project().workspace_parent_current() != &parent
            || snapshot.project().workspace().exact() != &workspace_exact
            || snapshot.project().workspace().name()
                != transaction_directory_name(&self.transaction_id)
        {
            return Err(JournalStoreError::invalid(
                path,
                "journal project/workspace/write-lock binding does not match the exact live authority",
            ));
        }
        Ok(())
    }

    fn load_completed_partial(
        &self,
        entry: &InventoryFile,
        predecessor: Option<&JournalSnapshotV2>,
    ) -> Result<PartialLoad, JournalStoreError> {
        let read = self.read_inventory_file(
            self.workspace_directory()?,
            entry,
            MAX_RECORD_ENVELOPE_BYTES,
        )?;
        let exact = exact_file(&read.observation)
            .map_err(|error| JournalStoreError::model(&entry.path, error))?;
        require_private_exact_file(&exact, "journal partial", &entry.path)?;
        if exact.link_count() != 1 {
            return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                reason: "journal partial is not independently linked".to_owned(),
                partial: Some(read.observation),
                published: None,
            }));
        }
        let (header, payload) = match PartialEnvelopeHeaderV2::parse_prefix(&read.bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                    reason: format!("partial has no complete canonical ownership header: {error}"),
                    partial: Some(read.observation),
                    published: None,
                }));
            }
        };
        if header.sequence() != entry.sequence {
            return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                reason: "partial ownership header does not match its next sequence".to_owned(),
                partial: Some(read.observation),
                published: None,
            }));
        }
        let header_index: PartialHeaderIndex = parse_json_prefix(&read.bytes, &entry.path)?;
        if (payload.len() as u64) < header_index.payload_len {
            let Some(predecessor) = predecessor else {
                return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                    reason: "an incomplete sequence-zero partial cannot be bound without its canonical project snapshot".to_owned(),
                    partial: Some(read.observation),
                    published: None,
                }));
            };
            if let Err(error) =
                header.validate_binding(&self.transaction_id, predecessor.project(), entry.sequence)
            {
                return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                    reason: format!(
                        "partial ownership header does not match the exact workspace: {error}"
                    ),
                    partial: Some(read.observation),
                    published: None,
                }));
            }
            return Ok(PartialLoad::Incomplete(
                ObservedCandidateWorld::OwnedIncomplete {
                    partial: read.observation,
                    bytes_present: read.bytes.len() as u64,
                },
            ));
        }
        if payload.len() as u64 > header_index.payload_len {
            return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                reason: "partial payload exceeds the length bound in its ownership header"
                    .to_owned(),
                partial: Some(read.observation),
                published: None,
            }));
        }
        let candidate = JournalSnapshotV2::from_record_envelope_slice(&read.bytes)
            .map_err(|error| JournalStoreError::model(&entry.path, error))?;
        header
            .validate_binding(&self.transaction_id, candidate.project(), entry.sequence)
            .map_err(|error| JournalStoreError::model(&entry.path, error))?;
        if candidate.transaction_id() != &self.transaction_id
            || candidate.sequence() != entry.sequence
        {
            return Err(JournalStoreError::invalid(
                &entry.path,
                "complete journal partial does not match its canonical filename",
            ));
        }
        match predecessor {
            Some(previous) => previous
                .validate_successor(&candidate)
                .map_err(|error| JournalStoreError::model(&entry.path, error))?,
            None if candidate.sequence() == 0 => candidate
                .validate()
                .map_err(|error| JournalStoreError::model(&entry.path, error))?,
            None => {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "nonzero complete partial has no contiguous predecessor",
                ));
            }
        }
        let binding = PartialRecordBindingV2::new(&candidate, exact, header, &read.bytes)
            .map_err(|error| JournalStoreError::model(&entry.path, error))?;
        Ok(PartialLoad::Complete(CompletedPartial {
            snapshot: candidate,
            binding,
            observation: read.observation,
        }))
    }

    fn observe_overlap_world(
        &self,
        partial: &InventoryFile,
        published: &InventoryFile,
    ) -> Result<ObservedCandidateWorld, JournalStoreError> {
        let partial_read = self.read_inventory_file(
            self.workspace_directory()?,
            partial,
            MAX_RECORD_ENVELOPE_BYTES,
        )?;
        let published_read = self.read_inventory_file(
            self.workspace_directory()?,
            published,
            MAX_RECORD_ENVELOPE_BYTES,
        )?;
        if partial_read.bytes != published_read.bytes {
            return Ok(ObservedCandidateWorld::Conflict {
                reason: "same-sequence partial and published paths have different envelope bytes"
                    .to_owned(),
                partial: Some(partial_read.observation),
                published: Some(published_read.observation),
            });
        }
        let snapshot = JournalSnapshotV2::from_record_envelope_slice(&published_read.bytes)
            .map_err(|error| JournalStoreError::model(&published.path, error))?;
        if snapshot.transaction_id() != &self.transaction_id
            || snapshot.sequence() != partial.sequence
            || snapshot.partial_name() != partial.name
            || snapshot.record_name() != published.name
        {
            return Ok(ObservedCandidateWorld::Conflict {
                reason: "linked publication envelope is not bound to its transaction, sequence, and names"
                    .to_owned(),
                partial: Some(partial_read.observation),
                published: Some(published_read.observation),
            });
        }
        Ok(classify_candidate_world(
            Some(partial_read.observation),
            Some(published_read.observation),
        ))
    }

    fn read_inventory_file(
        &self,
        parent: &Dir,
        entry: &InventoryFile,
        max_bytes: u64,
    ) -> Result<ExactFileRead, JournalStoreError> {
        let read = self
            .runtime
            .fs()
            .read_regular_file_exact(parent, Path::new(&entry.name), &entry.path, max_bytes)
            .map_err(|source| {
                JournalStoreError::io(&entry.path, "read exact bounded journal file", source)
            })?;
        require_inventory_entry_matches_read(entry, &read)?;
        Ok(read)
    }

    fn require_transaction(
        &self,
        transaction_id: &TransactionId,
        path: &Path,
    ) -> Result<(), JournalStoreError> {
        if transaction_id != &self.transaction_id {
            return Err(JournalStoreError::invalid(
                path,
                "strict journal namespace contains a second current transaction",
            ));
        }
        Ok(())
    }

    fn workspace_directory(&self) -> Result<&Dir, JournalStoreError> {
        self.capabilities
            .workspace
            .map(|workspace| workspace.directory)
            .ok_or_else(|| self.missing_workspace_error())
    }

    fn workspace_path(&self) -> &Path {
        self.capabilities
            .workspace
            .map_or(self.capabilities.workspace_parent.path, |workspace| {
                workspace.path
            })
    }

    fn missing_workspace_error(&self) -> JournalStoreError {
        JournalStoreError::invalid(
            self.capabilities.workspace_parent.path,
            "operation requires an exact active transaction-workspace capability",
        )
    }
}

#[derive(Debug)]
pub(super) enum ActiveJournalLoad {
    Stable(LoadedJournal),
    ReconciliationRequired(ActiveReconciliation),
}

#[derive(Debug)]
pub(super) enum JournalNamespace {
    Empty,
    Bootstrap(LoadedBootstrap),
    Active(LoadedJournal),
    Finalizing(LoadedFinalization),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LoadedBootstrap {
    lineage: LoadedJournal,
}

impl LoadedBootstrap {
    pub(super) fn lineage(&self) -> &LoadedJournal {
        &self.lineage
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LoadedFinalization {
    history: Vec<FinalizationRecord>,
    partial: Option<FinalizationRecord>,
    reconciliation: Option<FinalizationWorld>,
}

impl LoadedFinalization {
    pub(super) fn latest(&self) -> Option<&FinalizationRecord> {
        self.history.last()
    }

    pub(super) fn history(&self) -> &[FinalizationRecord] {
        &self.history
    }

    pub(super) fn partial(&self) -> Option<&FinalizationRecord> {
        self.partial.as_ref()
    }

    pub(super) fn reconciliation(&self) -> Option<&FinalizationWorld> {
        self.reconciliation.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FinalizationRecord {
    lease: FinalizationLeaseV2,
    exact: ExactFileStateV2,
    observation: ExactFileObservation,
    name: String,
}

impl FinalizationRecord {
    pub(super) fn lease(&self) -> &FinalizationLeaseV2 {
        &self.lease
    }

    pub(super) fn exact(&self) -> &ExactFileStateV2 {
        &self.exact
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FinalizationWorld {
    PreparedNext { generation: u64 },
    LinkedAliases { generation: u64 },
    Conflict { generation: u64, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LoadedJournal {
    snapshots: Vec<JournalSnapshotV2>,
    records: Vec<RecordBindingV2>,
    partial: Option<CompletedPartial>,
}

impl LoadedJournal {
    pub(super) fn latest(&self) -> Option<&JournalSnapshotV2> {
        self.snapshots.last()
    }

    pub(super) fn snapshots(&self) -> &[JournalSnapshotV2] {
        &self.snapshots
    }

    pub(super) fn records(&self) -> &[RecordBindingV2] {
        &self.records
    }

    pub(super) fn partial(&self) -> Option<&CompletedPartial> {
        self.partial.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompletedPartial {
    snapshot: JournalSnapshotV2,
    binding: PartialRecordBindingV2,
    observation: ExactFileObservation,
}

#[derive(Debug)]
pub(super) enum PublicationDisposition {
    Durable {
        record: RecordBindingV2,
    },
    DurableFinishOnlyResidual {
        record: RecordBindingV2,
        reconciliation: PublicationReconciliation,
    },
    ReconcileRequired {
        reconciliation: PublicationReconciliation,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublicationBoundary {
    JournalRecord { sequence: u64 },
    CommitBoundary { sequence: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DurabilityKnowledge {
    NotPublished,
    VisibilityOrDurabilityUnknown,
    DurableRecord,
}

#[derive(Debug)]
pub(super) struct PublicationReconciliation {
    boundary: PublicationBoundary,
    durability: DurabilityKnowledge,
    mutation: StoreMutation,
    world: ObservedCandidateWorld,
    source: io::Error,
}

impl PublicationReconciliation {
    pub(super) const fn boundary(&self) -> PublicationBoundary {
        self.boundary
    }

    pub(super) const fn durability(&self) -> DurabilityKnowledge {
        self.durability
    }

    pub(super) const fn mutation(&self) -> StoreMutation {
        self.mutation
    }

    pub(super) fn world(&self) -> &ObservedCandidateWorld {
        &self.world
    }

    pub(super) fn source(&self) -> &io::Error {
        &self.source
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StoreMutation {
    CreatePartial,
    SetPartialMode,
    WritePartial,
    FlushPartial,
    SyncPartial,
    VerifyPartial,
    SyncPartialParent,
    PublishImmutable,
    CleanupPublishedPartial,
}

impl CompletedPartial {
    pub(super) fn snapshot(&self) -> &JournalSnapshotV2 {
        &self.snapshot
    }

    pub(super) fn binding(&self) -> &PartialRecordBindingV2 {
        &self.binding
    }
}

#[derive(Debug)]
pub(super) struct ActiveReconciliation {
    sequence: u64,
    stable_record_count: usize,
    world: ObservedCandidateWorld,
}

impl ActiveReconciliation {
    pub(super) const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) const fn stable_record_count(&self) -> usize {
        self.stable_record_count
    }

    pub(super) fn world(&self) -> &ObservedCandidateWorld {
        &self.world
    }
}

#[derive(Debug)]
pub(super) enum ObservedCandidateWorld {
    Missing,
    PreparedOnly {
        partial: ExactFileObservation,
    },
    PublishedOnly {
        published: ExactFileObservation,
    },
    LinkedAliases {
        partial: ExactFileObservation,
        published: ExactFileObservation,
    },
    OwnedIncomplete {
        partial: ExactFileObservation,
        bytes_present: u64,
    },
    Conflict {
        reason: String,
        partial: Option<ExactFileObservation>,
        published: Option<ExactFileObservation>,
    },
    ObservationUnavailable {
        reason: String,
    },
}

enum PartialLoad {
    Complete(CompletedPartial),
    Incomplete(ObservedCandidateWorld),
}

struct PreparedRecord {
    binding: PartialRecordBindingV2,
    observation: ExactFileObservation,
}

enum PrepareDisposition {
    Durable(PreparedRecord),
    ReconcileRequired(PublicationReconciliation),
}

#[derive(Debug)]
struct AuthorityCapture {
    root: ExactDirectoryObservation,
    write_lock: ExactFileRead,
    parent_inventory: ExactDirectoryInventory,
    parent_namespace: ParentNamespace,
    workspace_inventory: Option<ExactDirectoryInventory>,
    workspace_namespace: Option<WorkspaceNamespace>,
}

#[derive(Debug, Default)]
struct ParentNamespace {
    write_lock: Option<InventoryFile>,
    workspace: Option<InventoryFile>,
    bootstrap_intent: Option<InventoryFile>,
    finalization: BTreeMap<u64, InventoryFile>,
    finalization_partial: Option<(u64, InventoryFile)>,
}

#[derive(Debug, Default)]
struct WorkspaceNamespace {
    bootstrap_owner: Option<InventoryFile>,
    published: BTreeMap<u64, InventoryFile>,
    partial: Option<InventoryFile>,
}

#[derive(Debug, Clone)]
struct InventoryFile {
    sequence: u64,
    name: String,
    path: PathBuf,
    identity: (u64, u64),
    byte_len: u64,
    mode: PreservedFileMode,
    link_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartialHeaderIndex {
    payload_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FinalizationStoreName {
    transaction_id: TransactionId,
    generation: u64,
    partial: bool,
}

fn inventory_entry(entry: &ExactDirectoryEntry, path: PathBuf) -> InventoryFile {
    InventoryFile {
        sequence: 0,
        name: entry
            .name
            .to_str()
            .expect("namespace validation accepted UTF-8")
            .to_owned(),
        path,
        identity: entry.identity,
        byte_len: entry.byte_len,
        mode: entry.mode,
        link_count: entry.link_count,
    }
}

fn set_once<T>(
    slot: &mut Option<T>,
    value: T,
    label: &str,
    path: &Path,
) -> Result<(), JournalStoreError> {
    if slot.replace(value).is_some() {
        return Err(JournalStoreError::invalid(
            path,
            format!("strict namespace contains multiple current {label} entries"),
        ));
    }
    Ok(())
}

fn require_child_name(name: &Path, path: &Path) -> Result<(), JournalStoreError> {
    let mut components = name.components();
    if matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none() {
        return Ok(());
    }
    Err(JournalStoreError::invalid(
        path,
        "capability-relative journal endpoint name must be one direct child component",
    ))
}

fn enforce_inventory_bound(
    inventory: &ExactDirectoryInventory,
    path: &Path,
) -> Result<(), JournalStoreError> {
    if inventory.entries.len() > MAX_NAMESPACE_ENTRIES {
        return Err(JournalStoreError::invalid(
            path,
            format!(
                "journal namespace exceeds the bounded inventory limit of {MAX_NAMESPACE_ENTRIES} entries"
            ),
        ));
    }
    Ok(())
}

fn require_entry_kind(
    entry: &ExactDirectoryEntry,
    expected: ExactDirectoryEntryKind,
    path: &Path,
    label: &str,
) -> Result<(), JournalStoreError> {
    if entry.kind != expected {
        return Err(JournalStoreError::invalid(
            path,
            format!("{label} has an unsafe filesystem type: {:?}", entry.kind),
        ));
    }
    Ok(())
}

fn require_private_entry(
    entry: &ExactDirectoryEntry,
    label: &str,
    path: &Path,
) -> Result<(), JournalStoreError> {
    require_private_mode(entry.mode, PRIVATE_FILE_MODE, label, path)?;
    if entry.link_count == Some(0) || entry.link_count.is_none() {
        return Err(JournalStoreError::invalid(
            path,
            format!("{label} has no exact positive link count"),
        ));
    }
    Ok(())
}

fn require_private_directory_entry(
    entry: &ExactDirectoryEntry,
    label: &str,
    path: &Path,
) -> Result<(), JournalStoreError> {
    require_private_mode(entry.mode, PRIVATE_DIRECTORY_MODE, label, path)?;
    if entry.link_count == Some(0) || entry.link_count.is_none() {
        return Err(JournalStoreError::invalid(
            path,
            format!("{label} has no exact positive link count"),
        ));
    }
    Ok(())
}

fn require_private_observation(
    observation: &ExactFileObservation,
    label: &str,
    path: &Path,
) -> Result<(), JournalStoreError> {
    require_private_mode(observation.mode, PRIVATE_FILE_MODE, label, path)
}

fn require_private_directory_observation(
    observation: &ExactDirectoryObservation,
    label: &str,
    path: &Path,
) -> Result<(), JournalStoreError> {
    require_private_mode(observation.mode, PRIVATE_DIRECTORY_MODE, label, path)
}

fn require_private_exact_file(
    exact: &ExactFileStateV2,
    label: &str,
    path: &Path,
) -> Result<(), JournalStoreError> {
    if exact.state().readonly() || exact.state().posix_mode() != platform_mode(PRIVATE_FILE_MODE) {
        return Err(JournalStoreError::invalid(
            path,
            format!("{label} must be writable with exact private mode {PRIVATE_FILE_MODE:#o}"),
        ));
    }
    Ok(())
}

fn require_private_mode(
    mode: PreservedFileMode,
    expected: u32,
    label: &str,
    path: &Path,
) -> Result<(), JournalStoreError> {
    if mode.readonly || mode.posix_mode != platform_mode(expected) {
        return Err(JournalStoreError::invalid(
            path,
            format!("{label} must be writable with exact private mode {expected:#o}"),
        ));
    }
    Ok(())
}

#[cfg(unix)]
const fn platform_mode(mode: u32) -> Option<u32> {
    Some(mode)
}

#[cfg(not(unix))]
const fn platform_mode(_mode: u32) -> Option<u32> {
    None
}

fn require_inventory_entry_matches_read(
    entry: &InventoryFile,
    read: &ExactFileRead,
) -> Result<(), JournalStoreError> {
    if entry.identity != read.observation.identity
        || entry.byte_len != read.observation.byte_len
        || entry.mode != read.observation.mode
        || entry.link_count != read.observation.link_count
    {
        return Err(JournalStoreError::invalid(
            &entry.path,
            "journal child changed between exact inventory and bounded no-follow read",
        ));
    }
    Ok(())
}

fn require_directory_entry_matches_observation(
    entry: &InventoryFile,
    observation: &ExactDirectoryObservation,
) -> Result<(), JournalStoreError> {
    if entry.identity != observation.identity
        || entry.mode != observation.mode
        || entry.link_count != observation.link_count
    {
        return Err(JournalStoreError::invalid(
            &entry.path,
            "workspace directory changed between parent inventory and exact capability observation",
        ));
    }
    Ok(())
}

pub(super) fn exact_file(
    observation: &ExactFileObservation,
) -> Result<ExactFileStateV2, JournalModelError> {
    ExactFileStateV2::new(
        ObjectIdentityV2::new(observation.identity.0, observation.identity.1),
        FileStateV2::new(
            Sha256Digest::parse(&observation.content_hash)?,
            observation.byte_len,
            observation.mode.readonly,
            observation.mode.posix_mode,
        )?,
        observation
            .link_count
            .ok_or_else(|| JournalModelError::new("exact file link count is unavailable"))?,
    )
}

pub(super) fn exact_directory(
    observation: &ExactDirectoryObservation,
) -> Result<ExactDirectoryStateV2, JournalModelError> {
    ExactDirectoryStateV2::new(
        ObjectIdentityV2::new(observation.identity.0, observation.identity.1),
        DirectoryModeV2::new(observation.mode.readonly, observation.mode.posix_mode)?,
        observation
            .link_count
            .ok_or_else(|| JournalModelError::new("exact directory link count is unavailable"))?,
    )
}

fn file_identity(exact: &ExactFileStateV2) -> (u64, u64) {
    (exact.identity().device(), exact.identity().inode())
}

pub(super) fn exact_file_observation(exact: &ExactFileStateV2) -> ExactFileObservation {
    ExactFileObservation {
        identity: file_identity(exact),
        byte_len: exact.state().byte_len(),
        content_hash: exact.state().content_hash().as_str().to_owned(),
        mode: PreservedFileMode {
            readonly: exact.state().readonly(),
            posix_mode: exact.state().posix_mode(),
        },
        link_count: Some(exact.link_count()),
    }
}

fn classify_candidate_world(
    partial: Option<ExactFileObservation>,
    published: Option<ExactFileObservation>,
) -> ObservedCandidateWorld {
    match (partial, published) {
        (None, None) => ObservedCandidateWorld::Missing,
        (Some(partial), None) if partial.link_count == Some(1) => {
            ObservedCandidateWorld::PreparedOnly { partial }
        }
        (None, Some(published)) if published.link_count == Some(1) => {
            ObservedCandidateWorld::PublishedOnly { published }
        }
        (Some(partial), Some(published))
            if same_file_state_except_links(&partial, &published)
                && partial.identity == published.identity
                && partial.link_count == Some(2)
                && published.link_count == Some(2) =>
        {
            ObservedCandidateWorld::LinkedAliases { partial, published }
        }
        (partial, published) => ObservedCandidateWorld::Conflict {
            reason: "partial and published paths do not form a valid immutable-publication world"
                .to_owned(),
            partial,
            published,
        },
    }
}

fn same_file_state_except_links(left: &ExactFileObservation, right: &ExactFileObservation) -> bool {
    left.identity == right.identity
        && left.byte_len == right.byte_len
        && left.content_hash == right.content_hash
        && left.mode == right.mode
}

fn same_expected_file_state(
    observation: &ExactFileObservation,
    expected: &ExactFileObservation,
) -> bool {
    observation.identity == expected.identity
        && observation.byte_len == expected.byte_len
        && observation.content_hash == expected.content_hash
        && observation.mode == expected.mode
        && matches!(observation.link_count, Some(1 | 2))
}

fn publication_boundary(candidate: &JournalSnapshotV2) -> PublicationBoundary {
    if candidate.phase().desired_state_is_irreversible() {
        PublicationBoundary::CommitBoundary {
            sequence: candidate.sequence(),
        }
    } else {
        PublicationBoundary::JournalRecord {
            sequence: candidate.sequence(),
        }
    }
}

fn publication_transition(
    boundary: PublicationBoundary,
    window: TransitionWindow,
) -> TransitionKey {
    match boundary {
        PublicationBoundary::JournalRecord { sequence } => {
            TransitionKey::PublishJournalRecord { sequence, window }
        }
        PublicationBoundary::CommitBoundary { sequence } => {
            TransitionKey::CommitBoundary { sequence, window }
        }
    }
}

fn parse_json_prefix<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    path: &Path,
) -> Result<T, JournalStoreError> {
    let newline = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| {
            JournalStoreError::invalid(
                path,
                "journal partial has no complete ownership-header line",
            )
        })?;
    serde_json::from_slice(&bytes[..newline]).map_err(|source| {
        JournalStoreError::invalid(path, format!("invalid journal ownership header: {source}"))
    })
}

fn parse_finalization_store_name(name: &str) -> Result<FinalizationStoreName, String> {
    let parsed = parse_finalization_file_name(name).map_err(|error| error.reason().to_owned())?;
    Ok(FinalizationStoreName {
        transaction_id: parsed.transaction_id().clone(),
        generation: parsed.generation(),
        partial: parsed.kind() == FinalizationFileKindV2::Partial,
    })
}
