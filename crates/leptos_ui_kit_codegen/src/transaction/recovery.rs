use std::{io, path::Path};

use cap_fs_ext::MetadataExt;
use cap_std::fs::Metadata;

use crate::path_safety::{ObjectIdentity, PlanningContext};
use crate::{CodegenError, PreservedFileMode};

use super::engine::{
    BoundedRecoveryStep, RecoveryBarrierCertificate, derive_recovery_adoption_plan,
    recover_loaded_transaction_step,
};
use super::fs::{DirectoryEndpoint, ExactDirectoryObservation, HardLinkEndpoint};
use super::journal::{FinalizationOutcomeV2, TransactionId, canonical_root_hash};
use super::lock::{DEFAULT_KIT_WRITE_LOCK_PATH, KIT_ADVISORY_LOCK_CONTENT, WriteLock};
use super::namespace_lifecycle::{
    NamespaceRetirementStep, arm_retirement_authority, check_retirement_pending,
    recover_retirement_step,
};
use super::recovery_capture::capture_stable_recovery_world;
use super::recovery_policy::{
    RecordReconciliationActionV2, RecoveryAssessmentV2, RecoveryOutcomeV2, RecoveryPhaseActionV2,
    assess_loaded_recovery, classify_phase, classify_record_reconciliation,
};
use super::replace::check_pending_recovery_v1;
use super::runtime::{TransactionOutcome, TransactionRuntime};
use super::store::{
    ActiveJournalLoad, ActiveReconciliation, ActiveReconciliationDisposition,
    DiscoveredJournalNamespace, ExactRemovalDisposition, FinalizationCleanupStage,
    FinalizationPreparationDisposition, FinalizationWorld, JournalDiscoveryCapabilities,
    JournalNamespace, JournalRecoveryStore, JournalStoreError, JournalTopLevelNamespace,
    LoadedBootstrap, LoadedFinalization, MAX_RECORDS, WorkspaceRemovalDisposition, exact_directory,
};

