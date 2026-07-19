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

use super::fs::{
    DirectoryEndpoint, ExactDirectoryEntry, ExactDirectoryEntryKind, ExactDirectoryHandle,
    ExactDirectoryInventory, ExactDirectoryObservation, ExactFileObservation, ExactFileRead,
    ExactObjectIdentity, HardLinkEndpoint, ImmutablePublicationOutcome, ParentSyncKind,
};
use super::journal::{
    ArtifactOrdinal, DirectoryModeV2, ExactDirectoryMetadataV2, ExactDirectoryStateV2,
    ExactFileMetadataV2, ExactFileStateV2, FileStateV2, FinalizationFileKindV2,
    FinalizationLeaseV2, FinalizationOutcomeV2, FinalizationStateV2, JournalFileKindV2,
    JournalModelError, JournalPhaseV2, JournalSnapshotV2, ObjectIdentityV2,
    OwnedResidualDeleteBindingV2, OwnedResidualObjectV2, OwnerArtifactKindV2,
    PartialEnvelopeHeaderV2, PartialRecordBindingV2, PreparationPendingIntentV2,
    PreparationPlacementIntentV2, RecordBindingV2, Sha256Digest, TransactionId,
    WorkspaceBootstrapBindingV2, WorkspaceBootstrapEnvelopeV2, WorkspaceBootstrapIntentBindingV2,
    WorkspaceBootstrapIntentEnvelopeV2, bootstrap_intent_name, bootstrap_owner_name,
    journal_partial_name, parse_bootstrap_intent_name, parse_bootstrap_owner_name,
    parse_finalization_file_name, parse_journal_file_name, parse_owner_artifact_name,
    parse_transaction_directory_name, transaction_directory_name,
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
const RESERVED_TRANSACTION_FAMILY: &str = "transaction-";
const RESERVED_BOOTSTRAP_FAMILY: &str = "bootstrap-";
const RESERVED_FINALIZATION_FAMILY: &str = "finalization-";

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
            JournalStoreErrorKind::Invalid { reason } => reason.clone(),
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
            JournalStoreErrorKind::Invalid { .. } => None,
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
    held_coordination_parent_identity: ExactObjectIdentity,
    held_write_lock_identity: ExactObjectIdentity,
    write_lock: HardLinkEndpoint<'a>,
    workspace_parent: DirectoryEndpoint<'a>,
    workspace: Option<DirectoryEndpoint<'a>>,
}

impl<'a> JournalStoreCapabilities<'a> {
    pub(super) fn active(
        project_root_path: &'a Path,
        project_root: ExactDirectoryObservation,
        held_coordination_parent_identity: ExactObjectIdentity,
        held_write_lock_identity: ExactObjectIdentity,
        write_lock: HardLinkEndpoint<'a>,
        workspace_parent: DirectoryEndpoint<'a>,
        workspace: DirectoryEndpoint<'a>,
    ) -> Self {
        Self {
            project_root_path,
            project_root,
            held_coordination_parent_identity,
            held_write_lock_identity,
            write_lock,
            workspace_parent,
            workspace: Some(workspace),
        }
    }

    pub(super) fn finalization_only(
        project_root_path: &'a Path,
        project_root: ExactDirectoryObservation,
        held_coordination_parent_identity: ExactObjectIdentity,
        held_write_lock_identity: ExactObjectIdentity,
        write_lock: HardLinkEndpoint<'a>,
        workspace_parent: DirectoryEndpoint<'a>,
    ) -> Self {
        Self {
            project_root_path,
            project_root,
            held_coordination_parent_identity,
            held_write_lock_identity,
            write_lock,
            workspace_parent,
            workspace: None,
        }
    }
}

/// Pinned authority needed to turn a stable top-level namespace discovery
/// into a store for exactly one transaction.  The workspace capability is
/// deliberately absent here: its canonical name and expected identity must
/// first come from store-owned discovery.
#[derive(Clone, Copy)]
pub(super) struct JournalDiscoveryCapabilities<'a> {
    project_root_path: &'a Path,
    project_root: ExactDirectoryObservation,
    held_coordination_parent_identity: ExactObjectIdentity,
    held_write_lock_identity: ExactObjectIdentity,
    write_lock: HardLinkEndpoint<'a>,
    workspace_parent: DirectoryEndpoint<'a>,
}

impl<'a> JournalDiscoveryCapabilities<'a> {
    pub(super) fn new(
        project_root_path: &'a Path,
        project_root: ExactDirectoryObservation,
        held_coordination_parent_identity: ExactObjectIdentity,
        held_write_lock_identity: ExactObjectIdentity,
        write_lock: HardLinkEndpoint<'a>,
        workspace_parent: DirectoryEndpoint<'a>,
    ) -> Self {
        Self {
            project_root_path,
            project_root,
            held_coordination_parent_identity,
            held_write_lock_identity,
            write_lock,
            workspace_parent,
        }
    }
}

/// A stable read-only inventory result.  `Transaction` contains an identifier
/// parsed and cross-checked by this module; it is suitable for diagnostics,
/// but recovery mutation still requires `JournalRecoveryStore::discover` and
/// the bound transaction returned by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JournalTopLevelNamespace {
    Empty,
    Transaction(TopLevelTransaction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TopLevelTransaction {
    transaction_id: TransactionId,
    workspace: Option<DiscoveredWorkspace>,
}

impl TopLevelTransaction {
    pub(super) fn transaction_id(&self) -> &TransactionId {
        &self.transaction_id
    }

    pub(super) fn workspace_path(&self) -> Option<&Path> {
        self.workspace
            .as_ref()
            .map(|workspace| workspace.path.as_path())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredWorkspace {
    name: String,
    path: PathBuf,
    observation: ExactDirectoryObservation,
}

/// The result of lock-bound namespace discovery.  Recovery can only obtain a
/// store through the `Transaction` value, so it never chooses a transaction
/// identifier or workspace name from an independent filename scan.
#[allow(
    clippy::large_enum_variant,
    reason = "discovery keeps the authenticated capability set inline so it cannot be detached from its borrow lifetime"
)]
pub(super) enum DiscoveredJournalNamespace<'a> {
    Empty,
    Transaction(DiscoveredJournalTransaction<'a>),
}

pub(super) struct DiscoveredJournalTransaction<'a> {
    runtime: &'a TransactionRuntime,
    canonical_root_hash: Sha256Digest,
    capabilities: JournalDiscoveryCapabilities<'a>,
    top_level: TopLevelTransaction,
}

impl<'a> DiscoveredJournalTransaction<'a> {
    pub(super) fn transaction_id(&self) -> &TransactionId {
        self.top_level.transaction_id()
    }

    pub(super) fn workspace_path(&self) -> Option<&Path> {
        self.top_level.workspace_path()
    }

    /// Opens the one inventoried workspace without following links and proves
    /// that the opened handle still has the exact identity and mode discovered
    /// in the stable parent inventory.
    pub(super) fn open_workspace(&self) -> Result<Option<ExactDirectoryHandle>, JournalStoreError> {
        let Some(workspace) = &self.top_level.workspace else {
            return Ok(None);
        };
        let opened = self
            .runtime
            .fs()
            .open_directory_exact(
                self.capabilities.workspace_parent.directory,
                Path::new(&workspace.name),
                &workspace.path,
                PRIVATE_DIRECTORY_MODE,
            )
            .map_err(|source| {
                JournalStoreError::io(
                    &workspace.path,
                    "open the discovered exact transaction workspace",
                    source,
                )
            })?;
        if opened.observation != workspace.observation {
            return Err(JournalStoreError::invalid(
                &workspace.path,
                "transaction workspace changed after stable top-level discovery",
            ));
        }
        Ok(Some(opened))
    }

