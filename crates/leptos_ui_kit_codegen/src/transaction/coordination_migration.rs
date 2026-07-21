//! Exact migration of the former two-line coordination ignore contract.
//!
//! Migration is independently identified and completes before ordinary
//! transaction recovery. The held write lock remains externally aliased until
//! the desired ignore file is durable and every contained migration artifact
//! has been retired.

use std::path::{Path, PathBuf};

use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};

use crate::CodegenError;
use crate::path_safety::{ObjectIdentity, PlanningContext};

use super::fs::{
    DirectoryEndpoint, ExactDirectoryEntryKind, ExactDirectoryHandle, ExactDirectoryObservation,
    ExactFileObservation, ExactRelocationSource, ExclusiveCreateFailure, FsOps, HardLinkEndpoint,
};
use super::journal::TransactionId;
use super::lock::{
    DEFAULT_KIT_COORDINATION_IGNORE_PATH, KIT_ADVISORY_LOCK_CONTENT,
    KIT_COORDINATION_IGNORE_CONTENT, LEGACY_KIT_COORDINATION_IGNORE_CONTENT, WriteLock,
    coordination_ignore_requires_migration,
};
use super::runtime::{EntropyPurpose, TransactionRuntime, TransitionKey, TransitionWindow};

const KIT_LOGICAL_PATH: &str = "src/components/ui/_kit";
const KIT_PARENT_LOGICAL_PATH: &str = "src/components/ui";
const CANONICAL_NAMESPACE_NAME: &str = ".transactions";
const MIGRATION_PREFIX: &str = ".transactions.bootstrap-v2-";
const MIGRATION_MARKER: &str = ".migration";
const ARMED_SUFFIX: &str = ".migration.armed";
const LEGACY_BOUND_MARKER: &str = ".migration-legacy-";
const WORKSPACE_BOUND_MARKER: &str = ".migration-workspace-";
const BOUND_SUFFIX: &str = ".bound";
const WORKSPACE_SUFFIX: &str = ".migration.namespace";
const INTENT_NAME: &str = "coordination-migration-v2.intent.json";
const INTENT_PARTIAL_NAME: &str = "coordination-migration-v2.intent.json.partial";
const OWNER_NAME: &str = "coordination-ignore-v2.owner";
const OWNER_PARTIAL_NAME: &str = "coordination-ignore-v2.owner.partial";
const COMPLETE_NAME: &str = "coordination-migration-v2.complete.json";
const COMPLETE_PARTIAL_NAME: &str = "coordination-migration-v2.complete.json.partial";
const INTENT_MAGIC: &str = "leptos-ui-kit-coordination-migration";
const COMPLETE_MAGIC: &str = "leptos-ui-kit-coordination-migration-complete";
const MAX_KIT_ENTRIES: usize = 16_384;
const MAX_CONTROL_BYTES: u64 = 16 * 1024;
const MAX_STEPS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AliasKind {
    Armed,
    LegacyBound(ObjectIdentity),
    WorkspaceBound(ObjectIdentity),
}

#[derive(Debug)]
struct MigrationAlias {
    name: String,
    transaction_id: TransactionId,
    kind: AliasKind,
    observation: ExactFileObservation,
}

#[derive(Debug)]
struct MigrationWorkspace {
    name: String,
    transaction_id: TransactionId,
    handle: ExactDirectoryHandle,
}

#[derive(Debug)]
struct MigrationDiscovery {
    kit: Dir,
    alias: Option<MigrationAlias>,
    workspace: Option<MigrationWorkspace>,
    legacy_residual: Option<ExactDirectoryHandle>,
    legacy_ignore: bool,
}