const KIT_LOGICAL_PATH: &str = "src/components/ui/_kit/.transactions";
const KIT_PARENT_LOGICAL_PATH: &str = "src/components/ui/_kit";
const MAX_RECOVERY_MUTATION_PASSES: usize = MAX_RECORDS * 2 + 32;

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecoveryLoopStep {
    Complete,
    DurableProgress,
    BarrierCertified(Box<RecoveryPassCertificate>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecoveryPassCertificate {
    BootstrapFinalizationSlot {
        transaction_id: TransactionId,
        loaded: Box<LoadedBootstrap>,
    },
    Transaction(RecoveryBarrierCertificate),
    ActivePublication {
        transaction_id: TransactionId,
        reconciliation: ActiveReconciliation,
    },
    FinalizationPublication {
        transaction_id: TransactionId,
        loaded: Box<LoadedFinalization>,
    },
    FinalizationWorld {
        transaction_id: TransactionId,
        loaded: Box<LoadedFinalization>,
    },
}

#[derive(Debug)]
struct RecoveryProgressBudget {
    remaining: usize,
}

impl RecoveryProgressBudget {
    const fn new(remaining: usize) -> Self {
        Self { remaining }
    }

    fn consume_durable_progress(&mut self) -> Result<(), ()> {
        if self.remaining > 0 {
            self.remaining -= 1;
            Ok(())
        } else {
            Err(())
        }
    }
}

pub fn check_pending_recovery(project_root: &Path) -> Result<(), CodegenError> {
    let context = PlanningContext::open(project_root)?;
    let runtime = TransactionRuntime::system();
    check_retirement_pending(&context, &runtime)?;
    let kit_parent = match context.open_directory(KIT_PARENT_LOGICAL_PATH) {
        Ok(parent) => parent,
        Err(CodegenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return check_pending_recovery_v1(project_root);
        }
        Err(error) => return Err(error),
    };
    let kit = match context.open_directory(KIT_LOGICAL_PATH) {
        Ok(kit) => kit,
        Err(CodegenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return check_pending_recovery_v1(project_root);
        }
        Err(error) => return Err(error),
    };
    let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
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
        .observe_regular_file_bounded(
            &kit_parent,
            Path::new(".write.lock"),
            &lock_path,
            KIT_ADVISORY_LOCK_CONTENT.len() as u64,
        )
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

pub(crate) fn recover_pending_locked(
    context: &PlanningContext,
    lock: &WriteLock,
) -> Result<(), CodegenError> {
    let runtime = TransactionRuntime::system();
    recover_pending_locked_with_runtime(context, lock, &runtime)
}

fn recover_pending_locked_with_runtime(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
) -> Result<(), CodegenError> {
    lock.validate_context(context)?;
    let mut budget = RecoveryProgressBudget::new(MAX_RECOVERY_MUTATION_PASSES);
    let mut barrier_certificate = None;
    loop {
        let supplied_certificate = barrier_certificate.take();
        let step = recover_v2_step(context, lock, runtime, supplied_certificate.as_ref())?;
        match step {
            RecoveryLoopStep::Complete => {
                if supplied_certificate.is_some() {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: context.project_root().join(KIT_LOGICAL_PATH),
                        reason: "ephemeral recovery barrier certificate rediscovered an empty namespace instead of its exact pending slot"
                            .to_owned(),
                    });
                }
                break;
            }
            RecoveryLoopStep::DurableProgress => {
                budget.consume_durable_progress().map_err(|()| {
                    CodegenError::RecoveryRequired {
                        journal_path: context.project_root().join(KIT_LOGICAL_PATH),
                        reason: format!(
                            "journal-v2 recovery exceeded its bounded {MAX_RECOVERY_MUTATION_PASSES}-mutation progress budget"
                        ),
                    }
                })?;
            }
            RecoveryLoopStep::BarrierCertified(certificate) => {
                if supplied_certificate.is_some() {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: context.project_root().join(KIT_LOGICAL_PATH),
                        reason: "matching recovery barrier certificate did not publish its successor on the immediate rediscovery pass"
                            .to_owned(),
                    });
                }
                barrier_certificate = Some(*certificate);
            }
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
    barrier_certificate: Option<&RecoveryPassCertificate>,
) -> Result<RecoveryLoopStep, CodegenError> {
    match recover_retirement_step(context, lock, runtime)? {
        NamespaceRetirementStep::NotPresent => {}
        NamespaceRetirementStep::DurableProgress => {
            return Ok(RecoveryLoopStep::DurableProgress);
        }
    }
    let root = context.open_pinned_project_root()?;
    let root_metadata = root.dir_metadata().map_err(|source| CodegenError::Io {
        path: context.project_root().to_path_buf(),
        source,
    })?;
    let root_observation = directory_observation(&root_metadata);
    let kit_parent = context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
    let kit = match context.open_directory(KIT_LOGICAL_PATH) {
        Ok(kit) => kit,
        Err(CodegenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(RecoveryLoopStep::Complete);
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
            lock.identity(),
            lock_endpoint,
            kit_endpoint,
        ),
    )
    .map_err(store_error)?;
    let transaction = match discovered {
        DiscoveredJournalNamespace::Empty => return Ok(RecoveryLoopStep::Complete),
        DiscoveredJournalNamespace::Transaction(transaction) => transaction,
    };
    let workspace_path = transaction
        .workspace_path()
        .unwrap_or(&kit_path)
        .to_path_buf();
    let workspace = transaction.open_workspace().map_err(store_error)?;
    let store = transaction.bind(workspace.as_ref()).map_err(store_error)?;
    match store.inspect_namespace().map_err(store_error)? {
        // Discovery identified a transaction workspace while the subsequent
        // strict namespace load found no transaction authority. That pass has
        // made no durable progress, so fail closed instead of rediscovering the
        // same raced or attacker-controlled world forever.
        JournalNamespace::Empty => Err(CodegenError::RecoveryRequired {
            journal_path: workspace_path,
            reason: "journal-v2 namespace disappeared between discovery and strict loading; no recovery mutation was attempted"
                .to_owned(),
        }),
        JournalNamespace::Bootstrap(loaded) => {
            match barrier_certificate {
                None => {
                    lock.validate_context(context)?;
                    store
                        .certify_bootstrap_finalization_slot(&loaded)
                        .map_err(store_error)?;
                    return Ok(RecoveryLoopStep::BarrierCertified(Box::new(
                        RecoveryPassCertificate::BootstrapFinalizationSlot {
                            transaction_id: store.transaction_id().clone(),
                            loaded: Box::new(loaded),
                        },
                    )));
                }
                Some(RecoveryPassCertificate::BootstrapFinalizationSlot {
                    transaction_id,
                    loaded: certified,
                }) if transaction_id == store.transaction_id()
                    && certified.as_ref() == &loaded => {}
                Some(_) => {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: workspace_path,
                        reason: "bootstrap finalization-slot certificate does not bind the exact rediscovered authority"
                            .to_owned(),
                    });
                }
            }
            let lease = super::journal::FinalizationLeaseV2::arm_bootstrap_abort(
                loaded.bootstrap().clone(),
            )
            .map_err(|error| CodegenError::RecoveryRequired {
                journal_path: workspace_path.clone(),
                reason: error.to_string(),
            })?;
            lock.validate_context(context)?;
            require_finalization_prepared(
                store.prepare_finalization_publication(None, &lease),
                &workspace_path,
            )?;
            Ok(RecoveryLoopStep::DurableProgress)
        }
        JournalNamespace::Active(loaded) => {
            let transaction_certificate = match barrier_certificate {
                None => None,
                Some(RecoveryPassCertificate::Transaction(certificate)) => Some(certificate),
                Some(_) => {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: workspace_path,
                        reason: "recovery pass certificate does not bind the exact stable active publication world"
                            .to_owned(),
                    });
                }
            };
            recover_active_step(
                context,
                lock,
                runtime,
                &store,
                loaded,
                &workspace_path,
                transaction_certificate,
            )
        }
        JournalNamespace::ActiveReconciliation(reconciliation) => {
            let action = classify_record_reconciliation(&reconciliation, &workspace_path)?;
            match action {
                RecordReconciliationActionV2::ReloadPredecessor => {
                    reject_pass_certificate(
                        barrier_certificate,
                        &workspace_path,
                        "active predecessor reload",
                    )?;
                    match store.load_active().map_err(store_error)? {
                        ActiveJournalLoad::Stable(loaded) => recover_active_step(
                            context,
                            lock,
                            runtime,
                            &store,
                            *loaded,
                            &workspace_path,
                            None,
                        ),
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
                    reject_pass_certificate(
                        barrier_certificate,
                        &workspace_path,
                        "owned-partial removal",
                    )?;
                    lock.validate_context(context)?;
                    require_active_reconciled(
                        store.discard_active_partial(
                            &ActiveJournalLoad::ReconciliationRequired(Box::new(reconciliation)),
                            TransactionOutcome::Rollback,
                        ),
                        &workspace_path,
                    )?;
                    Ok(RecoveryLoopStep::DurableProgress)
                }
                RecordReconciliationActionV2::AdoptPublishedAndReload => {
                    match barrier_certificate {
                        None => {
                            lock.validate_context(context)?;
                            store
                                .certify_active_publication(&reconciliation)
                                .map_err(store_error)?;
                            Ok(RecoveryLoopStep::BarrierCertified(Box::new(
                                RecoveryPassCertificate::ActivePublication {
                                    transaction_id: store.transaction_id().clone(),
                                    reconciliation,
                                },
                            )))
                        }
                        Some(RecoveryPassCertificate::ActivePublication {
                            transaction_id,
                            reconciliation: certified,
                        }) if transaction_id == store.transaction_id()
                            && certified == &reconciliation =>
                        {
                            lock.validate_context(context)?;
                            require_active_reconciled(
                                store.retire_active_publication_partial(&reconciliation),
                                &workspace_path,
                            )?;
                            Ok(RecoveryLoopStep::DurableProgress)
                        }
                        Some(_) => Err(CodegenError::RecoveryRequired {
                            journal_path: workspace_path,
                            reason: "active-publication barrier certificate does not match the exact rediscovered transaction and linked aliases"
                                .to_owned(),
                        }),
                    }
                }
            }
        }
        JournalNamespace::Finalizing(loaded) => {
            lock.validate_context(context)?;
            recover_finalization_step(
                context,
                lock,
                &store,
                &loaded,
                kit_endpoint,
                &kit_path,
                barrier_certificate,
            )
        }
    }
}