    /// Completes binding using only the exact workspace handle authorized by
    /// this discovery.  Supplying or omitting a handle in any other world is a
    /// hard error rather than a caller-selected finalization mode.
    pub(super) fn bind<'b>(
        &'b self,
        workspace: Option<&'b ExactDirectoryHandle>,
    ) -> Result<JournalRecoveryStore<'b>, JournalStoreError>
    where
        'a: 'b,
    {
        let capabilities = match (&self.top_level.workspace, workspace) {
            (Some(expected), Some(opened)) => {
                if opened.observation != expected.observation {
                    return Err(JournalStoreError::invalid(
                        &expected.path,
                        "workspace handle does not match the store-owned discovery authority",
                    ));
                }
                JournalStoreCapabilities::active(
                    self.capabilities.project_root_path,
                    self.capabilities.project_root,
                    self.capabilities.held_coordination_parent_identity,
                    self.capabilities.held_write_lock_identity,
                    self.capabilities.write_lock,
                    self.capabilities.workspace_parent,
                    DirectoryEndpoint::new(
                        self.capabilities.workspace_parent.directory,
                        Path::new(&expected.name),
                        &opened.directory,
                        &expected.path,
                    ),
                )
            }
            (None, None) => JournalStoreCapabilities::finalization_only(
                self.capabilities.project_root_path,
                self.capabilities.project_root,
                self.capabilities.held_coordination_parent_identity,
                self.capabilities.held_write_lock_identity,
                self.capabilities.write_lock,
                self.capabilities.workspace_parent,
            ),
            (Some(expected), None) => {
                return Err(JournalStoreError::invalid(
                    &expected.path,
                    "discovered active namespace requires its exact workspace handle",
                ));
            }
            (None, Some(_)) => {
                return Err(JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "finalization-only discovery cannot accept a caller-selected workspace",
                ));
            }
        };
        JournalRecoveryStore::bind(
            self.runtime,
            self.top_level.transaction_id.clone(),
            self.canonical_root_hash.clone(),
            capabilities,
        )
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
    /// Strict, stable read-only discovery for `check`/`doctor` surfaces.  It
    /// identifies zero or one journal-v2 transaction but does not confer
    /// mutation authority.
    pub(super) fn inspect_top_level(
        runtime: &TransactionRuntime,
        workspace_parent: DirectoryEndpoint<'_>,
    ) -> Result<JournalTopLevelNamespace, JournalStoreError> {
        require_discovery_platform(workspace_parent.path)?;
        require_child_name(workspace_parent.name, workspace_parent.path)?;
        let before = capture_top_level(runtime, workspace_parent, false)?;
        let after = capture_top_level(runtime, workspace_parent, false)?;
        if before.inventory != after.inventory || before.namespace != after.namespace {
            return Err(JournalStoreError::invalid(
                workspace_parent.path,
                "top-level journal namespace changed during stable discovery",
            ));
        }
        Ok(before.namespace)
    }

    /// Performs strict discovery while proving the held lock, project, and
    /// workspace-parent authority needed for a later store bind.  The returned
    /// transaction is the recovery-facing path to construct a store from the
    /// discovered identifier.
    pub(super) fn discover(
        runtime: &'a TransactionRuntime,
        canonical_root_hash: Sha256Digest,
        capabilities: JournalDiscoveryCapabilities<'a>,
    ) -> Result<DiscoveredJournalNamespace<'a>, JournalStoreError> {
        require_discovery_platform(capabilities.workspace_parent.path)?;
        require_child_name(capabilities.write_lock.name, capabilities.write_lock.path)?;
        require_child_name(
            capabilities.workspace_parent.name,
            capabilities.workspace_parent.path,
        )?;
        if capabilities.write_lock.name != Path::new(WRITE_LOCK_NAME) {
            return Err(JournalStoreError::invalid(
                capabilities.write_lock.path,
                "journal discovery must be bound to the persistent .write.lock child",
            ));
        }
        exact_directory(&capabilities.project_root)
            .map_err(|error| JournalStoreError::model(capabilities.project_root_path, error))?;

        let before = capture_bound_top_level(runtime, capabilities)?;
        let after = capture_bound_top_level(runtime, capabilities)?;
        if before.top_level.inventory != after.top_level.inventory
            || before.top_level.namespace != after.top_level.namespace
            || before.write_lock != after.write_lock
        {
            return Err(JournalStoreError::invalid(
                capabilities.workspace_parent.path,
                "journal discovery authority changed during bounded validation",
            ));
        }
        match before.top_level.namespace {
            JournalTopLevelNamespace::Empty => Ok(DiscoveredJournalNamespace::Empty),
            JournalTopLevelNamespace::Transaction(top_level) => Ok(
                DiscoveredJournalNamespace::Transaction(DiscoveredJournalTransaction {
                    runtime,
                    canonical_root_hash,
                    capabilities,
                    top_level,
                }),
            ),
        }
    }

    pub(super) fn bind(
        runtime: &'a TransactionRuntime,
        transaction_id: TransactionId,
        canonical_root_hash: Sha256Digest,
        capabilities: JournalStoreCapabilities<'a>,
    ) -> Result<Self, JournalStoreError> {
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
                    self.authenticate_workspace_owners(workspace, None)?;
                    let bootstrap = self.validate_bootstrap_syntax(&capture, workspace)?;
                    self.recapture_matches(&capture, false)?;
                    Ok(JournalNamespace::Bootstrap(LoadedBootstrap { bootstrap }))
                } else {
                    match self.load_active()? {
                        ActiveJournalLoad::Stable(lineage) => Ok(JournalNamespace::Active(lineage)),
                        ActiveJournalLoad::ReconciliationRequired(reconciliation) => {
                            Ok(JournalNamespace::ActiveReconciliation(reconciliation))
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
            if (!is_overlap && exact.link_count() != 1) || (is_overlap && exact.link_count() != 2) {
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
            let canonical_bytes = snapshot
                .record_envelope_bytes()
                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            if read.bytes != canonical_bytes {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "journal record bytes are valid JSON but not the canonical immutable envelope",
                ));
            }
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

        // Authenticate the complete live bootstrap authority before a partial
        // is classified.  This is what makes an empty or header-truncated
        // canonical next partial safe to identify as transaction-owned rather
        // than treating its unauthenticated bytes as deletion authority.
        let bootstrap = self.validate_bootstrap_syntax(&before, workspace_namespace)?;
        if let Some(project) = snapshots.first().map(JournalSnapshotV2::project) {
            let project_bootstrap = self.load_bootstrap(&before, workspace_namespace, project)?;
            if project_bootstrap != bootstrap {
                return Err(JournalStoreError::invalid(
                    self.workspace_path(),
                    "bootstrap syntax and immutable project lineage disagree",
                ));
            }
        }
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
        if snapshots.is_empty() {
            if self.validate_bootstrap_syntax(&before, workspace_namespace)? != bootstrap {
                return Err(JournalStoreError::invalid(
                    self.workspace_path(),
                    "bootstrap authority changed during empty-lineage validation",
                ));
            }
        } else {
            self.revalidate_loaded_content(
                &before,
                workspace_namespace,
                &snapshots,
                None,
                &bootstrap,
            )?;
        }
        self.authenticate_workspace_owners(workspace_namespace, snapshots.last())?;

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
                match self.load_completed_partial(partial, snapshots.last(), &bootstrap)? {
                    PartialLoad::Complete(completed) => {
                        self.validate_snapshot_authority(
                            completed.snapshot(),
                            &before,
                            &bootstrap,
                            &partial.path,
                        )?;
                        Some(completed)
                    }
                    PartialLoad::Incomplete(world) => {
                        // Inventory stability alone does not prove content
                        // stability.  Recapture every authority and then
                        // repeat the bounded exact read/classification before
                        // returning typed discard authority.
                        self.recapture_matches(&before, true)?;
                        if self.validate_bootstrap_syntax(&before, workspace_namespace)?
                            != bootstrap
                        {
                            return Err(JournalStoreError::invalid(
                                self.workspace_path(),
                                "bootstrap authority changed while authenticating an incomplete partial",
                            ));
                        }
                        let confirmed =
                            self.load_completed_partial(partial, snapshots.last(), &bootstrap)?;
                        if confirmed != PartialLoad::Incomplete(world.clone()) {
                            return Err(JournalStoreError::invalid(
                                &partial.path,
                                "journal partial changed during stable ownership classification",
                            ));
                        }
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

        if partial.is_some() {
            self.revalidate_loaded_content(
                &before,
                workspace_namespace,
                &snapshots,
                partial.as_ref(),
                &bootstrap,
            )?;
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

    /// Makes an authenticated linked published record durable, removes its
    /// exact partial alias, durably syncs that cleanup, and reloads the full
    /// contiguous lineage.  Every failure after the first durability barrier
    /// is represented by `ActiveReconciliationDisposition`.
    pub(super) fn adopt_active_publication(
        &self,
        reconciliation: &ActiveReconciliation,
    ) -> Result<ActiveReconciliationDisposition, JournalStoreError> {
        let (expected_partial, expected_published) = match &reconciliation.world {
            ObservedCandidateWorld::LinkedAliases { partial, published } => (partial, published),
            _ => {
                return Err(JournalStoreError::invalid(
                    self.workspace_path(),
                    "published-record adoption requires an authenticated linked-alias world",
                ));
            }
        };
        let current = self.load_active()?;
        if current != ActiveJournalLoad::ReconciliationRequired(reconciliation.clone()) {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "active publication authority changed before adoption",
            ));
        }

        let capture = self.capture_authority(true)?;
        let namespace = capture
            .workspace_namespace
            .as_ref()
            .ok_or_else(|| self.missing_workspace_error())?;
        let partial_entry = namespace.partial.as_ref().ok_or_else(|| {
            JournalStoreError::invalid(
                self.workspace_path(),
                "linked publication lost its partial alias before adoption",
            )
        })?;
        let published_entry = namespace
            .published
            .get(&reconciliation.sequence)
            .ok_or_else(|| {
                JournalStoreError::invalid(
                    self.workspace_path(),
                    "linked publication lost its published alias before adoption",
                )
            })?;
        if partial_entry.sequence != reconciliation.sequence {
            return Err(JournalStoreError::invalid(
                &partial_entry.path,
                "linked publication partial no longer has the reconciled sequence",
            ));
        }
        let partial_read = self.read_inventory_file(
            self.workspace_directory()?,
            partial_entry,
            MAX_RECORD_ENVELOPE_BYTES,
        )?;
        let published_read = self.read_inventory_file(
            self.workspace_directory()?,
            published_entry,
            MAX_RECORD_ENVELOPE_BYTES,
        )?;
        if &partial_read.observation != expected_partial
            || &published_read.observation != expected_published
            || partial_read.bytes != published_read.bytes
        {
            return Err(JournalStoreError::invalid(
                &partial_entry.path,
                "linked publication changed after authenticated lineage loading",
            ));
        }
        let candidate = JournalSnapshotV2::from_record_envelope_slice(&published_read.bytes)
            .map_err(|error| JournalStoreError::model(&published_entry.path, error))?;
        if candidate.transaction_id() != &self.transaction_id
            || candidate.sequence() != reconciliation.sequence
            || candidate.partial_name() != partial_entry.name
            || candidate.record_name() != published_entry.name
            || candidate
                .record_envelope_bytes()
                .map_err(|error| JournalStoreError::model(&published_entry.path, error))?
                != published_read.bytes
        {
            return Err(JournalStoreError::invalid(
                &published_entry.path,
                "linked publication is not the canonical reconciled transaction record",
            ));
        }

        let boundary = publication_boundary(&candidate);
        let outcome = if candidate.phase().desired_state_is_irreversible() {
            TransactionOutcome::Commit
        } else {
            TransactionOutcome::Rollback
        };
        let workspace = self
            .capabilities
            .workspace
            .ok_or_else(|| self.missing_workspace_error())?;
        self.runtime
            .observe(publication_transition(boundary, TransitionWindow::Before));
        let parent_observation = match self.runtime.fs().observe_directory(workspace) {
            Ok(observation) => observation,
            Err(source) => {
                return Ok(self.active_reconciliation_required(
                    ActiveReconciliationAction::AdoptPublished,
                    reconciliation.sequence,
                    DurabilityKnowledge::VisibilityOrDurabilityUnknown,
                    ActiveReconciliationMutation::ObservePublishedParent,
                    source,
                ));
            }
        };
        if let Err(source) =
            self.runtime
                .fs()
                .sync_parent(workspace, &parent_observation, ParentSyncKind::Journal)
        {
            return Ok(self.active_reconciliation_required(
                ActiveReconciliationAction::AdoptPublished,
                reconciliation.sequence,
                DurabilityKnowledge::VisibilityOrDurabilityUnknown,
                ActiveReconciliationMutation::SyncPublishedParent,
                source,
            ));
        }

        let partial_exact = match exact_file(&partial_read.observation) {
            Ok(exact) => exact,
            Err(error) => {
                return Ok(self.active_reconciliation_conflict(
                    ActiveReconciliationAction::AdoptPublished,
                    reconciliation.sequence,
                    DurabilityKnowledge::DurableRecord,
                    &format!(
                        "linked partial became unrepresentable after durability sync: {error}"
                    ),
                ));
            }
        };
        let removal = match self.remove_exact_file(
            workspace,
            &partial_entry.name,
            &partial_exact,
            MAX_RECORD_ENVELOPE_BYTES,
            RemovalObject::JournalPartial {
                sequence: reconciliation.sequence,
            },
            outcome,
        ) {
            Ok(removal) => removal,
            Err(error) => {
                return Ok(self.active_reconciliation_observation_unavailable(
                    ActiveReconciliationAction::AdoptPublished,
                    reconciliation.sequence,
                    DurabilityKnowledge::DurableRecord,
                    ActiveReconciliationMutation::RemovePartial,
                    error,
                ));
            }
        };
        if let ExactRemovalDisposition::ReconcileRequired(removal) = removal {
            if matches!(removal.world, RemovalWorld::Missing) {
                let cleanup_parent = match self.runtime.fs().observe_directory(workspace) {
                    Ok(observation) => observation,
                    Err(source) => {
                        return Ok(self.active_reconciliation_required(
                            ActiveReconciliationAction::AdoptPublished,
                            reconciliation.sequence,
                            DurabilityKnowledge::DurableRecord,
                            ActiveReconciliationMutation::ObserveCleanupParent,
                            source,
                        ));
                    }
                };
                if let Err(source) = self.runtime.fs().sync_parent(
                    workspace,
                    &cleanup_parent,
                    ParentSyncKind::Journal,
                ) {
                    return Ok(self.active_reconciliation_required(
                        ActiveReconciliationAction::AdoptPublished,
                        reconciliation.sequence,
                        DurabilityKnowledge::DurableRecord,
                        ActiveReconciliationMutation::SyncCleanupParent,
                        source,
                    ));
                }
                self.runtime.observe(removal_transition(
                    RemovalObject::JournalPartial {
                        sequence: reconciliation.sequence,
                    },
                    outcome,
                    TransitionWindow::After,
                ));
            } else {
                let mutation = active_removal_mutation(removal.mutation);
                return Ok(self.active_reconciliation_required(
                    ActiveReconciliationAction::AdoptPublished,
                    reconciliation.sequence,
                    DurabilityKnowledge::DurableRecord,
                    mutation,
                    removal.source,
                ));
            }
        }

        match self.load_active() {
            Ok(ActiveJournalLoad::Stable(loaded))
                if loaded.snapshots().iter().any(|snapshot| {
                    snapshot.sequence() == reconciliation.sequence && snapshot == &candidate
                }) && loaded.partial().is_none() =>
            {
                self.runtime
                    .observe(publication_transition(boundary, TransitionWindow::After));
                Ok(ActiveReconciliationDisposition::Durable)
            }
            Ok(ActiveJournalLoad::Stable(_)) => Ok(self.active_reconciliation_conflict(
                ActiveReconciliationAction::AdoptPublished,
                reconciliation.sequence,
                DurabilityKnowledge::DurableRecord,
                "reloaded lineage does not contain the adopted canonical record",
            )),
            Ok(ActiveJournalLoad::ReconciliationRequired(current)) => {
                Ok(ActiveReconciliationDisposition::ReconcileRequired {
                    reconciliation: ActiveMutationReconciliation {
                        action: ActiveReconciliationAction::AdoptPublished,
                        sequence: reconciliation.sequence,
                        durability: DurabilityKnowledge::DurableRecord,
                        mutation: ActiveReconciliationMutation::ReloadLineage,
                        world: current.world,
                        source: io::Error::new(
                            io::ErrorKind::Interrupted,
                            "adopted record still requires exact lineage reconciliation",
                        ),
                    },
                })
            }
            Err(error) => Ok(self.active_reconciliation_observation_unavailable(
                ActiveReconciliationAction::AdoptPublished,
                reconciliation.sequence,
                DurabilityKnowledge::DurableRecord,
                ActiveReconciliationMutation::ReloadLineage,
                error,
            )),
        }
    }

    /// Removes either a fully authenticated complete next partial or an
    /// ownership-header-authenticated incomplete partial, durably syncs the
    /// absence, and reloads the unchanged published lineage.
    pub(super) fn discard_active_partial(
        &self,
        loaded: &ActiveJournalLoad,
        outcome: TransactionOutcome,
    ) -> Result<ActiveReconciliationDisposition, JournalStoreError> {
        let current = self.load_active()?;
        if &current != loaded {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "active partial authority changed before exact discard",
            ));
        }
        let (sequence, stable_record_count, observation) = match &current {
            ActiveJournalLoad::Stable(lineage) => {
                let partial = lineage.partial.as_ref().ok_or_else(|| {
                    JournalStoreError::invalid(
                        self.workspace_path(),
                        "stable active lineage has no complete partial to discard",
                    )
                })?;
                (
                    partial.snapshot.sequence(),
                    lineage.records.len(),
                    partial.observation.clone(),
                )
            }
            ActiveJournalLoad::ReconciliationRequired(reconciliation) => {
                let partial = match &reconciliation.world {
                    ObservedCandidateWorld::OwnedIncomplete { partial, .. }
                    | ObservedCandidateWorld::PreparedOnly { partial } => partial.clone(),
                    _ => {
                        return Err(JournalStoreError::invalid(
                            self.workspace_path(),
                            "partial discard requires an authenticated owned partial without a published alias",
                        ));
                    }
                };
                (
                    reconciliation.sequence,
                    reconciliation.stable_record_count,
                    partial,
                )
            }
        };
        if observation.link_count != Some(1) {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "discarded active partial must have exactly one authenticated hard link",
            ));
        }
        let partial_name = journal_partial_name(&self.transaction_id, sequence);
        let partial_path = self.workspace_path().join(&partial_name);
        let partial_exact = exact_file(&observation)
            .map_err(|error| JournalStoreError::model(&partial_path, error))?;
        let workspace = self
            .capabilities
            .workspace
            .ok_or_else(|| self.missing_workspace_error())?;
        let removal = self.remove_exact_file(
            workspace,
            &partial_name,
            &partial_exact,
            MAX_RECORD_ENVELOPE_BYTES,
            RemovalObject::JournalPartial { sequence },
            outcome,
        )?;
        if let ExactRemovalDisposition::ReconcileRequired(removal) = removal {
            if matches!(removal.world, RemovalWorld::Missing) {
                let cleanup_parent = match self.runtime.fs().observe_directory(workspace) {
                    Ok(observation) => observation,
                    Err(source) => {
                        return Ok(self.active_reconciliation_required(
                            ActiveReconciliationAction::DiscardPartial,
                            sequence,
                            DurabilityKnowledge::NotPublished,
                            ActiveReconciliationMutation::ObserveCleanupParent,
                            source,
                        ));
                    }
                };
                if let Err(source) = self.runtime.fs().sync_parent(
                    workspace,
                    &cleanup_parent,
                    ParentSyncKind::Journal,
                ) {
                    return Ok(self.active_reconciliation_required(
                        ActiveReconciliationAction::DiscardPartial,
                        sequence,
                        DurabilityKnowledge::NotPublished,
                        ActiveReconciliationMutation::SyncCleanupParent,
                        source,
                    ));
                }
                self.runtime.observe(removal_transition(
                    RemovalObject::JournalPartial { sequence },
                    outcome,
                    TransitionWindow::After,
                ));
            } else {
                let mutation = active_removal_mutation(removal.mutation);
                return Ok(self.active_reconciliation_required(
                    ActiveReconciliationAction::DiscardPartial,
                    sequence,
                    DurabilityKnowledge::NotPublished,
                    mutation,
                    removal.source,
                ));
            }
        }
        match self.load_active() {
            Ok(ActiveJournalLoad::Stable(loaded))
                if loaded.records.len() == stable_record_count && loaded.partial.is_none() =>
            {
                Ok(ActiveReconciliationDisposition::Durable)
            }
            Ok(ActiveJournalLoad::Stable(_)) => Ok(self.active_reconciliation_conflict(
                ActiveReconciliationAction::DiscardPartial,
                sequence,
                DurabilityKnowledge::NotPublished,
                "reloaded lineage changed while discarding an unpublished partial",
            )),
            Ok(ActiveJournalLoad::ReconciliationRequired(current)) => {
                Ok(ActiveReconciliationDisposition::ReconcileRequired {
                    reconciliation: ActiveMutationReconciliation {
                        action: ActiveReconciliationAction::DiscardPartial,
                        sequence,
                        durability: DurabilityKnowledge::NotPublished,
                        mutation: ActiveReconciliationMutation::ReloadLineage,
                        world: current.world,
                        source: io::Error::new(
                            io::ErrorKind::Interrupted,
                            "partial discard still requires exact lineage reconciliation",
                        ),
                    },
                })
            }
            Err(error) => Ok(self.active_reconciliation_observation_unavailable(
                ActiveReconciliationAction::DiscardPartial,
                sequence,
                DurabilityKnowledge::NotPublished,
                ActiveReconciliationMutation::ReloadLineage,
                error,
            )),
        }
    }

    fn active_reconciliation_required(
        &self,
        action: ActiveReconciliationAction,
        sequence: u64,
        durability: DurabilityKnowledge,
        mutation: ActiveReconciliationMutation,
        source: io::Error,
    ) -> ActiveReconciliationDisposition {
        ActiveReconciliationDisposition::ReconcileRequired {
            reconciliation: ActiveMutationReconciliation {
                action,
                sequence,
                durability,
                mutation,
                world: self.probe_active_sequence(sequence),
                source,
            },
        }
    }

    fn active_reconciliation_conflict(
        &self,
        action: ActiveReconciliationAction,
        sequence: u64,
        durability: DurabilityKnowledge,
        reason: &str,
    ) -> ActiveReconciliationDisposition {
        ActiveReconciliationDisposition::ReconcileRequired {
            reconciliation: ActiveMutationReconciliation {
                action,
                sequence,
                durability,
                mutation: ActiveReconciliationMutation::ReloadLineage,
                world: ObservedCandidateWorld::Conflict {
                    reason: reason.to_owned(),
                    partial: None,
                    published: None,
                },
                source: io::Error::new(io::ErrorKind::InvalidData, reason),
            },
        }
    }

    fn active_reconciliation_observation_unavailable(
        &self,
        action: ActiveReconciliationAction,
        sequence: u64,
        durability: DurabilityKnowledge,
        mutation: ActiveReconciliationMutation,
        error: JournalStoreError,
    ) -> ActiveReconciliationDisposition {
        let reason = error.to_string();
        ActiveReconciliationDisposition::ReconcileRequired {
            reconciliation: ActiveMutationReconciliation {
                action,
                sequence,
                durability,
                mutation,
                world: ObservedCandidateWorld::ObservationUnavailable {
                    reason: reason.clone(),
                },
                source: io::Error::other(reason),
            },
        }
    }

    fn probe_active_sequence(&self, sequence: u64) -> ObservedCandidateWorld {
        match self.load_active() {
            Ok(ActiveJournalLoad::ReconciliationRequired(reconciliation))
                if reconciliation.sequence == sequence =>
            {
                reconciliation.world
            }
            Ok(ActiveJournalLoad::ReconciliationRequired(reconciliation)) => {
                ObservedCandidateWorld::Conflict {
                    reason: format!(
                        "active reconciliation moved from sequence {sequence} to sequence {}",
                        reconciliation.sequence
                    ),
                    partial: None,
                    published: None,
                }
            }
            Ok(ActiveJournalLoad::Stable(loaded)) => {
                if let Some(partial) = loaded
                    .partial
                    .as_ref()
                    .filter(|partial| partial.snapshot.sequence() == sequence)
                {
                    return ObservedCandidateWorld::PreparedOnly {
                        partial: partial.observation.clone(),
                    };
                }
                if let Some(record) = loaded
                    .records
                    .iter()
                    .find(|record| record.sequence() == sequence)
                {
                    return ObservedCandidateWorld::PublishedOnly {
                        published: exact_file_observation(record.exact()),
                    };
                }
                ObservedCandidateWorld::Missing
            }
            Err(error) => ObservedCandidateWorld::ObservationUnavailable {
                reason: error.to_string(),
            },
        }
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

    fn load_finalization_from_capture(
        &self,
        capture: &AuthorityCapture,
    ) -> Result<LoadedFinalization, JournalStoreError> {
        let namespace = &capture.parent_namespace;
        if namespace.finalization.len() > 2 {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "closed finalization model permits at most workspace-present and workspace-removed generations",
            ));
        }
        let overlap_generation =
            namespace
                .finalization_partial
                .as_ref()
                .and_then(|(generation, _)| {
                    namespace
                        .finalization
                        .contains_key(generation)
                        .then_some(*generation)
                });
        let parent_exact =
            exact_directory(&capture.parent_inventory.directory).map_err(|error| {
                JournalStoreError::model(self.capabilities.workspace_parent.path, error)
            })?;
        let root_exact = exact_directory(&capture.root).map_err(|error| {
            JournalStoreError::model(self.capabilities.project_root_path, error)
        })?;
        let write_lock_exact = exact_file(&capture.write_lock.observation)
            .map_err(|error| JournalStoreError::model(self.capabilities.write_lock.path, error))?;
        let first_generation = namespace.finalization.keys().next().copied();
        let terminal_suffix = first_generation == Some(1);
        if terminal_suffix
            && (namespace.finalization.len() != 1 || namespace.finalization_partial.is_some())
        {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "a retired finalization prefix may retain only the lone workspace-removed generation-one record",
            ));
        }
        let mut history: Vec<FinalizationRecord> = Vec::with_capacity(namespace.finalization.len());
        let mut history_bytes = Vec::with_capacity(namespace.finalization.len());
        for (index, (generation, entry)) in namespace.finalization.iter().enumerate() {
            let expected_generation = first_generation.unwrap_or(0) + index as u64;
            if *generation != expected_generation
                || (!terminal_suffix && first_generation.is_some_and(|first| first != 0))
            {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    format!(
                        "finalization lineage is not an accepted contiguous generation-zero lineage or terminal generation-one suffix: expected generation {expected_generation}, found {generation}"
                    ),
                ));
            }
            let read = self.read_inventory_file(
                self.capabilities.workspace_parent.directory,
                entry,
                MAX_RECORD_ENVELOPE_BYTES,
            )?;
            let exact = exact_file(&read.observation)
                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            require_private_exact_file(&exact, "finalization record", &entry.path)?;
            let expected_links = if overlap_generation == Some(*generation) {
                2
            } else {
                1
            };
            if exact.link_count() != expected_links {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "finalization record has an invalid immutable-publication link count",
                ));
            }
            let lease = FinalizationLeaseV2::from_json_slice(&read.bytes)
                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            let canonical = lease
                .to_json_bytes()
                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            if read.bytes != canonical
                || lease.transaction_id() != &self.transaction_id
                || lease.canonical_root_hash() != &self.canonical_root_hash
                || lease.root() != &root_exact
                || lease.write_lock() != &write_lock_exact
                || model_identity(lease.coordination_parent().identity())
                    != self.capabilities.held_coordination_parent_identity
                || lease.generation() != *generation
                || lease.record_name() != entry.name
            {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "finalization record is not canonical and bound to the exact project/parent/generation authority",
                ));
            }
            if let Some(previous) = history.last() {
                previous
                    .lease
                    .validate_successor(&lease)
                    .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            } else if terminal_suffix && lease.state() != FinalizationStateV2::WorkspaceRemoved {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "lone generation-one finalization suffix is not the closed workspace-removed authority",
                ));
            }
            history_bytes.push(canonical);
            history.push(FinalizationRecord {
                lease,
                exact,
                observation: read.observation,
                name: entry.name.clone(),
            });
        }

        let mut partial = None;
        let mut reconciliation = None;
        if let Some((generation, entry)) = &namespace.finalization_partial {
            let read = self.read_inventory_file(
                self.capabilities.workspace_parent.directory,
                entry,
                MAX_RECORD_ENVELOPE_BYTES,
            )?;
            if overlap_generation == Some(*generation) {
                let published = history
                    .iter()
                    .find(|record| record.lease.generation() == *generation)
                    .ok_or_else(|| {
                        JournalStoreError::invalid(
                            &entry.path,
                            "overlapping finalization partial has no published generation",
                        )
                    })?;
                if read.bytes
                    != published
                        .lease
                        .to_json_bytes()
                        .map_err(|error| JournalStoreError::model(&entry.path, error))?
                    || !matches!(
                        classify_candidate_world(
                            Some(read.observation.clone()),
                            Some(published.observation.clone()),
                        ),
                        ObservedCandidateWorld::LinkedAliases { .. }
                    )
                {
                    return Err(JournalStoreError::invalid(
                        &entry.path,
                        "overlapping finalization names are not exact canonical hard-link aliases",
                    ));
                }
                let exact = exact_file(&read.observation)
                    .map_err(|error| JournalStoreError::model(&entry.path, error))?;
                require_private_exact_file(&exact, "finalization partial", &entry.path)?;
                if exact.link_count() != 2 {
                    return Err(JournalStoreError::invalid(
                        &entry.path,
                        "overlapping finalization partial must be the exact two-link published alias",
                    ));
                }
                partial = Some(FinalizationRecord {
                    lease: published.lease.clone(),
                    exact,
                    observation: read.observation,
                    name: entry.name.clone(),
                });
                reconciliation = Some(FinalizationWorld::LinkedAliases {
                    generation: *generation,
                });
            } else {
                let expected_generation = history
                    .last()
                    .map_or(0, |record| record.lease.generation() + 1);
                if *generation != expected_generation {
                    return Err(JournalStoreError::invalid(
                        &entry.path,
                        format!(
                            "finalization partial must be next generation {expected_generation}"
                        ),
                    ));
                }
                match FinalizationLeaseV2::from_json_slice(&read.bytes) {
                    Ok(lease) => {
                        let canonical = lease
                            .to_json_bytes()
                            .map_err(|error| JournalStoreError::model(&entry.path, error))?;
                        if read.bytes != canonical
                            || lease.transaction_id() != &self.transaction_id
                            || lease.canonical_root_hash() != &self.canonical_root_hash
                            || lease.generation() != *generation
                            || lease.partial_name() != entry.name
                        {
                            return Err(JournalStoreError::invalid(
                                &entry.path,
                                "finalization partial is not its canonical authority-bound successor",
                            ));
                        }
                        if let Some(previous) = history.last() {
                            previous
                                .lease
                                .validate_successor(&lease)
                                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
                        } else if lease.generation() != 0 {
                            return Err(JournalStoreError::invalid(
                                &entry.path,
                                "nonzero finalization partial has no durable predecessor",
                            ));
                        }
                        let exact = exact_file(&read.observation)
                            .map_err(|error| JournalStoreError::model(&entry.path, error))?;
                        require_private_exact_file(&exact, "finalization partial", &entry.path)?;
                        if exact.link_count() != 1 {
                            return Err(JournalStoreError::invalid(
                                &entry.path,
                                "prepared finalization partial must have one hard link",
                            ));
                        }
                        partial = Some(FinalizationRecord {
                            lease,
                            exact,
                            observation: read.observation,
                            name: entry.name.clone(),
                        });
                        reconciliation = Some(FinalizationWorld::PreparedNext {
                            generation: *generation,
                        });
                    }
                    Err(error) => {
                        reconciliation = Some(FinalizationWorld::Conflict {
                            generation: *generation,
                            reason: format!(
                                "finalization partial is incomplete or corrupt and remains evidence: {error}"
                            ),
                        });
                    }
                }
            }
        }

        if let Some(latest) = history.last() {
            if reconciliation.is_none() {
                let stage = self.validate_finalization_live_world(capture, latest)?;
                self.validate_finalization_parent_world(&parent_exact, latest, stage)?;
                if stage != FinalizationCleanupStage::CompleteManifest {
                    reconciliation = Some(FinalizationWorld::CleanupProgress { stage });
                }
            } else {
                // Even a publication residual must not mask an unsafe cleanup
                // subset or substituted live object.
                let stage = self.validate_finalization_live_world(capture, latest)?;
                self.validate_finalization_parent_world(&parent_exact, latest, stage)?;
            }
        }
        if matches!(reconciliation, Some(FinalizationWorld::PreparedNext { .. })) {
            let prepared = partial.as_ref().ok_or_else(|| {
                JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "prepared finalization classification has no exact partial binding",
                )
            })?;
            let stage = self.validate_finalization_live_world(capture, prepared)?;
            self.validate_finalization_parent_world(&parent_exact, prepared, stage)?;
        }
        for (record, bytes) in history.iter().zip(history_bytes) {
            let entry = namespace
                .finalization
                .get(&record.lease.generation())
                .expect("loaded finalization generation remains inventoried");
            let reread = self.read_inventory_file(
                self.capabilities.workspace_parent.directory,
                entry,
                MAX_RECORD_ENVELOPE_BYTES,
            )?;
            if reread.bytes != bytes || reread.observation != record.observation {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "finalization authority changed during bounded validation",
                ));
            }
        }
        if let Some(partial_record) = &partial {
            let (_, entry) = namespace.finalization_partial.as_ref().ok_or_else(|| {
                JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "loaded finalization partial disappeared during bounded validation",
                )
            })?;
            let reread = self.read_inventory_file(
                self.capabilities.workspace_parent.directory,
                entry,
                MAX_RECORD_ENVELOPE_BYTES,
            )?;
            let expected_bytes = partial_record
                .lease
                .to_json_bytes()
                .map_err(|error| JournalStoreError::model(&entry.path, error))?;
            if reread.bytes != expected_bytes || reread.observation != partial_record.observation {
                return Err(JournalStoreError::invalid(
                    &entry.path,
                    "finalization partial changed during bounded validation",
                ));
            }
        }
        self.recapture_matches(capture, false)?;
        Ok(LoadedFinalization {
            history,
            partial,
            reconciliation,
        })
    }

    fn validate_finalization_parent_world(
        &self,
        live_parent: &ExactDirectoryStateV2,
        latest: &FinalizationRecord,
        stage: FinalizationCleanupStage,
    ) -> Result<(), JournalStoreError> {
        if latest.lease.state() == FinalizationStateV2::WorkspaceRemoved {
            if latest.lease.workspace_parent_current() != live_parent {
                return Err(JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "workspace-removed finalization authority does not bind the exact live parent",
                ));
            }
            return Ok(());
        }
        if stage == FinalizationCleanupStage::WorkspaceRemoved {
            latest
                .lease
                .mark_workspace_removed(live_parent.clone())
                .map_err(|error| {
                    JournalStoreError::model(self.capabilities.workspace_parent.path, error)
                })?;
            return Ok(());
        }
        if latest.lease.workspace_parent_current() != live_parent {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "workspace-present finalization authority does not bind the exact live parent",
            ));
        }
        Ok(())
    }

    fn validate_finalization_live_world(
        &self,
        capture: &AuthorityCapture,
        latest: &FinalizationRecord,
    ) -> Result<FinalizationCleanupStage, JournalStoreError> {
        match latest.lease.workspace().as_present() {
            Some(expected_workspace) => {
                let Some(inventory) = capture.workspace_inventory.as_ref() else {
                    if capture.parent_namespace.workspace.is_some()
                        || capture.parent_namespace.bootstrap_intent.is_some()
                    {
                        return Err(JournalStoreError::invalid(
                            self.capabilities.workspace_parent.path,
                            "workspace cleanup state is missing a required exact workspace capability or retains an intent after workspace removal",
                        ));
                    }
                    return Ok(FinalizationCleanupStage::WorkspaceRemoved);
                };
                let actual = exact_directory(&inventory.directory)
                    .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?;
                if &actual != expected_workspace {
                    return Err(JournalStoreError::invalid(
                        self.workspace_path(),
                        "workspace-present finalization authority does not match the live workspace",
                    ));
                }
                let namespace = capture.workspace_namespace.as_ref().ok_or_else(|| {
                    JournalStoreError::invalid(
                        self.workspace_path(),
                        "workspace cleanup state has no strict child inventory",
                    )
                })?;
                if let Some((_, owner)) = namespace.owners.first_key_value() {
                    return Err(JournalStoreError::invalid(
                        &owner.entry.path,
                        "finalization cannot begin or advance while a transaction owner artifact remains",
                    ));
                }

                let intent_present = match &capture.parent_namespace.bootstrap_intent {
                    Some(entry) => {
                        self.require_manifest_file(
                            self.capabilities.workspace_parent.directory,
                            entry,
                            latest.lease.bootstrap().intent().exact(),
                            MAX_CONTROL_ENVELOPE_BYTES,
                            "bootstrap intent",
                        )?;
                        true
                    }
                    None => false,
                };
                let owner_present = match &namespace.bootstrap_owner {
                    Some(entry) => {
                        self.require_manifest_file(
                            self.workspace_directory()?,
                            entry,
                            latest.lease.bootstrap().exact(),
                            MAX_CONTROL_ENVELOPE_BYTES,
                            "bootstrap owner",
                        )?;
                        true
                    }
                    None => false,
                };

                let manifest_records = latest.lease.records();
                let remaining_records = namespace.published.len();
                if remaining_records > manifest_records.len() {
                    return Err(JournalStoreError::invalid(
                        self.workspace_path(),
                        "live journal history is not a subset of the finalization manifest",
                    ));
                }
                for (index, (sequence, entry)) in namespace.published.iter().enumerate() {
                    if *sequence != index as u64 {
                        return Err(JournalStoreError::invalid(
                            &entry.path,
                            "cleanup must remove immutable journal history monotonically from newest to oldest",
                        ));
                    }
                    let manifest = manifest_records.get(index).ok_or_else(|| {
                        JournalStoreError::invalid(
                            &entry.path,
                            "live journal record is absent from the finalization manifest",
                        )
                    })?;
                    if manifest.sequence() != *sequence || manifest.name() != entry.name {
                        return Err(JournalStoreError::invalid(
                            &entry.path,
                            "live journal record name/sequence is not manifest-bound",
                        ));
                    }
                    self.require_manifest_file(
                        self.workspace_directory()?,
                        entry,
                        manifest.exact(),
                        MAX_RECORD_ENVELOPE_BYTES,
                        "journal record",
                    )?;
                }

                let partial_present = match (&namespace.partial, latest.lease.partial()) {
                    (Some(entry), Some(manifest)) => {
                        if entry.sequence != manifest.sequence() || entry.name != manifest.name() {
                            return Err(JournalStoreError::invalid(
                                &entry.path,
                                "live journal partial is not the exact finalization-manifest partial",
                            ));
                        }
                        self.require_manifest_file(
                            self.workspace_directory()?,
                            entry,
                            manifest.exact(),
                            MAX_RECORD_ENVELOPE_BYTES,
                            "journal partial",
                        )?;
                        true
                    }
                    (None, _) => false,
                    (Some(entry), None) => {
                        return Err(JournalStoreError::invalid(
                            &entry.path,
                            "unmanifested journal partial remains during finalization",
                        ));
                    }
                };

                let all_records_present = remaining_records == manifest_records.len();
                let manifest_partial_present = latest.lease.partial().is_some();
                if intent_present
                    && (!owner_present
                        || !all_records_present
                        || partial_present != manifest_partial_present)
                {
                    return Err(JournalStoreError::invalid(
                        self.workspace_path(),
                        "cleanup skipped ahead while the bootstrap intent is still present",
                    ));
                }
                if owner_present
                    && (!all_records_present || partial_present != manifest_partial_present)
                {
                    return Err(JournalStoreError::invalid(
                        self.workspace_path(),
                        "cleanup skipped journal evidence before removing workspace ownership",
                    ));
                }
                if !owner_present && partial_present && !all_records_present {
                    return Err(JournalStoreError::invalid(
                        self.workspace_path(),
                        "cleanup must remove the manifest partial before published history",
                    ));
                }

                let stage = if intent_present {
                    FinalizationCleanupStage::CompleteManifest
                } else if owner_present {
                    FinalizationCleanupStage::IntentRemoved
                } else if all_records_present && partial_present == manifest_partial_present {
                    FinalizationCleanupStage::OwnershipRemoved
                } else if manifest_partial_present && all_records_present {
                    FinalizationCleanupStage::PartialRemoved
                } else if remaining_records > 0 {
                    FinalizationCleanupStage::HistoryRemoving { remaining_records }
                } else {
                    FinalizationCleanupStage::WorkspaceEmpty
                };

                if latest.lease.generation() == 0
                    && stage == FinalizationCleanupStage::CompleteManifest
                {
                    let expected = if latest.lease.records().is_empty() {
                        let workspace = capture.workspace_namespace.as_ref().ok_or_else(|| {
                            JournalStoreError::invalid(
                                self.workspace_path(),
                                "bootstrap-abort finalization has no workspace namespace",
                            )
                        })?;
                        FinalizationLeaseV2::arm_bootstrap_abort(
                            self.validate_bootstrap_syntax(capture, workspace)?,
                        )
                        .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?
                    } else {
                        let loaded = match self.load_active()? {
                            ActiveJournalLoad::Stable(loaded) => loaded,
                            ActiveJournalLoad::ReconciliationRequired(_) => {
                                return Err(JournalStoreError::invalid(
                                    self.workspace_path(),
                                    "finalization cannot begin from an unreconciled journal publication",
                                ));
                            }
                        };
                        let terminal = loaded.latest().ok_or_else(|| {
                            JournalStoreError::invalid(
                                self.workspace_path(),
                                "generation-zero finalization has no terminal snapshot",
                            )
                        })?;
                        FinalizationLeaseV2::arm(
                            terminal,
                            loaded.records().to_vec(),
                            loaded.partial().map(|partial| partial.binding().clone()),
                        )
                        .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?
                    };
                    if latest.lease != expected {
                        return Err(JournalStoreError::invalid(
                            Path::new(&latest.name),
                            "generation-zero finalization authority is not the exact terminal journal manifest",
                        ));
                    }
                }
                Ok(stage)
            }
            None => {
                if capture.parent_namespace.workspace.is_some()
                    || capture.parent_namespace.bootstrap_intent.is_some()
                    || capture.workspace_inventory.is_some()
                {
                    return Err(JournalStoreError::invalid(
                        self.capabilities.workspace_parent.path,
                        "workspace-removed finalization tombstone conflicts with live workspace/bootstrap authority",
                    ));
                }
                Ok(FinalizationCleanupStage::WorkspaceRemoved)
            }
        }
    }

    fn require_manifest_file(
        &self,
        parent: &Dir,
        entry: &InventoryFile,
        expected: &ExactFileStateV2,
        max_bytes: u64,
        label: &str,
    ) -> Result<(), JournalStoreError> {
        let read = self.read_inventory_file(parent, entry, max_bytes)?;
        let actual = exact_file(&read.observation)
            .map_err(|error| JournalStoreError::model(&entry.path, error))?;
        if &actual != expected {
            return Err(JournalStoreError::invalid(
                &entry.path,
                format!("remaining {label} does not match its immutable finalization manifest"),
            ));
        }
        Ok(())
    }

    pub(super) fn publish_finalization(
        &self,
        previous: Option<&LoadedFinalization>,
        candidate: &FinalizationLeaseV2,
    ) -> Result<FinalizationDisposition, JournalStoreError> {
        let bytes = candidate.to_json_bytes().map_err(|error| {
            JournalStoreError::model(self.capabilities.workspace_parent.path, error)
        })?;
        if bytes.len() as u64 > MAX_RECORD_ENVELOPE_BYTES {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "canonical finalization lease exceeds the bounded envelope limit",
            ));
        }
        if candidate.transaction_id() != &self.transaction_id
            || candidate.canonical_root_hash() != &self.canonical_root_hash
        {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "finalization candidate is not bound to this project and transaction",
            ));
        }

        let capture = self.capture_authority(false)?;
        let root_exact = exact_directory(&capture.root).map_err(|error| {
            JournalStoreError::model(self.capabilities.project_root_path, error)
        })?;
        let write_lock_exact = exact_file(&capture.write_lock.observation)
            .map_err(|error| JournalStoreError::model(self.capabilities.write_lock.path, error))?;
        if candidate.root() != &root_exact
            || candidate.write_lock() != &write_lock_exact
            || model_identity(candidate.coordination_parent().identity())
                != self.capabilities.held_coordination_parent_identity
        {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "finalization candidate is not bound to the live root, coordination parent, and held write lock",
            ));
        }
        let current = self.load_finalization_from_capture(&capture)?;
        match previous {
            Some(previous) if previous != &current => {
                return Err(JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "loaded finalization authority changed before immutable publication",
                ));
            }
            None if !current.history.is_empty() || current.partial.is_some() => {
                return Err(JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "generation-zero finalization requires an empty finalization namespace",
                ));
            }
            _ => {}
        }

        if let Some(last) = current.latest() {
            last.lease.validate_successor(candidate).map_err(|error| {
                JournalStoreError::model(self.capabilities.workspace_parent.path, error)
            })?;
        } else if candidate.generation() != 0 {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "first immutable finalization record must be generation zero",
            ));
        }
        if candidate.generation() == 0 {
            let expected = if candidate.records().is_empty() {
                let capture = self.capture_authority(false)?;
                let workspace = capture.workspace_namespace.as_ref().ok_or_else(|| {
                    JournalStoreError::invalid(
                        self.workspace_path(),
                        "bootstrap-abort finalization has no workspace namespace",
                    )
                })?;
                FinalizationLeaseV2::arm_bootstrap_abort(
                    self.validate_bootstrap_syntax(&capture, workspace)?,
                )
                .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?
            } else {
                let active = match self.load_active()? {
                    ActiveJournalLoad::Stable(active) => active,
                    ActiveJournalLoad::ReconciliationRequired(reconciliation) => {
                        return Ok(FinalizationDisposition::ReconcileRequired {
                            reconciliation: FinalizationReconciliation {
                                generation: 0,
                                outcome: candidate.outcome(),
                                durability: DurabilityKnowledge::NotPublished,
                                mutation: StoreMutation::PublishImmutable,
                                world: reconciliation.world,
                                source: io::Error::new(
                                    io::ErrorKind::Interrupted,
                                    "journal publication must be reconciled before finalization",
                                ),
                            },
                        });
                    }
                };
                let terminal = active.latest().ok_or_else(|| {
                    JournalStoreError::invalid(
                        self.workspace_path(),
                        "generation-zero finalization requires a terminal journal snapshot",
                    )
                })?;
                FinalizationLeaseV2::arm(
                    terminal,
                    active.records().to_vec(),
                    active.partial().map(|partial| partial.binding().clone()),
                )
                .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?
            };
            if &expected != candidate {
                return Err(JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "generation-zero finalization candidate is not the exact terminal manifest",
                ));
            }
        } else if current.reconciliation()
            != Some(&FinalizationWorld::CleanupProgress {
                stage: FinalizationCleanupStage::WorkspaceRemoved,
            })
        {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "workspace-removed successor may publish only after exact monotonic cleanup reaches workspace absence",
            ));
        }

        let generation = candidate.generation();
        let outcome = candidate.outcome();
        let record_name = candidate.record_name();
        let record_path = self.capabilities.workspace_parent.path.join(&record_name);
        let partial_name = candidate.partial_name();
        let partial_path = self.capabilities.workspace_parent.path.join(&partial_name);
        self.runtime
            .observe(finalization_transition(candidate, TransitionWindow::Before));

        let prepared = if let Some(partial) = current.partial() {
            if partial.lease != *candidate {
                return Err(JournalStoreError::invalid(
                    &partial.name,
                    "existing finalization partial is not the requested canonical successor",
                ));
            }
            PreparedFinalization {
                observation: partial.observation.clone(),
            }
        } else {
            match self.prepare_finalization_file(candidate, &bytes)? {
                PrepareFinalizationDisposition::Durable(prepared) => prepared,
                PrepareFinalizationDisposition::ReconcileRequired(reconciliation) => {
                    return Ok(FinalizationDisposition::ReconcileRequired { reconciliation });
                }
            }
        };

        let parent_observation = match self
            .runtime
            .fs()
            .observe_directory(self.capabilities.workspace_parent)
        {
            Ok(observation) => observation,
            Err(source) => {
                return Ok(FinalizationDisposition::ReconcileRequired {
                    reconciliation: FinalizationReconciliation {
                        generation,
                        outcome,
                        durability: DurabilityKnowledge::NotPublished,
                        mutation: StoreMutation::ObservePartialParent,
                        world: self.probe_finalization_world(candidate),
                        source,
                    },
                });
            }
        };
        let publication = self.runtime.fs().publish_immutable(
            HardLinkEndpoint::new(
                self.capabilities.workspace_parent.directory,
                Path::new(&partial_name),
                &partial_path,
            ),
            &prepared.observation,
            HardLinkEndpoint::new(
                self.capabilities.workspace_parent.directory,
                Path::new(&record_name),
                &record_path,
            ),
            self.capabilities.workspace_parent,
            &parent_observation,
            ParentSyncKind::Journal,
        );
        match publication {
            ImmutablePublicationOutcome::Durable { published } => {
                let record = self.finalization_record(candidate, published, &record_path)?;
                self.runtime
                    .observe(finalization_transition(candidate, TransitionWindow::After));
                Ok(FinalizationDisposition::Durable { record })
            }
            ImmutablePublicationOutcome::NotPublished { partial, source } => {
                Ok(FinalizationDisposition::ReconcileRequired {
                    reconciliation: FinalizationReconciliation {
                        generation,
                        outcome,
                        durability: DurabilityKnowledge::NotPublished,
                        mutation: StoreMutation::PublishImmutable,
                        world: authenticate_expected_world(partial, None, &prepared.observation),
                        source,
                    },
                })
            }
            ImmutablePublicationOutcome::VisibleDurabilityUnknown {
                partial,
                published,
                source,
            } => Ok(FinalizationDisposition::ReconcileRequired {
                reconciliation: FinalizationReconciliation {
                    generation,
                    outcome,
                    durability: DurabilityKnowledge::VisibilityOrDurabilityUnknown,
                    mutation: StoreMutation::PublishImmutable,
                    world: authenticate_expected_world(
                        Some(partial),
                        published,
                        &prepared.observation,
                    ),
                    source,
                },
            }),
            ImmutablePublicationOutcome::DurableWithPartialResidual {
                last_linked_published,
                last_linked_partial,
                partial_absent_in_process,
                source,
            } => {
                let mut durable_observation = prepared.observation.clone();
                durable_observation.link_count = Some(1);
                let _record =
                    self.finalization_record(candidate, durable_observation, &record_path)?;
                Ok(FinalizationDisposition::DurableResidual {
                    reconciliation: FinalizationReconciliation {
                        generation,
                        outcome,
                        durability: DurabilityKnowledge::DurableRecord,
                        mutation: StoreMutation::CleanupPublishedPartial,
                        world: authenticate_expected_world(
                            (!partial_absent_in_process).then_some(last_linked_partial),
                            Some(last_linked_published),
                            &prepared.observation,
                        ),
                        source,
                    },
                })
            }
        }
    }

    /// Converts an authenticated hard-link overlap into one durable
    /// finalization record.  The publication parent is synced before the
    /// partial alias is removed, the cleanup is synced exactly, and the
    /// namespace is then reloaded from fresh authority.
    pub(super) fn adopt_finalization_publication(
        &self,
        loaded: &LoadedFinalization,
        outcome: TransactionOutcome,
    ) -> Result<FinalizationAdoptionDisposition, JournalStoreError> {
        let published = loaded.latest().ok_or_else(|| {
            JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "finalization adoption has no published immutable generation",
            )
        })?;
        let generation = published.lease.generation();
        let linked_partial = match loaded.reconciliation() {
            Some(FinalizationWorld::LinkedAliases {
                generation: linked_generation,
            }) if *linked_generation == generation => {
                let partial = loaded.partial().ok_or_else(|| {
                    JournalStoreError::invalid(
                        self.capabilities.workspace_parent.path,
                        "linked finalization residual has no exact partial binding",
                    )
                })?;
                if partial.lease != published.lease
                    || partial.name != partial.lease.partial_name()
                    || published.name != published.lease.record_name()
                    || partial.exact.identity() != published.exact.identity()
                    || partial.exact.link_count() != 2
                    || published.exact.link_count() != 2
                {
                    return Err(JournalStoreError::invalid(
                        self.capabilities.workspace_parent.path,
                        "finalization linked aliases lost their exact lease/name/identity binding",
                    ));
                }
                Some(partial)
            }
            None | Some(FinalizationWorld::CleanupProgress { .. })
                if loaded.partial().is_none()
                    && published.name == published.lease.record_name()
                    && published.exact.link_count() == 1 =>
            {
                // Publication was already durable before this adoption call,
                // or the partial unlink completed before an observation/sync
                // failure.  The parent durability barrier below is still
                // required before this single-name world is accepted.
                None
            }
            _ => {
                return Err(JournalStoreError::invalid(
                    self.capabilities.workspace_parent.path,
                    "finalization adoption requires exact linked aliases or one exact published-only generation",
                ));
            }
        };

        let capture = self.capture_authority(false)?;
        if self.load_finalization_from_capture(&capture)? != *loaded {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "finalization publication authority changed before adoption",
            ));
        }
        let reconcile = |mutation, source| FinalizationAdoptionDisposition::ReconcileRequired {
            reconciliation: FinalizationReconciliation {
                generation,
                outcome: published.lease.outcome(),
                durability: DurabilityKnowledge::DurableRecord,
                mutation,
                world: self.probe_finalization_world(&published.lease),
                source,
            },
        };
        let parent_observation = match self
            .runtime
            .fs()
            .observe_directory(self.capabilities.workspace_parent)
        {
            Ok(observation) => observation,
            Err(source) => {
                return Ok(reconcile(StoreMutation::ObservePartialParent, source));
            }
        };
        if let Err(source) = self.runtime.fs().sync_parent(
            self.capabilities.workspace_parent,
            &parent_observation,
            ParentSyncKind::Journal,
        ) {
            return Ok(reconcile(StoreMutation::SyncPartialParent, source));
        }

        if let Some(partial) = linked_partial {
            let removal = self.remove_finalization_partial(partial, outcome)?;
            if let ExactRemovalDisposition::ReconcileRequired(removal) = removal {
                return Ok(reconcile(
                    StoreMutation::CleanupPublishedPartial,
                    removal.source,
                ));
            }
        }

        let capture = match self.capture_authority(false) {
            Ok(capture) => capture,
            Err(error) => {
                return Ok(reconcile(
                    StoreMutation::CleanupPublishedPartial,
                    io::Error::other(error.to_string()),
                ));
            }
        };
        let current = match self.load_finalization_from_capture(&capture) {
            Ok(current) => current,
            Err(error) => {
                return Ok(reconcile(
                    StoreMutation::CleanupPublishedPartial,
                    io::Error::other(error.to_string()),
                ));
            }
        };
        let Some(record) = current
            .history()
            .iter()
            .find(|record| record.lease.generation() == generation)
            .cloned()
        else {
            return Ok(reconcile(
                StoreMutation::CleanupPublishedPartial,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "adopted finalization generation disappeared during reload",
                ),
            ));
        };
        if record.lease != published.lease
            || record.exact.identity() != published.exact.identity()
            || record.exact.link_count() != 1
            || current.partial().is_some()
        {
            return Ok(reconcile(
                StoreMutation::CleanupPublishedPartial,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "reloaded finalization publication is not the sole durable exact record",
                ),
            ));
        }
        self.runtime.observe(finalization_transition(
            &record.lease,
            TransitionWindow::After,
        ));
        Ok(FinalizationAdoptionDisposition::Durable {
            loaded: current,
            record,
        })
    }

    fn prepare_finalization_file(
        &self,
        candidate: &FinalizationLeaseV2,
        bytes: &[u8],
    ) -> Result<PrepareFinalizationDisposition, JournalStoreError> {
        let name = candidate.partial_name();
        let path = self.capabilities.workspace_parent.path.join(&name);
        let parent = self.capabilities.workspace_parent.directory;
        let mutation_error = |store: &Self,
                              mutation: StoreMutation,
                              source: io::Error|
         -> PrepareFinalizationDisposition {
            PrepareFinalizationDisposition::ReconcileRequired(FinalizationReconciliation {
                generation: candidate.generation(),
                outcome: candidate.outcome(),
                durability: DurabilityKnowledge::NotPublished,
                mutation,
                world: store.probe_finalization_world(candidate),
                source,
            })
        };
        let mut created = match self.runtime.fs().create_new_file(
            parent,
            Path::new(&name),
            &path,
            PRIVATE_FILE_MODE,
        ) {
            Ok(created) => created,
            Err(source) => {
                return Ok(mutation_error(self, StoreMutation::CreatePartial, source));
            }
        };
        if let Err(source) =
            self.runtime
                .fs()
                .set_file_mode(&created.file, &path, PRIVATE_FILE_MODE)
        {
            return Ok(mutation_error(self, StoreMutation::SetPartialMode, source));
        }
        if let Err(source) = self
            .runtime
            .fs()
            .write_handle(&mut created.file, &path, bytes)
        {
            return Ok(mutation_error(self, StoreMutation::WritePartial, source));
        }
        if let Err(source) = self.runtime.fs().flush_file(&created.file, &path) {
            return Ok(mutation_error(self, StoreMutation::FlushPartial, source));
        }
        if let Err(source) = self.runtime.fs().sync_handle(&created.file, &path) {
            return Ok(mutation_error(self, StoreMutation::SyncPartial, source));
        }
        let first = match self.runtime.fs().read_regular_file_exact(
            parent,
            Path::new(&name),
            &path,
            MAX_RECORD_ENVELOPE_BYTES,
        ) {
            Ok(read) => read,
            Err(source) => {
                return Ok(mutation_error(self, StoreMutation::VerifyPartial, source));
            }
        };
        if first.observation.identity != created.identity
            || first.bytes != bytes
            || first.observation.link_count != Some(1)
        {
            return Ok(mutation_error(
                self,
                StoreMutation::VerifyPartial,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "finalization partial changed before durability",
                ),
            ));
        }
        if let Err(error) =
            require_private_observation(&first.observation, "finalization partial", &path)
        {
            return Ok(mutation_error(
                self,
                StoreMutation::VerifyPartial,
                io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
            ));
        }
        let parent_observation = match self
            .runtime
            .fs()
            .observe_directory(self.capabilities.workspace_parent)
        {
            Ok(observation) => observation,
            Err(source) => {
                return Ok(mutation_error(
                    self,
                    StoreMutation::ObservePartialParent,
                    source,
                ));
            }
        };
        if let Err(source) = self.runtime.fs().sync_parent(
            self.capabilities.workspace_parent,
            &parent_observation,
            ParentSyncKind::Journal,
        ) {
            return Ok(mutation_error(
                self,
                StoreMutation::SyncPartialParent,
                source,
            ));
        }
        let durable = match self.runtime.fs().read_regular_file_exact(
            parent,
            Path::new(&name),
            &path,
            MAX_RECORD_ENVELOPE_BYTES,
        ) {
            Ok(read) => read,
            Err(source) => {
                return Ok(mutation_error(self, StoreMutation::VerifyPartial, source));
            }
        };
        if durable != first {
            return Ok(mutation_error(
                self,
                StoreMutation::VerifyPartial,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "finalization partial changed across its parent durability barrier",
                ),
            ));
        }
        if let Err(error) = exact_file(&durable.observation) {
            return Ok(mutation_error(
                self,
                StoreMutation::VerifyPartial,
                io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
            ));
        }
        Ok(PrepareFinalizationDisposition::Durable(
            PreparedFinalization {
                observation: durable.observation,
            },
        ))
    }

    fn probe_finalization_world(&self, candidate: &FinalizationLeaseV2) -> ObservedCandidateWorld {
        let result = (|| -> Result<ObservedCandidateWorld, JournalStoreError> {
            let observation = self
                .runtime
                .fs()
                .observe_directory(self.capabilities.workspace_parent)
                .map_err(|source| {
                    JournalStoreError::io(
                        self.capabilities.workspace_parent.path,
                        "observe finalization reconciliation parent",
                        source,
                    )
                })?;
            let inventory = self
                .runtime
                .fs()
                .inventory_directory_exact(self.capabilities.workspace_parent, &observation)
                .map_err(|source| {
                    JournalStoreError::io(
                        self.capabilities.workspace_parent.path,
                        "inventory finalization reconciliation parent",
                        source,
                    )
                })?;
            let namespace = self.validate_parent_namespace(&inventory, false)?;
            let partial = namespace
                .finalization_partial
                .as_ref()
                .filter(|(generation, _)| *generation == candidate.generation())
                .map(|(_, entry)| {
                    self.read_inventory_file(
                        self.capabilities.workspace_parent.directory,
                        entry,
                        MAX_RECORD_ENVELOPE_BYTES,
                    )
                })
                .transpose()?;
            let published = namespace
                .finalization
                .get(&candidate.generation())
                .map(|entry| {
                    self.read_inventory_file(
                        self.capabilities.workspace_parent.directory,
                        entry,
                        MAX_RECORD_ENVELOPE_BYTES,
                    )
                })
                .transpose()?;
            let expected = candidate.to_json_bytes().map_err(|error| {
                JournalStoreError::model(self.capabilities.workspace_parent.path, error)
            })?;
            if partial.as_ref().is_some_and(|read| read.bytes != expected)
                || published
                    .as_ref()
                    .is_some_and(|read| read.bytes != expected)
            {
                return Ok(ObservedCandidateWorld::Conflict {
                    reason:
                        "finalization reconciliation names do not contain the canonical candidate"
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

    fn finalization_record(
        &self,
        candidate: &FinalizationLeaseV2,
        observation: ExactFileObservation,
        path: &Path,
    ) -> Result<FinalizationRecord, JournalStoreError> {
        let exact =
            exact_file(&observation).map_err(|error| JournalStoreError::model(path, error))?;
        require_private_exact_file(&exact, "finalization record", path)?;
        if exact.link_count() != 1 {
            return Err(JournalStoreError::invalid(
                path,
                "durable finalization record must have exactly one hard link",
            ));
        }
        let expected_bytes = candidate
            .to_json_bytes()
            .map_err(|error| JournalStoreError::model(path, error))?;
        if observation.byte_len != expected_bytes.len() as u64
            || observation.content_hash != crate::hash_content_bytes(&expected_bytes)
        {
            return Err(JournalStoreError::invalid(
                path,
                "published finalization record does not match its canonical bytes",
            ));
        }
        Ok(FinalizationRecord {
            lease: candidate.clone(),
            exact,
            observation,
            name: candidate.record_name(),
        })
    }

    pub(super) fn remove_bootstrap_intent(
        &self,
        lease: &FinalizationLeaseV2,
        outcome: TransactionOutcome,
    ) -> Result<ExactRemovalDisposition, JournalStoreError> {
        if lease.transaction_id() != &self.transaction_id {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "bootstrap-intent removal lease belongs to another transaction",
            ));
        }
        let name = bootstrap_intent_name(&self.transaction_id);
        self.remove_exact_file(
            self.capabilities.workspace_parent,
            &name,
            lease.bootstrap().intent().exact(),
            MAX_CONTROL_ENVELOPE_BYTES,
            RemovalObject::BootstrapIntent,
            outcome,
        )
    }

    pub(super) fn remove_bootstrap_owner(
        &self,
        lease: &FinalizationLeaseV2,
        outcome: TransactionOutcome,
    ) -> Result<ExactRemovalDisposition, JournalStoreError> {
        if lease.transaction_id() != &self.transaction_id {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "bootstrap-owner removal lease belongs to another transaction",
            ));
        }
        let name = bootstrap_owner_name(&self.transaction_id);
        let workspace = self
            .capabilities
            .workspace
            .ok_or_else(|| self.missing_workspace_error())?;
        self.remove_exact_file(
            workspace,
            &name,
            lease.bootstrap().exact(),
            MAX_CONTROL_ENVELOPE_BYTES,
            RemovalObject::BootstrapOwner,
            outcome,
        )
    }

    pub(super) fn remove_journal_record(
        &self,
        record: &RecordBindingV2,
        outcome: TransactionOutcome,
    ) -> Result<ExactRemovalDisposition, JournalStoreError> {
        let parsed = parse_journal_file_name(record.name())
            .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?;
        if parsed.transaction_id() != &self.transaction_id
            || parsed.kind() != JournalFileKindV2::Published
            || parsed.sequence() != record.sequence()
        {
            return Err(JournalStoreError::invalid(
                self.workspace_path().join(record.name()),
                "journal-record removal binding has a noncanonical transaction/name/sequence",
            ));
        }
        let workspace = self
            .capabilities
            .workspace
            .ok_or_else(|| self.missing_workspace_error())?;
        self.remove_exact_file(
            workspace,
            record.name(),
            record.exact(),
            MAX_RECORD_ENVELOPE_BYTES,
            RemovalObject::JournalRecord {
                sequence: record.sequence(),
            },
            outcome,
        )
    }

    pub(super) fn remove_journal_partial(
        &self,
        partial: &PartialRecordBindingV2,
        outcome: TransactionOutcome,
    ) -> Result<ExactRemovalDisposition, JournalStoreError> {
        let parsed = parse_journal_file_name(partial.name())
            .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?;
        if parsed.transaction_id() != &self.transaction_id
            || parsed.kind() != JournalFileKindV2::Partial
            || parsed.sequence() != partial.sequence()
        {
            return Err(JournalStoreError::invalid(
                self.workspace_path().join(partial.name()),
                "journal-partial removal binding has a noncanonical transaction/name/sequence",
            ));
        }
        let workspace = self
            .capabilities
            .workspace
            .ok_or_else(|| self.missing_workspace_error())?;
        self.remove_exact_file(
            workspace,
            partial.name(),
            partial.exact(),
            MAX_RECORD_ENVELOPE_BYTES,
            RemovalObject::JournalPartial {
                sequence: partial.sequence(),
            },
            outcome,
        )
    }

    pub(super) fn remove_finalization_record(
        &self,
        record: &FinalizationRecord,
        outcome: TransactionOutcome,
    ) -> Result<ExactRemovalDisposition, JournalStoreError> {
        if record.lease.transaction_id() != &self.transaction_id
            || record.name != record.lease.record_name()
        {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "finalization removal record has a noncanonical transaction/name binding",
            ));
        }
        self.remove_exact_file(
            self.capabilities.workspace_parent,
            &record.name,
            &record.exact,
            MAX_RECORD_ENVELOPE_BYTES,
            RemovalObject::Finalization {
                generation: record.lease.generation(),
            },
            outcome,
        )
    }

    pub(super) fn remove_finalization_partial(
        &self,
        record: &FinalizationRecord,
        outcome: TransactionOutcome,
    ) -> Result<ExactRemovalDisposition, JournalStoreError> {
        if record.lease.transaction_id() != &self.transaction_id
            || record.name != record.lease.partial_name()
        {
            return Err(JournalStoreError::invalid(
                self.capabilities.workspace_parent.path,
                "finalization-partial removal record has a noncanonical transaction/name binding",
            ));
        }
        self.remove_exact_file(
            self.capabilities.workspace_parent,
            &record.name,
            &record.exact,
            MAX_RECORD_ENVELOPE_BYTES,
            RemovalObject::Finalization {
                generation: record.lease.generation(),
            },
            outcome,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "exact cleanup keeps its capability, bound name, state, read bound, object, and outcome explicit"
    )]
    fn remove_exact_file(
        &self,
        parent: DirectoryEndpoint<'_>,
        name: &str,
        expected: &ExactFileStateV2,
        max_bytes: u64,
        object: RemovalObject,
        outcome: TransactionOutcome,
    ) -> Result<ExactRemovalDisposition, JournalStoreError> {
        require_child_name(Path::new(name), &parent.path.join(name))?;
        // Revalidate the held lock and the exact parent/workspace namespace at
        // the mutation boundary.  The project-root observation itself remains
        // caller-pinned because this capability shape has no root directory
        // handle from which to recapture it.
        self.capture_authority(false)?;
        let path = parent.path.join(name);
        let before = match self.runtime.fs().read_regular_file_exact(
            parent.directory,
            Path::new(name),
            &path,
            max_bytes,
        ) {
            Ok(before) => before,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return self.confirm_durable_file_absence(
                    parent, name, expected, max_bytes, object, outcome,
                );
            }
            Err(source) => {
                return Err(JournalStoreError::io(
                    &path,
                    "read exact cleanup object",
                    source,
                ));
            }
        };
        let actual = exact_file(&before.observation)
            .map_err(|error| JournalStoreError::model(&path, error))?;
        if &actual != expected {
            return Err(JournalStoreError::invalid(
                &path,
                "cleanup object does not match its exact immutable manifest binding",
            ));
        }
        self.runtime.observe(removal_transition(
            object,
            outcome,
            TransitionWindow::Before,
        ));
        if let Err(source) = self.runtime.fs().remove_file_exact(
            parent.directory,
            Path::new(name),
            &path,
            &before.observation,
        ) {
            return Ok(ExactRemovalDisposition::ReconcileRequired(
                RemovalReconciliation {
                    object,
                    outcome,
                    mutation: RemovalMutation::RemoveExact,
                    world: self.probe_file_removal(parent, name, expected, max_bytes),
                    source,
                },
            ));
        }
        let parent_observation = match self.runtime.fs().observe_directory(parent) {
            Ok(observation) => observation,
            Err(source) => {
                return Ok(ExactRemovalDisposition::ReconcileRequired(
                    RemovalReconciliation {
                        object,
                        outcome,
                        mutation: RemovalMutation::ObserveParent,
                        world: self.probe_file_removal(parent, name, expected, max_bytes),
                        source,
                    },
                ));
            }
        };
        if let Err(source) =
            self.runtime
                .fs()
                .sync_parent(parent, &parent_observation, ParentSyncKind::Journal)
        {
            return Ok(ExactRemovalDisposition::ReconcileRequired(
                RemovalReconciliation {
                    object,
                    outcome,
                    mutation: RemovalMutation::SyncParent,
                    world: self.probe_file_removal(parent, name, expected, max_bytes),
                    source,
                },
            ));
        }
        let inventory = match self
            .runtime
            .fs()
            .inventory_directory_exact(parent, &parent_observation)
        {
            Ok(inventory) => inventory,
            Err(source) => {
                return Ok(ExactRemovalDisposition::ReconcileRequired(
                    RemovalReconciliation {
                        object,
                        outcome,
                        mutation: RemovalMutation::VerifyAbsent,
                        world: self.probe_file_removal(parent, name, expected, max_bytes),
                        source,
                    },
                ));
            }
        };
        if inventory.entries.iter().any(|entry| entry.name == name) {
            return Ok(ExactRemovalDisposition::ReconcileRequired(
                RemovalReconciliation {
                    object,
                    outcome,
                    mutation: RemovalMutation::VerifyAbsent,
                    world: self.probe_file_removal(parent, name, expected, max_bytes),
                    source: io::Error::new(
                        io::ErrorKind::InvalidData,
                        "cleanup name remains after exact removal and parent sync",
                    ),
                },
            ));
        }
        self.runtime
            .observe(removal_transition(object, outcome, TransitionWindow::After));
        Ok(ExactRemovalDisposition::DurableAbsent)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "absence confirmation binds the same complete cleanup tuple as the preceding mutation"
    )]
    fn confirm_durable_file_absence(
        &self,
        parent: DirectoryEndpoint<'_>,
        name: &str,
        expected: &ExactFileStateV2,
        max_bytes: u64,
        object: RemovalObject,
        outcome: TransactionOutcome,
    ) -> Result<ExactRemovalDisposition, JournalStoreError> {
        let reconciliation = |mutation, source| {
            ExactRemovalDisposition::ReconcileRequired(RemovalReconciliation {
                object,
                outcome,
                mutation,
                world: self.probe_file_removal(parent, name, expected, max_bytes),
                source,
            })
        };
        let before = match self.runtime.fs().observe_directory(parent) {
            Ok(observation) => observation,
            Err(source) => {
                return Ok(reconciliation(RemovalMutation::ObserveParent, source));
            }
        };
        let inventory = match self.runtime.fs().inventory_directory_exact(parent, &before) {
            Ok(inventory) => inventory,
            Err(source) => {
                return Ok(reconciliation(RemovalMutation::VerifyAbsent, source));
            }
        };
        if inventory.entries.iter().any(|entry| entry.name == name) {
            return Ok(reconciliation(
                RemovalMutation::VerifyAbsent,
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "cleanup name appeared after the initial missing observation",
                ),
            ));
        }
        if let Err(source) = self
            .runtime
            .fs()
            .sync_parent(parent, &before, ParentSyncKind::Journal)
        {
            return Ok(reconciliation(RemovalMutation::SyncParent, source));
        }
        let after = match self.runtime.fs().observe_directory(parent) {
            Ok(observation) => observation,
            Err(source) => {
                return Ok(reconciliation(RemovalMutation::ObserveParent, source));
            }
        };
        if after != before {
            return Ok(reconciliation(
                RemovalMutation::VerifyAbsent,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "cleanup parent changed while confirming an already-absent exact object",
                ),
            ));
        }
        let inventory = match self.runtime.fs().inventory_directory_exact(parent, &after) {
            Ok(inventory) => inventory,
            Err(source) => {
                return Ok(reconciliation(RemovalMutation::VerifyAbsent, source));
            }
        };
        if inventory.entries.iter().any(|entry| entry.name == name) {
            return Ok(reconciliation(
                RemovalMutation::VerifyAbsent,
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "cleanup name reappeared across the absence durability barrier",
                ),
            ));
        }
        self.runtime
            .observe(removal_transition(object, outcome, TransitionWindow::After));
        Ok(ExactRemovalDisposition::DurableAbsent)
    }

    fn probe_file_removal(
        &self,
        parent: DirectoryEndpoint<'_>,
        name: &str,
        expected: &ExactFileStateV2,
        max_bytes: u64,
    ) -> RemovalWorld {
        let result = (|| -> Result<RemovalWorld, JournalStoreError> {
            let observation = self
                .runtime
                .fs()
                .observe_directory(parent)
                .map_err(|source| {
                    JournalStoreError::io(
                        parent.path,
                        "observe cleanup reconciliation parent",
                        source,
                    )
                })?;
            let inventory = self
                .runtime
                .fs()
                .inventory_directory_exact(parent, &observation)
                .map_err(|source| {
                    JournalStoreError::io(
                        parent.path,
                        "inventory cleanup reconciliation parent",
                        source,
                    )
                })?;
            let Some(entry) = inventory.entries.iter().find(|entry| entry.name == name) else {
                return Ok(RemovalWorld::Missing);
            };
            if entry.kind != ExactDirectoryEntryKind::RegularFile {
                return Ok(RemovalWorld::Conflict {
                    reason: "cleanup name is occupied by an unsafe filesystem type".to_owned(),
                });
            }
            let path = parent.path.join(name);
            let read = self
                .runtime
                .fs()
                .read_regular_file_exact(parent.directory, Path::new(name), &path, max_bytes)
                .map_err(|source| {
                    JournalStoreError::io(&path, "read cleanup reconciliation object", source)
                })?;
            let actual = exact_file(&read.observation)
                .map_err(|error| JournalStoreError::model(&path, error))?;
            if &actual == expected {
                Ok(RemovalWorld::PresentExact)
            } else {
                Ok(RemovalWorld::Conflict {
                    reason: "cleanup name contains a substituted or modified object".to_owned(),
                })
            }
        })();
        result.unwrap_or_else(|error| RemovalWorld::ObservationUnavailable {
            reason: error.to_string(),
        })
    }

    pub(super) fn remove_workspace(
        &self,
        expected: &ExactDirectoryStateV2,
        outcome: TransactionOutcome,
    ) -> Result<WorkspaceRemovalDisposition, JournalStoreError> {
        let workspace = self
            .capabilities
            .workspace
            .ok_or_else(|| self.missing_workspace_error())?;
        let observed = self
            .runtime
            .fs()
            .observe_directory(workspace)
            .map_err(|source| {
                JournalStoreError::io(workspace.path, "observe exact cleanup workspace", source)
            })?;
        let actual = exact_directory(&observed)
            .map_err(|error| JournalStoreError::model(workspace.path, error))?;
        if &actual != expected {
            return Err(JournalStoreError::invalid(
                workspace.path,
                "cleanup workspace does not match its exact finalization authority",
            ));
        }
        let inventory = self
            .runtime
            .fs()
            .inventory_directory_exact(workspace, &observed)
            .map_err(|source| {
                JournalStoreError::io(workspace.path, "inventory cleanup workspace", source)
            })?;
        if !inventory.entries.is_empty() {
            return Err(JournalStoreError::invalid(
                workspace.path,
                "exact transaction workspace is not empty at its removal boundary",
            ));
        }
        let object = RemovalObject::Workspace;
        self.runtime.observe(removal_transition(
            object,
            outcome,
            TransitionWindow::Before,
        ));
        if let Err(source) = self
            .runtime
            .fs()
            .remove_empty_directory_exact(workspace, &observed)
        {
            return Ok(WorkspaceRemovalDisposition::ReconcileRequired(
                RemovalReconciliation {
                    object,
                    outcome,
                    mutation: RemovalMutation::RemoveExact,
                    world: self.probe_workspace_removal(expected),
                    source,
                },
            ));
        }
        let parent = self.capabilities.workspace_parent;
        let parent_after = match self.runtime.fs().observe_directory(parent) {
            Ok(observation) => observation,
            Err(source) => {
                return Ok(WorkspaceRemovalDisposition::ReconcileRequired(
                    RemovalReconciliation {
                        object,
                        outcome,
                        mutation: RemovalMutation::ObserveParent,
                        world: self.probe_workspace_removal(expected),
                        source,
                    },
                ));
            }
        };
        if let Err(source) =
            self.runtime
                .fs()
                .sync_parent(parent, &parent_after, ParentSyncKind::Journal)
        {
            return Ok(WorkspaceRemovalDisposition::ReconcileRequired(
                RemovalReconciliation {
                    object,
                    outcome,
                    mutation: RemovalMutation::SyncParent,
                    world: self.probe_workspace_removal(expected),
                    source,
                },
            ));
        }
        let parent_inventory = match self
            .runtime
            .fs()
            .inventory_directory_exact(parent, &parent_after)
        {
            Ok(inventory) => inventory,
            Err(source) => {
                return Ok(WorkspaceRemovalDisposition::ReconcileRequired(
                    RemovalReconciliation {
                        object,
                        outcome,
                        mutation: RemovalMutation::VerifyAbsent,
                        world: self.probe_workspace_removal(expected),
                        source,
                    },
                ));
            }
        };
        if parent_inventory
            .entries
            .iter()
            .any(|entry| entry.name == workspace.name)
        {
            return Ok(WorkspaceRemovalDisposition::ReconcileRequired(
                RemovalReconciliation {
                    object,
                    outcome,
                    mutation: RemovalMutation::VerifyAbsent,
                    world: self.probe_workspace_removal(expected),
                    source: io::Error::new(
                        io::ErrorKind::InvalidData,
                        "workspace remains after exact removal and parent sync",
                    ),
                },
            ));
        }
        self.runtime
            .observe(removal_transition(object, outcome, TransitionWindow::After));
        Ok(WorkspaceRemovalDisposition::Durable {
            workspace_parent_after: exact_directory(&parent_after)
                .map_err(|error| JournalStoreError::model(parent.path, error))?,
        })
    }

    fn probe_workspace_removal(&self, expected: &ExactDirectoryStateV2) -> RemovalWorld {
        let parent = self.capabilities.workspace_parent;
        let Some(workspace) = self.capabilities.workspace else {
            return RemovalWorld::ObservationUnavailable {
                reason: "no workspace capability is available".to_owned(),
            };
        };
        let result = (|| -> Result<RemovalWorld, JournalStoreError> {
            let parent_observation =
                self.runtime
                    .fs()
                    .observe_directory(parent)
                    .map_err(|source| {
                        JournalStoreError::io(
                            parent.path,
                            "observe workspace cleanup parent",
                            source,
                        )
                    })?;
            let inventory = self
                .runtime
                .fs()
                .inventory_directory_exact(parent, &parent_observation)
                .map_err(|source| {
                    JournalStoreError::io(parent.path, "inventory workspace cleanup parent", source)
                })?;
            let Some(entry) = inventory
                .entries
                .iter()
                .find(|entry| entry.name == workspace.name)
            else {
                return Ok(RemovalWorld::Missing);
            };
            if entry.kind != ExactDirectoryEntryKind::Directory {
                return Ok(RemovalWorld::Conflict {
                    reason: "workspace cleanup name is occupied by a non-directory".to_owned(),
                });
            }
            let observed = self
                .runtime
                .fs()
                .observe_directory(workspace)
                .map_err(|source| {
                    JournalStoreError::io(workspace.path, "observe cleanup workspace", source)
                })?;
            let actual = exact_directory(&observed)
                .map_err(|error| JournalStoreError::model(workspace.path, error))?;
            if &actual == expected {
                Ok(RemovalWorld::PresentExact)
            } else {
                Ok(RemovalWorld::Conflict {
                    reason: "workspace cleanup name contains a substituted directory".to_owned(),
                })
            }
        })();
        result.unwrap_or_else(|error| RemovalWorld::ObservationUnavailable {
            reason: error.to_string(),
        })
    }

    /// Publishes exactly one canonical successor through the immutable
    /// partial -> hard-link -> parent-sync -> exact-partial-cleanup protocol.
    ///
    /// Once mutation begins, every failure is returned as a typed disposition
    /// carrying the strongest durability knowledge and exact world available.
    pub(super) fn publish_snapshot(
        &self,
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
        self.authenticate_workspace_owners(workspace_namespace, Some(candidate))?;
        self.recapture_matches(&authority, true)?;

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
        let workspace_observation = match self.runtime.fs().observe_directory(workspace) {
            Ok(observation) => observation,
            Err(source) => {
                return Ok(PublicationDisposition::ReconcileRequired {
                    reconciliation: PublicationReconciliation {
                        boundary,
                        durability: DurabilityKnowledge::NotPublished,
                        mutation: StoreMutation::ObservePartialParent,
                        world: self.probe_snapshot_world(candidate),
                        source,
                    },
                });
            }
        };
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
                partial_absent_in_process,
                source,
            } => {
                let _record = candidate
                    .expected_record_binding(ObjectIdentityV2::from_parts(
                        prepared.observation.identity.namespace(),
                        prepared.observation.identity.object(),
                    ))
                    .map_err(|error| JournalStoreError::model(&record_path, error))?;
                let reconciliation = PublicationReconciliation {
                    boundary,
                    durability: DurabilityKnowledge::DurableRecord,
                    mutation: StoreMutation::CleanupPublishedPartial,
                    world: self.authenticate_publication_world(
                        candidate,
                        (!partial_absent_in_process).then_some(last_linked_partial),
                        Some(last_linked_published),
                        &prepared.observation,
                    ),
                    source,
                };
                if candidate.phase().desired_state_is_irreversible() {
                    Ok(PublicationDisposition::DurableFinishOnlyResidual { reconciliation })
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
        if let Err(error) =
            require_private_observation(&first_read.observation, "journal partial", &partial_path)
        {
            return Ok(PrepareDisposition::ReconcileRequired(
                self.prepare_reconciliation(
                    candidate,
                    boundary,
                    StoreMutation::VerifyPartial,
                    io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
                ),
            ));
        }
        if first_read.observation.link_count != Some(1) {
            return Ok(PrepareDisposition::ReconcileRequired(
                self.prepare_reconciliation(
                    candidate,
                    boundary,
                    StoreMutation::VerifyPartial,
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "prepared journal partial must have exactly one hard link",
                    ),
                ),
            ));
        }
        let workspace_observation = match self.runtime.fs().observe_directory(workspace) {
            Ok(observation) => observation,
            Err(source) => {
                return Ok(PrepareDisposition::ReconcileRequired(
                    self.prepare_reconciliation(
                        candidate,
                        boundary,
                        StoreMutation::ObservePartialParent,
                        source,
                    ),
                ));
            }
        };
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
            if is_reserved_journal_family(name) {
                return Err(JournalStoreError::invalid(
                    path,
                    "unknown, noncanonical, or legacy journal child occupies a reserved top-level namespace",
                ));
            }
            return Err(JournalStoreError::invalid(
                path,
                "unknown child occupies the private transaction authority namespace",
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
            if let Ok(owner) = parse_owner_artifact_name(name) {
                self.require_transaction(owner.transaction_id(), &path)?;
                let expected_kind = match owner.kind() {
                    OwnerArtifactKindV2::Directory => ExactDirectoryEntryKind::Directory,
                    OwnerArtifactKindV2::Stage | OwnerArtifactKindV2::Backup => {
                        ExactDirectoryEntryKind::RegularFile
                    }
                };
                require_entry_kind(entry, expected_kind, &path, "transaction artifact owner")?;
                if namespace
                    .owners
                    .insert(
                        name.to_owned(),
                        WorkspaceOwner {
                            ordinal: owner.ordinal(),
                            artifact: owner.kind(),
                            entry: inventory_entry(entry, path),
                        },
                    )
                    .is_some()
                {
                    return Err(JournalStoreError::invalid(
                        self.workspace_path(),
                        "transaction workspace contains duplicate canonical owner names",
                    ));
                }
                continue;
            }
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

    fn authenticate_workspace_owners(
        &self,
        namespace: &WorkspaceNamespace,
        snapshot: Option<&JournalSnapshotV2>,
    ) -> Result<(), JournalStoreError> {
        let Some(snapshot) = snapshot else {
            if namespace.owners.is_empty() {
                return Ok(());
            }
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "bootstrap-only transaction workspace contains an owner artifact without immutable journal authority",
            ));
        };

        let mut expected = BTreeMap::new();
        for directory in snapshot.directories() {
            if let (Some(name), Some(exact)) = (
                directory.candidate_name(),
                directory.candidate_current().as_present(),
            ) {
                insert_expected_workspace_owner(
                    &mut expected,
                    name,
                    ExpectedWorkspaceOwner {
                        ordinal: directory.ordinal(),
                        artifact: OwnerArtifactKindV2::Directory,
                        state: ExpectedWorkspaceOwnerState::Directory(exact),
                    },
                    self.workspace_path(),
                )?;
            }
        }
        for entry in snapshot.entries() {
            if let Some(exact) = entry.stage().owner_current().as_present() {
                insert_expected_workspace_owner(
                    &mut expected,
                    entry.stage().owner_name(),
                    ExpectedWorkspaceOwner {
                        ordinal: entry.ordinal(),
                        artifact: OwnerArtifactKindV2::Stage,
                        state: ExpectedWorkspaceOwnerState::File(exact),
                    },
                    self.workspace_path(),
                )?;
            }
            if let Some(backup) = entry.backup()
                && let Some(exact) = backup.owner_current().as_present()
            {
                insert_expected_workspace_owner(
                    &mut expected,
                    backup.owner_name(),
                    ExpectedWorkspaceOwner {
                        ordinal: entry.ordinal(),
                        artifact: OwnerArtifactKindV2::Backup,
                        state: ExpectedWorkspaceOwnerState::File(exact),
                    },
                    self.workspace_path(),
                )?;
            }
        }

        let pending = match snapshot.phase() {
            JournalPhaseV2::Preparing {
                completed,
                pending: Some(pending),
            } => Some((*completed as usize, pending)),
            _ => None,
        };
        let optional_placed_owner = pending.and_then(|(_, pending)| match pending {
            PreparationPendingIntentV2::Place(intent) => Some(match intent {
                PreparationPlacementIntentV2::Directory(intent) => intent.owner_name(),
                PreparationPlacementIntentV2::File(intent) => intent.owner_name(),
            }),
            PreparationPendingIntentV2::Create(_) | PreparationPendingIntentV2::Discard(_) => None,
        });

        for (name, expected_owner) in &expected {
            match namespace.owners.get(name) {
                Some(observed) => self.require_exact_workspace_owner(observed, expected_owner)?,
                None if optional_placed_owner == Some(name.as_str()) => {}
                None => {
                    return Err(JournalStoreError::invalid(
                        self.workspace_path().join(name),
                        "durable journal owner manifest requires an exact live owner artifact",
                    ));
                }
            }
        }

        for (name, observed) in &namespace.owners {
            if expected.contains_key(name) {
                continue;
            }
            let allowed = match pending {
                Some((completed, PreparationPendingIntentV2::Create(intent)))
                    if name == intent.owner_name() =>
                {
                    let object = self.workspace_owner_residual(observed, intent)?;
                    let binding = OwnedResidualDeleteBindingV2::new(intent.clone(), object);
                    snapshot
                        .validate_owner_residual(completed, &binding)
                        .map_err(|error| JournalStoreError::model(&observed.entry.path, error))?;
                    true
                }
                Some((_, PreparationPendingIntentV2::Discard(binding)))
                    if name == binding.owner().owner_name() =>
                {
                    let object = self.workspace_owner_residual(observed, binding.owner())?;
                    if &object != binding.object() {
                        return Err(JournalStoreError::invalid(
                            &observed.entry.path,
                            "live owner residual does not match its durable Discard binding",
                        ));
                    }
                    true
                }
                _ => false,
            };
            if !allowed {
                return Err(JournalStoreError::invalid(
                    &observed.entry.path,
                    "workspace owner is absent from the latest immutable owner manifest or occupies an out-of-order owner slot",
                ));
            }
        }
        Ok(())
    }

    fn require_exact_workspace_owner(
        &self,
        observed: &WorkspaceOwner,
        expected: &ExpectedWorkspaceOwner<'_>,
    ) -> Result<(), JournalStoreError> {
        if observed.ordinal != expected.ordinal || observed.artifact != expected.artifact {
            return Err(JournalStoreError::invalid(
                &observed.entry.path,
                "workspace owner name kind or ordinal does not match the immutable owner manifest",
            ));
        }
        let entry = &observed.entry;
        let metadata_matches = match expected.state {
            ExpectedWorkspaceOwnerState::File(exact) => {
                observed.artifact != OwnerArtifactKindV2::Directory
                    && entry.identity
                        == ExactObjectIdentity::from_parts(
                            exact.identity().namespace_bytes(),
                            exact.identity().object_bytes(),
                        )
                    && entry.byte_len == exact.state().byte_len()
                    && entry.mode.readonly == exact.state().readonly()
                    && entry.mode.posix_mode == exact.state().posix_mode()
                    && entry.link_count == Some(exact.link_count())
            }
            ExpectedWorkspaceOwnerState::Directory(exact) => {
                observed.artifact == OwnerArtifactKindV2::Directory
                    && entry.identity
                        == ExactObjectIdentity::from_parts(
                            exact.identity().namespace_bytes(),
                            exact.identity().object_bytes(),
                        )
                    && entry.mode.readonly == exact.mode().readonly()
                    && entry.mode.posix_mode == exact.mode().posix_mode()
            }
        };
        if !metadata_matches {
            return Err(JournalStoreError::invalid(
                &entry.path,
                "workspace owner metadata does not match its exact immutable owner manifest",
            ));
        }

        match expected.state {
            ExpectedWorkspaceOwnerState::File(exact) => {
                let observed_exact = self
                    .runtime
                    .fs()
                    .observe_regular_file_bounded(
                        self.workspace_directory()?,
                        Path::new(&entry.name),
                        &entry.path,
                        exact.state().byte_len(),
                    )
                    .map_err(|source| {
                        JournalStoreError::io(
                            &entry.path,
                            "bounded-hash the manifested file owner",
                            source,
                        )
                    })?;
                let actual = exact_file(&observed_exact)
                    .map_err(|error| JournalStoreError::model(&entry.path, error))?;
                if &actual != exact {
                    return Err(JournalStoreError::invalid(
                        &entry.path,
                        "manifested file owner bytes do not match the immutable content authority",
                    ));
                }
            }
            ExpectedWorkspaceOwnerState::Directory(exact) => {
                let opened = self
                    .runtime
                    .fs()
                    .open_directory_exact(
                        self.workspace_directory()?,
                        Path::new(&entry.name),
                        &entry.path,
                        exact.mode().posix_mode().unwrap_or(0o755),
                    )
                    .map_err(|source| {
                        JournalStoreError::io(
                            &entry.path,
                            "rebind the manifested directory owner",
                            source,
                        )
                    })?;
                let actual = exact_directory(&opened.observation)
                    .map_err(|error| JournalStoreError::model(&entry.path, error))?;
                if &actual != exact {
                    return Err(JournalStoreError::invalid(
                        &entry.path,
                        "manifested directory owner does not match its exact immutable authority",
                    ));
                }
                self.runtime
                    .fs()
                    .inventory_directory_exact_bounded(
                        DirectoryEndpoint::new(
                            self.workspace_directory()?,
                            Path::new(&entry.name),
                            &opened.directory,
                            &entry.path,
                        ),
                        &opened.observation,
                        0,
                    )
                    .map_err(|source| {
                        JournalStoreError::io(
                            &entry.path,
                            "prove the manifested directory owner is exactly empty",
                            source,
                        )
                    })?;
            }
        }
        Ok(())
    }

    fn workspace_owner_residual(
        &self,
        observed: &WorkspaceOwner,
        intent: &super::journal::OwnerCreationIntentV2,
    ) -> Result<OwnedResidualObjectV2, JournalStoreError> {
        if observed.ordinal != intent.ordinal() || observed.artifact != intent.artifact() {
            return Err(JournalStoreError::invalid(
                &observed.entry.path,
                "workspace residual name kind or ordinal does not match its durable owner intent",
            ));
        }
        let entry = &observed.entry;
        let identity =
            ObjectIdentityV2::from_parts(entry.identity.namespace(), entry.identity.object());
        let link_count = entry.link_count.ok_or_else(|| {
            JournalStoreError::invalid(
                &entry.path,
                "workspace owner residual has no exact hard-link count",
            )
        })?;
        match intent.artifact() {
            OwnerArtifactKindV2::Directory => {
                let mode = DirectoryModeV2::new(entry.mode.readonly, entry.mode.posix_mode)
                    .map_err(|error| JournalStoreError::model(&entry.path, error))?;
                let exact = ExactDirectoryStateV2::new(identity, mode, link_count)
                    .map_err(|error| JournalStoreError::model(&entry.path, error))?;
                ExactDirectoryMetadataV2::new(exact, link_count)
                    .map(OwnedResidualObjectV2::Directory)
                    .map_err(|error| JournalStoreError::model(&entry.path, error))
            }
            OwnerArtifactKindV2::Stage | OwnerArtifactKindV2::Backup => ExactFileMetadataV2::new(
                identity,
                entry.byte_len,
                entry.mode.readonly,
                entry.mode.posix_mode,
                link_count,
            )
            .map(OwnedResidualObjectV2::File)
            .map_err(|error| JournalStoreError::model(&entry.path, error)),
        }
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
        let intent = WorkspaceBootstrapIntentBindingV2::new(
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
        let bootstrap =
            WorkspaceBootstrapBindingV2::from_exact_envelopes(intent, owner, owner_exact)
                .map_err(|error| JournalStoreError::model(&owner_entry.path, error))?;
        let live_workspace = capture.workspace_inventory.as_ref().ok_or_else(|| {
            JournalStoreError::invalid(
                self.workspace_path(),
                "bootstrap-only namespace has no exact workspace inventory",
            )
        })?;
        let live_workspace = exact_directory(&live_workspace.directory)
            .map_err(|error| JournalStoreError::model(self.workspace_path(), error))?;
        let live_parent =
            exact_directory(&capture.parent_inventory.directory).map_err(|error| {
                JournalStoreError::model(self.capabilities.workspace_parent.path, error)
            })?;
        let live_root = exact_directory(&capture.root).map_err(|error| {
            JournalStoreError::model(self.capabilities.project_root_path, error)
        })?;
        let live_lock = exact_file(&capture.write_lock.observation)
            .map_err(|error| JournalStoreError::model(self.capabilities.write_lock.path, error))?;
        if bootstrap.envelope().workspace_exact() != &live_workspace
            || bootstrap.envelope().workspace_parent_after_workspace() != &live_parent
            || bootstrap.envelope().root() != &live_root
            || bootstrap.envelope().write_lock() != &live_lock
            || bootstrap.envelope().canonical_root_hash() != &self.canonical_root_hash
            || bootstrap.envelope().transaction_id() != &self.transaction_id
            || model_identity(bootstrap.envelope().coordination_parent().identity())
                != self.capabilities.held_coordination_parent_identity
        {
            return Err(JournalStoreError::invalid(
                self.workspace_path(),
                "bootstrap-only authority is not bound to the live project parent and workspace",
            ));
        }
        Ok(bootstrap)
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
            || model_identity(snapshot.project().coordination_parent().identity())
                != self.capabilities.held_coordination_parent_identity
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
        bootstrap: &WorkspaceBootstrapBindingV2,
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
                return match PartialEnvelopeHeaderV2::validate_incomplete_ownership_prefix(
                    &read.bytes,
                    &self.transaction_id,
                    bootstrap,
                    entry.sequence,
                ) {
                    Ok(()) => Ok(PartialLoad::Incomplete(
                        ObservedCandidateWorld::OwnedIncomplete {
                            partial: read.observation,
                            bytes_present: read.bytes.len() as u64,
                        },
                    )),
                    Err(prefix_error) => {
                        Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                            reason: format!(
                                "partial has neither a canonical ownership header nor an authenticated crash-truncated prefix: {error}; {prefix_error}"
                            ),
                            partial: Some(read.observation),
                            published: None,
                        }))
                    }
                };
            }
        };
        if let Err(error) =
            header.validate_bootstrap_binding(&self.transaction_id, bootstrap, entry.sequence)
        {
            return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                reason: format!(
                    "partial ownership header does not match the exact bootstrap authority: {error}"
                ),
                partial: Some(read.observation),
                published: None,
            }));
        }
        if (payload.len() as u64) < header.payload_len() {
            return Ok(PartialLoad::Incomplete(
                ObservedCandidateWorld::OwnedIncomplete {
                    partial: read.observation,
                    bytes_present: read.bytes.len() as u64,
                },
            ));
        }
        if payload.len() as u64 > header.payload_len() {
            return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                reason: "partial payload exceeds the length bound in its ownership header"
                    .to_owned(),
                partial: Some(read.observation),
                published: None,
            }));
        }
        let candidate = match JournalSnapshotV2::from_record_envelope_slice(&read.bytes) {
            Ok(candidate) => candidate,
            Err(error) => {
                return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                    reason: format!(
                        "complete partial bytes are corrupt or noncanonical and remain evidence: {error}"
                    ),
                    partial: Some(read.observation),
                    published: None,
                }));
            }
        };
        if let Err(error) =
            header.validate_binding(&self.transaction_id, candidate.project(), entry.sequence)
        {
            return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                reason: format!(
                    "complete partial project binding disagrees with its bootstrap-owned header: {error}"
                ),
                partial: Some(read.observation),
                published: None,
            }));
        }
        if candidate.transaction_id() != &self.transaction_id
            || candidate.sequence() != entry.sequence
            || candidate.partial_name() != entry.name
        {
            return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                reason: "complete journal partial does not match its canonical transaction, sequence, and filename"
                    .to_owned(),
                partial: Some(read.observation),
                published: None,
            }));
        }
        let lineage_result = match predecessor {
            Some(previous) => previous.validate_successor(&candidate),
            None if candidate.sequence() == 0 => candidate.validate(),
            None => {
                return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                    reason: "nonzero complete partial has no contiguous predecessor".to_owned(),
                    partial: Some(read.observation),
                    published: None,
                }));
            }
        };
        if let Err(error) = lineage_result {
            return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                reason: format!(
                    "complete partial is not the canonical next lineage record: {error}"
                ),
                partial: Some(read.observation),
                published: None,
            }));
        }
        let binding = match PartialRecordBindingV2::new(&candidate, exact, header, &read.bytes) {
            Ok(binding) => binding,
            Err(error) => {
                return Ok(PartialLoad::Incomplete(ObservedCandidateWorld::Conflict {
                    reason: format!(
                        "complete partial exact state is not its canonical binding: {error}"
                    ),
                    partial: Some(read.observation),
                    published: None,
                }));
            }
        };
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
        let canonical_bytes = snapshot
            .record_envelope_bytes()
            .map_err(|error| JournalStoreError::model(&published.path, error))?;
        if published_read.bytes != canonical_bytes {
            return Ok(ObservedCandidateWorld::Conflict {
                reason: "linked publication bytes are not the canonical immutable envelope"
                    .to_owned(),
                partial: Some(partial_read.observation),
                published: Some(published_read.observation),
            });
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "the exact loaded lineage remains an owned atomic classification result"
)]
pub(super) enum ActiveJournalLoad {
    Stable(LoadedJournal),
    ReconciliationRequired(ActiveReconciliation),
}