#[derive(Debug, Default)]
struct WorkspaceInventory {
    intent: bool,
    intent_partial: bool,
    owner: bool,
    owner_partial: bool,
    complete: bool,
    complete_partial: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MigrationIntent {
    magic: String,
    version: u32,
    transaction_id: String,
    coordination_parent_identity: String,
    write_lock_identity: String,
    workspace_identity: String,
    legacy_ignore_identity: String,
    legacy_ignore_hash: String,
    desired_ignore_hash: String,
    owner_name: String,
    target_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MigrationComplete {
    magic: String,
    version: u32,
    transaction_id: String,
    desired_ignore_identity: String,
    desired_ignore_hash: String,
}

pub(super) fn check_coordination_migration_pending(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
) -> Result<(), CodegenError> {
    let evidence = migration_evidence_present(context, runtime)?;
    if evidence || coordination_ignore_requires_migration(context, runtime.fs())? {
        return Err(recovery_required(
            context
                .project_root()
                .join(DEFAULT_KIT_COORDINATION_IGNORE_PATH),
            "the exact legacy coordination ignore contract requires mutating migration",
        ));
    }
    Ok(())
}

pub(super) fn recover_coordination_migration(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
) -> Result<(), CodegenError> {
    for _ in 0..MAX_STEPS {
        let discovery = discover(context, lock, runtime)?;
        if discovery.alias.is_none() && discovery.workspace.is_none() && !discovery.legacy_ignore {
            // A separately authenticated namespace-bootstrap alias may
            // already be the held lock's second link. Migration discovery
            // owns only migration names, so defer the exact one-link
            // requirement until namespace recovery has also converged.
            lock.validate_lifecycle_context(context)?;
            return Ok(());
        }
        progress(context, lock, runtime, discovery)?;
    }
    Err(recovery_required(
        context.project_root().join(KIT_LOGICAL_PATH),
        "coordination migration exceeded its bounded progress budget",
    ))
}

fn progress(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    discovery: MigrationDiscovery,
) -> Result<(), CodegenError> {
    let MigrationDiscovery {
        kit,
        alias,
        workspace,
        legacy_residual,
        legacy_ignore,
    } = discovery;
    match (alias.as_ref(), workspace.as_ref(), legacy_residual.as_ref()) {
        (None, None, None) if legacy_ignore => arm(context, lock, runtime, &kit),
        (None, None, Some(_)) if legacy_ignore => arm(context, lock, runtime, &kit),
        (Some(alias), None, Some(residual)) => match alias.kind {
            AliasKind::Armed => bind_legacy(context, lock, runtime, &kit, alias, residual),
            AliasKind::LegacyBound(identity) if identity == residual.observation.identity => {
                cancel_legacy_residual(context, lock, runtime, &kit, residual)
            }
            _ => Err(invalid_world(
                context,
                "migration alias does not bind the exact legacy namespace residual",
            )),
        },
        (Some(alias), None, None) if legacy_ignore => {
            create_workspace(context, lock, runtime, &kit, alias)
        }
        (Some(alias), Some(workspace), None) => {
            if alias.transaction_id != workspace.transaction_id {
                return Err(invalid_world(
                    context,
                    "migration alias and workspace use different identifiers",
                ));
            }
            if alias.kind != AliasKind::WorkspaceBound(workspace.handle.observation.identity) {
                return bind_workspace(context, lock, runtime, &kit, alias, workspace);
            }
            validate_alias(context, lock, alias)?;
            let inventory = workspace_inventory(context, runtime, &kit, workspace)?;
            if inventory.intent_partial {
                if inventory.intent
                    || inventory.owner
                    || inventory.owner_partial
                    || inventory.complete
                    || inventory.complete_partial
                    || !legacy_ignore
                {
                    return Err(invalid_world(
                        context,
                        "migration intent partial coexists with a later or incompatible state",
                    ));
                }
                return publish_intent(context, lock, runtime, &kit, alias, workspace);
            }
            if !inventory.intent {
                if !legacy_ignore
                    && !inventory.owner
                    && !inventory.owner_partial
                    && !inventory.complete_partial
                {
                    return if inventory.complete {
                        cleanup_workspace_file(
                            context,
                            lock,
                            runtime,
                            &kit,
                            alias,
                            workspace,
                            COMPLETE_NAME,
                        )
                    } else {
                        retire_workspace(context, lock, runtime, &kit, alias, workspace)
                    };
                }
                if inventory.owner
                    || inventory.owner_partial
                    || inventory.complete
                    || inventory.complete_partial
                    || !legacy_ignore
                {
                    return Err(invalid_world(
                        context,
                        "migration workspace lacks its required intent predecessor",
                    ));
                }
                return prepare_intent(context, lock, runtime, &kit, alias, workspace);
            }
            if inventory.owner_partial {
                if inventory.owner
                    || inventory.complete
                    || inventory.complete_partial
                    || !legacy_ignore
                {
                    return Err(invalid_world(
                        context,
                        "coordination ignore owner partial coexists with a later state",
                    ));
                }
                return publish_owner(context, lock, runtime, &kit, alias, workspace);
            }
            if legacy_ignore {
                return if inventory.complete || inventory.complete_partial {
                    Err(invalid_world(
                        context,
                        "migration completion exists before the irreversible ignore replacement",
                    ))
                } else if inventory.owner {
                    replace_ignore(context, lock, runtime, &kit, alias, workspace)
                } else {
                    create_owner(context, lock, runtime, &kit, alias, workspace)
                };
            }
            if inventory.owner {
                return Err(invalid_world(
                    context,
                    "coordination ignore owner remains after exact replacement",
                ));
            }
            if inventory.complete_partial {
                if inventory.complete {
                    return Err(invalid_world(
                        context,
                        "migration completion partial and final coexist",
                    ));
                }
                return publish_complete(context, lock, runtime, &kit, alias, workspace);
            }
            if !inventory.complete {
                return prepare_complete(context, lock, runtime, &kit, alias, workspace);
            }
            if inventory.intent {
                cleanup_workspace_file(context, lock, runtime, &kit, alias, workspace, INTENT_NAME)
            } else if inventory.complete {
                cleanup_workspace_file(
                    context,
                    lock,
                    runtime,
                    &kit,
                    alias,
                    workspace,
                    COMPLETE_NAME,
                )
            } else {
                retire_workspace(context, lock, runtime, &kit, alias, workspace)
            }
        }
        (Some(alias), None, None) if !legacy_ignore => {
            retire_alias(context, lock, runtime, &kit, alias)
        }
        (Some(_), None, None) => Err(invalid_world(
            context,
            "coordination migration alias has an inconsistent ignore state",
        )),
        (None, None, None) => Ok(()),
        (None, Some(_), _) => Err(invalid_world(
            context,
            "coordination migration workspace exists without its held-lock alias",
        )),
        (_, _, Some(_)) => Err(invalid_world(
            context,
            "legacy transaction namespace residual has no exact migration binding",
        )),
    }
}

fn arm(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
) -> Result<(), CodegenError> {
    lock.validate_context(context)?;
    let mut entropy = [0_u8; 16];
    runtime
        .fill_entropy(EntropyPurpose::CoordinationMigration, &mut entropy)
        .map_err(|source| {
            recovery_required(
                context.project_root().join(KIT_LOGICAL_PATH),
                format!("could not generate coordination-migration identity: {source}"),
            )
        })?;
    let transaction_id = TransactionId::parse(&hex(&entropy)).map_err(|error| {
        recovery_required(
            context.project_root().join(KIT_LOGICAL_PATH),
            error.to_string(),
        )
    })?;
    let name = armed_name(&transaction_id);
    let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
    let path = kit_path.join(&name);
    let lock_path = kit_path.join(".write.lock");
    let source = runtime
        .fs()
        .read_regular_file_exact(
            kit,
            Path::new(".write.lock"),
            &lock_path,
            KIT_ADVISORY_LOCK_CONTENT.len() as u64,
        )
        .map_err(|source| {
            recovery_required(
                &lock_path,
                format!("held lock could not be rebound before migration arm: {source}"),
            )
        })?;
    if source.bytes != KIT_ADVISORY_LOCK_CONTENT
        || source.observation.identity != lock.identity()
        || source.observation.link_count != Some(1)
    {
        return Err(invalid_world(
            context,
            "migration arm source is not the exact single-link held lock",
        ));
    }
    runtime.observe(TransitionKey::ArmCoordinationMigration {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .hard_link(
            &[],
            HardLinkEndpoint::new(kit, Path::new(".write.lock"), &lock_path),
            &source.observation,
            HardLinkEndpoint::new(kit, Path::new(&name), &path),
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("coordination migration arm has an uncertain outcome: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    let alias = read_alias(
        context,
        runtime,
        kit,
        &name,
        transaction_id,
        AliasKind::Armed,
    )?;
    validate_alias(context, lock, &alias)?;
    runtime.observe(TransitionKey::ArmCoordinationMigration {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn bind_legacy(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    residual: &ExactDirectoryHandle,
) -> Result<(), CodegenError> {
    validate_alias(context, lock, alias)?;
    require_empty_directory(context, runtime, kit, CANONICAL_NAMESPACE_NAME, residual)?;
    let destination = legacy_bound_name(&alias.transaction_id, residual.observation.identity);
    relocate_alias(
        context,
        lock,
        runtime,
        kit,
        alias,
        &destination,
        TransitionKey::BindLegacyTransactionNamespaceResidual {
            window: TransitionWindow::Before,
        },
        TransitionKey::BindLegacyTransactionNamespaceResidual {
            window: TransitionWindow::After,
        },
    )
}

fn cancel_legacy_residual(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    residual: &ExactDirectoryHandle,
) -> Result<(), CodegenError> {
    require_empty_directory(context, runtime, kit, CANONICAL_NAMESPACE_NAME, residual)?;
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(CANONICAL_NAMESPACE_NAME);
    runtime.observe(TransitionKey::CancelLegacyTransactionNamespaceResidual {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .remove_empty_directory_exact(
            DirectoryEndpoint::new(
                kit,
                Path::new(CANONICAL_NAMESPACE_NAME),
                &residual.directory,
                &path,
            ),
            &residual.observation,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("legacy namespace residual cancellation requires recovery: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    runtime.observe(TransitionKey::CancelLegacyTransactionNamespaceResidual {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn create_workspace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
) -> Result<(), CodegenError> {
    validate_alias(context, lock, alias)?;
    let name = workspace_name(&alias.transaction_id);
    let path = context.project_root().join(KIT_LOGICAL_PATH).join(&name);
    runtime.observe(TransitionKey::CreateCoordinationMigrationWorkspace {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .create_directory_exact(kit, Path::new(&name), &path, 0o700)
        .map_err(|source| {
            recovery_required(
                &path,
                format!("coordination migration workspace creation is uncertain: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    let workspace = runtime
        .fs()
        .open_directory_exact(kit, Path::new(&name), &path, 0o700)
        .map_err(|source| {
            recovery_required(
                &path,
                format!("coordination migration workspace could not be rebound: {source}"),
            )
        })?;
    require_empty_directory(context, runtime, kit, &name, &workspace)?;
    runtime.observe(TransitionKey::CreateCoordinationMigrationWorkspace {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn bind_workspace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    validate_alias(context, lock, alias)?;
    require_empty_directory(context, runtime, kit, &workspace.name, &workspace.handle)?;
    let destination =
        workspace_bound_name(&alias.transaction_id, workspace.handle.observation.identity);
    relocate_alias(
        context,
        lock,
        runtime,
        kit,
        alias,
        &destination,
        TransitionKey::BindCoordinationMigrationWorkspace {
            window: TransitionWindow::Before,
        },
        TransitionKey::BindCoordinationMigrationWorkspace {
            window: TransitionWindow::After,
        },
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the exact alias relocation keeps both semantic transition windows explicit"
)]
fn relocate_alias(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    destination_name: &str,
    before: TransitionKey,
    after: TransitionKey,
) -> Result<(), CodegenError> {
    let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
    let source_path = kit_path.join(&alias.name);
    let destination_path = kit_path.join(destination_name);
    runtime.observe(before);
    runtime
        .fs()
        .relocate_noreplace(
            kit,
            Path::new(&alias.name),
            &source_path,
            kit,
            Path::new(destination_name),
            &destination_path,
            &ExactRelocationSource::File(alias.observation.clone()),
        )
        .map_err(|source| {
            recovery_required(
                &destination_path,
                format!(
                    "coordination migration alias binding is uncertain: {}",
                    source.into_io()
                ),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    runtime.observe(after);
    Ok(())
}

fn prepare_intent(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    let bytes = expected_intent_bytes(context, lock, runtime, kit, alias, workspace)?;
    let path = workspace_path(context, workspace).join(INTENT_PARTIAL_NAME);
    runtime.observe(TransitionKey::PrepareCoordinationMigrationIntent {
        window: TransitionWindow::Before,
    });
    write_private_exact(
        runtime.fs(),
        &workspace.handle.directory,
        Path::new(INTENT_PARTIAL_NAME),
        &path,
        &bytes,
    )?;
    sync_workspace(context, lock, runtime, kit, alias, workspace)?;
    runtime.observe(TransitionKey::PrepareCoordinationMigrationIntent {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn publish_intent(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    let bytes = expected_intent_bytes(context, lock, runtime, kit, alias, workspace)?;
    publish_control_partial(
        context,
        lock,
        runtime,
        kit,
        alias,
        workspace,
        INTENT_PARTIAL_NAME,
        INTENT_NAME,
        &bytes,
        0o600,
        TransitionKey::PublishCoordinationMigrationIntent {
            window: TransitionWindow::Before,
        },
        TransitionKey::PublishCoordinationMigrationIntent {
            window: TransitionWindow::After,
        },
    )
}

fn expected_intent_bytes(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<Vec<u8>, CodegenError> {
    validate_workspace_alias(context, lock, alias, workspace)?;
    let ignore = read_ignore(context, runtime, kit)?;
    if ignore.bytes != LEGACY_KIT_COORDINATION_IGNORE_CONTENT {
        return Err(invalid_world(
            context,
            "coordination migration intent requires the exact legacy target",
        ));
    }
    let coordination_parent = observe_kit(context, runtime, kit)?;
    let intent = MigrationIntent {
        magic: INTENT_MAGIC.to_owned(),
        version: 2,
        transaction_id: alias.transaction_id.as_str().to_owned(),
        coordination_parent_identity: identity_hex(coordination_parent.identity),
        write_lock_identity: identity_hex(lock.identity()),
        workspace_identity: identity_hex(workspace.handle.observation.identity),
        legacy_ignore_identity: identity_hex(ignore.observation.identity),
        legacy_ignore_hash: ignore.observation.content_hash.clone(),
        desired_ignore_hash: crate::hash_content_bytes(KIT_COORDINATION_IGNORE_CONTENT),
        owner_name: OWNER_NAME.to_owned(),
        target_name: ".gitignore".to_owned(),
    };
    canonical_json(&intent, context)
}

fn create_owner(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    let _intent = validate_intent(context, lock, runtime, kit, alias, workspace)?;
    let path = workspace_path(context, workspace).join(OWNER_PARTIAL_NAME);
    runtime.observe(TransitionKey::CreateCoordinationIgnoreOwner {
        window: TransitionWindow::Before,
    });
    write_public_exact(
        runtime.fs(),
        &workspace.handle.directory,
        Path::new(OWNER_PARTIAL_NAME),
        &path,
        KIT_COORDINATION_IGNORE_CONTENT,
    )?;
    sync_workspace(context, lock, runtime, kit, alias, workspace)?;
    runtime.observe(TransitionKey::CreateCoordinationIgnoreOwner {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn publish_owner(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    let _intent = validate_intent(context, lock, runtime, kit, alias, workspace)?;
    publish_control_partial(
        context,
        lock,
        runtime,
        kit,
        alias,
        workspace,
        OWNER_PARTIAL_NAME,
        OWNER_NAME,
        KIT_COORDINATION_IGNORE_CONTENT,
        0o644,
        TransitionKey::PublishCoordinationIgnoreOwner {
            window: TransitionWindow::Before,
        },
        TransitionKey::PublishCoordinationIgnoreOwner {
            window: TransitionWindow::After,
        },
    )
}

fn replace_ignore(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    let intent = validate_intent(context, lock, runtime, kit, alias, workspace)?;
    let owner_path = workspace_path(context, workspace).join(OWNER_NAME);
    let owner = runtime
        .fs()
        .read_regular_file_exact(
            &workspace.handle.directory,
            Path::new(OWNER_NAME),
            &owner_path,
            KIT_COORDINATION_IGNORE_CONTENT.len() as u64,
        )
        .map_err(|source| {
            recovery_required(
                &owner_path,
                format!("coordination ignore owner could not be read exactly: {source}"),
            )
        })?;
    require_desired_ignore(&owner.bytes, &owner.observation, context)?;
    if owner.observation.content_hash != intent.desired_ignore_hash {
        return Err(invalid_world(
            context,
            "coordination ignore owner does not match the desired intent hash",
        ));
    }
    let target = read_ignore(context, runtime, kit)?;
    if target.bytes != LEGACY_KIT_COORDINATION_IGNORE_CONTENT {
        return Err(invalid_world(
            context,
            "coordination ignore replacement lost its exact legacy target",
        ));
    }
    if identity_hex(target.observation.identity) != intent.legacy_ignore_identity
        || target.observation.content_hash != intent.legacy_ignore_hash
    {
        return Err(invalid_world(
            context,
            "coordination ignore replacement target differs from the intent-bound legacy preimage",
        ));
    }
    let target_path = context
        .project_root()
        .join(DEFAULT_KIT_COORDINATION_IGNORE_PATH);
    runtime.observe(TransitionKey::ReplaceCoordinationIgnore {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .replace_existing(
            HardLinkEndpoint::new(
                &workspace.handle.directory,
                Path::new(OWNER_NAME),
                &owner_path,
            ),
            &owner.observation,
            HardLinkEndpoint::new(kit, Path::new(".gitignore"), &target_path),
            &target.observation,
        )
        .map_err(|source| {
            recovery_required(
                &target_path,
                format!("coordination ignore replacement has an uncertain outcome: {source}"),
            )
        })?;
    runtime
        .fs()
        .sync_directory(&workspace.handle.directory, &owner_path)
        .map_err(|source| {
            recovery_required(
                &owner_path,
                format!("migration workspace durability requires recovery: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    let desired = read_ignore(context, runtime, kit)?;
    require_desired_ignore(&desired.bytes, &desired.observation, context)?;
    if desired.observation.identity != owner.observation.identity {
        return Err(invalid_world(
            context,
            "desired coordination ignore is not the exact migrated owner",
        ));
    }
    runtime.observe(TransitionKey::ReplaceCoordinationIgnore {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn prepare_complete(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    let bytes = expected_complete_bytes(context, lock, runtime, kit, alias, workspace)?;
    let path = workspace_path(context, workspace).join(COMPLETE_PARTIAL_NAME);
    runtime.observe(TransitionKey::PrepareCoordinationMigrationComplete {
        window: TransitionWindow::Before,
    });
    write_private_exact(
        runtime.fs(),
        &workspace.handle.directory,
        Path::new(COMPLETE_PARTIAL_NAME),
        &path,
        &bytes,
    )?;
    sync_workspace(context, lock, runtime, kit, alias, workspace)?;
    runtime.observe(TransitionKey::PrepareCoordinationMigrationComplete {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn publish_complete(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    let bytes = expected_complete_bytes(context, lock, runtime, kit, alias, workspace)?;
    publish_control_partial(
        context,
        lock,
        runtime,
        kit,
        alias,
        workspace,
        COMPLETE_PARTIAL_NAME,
        COMPLETE_NAME,
        &bytes,
        0o600,
        TransitionKey::PublishCoordinationMigrationComplete {
            window: TransitionWindow::Before,
        },
        TransitionKey::PublishCoordinationMigrationComplete {
            window: TransitionWindow::After,
        },
    )
}

fn expected_complete_bytes(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<Vec<u8>, CodegenError> {
    let intent = validate_intent(context, lock, runtime, kit, alias, workspace)?;
    let desired = read_ignore(context, runtime, kit)?;
    require_desired_ignore(&desired.bytes, &desired.observation, context)?;
    if desired.observation.content_hash != intent.desired_ignore_hash {
        return Err(invalid_world(
            context,
            "desired coordination ignore differs from the migration intent",
        ));
    }
    let complete = MigrationComplete {
        magic: COMPLETE_MAGIC.to_owned(),
        version: 2,
        transaction_id: alias.transaction_id.as_str().to_owned(),
        desired_ignore_identity: identity_hex(desired.observation.identity),
        desired_ignore_hash: desired.observation.content_hash,
    };
    canonical_json(&complete, context)
}

#[expect(
    clippy::too_many_arguments,
    reason = "control publication keeps both exact names, mode, and semantic windows explicit"
)]
fn publish_control_partial(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
    partial_name: &str,
    published_name: &str,
    expected_bytes: &[u8],
    expected_mode: u32,
    before: TransitionKey,
    after: TransitionKey,
) -> Result<(), CodegenError> {
    validate_workspace_alias(context, lock, alias, workspace)?;
    let partial_path = workspace_path(context, workspace).join(partial_name);
    let partial = runtime
        .fs()
        .read_regular_file_exact(
            &workspace.handle.directory,
            Path::new(partial_name),
            &partial_path,
            expected_bytes.len() as u64,
        )
        .map_err(|source| {
            recovery_required(
                &partial_path,
                format!("migration control partial could not be read exactly: {source}"),
            )
        })?;
    let admitted_mode = partial
        .observation
        .mode
        .posix_mode
        .is_none_or(|mode| mode == 0o600 || mode == expected_mode);
    if partial.observation.link_count != Some(1)
        || partial.observation.mode.readonly
        || !admitted_mode
        || !expected_bytes.starts_with(&partial.bytes)
    {
        return Err(invalid_world(
            context,
            "migration control partial is not an exact bounded canonical prefix",
        ));
    }
    if partial.bytes != expected_bytes {
        return cleanup_control_partial(
            context,
            lock,
            runtime,
            kit,
            alias,
            workspace,
            partial_name,
            &partial,
        );
    }
    if partial
        .observation
        .mode
        .posix_mode
        .is_some_and(|mode| mode != expected_mode)
    {
        return Err(invalid_world(
            context,
            "complete migration control partial has not reached its declared final mode",
        ));
    }
    sync_workspace(context, lock, runtime, kit, alias, workspace)?;
    let published_path = workspace_path(context, workspace).join(published_name);
    runtime.observe(before);
    runtime
        .fs()
        .relocate_noreplace(
            &workspace.handle.directory,
            Path::new(partial_name),
            &partial_path,
            &workspace.handle.directory,
            Path::new(published_name),
            &published_path,
            &ExactRelocationSource::File(partial.observation),
        )
        .map_err(|source| {
            recovery_required(
                &published_path,
                format!(
                    "migration control publication has an uncertain outcome: {}",
                    source.into_io()
                ),
            )
        })?;
    sync_workspace(context, lock, runtime, kit, alias, workspace)?;
    runtime.observe(after);
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "partial cleanup retains the exact migration authority inputs"
)]
fn cleanup_control_partial(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
    partial_name: &str,
    partial: &super::fs::ExactFileRead,
) -> Result<(), CodegenError> {
    let path = workspace_path(context, workspace).join(partial_name);
    runtime.observe(TransitionKey::CleanupCoordinationMigrationObject {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .remove_file_exact(
            &workspace.handle.directory,
            Path::new(partial_name),
            &path,
            &partial.observation,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("migration partial cleanup requires recovery: {source}"),
            )
        })?;
    sync_workspace(context, lock, runtime, kit, alias, workspace)?;
    runtime.observe(TransitionKey::CleanupCoordinationMigrationObject {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn cleanup_workspace_file(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
    name: &str,
) -> Result<(), CodegenError> {
    validate_workspace_alias(context, lock, alias, workspace)?;
    match name {
        INTENT_NAME => {
            let _intent = validate_intent(context, lock, runtime, kit, alias, workspace)?;
        }
        COMPLETE_NAME => {
            validate_complete(context, lock, runtime, kit, alias, workspace)?;
        }
        _ => {
            return Err(invalid_world(
                context,
                "migration cleanup requested an unrecognized control object",
            ));
        }
    }
    let path = workspace_path(context, workspace).join(name);
    let read = runtime
        .fs()
        .read_regular_file_exact(
            &workspace.handle.directory,
            Path::new(name),
            &path,
            MAX_CONTROL_BYTES,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("migration cleanup object could not be rebound: {source}"),
            )
        })?;
    runtime.observe(TransitionKey::CleanupCoordinationMigrationObject {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .remove_file_exact(
            &workspace.handle.directory,
            Path::new(name),
            &path,
            &read.observation,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("migration object cleanup requires recovery: {source}"),
            )
        })?;
    sync_workspace(context, lock, runtime, kit, alias, workspace)?;
    runtime.observe(TransitionKey::CleanupCoordinationMigrationObject {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn retire_workspace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    validate_workspace_alias(context, lock, alias, workspace)?;
    require_empty_directory(context, runtime, kit, &workspace.name, &workspace.handle)?;
    let path = workspace_path(context, workspace);
    runtime.observe(TransitionKey::RetireCoordinationMigrationWorkspace {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .remove_empty_directory_exact(
            DirectoryEndpoint::new(
                kit,
                Path::new(&workspace.name),
                &workspace.handle.directory,
                &path,
            ),
            &workspace.handle.observation,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("migration workspace retirement requires recovery: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    runtime.observe(TransitionKey::RetireCoordinationMigrationWorkspace {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn retire_alias(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
) -> Result<(), CodegenError> {
    validate_alias(context, lock, alias)?;
    let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
    let lock_path = kit_path.join(".write.lock");
    let owner = runtime
        .fs()
        .read_regular_file_exact(
            kit,
            Path::new(".write.lock"),
            &lock_path,
            KIT_ADVISORY_LOCK_CONTENT.len() as u64,
        )
        .map_err(|source| {
            recovery_required(
                &lock_path,
                format!("held lock could not be rebound for migration retirement: {source}"),
            )
        })?;
    if owner.bytes != KIT_ADVISORY_LOCK_CONTENT || owner.observation != alias.observation {
        return Err(invalid_world(
            context,
            "held lock and migration alias are not the same exact two-link authority",
        ));
    }
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(&alias.name);
    runtime.observe(TransitionKey::RetireCoordinationMigrationAlias {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .retire_hard_link_alias(
            HardLinkEndpoint::new(kit, Path::new(".write.lock"), &lock_path),
            HardLinkEndpoint::new(kit, Path::new(&alias.name), &path),
            &alias.observation,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("migration alias retirement requires recovery: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 1)?;
    runtime.observe(TransitionKey::RetireCoordinationMigrationAlias {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn discover(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
) -> Result<MigrationDiscovery, CodegenError> {
    lock.validate_lifecycle_context(context)?;
    let kit = context.open_directory(KIT_LOGICAL_PATH)?;
    let observation = observe_kit(context, runtime, &kit)?;
    let parent = context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
    let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
    let inventory = runtime
        .fs()
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(&parent, Path::new("_kit"), &kit, &kit_path),
            &observation,
            MAX_KIT_ENTRIES,
        )
        .map_err(|source| {
            recovery_required(
                &kit_path,
                format!("could not capture bounded coordination migration state: {source}"),
            )
        })?;
    let mut alias = None;
    let mut workspace = None;
    let mut legacy_residual = None;
    for entry in &inventory.entries {
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        if name == CANONICAL_NAMESPACE_NAME {
            if entry.kind != ExactDirectoryEntryKind::Directory {
                return Err(invalid_world(
                    context,
                    "legacy transaction namespace residual has the wrong kind",
                ));
            }
            legacy_residual = Some(
                runtime
                    .fs()
                    .open_directory_exact(&kit, Path::new(name), &kit_path.join(name), 0o700)
                    .map_err(|source| {
                        recovery_required(
                            kit_path.join(name),
                            format!("legacy namespace residual is not exact-private: {source}"),
                        )
                    })?,
            );
            continue;
        }
        if !name.starts_with(MIGRATION_PREFIX) || !name.contains(MIGRATION_MARKER) {
            continue;
        }
        if let Some(transaction_id) = parse_workspace_name(name)? {
            if workspace.is_some() || entry.kind != ExactDirectoryEntryKind::Directory {
                return Err(invalid_world(
                    context,
                    "coordination migration workspace is duplicated or has the wrong kind",
                ));
            }
            workspace = Some(MigrationWorkspace {
                name: name.to_owned(),
                transaction_id,
                handle: runtime
                    .fs()
                    .open_directory_exact(&kit, Path::new(name), &kit_path.join(name), 0o700)
                    .map_err(|source| {
                        recovery_required(
                            kit_path.join(name),
                            format!("migration workspace could not be opened exactly: {source}"),
                        )
                    })?,
            });
            continue;
        }
        let (transaction_id, kind) = parse_alias_name(name)?.ok_or_else(|| {
            invalid_world(
                context,
                "unrecognized coordination migration lifecycle name",
            )
        })?;
        if alias.is_some() || entry.kind != ExactDirectoryEntryKind::RegularFile {
            return Err(invalid_world(
                context,
                "coordination migration alias is duplicated or has the wrong kind",
            ));
        }
        alias = Some(read_alias(
            context,
            runtime,
            &kit,
            name,
            transaction_id,
            kind,
        )?);
    }
    let legacy_ignore = coordination_ignore_requires_migration(context, runtime.fs())?;
    if !legacy_ignore && alias.is_none() && workspace.is_none() {
        legacy_residual = None;
    } else if legacy_residual.is_some() && !legacy_ignore {
        return Err(invalid_world(
            context,
            "canonical transaction namespace cannot be treated as a migration residual after ignore convergence",
        ));
    }
    if legacy_ignore && let Some(residual) = legacy_residual.as_ref() {
        require_empty_directory(context, runtime, &kit, CANONICAL_NAMESPACE_NAME, residual)?;
    }
    Ok(MigrationDiscovery {
        kit,
        alias,
        workspace,
        legacy_residual,
        legacy_ignore,
    })
}

fn migration_evidence_present(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
) -> Result<bool, CodegenError> {
    let kit = match context.open_directory(KIT_LOGICAL_PATH) {
        Ok(kit) => kit,
        Err(CodegenError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let observation = observe_kit(context, runtime, &kit)?;
    let parent = context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
    let path = context.project_root().join(KIT_LOGICAL_PATH);
    let inventory = runtime
        .fs()
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(&parent, Path::new("_kit"), &kit, &path),
            &observation,
            MAX_KIT_ENTRIES,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("could not inspect coordination migration evidence: {source}"),
            )
        })?;
    Ok(inventory.entries.iter().any(|entry| {
        entry.name.to_str().is_some_and(|name| {
            name.starts_with(MIGRATION_PREFIX) && name.contains(MIGRATION_MARKER)
        })
    }))
}

pub(super) fn validate_acquisition_alias(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit: &Dir,
    lock_identity: ObjectIdentity,
) -> Result<(), CodegenError> {
    let parent = context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
    let path = context.project_root().join(KIT_LOGICAL_PATH);
    let observation = fs
        .observe_directory(DirectoryEndpoint::new(
            &parent,
            Path::new("_kit"),
            kit,
            &path,
        ))
        .map_err(|source| {
            recovery_required(
                &path,
                format!("could not observe coordination parent during lock acquisition: {source}"),
            )
        })?;
    let inventory = fs
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(&parent, Path::new("_kit"), kit, &path),
            &observation,
            MAX_KIT_ENTRIES,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("could not inventory migration alias during lock acquisition: {source}"),
            )
        })?;
    let mut matched = false;
    for entry in inventory.entries {
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        if !name.starts_with(MIGRATION_PREFIX) || !name.contains(MIGRATION_MARKER) {
            continue;
        }
        if parse_workspace_name(name)?.is_some() {
            continue;
        }
        if parse_alias_name(name)?.is_none()
            || entry.kind != ExactDirectoryEntryKind::RegularFile
            || matched
        {
            return Err(invalid_world(
                context,
                "held lock has duplicated or malformed coordination-migration alias authority",
            ));
        }
        let alias_path = path.join(name);
        let read = fs
            .read_regular_file_exact(
                kit,
                Path::new(name),
                &alias_path,
                KIT_ADVISORY_LOCK_CONTENT.len() as u64,
            )
            .map_err(|source| {
                recovery_required(
                    &alias_path,
                    format!(
                        "could not authenticate migration alias during lock acquisition: {source}"
                    ),
                )
            })?;
        if read.bytes != KIT_ADVISORY_LOCK_CONTENT
            || read.observation.identity != lock_identity
            || read.observation.link_count != Some(2)
            || read.observation.mode.readonly
            || read
                .observation
                .mode
                .posix_mode
                .is_some_and(|mode| mode != 0o600)
        {
            return Err(invalid_world(
                context,
                "held lock migration alias does not match its exact two-link authority",
            ));
        }
        matched = true;
    }
    if !matched {
        return Err(invalid_world(
            context,
            "two-link held lock has no authenticated coordination-migration alias",
        ));
    }
    Ok(())
}

fn workspace_inventory(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
    kit: &Dir,
    workspace: &MigrationWorkspace,
) -> Result<WorkspaceInventory, CodegenError> {
    let path = workspace_path(context, workspace);
    let inventory = runtime
        .fs()
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(
                kit,
                Path::new(&workspace.name),
                &workspace.handle.directory,
                &path,
            ),
            &workspace.handle.observation,
            6,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("migration workspace inventory is invalid: {source}"),
            )
        })?;
    let mut result = WorkspaceInventory::default();
    for entry in inventory.entries {
        if entry.kind != ExactDirectoryEntryKind::RegularFile {
            return Err(invalid_world(
                context,
                "migration workspace contains a non-regular control object",
            ));
        }
        match entry.name.to_str() {
            Some(INTENT_NAME) if !result.intent => result.intent = true,
            Some(INTENT_PARTIAL_NAME) if !result.intent_partial => result.intent_partial = true,
            Some(OWNER_NAME) if !result.owner => result.owner = true,
            Some(OWNER_PARTIAL_NAME) if !result.owner_partial => result.owner_partial = true,
            Some(COMPLETE_NAME) if !result.complete => result.complete = true,
            Some(COMPLETE_PARTIAL_NAME) if !result.complete_partial => {
                result.complete_partial = true;
            }
            _ => {
                return Err(invalid_world(
                    context,
                    "migration workspace contains an unknown or duplicate object",
                ));
            }
        }
    }
    Ok(result)
}

fn validate_intent(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<MigrationIntent, CodegenError> {
    validate_workspace_alias(context, lock, alias, workspace)?;
    let path = workspace_path(context, workspace).join(INTENT_NAME);
    let read = runtime
        .fs()
        .read_regular_file_exact(
            &workspace.handle.directory,
            Path::new(INTENT_NAME),
            &path,
            MAX_CONTROL_BYTES,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("migration intent could not be read exactly: {source}"),
            )
        })?;
    require_private_control(&read.observation, context)?;
    let intent: MigrationIntent = serde_json::from_slice(&read.bytes).map_err(|source| {
        recovery_required(&path, format!("migration intent JSON is invalid: {source}"))
    })?;
    if canonical_json(&intent, context)? != read.bytes {
        return Err(invalid_world(
            context,
            "migration intent bytes are not canonical",
        ));
    }
    let coordination = observe_kit(context, runtime, kit)?;
    if intent.magic != INTENT_MAGIC
        || intent.version != 2
        || intent.transaction_id != alias.transaction_id.as_str()
        || intent.coordination_parent_identity != identity_hex(coordination.identity)
        || intent.write_lock_identity != identity_hex(lock.identity())
        || intent.workspace_identity != identity_hex(workspace.handle.observation.identity)
        || intent.desired_ignore_hash != crate::hash_content_bytes(KIT_COORDINATION_IGNORE_CONTENT)
        || intent.owner_name != OWNER_NAME
        || intent.target_name != ".gitignore"
    {
        return Err(invalid_world(
            context,
            "migration intent does not bind the exact migration authority",
        ));
    }
    Ok(intent)
}

fn validate_complete(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<MigrationComplete, CodegenError> {
    validate_workspace_alias(context, lock, alias, workspace)?;
    let desired = read_ignore(context, runtime, kit)?;
    require_desired_ignore(&desired.bytes, &desired.observation, context)?;
    let path = workspace_path(context, workspace).join(COMPLETE_NAME);
    let read = runtime
        .fs()
        .read_regular_file_exact(
            &workspace.handle.directory,
            Path::new(COMPLETE_NAME),
            &path,
            MAX_CONTROL_BYTES,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("migration completion record could not be read exactly: {source}"),
            )
        })?;
    require_private_control(&read.observation, context)?;
    let complete: MigrationComplete = serde_json::from_slice(&read.bytes).map_err(|source| {
        recovery_required(
            &path,
            format!("migration completion JSON is invalid: {source}"),
        )
    })?;
    if canonical_json(&complete, context)? != read.bytes {
        return Err(invalid_world(
            context,
            "migration completion bytes are not canonical",
        ));
    }
    if complete.magic != COMPLETE_MAGIC
        || complete.version != 2
        || complete.transaction_id != alias.transaction_id.as_str()
        || complete.desired_ignore_identity != identity_hex(desired.observation.identity)
        || complete.desired_ignore_hash != desired.observation.content_hash
    {
        return Err(invalid_world(
            context,
            "migration completion does not bind the exact desired ignore authority",
        ));
    }
    Ok(complete)
}

fn validate_workspace_alias(
    context: &PlanningContext,
    lock: &WriteLock,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    validate_alias(context, lock, alias)?;
    if alias.kind != AliasKind::WorkspaceBound(workspace.handle.observation.identity) {
        return Err(invalid_world(
            context,
            "migration alias does not bind the exact workspace identity",
        ));
    }
    Ok(())
}

fn validate_alias(
    context: &PlanningContext,
    lock: &WriteLock,
    alias: &MigrationAlias,
) -> Result<(), CodegenError> {
    lock.validate_context_link_count(context, 2)?;
    if alias.observation.identity != lock.identity()
        || alias.observation.byte_len != KIT_ADVISORY_LOCK_CONTENT.len() as u64
        || alias.observation.link_count != Some(2)
        || alias.observation.mode.readonly
        || alias
            .observation
            .mode
            .posix_mode
            .is_some_and(|mode| mode != 0o600)
    {
        return Err(invalid_world(
            context,
            "migration alias is not the exact two-link held write lock",
        ));
    }
    Ok(())
}

fn read_alias(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
    kit: &Dir,
    name: &str,
    transaction_id: TransactionId,
    kind: AliasKind,
) -> Result<MigrationAlias, CodegenError> {
    let path = context.project_root().join(KIT_LOGICAL_PATH).join(name);
    let read = runtime
        .fs()
        .read_regular_file_exact(
            kit,
            Path::new(name),
            &path,
            KIT_ADVISORY_LOCK_CONTENT.len() as u64,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("migration alias could not be read exactly: {source}"),
            )
        })?;
    if read.bytes != KIT_ADVISORY_LOCK_CONTENT {
        return Err(invalid_world(
            context,
            "migration alias bytes differ from the held lock marker",
        ));
    }
    Ok(MigrationAlias {
        name: name.to_owned(),
        transaction_id,
        kind,
        observation: read.observation,
    })
}

fn read_ignore(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
    kit: &Dir,
) -> Result<super::fs::ExactFileRead, CodegenError> {
    let path = context
        .project_root()
        .join(DEFAULT_KIT_COORDINATION_IGNORE_PATH);
    runtime
        .fs()
        .read_regular_file_exact(
            kit,
            Path::new(".gitignore"),
            &path,
            KIT_COORDINATION_IGNORE_CONTENT.len() as u64,
        )
        .map_err(|source| {
            recovery_required(
                path,
                format!("coordination ignore could not be read exactly: {source}"),
            )
        })
}

fn require_desired_ignore(
    bytes: &[u8],
    observation: &ExactFileObservation,
    context: &PlanningContext,
) -> Result<(), CodegenError> {
    if bytes != KIT_COORDINATION_IGNORE_CONTENT
        || observation.link_count != Some(1)
        || observation.mode.readonly
        || observation
            .mode
            .posix_mode
            .is_some_and(|mode| mode != 0o644)
    {
        return Err(invalid_world(
            context,
            "desired coordination ignore is not the exact public single-link file",
        ));
    }
    Ok(())
}

fn require_private_control(
    observation: &ExactFileObservation,
    context: &PlanningContext,
) -> Result<(), CodegenError> {
    if observation.link_count != Some(1)
        || observation.mode.readonly
        || observation
            .mode
            .posix_mode
            .is_some_and(|mode| mode != 0o600)
    {
        return Err(invalid_world(
            context,
            "migration control object is not an independent private file",
        ));
    }
    Ok(())
}

fn require_empty_directory(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
    parent: &Dir,
    name: &str,
    directory: &ExactDirectoryHandle,
) -> Result<(), CodegenError> {
    let path = context.project_root().join(KIT_LOGICAL_PATH).join(name);
    let inventory = runtime
        .fs()
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(parent, Path::new(name), &directory.directory, &path),
            &directory.observation,
            0,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("migration directory is not exact-empty: {source}"),
            )
        })?;
    if !inventory.entries.is_empty() {
        return Err(invalid_world(
            context,
            "migration directory is not exact-empty",
        ));
    }
    Ok(())
}

fn write_private_exact(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<ExactFileObservation, CodegenError> {
    write_exact(fs, parent, name, path, bytes, 0o600)
}

fn write_public_exact(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<ExactFileObservation, CodegenError> {
    write_exact(fs, parent, name, path, bytes, 0o644)
}

fn write_exact(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &Path,
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<ExactFileObservation, CodegenError> {
    let mut created = match fs
        .create_new_file(parent, name, path, 0o600)
        .bind_empty(fs, parent, name, path)
    {
        Ok(created) => created,
        Err(ExclusiveCreateFailure::NotCreated(source)) => {
            return Err(recovery_required(
                path,
                format!("migration file was not created: {source}"),
            ));
        }
        Err(ExclusiveCreateFailure::CreatedUnverified { created, source }) => {
            let _retained = created;
            return Err(recovery_required(
                path,
                format!("migration file was created but remains unverified: {source}"),
            ));
        }
    };
    fs.set_file_mode(&created.file, path, mode)
        .map_err(|source| recovery_required(path, format!("could not set file mode: {source}")))?;
    fs.write_handle(&mut created.file, path, bytes)
        .map_err(|source| recovery_required(path, format!("could not write file: {source}")))?;
    fs.flush_file(&created.file, path)
        .map_err(|source| recovery_required(path, format!("could not flush file: {source}")))?;
    fs.sync_handle(&created.file, path)
        .map_err(|source| recovery_required(path, format!("could not sync file: {source}")))?;
    fs.observe_created_file_exact(parent, name, path, &mut created, bytes.len() as u64)
        .map_err(|source| {
            recovery_required(
                path,
                format!("durable migration file could not be verified: {source}"),
            )
        })
}

fn sync_workspace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &MigrationAlias,
    workspace: &MigrationWorkspace,
) -> Result<(), CodegenError> {
    validate_workspace_alias(context, lock, alias, workspace)?;
    runtime
        .fs()
        .sync_directory(
            &workspace.handle.directory,
            &workspace_path(context, workspace),
        )
        .map_err(|source| {
            recovery_required(
                workspace_path(context, workspace),
                format!("migration workspace durability requires recovery: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)
}

fn sync_kit(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    expected_links: u64,
) -> Result<(), CodegenError> {
    runtime
        .fs()
        .sync_directory(kit, &context.project_root().join(KIT_LOGICAL_PATH))
        .map_err(|source| {
            recovery_required(
                context.project_root().join(KIT_LOGICAL_PATH),
                format!("coordination migration parent sync requires recovery: {source}"),
            )
        })?;
    lock.validate_context_link_count(context, expected_links)?;
    observe_kit(context, runtime, kit)?;
    Ok(())
}

fn observe_kit(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
    kit: &Dir,
) -> Result<ExactDirectoryObservation, CodegenError> {
    let parent = context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
    let path = context.project_root().join(KIT_LOGICAL_PATH);
    runtime
        .fs()
        .observe_directory(DirectoryEndpoint::new(
            &parent,
            Path::new("_kit"),
            kit,
            &path,
        ))
        .map_err(|source| {
            recovery_required(
                path,
                format!("coordination directory could not be observed exactly: {source}"),
            )
        })
}

fn parse_workspace_name(name: &str) -> Result<Option<TransactionId>, CodegenError> {
    let Some(value) = name
        .strip_prefix(MIGRATION_PREFIX)
        .and_then(|value| value.strip_suffix(WORKSPACE_SUFFIX))
    else {
        return Ok(None);
    };
    TransactionId::parse(value)
        .map(Some)
        .map_err(|error| recovery_required(name, error.to_string()))
}

fn parse_alias_name(name: &str) -> Result<Option<(TransactionId, AliasKind)>, CodegenError> {
    let Some(value) = name.strip_prefix(MIGRATION_PREFIX) else {
        return Ok(None);
    };
    if let Some(transaction) = value.strip_suffix(ARMED_SUFFIX) {
        return TransactionId::parse(transaction)
            .map(|transaction| Some((transaction, AliasKind::Armed)))
            .map_err(|error| recovery_required(name, error.to_string()));
    }
    let Some(value) = value.strip_suffix(BOUND_SUFFIX) else {
        return Ok(None);
    };
    let (transaction, identity, legacy) =
        if let Some((transaction, identity)) = value.split_once(LEGACY_BOUND_MARKER) {
            (transaction, identity, true)
        } else if let Some((transaction, identity)) = value.split_once(WORKSPACE_BOUND_MARKER) {
            (transaction, identity, false)
        } else {
            return Ok(None);
        };
    let transaction = TransactionId::parse(transaction)
        .map_err(|error| recovery_required(name, error.to_string()))?;
    let identity = parse_identity_hex(identity)
        .ok_or_else(|| recovery_required(name, "migration alias has an invalid identity"))?;
    Ok(Some((
        transaction,
        if legacy {
            AliasKind::LegacyBound(identity)
        } else {
            AliasKind::WorkspaceBound(identity)
        },
    )))
}

fn armed_name(transaction_id: &TransactionId) -> String {
    format!(
        "{MIGRATION_PREFIX}{}{ARMED_SUFFIX}",
        transaction_id.as_str()
    )
}

fn legacy_bound_name(transaction_id: &TransactionId, identity: ObjectIdentity) -> String {
    format!(
        "{MIGRATION_PREFIX}{}{LEGACY_BOUND_MARKER}{}{BOUND_SUFFIX}",
        transaction_id.as_str(),
        identity_hex(identity)
    )
}

fn workspace_bound_name(transaction_id: &TransactionId, identity: ObjectIdentity) -> String {
    format!(
        "{MIGRATION_PREFIX}{}{WORKSPACE_BOUND_MARKER}{}{BOUND_SUFFIX}",
        transaction_id.as_str(),
        identity_hex(identity)
    )
}

fn workspace_name(transaction_id: &TransactionId) -> String {
    format!(
        "{MIGRATION_PREFIX}{}{WORKSPACE_SUFFIX}",
        transaction_id.as_str()
    )
}

fn workspace_path(context: &PlanningContext, workspace: &MigrationWorkspace) -> PathBuf {
    context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(&workspace.name)
}

fn canonical_json<T: Serialize>(
    value: &T,
    context: &PlanningContext,
) -> Result<Vec<u8>, CodegenError> {
    let mut bytes = serde_json::to_vec(value).map_err(|source| {
        invalid_world(
            context,
            format!("could not serialize canonical migration control JSON: {source}"),
        )
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn identity_hex(identity: ObjectIdentity) -> String {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(&identity.namespace());
    bytes[16..].copy_from_slice(&identity.object_u128().to_le_bytes());
    hex(&bytes)
}

fn parse_identity_hex(value: &str) -> Option<ObjectIdentity> {
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    let mut namespace = [0_u8; 16];
    namespace.copy_from_slice(&bytes[..16]);
    let mut object = [0_u8; 16];
    object.copy_from_slice(&bytes[16..]);
    Some(ObjectIdentity::from_u128(
        u128::from_le_bytes(namespace),
        u128::from_le_bytes(object),
    ))
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

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn invalid_world(context: &PlanningContext, reason: impl Into<String>) -> CodegenError {
    recovery_required(context.project_root().join(KIT_LOGICAL_PATH), reason)
}

fn recovery_required(path: impl Into<PathBuf>, reason: impl Into<String>) -> CodegenError {
    CodegenError::RecoveryRequired {
        journal_path: path.into(),
        reason: reason.into(),
    }
}
