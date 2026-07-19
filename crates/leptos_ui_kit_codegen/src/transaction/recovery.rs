#[cfg(test)]
use std::sync::Arc;
use std::{io, path::Path};

use cap_fs_ext::MetadataExt;
use cap_std::fs::Metadata;

use crate::path_safety::PlanningContext;
use crate::{CodegenError, PreservedFileMode};

use super::engine::{BoundedRecoveryStep, recover_loaded_transaction_step};
use super::fs::{
    DirectoryEndpoint, ExactDirectoryObservation, ExactObjectIdentity, ExactRelocationSource,
    FsOps, HardLinkEndpoint, SystemFs,
};
use super::journal::{
    FinalizationLeaseV2, FinalizationOutcomeV2, FinalizationStateV2, canonical_root_hash,
    parse_retirement_authority_name, parse_retirement_namespace_name, retirement_authority_name,
    retirement_namespace_name,
};
use super::lock::{DEFAULT_KIT_WRITE_LOCK_PATH, WriteLock};
use super::recovery_capture::capture_stable_recovery_world;
use super::recovery_policy::{
    RecordReconciliationActionV2, RecoveryAssessmentV2, RecoveryOutcomeV2, RecoveryPhaseActionV2,
    assess_loaded_recovery, classify_phase, classify_record_reconciliation,
};
use super::replace::check_pending_recovery_v1;
#[cfg(test)]
use super::runtime::{NoopTransitionObserver, SystemEntropy};
use super::runtime::{TransactionOutcome, TransactionRuntime};
use super::store::{
    ActiveJournalLoad, ActiveReconciliationDisposition, DiscoveredJournalNamespace,
    ExactRemovalDisposition, FinalizationCleanupStage, FinalizationDisposition, FinalizationRecord,
    FinalizationWorld, JournalDiscoveryCapabilities, JournalNamespace, JournalRecoveryStore,
    JournalStoreError, JournalTopLevelNamespace, LoadedFinalization, WorkspaceRemovalDisposition,
    exact_directory, exact_file,
};

use super::runtime::{TransitionKey, TransitionWindow};

const KIT_LOGICAL_PATH: &str = "src/components/ui/_kit/.transactions";
const KIT_PARENT_LOGICAL_PATH: &str = "src/components/ui/_kit";
const RETIREMENT_AUTHORITY_LIMIT: u64 = 16 * 1024 * 1024;

pub fn check_pending_recovery(project_root: &Path) -> Result<(), CodegenError> {
    let context = PlanningContext::open(project_root)?;
    let kit_parent = match context.open_directory(KIT_PARENT_LOGICAL_PATH) {
        Ok(parent) => parent,
        Err(CodegenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return check_pending_recovery_v1(project_root);
        }
        Err(error) => return Err(error),
    };
    check_retirement_alias_read_only(&context, &kit_parent)?;
    let kit = match context.open_directory(KIT_LOGICAL_PATH) {
        Ok(kit) => kit,
        Err(CodegenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return check_pending_recovery_v1(project_root);
        }
        Err(error) => return Err(error),
    };
    let runtime = TransactionRuntime::system();
    let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
    let coordination_grandparent = context.open_directory("src/components/ui")?;
    let coordination_parent_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    let coordination_observation = runtime
        .fs()
        .observe_directory(DirectoryEndpoint::new(
            &coordination_grandparent,
            Path::new("_kit"),
            &kit_parent,
            &coordination_parent_path,
        ))
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: coordination_parent_path,
            reason: format!("could not bind the read-only coordination parent: {source}"),
        })?;
    let kit_endpoint =
        DirectoryEndpoint::new(&kit_parent, Path::new(".transactions"), &kit, &kit_path);
    let namespace =
        JournalRecoveryStore::inspect_top_level(&runtime, kit_endpoint).map_err(store_error)?;
    let JournalTopLevelNamespace::Transaction(top_level) = namespace else {
        return check_pending_recovery_v1(project_root);
    };

    // Do not acquire the mutating coordination bootstrap on a check-only
    // surface. Bind strict discovery conservatively to a stable exact
    // observation of the persistent lock pathname instead. The store and
    // capture layers both perform stable double reads; every race is reported
    // as recovery-required and this path never mutates the filesystem.
    let root = context.open_pinned_project_root()?;
    let root_metadata = root.dir_metadata().map_err(|source| CodegenError::Io {
        path: context.project_root().to_path_buf(),
        source,
    })?;
    let lock_path = context.project_root().join(DEFAULT_KIT_WRITE_LOCK_PATH);
    let lock_observation = runtime
        .fs()
        .observe_regular_file(&kit_parent, Path::new(".write.lock"), &lock_path)
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: top_level.workspace_path().unwrap_or(&kit_path).to_path_buf(),
            reason: format!(
                "journal-v2 namespace exists but its persistent lock could not be bound read-only: {source}"
            ),
        })?;
    let discovered = JournalRecoveryStore::discover(
        &runtime,
        canonical_root_hash(&canonical_native_bytes(context.project_root())),
        JournalDiscoveryCapabilities::new(
            context.project_root(),
            directory_observation(&root_metadata),
            coordination_observation.identity,
            lock_observation.identity,
            HardLinkEndpoint::new(&kit_parent, Path::new(".write.lock"), &lock_path),
            kit_endpoint,
        ),
    )
    .map_err(store_error)?;
    let transaction = match discovered {
        DiscoveredJournalNamespace::Empty => {
            return Err(CodegenError::RecoveryRequired {
                journal_path: kit_path,
                reason: "journal-v2 namespace changed after stable read-only discovery".to_owned(),
            });
        }
        DiscoveredJournalNamespace::Transaction(transaction) => transaction,
    };
    if transaction.transaction_id() != top_level.transaction_id() {
        return Err(CodegenError::RecoveryRequired {
            journal_path: kit_path,
            reason: "journal-v2 transaction identity changed during read-only binding".to_owned(),
        });
    }
    let workspace_path = transaction
        .workspace_path()
        .unwrap_or(&kit_path)
        .to_path_buf();
    let workspace = transaction.open_workspace().map_err(store_error)?;
    let store = transaction.bind(workspace.as_ref()).map_err(store_error)?;
    let reason = match store.inspect_namespace().map_err(store_error)? {
        JournalNamespace::Empty => {
            "journal-v2 namespace disappeared during read-only classification".to_owned()
        }
        JournalNamespace::Bootstrap(_) => {
            "journal-v2 bootstrap requires bounded rollback finalization".to_owned()
        }
        JournalNamespace::Active(_) | JournalNamespace::ActiveReconciliation(_) => {
            let load = store.load_active().map_err(store_error)?;
            let observed = match &load {
                ActiveJournalLoad::Stable(loaded) => loaded
                    .latest()
                    .map(|snapshot| {
                        capture_stable_recovery_world(&context, &runtime, snapshot, &workspace_path)
                    })
                    .transpose()?,
                ActiveJournalLoad::ReconciliationRequired(_) => None,
            };
            let assessment = assess_loaded_recovery(&load, observed.as_ref(), &workspace_path)?;
            format!("journal-v2 requires bounded recovery: {assessment:?}")
        }
        JournalNamespace::Finalizing(loaded) => format!(
            "journal-v2 finalization requires bounded recovery from exact world {:?}",
            loaded.reconciliation()
        ),
    };
    Err(CodegenError::RecoveryRequired {
        journal_path: workspace_path,
        reason,
    })
}