#[derive(Debug)]
pub(super) enum JournalNamespace {
    Empty,
    Bootstrap(LoadedBootstrap),
    Active(LoadedJournal),
    ActiveReconciliation(ActiveReconciliation),
    Finalizing(LoadedFinalization),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LoadedBootstrap {
    bootstrap: WorkspaceBootstrapBindingV2,
}

impl LoadedBootstrap {
    pub(super) fn bootstrap(&self) -> &WorkspaceBootstrapBindingV2 {
        &self.bootstrap
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
    CleanupProgress { stage: FinalizationCleanupStage },
    Conflict { generation: u64, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FinalizationCleanupStage {
    CompleteManifest,
    IntentRemoved,
    OwnershipRemoved,
    PartialRemoved,
    HistoryRemoving { remaining_records: usize },
    WorkspaceEmpty,
    WorkspaceRemoved,
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
        reconciliation: PublicationReconciliation,
    },
    ReconcileRequired {
        reconciliation: PublicationReconciliation,
    },
}

#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "the reconciliation evidence remains an owned typed outcome across the mutation boundary"
)]
pub(super) enum ActiveReconciliationDisposition {
    Durable,
    ReconcileRequired {
        reconciliation: ActiveMutationReconciliation,
    },
}

#[derive(Debug)]
pub(super) struct ActiveMutationReconciliation {
    action: ActiveReconciliationAction,
    sequence: u64,
    durability: DurabilityKnowledge,
    mutation: ActiveReconciliationMutation,
    world: ObservedCandidateWorld,
    source: io::Error,
}

impl ActiveMutationReconciliation {
    pub(super) const fn action(&self) -> ActiveReconciliationAction {
        self.action
    }