fn recover_active_step(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    store: &JournalRecoveryStore<'_>,
    loaded: super::store::LoadedJournal,
    workspace_path: &Path,
    barrier_certificate: Option<&RecoveryBarrierCertificate>,
) -> Result<RecoveryLoopStep, CodegenError> {
    let load = ActiveJournalLoad::Stable(Box::new(loaded.clone()));
    let observed = loaded
        .latest()
        .map(|snapshot| capture_stable_recovery_world(context, runtime, snapshot, workspace_path))
        .transpose()?;
    let assessment = assess_loaded_recovery(&load, observed.as_ref(), workspace_path)?;

    if barrier_certificate.is_some() && !matches!(&assessment, RecoveryAssessmentV2::Stable { .. })
    {
        reject_transaction_certificate(
            barrier_certificate,
            workspace_path,
            "a different active recovery assessment",
        )?;
    }

    if loaded.partial().is_some() {
        reject_transaction_certificate(
            barrier_certificate,
            workspace_path,
            "active partial reconciliation",
        )?;
        let outcome = loaded
            .latest()
            .map_or(TransactionOutcome::Rollback, |snapshot| {
                recovery_outcome(classify_phase(snapshot.phase()))
            });
        lock.validate_context(context)?;
        require_active_reconciled(store.discard_active_partial(&load, outcome), workspace_path)?;
        return Ok(RecoveryLoopStep::DurableProgress);
    }

    let adoption_authority = match (&assessment, barrier_certificate) {
        (
            RecoveryAssessmentV2::Stable {
                preflight,
                has_unpublished_complete_partial: false,
                ..
            },
            None,
        ) => {
            let plan = derive_recovery_adoption_plan(&loaded, preflight, context)?;
            lock.validate_context(context)?;
            let authority = store
                .certify_active_recovery_authority(&loaded, plan.clone())
                .map_err(store_error)?;
            let ActiveJournalLoad::Stable(rediscovered) =
                store.load_active().map_err(store_error)?
            else {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: workspace_path.to_path_buf(),
                    reason:
                        "same-pass recovery adoption did not rediscover a stable active lineage"
                            .to_owned(),
                });
            };
            if rediscovered.as_ref() != &loaded {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: workspace_path.to_path_buf(),
                    reason: "stable active lineage changed across same-pass recovery adoption"
                        .to_owned(),
                });
            }
            let observed_after = rediscovered
                .latest()
                .map(|snapshot| {
                    capture_stable_recovery_world(context, runtime, snapshot, workspace_path)
                })
                .transpose()?;
            let reassessment = assess_loaded_recovery(
                &ActiveJournalLoad::Stable(rediscovered.clone()),
                observed_after.as_ref(),
                workspace_path,
            )?;
            require_same_recovery_assessment(&assessment, &reassessment, workspace_path)?;
            let RecoveryAssessmentV2::Stable {
                preflight: reassessed_preflight,
                ..
            } = &reassessment
            else {
                unreachable!("equal recovery assessments have the same variant");
            };
            let reassessed_plan =
                derive_recovery_adoption_plan(&rediscovered, reassessed_preflight, context)?;
            if reassessed_plan != plan {
                return Err(CodegenError::RecoveryRequired {
                    journal_path: workspace_path.to_path_buf(),
                    reason: "exact recovery action changed across journal-parent adoption sync"
                        .to_owned(),
                });
            }
            store
                .authorize_active_recovery_authority(&rediscovered, &authority, &reassessed_plan)
                .map_err(store_error)?;
            Some(authority)
        }
        _ => None,
    };

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
                adoption_authority,
                barrier_certificate,
            )? {
                BoundedRecoveryStep::Advanced => {}
                BoundedRecoveryStep::BarrierCertified(certificate) => {
                    return Ok(RecoveryLoopStep::BarrierCertified(Box::new(
                        RecoveryPassCertificate::Transaction(*certificate),
                    )));
                }
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
                    require_finalization_prepared(
                        store.prepare_finalization_publication(None, &lease),
                        workspace_path,
                    )?;
                }
            }
            Ok(RecoveryLoopStep::DurableProgress)
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

