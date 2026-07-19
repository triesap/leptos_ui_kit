#![forbid(unsafe_code)]

//! Conservative, read-only policy for journal-v2 recovery.
//!
//! The strict store owns namespace discovery, canonical envelope parsing,
//! immutable lineage validation, and partial/published-name reconciliation.
//! This module owns the policy shared by check, doctor, dry-run, and mutating
//! recovery: classify the durable phase, require a complete exact filesystem
//! world, accept only one protocol before-world or after-world, and block on a
//! hybrid or third state before the transaction engine is entered.
//!
//! The capture adapter is responsible for observing every key in
//! `RecoveryObjectKey` twice through `TransactionRuntime::fs()`, inventorying
//! every transaction-created directory, and proving that the two captures are
//! equal. This module then proves that the capture is complete and consistent
//! with the immutable journal model. It never parses a namespace filename and
//! never performs a filesystem mutation.

use std::{collections::BTreeMap, path::Path};

use crate::CodegenError;

use super::journal::{
    ArtifactOrdinal, CleanupIntentV2, CleanupTargetV2, DirectoryDispositionV2,
    DirectoryPublicationWorldV2, EntryActionV2, ExactDirectoryMetadataV2, ExactDirectoryStateV2,
    ExactFileMetadataV2, ExactFileStateV2, FileArtifactKindV2, JournalPhaseV2, JournalSnapshotV2,
    ManagedChildKindV2, OwnedResidualDeleteBindingV2, OwnedResidualObjectV2, OwnerArtifactKindV2,
    OwnerCreationIntentV2, PreparationPendingIntentV2, PreparationPlacementIntentV2,
    PreparationPlacementWorldV2, PresenceV2, RollbackIntentV2,
};
use super::store::{ActiveJournalLoad, ActiveReconciliation, ObservedCandidateWorld};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryOutcomeV2 {
    Rollback,
    Commit,
}

/// Durable phase policy shared by every command surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryPhaseActionV2 {
    BeginRollback,
    ResumeRollback {
        next: u32,
        has_pending_intent: bool,
    },
    ResumeCleanup {
        outcome: RecoveryOutcomeV2,
        completed: u32,
        has_pending_intent: bool,
    },
}

/// `CommitComplete` is the sole finish-only boundary. Every other reachable
/// phase is rollback-class, including `RollbackComplete` cleanup.
pub(super) const fn classify_phase(phase: &JournalPhaseV2) -> RecoveryPhaseActionV2 {
    match phase {
        JournalPhaseV2::Preparing { .. }
        | JournalPhaseV2::Prepared
        | JournalPhaseV2::Replacing { .. } => RecoveryPhaseActionV2::BeginRollback,
        JournalPhaseV2::RollingBack { next, pending } => RecoveryPhaseActionV2::ResumeRollback {
            next: *next,
            has_pending_intent: pending.is_some(),
        },
        JournalPhaseV2::RollbackComplete {
            cleanup_completed,
            pending,
        } => RecoveryPhaseActionV2::ResumeCleanup {
            outcome: RecoveryOutcomeV2::Rollback,
            completed: *cleanup_completed,
            has_pending_intent: pending.is_some(),
        },
        JournalPhaseV2::CommitComplete {
            cleanup_completed,
            pending,
        } => RecoveryPhaseActionV2::ResumeCleanup {
            outcome: RecoveryOutcomeV2::Commit,
            completed: *cleanup_completed,
            has_pending_intent: pending.is_some(),
        },
    }
}

/// Store-owned action needed to reconcile an immutable record publication.
/// A visible record is never interpreted by this policy directly; the store
/// must authenticate, durably reconcile, and reload the lineage first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecordReconciliationActionV2 {
    ReloadPredecessor,
    RemoveOwnedPartial,
    AdoptPublishedAndReload,
}

pub(super) fn classify_record_reconciliation(
    reconciliation: &ActiveReconciliation,
    journal_path: &Path,
) -> Result<RecordReconciliationActionV2, CodegenError> {
    if reconciliation.sequence() != reconciliation.stable_record_count() as u64 {
        return Err(recovery_blocked(
            journal_path,
            "journal publication reconciliation is not the next contiguous sequence",
        ));
    }
    match reconciliation.world() {
        ObservedCandidateWorld::Missing => Ok(RecordReconciliationActionV2::ReloadPredecessor),
        ObservedCandidateWorld::PreparedOnly { .. }
        | ObservedCandidateWorld::OwnedIncomplete { .. } => {
            Ok(RecordReconciliationActionV2::RemoveOwnedPartial)
        }
        ObservedCandidateWorld::PublishedOnly { .. }
        | ObservedCandidateWorld::LinkedAliases { .. } => {
            Ok(RecordReconciliationActionV2::AdoptPublishedAndReload)
        }
        ObservedCandidateWorld::Conflict { reason, .. }
        | ObservedCandidateWorld::ObservationUnavailable { reason } => Err(recovery_blocked(
            journal_path,
            format!("journal publication has a third state: {reason}"),
        )),
    }
}