    pub(super) const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) const fn durability(&self) -> DurabilityKnowledge {
        self.durability
    }

    pub(super) const fn mutation(&self) -> ActiveReconciliationMutation {
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
pub(super) enum ActiveReconciliationAction {
    AdoptPublished,
    DiscardPartial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActiveReconciliationMutation {
    ObservePublishedParent,
    SyncPublishedParent,
    RemovePartial,
    ObserveCleanupParent,
    SyncCleanupParent,
    VerifyCleanup,
    ReloadLineage,
}

#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "finalization returns complete authenticated records or reconciliation evidence as one owned result"
)]
pub(super) enum FinalizationDisposition {
    Durable {
        record: FinalizationRecord,
    },
    DurableResidual {
        reconciliation: FinalizationReconciliation,
    },
    ReconcileRequired {
        reconciliation: FinalizationReconciliation,
    },
}

#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "adoption returns the mutually consistent loaded world and record without splitting ownership"
)]
pub(super) enum FinalizationAdoptionDisposition {
    Durable {
        loaded: LoadedFinalization,
        record: FinalizationRecord,
    },
    ReconcileRequired {
        reconciliation: FinalizationReconciliation,
    },
}

#[derive(Debug)]
pub(super) struct FinalizationReconciliation {
    generation: u64,
    outcome: FinalizationOutcomeV2,
    durability: DurabilityKnowledge,
    mutation: StoreMutation,
    world: ObservedCandidateWorld,
    source: io::Error,
}