fn require_same_recovery_assessment(
    before: &RecoveryAssessmentV2,
    after: &RecoveryAssessmentV2,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    if before == after {
        Ok(())
    } else {
        Err(CodegenError::RecoveryRequired {
            journal_path: journal_path.to_path_buf(),
            reason: "exact recovery assessment changed across journal-parent adoption sync"
                .to_owned(),
        })
    }
}

fn reject_transaction_certificate(
    barrier_certificate: Option<&RecoveryBarrierCertificate>,
    journal_path: &Path,
    rediscovered_world: &str,
) -> Result<(), CodegenError> {
    if barrier_certificate.is_some() {
        return Err(CodegenError::RecoveryRequired {
            journal_path: journal_path.to_path_buf(),
            reason: format!(
                "ephemeral recovery barrier certificate rediscovered {rediscovered_world} instead of its exact active pending slot"
            ),
        });
    }
    Ok(())
}

fn reject_pass_certificate(
    barrier_certificate: Option<&RecoveryPassCertificate>,
    journal_path: &Path,
    rediscovered_world: &str,
) -> Result<(), CodegenError> {
    if barrier_certificate.is_some() {
        return Err(CodegenError::RecoveryRequired {
            journal_path: journal_path.to_path_buf(),
            reason: format!(
                "ephemeral recovery pass certificate rediscovered {rediscovered_world} instead of its exact certified world"
            ),
        });
    }
    Ok(())
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
        ActiveReconciliationDisposition::Durable { .. } => Ok(()),
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
    store: &JournalRecoveryStore<'_>,
    loaded: &LoadedFinalization,
    kit_endpoint: DirectoryEndpoint<'_>,
    kit_path: &Path,
    barrier_certificate: Option<&RecoveryPassCertificate>,
) -> Result<RecoveryLoopStep, CodegenError> {
    let outcome = loaded
        .latest()
        .or_else(|| loaded.partial())
        .map_or(TransactionOutcome::Rollback, |authority| {
            transaction_outcome(authority.lease().outcome())
        });
    let result = match loaded.reconciliation() {
        Some(FinalizationWorld::PreparedNext { .. }) => {
            return match barrier_certificate {
                None => {
                    store
                        .certify_finalization_world(loaded)
                        .map_err(store_error)?;
                    Ok(RecoveryLoopStep::BarrierCertified(Box::new(
                        RecoveryPassCertificate::FinalizationWorld {
                            transaction_id: store.transaction_id().clone(),
                            loaded: Box::new(loaded.clone()),
                        },
                    )))
                }
                Some(RecoveryPassCertificate::FinalizationWorld {
                    transaction_id,
                    loaded: certified,
                }) if transaction_id == store.transaction_id()
                    && certified.as_ref() == loaded => {
                    store
                        .link_finalization_publication(loaded)
                        .map_err(store_error)?;
                    Ok(RecoveryLoopStep::DurableProgress)
                }
                Some(_) => Err(CodegenError::RecoveryRequired {
                    journal_path: kit_path.to_path_buf(),
                    reason: "prepared-finalization certificate does not bind the exact rediscovered partial"
                        .to_owned(),
                }),
            };
        }
        Some(FinalizationWorld::LinkedAliases { .. }) => {
            let partial = loaded
                .partial()
                .ok_or_else(|| CodegenError::RecoveryRequired {
                    journal_path: kit_path.to_path_buf(),
                    reason: "linked finalization world has no exact partial alias".to_owned(),
                })?;
            return match barrier_certificate {
                None => {
                    store
                        .certify_finalization_publication(loaded)
                        .map_err(store_error)?;
                    Ok(RecoveryLoopStep::BarrierCertified(Box::new(
                        RecoveryPassCertificate::FinalizationPublication {
                            transaction_id: store.transaction_id().clone(),
                            loaded: Box::new(loaded.clone()),
                        },
                    )))
                }
                Some(RecoveryPassCertificate::FinalizationPublication {
                    transaction_id,
                    loaded: certified,
                }) if transaction_id == store.transaction_id()
                    && certified.as_ref() == loaded => {
                    require_removed(
                        store.remove_finalization_partial(partial, outcome),
                        kit_path,
                    )?;
                    Ok(RecoveryLoopStep::DurableProgress)
                }
                Some(_) => Err(CodegenError::RecoveryRequired {
                    journal_path: kit_path.to_path_buf(),
                    reason: "finalization publication certificate does not match the exact rediscovered transaction and linked aliases"
                        .to_owned(),
                }),
            };
        }
        Some(FinalizationWorld::Conflict { reason, .. }) => Err(CodegenError::RecoveryRequired {
            journal_path: kit_path.to_path_buf(),
            reason: reason.clone(),
        }),
        Some(FinalizationWorld::OwnedIncomplete { .. }) => {
            reject_pass_certificate(
                barrier_certificate,
                kit_path,
                "owned incomplete finalization partial",
            )?;
            let incomplete =
                loaded
                    .incomplete_partial()
                    .ok_or_else(|| CodegenError::RecoveryRequired {
                        journal_path: kit_path.to_path_buf(),
                        reason:
                            "owned incomplete finalization world has no exact removal authority"
                                .to_owned(),
                    })?;
            let incomplete_outcome = transaction_outcome(incomplete.outcome());
            require_removed(
                store.remove_incomplete_finalization_partial(incomplete, incomplete_outcome),
                kit_path,
            )
        }
        Some(FinalizationWorld::AdoptedPublished { stage }) => {
            match barrier_certificate {
                None => {
                    store
                        .certify_finalization_world(loaded)
                        .map_err(store_error)?;
                    return Ok(RecoveryLoopStep::BarrierCertified(Box::new(
                        RecoveryPassCertificate::FinalizationWorld {
                            transaction_id: store.transaction_id().clone(),
                            loaded: Box::new(loaded.clone()),
                        },
                    )));
                }
                Some(RecoveryPassCertificate::FinalizationWorld {
                    transaction_id,
                    loaded: certified,
                }) if transaction_id == store.transaction_id() && certified.as_ref() == loaded => {}
                Some(_) => {
                    return Err(CodegenError::RecoveryRequired {
                        journal_path: kit_path.to_path_buf(),
                        reason: "finalization cleanup certificate does not bind the exact rediscovered stage"
                            .to_owned(),
                    });
                }
            }
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
                        let observed = store
                            .runtime()
                            .fs()
                            .observe_directory(kit_endpoint)
                            .map_err(|source| CodegenError::RecoveryRequired {
                                journal_path: kit_path.to_path_buf(),
                                reason: format!("could not inspect finalization parent: {source}"),
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
                        require_finalization_prepared(
                            store.prepare_finalization_publication(Some(loaded), &closed),
                            kit_path,
                        )
                    } else if let Some(initial) = loaded.history().first()
                        && initial.lease().generation() != latest.lease().generation()
                    {
                        // Remove generation zero first and rediscover before
                        // retiring the closed tombstone. Each pass performs
                        // exactly one durable namespace mutation.
                        require_removed(
                            store.remove_finalization_record(initial, outcome),
                            kit_path,
                        )
                    } else {
                        arm_retirement_authority(context, lock, store.runtime(), latest)
                    }
                }
                FinalizationCleanupStage::RetiredPrefix => {
                    arm_retirement_authority(context, lock, store.runtime(), latest)
                }
            }
        }
        None => Err(CodegenError::RecoveryRequired {
            journal_path: kit_path.to_path_buf(),
            reason: "finalization namespace has no explicit publication or cleanup world"
                .to_owned(),
        }),
    };
    result?;
    Ok(RecoveryLoopStep::DurableProgress)
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
            Err(reconciliation_error(journal_path, reconciliation.world()))
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
            Err(reconciliation_error(journal_path, reconciliation.world()))
        }
    }
}

