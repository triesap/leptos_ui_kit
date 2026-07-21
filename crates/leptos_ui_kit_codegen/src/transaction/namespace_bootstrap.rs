//! Authenticated activation of the journal-v2 transaction namespace.
//!
//! A private namespace is never created under the canonical `.transactions`
//! name. The held persistent lock first gains an external lifecycle alias,
//! which remains the recovery authority until the ordinary workspace intent is
//! durable inside the activated namespace.

use std::path::{Path, PathBuf};

use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};

use crate::CodegenError;
use crate::path_safety::{ObjectIdentity, PlanningContext};

use super::fs::{
    DirectoryEndpoint, ExactDirectoryEntryKind, ExactDirectoryHandle, ExactDirectoryObservation,
    ExactFileObservation, ExactRelocationSource, ExclusiveCreateFailure, FsOps, HardLinkEndpoint,
};
use super::journal::{
    TransactionId, WorkspaceBootstrapIntentEnvelopeV2, bootstrap_intent_name, canonical_root_hash,
    parse_bootstrap_intent_name,
};
use super::lock::{KIT_ADVISORY_LOCK_CONTENT, WriteLock};
use super::runtime::{TransactionRuntime, TransitionKey, TransitionWindow};
use super::store::exact_directory;
use super::writer::canonical_native_bytes;

const KIT_LOGICAL_PATH: &str = "src/components/ui/_kit";
const KIT_PARENT_LOGICAL_PATH: &str = "src/components/ui";
const CANONICAL_NAMESPACE_NAME: &str = ".transactions";
const BOOTSTRAP_PREFIX: &str = ".transactions.bootstrap-v2-";
const ARMED_SUFFIX: &str = ".armed";
const BOUND_SUFFIX: &str = ".bound";
const NAMESPACE_SUFFIX: &str = ".namespace";
const INTENT_PREFIX: &str = "namespace-bootstrap-v2-";
const INTENT_SUFFIX: &str = ".json";
const INTENT_PARTIAL_SUFFIX: &str = ".json.partial";
const INTENT_MAGIC: &str = "leptos-ui-kit-transaction-namespace-bootstrap";
const MAX_KIT_ENTRIES: usize = 16_384;
const MAX_INTENT_BYTES: u64 = 16 * 1024;
const MAX_PROGRESS_STEPS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AliasKind {
    Armed,
    Bound(ObjectIdentity),
}

#[derive(Debug)]
struct BootstrapAlias {
    name: String,
    transaction_id: TransactionId,
    kind: AliasKind,
    observation: ExactFileObservation,
}

#[derive(Debug)]
struct BootstrapNamespace {
    name: String,
    transaction_id: TransactionId,
    handle: ExactDirectoryHandle,
}

#[derive(Debug)]
struct Discovery {
    kit: Dir,
    alias: Option<BootstrapAlias>,
    bootstrap: Option<BootstrapNamespace>,
    canonical: Option<ExactDirectoryHandle>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NamespaceBootstrapIntent {
    magic: String,
    version: u32,
    transaction_id: String,
    canonical_root_hash: String,
    coordination_parent_identity: String,
    write_lock_identity: String,
    namespace_identity: String,
    bootstrap_name: String,
    active_name: String,
}

impl NamespaceBootstrapIntent {
    fn new(
        context: &PlanningContext,
        lock: &WriteLock,
        transaction_id: &TransactionId,
        bootstrap: &BootstrapNamespace,
        coordination_parent: ExactDirectoryObservation,
    ) -> Self {
        Self {
            magic: INTENT_MAGIC.to_owned(),
            version: 2,
            transaction_id: transaction_id.as_str().to_owned(),
            canonical_root_hash: canonical_root_hash(&canonical_native_bytes(
                context.project_root(),
            ))
            .as_str()
            .to_owned(),
            coordination_parent_identity: identity_hex(coordination_parent.identity),
            write_lock_identity: identity_hex(lock.identity()),
            namespace_identity: identity_hex(bootstrap.handle.observation.identity),
            bootstrap_name: bootstrap.name.clone(),
            active_name: CANONICAL_NAMESPACE_NAME.to_owned(),
        }
    }