#[derive(Debug)]
pub(super) enum ExactRemovalDisposition {
    DurableAbsent,
    ReconcileRequired(RemovalReconciliation),
}

#[derive(Debug)]
pub(super) enum WorkspaceRemovalDisposition {
    Durable {
        workspace_parent_after: ExactDirectoryStateV2,
    },
    ReconcileRequired(RemovalReconciliation),
}

#[derive(Debug)]
pub(super) struct RemovalReconciliation {
    object: RemovalObject,
    outcome: TransactionOutcome,
    mutation: RemovalMutation,
    world: RemovalWorld,
    source: io::Error,
}

impl RemovalReconciliation {
    pub(super) const fn object(&self) -> RemovalObject {
        self.object
    }

    pub(super) const fn mutation(&self) -> RemovalMutation {
        self.mutation
    }

    pub(super) const fn outcome(&self) -> TransactionOutcome {
        self.outcome
    }

    pub(super) fn world(&self) -> &RemovalWorld {
        &self.world
    }

    pub(super) fn source(&self) -> &io::Error {
        &self.source
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemovalObject {
    BootstrapIntent,
    BootstrapOwner,
    JournalRecord { sequence: u64 },
    JournalPartial { sequence: u64 },
    Workspace,
    Finalization { generation: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemovalMutation {
    RemoveExact,
    ObserveParent,
    SyncParent,
    VerifyAbsent,
}

#[derive(Debug)]
pub(super) enum RemovalWorld {
    PresentExact,
    Missing,
    Conflict { reason: String },
    ObservationUnavailable { reason: String },
}

impl RemovalWorld {
    pub(super) fn description(&self) -> &str {
        match self {
            Self::PresentExact => "the exact object is still present",
            Self::Missing => "the object is absent",
            Self::Conflict { reason } | Self::ObservationUnavailable { reason } => reason,
        }
    }
}

impl FinalizationReconciliation {
    pub(super) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) const fn outcome(&self) -> FinalizationOutcomeV2 {
        self.outcome
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
    ObservePartialParent,
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "partial loading returns either the complete canonical envelope or its exact observed world"
)]
enum PartialLoad {
    Complete(CompletedPartial),
    Incomplete(ObservedCandidateWorld),
}

struct PreparedRecord {
    binding: PartialRecordBindingV2,
    observation: ExactFileObservation,
}

struct PreparedFinalization {
    observation: ExactFileObservation,
}

enum PrepareFinalizationDisposition {
    Durable(PreparedFinalization),
    ReconcileRequired(FinalizationReconciliation),
}

enum PrepareDisposition {
    Durable(PreparedRecord),
    ReconcileRequired(PublicationReconciliation),
}

#[derive(Debug)]
struct TopLevelCapture {
    inventory: ExactDirectoryInventory,
    namespace: JournalTopLevelNamespace,
}

#[derive(Debug)]
struct BoundTopLevelCapture {
    top_level: TopLevelCapture,
    write_lock: ExactFileRead,
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
    owners: BTreeMap<String, WorkspaceOwner>,
}

#[derive(Debug, Clone)]
struct WorkspaceOwner {
    ordinal: ArtifactOrdinal,
    artifact: OwnerArtifactKindV2,
    entry: InventoryFile,
}

#[derive(Debug, Clone, Copy)]
struct ExpectedWorkspaceOwner<'a> {
    ordinal: ArtifactOrdinal,
    artifact: OwnerArtifactKindV2,
    state: ExpectedWorkspaceOwnerState<'a>,
}

#[derive(Debug, Clone, Copy)]
enum ExpectedWorkspaceOwnerState<'a> {
    File(&'a ExactFileStateV2),
    Directory(&'a ExactDirectoryStateV2),
}

fn insert_expected_workspace_owner<'a>(
    expected: &mut BTreeMap<String, ExpectedWorkspaceOwner<'a>>,
    name: &str,
    owner: ExpectedWorkspaceOwner<'a>,
    workspace_path: &Path,
) -> Result<(), JournalStoreError> {
    if expected.insert(name.to_owned(), owner).is_some() {
        Err(JournalStoreError::invalid(
            workspace_path,
            "immutable journal owner manifest contains a duplicate canonical owner name",
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct InventoryFile {
    sequence: u64,
    name: String,
    path: PathBuf,
    identity: ExactObjectIdentity,
    byte_len: u64,
    mode: PreservedFileMode,
    link_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FinalizationStoreName {
    transaction_id: TransactionId,
    generation: u64,
    partial: bool,
}

fn require_discovery_platform(path: &Path) -> Result<(), JournalStoreError> {
    let _ = path;
    Ok(())
}

fn capture_top_level(
    runtime: &TransactionRuntime,
    workspace_parent: DirectoryEndpoint<'_>,
    require_writable: bool,
) -> Result<TopLevelCapture, JournalStoreError> {
    let observation = runtime
        .fs()
        .observe_directory(workspace_parent)
        .map_err(|source| {
            JournalStoreError::io(
                workspace_parent.path,
                "observe the journal workspace parent for discovery",
                source,
            )
        })?;
    if require_writable {
        require_private_directory_observation(
            &observation,
            "journal workspace parent",
            workspace_parent.path,
        )?;
    } else {
        require_private_directory_observation_read_only(
            &observation,
            "journal workspace parent",
            workspace_parent.path,
        )?;
    }
    let inventory = runtime
        .fs()
        .inventory_directory_exact(workspace_parent, &observation)
        .map_err(|source| {
            JournalStoreError::io(
                workspace_parent.path,
                "inventory the journal workspace parent for discovery",
                source,
            )
        })?;
    enforce_inventory_bound(&inventory, workspace_parent.path)?;
    let namespace = parse_top_level_namespace(&inventory, workspace_parent.path)?;
    Ok(TopLevelCapture {
        inventory,
        namespace,
    })
}

fn capture_bound_top_level(
    runtime: &TransactionRuntime,
    capabilities: JournalDiscoveryCapabilities<'_>,
) -> Result<BoundTopLevelCapture, JournalStoreError> {
    let top_level = capture_top_level(runtime, capabilities.workspace_parent, true)?;
    let write_lock = runtime
        .fs()
        .read_regular_file_exact(
            capabilities.write_lock.parent,
            capabilities.write_lock.name,
            capabilities.write_lock.path,
            MAX_CONTROL_ENVELOPE_BYTES,
        )
        .map_err(|source| {
            JournalStoreError::io(
                capabilities.write_lock.path,
                "read the exact persistent write lock during discovery",
                source,
            )
        })?;
    require_private_observation(
        &write_lock.observation,
        "persistent write lock",
        capabilities.write_lock.path,
    )?;
    if write_lock.observation.identity != capabilities.held_write_lock_identity {
        return Err(JournalStoreError::invalid(
            capabilities.write_lock.path,
            "journal discovery observed a different inode than the held advisory lock",
        ));
    }
    if write_lock.observation.link_count != Some(1) {
        return Err(JournalStoreError::invalid(
            capabilities.write_lock.path,
            "persistent write lock must have exactly one hard link during discovery",
        ));
    }
    Ok(BoundTopLevelCapture {
        top_level,
        write_lock,
    })
}

fn parse_top_level_namespace(
    inventory: &ExactDirectoryInventory,
    parent_path: &Path,
) -> Result<JournalTopLevelNamespace, JournalStoreError> {
    let mut transaction_id = None;
    let mut workspace = None;
    let mut bootstrap_intent = None;
    let mut finalization_records = BTreeSet::new();
    let mut finalization_partial = None;

    for entry in &inventory.entries {
        let name = entry.name.to_str().ok_or_else(|| {
            JournalStoreError::invalid(
                parent_path,
                "top-level journal inventory contains a non-UTF-8 child name",
            )
        })?;
        let path = parent_path.join(name);
        if name.starts_with(TRANSACTION_PREFIX) {
            let parsed = parse_transaction_directory_name(name)
                .map_err(|error| JournalStoreError::model(&path, error))?;
            include_discovered_transaction(&mut transaction_id, &parsed, &path)?;
            require_entry_kind(
                entry,
                ExactDirectoryEntryKind::Directory,
                &path,
                "transaction workspace",
            )?;
            require_private_directory_entry(entry, "transaction workspace", &path)?;
            if workspace
                .replace(DiscoveredWorkspace {
                    name: name.to_owned(),
                    path,
                    observation: ExactDirectoryObservation {
                        identity: entry.identity,
                        mode: entry.mode,
                        link_count: entry.link_count,
                    },
                })
                .is_some()
            {
                return Err(JournalStoreError::invalid(
                    parent_path,
                    "top-level journal namespace contains multiple transaction workspaces",
                ));
            }
            continue;
        }
        if name.starts_with(BOOTSTRAP_INTENT_PREFIX) {
            let parsed = parse_bootstrap_intent_name(name)
                .map_err(|error| JournalStoreError::model(&path, error))?;
            include_discovered_transaction(&mut transaction_id, &parsed, &path)?;
            require_entry_kind(
                entry,
                ExactDirectoryEntryKind::RegularFile,
                &path,
                "bootstrap intent",
            )?;
            require_private_entry(entry, "bootstrap intent", &path)?;
            set_once(&mut bootstrap_intent, (), "bootstrap intent", parent_path)?;
            continue;
        }
        if name.starts_with(FINALIZATION_PREFIX) {
            let parsed = parse_finalization_file_name(name)
                .map_err(|error| JournalStoreError::model(&path, error))?;
            include_discovered_transaction(&mut transaction_id, parsed.transaction_id(), &path)?;
            require_entry_kind(
                entry,
                ExactDirectoryEntryKind::RegularFile,
                &path,
                "finalization authority",
            )?;
            require_private_entry(entry, "finalization authority", &path)?;
            match parsed.kind() {
                FinalizationFileKindV2::Record => {
                    if !finalization_records.insert(parsed.generation()) {
                        return Err(JournalStoreError::invalid(
                            parent_path,
                            "top-level namespace contains duplicate finalization records for one generation",
                        ));
                    }
                }
                FinalizationFileKindV2::Partial => {
                    if finalization_partial.replace(parsed.generation()).is_some() {
                        return Err(JournalStoreError::invalid(
                            parent_path,
                            "top-level namespace contains multiple current finalization partials",
                        ));
                    }
                }
            }
            continue;
        }
        if name.starts_with(BOOTSTRAP_PREFIX) {
            return Err(JournalStoreError::invalid(
                path,
                "bootstrap-owner envelopes are only valid inside their exact transaction workspace",
            ));
        }
        if is_reserved_journal_family(name) {
            return Err(JournalStoreError::invalid(
                path,
                "unknown, noncanonical, or legacy journal child occupies a reserved top-level namespace",
            ));
        }
        return Err(JournalStoreError::invalid(
            path,
            "unknown child occupies the private transaction authority namespace",
        ));
    }
    let namespace = match transaction_id {
        None => JournalTopLevelNamespace::Empty,
        Some(transaction_id) => JournalTopLevelNamespace::Transaction(TopLevelTransaction {
            transaction_id,
            workspace,
        }),
    };
    Ok(namespace)
}

fn include_discovered_transaction(
    current: &mut Option<TransactionId>,
    candidate: &TransactionId,
    path: &Path,
) -> Result<(), JournalStoreError> {
    match current {
        Some(current) if current != candidate => Err(JournalStoreError::invalid(
            path,
            "mixed transaction identifiers occupy the top-level journal namespace",
        )),
        Some(_) => Ok(()),
        None => {
            *current = Some(candidate.clone());
            Ok(())
        }
    }
}

fn is_reserved_journal_family(name: &str) -> bool {
    name.starts_with(RESERVED_TRANSACTION_FAMILY)
        || name.starts_with(RESERVED_BOOTSTRAP_FAMILY)
        || name.starts_with(RESERVED_FINALIZATION_FAMILY)
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

fn require_private_directory_observation_read_only(
    observation: &ExactDirectoryObservation,
    label: &str,
    path: &Path,
) -> Result<(), JournalStoreError> {
    #[cfg(unix)]
    {
        if observation
            .mode
            .posix_mode
            .is_some_and(|mode| mode & 0o077 == 0)
        {
            return Ok(());
        }
        Err(JournalStoreError::invalid(
            path,
            format!("{label} must not grant group or other permissions"),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = (observation, label, path);
        Ok(())
    }
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
        ObjectIdentityV2::from_parts(
            observation.identity.namespace(),
            observation.identity.object(),
        ),
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
        ObjectIdentityV2::from_parts(
            observation.identity.namespace(),
            observation.identity.object(),
        ),
        DirectoryModeV2::new(observation.mode.readonly, observation.mode.posix_mode)?,
        observation
            .link_count
            .ok_or_else(|| JournalModelError::new("exact directory link count is unavailable"))?,
    )
}

fn file_identity(exact: &ExactFileStateV2) -> ExactObjectIdentity {
    model_identity(exact.identity())
}

fn model_identity(identity: ObjectIdentityV2) -> ExactObjectIdentity {
    ExactObjectIdentity::from_parts(identity.namespace_bytes(), identity.object_bytes())
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

fn authenticate_expected_world(
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
            reason: "publication observations do not match the prepared canonical envelope"
                .to_owned(),
            partial,
            published,
        };
    }
    classify_candidate_world(partial, published)
}

fn finalization_transition(lease: &FinalizationLeaseV2, window: TransitionWindow) -> TransitionKey {
    let outcome = match lease.outcome() {
        FinalizationOutcomeV2::Commit => TransactionOutcome::Commit,
        FinalizationOutcomeV2::Rollback => TransactionOutcome::Rollback,
    };
    if lease.generation() == 0 {
        TransitionKey::PublishFinalizationLease {
            outcome,
            generation: lease.generation(),
            window,
        }
    } else {
        TransitionKey::PublishFinalizationProgress {
            outcome,
            generation: lease.generation(),
            window,
        }
    }
}

fn removal_transition(
    object: RemovalObject,
    outcome: TransactionOutcome,
    window: TransitionWindow,
) -> TransitionKey {
    match object {
        RemovalObject::BootstrapIntent | RemovalObject::BootstrapOwner => {
            TransitionKey::RemoveWorkspaceOwnership { outcome, window }
        }
        RemovalObject::JournalRecord { sequence } => TransitionKey::RemoveJournalHistory {
            outcome,
            kind: JournalRecordKind::Published,
            sequence,
            window,
        },
        RemovalObject::JournalPartial { sequence } => TransitionKey::RemoveJournalHistory {
            outcome,
            kind: JournalRecordKind::Partial,
            sequence,
            window,
        },
        RemovalObject::Workspace => TransitionKey::RemoveTransactionWorkspace { outcome, window },
        RemovalObject::Finalization { generation } => TransitionKey::RemoveFinalizationLease {
            outcome,
            generation,
            window,
        },
    }
}

const fn active_removal_mutation(mutation: RemovalMutation) -> ActiveReconciliationMutation {
    match mutation {
        RemovalMutation::RemoveExact => ActiveReconciliationMutation::RemovePartial,
        RemovalMutation::ObserveParent => ActiveReconciliationMutation::ObserveCleanupParent,
        RemovalMutation::SyncParent => ActiveReconciliationMutation::SyncCleanupParent,
        RemovalMutation::VerifyAbsent => ActiveReconciliationMutation::VerifyCleanup,
    }
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

fn parse_finalization_store_name(name: &str) -> Result<FinalizationStoreName, String> {
    let parsed = parse_finalization_file_name(name).map_err(|error| error.reason().to_owned())?;
    Ok(FinalizationStoreName {
        transaction_id: parsed.transaction_id().clone(),
        generation: parsed.generation(),
        partial: parsed.kind() == FinalizationFileKindV2::Partial,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use cap_std::ambient_authority;

    use super::*;
    use crate::transaction::fs::{FaultFs, FsOperation, FsOps};
    use crate::transaction::journal::{finalization_partial_name, finalization_record_name};
    use crate::transaction::runtime::{DeterministicEntropy, RecordingTransitionObserver};

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set fixture mode");
    }

    #[cfg(not(unix))]
    fn set_mode(_path: &Path, _mode: u32) {}

    fn observation(identity: (u64, u64), links: u64) -> ExactFileObservation {
        ExactFileObservation {
            identity: ExactObjectIdentity::from_unix(identity.0, identity.1),
            byte_len: 17,
            content_hash: format!("sha256:{}", "a".repeat(64)),
            mode: PreservedFileMode {
                readonly: false,
                posix_mode: platform_mode(PRIVATE_FILE_MODE),
            },
            link_count: Some(links),
        }
    }

    fn inventory_child(
        name: impl Into<std::ffi::OsString>,
        kind: ExactDirectoryEntryKind,
        identity: (u64, u64),
        mode: u32,
    ) -> ExactDirectoryEntry {
        ExactDirectoryEntry {
            name: name.into(),
            kind,
            identity: ExactObjectIdentity::from_unix(identity.0, identity.1),
            byte_len: 0,
            mode: PreservedFileMode {
                readonly: false,
                posix_mode: platform_mode(mode),
            },
            link_count: Some(1),
        }
    }

    fn top_level_inventory(entries: Vec<ExactDirectoryEntry>) -> ExactDirectoryInventory {
        ExactDirectoryInventory {
            directory: ExactDirectoryObservation {
                identity: ExactObjectIdentity::from_unix(1, 2),
                mode: PreservedFileMode {
                    readonly: false,
                    posix_mode: platform_mode(PRIVATE_DIRECTORY_MODE),
                },
                link_count: Some(2),
            },
            entries,
        }
    }

    #[test]
    fn top_level_discovery_binds_one_transaction_in_the_private_namespace() {
        let transaction_id =
            TransactionId::parse("4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a").expect("transaction id");
        let workspace_name = transaction_directory_name(&transaction_id);
        let intent_name = bootstrap_intent_name(&transaction_id);
        let inventory = top_level_inventory(vec![
            inventory_child(
                workspace_name,
                ExactDirectoryEntryKind::Directory,
                (1, 12),
                PRIVATE_DIRECTORY_MODE,
            ),
            inventory_child(
                intent_name,
                ExactDirectoryEntryKind::RegularFile,
                (1, 13),
                PRIVATE_FILE_MODE,
            ),
        ]);
        let namespace = parse_top_level_namespace(&inventory, Path::new(".transactions"))
            .expect("strict discovery");
        let JournalTopLevelNamespace::Transaction(discovered) = namespace else {
            panic!("expected one transaction");
        };
        assert_eq!(discovered.transaction_id(), &transaction_id);
        assert_eq!(
            discovered.workspace_path(),
            Some(
                Path::new(".transactions")
                    .join(transaction_directory_name(&transaction_id))
                    .as_path()
            )
        );
    }

    #[test]
    fn top_level_discovery_rejects_mixed_and_duplicate_transactions() {
        let first = TransactionId::parse("4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a").expect("first id");
        let second = TransactionId::parse("5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b").expect("second id");
        let mixed = top_level_inventory(vec![
            inventory_child(
                transaction_directory_name(&first),
                ExactDirectoryEntryKind::Directory,
                (1, 20),
                PRIVATE_DIRECTORY_MODE,
            ),
            inventory_child(
                bootstrap_intent_name(&second),
                ExactDirectoryEntryKind::RegularFile,
                (1, 21),
                PRIVATE_FILE_MODE,
            ),
        ]);
        assert!(
            parse_top_level_namespace(&mixed, Path::new(".transactions"))
                .expect_err("mixed ids must fail")
                .reason()
                .contains("mixed transaction identifiers")
        );

        let workspace = inventory_child(
            transaction_directory_name(&first),
            ExactDirectoryEntryKind::Directory,
            (1, 22),
            PRIVATE_DIRECTORY_MODE,
        );
        let duplicate = top_level_inventory(vec![workspace.clone(), workspace]);
        assert!(
            parse_top_level_namespace(&duplicate, Path::new(".transactions"))
                .expect_err("duplicate workspaces must fail")
                .reason()
                .contains("multiple transaction workspaces")
        );
    }

    #[test]
    fn top_level_discovery_rejects_reserved_v1_and_nested_namespace_names() {
        let transaction_id =
            TransactionId::parse("4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a").expect("transaction id");
        let reserved_v1 = top_level_inventory(vec![inventory_child(
            "transaction-v1-4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a",
            ExactDirectoryEntryKind::Directory,
            (1, 30),
            PRIVATE_DIRECTORY_MODE,
        )]);
        assert!(
            parse_top_level_namespace(&reserved_v1, Path::new(".transactions"))
                .expect_err("v1 reserved family must fail")
                .reason()
                .contains("reserved top-level namespace")
        );

        let nested_namespace = top_level_inventory(vec![
            inventory_child(
                ".transactions",
                ExactDirectoryEntryKind::Directory,
                (1, 31),
                PRIVATE_DIRECTORY_MODE,
            ),
            inventory_child(
                transaction_directory_name(&transaction_id),
                ExactDirectoryEntryKind::Directory,
                (1, 32),
                PRIVATE_DIRECTORY_MODE,
            ),
        ]);
        assert!(
            parse_top_level_namespace(&nested_namespace, Path::new(".transactions"))
                .expect_err("a nested authority namespace must fail")
                .reason()
                .contains("unknown child")
        );
    }

    #[cfg(unix)]
    #[test]
    fn top_level_discovery_rejects_non_utf8_names() {
        use std::os::unix::ffi::OsStringExt;

        let inventory = top_level_inventory(vec![inventory_child(
            std::ffi::OsString::from_vec(vec![0xff]),
            ExactDirectoryEntryKind::RegularFile,
            (1, 40),
            PRIVATE_FILE_MODE,
        )]);
        assert!(
            parse_top_level_namespace(&inventory, Path::new(".transactions"))
                .expect_err("non-UTF-8 namespace must fail")
                .reason()
                .contains("non-UTF-8")
        );
    }

    #[test]
    fn publication_world_requires_exact_link_topology() {
        let prepared = observation((7, 11), 1);
        assert!(matches!(
            classify_candidate_world(Some(prepared.clone()), None),
            ObservedCandidateWorld::PreparedOnly { .. }
        ));
        assert!(matches!(
            classify_candidate_world(None, Some(prepared.clone())),
            ObservedCandidateWorld::PublishedOnly { .. }
        ));

        let partial = observation((7, 11), 2);
        let published = partial.clone();
        assert!(matches!(
            authenticate_expected_world(Some(partial), Some(published), &prepared),
            ObservedCandidateWorld::LinkedAliases { .. }
        ));

        let substituted = observation((7, 12), 2);
        assert!(matches!(
            authenticate_expected_world(
                Some(observation((7, 11), 2)),
                Some(substituted),
                &prepared,
            ),
            ObservedCandidateWorld::Conflict { .. }
        ));
    }

    #[test]
    fn finalization_namespace_uses_generation_native_model_parser() {
        let transaction_id =
            TransactionId::parse("4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a").expect("transaction id");
        for generation in [0, 1, u64::MAX] {
            let record = finalization_record_name(&transaction_id, generation);
            let partial = finalization_partial_name(&transaction_id, generation);
            let record = parse_finalization_store_name(&record).expect("record name");
            let partial = parse_finalization_store_name(&partial).expect("partial name");
            assert_eq!(record.transaction_id, transaction_id);
            assert_eq!(record.generation, generation);
            assert!(!record.partial);
            assert_eq!(partial.transaction_id, transaction_id);
            assert_eq!(partial.generation, generation);
            assert!(partial.partial);
        }
        assert!(parse_finalization_store_name("finalization-v2-bad.json").is_err());
    }

    fn remove_with_fault(
        operation: FsOperation,
        ordinal: usize,
        retry_after_success: bool,
    ) -> (ExactRemovalDisposition, bool) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let kit_path = temporary.path().join("_kit");
        fs::create_dir(&kit_path).expect("kit directory");
        let transactions_path = kit_path.join(".transactions");
        fs::create_dir(&transactions_path).expect("transaction namespace");
        let lock_path = kit_path.join(WRITE_LOCK_NAME);
        fs::write(&lock_path, b"held lock\n").expect("write lock fixture");
        set_mode(&lock_path, PRIVATE_FILE_MODE);
        let transaction_id =
            TransactionId::parse("4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a").expect("transaction id");
        let victim_name = finalization_record_name(&transaction_id, 0);
        let victim_path = transactions_path.join(&victim_name);
        fs::write(&victim_path, b"transaction evidence\n").expect("write victim");
        set_mode(&victim_path, PRIVATE_FILE_MODE);

        let fault = Arc::new(FaultFs::fail_nth(operation, ordinal));
        let runtime = TransactionRuntime::new(
            fault.clone(),
            Arc::new(DeterministicEntropy::new()),
            Arc::new(RecordingTransitionObserver::new()),
        );
        let root = Dir::open_ambient_dir(temporary.path(), ambient_authority())
            .expect("open fixture root");
        let kit = root.open_dir("_kit").expect("open kit capability");
        let transactions = kit
            .open_dir(".transactions")
            .expect("open transaction namespace capability");
        let kit_endpoint = DirectoryEndpoint::new(&root, Path::new("_kit"), &kit, &kit_path);
        let transactions_endpoint = DirectoryEndpoint::new(
            &kit,
            Path::new(".transactions"),
            &transactions,
            &transactions_path,
        );
        let kit_observation = fault
            .observe_directory(kit_endpoint)
            .expect("observe kit before selected fault");
        let lock_observation = fault
            .read_regular_file_exact(
                &kit,
                Path::new(WRITE_LOCK_NAME),
                &lock_path,
                MAX_CONTROL_ENVELOPE_BYTES,
            )
            .expect("read lock before selected fault")
            .observation;
        let victim = fault
            .read_regular_file_exact(
                &transactions,
                Path::new(&victim_name),
                &victim_path,
                MAX_CONTROL_ENVELOPE_BYTES,
            )
            .expect("read victim before selected fault");
        let exact = exact_file(&victim.observation).expect("exact victim");
        let capabilities = JournalStoreCapabilities::finalization_only(
            temporary.path(),
            kit_observation,
            kit_observation.identity,
            lock_observation.identity,
            HardLinkEndpoint::new(&kit, Path::new(WRITE_LOCK_NAME), &lock_path),
            transactions_endpoint,
        );
        let store = JournalRecoveryStore::bind(
            &runtime,
            transaction_id,
            Sha256Digest::parse(&format!("sha256:{}", "b".repeat(64))).expect("root digest"),
            capabilities,
        )
        .expect("bind store");
        let mut disposition = store
            .remove_exact_file(
                transactions_endpoint,
                &victim_name,
                &exact,
                MAX_CONTROL_ENVELOPE_BYTES,
                RemovalObject::Finalization { generation: 0 },
                TransactionOutcome::Commit,
            )
            .expect("typed removal disposition");
        if retry_after_success {
            disposition = store
                .remove_exact_file(
                    transactions_endpoint,
                    &victim_name,
                    &exact,
                    MAX_CONTROL_ENVELOPE_BYTES,
                    RemovalObject::Finalization { generation: 0 },
                    TransactionOutcome::Commit,
                )
                .expect("already-absent retry remains a typed disposition");
        }
        (disposition, victim_path.exists())
    }

    #[test]
    fn injected_pre_unlink_failure_reconciles_to_exact_presence() {
        let (disposition, exists) = remove_with_fault(FsOperation::RemoveFileExact, 1, false);
        assert!(exists);
        let ExactRemovalDisposition::ReconcileRequired(reconciliation) = disposition else {
            panic!("failure must require reconciliation");
        };
        assert_eq!(reconciliation.mutation, RemovalMutation::RemoveExact);
        assert!(matches!(reconciliation.world, RemovalWorld::PresentExact));
    }

    #[test]
    fn injected_post_unlink_parent_sync_failure_reconciles_to_absence() {
        let (disposition, exists) = remove_with_fault(FsOperation::SyncJournalParent, 1, false);
        assert!(!exists);
        let ExactRemovalDisposition::ReconcileRequired(reconciliation) = disposition else {
            panic!("failure must require reconciliation");
        };
        assert_eq!(reconciliation.mutation, RemovalMutation::SyncParent);
        assert!(matches!(reconciliation.world, RemovalWorld::Missing));
    }

    #[test]
    fn injected_post_unlink_inventory_failure_remains_typed() {
        // The first inventory validates the complete lock/parent authority at
        // the mutation boundary; the second is the post-unlink absence check.
        let (disposition, exists) =
            remove_with_fault(FsOperation::InventoryDirectoryExact, 2, false);
        assert!(!exists);
        let ExactRemovalDisposition::ReconcileRequired(reconciliation) = disposition else {
            panic!("failure must require reconciliation");
        };
        assert_eq!(reconciliation.mutation, RemovalMutation::VerifyAbsent);
        assert!(matches!(reconciliation.world, RemovalWorld::Missing));
    }

    #[test]
    fn already_absent_exact_removal_is_durably_idempotent() {
        let (disposition, exists) =
            remove_with_fault(FsOperation::RemoveFileExact, usize::MAX, true);
        assert!(!exists);
        assert!(matches!(
            disposition,
            ExactRemovalDisposition::DurableAbsent
        ));
    }
}