fn require_finalization_prepared(
    result: Result<FinalizationPreparationDisposition, JournalStoreError>,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    match result.map_err(store_error)? {
        FinalizationPreparationDisposition::Durable => Ok(()),
        FinalizationPreparationDisposition::ReconcileRequired { reconciliation } => {
            Err(CodegenError::RecoveryRequired {
                journal_path: journal_path.to_path_buf(),
                reason: format!(
                    "finalization generation {} {:?} preparation requires exact recovery after {:?} with {:?} durability: {} ({:?})",
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

fn reconciliation_error(journal_path: &Path, world: &super::store::RemovalWorld) -> CodegenError {
    CodegenError::RecoveryRequired {
        journal_path: journal_path.to_path_buf(),
        reason: format!(
            "exact cleanup requires another recovery pass: {} ({world:?})",
            world.description()
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

fn directory_observation(metadata: &Metadata) -> ExactDirectoryObservation {
    ExactDirectoryObservation {
        identity: ObjectIdentity::from_u64(MetadataExt::dev(metadata), MetadataExt::ino(metadata)),
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{RecoveryProgressBudget, require_same_recovery_assessment};
    use crate::transaction::journal::ArtifactOrdinal;
    use crate::transaction::recovery_policy::{
        MutationWorldV2, RecoveryAssessmentV2, RecoveryPhaseActionV2, RecoveryPreflightV2,
        RecoveryPreparationArtifactV2,
    };

    fn placement_assessment(world: MutationWorldV2, ordinal: u32) -> RecoveryAssessmentV2 {
        RecoveryAssessmentV2::Stable {
            transaction_sequence: 9,
            phase: RecoveryPhaseActionV2::BeginRollback,
            preflight: RecoveryPreflightV2::PendingPlacement {
                ordinal: ArtifactOrdinal::new(ordinal).unwrap(),
                artifact: RecoveryPreparationArtifactV2::Stage,
                world,
            },
            has_unpublished_complete_partial: false,
        }
    }

    #[test]
    fn recovery_progress_budget_fails_closed_instead_of_spinning() {
        let mut budget = RecoveryProgressBudget::new(1);
        assert_eq!(budget.consume_durable_progress(), Ok(()));
        assert_eq!(budget.consume_durable_progress(), Err(()));
    }

    #[test]
    fn completed_recovery_does_not_consume_the_progress_budget() {
        let budget = RecoveryProgressBudget::new(0);
        assert_eq!(budget.remaining, 0);
    }

    #[test]
    fn journal_adoption_rejects_before_to_after_world_drift() {
        let before = placement_assessment(MutationWorldV2::Before, 3);
        let after = placement_assessment(MutationWorldV2::After, 3);

        assert!(
            require_same_recovery_assessment(&before, &after, Path::new("journal-v2")).is_err()
        );
    }

    #[test]
    fn journal_adoption_rejects_equal_phase_counters_with_a_different_ordinal() {
        let expected = placement_assessment(MutationWorldV2::Before, 3);
        let substituted = placement_assessment(MutationWorldV2::Before, 4);

        assert!(
            require_same_recovery_assessment(&expected, &substituted, Path::new("journal-v2"))
                .is_err()
        );
        assert!(
            require_same_recovery_assessment(&expected, &expected, Path::new("journal-v2")).is_ok()
        );
    }
}