    fn canonical_bytes(&self) -> Result<Vec<u8>, CodegenError> {
        let mut bytes = serde_json::to_vec(self).map_err(|source| {
            recovery_required(
                PathBuf::from(KIT_LOGICAL_PATH),
                format!("could not serialize namespace-bootstrap intent: {source}"),
            )
        })?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn validate(
        &self,
        context: &PlanningContext,
        lock: &WriteLock,
        transaction_id: &TransactionId,
        namespace_name: &str,
        namespace: &ExactDirectoryObservation,
        coordination_parent: &ExactDirectoryObservation,
    ) -> Result<(), CodegenError> {
        let expected_root = canonical_root_hash(&canonical_native_bytes(context.project_root()));
        if self.magic != INTENT_MAGIC
            || self.version != 2
            || self.transaction_id != transaction_id.as_str()
            || self.canonical_root_hash != expected_root.as_str()
            || self.coordination_parent_identity != identity_hex(coordination_parent.identity)
            || self.write_lock_identity != identity_hex(lock.identity())
            || self.namespace_identity != identity_hex(namespace.identity)
            || self.bootstrap_name != namespace_name
            || self.active_name != CANONICAL_NAMESPACE_NAME
        {
            return Err(recovery_required(
                context.project_root().join(KIT_LOGICAL_PATH),
                "namespace-bootstrap intent does not bind the exact root, coordination parent, held lock, namespace, transaction, and names",
            ));
        }
        Ok(())
    }
}

pub(super) fn check_namespace_bootstrap_pending(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
) -> Result<(), CodegenError> {
    let discovery = discover(context, runtime)?;
    if discovery.alias.is_some() || discovery.bootstrap.is_some() {
        return Err(recovery_required(
            context.project_root().join(KIT_LOGICAL_PATH),
            "a transaction-namespace bootstrap requires the mutating recovery path",
        ));
    }
    Ok(())
}

pub(super) fn recover_namespace_bootstrap(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
) -> Result<(), CodegenError> {
    for _ in 0..MAX_PROGRESS_STEPS {
        let discovery = discover(context, runtime)?;
        if discovery.alias.is_none() && discovery.bootstrap.is_none() {
            if let Some(canonical) = discovery.canonical.as_ref()
                && cancel_orphan_workspace_intent(
                    context,
                    lock,
                    runtime,
                    &discovery.kit,
                    canonical,
                )?
            {
                continue;
            }
            return Ok(());
        }
        recover_or_progress(context, lock, runtime, discovery, None)?;
    }
    Err(recovery_required(
        context.project_root().join(KIT_LOGICAL_PATH),
        "transaction-namespace bootstrap recovery exceeded its bounded progress budget",
    ))
}

pub(super) fn ensure_transaction_namespace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    transaction_id: &TransactionId,
) -> Result<Dir, CodegenError> {
    for _ in 0..MAX_PROGRESS_STEPS {
        let discovery = discover(context, runtime)?;
        if let Some(canonical) = discovery.canonical {
            match &discovery.alias {
                Some(alias)
                    if alias.transaction_id == *transaction_id
                        && namespace_intent_is_present(
                            context,
                            runtime,
                            &canonical,
                            transaction_id,
                        )? =>
                {
                    validate_alias_for_namespace(context, lock, alias, &canonical.observation)?;
                    validate_namespace_intent(
                        context,
                        lock,
                        runtime,
                        &canonical,
                        transaction_id,
                        &namespace_name(transaction_id),
                    )?;
                    return Ok(canonical.directory);
                }
                Some(_) => {
                    recover_or_progress(
                        context,
                        lock,
                        runtime,
                        Discovery {
                            canonical: Some(canonical),
                            ..discovery
                        },
                        Some(transaction_id),
                    )?;
                    continue;
                }
                None => {
                    return Err(recovery_required(
                        context
                            .project_root()
                            .join(KIT_LOGICAL_PATH)
                            .join(CANONICAL_NAMESPACE_NAME),
                        "canonical transaction namespace exists without lifecycle or journal authority",
                    ));
                }
            }
        }
        recover_or_progress(context, lock, runtime, discovery, Some(transaction_id))?;
    }
    Err(recovery_required(
        context.project_root().join(KIT_LOGICAL_PATH),
        "transaction-namespace bootstrap exceeded its bounded progress budget",
    ))
}

pub(super) fn finish_transaction_namespace_bootstrap(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    transaction_id: &TransactionId,
) -> Result<Dir, CodegenError> {
    for _ in 0..4 {
        let discovery = discover(context, runtime)?;
        let canonical = discovery.canonical.as_ref().ok_or_else(|| {
            recovery_required(
                context.project_root().join(KIT_LOGICAL_PATH),
                "activated transaction namespace disappeared before bootstrap retirement",
            )
        })?;
        let workspace_intent =
            workspace_intent_is_present(context, runtime, canonical, transaction_id)?;
        if !workspace_intent {
            return Err(recovery_required(
                context.project_root().join(KIT_LOGICAL_PATH),
                "namespace-bootstrap authority cannot retire before the ordinary workspace intent is durable",
            ));
        }
        if namespace_intent_is_present(context, runtime, canonical, transaction_id)? {
            remove_namespace_intent(
                context,
                lock,
                runtime,
                &discovery.kit,
                canonical,
                transaction_id,
                false,
            )?;
            continue;
        }
        let Some(alias) = discovery.alias.as_ref() else {
            lock.validate_context(context)?;
            return canonical.directory.try_clone().map_err(|source| {
                recovery_required(
                    context.project_root().join(KIT_LOGICAL_PATH),
                    format!("could not retain activated namespace capability: {source}"),
                )
            });
        };
        validate_alias_for_namespace(context, lock, alias, &canonical.observation)?;
        retire_alias(context, lock, runtime, &discovery.kit, alias)?;
    }
    let discovery = discover(context, runtime)?;
    if discovery.alias.is_none()
        && discovery.bootstrap.is_none()
        && let Some(canonical) = discovery.canonical
    {
        lock.validate_context(context)?;
        return Ok(canonical.directory);
    }
    Err(recovery_required(
        context.project_root().join(KIT_LOGICAL_PATH),
        "namespace-bootstrap authority did not converge after ordinary workspace ownership",
    ))
}

fn cancel_orphan_workspace_intent(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    canonical: &ExactDirectoryHandle,
) -> Result<bool, CodegenError> {
    lock.validate_context(context)?;
    let namespace_path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(CANONICAL_NAMESPACE_NAME);
    let inventory = runtime
        .fs()
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(
                kit,
                Path::new(CANONICAL_NAMESPACE_NAME),
                &canonical.directory,
                &namespace_path,
            ),
            &canonical.observation,
            MAX_KIT_ENTRIES,
        )
        .map_err(|source| {
            recovery_required(
                &namespace_path,
                format!("could not classify an orphan workspace intent exactly: {source}"),
            )
        })?;
    let [entry] = inventory.entries.as_slice() else {
        return Ok(false);
    };
    if entry.kind != ExactDirectoryEntryKind::RegularFile {
        return Ok(false);
    }
    let Some(name) = entry.name.to_str() else {
        return Ok(false);
    };
    let transaction_id = match parse_bootstrap_intent_name(name) {
        Ok(transaction_id) => transaction_id,
        Err(_) => return Ok(false),
    };
    if !workspace_intent_is_present(context, runtime, canonical, &transaction_id)? {
        return Ok(false);
    }
    let path = namespace_path.join(name);
    let read = runtime
        .fs()
        .read_regular_file_exact(
            &canonical.directory,
            Path::new(name),
            &path,
            MAX_INTENT_BYTES,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("orphan workspace intent could not be rebound exactly: {source}"),
            )
        })?;
    runtime.observe(TransitionKey::RemoveWorkspaceBootstrapIntent {
        outcome: super::runtime::TransactionOutcome::Rollback,
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .remove_file_exact(
            &canonical.directory,
            Path::new(name),
            &path,
            &read.observation,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("orphan workspace-intent retirement requires recovery: {source}"),
            )
        })?;
    runtime
        .fs()
        .sync_directory(&canonical.directory, &path)
        .map_err(|source| {
            recovery_required(
                &path,
                format!("orphan workspace-intent durability requires recovery: {source}"),
            )
        })?;
    runtime.observe(TransitionKey::RemoveWorkspaceBootstrapIntent {
        outcome: super::runtime::TransactionOutcome::Rollback,
        window: TransitionWindow::After,
    });

    let current = runtime
        .fs()
        .observe_directory(DirectoryEndpoint::new(
            kit,
            Path::new(CANONICAL_NAMESPACE_NAME),
            &canonical.directory,
            &namespace_path,
        ))
        .map_err(|source| {
            recovery_required(
                &namespace_path,
                format!("orphan transaction namespace could not be rebound after intent retirement: {source}"),
            )
        })?;
    runtime.observe(TransitionKey::CancelTransactionNamespaceBootstrap {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .remove_empty_directory_exact(
            DirectoryEndpoint::new(
                kit,
                Path::new(CANONICAL_NAMESPACE_NAME),
                &canonical.directory,
                &namespace_path,
            ),
            &current,
        )
        .map_err(|source| {
            recovery_required(
                &namespace_path,
                format!("orphan transaction namespace retirement requires recovery: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 1)?;
    runtime.observe(TransitionKey::CancelTransactionNamespaceBootstrap {
        window: TransitionWindow::After,
    });
    Ok(true)
}

fn recover_or_progress(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    discovery: Discovery,
    desired: Option<&TransactionId>,
) -> Result<(), CodegenError> {
    match (
        discovery.alias.as_ref(),
        discovery.bootstrap.as_ref(),
        discovery.canonical.as_ref(),
    ) {
        (None, None, None) => {
            let desired = desired.ok_or_else(|| {
                recovery_required(
                    context.project_root().join(KIT_LOGICAL_PATH),
                    "namespace-bootstrap recovery has no state to recover",
                )
            })?;
            arm(context, lock, runtime, &discovery.kit, desired)
        }
        (Some(alias), None, None) if alias.kind == AliasKind::Armed => {
            if desired == Some(&alias.transaction_id) {
                create_namespace(context, lock, runtime, &discovery.kit, alias)
            } else {
                cancel_alias(context, lock, runtime, &discovery.kit, alias)
            }
        }
        (Some(alias), Some(namespace), None) if alias.kind == AliasKind::Armed => {
            if alias.transaction_id != namespace.transaction_id {
                return Err(invalid_world(
                    context,
                    "armed alias and private namespace use different transaction identifiers",
                ));
            }
            if desired == Some(&alias.transaction_id) {
                bind_alias(context, lock, runtime, &discovery.kit, alias, namespace)
            } else {
                cancel_namespace(context, lock, runtime, &discovery.kit, namespace)
            }
        }
        (Some(alias), Some(namespace), None) => {
            validate_alias_for_namespace(context, lock, alias, &namespace.handle.observation)?;
            if alias.transaction_id != namespace.transaction_id {
                return Err(invalid_world(
                    context,
                    "bound alias and private namespace use different transaction identifiers",
                ));
            }
            let intent = namespace_intent_is_present(
                context,
                runtime,
                &namespace.handle,
                &namespace.transaction_id,
            )?;
            let partial = namespace_intent_partial_is_present(
                context,
                &namespace.handle,
                &namespace.transaction_id,
                &namespace.name,
            )?;
            if intent && partial {
                return Err(invalid_world(
                    context,
                    "namespace-bootstrap intent partial and final coexist",
                ));
            }
            if desired == Some(&alias.transaction_id) {
                if intent {
                    activate(context, lock, runtime, &discovery.kit, alias, namespace)
                } else if partial {
                    publish_intent(context, lock, runtime, &discovery.kit, alias, namespace)
                } else {
                    prepare_intent(context, lock, runtime, &discovery.kit, alias, namespace)
                }
            } else if intent {
                remove_namespace_intent(
                    context,
                    lock,
                    runtime,
                    &discovery.kit,
                    &namespace.handle,
                    &namespace.transaction_id,
                    true,
                )
            } else if partial {
                cleanup_namespace_intent_partial(
                    context,
                    lock,
                    runtime,
                    &discovery.kit,
                    alias,
                    namespace,
                )
            } else {
                cancel_namespace(context, lock, runtime, &discovery.kit, namespace)
            }
        }
        (Some(alias), None, Some(canonical)) => {
            validate_alias_for_namespace(context, lock, alias, &canonical.observation)?;
            let workspace_intent =
                workspace_intent_is_present(context, runtime, canonical, &alias.transaction_id)?;
            let namespace_intent =
                namespace_intent_is_present(context, runtime, canonical, &alias.transaction_id)?;
            if desired == Some(&alias.transaction_id) && namespace_intent && !workspace_intent {
                return Ok(());
            }
            if namespace_intent {
                remove_namespace_intent(
                    context,
                    lock,
                    runtime,
                    &discovery.kit,
                    canonical,
                    &alias.transaction_id,
                    !workspace_intent,
                )
            } else if workspace_intent {
                retire_alias(context, lock, runtime, &discovery.kit, alias)
            } else {
                cancel_canonical_namespace(context, lock, runtime, &discovery.kit, canonical)
            }
        }
        (Some(alias), None, None) => cancel_alias(context, lock, runtime, &discovery.kit, alias),
        (None, Some(_), _) => Err(invalid_world(
            context,
            "private bootstrap namespace exists without its held-lock alias",
        )),
        (_, Some(_), Some(_)) => Err(invalid_world(
            context,
            "private and canonical transaction namespaces coexist",
        )),
        (None, None, Some(_)) => Ok(()),
    }
}

fn arm(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    transaction_id: &TransactionId,
) -> Result<(), CodegenError> {
    lock.validate_context(context)?;
    let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
    let lock_path = kit_path.join(".write.lock");
    let alias_name = armed_alias_name(transaction_id);
    let alias_path = kit_path.join(&alias_name);
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
                format!("held lock could not be rebound before bootstrap arm: {source}"),
            )
        })?;
    if source.bytes != KIT_ADVISORY_LOCK_CONTENT
        || source.observation.identity != lock.identity()
        || source.observation.link_count != Some(1)
    {
        return Err(invalid_world(
            context,
            "bootstrap arm source is not the exact single-link held lock",
        ));
    }
    runtime.observe(TransitionKey::ArmTransactionNamespaceBootstrap {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .hard_link(
            &[],
            HardLinkEndpoint::new(kit, Path::new(".write.lock"), &lock_path),
            &source.observation,
            HardLinkEndpoint::new(kit, Path::new(&alias_name), &alias_path),
        )
        .map_err(|source| {
            recovery_required(
                &alias_path,
                format!(
                    "transaction-namespace bootstrap arm has an uncertain visible outcome: {source}"
                ),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    let alias = read_alias(
        context,
        runtime,
        kit,
        &alias_name,
        transaction_id,
        AliasKind::Armed,
    )?;
    validate_alias_lock_observation(context, lock, &alias.observation)?;
    runtime.observe(TransitionKey::ArmTransactionNamespaceBootstrap {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn create_namespace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &BootstrapAlias,
) -> Result<(), CodegenError> {
    validate_alias_lock_observation(context, lock, &alias.observation)?;
    let name = namespace_name(&alias.transaction_id);
    let path = context.project_root().join(KIT_LOGICAL_PATH).join(&name);
    runtime.observe(TransitionKey::CreateTransactionNamespace {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .create_directory_exact(kit, Path::new(&name), &path, 0o700)
        .map_err(|source| {
            recovery_required(
                &path,
                format!(
                    "private transaction-namespace creation has an uncertain visible outcome: {source}"
                ),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    let handle = runtime
        .fs()
        .open_directory_exact(kit, Path::new(&name), &path, 0o700)
        .map_err(|source| {
            recovery_required(
                &path,
                format!("created private namespace could not be rebound exactly: {source}"),
            )
        })?;
    require_empty(runtime.fs(), kit, &name, &path, &handle)?;
    runtime.observe(TransitionKey::CreateTransactionNamespace {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn bind_alias(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &BootstrapAlias,
    namespace: &BootstrapNamespace,
) -> Result<(), CodegenError> {
    validate_alias_lock_observation(context, lock, &alias.observation)?;
    require_empty(
        runtime.fs(),
        kit,
        &namespace.name,
        &context
            .project_root()
            .join(KIT_LOGICAL_PATH)
            .join(&namespace.name),
        &namespace.handle,
    )?;
    let destination_name =
        bound_alias_name(&alias.transaction_id, namespace.handle.observation.identity);
    let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
    let source_path = kit_path.join(&alias.name);
    let destination_path = kit_path.join(&destination_name);
    runtime.observe(TransitionKey::BindTransactionNamespaceBootstrap {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .relocate_noreplace(
            kit,
            Path::new(&alias.name),
            &source_path,
            kit,
            Path::new(&destination_name),
            &destination_path,
            &ExactRelocationSource::File(alias.observation.clone()),
        )
        .map_err(|source| {
            recovery_required(
                &destination_path,
                format!(
                    "bootstrap-alias identity binding has an uncertain visible outcome: {}",
                    source.into_io()
                ),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    let bound = read_alias(
        context,
        runtime,
        kit,
        &destination_name,
        &alias.transaction_id,
        AliasKind::Bound(namespace.handle.observation.identity),
    )?;
    validate_alias_for_namespace(context, lock, &bound, &namespace.handle.observation)?;
    runtime.observe(TransitionKey::BindTransactionNamespaceBootstrap {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn prepare_intent(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &BootstrapAlias,
    namespace: &BootstrapNamespace,
) -> Result<(), CodegenError> {
    require_empty(
        runtime.fs(),
        kit,
        &namespace.name,
        &context
            .project_root()
            .join(KIT_LOGICAL_PATH)
            .join(&namespace.name),
        &namespace.handle,
    )?;
    let bytes = expected_namespace_intent_bytes(context, lock, runtime, kit, alias, namespace)?;
    let name = intent_partial_name(&namespace.transaction_id);
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(&namespace.name)
        .join(&name);
    runtime.observe(TransitionKey::PrepareTransactionNamespaceBootstrapIntent {
        window: TransitionWindow::Before,
    });
    write_private_exact(
        runtime.fs(),
        &namespace.handle.directory,
        Path::new(&name),
        &path,
        &bytes,
    )?;
    runtime
        .fs()
        .sync_directory(&namespace.handle.directory, &path)
        .map_err(|source| {
            recovery_required(
                &path,
                format!("could not sync namespace-bootstrap intent partial parent: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    runtime.observe(TransitionKey::PrepareTransactionNamespaceBootstrapIntent {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn publish_intent(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &BootstrapAlias,
    namespace: &BootstrapNamespace,
) -> Result<(), CodegenError> {
    let bytes = expected_namespace_intent_bytes(context, lock, runtime, kit, alias, namespace)?;
    let partial_name = intent_partial_name(&namespace.transaction_id);
    let partial_path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(&namespace.name)
        .join(&partial_name);
    let partial = runtime
        .fs()
        .read_regular_file_exact(
            &namespace.handle.directory,
            Path::new(&partial_name),
            &partial_path,
            bytes.len() as u64,
        )
        .map_err(|source| {
            recovery_required(
                &partial_path,
                format!("namespace-bootstrap intent partial could not be read exactly: {source}"),
            )
        })?;
    if partial.observation.link_count != Some(1)
        || partial.observation.mode.readonly
        || partial
            .observation
            .mode
            .posix_mode
            .is_some_and(|mode| mode != 0o600)
        || !bytes.starts_with(&partial.bytes)
    {
        return Err(invalid_world(
            context,
            "namespace-bootstrap intent partial is not an exact private canonical prefix",
        ));
    }
    if partial.bytes != bytes {
        return cleanup_namespace_intent_partial(context, lock, runtime, kit, alias, namespace);
    }
    runtime
        .fs()
        .sync_directory(&namespace.handle.directory, &partial_path)
        .map_err(|source| {
            recovery_required(
                &partial_path,
                format!("could not sync complete namespace-bootstrap intent partial: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    let name = intent_name(&namespace.transaction_id);
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(&namespace.name)
        .join(&name);
    runtime.observe(TransitionKey::PublishTransactionNamespaceBootstrapIntent {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .relocate_noreplace(
            &namespace.handle.directory,
            Path::new(&partial_name),
            &partial_path,
            &namespace.handle.directory,
            Path::new(&name),
            &path,
            &ExactRelocationSource::File(partial.observation),
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!(
                    "namespace-bootstrap intent publication has an uncertain outcome: {}",
                    source.into_io()
                ),
            )
        })?;
    runtime
        .fs()
        .sync_directory(&namespace.handle.directory, &path)
        .map_err(|source| {
            recovery_required(
                &path,
                format!("could not sync namespace-bootstrap intent parent: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    validate_namespace_intent(
        context,
        lock,
        runtime,
        &namespace.handle,
        &namespace.transaction_id,
        &namespace.name,
    )?;
    runtime.observe(TransitionKey::PublishTransactionNamespaceBootstrapIntent {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn expected_namespace_intent_bytes(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &BootstrapAlias,
    namespace: &BootstrapNamespace,
) -> Result<Vec<u8>, CodegenError> {
    validate_alias_for_namespace(context, lock, alias, &namespace.handle.observation)?;
    let coordination_parent = observe_kit(context, runtime, kit)?;
    let intent = NamespaceBootstrapIntent::new(
        context,
        lock,
        &namespace.transaction_id,
        namespace,
        coordination_parent,
    );
    let bytes = intent.canonical_bytes()?;
    Ok(bytes)
}

fn activate(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &BootstrapAlias,
    namespace: &BootstrapNamespace,
) -> Result<(), CodegenError> {
    validate_alias_for_namespace(context, lock, alias, &namespace.handle.observation)?;
    validate_namespace_intent(
        context,
        lock,
        runtime,
        &namespace.handle,
        &namespace.transaction_id,
        &namespace.name,
    )?;
    let kit_path = context.project_root().join(KIT_LOGICAL_PATH);
    let source_path = kit_path.join(&namespace.name);
    let destination_path = kit_path.join(CANONICAL_NAMESPACE_NAME);
    runtime.observe(TransitionKey::ActivateTransactionNamespace {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .relocate_noreplace(
            kit,
            Path::new(&namespace.name),
            &source_path,
            kit,
            Path::new(CANONICAL_NAMESPACE_NAME),
            &destination_path,
            &ExactRelocationSource::Directory(namespace.handle.observation),
        )
        .map_err(|source| {
            recovery_required(
                &destination_path,
                format!(
                    "transaction-namespace activation has an uncertain visible outcome: {}",
                    source.into_io()
                ),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    let active = runtime
        .fs()
        .open_directory_exact(
            kit,
            Path::new(CANONICAL_NAMESPACE_NAME),
            &destination_path,
            0o700,
        )
        .map_err(|source| {
            recovery_required(
                &destination_path,
                format!("activated namespace could not be rebound exactly: {source}"),
            )
        })?;
    if active.observation != namespace.handle.observation {
        return Err(invalid_world(
            context,
            "activated canonical namespace has a substituted identity",
        ));
    }
    validate_namespace_intent(
        context,
        lock,
        runtime,
        &active,
        &namespace.transaction_id,
        &namespace.name,
    )?;
    runtime.observe(TransitionKey::ActivateTransactionNamespace {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn cleanup_namespace_intent_partial(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &BootstrapAlias,
    namespace: &BootstrapNamespace,
) -> Result<(), CodegenError> {
    let expected = expected_namespace_intent_bytes(context, lock, runtime, kit, alias, namespace)?;
    let name = intent_partial_name(&namespace.transaction_id);
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(&namespace.name)
        .join(&name);
    let partial = runtime
        .fs()
        .read_regular_file_exact(
            &namespace.handle.directory,
            Path::new(&name),
            &path,
            expected.len() as u64,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("namespace-bootstrap intent partial could not be rebound: {source}"),
            )
        })?;
    if partial.observation.link_count != Some(1)
        || partial.observation.mode.readonly
        || partial
            .observation
            .mode
            .posix_mode
            .is_some_and(|mode| mode != 0o600)
        || !expected.starts_with(&partial.bytes)
    {
        return Err(invalid_world(
            context,
            "namespace-bootstrap cleanup partial is not an admitted canonical prefix",
        ));
    }
    runtime.observe(
        TransitionKey::CleanupTransactionNamespaceBootstrapIntentPartial {
            window: TransitionWindow::Before,
        },
    );
    runtime
        .fs()
        .remove_file_exact(
            &namespace.handle.directory,
            Path::new(&name),
            &path,
            &partial.observation,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("namespace-bootstrap partial cleanup requires recovery: {source}"),
            )
        })?;
    runtime
        .fs()
        .sync_directory(&namespace.handle.directory, &path)
        .map_err(|source| {
            recovery_required(
                &path,
                format!("namespace-bootstrap partial cleanup sync requires recovery: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    runtime.observe(
        TransitionKey::CleanupTransactionNamespaceBootstrapIntentPartial {
            window: TransitionWindow::After,
        },
    );
    Ok(())
}

fn remove_namespace_intent(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    namespace: &ExactDirectoryHandle,
    transaction_id: &TransactionId,
    cancel: bool,
) -> Result<(), CodegenError> {
    let location_name = if name_is_present(
        kit,
        Path::new(CANONICAL_NAMESPACE_NAME),
        &context
            .project_root()
            .join(KIT_LOGICAL_PATH)
            .join(CANONICAL_NAMESPACE_NAME),
    )? {
        CANONICAL_NAMESPACE_NAME.to_owned()
    } else {
        namespace_name(transaction_id)
    };
    let intent_name = intent_name(transaction_id);
    validate_namespace_intent(
        context,
        lock,
        runtime,
        namespace,
        transaction_id,
        &namespace_name(transaction_id),
    )?;
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(&location_name)
        .join(&intent_name);
    let read = runtime
        .fs()
        .read_regular_file_exact(
            &namespace.directory,
            Path::new(&intent_name),
            &path,
            MAX_INTENT_BYTES,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("namespace-bootstrap intent could not be rebound for retirement: {source}"),
            )
        })?;
    let key = if cancel {
        TransitionKey::CancelTransactionNamespaceBootstrapIntent {
            window: TransitionWindow::Before,
        }
    } else {
        TransitionKey::RetireTransactionNamespaceBootstrapIntent {
            window: TransitionWindow::Before,
        }
    };
    runtime.observe(key);
    runtime
        .fs()
        .remove_file_exact(
            &namespace.directory,
            Path::new(&intent_name),
            &path,
            &read.observation,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("namespace-bootstrap intent retirement requires recovery: {source}"),
            )
        })?;
    runtime
        .fs()
        .sync_directory(&namespace.directory, &path)
        .map_err(|source| {
            recovery_required(
                &path,
                format!("namespace-bootstrap intent parent sync requires recovery: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    if namespace_intent_is_present(context, runtime, namespace, transaction_id)? {
        return Err(invalid_world(
            context,
            "retired namespace-bootstrap intent is still visible",
        ));
    }
    runtime.observe(if cancel {
        TransitionKey::CancelTransactionNamespaceBootstrapIntent {
            window: TransitionWindow::After,
        }
    } else {
        TransitionKey::RetireTransactionNamespaceBootstrapIntent {
            window: TransitionWindow::After,
        }
    });
    Ok(())
}

fn cancel_namespace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    namespace: &BootstrapNamespace,
) -> Result<(), CodegenError> {
    require_empty(
        runtime.fs(),
        kit,
        &namespace.name,
        &context
            .project_root()
            .join(KIT_LOGICAL_PATH)
            .join(&namespace.name),
        &namespace.handle,
    )?;
    cancel_directory(
        context,
        lock,
        runtime,
        kit,
        &namespace.name,
        &namespace.handle,
    )
}

fn cancel_canonical_namespace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    canonical: &ExactDirectoryHandle,
) -> Result<(), CodegenError> {
    require_empty(
        runtime.fs(),
        kit,
        CANONICAL_NAMESPACE_NAME,
        &context
            .project_root()
            .join(KIT_LOGICAL_PATH)
            .join(CANONICAL_NAMESPACE_NAME),
        canonical,
    )?;
    cancel_directory(
        context,
        lock,
        runtime,
        kit,
        CANONICAL_NAMESPACE_NAME,
        canonical,
    )
}

fn cancel_directory(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    name: &str,
    namespace: &ExactDirectoryHandle,
) -> Result<(), CodegenError> {
    let path = context.project_root().join(KIT_LOGICAL_PATH).join(name);
    runtime.observe(TransitionKey::CancelTransactionNamespaceBootstrap {
        window: TransitionWindow::Before,
    });
    runtime
        .fs()
        .remove_empty_directory_exact(
            DirectoryEndpoint::new(kit, Path::new(name), &namespace.directory, &path),
            &namespace.observation,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("exact bootstrap-namespace cancellation requires recovery: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 2)?;
    runtime.observe(TransitionKey::CancelTransactionNamespaceBootstrap {
        window: TransitionWindow::After,
    });
    Ok(())
}

fn cancel_alias(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &BootstrapAlias,
) -> Result<(), CodegenError> {
    retire_alias_with_key(
        context,
        lock,
        runtime,
        kit,
        alias,
        TransitionKey::CancelTransactionNamespaceBootstrap {
            window: TransitionWindow::Before,
        },
        TransitionKey::CancelTransactionNamespaceBootstrap {
            window: TransitionWindow::After,
        },
    )
}

fn retire_alias(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &BootstrapAlias,
) -> Result<(), CodegenError> {
    retire_alias_with_key(
        context,
        lock,
        runtime,
        kit,
        alias,
        TransitionKey::RetireTransactionNamespaceBootstrapAlias {
            window: TransitionWindow::Before,
        },
        TransitionKey::RetireTransactionNamespaceBootstrapAlias {
            window: TransitionWindow::After,
        },
    )
}

fn retire_alias_with_key(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    alias: &BootstrapAlias,
    before: TransitionKey,
    after: TransitionKey,
) -> Result<(), CodegenError> {
    validate_alias_lock_observation(context, lock, &alias.observation)?;
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
                format!("held lock could not be rebound for alias retirement: {source}"),
            )
        })?;
    if owner.bytes != KIT_ADVISORY_LOCK_CONTENT || owner.observation != alias.observation {
        return Err(invalid_world(
            context,
            "held lock and bootstrap alias are not the same exact two-link authority",
        ));
    }
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(&alias.name);
    runtime.observe(before);
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
                format!("held-lock lifecycle alias retirement requires recovery: {source}"),
            )
        })?;
    sync_kit(context, lock, runtime, kit, 1)?;
    runtime.observe(after);
    Ok(())
}

fn discover(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
) -> Result<Discovery, CodegenError> {
    let kit = match context.open_directory(KIT_LOGICAL_PATH) {
        Ok(kit) => kit,
        Err(CodegenError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Discovery {
                kit: context.open_pinned_project_root()?,
                alias: None,
                bootstrap: None,
                canonical: None,
            });
        }
        Err(error) => return Err(error),
    };
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
                format!("could not capture the bounded coordination inventory: {source}"),
            )
        })?;

    let mut alias = None;
    let mut bootstrap = None;
    let mut canonical = None;
    for entry in &inventory.entries {
        let Some(name) = entry.name.to_str() else {
            if entry.name.to_string_lossy().starts_with(BOOTSTRAP_PREFIX) {
                return Err(invalid_world(
                    context,
                    "namespace lifecycle entry has a non-UTF-8 name",
                ));
            }
            continue;
        };
        if name == CANONICAL_NAMESPACE_NAME {
            if entry.kind != ExactDirectoryEntryKind::Directory || canonical.is_some() {
                return Err(invalid_world(
                    context,
                    "canonical transaction namespace has the wrong kind or is duplicated",
                ));
            }
            canonical = Some(
                runtime
                    .fs()
                    .open_directory_exact(&kit, Path::new(name), &kit_path.join(name), 0o700)
                    .map_err(|source| {
                        recovery_required(
                            kit_path.join(name),
                            format!("canonical namespace could not be opened exactly: {source}"),
                        )
                    })?,
            );
            continue;
        }
        if !name.starts_with(BOOTSTRAP_PREFIX) {
            continue;
        }
        if name.contains(".migration") {
            continue;
        }
        if let Some(transaction_id) = parse_namespace_name(name)? {
            if entry.kind != ExactDirectoryEntryKind::Directory || bootstrap.is_some() {
                return Err(invalid_world(
                    context,
                    "private bootstrap namespace has the wrong kind or is duplicated",
                ));
            }
            let handle = runtime
                .fs()
                .open_directory_exact(&kit, Path::new(name), &kit_path.join(name), 0o700)
                .map_err(|source| {
                    recovery_required(
                        kit_path.join(name),
                        format!(
                            "private bootstrap namespace could not be opened exactly: {source}"
                        ),
                    )
                })?;
            bootstrap = Some(BootstrapNamespace {
                name: name.to_owned(),
                transaction_id,
                handle,
            });
            continue;
        }
        let Some((transaction_id, kind)) = parse_alias_name(name)? else {
            return Err(invalid_world(
                context,
                "unrecognized transaction-namespace bootstrap lifecycle name",
            ));
        };
        if entry.kind != ExactDirectoryEntryKind::RegularFile || alias.is_some() {
            return Err(invalid_world(
                context,
                "bootstrap lifecycle alias has the wrong kind or is duplicated",
            ));
        }
        alias = Some(read_alias(
            context,
            runtime,
            &kit,
            name,
            &transaction_id,
            kind,
        )?);
    }
    Ok(Discovery {
        kit,
        alias,
        bootstrap,
        canonical,
    })
}

pub(super) fn validate_acquisition_alias_if_present(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit: &Dir,
    lock_identity: ObjectIdentity,
) -> Result<bool, CodegenError> {
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
                format!("could not observe namespace-bootstrap authority during lock acquisition: {source}"),
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
                format!("could not inventory namespace-bootstrap authority during lock acquisition: {source}"),
            )
        })?;
    let mut matched = false;
    for entry in inventory.entries {
        let Some(name) = entry.name.to_str() else {
            if entry.name.to_string_lossy().starts_with(BOOTSTRAP_PREFIX) {
                return Err(invalid_world(
                    context,
                    "namespace-bootstrap lifecycle entry has a non-UTF-8 name",
                ));
            }
            continue;
        };
        if !name.starts_with(BOOTSTRAP_PREFIX) || name.contains(".migration") {
            continue;
        }
        if parse_namespace_name(name)?.is_some() {
            continue;
        }
        let Some((_transaction_id, _kind)) = parse_alias_name(name)? else {
            return Err(invalid_world(
                context,
                "unrecognized namespace-bootstrap lifecycle name during lock acquisition",
            ));
        };
        if entry.kind != ExactDirectoryEntryKind::RegularFile || matched {
            return Err(invalid_world(
                context,
                "held lock has duplicated or malformed namespace-bootstrap alias authority",
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
                    format!("could not authenticate namespace-bootstrap alias during lock acquisition: {source}"),
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
                "held lock namespace-bootstrap alias does not match its exact two-link authority",
            ));
        }
        matched = true;
    }
    Ok(matched)
}

fn read_alias(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
    kit: &Dir,
    name: &str,
    transaction_id: &TransactionId,
    kind: AliasKind,
) -> Result<BootstrapAlias, CodegenError> {
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
                format!("bootstrap lifecycle alias could not be read exactly: {source}"),
            )
        })?;
    if read.bytes != KIT_ADVISORY_LOCK_CONTENT
        || read.observation.byte_len != KIT_ADVISORY_LOCK_CONTENT.len() as u64
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
            "bootstrap lifecycle alias is not the exact private two-link lock marker",
        ));
    }
    Ok(BootstrapAlias {
        name: name.to_owned(),
        transaction_id: transaction_id.clone(),
        kind,
        observation: read.observation,
    })
}

fn validate_alias_lock_observation(
    context: &PlanningContext,
    lock: &WriteLock,
    observation: &ExactFileObservation,
) -> Result<(), CodegenError> {
    lock.validate_context_link_count(context, 2)?;
    if observation.identity != lock.identity()
        || observation.byte_len != KIT_ADVISORY_LOCK_CONTENT.len() as u64
        || observation.link_count != Some(2)
    {
        return Err(invalid_world(
            context,
            "bootstrap lifecycle alias does not identify the held write lock",
        ));
    }
    Ok(())
}

fn validate_alias_for_namespace(
    context: &PlanningContext,
    lock: &WriteLock,
    alias: &BootstrapAlias,
    namespace: &ExactDirectoryObservation,
) -> Result<(), CodegenError> {
    validate_alias_lock_observation(context, lock, &alias.observation)?;
    match alias.kind {
        AliasKind::Bound(identity) if identity == namespace.identity => Ok(()),
        AliasKind::Bound(_) => Err(invalid_world(
            context,
            "bound bootstrap alias names a different namespace identity",
        )),
        AliasKind::Armed => Err(invalid_world(
            context,
            "armed bootstrap alias cannot authorize an activated namespace",
        )),
    }
}

fn namespace_intent_is_present(
    context: &PlanningContext,
    _runtime: &TransactionRuntime,
    namespace: &ExactDirectoryHandle,
    transaction_id: &TransactionId,
) -> Result<bool, CodegenError> {
    let name = intent_name(transaction_id);
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(CANONICAL_NAMESPACE_NAME)
        .join(&name);
    match namespace.directory.symlink_metadata(&name) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(recovery_required(
            path,
            "namespace-bootstrap intent has the wrong filesystem kind",
        )),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(recovery_required(
            path,
            format!("namespace-bootstrap intent presence could not be inspected: {source}"),
        )),
    }
}

fn namespace_intent_partial_is_present(
    context: &PlanningContext,
    namespace: &ExactDirectoryHandle,
    transaction_id: &TransactionId,
    namespace_name: &str,
) -> Result<bool, CodegenError> {
    let name = intent_partial_name(transaction_id);
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(namespace_name)
        .join(&name);
    match namespace.directory.symlink_metadata(&name) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(recovery_required(
            path,
            "namespace-bootstrap intent partial has the wrong filesystem kind",
        )),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(recovery_required(
            path,
            format!("namespace-bootstrap partial presence could not be inspected: {source}"),
        )),
    }
}

fn validate_namespace_intent(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    namespace: &ExactDirectoryHandle,
    transaction_id: &TransactionId,
    namespace_name: &str,
) -> Result<(), CodegenError> {
    let name = intent_name(transaction_id);
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(namespace_name)
        .join(&name);
    let read = runtime
        .fs()
        .read_regular_file_exact(
            &namespace.directory,
            Path::new(&name),
            &path,
            MAX_INTENT_BYTES,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("namespace-bootstrap intent could not be read exactly: {source}"),
            )
        })?;
    if read.observation.link_count != Some(1)
        || read.observation.mode.readonly
        || read
            .observation
            .mode
            .posix_mode
            .is_some_and(|mode| mode != 0o600)
    {
        return Err(invalid_world(
            context,
            "namespace-bootstrap intent is not an independent private regular file",
        ));
    }
    let intent: NamespaceBootstrapIntent =
        serde_json::from_slice(&read.bytes).map_err(|source| {
            recovery_required(
                &path,
                format!("namespace-bootstrap intent is not canonical JSON: {source}"),
            )
        })?;
    if intent.canonical_bytes()? != read.bytes {
        return Err(invalid_world(
            context,
            "namespace-bootstrap intent bytes are not canonical",
        ));
    }
    let coordination_parent =
        observe_kit(context, runtime, &context.open_directory(KIT_LOGICAL_PATH)?)?;
    intent.validate(
        context,
        lock,
        transaction_id,
        namespace_name,
        &namespace.observation,
        &coordination_parent,
    )
}

fn workspace_intent_is_present(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
    namespace: &ExactDirectoryHandle,
    transaction_id: &TransactionId,
) -> Result<bool, CodegenError> {
    let name = bootstrap_intent_name(transaction_id);
    let path = context
        .project_root()
        .join(KIT_LOGICAL_PATH)
        .join(CANONICAL_NAMESPACE_NAME)
        .join(&name);
    match namespace.directory.symlink_metadata(&name) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => Err(recovery_required(
            &path,
            "ordinary workspace intent has the wrong filesystem kind",
        ))?,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(recovery_required(
                path,
                format!("ordinary workspace intent presence could not be inspected: {source}"),
            ));
        }
    }
    let read = runtime
        .fs()
        .read_regular_file_exact(
            &namespace.directory,
            Path::new(&name),
            &path,
            MAX_INTENT_BYTES,
        )
        .map_err(|source| {
            recovery_required(
                &path,
                format!("ordinary workspace intent could not be read exactly: {source}"),
            )
        })?;
    if read.observation.link_count != Some(1)
        || read.observation.mode.readonly
        || read
            .observation
            .mode
            .posix_mode
            .is_some_and(|mode| mode != 0o600)
    {
        return Err(invalid_world(
            context,
            "ordinary workspace intent is not an independent private file",
        ));
    }
    let envelope: WorkspaceBootstrapIntentEnvelopeV2 = serde_json::from_slice(&read.bytes)
        .map_err(|source| {
            recovery_required(
                &path,
                format!("ordinary workspace intent is not valid canonical JSON: {source}"),
            )
        })?;
    let canonical = envelope
        .to_json_bytes()
        .map_err(|error| recovery_required(&path, error.to_string()))?;
    let expected_root = canonical_root_hash(&canonical_native_bytes(context.project_root()));
    let namespace_exact = exact_directory(&namespace.observation)
        .map_err(|error| recovery_required(&path, error.to_string()))?;
    if canonical != read.bytes
        || envelope.transaction_id() != transaction_id
        || envelope.canonical_root_hash() != &expected_root
        || envelope.workspace_parent_preimage() != &namespace_exact
    {
        return Err(invalid_world(
            context,
            "ordinary workspace intent does not bind the exact transaction, root, and namespace",
        ));
    }
    Ok(true)
}

fn require_empty(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &str,
    path: &Path,
    namespace: &ExactDirectoryHandle,
) -> Result<(), CodegenError> {
    let inventory = fs
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(parent, Path::new(name), &namespace.directory, path),
            &namespace.observation,
            0,
        )
        .map_err(|source| {
            recovery_required(
                path,
                format!("private bootstrap namespace is not exact-empty: {source}"),
            )
        })?;
    if !inventory.entries.is_empty() {
        return Err(recovery_required(
            path,
            "private bootstrap namespace is not exact-empty",
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
    let mut created = match fs
        .create_new_file(parent, name, path, 0o600)
        .bind_empty(fs, parent, name, path)
    {
        Ok(created) => created,
        Err(ExclusiveCreateFailure::NotCreated(source)) => {
            return Err(recovery_required(
                path,
                format!("namespace-bootstrap intent was not created: {source}"),
            ));
        }
        Err(ExclusiveCreateFailure::CreatedUnverified { created, source }) => {
            let _retained = created;
            return Err(recovery_required(
                path,
                format!(
                    "namespace-bootstrap intent was created but could not be rebound exactly: {source}"
                ),
            ));
        }
    };
    fs.set_file_mode(&created.file, path, 0o600)
        .map_err(|source| {
            recovery_required(path, format!("could not set intent mode: {source}"))
        })?;
    fs.write_handle(&mut created.file, path, bytes)
        .map_err(|source| recovery_required(path, format!("could not write intent: {source}")))?;
    fs.flush_file(&created.file, path)
        .map_err(|source| recovery_required(path, format!("could not flush intent: {source}")))?;
    fs.sync_handle(&created.file, path)
        .map_err(|source| recovery_required(path, format!("could not sync intent: {source}")))?;
    fs.observe_created_file_exact(parent, name, path, &mut created, bytes.len() as u64)
        .map_err(|source| {
            recovery_required(
                path,
                format!("durable namespace-bootstrap intent could not be verified: {source}"),
            )
        })
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
                format!("coordination parent could not be observed exactly: {source}"),
            )
        })
}

fn sync_kit(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    kit: &Dir,
    expected_lock_links: u64,
) -> Result<(), CodegenError> {
    runtime
        .fs()
        .sync_directory(kit, &context.project_root().join(KIT_LOGICAL_PATH))
        .map_err(|source| {
            recovery_required(
                context.project_root().join(KIT_LOGICAL_PATH),
                format!("coordination parent durability requires recovery: {source}"),
            )
        })?;
    lock.validate_context_link_count(context, expected_lock_links)?;
    observe_kit(context, runtime, kit)?;
    Ok(())
}

fn name_is_present(parent: &Dir, name: &Path, path: &Path) -> Result<bool, CodegenError> {
    match parent.symlink_metadata(name) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(recovery_required(
            path,
            format!("lifecycle name presence could not be inspected: {source}"),
        )),
    }
}

fn parse_namespace_name(name: &str) -> Result<Option<TransactionId>, CodegenError> {
    let Some(value) = name
        .strip_prefix(BOOTSTRAP_PREFIX)
        .and_then(|value| value.strip_suffix(NAMESPACE_SUFFIX))
    else {
        return Ok(None);
    };
    TransactionId::parse(value)
        .map(Some)
        .map_err(|error| recovery_required(name, error.to_string()))
}

fn parse_alias_name(name: &str) -> Result<Option<(TransactionId, AliasKind)>, CodegenError> {
    let Some(value) = name.strip_prefix(BOOTSTRAP_PREFIX) else {
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
    let Some((transaction, identity)) = value.split_once('-') else {
        return Err(recovery_required(
            name,
            "bound namespace-bootstrap alias is missing its identity binding",
        ));
    };
    let transaction = TransactionId::parse(transaction)
        .map_err(|error| recovery_required(name, error.to_string()))?;
    let identity = parse_identity_hex(identity)
        .ok_or_else(|| recovery_required(name, "bound alias has an invalid full-width identity"))?;
    Ok(Some((transaction, AliasKind::Bound(identity))))
}

fn armed_alias_name(transaction_id: &TransactionId) -> String {
    format!(
        "{BOOTSTRAP_PREFIX}{}{ARMED_SUFFIX}",
        transaction_id.as_str()
    )
}

fn namespace_name(transaction_id: &TransactionId) -> String {
    format!(
        "{BOOTSTRAP_PREFIX}{}{NAMESPACE_SUFFIX}",
        transaction_id.as_str()
    )
}

fn bound_alias_name(transaction_id: &TransactionId, identity: ObjectIdentity) -> String {
    format!(
        "{BOOTSTRAP_PREFIX}{}-{}{BOUND_SUFFIX}",
        transaction_id.as_str(),
        identity_hex(identity)
    )
}

fn intent_name(transaction_id: &TransactionId) -> String {
    format!("{INTENT_PREFIX}{}{INTENT_SUFFIX}", transaction_id.as_str())
}

fn intent_partial_name(transaction_id: &TransactionId) -> String {
    format!(
        "{INTENT_PREFIX}{}{INTENT_PARTIAL_SUFFIX}",
        transaction_id.as_str()
    )
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