fn check_retirement_alias_read_only(
    context: &PlanningContext,
    coordination_parent: &cap_std::fs::Dir,
) -> Result<(), CodegenError> {
    let coordination_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    for entry in coordination_parent
        .entries()
        .map_err(|source| CodegenError::Io {
            path: coordination_path.clone(),
            source,
        })?
    {
        let entry = entry.map_err(|source| CodegenError::Io {
            path: coordination_path.clone(),
            source,
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".transactions.retirement-v2-") {
            return Err(CodegenError::RecoveryRequired {
                journal_path: coordination_path.join(name.as_ref()),
                reason: "terminal-retirement authority requires bounded mutating recovery"
                    .to_owned(),
            });
        }
    }
    Ok(())
}

pub(crate) fn recover_pending_locked(
    context: &PlanningContext,
    lock: &WriteLock,
) -> Result<(), CodegenError> {
    let runtime = TransactionRuntime::system();
    recover_pending_locked_with_runtime(context, lock, &runtime)
}

#[cfg(test)]
pub(crate) fn recover_pending_locked_with_fs(
    context: &PlanningContext,
    lock: &WriteLock,
    fs: Arc<dyn FsOps>,
) -> Result<(), CodegenError> {
    let runtime = TransactionRuntime::new(
        fs,
        Arc::new(SystemEntropy),
        Arc::new(NoopTransitionObserver),
    );
    recover_pending_locked_with_runtime(context, lock, &runtime)
}

pub(crate) fn recover_pending_locked_with_runtime(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
) -> Result<(), CodegenError> {
    recover_pending_locked_with_runtime_inner(context, lock, runtime).map_err(|error| match error {
        CodegenError::RecoveryRequired { .. } => error,
        error => CodegenError::RecoveryRequired {
            journal_path: context
                .project_root()
                .join("src/components/ui/_kit/.transactions"),
            reason: format!(
                "exact journal-v2 recovery could not re-establish its durable authority: {error}"
            ),
        },
    })
}

fn recover_pending_locked_with_runtime_inner(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
) -> Result<(), CodegenError> {
    lock.validate_context(context)?;
    loop {
        if recover_v2_step(context, lock, runtime)? {
            break;
        }
    }
    // Journal-v1 predates exact cohort capture and immutable publication. It
    // remains discoverable for diagnostics, but journal-v2 recovery must
    // never mutate it.
    check_pending_recovery_v1(context.project_root())
}

fn recover_v2_step(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
) -> Result<bool, CodegenError> {
    let root = context.open_pinned_project_root()?;
    let root_metadata = root.dir_metadata().map_err(|source| CodegenError::Io {
        path: context.project_root().to_path_buf(),
        source,
    })?;
    let root_observation = directory_observation(&root_metadata);
    let kit_parent = context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
    if recover_retirement_alias_step(context, lock, runtime, &kit_parent)? {
        return Ok(false);
    }
    let kit = match context.open_directory(KIT_LOGICAL_PATH) {
        Ok(kit) => kit,
        Err(CodegenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(true);
        }
        Err(error) => return Err(error),
    };
    let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
    let kit_endpoint =
        DirectoryEndpoint::new(&kit_parent, Path::new(".transactions"), &kit, &kit_path);
    let lock_path = context.project_root().join(DEFAULT_KIT_WRITE_LOCK_PATH);
    let lock_endpoint = HardLinkEndpoint::new(&kit_parent, Path::new(".write.lock"), &lock_path);
    let discovered = JournalRecoveryStore::discover(
        runtime,
        canonical_root_hash(&canonical_native_bytes(context.project_root())),
        JournalDiscoveryCapabilities::new(
            context.project_root(),
            root_observation,
            lock.coordination_parent_identity(),
            lock.identity(),
            lock_endpoint,
            kit_endpoint,
        ),
    )
    .map_err(store_error)?;
    let transaction = match discovered {
        DiscoveredJournalNamespace::Empty => return Ok(true),
        DiscoveredJournalNamespace::Transaction(transaction) => transaction,
    };
    let workspace_path = transaction
        .workspace_path()
        .unwrap_or(&kit_path)
        .to_path_buf();
    let workspace = transaction.open_workspace().map_err(store_error)?;
    let store = transaction.bind(workspace.as_ref()).map_err(store_error)?;
    match store.inspect_namespace().map_err(store_error)? {
        // A transaction discovered one stable capture earlier disappeared
        // before strict namespace loading. Rediscover from the top rather than
        // treating this raced pass as globally quiescent.
        JournalNamespace::Empty => Ok(false),
        JournalNamespace::Bootstrap(loaded) => {
            let lease = super::journal::FinalizationLeaseV2::arm_bootstrap_abort(
                loaded.bootstrap().clone(),
            )
            .map_err(|error| CodegenError::RecoveryRequired {
                journal_path: workspace_path.clone(),
                reason: error.to_string(),
            })?;
            lock.validate_context(context)?;
            require_finalization_published(
                store.publish_finalization(None, &lease),
                &workspace_path,
            )?;
            Ok(false)
        }
        JournalNamespace::Active(loaded) => {
            recover_active_step(context, lock, runtime, &store, loaded, &workspace_path)
        }
        JournalNamespace::ActiveReconciliation(reconciliation) => {
            let action = classify_record_reconciliation(&reconciliation, &workspace_path)?;
            match action {
                RecordReconciliationActionV2::ReloadPredecessor => {
                    match store.load_active().map_err(store_error)? {
                        ActiveJournalLoad::Stable(_) => Ok(false),
                        ActiveJournalLoad::ReconciliationRequired(current) => {
                            Err(CodegenError::RecoveryRequired {
                                journal_path: workspace_path,
                                reason: format!(
                                    "journal sequence {} still requires a predecessor reload in exact world {:?}; no filesystem mutation was attempted",
                                    current.sequence(),
                                    current.world(),
                                ),
                            })
                        }
                    }
                }
                RecordReconciliationActionV2::RemoveOwnedPartial => {
                    lock.validate_context(context)?;
                    require_active_reconciled(
                        store.discard_active_partial(
                            &ActiveJournalLoad::ReconciliationRequired(reconciliation),
                            TransactionOutcome::Rollback,
                        ),
                        &workspace_path,
                    )?;
                    Ok(false)
                }
                RecordReconciliationActionV2::AdoptPublishedAndReload => {
                    lock.validate_context(context)?;
                    require_active_reconciled(
                        store.adopt_active_publication(&reconciliation),
                        &workspace_path,
                    )?;
                    Ok(false)
                }
            }
        }
        JournalNamespace::Finalizing(loaded) => {
            lock.validate_context(context)?;
            recover_finalization_step(
                context,
                lock,
                runtime,
                &store,
                &loaded,
                kit_endpoint,
                &kit_path,
            )?;
            Ok(false)
        }
    }
}

fn recover_retirement_alias_step(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    coordination_parent: &cap_std::fs::Dir,
) -> Result<bool, CodegenError> {
    let mut aliases = Vec::new();
    let mut retirement_namespaces = Vec::new();
    for entry in coordination_parent
        .entries()
        .map_err(|source| CodegenError::Io {
            path: context.project_root().join(KIT_PARENT_LOGICAL_PATH),
            source,
        })?
    {
        let entry = entry.map_err(|source| CodegenError::Io {
            path: context.project_root().join(KIT_PARENT_LOGICAL_PATH),
            source,
        })?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if text.starts_with(".transactions.retirement-v2-") && text.ends_with(".authority") {
            let transaction_id = parse_retirement_authority_name(&text).map_err(|error| {
                CodegenError::RecoveryRequired {
                    journal_path: context.project_root().join(KIT_PARENT_LOGICAL_PATH),
                    reason: error.to_string(),
                }
            })?;
            aliases.push((text.into_owned(), transaction_id));
        } else if text.starts_with(".transactions.retirement-v2-") && text.ends_with(".namespace") {
            let transaction_id = parse_retirement_namespace_name(&text).map_err(|error| {
                CodegenError::RecoveryRequired {
                    journal_path: context.project_root().join(KIT_PARENT_LOGICAL_PATH),
                    reason: error.to_string(),
                }
            })?;
            retirement_namespaces.push((text.into_owned(), transaction_id));
        }
    }
    if aliases.is_empty() {
        if !retirement_namespaces.is_empty() {
            return Err(CodegenError::RecoveryRequired {
                journal_path: context.project_root().join(KIT_PARENT_LOGICAL_PATH),
                reason: "a retiring transaction namespace exists without its exact authority"
                    .to_owned(),
            });
        }
        return Ok(false);
    }
    if aliases.len() != 1 || retirement_namespaces.len() > 1 {
        return Err(CodegenError::RecoveryRequired {
            journal_path: context.project_root().join(KIT_PARENT_LOGICAL_PATH),
            reason: "multiple terminal-retirement authorities are present".to_owned(),
        });
    }

    let (alias_name, transaction_id) = aliases.pop().expect("one alias");
    if retirement_namespaces
        .first()
        .is_some_and(|(_, retiring_id)| retiring_id != &transaction_id)
    {
        return Err(CodegenError::RecoveryRequired {
            journal_path: context.project_root().join(KIT_PARENT_LOGICAL_PATH),
            reason: "retirement authority and namespace name different transactions".to_owned(),
        });
    }
    let alias_path = context
        .project_root()
        .join(KIT_PARENT_LOGICAL_PATH)
        .join(&alias_name);
    let alias = runtime
        .fs()
        .read_regular_file_exact(
            coordination_parent,
            Path::new(&alias_name),
            &alias_path,
            RETIREMENT_AUTHORITY_LIMIT,
        )
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: alias_path.clone(),
            reason: format!("could not authenticate terminal-retirement authority: {source}"),
        })?;
    let lease = FinalizationLeaseV2::from_json_slice(&alias.bytes).map_err(|error| {
        CodegenError::RecoveryRequired {
            journal_path: alias_path.clone(),
            reason: error.to_string(),
        }
    })?;
    lock.validate_context(context)?;
    let root = context.open_pinned_project_root()?;
    let root_metadata = root
        .dir_metadata()
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: alias_path.clone(),
            reason: format!("could not authenticate terminal-retirement project root: {source}"),
        })?;
    let root_exact = exact_directory(&directory_observation(&root_metadata)).map_err(|error| {
        CodegenError::RecoveryRequired {
            journal_path: alias_path.clone(),
            reason: error.to_string(),
        }
    })?;
    let coordination_grandparent = context.open_directory("src/components/ui")?;
    let coordination_parent_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    let coordination_exact = exact_directory(
        &runtime
            .fs()
            .observe_directory(DirectoryEndpoint::new(
                &coordination_grandparent,
                Path::new("_kit"),
                coordination_parent,
                &coordination_parent_path,
            ))
            .map_err(|source| CodegenError::RecoveryRequired {
                journal_path: alias_path.clone(),
                reason: format!(
                    "could not authenticate terminal-retirement coordination parent: {source}"
                ),
            })?,
    )
    .map_err(|error| CodegenError::RecoveryRequired {
        journal_path: alias_path.clone(),
        reason: error.to_string(),
    })?;
    let lock_path = context.project_root().join(DEFAULT_KIT_WRITE_LOCK_PATH);
    let lock_exact = exact_file(
        &runtime
            .fs()
            .observe_regular_file(coordination_parent, Path::new(".write.lock"), &lock_path)
            .map_err(|source| CodegenError::RecoveryRequired {
                journal_path: alias_path.clone(),
                reason: format!("could not authenticate terminal-retirement write lock: {source}"),
            })?,
    )
    .map_err(|error| CodegenError::RecoveryRequired {
        journal_path: alias_path.clone(),
        reason: error.to_string(),
    })?;
    if lease
        .to_json_bytes()
        .map_err(|error| CodegenError::RecoveryRequired {
            journal_path: alias_path.clone(),
            reason: error.to_string(),
        })?
        != alias.bytes
        || lease.transaction_id() != &transaction_id
        || lease.state() != FinalizationStateV2::WorkspaceRemoved
        || lease.generation() != 1
        || lease.canonical_root_hash()
            != &canonical_root_hash(&canonical_native_bytes(context.project_root()))
        || lease.root() != &root_exact
        || lease.coordination_parent() != &coordination_exact
        || lease.write_lock() != &lock_exact
        || alias.observation.mode
            != (PreservedFileMode {
                readonly: false,
                posix_mode: private_posix_mode(),
            })
    {
        return Err(CodegenError::RecoveryRequired {
            journal_path: alias_path,
            reason: "terminal-retirement authority is not the canonical closed tombstone"
                .to_owned(),
        });
    }
    let outcome = transaction_outcome(lease.outcome());
    lock.validate_context(context)?;

    let transactions_path = context.project_root().join(KIT_LOGICAL_PATH);
    let retirement_name = retirement_namespace_name(&transaction_id);
    let retirement_path = context
        .project_root()
        .join(KIT_PARENT_LOGICAL_PATH)
        .join(&retirement_name);
    let canonical_present = match coordination_parent.symlink_metadata(".transactions") {
        Ok(_) => true,
        Err(source) if source.kind() == io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(CodegenError::Io {
                path: transactions_path.clone(),
                source,
            });
        }
    };
    let retirement_present = match coordination_parent.symlink_metadata(&retirement_name) {
        Ok(_) => true,
        Err(source) if source.kind() == io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(CodegenError::Io {
                path: retirement_path.clone(),
                source,
            });
        }
    };
    if canonical_present && retirement_present {
        return Err(CodegenError::RecoveryRequired {
            journal_path: transactions_path,
            reason: "canonical and retiring transaction namespaces are both present".to_owned(),
        });
    }

    if !canonical_present && !retirement_present {
        if alias.observation.link_count != Some(1) {
            return Err(CodegenError::RecoveryRequired {
                journal_path: alias_path,
                reason: "retirement authority has unexpected aliases after namespace removal"
                    .to_owned(),
            });
        }
        runtime.observe(TransitionKey::RemoveRetirementAuthority {
            outcome,
            window: TransitionWindow::Before,
        });
        runtime
            .fs()
            .remove_file_exact(
                coordination_parent,
                Path::new(&alias_name),
                &alias_path,
                &alias.observation,
            )
            .map_err(|source| CodegenError::FilesystemOperation {
                operation: "remove terminal retirement authority",
                logical_path: alias_name.clone(),
                path: alias_path.clone(),
                source,
            })?;
        runtime
            .fs()
            .sync_directory(coordination_parent, &alias_path)
            .map_err(|source| CodegenError::FilesystemOperation {
                operation: "sync coordination parent",
                logical_path: KIT_PARENT_LOGICAL_PATH.to_owned(),
                path: alias_path,
                source,
            })?;
        runtime.observe(TransitionKey::RemoveRetirementAuthority {
            outcome,
            window: TransitionWindow::After,
        });
        return Ok(true);
    }

    let (namespace_name, namespace_path, transactions) = if canonical_present {
        (
            ".transactions".to_owned(),
            transactions_path.clone(),
            context.open_directory(KIT_LOGICAL_PATH)?,
        )
    } else {
        let opened = runtime
            .fs()
            .open_directory_exact(
                coordination_parent,
                Path::new(&retirement_name),
                &retirement_path,
                0o700,
            )
            .map_err(|source| CodegenError::RecoveryRequired {
                journal_path: retirement_path.clone(),
                reason: format!(
                    "could not open the exact retiring transaction namespace: {source}"
                ),
            })?;
        (
            retirement_name.clone(),
            retirement_path.clone(),
            opened.directory,
        )
    };

    let endpoint = DirectoryEndpoint::new(
        coordination_parent,
        Path::new(&namespace_name),
        &transactions,
        &namespace_path,
    );
    let observation = runtime.fs().observe_directory(endpoint).map_err(|source| {
        CodegenError::RecoveryRequired {
            journal_path: namespace_path.clone(),
            reason: format!("could not authenticate terminal transaction namespace: {source}"),
        }
    })?;
    if exact_directory(&observation).map_err(|error| CodegenError::RecoveryRequired {
        journal_path: namespace_path.clone(),
        reason: error.to_string(),
    })? != *lease.workspace_parent_current()
    {
        return Err(CodegenError::RecoveryRequired {
            journal_path: namespace_path,
            reason: "terminal-retirement authority names a different transaction namespace"
                .to_owned(),
        });
    }
    let inventory = runtime
        .fs()
        .inventory_directory_exact(endpoint, &observation)
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: namespace_path.clone(),
            reason: format!("could not inventory terminal transaction namespace: {source}"),
        })?;
    let tombstone_name = lease.record_name();
    if inventory.entries.is_empty() {
        if canonical_present {
            return Err(CodegenError::RecoveryRequired {
                journal_path: namespace_path,
                reason: "canonical transaction namespace became empty before its authenticated retirement move"
                    .to_owned(),
            });
        }
        if alias.observation.link_count != Some(1) {
            return Err(CodegenError::RecoveryRequired {
                journal_path: alias_path,
                reason: "retirement authority link topology conflicts with an empty namespace"
                    .to_owned(),
            });
        }
        runtime.observe(TransitionKey::RetireTransactionNamespace {
            outcome,
            window: TransitionWindow::Before,
        });
        runtime
            .fs()
            .remove_empty_directory_exact(endpoint, &observation)
            .map_err(|source| CodegenError::FilesystemOperation {
                operation: "retire transaction namespace",
                logical_path: namespace_name.clone(),
                path: namespace_path.clone(),
                source,
            })?;
        runtime
            .fs()
            .sync_directory(coordination_parent, &transactions_path)
            .map_err(|source| CodegenError::FilesystemOperation {
                operation: "sync coordination parent",
                logical_path: KIT_PARENT_LOGICAL_PATH.to_owned(),
                path: namespace_path,
                source,
            })?;
        runtime.observe(TransitionKey::RetireTransactionNamespace {
            outcome,
            window: TransitionWindow::After,
        });
        return Ok(true);
    }
    if inventory.entries.len() != 1 || inventory.entries[0].name.to_string_lossy() != tombstone_name
    {
        return Err(CodegenError::RecoveryRequired {
            journal_path: namespace_path,
            reason: "terminal transaction namespace contains foreign retirement evidence"
                .to_owned(),
        });
    }
    let tombstone_path = namespace_path.join(&tombstone_name);
    let tombstone = runtime
        .fs()
        .read_regular_file_exact(
            &transactions,
            Path::new(&tombstone_name),
            &tombstone_path,
            RETIREMENT_AUTHORITY_LIMIT,
        )
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: tombstone_path.clone(),
            reason: format!("could not authenticate terminal tombstone alias: {source}"),
        })?;
    if tombstone.bytes != alias.bytes
        || tombstone.observation.identity != alias.observation.identity
        || tombstone.observation.link_count != Some(2)
        || alias.observation.link_count != Some(2)
    {
        return Err(CodegenError::RecoveryRequired {
            journal_path: tombstone_path,
            reason: "terminal tombstone and retirement authority are not exact hard-link aliases"
                .to_owned(),
        });
    }

    if canonical_present {
        runtime.observe(TransitionKey::MoveTransactionNamespaceToRetirement {
            outcome,
            window: TransitionWindow::Before,
        });
        runtime
            .fs()
            .relocate_noreplace(
                coordination_parent,
                Path::new(".transactions"),
                &transactions_path,
                coordination_parent,
                Path::new(&retirement_name),
                &retirement_path,
                &ExactRelocationSource::Directory(inventory),
            )
            .map_err(|error| CodegenError::FilesystemOperation {
                operation: "move transaction namespace to retirement",
                logical_path: KIT_LOGICAL_PATH.to_owned(),
                path: retirement_path.clone(),
                source: error.into_io(),
            })?;
        runtime
            .fs()
            .sync_directory(coordination_parent, &retirement_path)
            .map_err(|source| CodegenError::FilesystemOperation {
                operation: "sync coordination parent",
                logical_path: KIT_PARENT_LOGICAL_PATH.to_owned(),
                path: retirement_path,
                source,
            })?;
        runtime.observe(TransitionKey::MoveTransactionNamespaceToRetirement {
            outcome,
            window: TransitionWindow::After,
        });
        return Ok(true);
    }

    runtime.observe(TransitionKey::RemoveFinalizationLease {
        outcome,
        generation: lease.generation(),
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .remove_file_exact(
            &transactions,
            Path::new(&tombstone_name),
            &tombstone_path,
            &tombstone.observation,
        )
        .map_err(|source| CodegenError::FilesystemOperation {
            operation: "remove terminal finalization lease",
            logical_path: tombstone_name,
            path: tombstone_path.clone(),
            source,
        })?;
    let after = runtime.fs().observe_directory(endpoint).map_err(|source| {
        CodegenError::RecoveryRequired {
            journal_path: namespace_path.clone(),
            reason: format!("could not reobserve terminal transaction namespace: {source}"),
        }
    })?;
    runtime
        .fs()
        .sync_parent(endpoint, &after, super::fs::ParentSyncKind::Journal)
        .map_err(|source| CodegenError::FilesystemOperation {
            operation: "sync transaction namespace",
            logical_path: namespace_name,
            path: namespace_path,
            source,
        })?;
    runtime.observe(TransitionKey::RemoveFinalizationLease {
        outcome,
        generation: lease.generation(),
        window: TransitionWindow::After,
    });
    Ok(true)
}

fn recover_active_step(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    store: &JournalRecoveryStore<'_>,
    loaded: super::store::LoadedJournal,
    workspace_path: &Path,
) -> Result<bool, CodegenError> {
    if let Some(snapshot) = loaded.latest() {
        let identity = snapshot.project().coordination_parent().identity();
        if ExactObjectIdentity::from_parts(identity.namespace_bytes(), identity.object_bytes())
            != lock.coordination_parent_identity()
        {
            return Err(CodegenError::RecoveryRequired {
                journal_path: workspace_path.to_path_buf(),
                reason: "journal-v2 snapshot is not bound to the held coordination parent"
                    .to_owned(),
            });
        }
    }
    let load = ActiveJournalLoad::Stable(loaded.clone());
    let observed = loaded
        .latest()
        .map(|snapshot| capture_stable_recovery_world(context, runtime, snapshot, workspace_path))
        .transpose()?;
    let assessment = assess_loaded_recovery(&load, observed.as_ref(), workspace_path)?;

    if loaded.partial().is_some() {
        let outcome = loaded
            .latest()
            .map_or(TransactionOutcome::Rollback, |snapshot| {
                recovery_outcome(classify_phase(snapshot.phase()))
            });
        lock.validate_context(context)?;
        require_active_reconciled(store.discard_active_partial(&load, outcome), workspace_path)?;
        return Ok(false);
    }

    match assessment {
        RecoveryAssessmentV2::Stable {
            preflight,
            has_unpublished_complete_partial: false,
            ..
        } => {
            lock.validate_context(context)?;
            match recover_loaded_transaction_step(
                context,
                lock,
                runtime.clone(),
                &loaded,
                preflight,
            )? {
                BoundedRecoveryStep::Advanced => {}
                BoundedRecoveryStep::ReadyForFinalization(outcome) => {
                    let latest = loaded.latest().ok_or_else(|| CodegenError::RecoveryRequired {
                        journal_path: workspace_path.to_path_buf(),
                        reason: "terminal recovery has no immutable journal authority".to_owned(),
                    })?;
                    let lease = super::journal::FinalizationLeaseV2::arm(
                        latest,
                        loaded.records().to_vec(),
                        None,
                    )
                    .map_err(|error| CodegenError::RecoveryRequired {
                        journal_path: workspace_path.to_path_buf(),
                        reason: error.to_string(),
                    })?;
                    if transaction_outcome(lease.outcome()) != outcome {
                        return Err(CodegenError::RecoveryRequired {
                            journal_path: workspace_path.to_path_buf(),
                            reason: "terminal recovery outcome disagrees with its immutable finalization authority"
                                .to_owned(),
                        });
                    }
                    lock.validate_context(context)?;
                    require_finalization_published(
                        store.publish_finalization(None, &lease),
                        workspace_path,
                    )?;
                }
            }
            Ok(false)
        }
        RecoveryAssessmentV2::Stable {
            has_unpublished_complete_partial: true,
            ..
        } => Err(CodegenError::RecoveryRequired {
            journal_path: workspace_path.to_path_buf(),
            reason: "active journal retained an unpublished complete partial after exact discard classification"
                .to_owned(),
        }),
        RecoveryAssessmentV2::BootstrapRollback => Err(CodegenError::RecoveryRequired {
            journal_path: workspace_path.to_path_buf(),
            reason: "active journal has neither a durable snapshot nor an exact discardable partial"
                .to_owned(),
        }),
        RecoveryAssessmentV2::ReconcileRecord { sequence, action } => {
            Err(CodegenError::RecoveryRequired {
                journal_path: workspace_path.to_path_buf(),
                reason: format!(
                    "stable active journal unexpectedly reclassified sequence {sequence} as record reconciliation {action:?}"
                ),
            })
        }
    }
}

const fn recovery_outcome(action: RecoveryPhaseActionV2) -> TransactionOutcome {
    match action {
        RecoveryPhaseActionV2::ResumeCleanup {
            outcome: RecoveryOutcomeV2::Commit,
            ..
        } => TransactionOutcome::Commit,
        RecoveryPhaseActionV2::BeginRollback
        | RecoveryPhaseActionV2::ResumeRollback { .. }
        | RecoveryPhaseActionV2::ResumeCleanup {
            outcome: RecoveryOutcomeV2::Rollback,
            ..
        } => TransactionOutcome::Rollback,
    }
}

fn require_active_reconciled(
    result: Result<ActiveReconciliationDisposition, JournalStoreError>,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    match result.map_err(store_error)? {
        ActiveReconciliationDisposition::Durable => Ok(()),
        ActiveReconciliationDisposition::ReconcileRequired { reconciliation } => {
            Err(CodegenError::RecoveryRequired {
                journal_path: journal_path.to_path_buf(),
                reason: format!(
                    "journal sequence {} requires another exact {:?} pass at {:?} with {:?} durability: {} ({:?})",
                    reconciliation.sequence(),
                    reconciliation.action(),
                    reconciliation.mutation(),
                    reconciliation.durability(),
                    reconciliation.source(),
                    reconciliation.world(),
                ),
            })
        }
    }
}

fn recover_finalization_step(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    store: &JournalRecoveryStore<'_>,
    loaded: &LoadedFinalization,
    kit_endpoint: DirectoryEndpoint<'_>,
    kit_path: &Path,
) -> Result<(), CodegenError> {
    let outcome_authority = loaded
        .latest()
        .or_else(|| loaded.partial())
        .ok_or_else(|| CodegenError::RecoveryRequired {
            journal_path: kit_path.to_path_buf(),
            reason: "finalization namespace has neither published nor prepared exact authority"
                .to_owned(),
        })?;
    lock.validate_context(context)?;
    let lease = outcome_authority.lease();
    let coordination_identity = lease.coordination_parent().identity();
    let write_lock_identity = lease.write_lock().identity();
    let root = context.open_pinned_project_root()?;
    let root_metadata = root
        .dir_metadata()
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: kit_path.to_path_buf(),
            reason: format!("could not authenticate finalization project root: {source}"),
        })?;
    let root_exact = exact_directory(&directory_observation(&root_metadata)).map_err(|error| {
        CodegenError::RecoveryRequired {
            journal_path: kit_path.to_path_buf(),
            reason: error.to_string(),
        }
    })?;
    if ExactObjectIdentity::from_parts(
        coordination_identity.namespace_bytes(),
        coordination_identity.object_bytes(),
    ) != lock.coordination_parent_identity()
        || ExactObjectIdentity::from_parts(
            write_lock_identity.namespace_bytes(),
            write_lock_identity.object_bytes(),
        ) != lock.identity()
        || lease.root() != &root_exact
    {
        return Err(CodegenError::RecoveryRequired {
            journal_path: kit_path.to_path_buf(),
            reason: "finalization authority is not bound to the live root, coordination parent, and held write lock"
                .to_owned(),
        });
    }
    let outcome = transaction_outcome(outcome_authority.lease().outcome());
    match loaded.reconciliation() {
        Some(FinalizationWorld::PreparedNext { .. }) => {
            let partial = loaded
                .partial()
                .ok_or_else(|| CodegenError::RecoveryRequired {
                    journal_path: kit_path.to_path_buf(),
                    reason: "prepared finalization world has no exact partial".to_owned(),
                })?;
            require_finalization_published(
                store.publish_finalization(Some(loaded), partial.lease()),
                kit_path,
            )
        }
        Some(FinalizationWorld::LinkedAliases { .. }) => {
            let partial = loaded
                .partial()
                .ok_or_else(|| CodegenError::RecoveryRequired {
                    journal_path: kit_path.to_path_buf(),
                    reason: "linked finalization world has no exact partial alias".to_owned(),
                })?;
            require_removed(
                store.remove_finalization_partial(partial, outcome),
                kit_path,
            )
        }
        Some(FinalizationWorld::Conflict { reason, .. }) => Err(CodegenError::RecoveryRequired {
            journal_path: kit_path.to_path_buf(),
            reason: reason.clone(),
        }),
        Some(FinalizationWorld::CleanupProgress { stage }) => {
            let latest = loaded
                .latest()
                .ok_or_else(|| CodegenError::RecoveryRequired {
                    journal_path: kit_path.to_path_buf(),
                    reason: "finalization cleanup has no published manifest authority".to_owned(),
                })?;
            match stage {
                FinalizationCleanupStage::CompleteManifest => require_removed(
                    store.remove_bootstrap_intent(latest.lease(), outcome),
                    kit_path,
                ),
                FinalizationCleanupStage::IntentRemoved => require_removed(
                    store.remove_bootstrap_owner(latest.lease(), outcome),
                    kit_path,
                ),
                FinalizationCleanupStage::OwnershipRemoved => {
                    if let Some(partial) = latest.lease().partial() {
                        require_removed(store.remove_journal_partial(partial, outcome), kit_path)
                    } else if let Some(record) = latest.lease().records().last() {
                        require_removed(store.remove_journal_record(record, outcome), kit_path)
                    } else {
                        remove_workspace(store, latest.lease(), outcome, kit_path)
                    }
                }
                FinalizationCleanupStage::PartialRemoved => {
                    let record = latest.lease().records().last().ok_or_else(|| {
                        CodegenError::RecoveryRequired {
                            journal_path: kit_path.to_path_buf(),
                            reason: "finalization history stage has no manifest record".to_owned(),
                        }
                    })?;
                    require_removed(store.remove_journal_record(record, outcome), kit_path)
                }
                FinalizationCleanupStage::HistoryRemoving { remaining_records } => {
                    let record_index = remaining_records.checked_sub(1).ok_or_else(|| {
                        CodegenError::RecoveryRequired {
                            journal_path: kit_path.to_path_buf(),
                            reason: "finalization history-removal stage has a zero remaining count"
                                .to_owned(),
                        }
                    })?;
                    let record = latest.lease().records().get(record_index).ok_or_else(|| {
                        CodegenError::RecoveryRequired {
                            journal_path: kit_path.to_path_buf(),
                            reason: "remaining finalization history exceeds its exact manifest"
                                .to_owned(),
                        }
                    })?;
                    require_removed(store.remove_journal_record(record, outcome), kit_path)
                }
                FinalizationCleanupStage::WorkspaceEmpty => {
                    remove_workspace(store, latest.lease(), outcome, kit_path)
                }
                FinalizationCleanupStage::WorkspaceRemoved => {
                    if latest.lease().workspace().as_present().is_some() {
                        let observed =
                            SystemFs.observe_directory(kit_endpoint).map_err(|source| {
                                CodegenError::RecoveryRequired {
                                    journal_path: kit_path.to_path_buf(),
                                    reason: format!(
                                        "could not inspect finalization parent: {source}"
                                    ),
                                }
                            })?;
                        let closed = latest
                            .lease()
                            .mark_workspace_removed(exact_directory(&observed).map_err(
                                |error| CodegenError::RecoveryRequired {
                                    journal_path: kit_path.to_path_buf(),
                                    reason: error.to_string(),
                                },
                            )?)
                            .map_err(|error| CodegenError::RecoveryRequired {
                                journal_path: kit_path.to_path_buf(),
                                reason: error.to_string(),
                            })?;
                        require_finalization_published(
                            store.publish_finalization(Some(loaded), &closed),
                            kit_path,
                        )
                    } else {
                        if let Some(initial) = loaded.history().first()
                            && initial.lease().generation() != latest.lease().generation()
                        {
                            return require_removed(
                                store.remove_finalization_record(initial, outcome),
                                kit_path,
                            );
                        }
                        arm_retirement_authority(
                            context,
                            lock,
                            runtime,
                            latest,
                            kit_endpoint,
                            kit_path,
                            outcome,
                        )
                    }
                }
            }
        }
        None => {
            let latest = loaded
                .latest()
                .ok_or_else(|| CodegenError::RecoveryRequired {
                    journal_path: kit_path.to_path_buf(),
                    reason: "finalization cleanup has no published manifest authority".to_owned(),
                })?;
            require_removed(
                store.remove_bootstrap_intent(latest.lease(), outcome),
                kit_path,
            )
        }
    }
}