/// Every path-bearing object that can influence a recovery decision. The
/// exact capture must contain this complete key set; omission is not treated
/// as absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum RecoveryObjectKey {
    Target(ArtifactOrdinal),
    StageOwner(ArtifactOrdinal),
    Stage(ArtifactOrdinal),
    BackupOwner(ArtifactOrdinal),
    Backup(ArtifactOrdinal),
    Directory(ArtifactOrdinal),
    DirectoryOwner(ArtifactOrdinal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryObjectKindV2 {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExactRecoveryObject {
    Missing,
    File(ExactFileStateV2),
    FileMetadata(ExactFileMetadataV2),
    Directory(ExactDirectoryStateV2),
    DirectoryMetadata(ExactDirectoryMetadataV2),
}

impl ExactRecoveryObject {
    const fn kind(&self) -> Option<RecoveryObjectKindV2> {
        match self {
            Self::Missing => None,
            Self::File(_) | Self::FileMetadata(_) => Some(RecoveryObjectKindV2::File),
            Self::Directory(_) | Self::DirectoryMetadata(_) => {
                Some(RecoveryObjectKindV2::Directory)
            }
        }
    }
}

pub(super) type ExactRecoveryInventory = BTreeMap<String, ExactRecoveryObject>;

/// A stable, complete cohort capture.
///
/// `inventories` contains entries only for present transaction-created
/// directories and their private candidates. Each child value is the exact
/// state observed through the directory inventory, allowing policy to prove
/// that it matches the independently observed cohort path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExactRecoveryWorld {
    objects: BTreeMap<RecoveryObjectKey, ExactRecoveryObject>,
    inventories: BTreeMap<RecoveryObjectKey, ExactRecoveryInventory>,
}

impl ExactRecoveryWorld {
    pub(super) fn from_complete_capture(
        objects: BTreeMap<RecoveryObjectKey, ExactRecoveryObject>,
        inventories: BTreeMap<RecoveryObjectKey, ExactRecoveryInventory>,
    ) -> Self {
        Self {
            objects,
            inventories,
        }
    }

    pub(super) fn inventories(&self) -> &BTreeMap<RecoveryObjectKey, ExactRecoveryInventory> {
        &self.inventories
    }

    fn object(&self, key: RecoveryObjectKey) -> Option<&ExactRecoveryObject> {
        self.objects.get(&key)
    }

    fn same_objects(&self, other: &Self) -> bool {
        self.objects == other.objects
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MutationWorldV2 {
    Before,
    After,
}

/// The result of global preflight. At most one protocol mutation may be
/// uncertain. All other objects and all created-directory inventories must be
/// exactly equal to the durable snapshot world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecoveryPreflightV2 {
    ExactSnapshot,
    PendingOwnerCreation {
        residual: Option<OwnedResidualDeleteBindingV2>,
    },
    PendingOwnerDiscard {
        world: MutationWorldV2,
    },
    PendingPlacement {
        ordinal: ArtifactOrdinal,
        artifact: RecoveryPreparationArtifactV2,
        world: MutationWorldV2,
    },
    ForwardReplacementCompleted {
        ordinal: ArtifactOrdinal,
    },
    PendingRollback {
        ordinal: ArtifactOrdinal,
        world: MutationWorldV2,
    },
    PendingCleanup {
        target: CleanupTargetV2,
        world: MutationWorldV2,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryPreparationArtifactV2 {
    Directory,
    Stage,
    Backup,
}

/// Classifies a stable exact capture without changing the filesystem.
pub(super) fn preflight_recovery_world(
    snapshot: &JournalSnapshotV2,
    observed: &ExactRecoveryWorld,
    journal_path: &Path,
) -> Result<RecoveryPreflightV2, CodegenError> {
    let before = exact_snapshot_world(snapshot);
    require_same_key_set(&before, observed, journal_path)?;
    validate_created_directory_inventories(snapshot, observed, journal_path)?;

    if observed.same_objects(&before) {
        if let JournalPhaseV2::Preparing {
            pending: Some(intent),
            ..
        } = snapshot.phase()
        {
            return match intent {
                PreparationPendingIntentV2::Place(intent) => {
                    classify_pending_placement(snapshot, observed, intent, journal_path)
                }
                PreparationPendingIntentV2::Create(_) => {
                    Ok(RecoveryPreflightV2::PendingOwnerCreation { residual: None })
                }
                PreparationPendingIntentV2::Discard(_) => {
                    Ok(RecoveryPreflightV2::PendingOwnerDiscard {
                        world: MutationWorldV2::After,
                    })
                }
            };
        }
        return Ok(exact_snapshot_decision(snapshot));
    }

    match snapshot.phase() {
        JournalPhaseV2::Preparing {
            completed,
            pending: Some(intent),
        } => match intent {
            PreparationPendingIntentV2::Create(intent) => classify_pending_owner_creation(
                snapshot,
                *completed as usize,
                observed,
                intent,
                journal_path,
            ),
            PreparationPendingIntentV2::Place(intent) => {
                classify_pending_placement(snapshot, observed, intent, journal_path)
            }
            PreparationPendingIntentV2::Discard(binding) => {
                classify_pending_owner_discard(snapshot, observed, binding, journal_path)
            }
        },
        JournalPhaseV2::Prepared | JournalPhaseV2::Replacing { .. } => {
            classify_forward_replacement(snapshot, observed, journal_path)
        }
        JournalPhaseV2::RollingBack {
            pending: Some(intent),
            ..
        } => classify_pending_rollback(snapshot, observed, intent, journal_path),
        JournalPhaseV2::RollbackComplete {
            pending: Some(intent),
            ..
        }
        | JournalPhaseV2::CommitComplete {
            pending: Some(intent),
            ..
        } => classify_pending_cleanup(snapshot, observed, intent, journal_path),
        JournalPhaseV2::Preparing { pending: None, .. }
        | JournalPhaseV2::RollingBack { pending: None, .. }
        | JournalPhaseV2::RollbackComplete { pending: None, .. }
        | JournalPhaseV2::CommitComplete { pending: None, .. } => Err(recovery_blocked(
            journal_path,
            "recovery world differs from the durable journal without a pending mutation intent",
        )),
    }
}

fn owner_key(intent: &OwnerCreationIntentV2) -> RecoveryObjectKey {
    match intent.artifact() {
        OwnerArtifactKindV2::Directory => RecoveryObjectKey::DirectoryOwner(intent.ordinal()),
        OwnerArtifactKindV2::Stage => RecoveryObjectKey::StageOwner(intent.ordinal()),
        OwnerArtifactKindV2::Backup => RecoveryObjectKey::BackupOwner(intent.ordinal()),
    }
}

fn residual_recovery_object(object: &OwnedResidualObjectV2) -> ExactRecoveryObject {
    match object {
        OwnedResidualObjectV2::File(exact) => ExactRecoveryObject::FileMetadata(exact.clone()),
        OwnedResidualObjectV2::Directory(exact) => {
            ExactRecoveryObject::DirectoryMetadata(exact.clone())
        }
    }
}

fn classify_pending_owner_creation(
    snapshot: &JournalSnapshotV2,
    completed: usize,
    observed: &ExactRecoveryWorld,
    intent: &OwnerCreationIntentV2,
    journal_path: &Path,
) -> Result<RecoveryPreflightV2, CodegenError> {
    let key = owner_key(intent);
    if intent.artifact() == OwnerArtifactKindV2::Directory
        && !observed
            .inventories()
            .get(&key)
            .is_some_and(BTreeMap::is_empty)
    {
        return Err(recovery_blocked(
            journal_path,
            "pending Create directory residual lacks a bounded exact-empty inventory",
        ));
    }
    let object = match (intent.artifact(), observed.object(key)) {
        (OwnerArtifactKindV2::Directory, Some(ExactRecoveryObject::DirectoryMetadata(exact))) => {
            OwnedResidualObjectV2::Directory(exact.clone())
        }
        (
            OwnerArtifactKindV2::Stage | OwnerArtifactKindV2::Backup,
            Some(ExactRecoveryObject::FileMetadata(exact)),
        ) => OwnedResidualObjectV2::File(exact.clone()),
        _ => {
            return Err(recovery_blocked(
                journal_path,
                "pending Create residual has the wrong kind or observation form",
            ));
        }
    };
    let binding = OwnedResidualDeleteBindingV2::new(intent.clone(), object);
    snapshot
        .validate_owner_residual(completed, &binding)
        .map_err(|error| recovery_blocked(journal_path, error.reason()))?;
    let mut allowed = exact_snapshot_world(snapshot);
    allowed
        .objects
        .insert(key, residual_recovery_object(binding.object()));
    if !allowed.same_objects(observed) {
        return Err(recovery_blocked(
            journal_path,
            "pending Create residual is mixed with another cohort change",
        ));
    }
    Ok(RecoveryPreflightV2::PendingOwnerCreation {
        residual: Some(binding),
    })
}

fn classify_pending_owner_discard(
    snapshot: &JournalSnapshotV2,
    observed: &ExactRecoveryWorld,
    binding: &OwnedResidualDeleteBindingV2,
    journal_path: &Path,
) -> Result<RecoveryPreflightV2, CodegenError> {
    let key = owner_key(binding.owner());
    if binding.owner().artifact() == OwnerArtifactKindV2::Directory
        && !observed
            .inventories()
            .get(&key)
            .is_some_and(BTreeMap::is_empty)
    {
        return Err(recovery_blocked(
            journal_path,
            "pending Discard directory residual lacks a bounded exact-empty inventory",
        ));
    }
    let mut allowed = exact_snapshot_world(snapshot);
    allowed
        .objects
        .insert(key, residual_recovery_object(binding.object()));
    if !allowed.same_objects(observed) {
        return Err(recovery_blocked(
            journal_path,
            "pending Discard differs from its exact residual-before world",
        ));
    }
    Ok(RecoveryPreflightV2::PendingOwnerDiscard {
        world: MutationWorldV2::Before,
    })
}

fn exact_snapshot_decision(snapshot: &JournalSnapshotV2) -> RecoveryPreflightV2 {
    match snapshot.phase() {
        JournalPhaseV2::RollingBack {
            pending: Some(intent),
            ..
        } => RecoveryPreflightV2::PendingRollback {
            ordinal: intent.ordinal(),
            world: MutationWorldV2::Before,
        },
        JournalPhaseV2::RollbackComplete {
            pending: Some(intent),
            ..
        }
        | JournalPhaseV2::CommitComplete {
            pending: Some(intent),
            ..
        } => RecoveryPreflightV2::PendingCleanup {
            target: intent.target(),
            world: MutationWorldV2::Before,
        },
        _ => RecoveryPreflightV2::ExactSnapshot,
    }
}

fn exact_snapshot_world(snapshot: &JournalSnapshotV2) -> ExactRecoveryWorld {
    let mut objects = BTreeMap::new();
    for entry in snapshot.entries() {
        objects.insert(
            RecoveryObjectKey::Target(entry.ordinal()),
            file_presence(entry.current_target()),
        );
        objects.insert(
            RecoveryObjectKey::StageOwner(entry.ordinal()),
            file_presence(entry.stage().owner_current()),
        );
        objects.insert(
            RecoveryObjectKey::Stage(entry.ordinal()),
            file_presence(entry.stage().current()),
        );
        if let Some(backup) = entry.backup() {
            objects.insert(
                RecoveryObjectKey::BackupOwner(entry.ordinal()),
                file_presence(backup.owner_current()),
            );
            objects.insert(
                RecoveryObjectKey::Backup(entry.ordinal()),
                file_presence(backup.current()),
            );
        }
    }
    for directory in snapshot.directories() {
        objects.insert(
            RecoveryObjectKey::Directory(directory.ordinal()),
            directory_presence(directory.current()),
        );
        if directory.candidate_name().is_some() {
            objects.insert(
                RecoveryObjectKey::DirectoryOwner(directory.ordinal()),
                directory_presence(directory.candidate_current()),
            );
        }
    }
    ExactRecoveryWorld {
        objects,
        inventories: BTreeMap::new(),
    }
}

fn file_presence(presence: &PresenceV2<ExactFileStateV2>) -> ExactRecoveryObject {
    match presence {
        PresenceV2::Missing => ExactRecoveryObject::Missing,
        PresenceV2::Present(exact) => ExactRecoveryObject::File(exact.clone()),
    }
}

fn directory_presence(presence: &PresenceV2<ExactDirectoryStateV2>) -> ExactRecoveryObject {
    match presence {
        PresenceV2::Missing => ExactRecoveryObject::Missing,
        PresenceV2::Present(exact) => ExactRecoveryObject::Directory(exact.clone()),
    }
}

fn require_same_key_set(
    expected: &ExactRecoveryWorld,
    observed: &ExactRecoveryWorld,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    if expected.objects.keys().eq(observed.objects.keys()) {
        Ok(())
    } else {
        Err(recovery_blocked(
            journal_path,
            "exact recovery capture omitted or added an owner, placement, target, backup, or directory",
        ))
    }
}

fn validate_created_directory_inventories(
    snapshot: &JournalSnapshotV2,
    observed: &ExactRecoveryWorld,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    let mut required_inventory_keys = BTreeMap::new();
    for directory in snapshot
        .directories()
        .iter()
        .filter(|directory| directory.disposition() == DirectoryDispositionV2::Create)
    {
        let directory_key = RecoveryObjectKey::Directory(directory.ordinal());
        validate_directory_inventory_slot(
            snapshot,
            observed,
            directory,
            directory_key,
            false,
            journal_path,
        )?;
        if matches!(
            observed.object(directory_key),
            Some(ExactRecoveryObject::Directory(_) | ExactRecoveryObject::DirectoryMetadata(_))
        ) {
            required_inventory_keys.insert(directory_key, ());
        }

        let candidate_key = RecoveryObjectKey::DirectoryOwner(directory.ordinal());
        validate_directory_inventory_slot(
            snapshot,
            observed,
            directory,
            candidate_key,
            true,
            journal_path,
        )?;
        if matches!(
            observed.object(candidate_key),
            Some(ExactRecoveryObject::Directory(_) | ExactRecoveryObject::DirectoryMetadata(_))
        ) {
            required_inventory_keys.insert(candidate_key, ());
        }
    }
    if !required_inventory_keys
        .keys()
        .eq(observed.inventories.keys())
    {
        return Err(recovery_blocked(
            journal_path,
            "created-directory inventory set is incomplete or contains an unrequested directory",
        ));
    }
    Ok(())
}

fn validate_directory_inventory_slot(
    snapshot: &JournalSnapshotV2,
    observed: &ExactRecoveryWorld,
    directory: &super::journal::JournalDirectoryV2,
    key: RecoveryObjectKey,
    candidate: bool,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    match observed.object(key) {
        Some(ExactRecoveryObject::Missing) => {
            if observed.inventories.contains_key(&key) {
                return Err(recovery_blocked(
                    journal_path,
                    "missing transaction-created directory has a contradictory inventory",
                ));
            }
            Ok(())
        }
        Some(ExactRecoveryObject::File(_) | ExactRecoveryObject::FileMetadata(_)) => {
            Err(recovery_blocked(
                journal_path,
                "transaction-created directory was substituted with a regular file",
            ))
        }
        Some(ExactRecoveryObject::DirectoryMetadata(_)) => {
            let actual = observed.inventories.get(&key).ok_or_else(|| {
                recovery_blocked(
                    journal_path,
                    "present metadata-only directory owner lacks its exact inventory",
                )
            })?;
            if candidate && actual.is_empty() {
                Ok(())
            } else {
                Err(recovery_blocked(
                    journal_path,
                    "metadata-only directory observation is only valid for an empty pending owner residual",
                ))
            }
        }
        Some(ExactRecoveryObject::Directory(_)) => {
            let actual = observed.inventories.get(&key).ok_or_else(|| {
                recovery_blocked(
                    journal_path,
                    "present transaction-created directory lacks its exact inventory",
                )
            })?;
            if candidate {
                if actual.is_empty() {
                    return Ok(());
                }
                return Err(recovery_blocked(
                    journal_path,
                    "private directory candidate contains an unowned child",
                ));
            }
            let expected = expected_managed_inventory(snapshot, observed, directory, journal_path)?;
            if actual == &expected {
                Ok(())
            } else {
                Err(recovery_blocked(
                    journal_path,
                    "created directory contains a missing, substituted, duplicate, or unowned child",
                ))
            }
        }
        None => Err(recovery_blocked(
            journal_path,
            "transaction-created directory is absent from the complete capture",
        )),
    }
}

fn expected_managed_inventory(
    snapshot: &JournalSnapshotV2,
    observed: &ExactRecoveryWorld,
    directory: &super::journal::JournalDirectoryV2,
    journal_path: &Path,
) -> Result<ExactRecoveryInventory, CodegenError> {
    let declared = directory
        .managed_children()
        .iter()
        .map(|child| (child.name(), child.kind()))
        .collect::<BTreeMap<_, _>>();
    if declared.len() != directory.managed_children().len() {
        return Err(recovery_blocked(
            journal_path,
            "created-directory managed-child plan contains a duplicate name",
        ));
    }

    let mut expected = BTreeMap::new();
    for (name, kind) in declared {
        let state =
            cohort_child(snapshot, observed, directory.logical_path(), name).ok_or_else(|| {
                recovery_blocked(
                    journal_path,
                    format!("managed child {name} is not represented by the transaction cohort"),
                )
            })?;
        let expected_kind = match kind {
            ManagedChildKindV2::File => RecoveryObjectKindV2::File,
            ManagedChildKindV2::Directory => RecoveryObjectKindV2::Directory,
        };
        match state.kind() {
            None => {}
            Some(actual_kind) if actual_kind == expected_kind => {
                expected.insert(name.to_owned(), state.clone());
            }
            Some(_) => {
                return Err(recovery_blocked(
                    journal_path,
                    format!("managed child {name} has the wrong filesystem kind"),
                ));
            }
        }
    }
    Ok(expected)
}

fn cohort_child<'a>(
    snapshot: &JournalSnapshotV2,
    observed: &'a ExactRecoveryWorld,
    parent: &str,
    name: &str,
) -> Option<&'a ExactRecoveryObject> {
    for entry in snapshot.entries() {
        if immediate_parent(entry.logical_path()) != parent {
            continue;
        }
        let key = if leaf_name(entry.logical_path()) == name {
            Some(RecoveryObjectKey::Target(entry.ordinal()))
        } else if entry.stage().name() == name {
            Some(RecoveryObjectKey::Stage(entry.ordinal()))
        } else if entry.backup().is_some_and(|backup| backup.name() == name) {
            Some(RecoveryObjectKey::Backup(entry.ordinal()))
        } else {
            None
        };
        if let Some(key) = key {
            return observed.object(key);
        }
    }
    for child in snapshot.directories() {
        if immediate_parent(child.logical_path()) != parent {
            continue;
        }
        let key = if leaf_name(child.logical_path()) == name {
            Some(RecoveryObjectKey::Directory(child.ordinal()))
        } else {
            None
        };
        if let Some(key) = key {
            return observed.object(key);
        }
    }
    None
}

fn classify_pending_placement(
    snapshot: &JournalSnapshotV2,
    observed: &ExactRecoveryWorld,
    intent: &PreparationPlacementIntentV2,
    journal_path: &Path,
) -> Result<RecoveryPreflightV2, CodegenError> {
    let mut allowed = exact_snapshot_world(snapshot);
    let (ordinal, artifact, world) = match intent {
        PreparationPlacementIntentV2::Directory(intent) => {
            let ordinal = intent.ordinal();
            let directory = snapshot
                .directories()
                .get(ordinal.get() as usize)
                .filter(|directory| directory.ordinal() == ordinal)
                .ok_or_else(|| {
                    recovery_blocked(journal_path, "directory placement ordinal is invalid")
                })?;
            let owner = observed_directory_presence(
                observed,
                RecoveryObjectKey::DirectoryOwner(ordinal),
                journal_path,
            )?;
            let target = observed_directory_presence(
                observed,
                RecoveryObjectKey::Directory(ordinal),
                journal_path,
            )?;
            let parent =
                observed_parent(snapshot, observed, directory.logical_path(), journal_path)?;
            let world = snapshot
                .validate_directory_publication_world(intent, &owner, &target, &parent)
                .map_err(|error| recovery_blocked(journal_path, error.reason()))?;
            allowed.objects.insert(
                RecoveryObjectKey::DirectoryOwner(ordinal),
                directory_presence(&owner),
            );
            allowed.objects.insert(
                RecoveryObjectKey::Directory(ordinal),
                directory_presence(&target),
            );
            (
                ordinal,
                RecoveryPreparationArtifactV2::Directory,
                match world {
                    DirectoryPublicationWorldV2::Before => MutationWorldV2::Before,
                    DirectoryPublicationWorldV2::After => MutationWorldV2::After,
                },
            )
        }
        PreparationPlacementIntentV2::File(intent) => {
            let ordinal = intent.ordinal();
            let entry = snapshot
                .entries()
                .get(ordinal.get() as usize)
                .filter(|entry| entry.ordinal() == ordinal)
                .ok_or_else(|| {
                    recovery_blocked(journal_path, "file placement ordinal is invalid")
                })?;
            let (owner_key, placed_key, artifact) = match intent.artifact() {
                FileArtifactKindV2::Stage => (
                    RecoveryObjectKey::StageOwner(ordinal),
                    RecoveryObjectKey::Stage(ordinal),
                    RecoveryPreparationArtifactV2::Stage,
                ),
                FileArtifactKindV2::Backup => (
                    RecoveryObjectKey::BackupOwner(ordinal),
                    RecoveryObjectKey::Backup(ordinal),
                    RecoveryPreparationArtifactV2::Backup,
                ),
            };
            let owner = observed_file_presence(observed, owner_key, journal_path)?;
            let placed = observed_file_presence(observed, placed_key, journal_path)?;
            let parent = observed_parent(snapshot, observed, entry.logical_path(), journal_path)?;
            let world = snapshot
                .validate_file_placement_world(intent, &owner, &placed, &parent)
                .map_err(|error| recovery_blocked(journal_path, error.reason()))?;
            allowed.objects.insert(owner_key, file_presence(&owner));
            allowed.objects.insert(placed_key, file_presence(&placed));
            (
                ordinal,
                artifact,
                match world {
                    PreparationPlacementWorldV2::Before => MutationWorldV2::Before,
                    PreparationPlacementWorldV2::After => MutationWorldV2::After,
                },
            )
        }
    };
    if !allowed.same_objects(observed) {
        return Err(recovery_blocked(
            journal_path,
            "artifact placement uncertainty is mixed with another object change",
        ));
    }
    Ok(RecoveryPreflightV2::PendingPlacement {
        ordinal,
        artifact,
        world,
    })
}

fn classify_forward_replacement(
    snapshot: &JournalSnapshotV2,
    observed: &ExactRecoveryWorld,
    journal_path: &Path,
) -> Result<RecoveryPreflightV2, CodegenError> {
    let index = match snapshot.phase() {
        JournalPhaseV2::Prepared => 0,
        JournalPhaseV2::Replacing { committed } => *committed as usize,
        _ => unreachable!("caller selected a replacement phase"),
    };
    let entry = snapshot.entries().get(index).ok_or_else(|| {
        recovery_blocked(
            journal_path,
            "replacement phase has no next cohort entry but lacks CommitComplete",
        )
    })?;
    let stage_before = entry.stage().current().as_present().ok_or_else(|| {
        recovery_blocked(journal_path, "next replacement has no exact prepared stage")
    })?;
    let target = observed
        .object(RecoveryObjectKey::Target(entry.ordinal()))
        .ok_or_else(|| recovery_blocked(journal_path, "target observation is missing"))?;
    let stage = observed
        .object(RecoveryObjectKey::Stage(entry.ordinal()))
        .ok_or_else(|| recovery_blocked(journal_path, "stage observation is missing"))?;

    let valid_after = match entry.action() {
        EntryActionV2::Create => {
            matches!((target, stage),
                (ExactRecoveryObject::File(target), ExactRecoveryObject::File(stage))
                if same_file_except_links(target, stage_before)
                    && same_file_except_links(stage, stage_before)
                    && target == stage
                    && target.link_count() == 2)
        }
        EntryActionV2::Replace => {
            matches!((target, stage),
                (ExactRecoveryObject::File(target), ExactRecoveryObject::Missing)
                if same_file_except_links(target, stage_before)
                    && target.link_count() == 1)
        }
    };
    if !valid_after {
        return Err(recovery_blocked(
            journal_path,
            "next target replacement is neither its exact before-world nor exact after-world",
        ));
    }

    let mut allowed = exact_snapshot_world(snapshot);
    allowed
        .objects
        .insert(RecoveryObjectKey::Target(entry.ordinal()), target.clone());
    allowed
        .objects
        .insert(RecoveryObjectKey::Stage(entry.ordinal()), stage.clone());
    if !allowed.same_objects(observed) {
        return Err(recovery_blocked(
            journal_path,
            "target replacement uncertainty is mixed with another object change",
        ));
    }
    Ok(RecoveryPreflightV2::ForwardReplacementCompleted {
        ordinal: entry.ordinal(),
    })
}

fn classify_pending_rollback(
    snapshot: &JournalSnapshotV2,
    observed: &ExactRecoveryWorld,
    intent: &RollbackIntentV2,
    journal_path: &Path,
) -> Result<RecoveryPreflightV2, CodegenError> {
    let mut after = exact_snapshot_world(snapshot);
    match intent {
        RollbackIntentV2::RemoveCreatedTarget {
            ordinal,
            expected_target,
        } => {
            after.objects.insert(
                RecoveryObjectKey::Target(*ordinal),
                ExactRecoveryObject::Missing,
            );
            after.objects.insert(
                RecoveryObjectKey::Stage(*ordinal),
                ExactRecoveryObject::File(file_with_link_count(expected_target, 1, journal_path)?),
            );
        }
        RollbackIntentV2::RestoreBackup {
            ordinal,
            expected_backup,
            ..
        } => {
            after.objects.insert(
                RecoveryObjectKey::Target(*ordinal),
                ExactRecoveryObject::File(expected_backup.clone()),
            );
            after.objects.insert(
                RecoveryObjectKey::Backup(*ordinal),
                ExactRecoveryObject::Missing,
            );
        }
    }
    require_exact_mutation_world(observed, &after, journal_path, "rollback mutation")?;
    Ok(RecoveryPreflightV2::PendingRollback {
        ordinal: intent.ordinal(),
        world: MutationWorldV2::After,
    })
}

fn classify_pending_cleanup(
    snapshot: &JournalSnapshotV2,
    observed: &ExactRecoveryWorld,
    intent: &CleanupIntentV2,
    journal_path: &Path,
) -> Result<RecoveryPreflightV2, CodegenError> {
    let mut after = exact_snapshot_world(snapshot);
    match intent {
        CleanupIntentV2::RemoveFile { target, expected } => {
            after
                .objects
                .insert(cleanup_key(*target), ExactRecoveryObject::Missing);
            if let CleanupTargetV2::PlacedStage { ordinal } = target
                && let Some(ExactRecoveryObject::File(current_target)) =
                    after.objects.get(&RecoveryObjectKey::Target(*ordinal))
                && current_target.identity() == expected.identity()
                && current_target.link_count() == 2
            {
                let target_after = file_with_link_count(current_target, 1, journal_path)?;
                after.objects.insert(
                    RecoveryObjectKey::Target(*ordinal),
                    ExactRecoveryObject::File(target_after),
                );
            }
        }
        CleanupIntentV2::RemoveDirectory { target, .. } => {
            after
                .objects
                .insert(cleanup_key(*target), ExactRecoveryObject::Missing);
        }
    }
    require_exact_mutation_world(observed, &after, journal_path, "cleanup mutation")?;
    Ok(RecoveryPreflightV2::PendingCleanup {
        target: intent.target(),
        world: MutationWorldV2::After,
    })
}

fn require_exact_mutation_world(
    observed: &ExactRecoveryWorld,
    after: &ExactRecoveryWorld,
    journal_path: &Path,
    label: &str,
) -> Result<(), CodegenError> {
    if observed.same_objects(after) {
        Ok(())
    } else {
        Err(recovery_blocked(
            journal_path,
            format!("{label} is neither its exact before-world nor exact after-world"),
        ))
    }
}

fn cleanup_key(target: CleanupTargetV2) -> RecoveryObjectKey {
    match target {
        CleanupTargetV2::OwnedStage { ordinal } => RecoveryObjectKey::StageOwner(ordinal),
        CleanupTargetV2::PlacedStage { ordinal } => RecoveryObjectKey::Stage(ordinal),
        CleanupTargetV2::OwnedBackup { ordinal } => RecoveryObjectKey::BackupOwner(ordinal),
        CleanupTargetV2::PlacedBackup { ordinal } => RecoveryObjectKey::Backup(ordinal),
        CleanupTargetV2::CreatedDirectory { ordinal } => RecoveryObjectKey::Directory(ordinal),
        CleanupTargetV2::OwnedDirectory { ordinal } => RecoveryObjectKey::DirectoryOwner(ordinal),
    }
}

fn observed_directory_presence(
    world: &ExactRecoveryWorld,
    key: RecoveryObjectKey,
    journal_path: &Path,
) -> Result<PresenceV2<ExactDirectoryStateV2>, CodegenError> {
    match world.object(key) {
        Some(ExactRecoveryObject::Missing) => Ok(PresenceV2::Missing),
        Some(ExactRecoveryObject::Directory(exact)) => Ok(PresenceV2::Present(exact.clone())),
        Some(ExactRecoveryObject::File(_) | ExactRecoveryObject::FileMetadata(_)) => {
            Err(recovery_blocked(
                journal_path,
                "directory recovery object was substituted with a regular file",
            ))
        }
        Some(ExactRecoveryObject::DirectoryMetadata(_)) => Err(recovery_blocked(
            journal_path,
            "directory recovery object was captured as residual metadata outside Create/Discard",
        )),
        None => Err(recovery_blocked(
            journal_path,
            "directory recovery object is absent from the complete capture",
        )),
    }
}

fn observed_file_presence(
    observed: &ExactRecoveryWorld,
    key: RecoveryObjectKey,
    journal_path: &Path,
) -> Result<PresenceV2<ExactFileStateV2>, CodegenError> {
    match observed.object(key) {
        Some(ExactRecoveryObject::Missing) => Ok(PresenceV2::Missing),
        Some(ExactRecoveryObject::File(exact)) => Ok(PresenceV2::Present(exact.clone())),
        Some(ExactRecoveryObject::Directory(_) | ExactRecoveryObject::DirectoryMetadata(_)) => {
            Err(recovery_blocked(
                journal_path,
                "file recovery object was substituted with a directory",
            ))
        }
        Some(ExactRecoveryObject::FileMetadata(_)) => Err(recovery_blocked(
            journal_path,
            "file recovery object was captured as residual metadata outside Create/Discard",
        )),
        None => Err(recovery_blocked(
            journal_path,
            "file recovery object is absent from the complete capture",
        )),
    }
}

fn observed_parent(
    snapshot: &JournalSnapshotV2,
    observed: &ExactRecoveryWorld,
    logical_path: &str,
    journal_path: &Path,
) -> Result<ExactDirectoryStateV2, CodegenError> {
    let Some((parent, _)) = logical_path.rsplit_once('/') else {
        return Ok(snapshot.project().root_current().clone());
    };
    let directory = snapshot
        .directories()
        .iter()
        .find(|directory| directory.logical_path() == parent)
        .ok_or_else(|| recovery_blocked(journal_path, "directory parent is outside the cohort"))?;
    match observed.object(RecoveryObjectKey::Directory(directory.ordinal())) {
        Some(ExactRecoveryObject::Directory(exact)) => Ok(exact.clone()),
        Some(ExactRecoveryObject::Missing) => Err(recovery_blocked(
            journal_path,
            "directory publication parent is missing",
        )),
        Some(ExactRecoveryObject::File(_) | ExactRecoveryObject::FileMetadata(_)) => {
            Err(recovery_blocked(
                journal_path,
                "directory publication parent was substituted with a regular file",
            ))
        }
        Some(ExactRecoveryObject::DirectoryMetadata(_)) => Err(recovery_blocked(
            journal_path,
            "directory publication parent cannot be a metadata-only owner residual",
        )),
        None => Err(recovery_blocked(
            journal_path,
            "directory publication parent is absent from the complete capture",
        )),
    }
}

fn same_file_except_links(left: &ExactFileStateV2, right: &ExactFileStateV2) -> bool {
    left.identity() == right.identity() && left.state() == right.state()
}

fn file_with_link_count(
    exact: &ExactFileStateV2,
    link_count: u64,
    journal_path: &Path,
) -> Result<ExactFileStateV2, CodegenError> {
    ExactFileStateV2::new(exact.identity(), exact.state().clone(), link_count)
        .map_err(|error| recovery_blocked(journal_path, error.reason()))
}

fn immediate_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn leaf_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Read-only result suitable for check, doctor, and dry-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecoveryAssessmentV2 {
    Stable {
        transaction_sequence: u64,
        phase: RecoveryPhaseActionV2,
        preflight: RecoveryPreflightV2,
        has_unpublished_complete_partial: bool,
    },
    ReconcileRecord {
        sequence: u64,
        action: RecordReconciliationActionV2,
    },
    BootstrapRollback,
}

pub(super) fn assess_loaded_recovery(
    load: &ActiveJournalLoad,
    observed: Option<&ExactRecoveryWorld>,
    journal_path: &Path,
) -> Result<RecoveryAssessmentV2, CodegenError> {
    match load {
        ActiveJournalLoad::ReconciliationRequired(reconciliation) => {
            Ok(RecoveryAssessmentV2::ReconcileRecord {
                sequence: reconciliation.sequence(),
                action: classify_record_reconciliation(reconciliation, journal_path)?,
            })
        }
        ActiveJournalLoad::Stable(loaded) => {
            let Some(snapshot) = loaded.latest() else {
                return Ok(RecoveryAssessmentV2::BootstrapRollback);
            };
            let observed = observed.ok_or_else(|| {
                recovery_blocked(
                    journal_path,
                    "stable journal recovery requires a complete exact cohort capture",
                )
            })?;
            Ok(RecoveryAssessmentV2::Stable {
                transaction_sequence: snapshot.sequence(),
                phase: classify_phase(snapshot.phase()),
                preflight: preflight_recovery_world(snapshot, observed, journal_path)?,
                has_unpublished_complete_partial: loaded.partial().is_some(),
            })
        }
    }
}

fn recovery_blocked(path: &Path, reason: impl Into<String>) -> CodegenError {
    CodegenError::RecoveryRequired {
        journal_path: path.to_path_buf(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::journal::{FileStateV2, ObjectIdentityV2, Sha256Digest};
    use super::*;

    fn exact_file(object: u64, links: u64) -> ExactFileStateV2 {
        ExactFileStateV2::new(
            ObjectIdentityV2::new(1, object),
            FileStateV2::new(
                Sha256Digest::parse(&format!("sha256:{object:064x}")).expect("valid test digest"),
                object,
                false,
                if cfg!(unix) { Some(0o644) } else { None },
            )
            .expect("valid test file state"),
            links,
        )
        .expect("valid exact test file")
    }

    fn world(
        objects: impl IntoIterator<Item = (RecoveryObjectKey, ExactRecoveryObject)>,
    ) -> ExactRecoveryWorld {
        ExactRecoveryWorld::from_complete_capture(objects.into_iter().collect(), BTreeMap::new())
    }

    #[test]
    fn every_precommit_phase_is_rollback_only_and_commit_complete_is_finish_only() {
        for phase in [
            JournalPhaseV2::Preparing {
                completed: 0,
                pending: None,
            },
            JournalPhaseV2::Prepared,
            JournalPhaseV2::Replacing { committed: 1 },
        ] {
            assert_eq!(classify_phase(&phase), RecoveryPhaseActionV2::BeginRollback);
        }
        assert_eq!(
            classify_phase(&JournalPhaseV2::RollingBack {
                next: 2,
                pending: None,
            }),
            RecoveryPhaseActionV2::ResumeRollback {
                next: 2,
                has_pending_intent: false,
            }
        );
        assert_eq!(
            classify_phase(&JournalPhaseV2::RollbackComplete {
                cleanup_completed: 3,
                pending: None,
            }),
            RecoveryPhaseActionV2::ResumeCleanup {
                outcome: RecoveryOutcomeV2::Rollback,
                completed: 3,
                has_pending_intent: false,
            }
        );
        assert_eq!(
            classify_phase(&JournalPhaseV2::CommitComplete {
                cleanup_completed: 4,
                pending: None,
            }),
            RecoveryPhaseActionV2::ResumeCleanup {
                outcome: RecoveryOutcomeV2::Commit,
                completed: 4,
                has_pending_intent: false,
            }
        );
    }

    #[test]
    fn exact_mutation_world_rejects_hybrid_third_and_incomplete_states() {
        let first = ArtifactOrdinal::new(0).expect("ordinal");
        let second = ArtifactOrdinal::new(1).expect("ordinal");
        let before_target = exact_file(10, 1);
        let desired = exact_file(20, 1);
        let unrelated = exact_file(30, 1);
        let third = exact_file(40, 1);

        let before = world([
            (
                RecoveryObjectKey::Target(first),
                ExactRecoveryObject::File(before_target),
            ),
            (
                RecoveryObjectKey::Stage(first),
                ExactRecoveryObject::File(desired.clone()),
            ),
            (
                RecoveryObjectKey::Target(second),
                ExactRecoveryObject::File(unrelated.clone()),
            ),
        ]);
        let after = world([
            (
                RecoveryObjectKey::Target(first),
                ExactRecoveryObject::File(desired),
            ),
            (
                RecoveryObjectKey::Stage(first),
                ExactRecoveryObject::Missing,
            ),
            (
                RecoveryObjectKey::Target(second),
                ExactRecoveryObject::File(unrelated),
            ),
        ]);
        require_exact_mutation_world(&after, &after, Path::new("journal"), "test")
            .expect("exact after-world is accepted");

        let mut hybrid = after.clone();
        hybrid.objects.insert(
            RecoveryObjectKey::Stage(first),
            before
                .object(RecoveryObjectKey::Stage(first))
                .expect("stage")
                .clone(),
        );
        assert!(
            require_exact_mutation_world(&hybrid, &after, Path::new("journal"), "test").is_err()
        );

        let mut third_state = after.clone();
        third_state.objects.insert(
            RecoveryObjectKey::Target(first),
            ExactRecoveryObject::File(third),
        );
        assert!(
            require_exact_mutation_world(&third_state, &after, Path::new("journal"), "test")
                .is_err()
        );

        let mut incomplete = after.clone();
        incomplete
            .objects
            .remove(&RecoveryObjectKey::Target(second));
        assert!(require_same_key_set(&before, &incomplete, Path::new("journal")).is_err());
    }

    #[test]
    fn exact_inventory_comparison_rejects_foreign_and_substituted_children() {
        let ordinal = ArtifactOrdinal::new(0).expect("ordinal");
        let stage = ExactRecoveryObject::File(exact_file(1, 1));
        let mut expected = BTreeMap::new();
        expected.insert("stage".to_owned(), stage.clone());
        let exact = expected.clone();
        assert_eq!(exact, expected);

        let mut foreign = exact.clone();
        foreign.insert("foreign".to_owned(), ExactRecoveryObject::Missing);
        assert_ne!(foreign, expected);

        let mut substituted = exact;
        substituted.insert(
            "stage".to_owned(),
            ExactRecoveryObject::File(exact_file(2, 1)),
        );
        assert_ne!(substituted, expected);

        let captured = world([(RecoveryObjectKey::Stage(ordinal), stage)]);
        assert!(captured.inventories().is_empty());
    }
}