fn arm_retirement_authority(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    terminal: &FinalizationRecord,
    transactions: DirectoryEndpoint<'_>,
    transactions_path: &Path,
    outcome: TransactionOutcome,
) -> Result<(), CodegenError> {
    if terminal.lease().state() != FinalizationStateV2::WorkspaceRemoved {
        return Err(CodegenError::RecoveryRequired {
            journal_path: transactions_path.to_path_buf(),
            reason: "retirement authority requires the closed workspace-removed tombstone"
                .to_owned(),
        });
    }
    let alias_name = retirement_authority_name(terminal.lease().transaction_id());
    let alias_path = context
        .project_root()
        .join(KIT_PARENT_LOGICAL_PATH)
        .join(&alias_name);
    let tombstone_path = transactions_path.join(terminal.name());
    let source = runtime
        .fs()
        .observe_regular_file(
            transactions.directory,
            Path::new(terminal.name()),
            &tombstone_path,
        )
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: tombstone_path.clone(),
            reason: format!("could not authenticate terminal tombstone: {source}"),
        })?;
    if exact_file(&source).map_err(|error| CodegenError::RecoveryRequired {
        journal_path: tombstone_path.clone(),
        reason: error.to_string(),
    })? != *terminal.exact()
    {
        return Err(CodegenError::RecoveryRequired {
            journal_path: tombstone_path,
            reason: "terminal tombstone changed before retirement authority publication".to_owned(),
        });
    }
    lock.validate_context(context)?;
    runtime.observe(TransitionKey::ArmRetirementAuthority {
        outcome,
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .hard_link(
            &[],
            HardLinkEndpoint::new(
                transactions.directory,
                Path::new(terminal.name()),
                &tombstone_path,
            ),
            HardLinkEndpoint::new(transactions.parent, Path::new(&alias_name), &alias_path),
        )
        .map_err(|source| CodegenError::FilesystemOperation {
            operation: "arm terminal retirement authority",
            logical_path: alias_name.clone(),
            path: alias_path.clone(),
            source,
        })?;
    let alias = runtime
        .fs()
        .read_regular_file_exact(
            transactions.parent,
            Path::new(&alias_name),
            &alias_path,
            RETIREMENT_AUTHORITY_LIMIT,
        )
        .map_err(|source| CodegenError::RecoveryRequired {
            journal_path: alias_path.clone(),
            reason: format!("could not verify terminal retirement authority: {source}"),
        })?;
    if alias.observation.identity != source.identity || alias.observation.link_count != Some(2) {
        return Err(CodegenError::RecoveryRequired {
            journal_path: alias_path,
            reason: "terminal retirement authority is not the exact tombstone hard link".to_owned(),
        });
    }
    runtime
        .fs()
        .sync_directory(transactions.parent, &alias_path)
        .map_err(|source| CodegenError::FilesystemOperation {
            operation: "sync coordination parent",
            logical_path: KIT_PARENT_LOGICAL_PATH.to_owned(),
            path: alias_path,
            source,
        })?;
    runtime.observe(TransitionKey::ArmRetirementAuthority {
        outcome,
        window: TransitionWindow::After,
    });
    Ok(())
}

fn remove_workspace(
    store: &JournalRecoveryStore<'_>,
    lease: &super::journal::FinalizationLeaseV2,
    outcome: TransactionOutcome,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    let expected =
        lease
            .workspace()
            .as_present()
            .ok_or_else(|| CodegenError::RecoveryRequired {
                journal_path: journal_path.to_path_buf(),
                reason: "workspace cleanup has no exact workspace authority".to_owned(),
            })?;
    match store
        .remove_workspace(expected, outcome)
        .map_err(store_error)?
    {
        WorkspaceRemovalDisposition::Durable { .. } => Ok(()),
        WorkspaceRemovalDisposition::ReconcileRequired(reconciliation) => {
            Err(reconciliation_error(journal_path, &reconciliation))
        }
    }
}

fn require_removed(
    result: Result<ExactRemovalDisposition, JournalStoreError>,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    match result.map_err(store_error)? {
        ExactRemovalDisposition::DurableAbsent => Ok(()),
        ExactRemovalDisposition::ReconcileRequired(reconciliation) => {
            Err(reconciliation_error(journal_path, &reconciliation))
        }
    }
}

fn require_finalization_published(
    result: Result<FinalizationDisposition, JournalStoreError>,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    match result.map_err(store_error)? {
        FinalizationDisposition::Durable { .. } => Ok(()),
        FinalizationDisposition::DurableResidual { reconciliation }
        | FinalizationDisposition::ReconcileRequired { reconciliation } => {
            Err(CodegenError::RecoveryRequired {
                journal_path: journal_path.to_path_buf(),
                reason: format!(
                    "finalization generation {} ({:?}) requires another {:?} pass with {:?} durability: {} ({:?})",
                    reconciliation.generation(),
                    reconciliation.outcome(),
                    reconciliation.mutation(),
                    reconciliation.durability(),
                    reconciliation.source(),
                    reconciliation.world(),
                ),
            })
        }
    }
}

fn reconciliation_error(
    journal_path: &Path,
    reconciliation: &super::store::RemovalReconciliation,
) -> CodegenError {
    CodegenError::RecoveryRequired {
        journal_path: journal_path.to_path_buf(),
        reason: format!(
            "exact {:?} cleanup for {:?} requires another {:?} pass: {} ({})",
            reconciliation.outcome(),
            reconciliation.object(),
            reconciliation.mutation(),
            reconciliation.world().description(),
            reconciliation.source(),
        ),
    }
}

fn store_error(error: JournalStoreError) -> CodegenError {
    CodegenError::RecoveryRequired {
        journal_path: error.path().to_path_buf(),
        reason: error.reason(),
    }
}

fn transaction_outcome(outcome: FinalizationOutcomeV2) -> TransactionOutcome {
    match outcome {
        FinalizationOutcomeV2::Commit => TransactionOutcome::Commit,
        FinalizationOutcomeV2::Rollback => TransactionOutcome::Rollback,
    }
}

#[cfg(unix)]
const fn private_posix_mode() -> Option<u32> {
    Some(0o600)
}

#[cfg(not(unix))]
const fn private_posix_mode() -> Option<u32> {
    None
}

fn directory_observation(metadata: &Metadata) -> ExactDirectoryObservation {
    ExactDirectoryObservation {
        identity: super::fs::ExactObjectIdentity::from_unix(
            MetadataExt::dev(metadata),
            MetadataExt::ino(metadata),
        ),
        mode: preserved_mode(metadata),
        link_count: Some(MetadataExt::nlink(metadata)),
    }
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
