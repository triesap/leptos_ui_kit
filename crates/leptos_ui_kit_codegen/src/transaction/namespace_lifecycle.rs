//! Exact terminal retirement for the journal-v2 transaction namespace.
//!
//! The final workspace-removed lease is retained as a hard-link authority
//! outside the namespace until the namespace itself has been durably removed.
//! Every recovery pass performs at most one semantic mutation.

use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

use cap_std::fs::Dir;

use crate::CodegenError;
use crate::path_safety::PlanningContext;

use super::fs::{
    DirectoryEndpoint, ExactDirectoryEntry, ExactDirectoryEntryKind, ExactDirectoryHandle,
    ExactDirectoryObservation, ExactFileObservation, ExactFileRead, ExactRelocationSource, FsOps,
    HardLinkEndpoint, ParentSyncKind,
};
use super::journal::{
    FinalizationLeaseV2, FinalizationStateV2, Sha256Digest, TransactionId, canonical_root_hash,
};
use super::lock::WriteLock;
use super::runtime::{TransactionRuntime, TransitionKey, TransitionWindow};
use super::store::{FinalizationRecord, MAX_RECORD_ENVELOPE_BYTES, exact_directory_observation};
use super::writer::{canonical_native_bytes, model_error_at, transaction_io};

const KIT_PARENT_LOGICAL_PATH: &str = "src/components/ui/_kit";
const KIT_GRANDPARENT_LOGICAL_PATH: &str = "src/components/ui";
const CANONICAL_NAMESPACE_NAME: &str = ".transactions";
const RETIREMENT_PREFIX: &str = ".transactions.retirement-v2-";
const RETIREMENT_AUTHORITY_SUFFIX: &str = ".authority";
const RETIREMENT_NAMESPACE_SUFFIX: &str = ".namespace";
const MAX_COORDINATION_ENTRIES: usize = 16_384;
const MAX_RETIREMENT_STEPS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NamespaceRetirementStep {
    NotPresent,
    DurableProgress,
}

#[derive(Debug)]
struct RetirementAuthority {
    transaction_id: TransactionId,
    lease: FinalizationLeaseV2,
    read: ExactFileRead,
    authority_name: String,
    namespace_name: String,
}

#[derive(Debug)]
struct RetirementDiscovery {
    kit: Dir,
    authority: RetirementAuthority,
    canonical: Option<ExactDirectoryHandle>,
    retiring: Option<ExactDirectoryHandle>,
}

pub(super) fn check_retirement_pending(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
) -> Result<(), CodegenError> {
    if discover(context, runtime)?.is_some() {
        return Err(recovery_required(
            context.project_root().join(KIT_PARENT_LOGICAL_PATH),
            "a terminal journal-v2 namespace retirement requires the mutating recovery path",
        ));
    }
    Ok(())
}

pub(super) fn retire_terminal_namespace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    terminal: &FinalizationRecord,
) -> Result<(), CodegenError> {
    arm_retirement_authority(context, lock, runtime, terminal)?;
    for _ in 0..MAX_RETIREMENT_STEPS {
        match recover_retirement_step(context, lock, runtime)? {
            NamespaceRetirementStep::NotPresent => return Ok(()),
            NamespaceRetirementStep::DurableProgress => {}
        }
    }
    Err(recovery_required(
        context.project_root().join(KIT_PARENT_LOGICAL_PATH),
        format!(
            "terminal namespace retirement exceeded its bounded {MAX_RETIREMENT_STEPS}-mutation progress budget"
        ),
    ))
}

pub(super) fn arm_retirement_authority(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    terminal: &FinalizationRecord,
) -> Result<(), CodegenError> {
    lock.validate_context(context)?;
    if terminal.lease().state() != FinalizationStateV2::WorkspaceRemoved
        || terminal.lease().generation() != 1
        || terminal.name() != terminal.lease().record_name()
        || terminal.lease().canonical_root_hash()
            != &canonical_root_hash(&canonical_native_bytes(context.project_root()))
    {
        return Err(recovery_required(
            context.project_root().join(KIT_PARENT_LOGICAL_PATH),
            "namespace retirement requires the exact canonical generation-one workspace-removed lease",
        ));
    }
    if discover(context, runtime)?.is_some() {
        return Err(recovery_required(
            context.project_root().join(KIT_PARENT_LOGICAL_PATH),
            "namespace retirement authority already exists before the terminal lease was armed",
        ));
    }

    let kit_parent = context.open_directory(KIT_PARENT_LOGICAL_PATH)?;
    let kit_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    let namespace_path = kit_path.join(CANONICAL_NAMESPACE_NAME);
    let expected_namespace =
        exact_directory_observation(terminal.lease().workspace_parent_current(), None);
    let namespace = open_bound_directory(
        runtime.fs(),
        &kit_parent,
        Path::new(CANONICAL_NAMESPACE_NAME),
        &namespace_path,
        &expected_namespace,
    )?;
    let inventory = runtime
        .fs()
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(
                &kit_parent,
                Path::new(CANONICAL_NAMESPACE_NAME),
                &namespace.directory,
                &namespace_path,
            ),
            &namespace.observation,
            1,
        )
        .map_err(|source| {
            transaction_io(
                "inventory terminal transaction namespace",
                KIT_PARENT_LOGICAL_PATH,
                &namespace_path,
                source,
            )
        })?;
    if inventory.entries.len() != 1
        || inventory.entries[0].name != OsStr::new(terminal.name())
        || inventory.entries[0].kind != ExactDirectoryEntryKind::RegularFile
    {
        return Err(recovery_required(
            &namespace_path,
            "terminal transaction namespace is not the exact lone finalization-lease world",
        ));
    }
    validate_entry_observation(
        &inventory.entries[0],
        terminal.observation(),
        &namespace_path,
    )?;
    require_private_lease(terminal.observation(), &namespace_path)?;
    if terminal.observation().link_count != Some(1) {
        return Err(recovery_required(
            &namespace_path,
            "terminal finalization lease must have one link before retirement is armed",
        ));
    }

    let authority_name = retirement_authority_name(terminal.lease().transaction_id());
    let authority_path = kit_path.join(&authority_name);
    runtime.observe(TransitionKey::ArmNamespaceRetirementAuthority {
        window: TransitionWindow::Before,
    });
    let link_result = runtime.fs().hard_link(
        &[],
        HardLinkEndpoint::new(
            &namespace.directory,
            Path::new(terminal.name()),
            &namespace_path.join(terminal.name()),
        ),
        HardLinkEndpoint::new(&kit_parent, Path::new(&authority_name), &authority_path),
    );
    lock.validate_context(context)?;
    let linked = runtime
        .fs()
        .read_regular_file_exact(
            &kit_parent,
            Path::new(&authority_name),
            &authority_path,
            MAX_RECORD_ENVELOPE_BYTES,
        )
        .map_err(|source| {
            if let Err(link_source) = link_result {
                recovery_required(
                    &authority_path,
                    format!(
                        "retirement authority link failed ({link_source}) and its exact visible outcome could not be authenticated: {source}"
                    ),
                )
            } else {
                transaction_io(
                    "authenticate retirement authority",
                    KIT_PARENT_LOGICAL_PATH,
                    &authority_path,
                    source,
                )
            }
        })?;
    let internal_path = namespace_path.join(terminal.name());
    let internal = runtime
        .fs()
        .read_regular_file_exact(
            &namespace.directory,
            Path::new(terminal.name()),
            &internal_path,
            MAX_RECORD_ENVELOPE_BYTES,
        )
        .map_err(|source| {
            transaction_io(
                "authenticate internal retirement alias",
                KIT_PARENT_LOGICAL_PATH,
                &internal_path,
                source,
            )
        })?;
    validate_armed_source(terminal.observation(), &internal, &internal_path)?;
    validate_linked_alias(terminal.lease(), &internal, &linked, &authority_path)?;
    sync_kit(context, runtime, &kit_parent)?;
    runtime.observe(TransitionKey::ArmNamespaceRetirementAuthority {
        window: TransitionWindow::After,
    });
    Ok(())
}

pub(super) fn recover_retirement_step(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
) -> Result<NamespaceRetirementStep, CodegenError> {
    lock.validate_context(context)?;
    let Some(discovery) = discover(context, runtime)? else {
        sync_kit_if_present(context, runtime)?;
        return Ok(NamespaceRetirementStep::NotPresent);
    };
    sync_kit(context, runtime, &discovery.kit)?;
    lock.validate_context(context)?;

    match (&discovery.canonical, &discovery.retiring) {
        (Some(_), Some(_)) => Err(recovery_required(
            context.project_root().join(KIT_PARENT_LOGICAL_PATH),
            "canonical and retiring transaction namespaces coexist",
        )),
        (Some(canonical), None) => move_namespace(context, lock, runtime, &discovery, canonical),
        (None, Some(retiring)) => {
            retire_namespace_contents(context, lock, runtime, &discovery, retiring)
        }
        (None, None) => retire_external_authority(context, lock, runtime, &discovery),
    }
}

fn move_namespace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    discovery: &RetirementDiscovery,
    canonical: &ExactDirectoryHandle,
) -> Result<NamespaceRetirementStep, CodegenError> {
    validate_namespace_binding(
        canonical,
        discovery.authority.lease.workspace_parent_current(),
        &context
            .project_root()
            .join(KIT_PARENT_LOGICAL_PATH)
            .join(CANONICAL_NAMESPACE_NAME),
    )?;
    validate_internal_lease(
        runtime,
        &discovery.kit,
        CANONICAL_NAMESPACE_NAME,
        canonical,
        &discovery.authority,
        &context
            .project_root()
            .join(KIT_PARENT_LOGICAL_PATH)
            .join(CANONICAL_NAMESPACE_NAME),
    )?;

    let kit_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    let canonical_path = kit_path.join(CANONICAL_NAMESPACE_NAME);
    let retiring_path = kit_path.join(&discovery.authority.namespace_name);
    runtime.observe(TransitionKey::MoveTransactionNamespaceToRetirement {
        window: TransitionWindow::Before,
    });
    let result = runtime.fs().relocate_noreplace(
        &discovery.kit,
        Path::new(CANONICAL_NAMESPACE_NAME),
        &canonical_path,
        &discovery.kit,
        Path::new(&discovery.authority.namespace_name),
        &retiring_path,
        &ExactRelocationSource::Directory(canonical.observation),
    );
    lock.validate_context(context)?;
    let moved = verify_namespace_moved(
        runtime,
        &discovery.kit,
        &canonical_path,
        &retiring_path,
        &discovery.authority.namespace_name,
        &canonical.observation,
    );
    if let Err(source) = moved {
        return Err(match result {
            Err(relocation) => recovery_required(
                &retiring_path,
                format!(
                    "no-replace namespace retirement failed ({}) and its visible outcome could not be authenticated: {source}",
                    relocation.into_io()
                ),
            ),
            Ok(()) => recovery_required(
                &retiring_path,
                format!(
                    "namespace retirement reported success but its exact outcome could not be authenticated: {source}"
                ),
            ),
        });
    }
    sync_kit(context, runtime, &discovery.kit)?;
    runtime.observe(TransitionKey::MoveTransactionNamespaceToRetirement {
        window: TransitionWindow::After,
    });
    Ok(NamespaceRetirementStep::DurableProgress)
}

fn retire_namespace_contents(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    discovery: &RetirementDiscovery,
    retiring: &ExactDirectoryHandle,
) -> Result<NamespaceRetirementStep, CodegenError> {
    let kit_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    let retiring_path = kit_path.join(&discovery.authority.namespace_name);
    validate_namespace_binding(
        retiring,
        discovery.authority.lease.workspace_parent_current(),
        &retiring_path,
    )?;
    sync_directory(
        runtime,
        DirectoryEndpoint::new(
            &discovery.kit,
            Path::new(&discovery.authority.namespace_name),
            &retiring.directory,
            &retiring_path,
        ),
    )?;
    let inventory = runtime
        .fs()
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(
                &discovery.kit,
                Path::new(&discovery.authority.namespace_name),
                &retiring.directory,
                &retiring_path,
            ),
            &retiring.observation,
            1,
        )
        .map_err(|source| {
            transaction_io(
                "inventory retiring transaction namespace",
                KIT_PARENT_LOGICAL_PATH,
                &retiring_path,
                source,
            )
        })?;
    match inventory.entries.as_slice() {
        [entry] => {
            if entry.name != OsStr::new(&discovery.authority.lease.record_name())
                || entry.kind != ExactDirectoryEntryKind::RegularFile
            {
                return Err(recovery_required(
                    &retiring_path,
                    "retiring transaction namespace contains an unrecognized object",
                ));
            }
            remove_internal_lease(context, lock, runtime, discovery, retiring, entry)
        }
        [] => retire_empty_namespace(context, lock, runtime, discovery, retiring),
        _ => Err(recovery_required(
            &retiring_path,
            "retiring transaction namespace is not exact-empty or the lone terminal-lease world",
        )),
    }
}

fn remove_internal_lease(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    discovery: &RetirementDiscovery,
    retiring: &ExactDirectoryHandle,
    entry: &ExactDirectoryEntry,
) -> Result<NamespaceRetirementStep, CodegenError> {
    let kit_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    let retiring_path = kit_path.join(&discovery.authority.namespace_name);
    let internal_name = discovery.authority.lease.record_name();
    let internal_path = retiring_path.join(&internal_name);
    let internal = runtime
        .fs()
        .read_regular_file_exact(
            &retiring.directory,
            Path::new(&internal_name),
            &internal_path,
            MAX_RECORD_ENVELOPE_BYTES,
        )
        .map_err(|source| {
            transaction_io(
                "authenticate internal finalization lease",
                KIT_PARENT_LOGICAL_PATH,
                &internal_path,
                source,
            )
        })?;
    validate_entry_observation(entry, &internal.observation, &internal_path)?;
    validate_linked_alias(
        &discovery.authority.lease,
        &internal,
        &discovery.authority.read,
        &internal_path,
    )?;
    runtime.observe(TransitionKey::RemoveInternalFinalizationLease {
        window: TransitionWindow::Before,
    });
    let removal = runtime.fs().remove_file_exact(
        &retiring.directory,
        Path::new(&internal_name),
        &internal_path,
        &internal.observation,
    );
    lock.validate_context(context)?;
    if let Err(error) = removal {
        let completed = error.mutation_may_have_completed()
            && name_is_absent(
                &retiring.directory,
                Path::new(&internal_name),
                &internal_path,
            )?;
        if !completed {
            return Err(recovery_required(
                &internal_path,
                format!("could not exact-remove the internal finalization lease: {error}"),
            ));
        }
    }
    let alias = read_and_validate_authority(
        runtime,
        &discovery.kit,
        &kit_path,
        &discovery.authority.authority_name,
        &discovery.authority.transaction_id,
        discovery.authority.lease.canonical_root_hash(),
    )?;
    if alias.lease != discovery.authority.lease
        || alias.read.observation.identity != discovery.authority.read.observation.identity
        || alias.read.observation.byte_len != discovery.authority.read.observation.byte_len
        || alias.read.observation.content_hash != discovery.authority.read.observation.content_hash
        || alias.read.observation.mode != discovery.authority.read.observation.mode
        || alias.read.observation.link_count != Some(1)
    {
        return Err(recovery_required(
            &internal_path,
            "internal lease removal did not leave the exact one-link external authority",
        ));
    }
    sync_directory(
        runtime,
        DirectoryEndpoint::new(
            &discovery.kit,
            Path::new(&discovery.authority.namespace_name),
            &retiring.directory,
            &retiring_path,
        ),
    )?;
    runtime.observe(TransitionKey::RemoveInternalFinalizationLease {
        window: TransitionWindow::After,
    });
    Ok(NamespaceRetirementStep::DurableProgress)
}

fn retire_empty_namespace(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    discovery: &RetirementDiscovery,
    retiring: &ExactDirectoryHandle,
) -> Result<NamespaceRetirementStep, CodegenError> {
    if discovery.authority.read.observation.link_count != Some(1) {
        return Err(recovery_required(
            context.project_root().join(KIT_PARENT_LOGICAL_PATH),
            "an exact-empty retiring namespace requires a one-link external authority",
        ));
    }
    let kit_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    let retiring_path = kit_path.join(&discovery.authority.namespace_name);
    runtime.observe(TransitionKey::RetireTransactionNamespace {
        window: TransitionWindow::Before,
    });
    let removal = runtime.fs().remove_empty_directory_exact(
        DirectoryEndpoint::new(
            &discovery.kit,
            Path::new(&discovery.authority.namespace_name),
            &retiring.directory,
            &retiring_path,
        ),
        &retiring.observation,
    );
    lock.validate_context(context)?;
    if let Err(error) = removal {
        let completed = error.mutation_may_have_completed()
            && name_is_absent(
                &discovery.kit,
                Path::new(&discovery.authority.namespace_name),
                &retiring_path,
            )?;
        if !completed {
            return Err(recovery_required(
                &retiring_path,
                format!("could not exact-remove the retiring transaction namespace: {error}"),
            ));
        }
    }
    sync_kit(context, runtime, &discovery.kit)?;
    runtime.observe(TransitionKey::RetireTransactionNamespace {
        window: TransitionWindow::After,
    });
    Ok(NamespaceRetirementStep::DurableProgress)
}

fn retire_external_authority(
    context: &PlanningContext,
    lock: &WriteLock,
    runtime: &TransactionRuntime,
    discovery: &RetirementDiscovery,
) -> Result<NamespaceRetirementStep, CodegenError> {
    if discovery.authority.read.observation.link_count != Some(1) {
        return Err(recovery_required(
            context.project_root().join(KIT_PARENT_LOGICAL_PATH),
            "terminal retirement authority is not the exact final one-link owner",
        ));
    }
    let kit_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    let authority_path = kit_path.join(&discovery.authority.authority_name);
    runtime.observe(TransitionKey::RetireNamespaceRetirementAuthority {
        window: TransitionWindow::Before,
    });
    let removal = runtime.fs().remove_file_exact(
        &discovery.kit,
        Path::new(&discovery.authority.authority_name),
        &authority_path,
        &discovery.authority.read.observation,
    );
    lock.validate_context(context)?;
    if let Err(error) = removal {
        let completed = error.mutation_may_have_completed()
            && name_is_absent(
                &discovery.kit,
                Path::new(&discovery.authority.authority_name),
                &authority_path,
            )?;
        if !completed {
            return Err(recovery_required(
                &authority_path,
                format!("could not exact-remove the terminal retirement authority: {error}"),
            ));
        }
    }
    sync_kit(context, runtime, &discovery.kit)?;
    runtime.observe(TransitionKey::RetireNamespaceRetirementAuthority {
        window: TransitionWindow::After,
    });
    Ok(NamespaceRetirementStep::DurableProgress)
}

fn discover(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
) -> Result<Option<RetirementDiscovery>, CodegenError> {
    let kit_parent = match context.open_directory(KIT_PARENT_LOGICAL_PATH) {
        Ok(directory) => directory,
        Err(CodegenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let kit_grandparent = context.open_directory(KIT_GRANDPARENT_LOGICAL_PATH)?;
    let kit_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    let kit_observation = runtime
        .fs()
        .observe_directory(DirectoryEndpoint::new(
            &kit_grandparent,
            Path::new("_kit"),
            &kit_parent,
            &kit_path,
        ))
        .map_err(|source| {
            transaction_io(
                "inspect coordination directory",
                KIT_PARENT_LOGICAL_PATH,
                &kit_path,
                source,
            )
        })?;
    let inventory = runtime
        .fs()
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(&kit_grandparent, Path::new("_kit"), &kit_parent, &kit_path),
            &kit_observation,
            MAX_COORDINATION_ENTRIES,
        )
        .map_err(|source| {
            transaction_io(
                "inventory coordination directory",
                KIT_PARENT_LOGICAL_PATH,
                &kit_path,
                source,
            )
        })?;

    let mut authority_entry = None;
    let mut namespace_entry = None;
    let mut canonical_entry = None;
    for entry in &inventory.entries {
        if entry.name == OsStr::new(CANONICAL_NAMESPACE_NAME) {
            canonical_entry = Some(entry);
            continue;
        }
        let Some(name) = entry.name.to_str() else {
            if entry.name.to_string_lossy().starts_with(".transactions.") {
                return Err(recovery_required(
                    &kit_path,
                    "reserved transaction lifecycle name is not valid UTF-8",
                ));
            }
            continue;
        };
        if !name.starts_with(RETIREMENT_PREFIX) {
            continue;
        }
        if parse_retirement_name(name, RETIREMENT_AUTHORITY_SUFFIX).is_ok() {
            if authority_entry.replace(entry).is_some() {
                return Err(recovery_required(
                    &kit_path,
                    "multiple terminal retirement authorities coexist",
                ));
            }
        } else if parse_retirement_name(name, RETIREMENT_NAMESPACE_SUFFIX).is_ok() {
            if namespace_entry.replace(entry).is_some() {
                return Err(recovery_required(
                    &kit_path,
                    "multiple retiring transaction namespaces coexist",
                ));
            }
        } else {
            return Err(recovery_required(
                kit_path.join(name),
                "malformed reserved terminal-retirement name",
            ));
        }
    }
    if authority_entry.is_none() && namespace_entry.is_none() {
        return Ok(None);
    }
    let authority_entry = authority_entry.ok_or_else(|| {
        recovery_required(
            &kit_path,
            "retiring transaction namespace exists without its external lease authority",
        )
    })?;
    if authority_entry.kind != ExactDirectoryEntryKind::RegularFile {
        return Err(recovery_required(
            kit_path.join(&authority_entry.name),
            "terminal retirement authority is not a regular file",
        ));
    }
    let authority_name = authority_entry
        .name
        .to_str()
        .expect("validated lifecycle names are UTF-8");
    let transaction_id = parse_retirement_name(authority_name, RETIREMENT_AUTHORITY_SUFFIX)
        .map_err(model_error_at(kit_path.join(authority_name)))?;
    let authority = read_and_validate_authority(
        runtime,
        &kit_parent,
        &kit_path,
        authority_name,
        &transaction_id,
        &canonical_root_hash(&canonical_native_bytes(context.project_root())),
    )?;
    validate_entry_observation(
        authority_entry,
        &authority.read.observation,
        &kit_path.join(authority_name),
    )?;

    let namespace_name = retirement_namespace_name(&transaction_id);
    let retiring = match namespace_entry {
        Some(entry) => {
            if entry.name != OsStr::new(&namespace_name)
                || entry.kind != ExactDirectoryEntryKind::Directory
            {
                return Err(recovery_required(
                    kit_path.join(&entry.name),
                    "retiring namespace does not match its external transaction authority",
                ));
            }
            let opened = runtime
                .fs()
                .open_directory_exact(
                    &kit_parent,
                    Path::new(&namespace_name),
                    &kit_path.join(&namespace_name),
                    entry.mode.posix_mode.unwrap_or(0o700),
                )
                .map_err(|source| {
                    transaction_io(
                        "open retiring transaction namespace",
                        KIT_PARENT_LOGICAL_PATH,
                        &kit_path.join(&namespace_name),
                        source,
                    )
                })?;
            validate_directory_entry(entry, &opened.observation, &kit_path.join(&namespace_name))?;
            Some(opened)
        }
        None => None,
    };

    let canonical = match canonical_entry {
        Some(entry) => {
            if entry.kind != ExactDirectoryEntryKind::Directory {
                return Err(recovery_required(
                    kit_path.join(CANONICAL_NAMESPACE_NAME),
                    "canonical transaction namespace is not a directory",
                ));
            }
            let opened = runtime
                .fs()
                .open_directory_exact(
                    &kit_parent,
                    Path::new(CANONICAL_NAMESPACE_NAME),
                    &kit_path.join(CANONICAL_NAMESPACE_NAME),
                    entry.mode.posix_mode.unwrap_or(0o700),
                )
                .map_err(|source| {
                    transaction_io(
                        "open canonical transaction namespace",
                        KIT_PARENT_LOGICAL_PATH,
                        &kit_path.join(CANONICAL_NAMESPACE_NAME),
                        source,
                    )
                })?;
            validate_directory_entry(
                entry,
                &opened.observation,
                &kit_path.join(CANONICAL_NAMESPACE_NAME),
            )?;
            Some(opened)
        }
        None => None,
    };

    Ok(Some(RetirementDiscovery {
        kit: kit_parent,
        authority,
        canonical,
        retiring,
    }))
}

fn read_and_validate_authority(
    runtime: &TransactionRuntime,
    kit: &Dir,
    kit_path: &Path,
    authority_name: &str,
    expected_transaction: &TransactionId,
    expected_root: &Sha256Digest,
) -> Result<RetirementAuthority, CodegenError> {
    let authority_path = kit_path.join(authority_name);
    let read = runtime
        .fs()
        .read_regular_file_exact(
            kit,
            Path::new(authority_name),
            &authority_path,
            MAX_RECORD_ENVELOPE_BYTES,
        )
        .map_err(|source| {
            transaction_io(
                "read terminal retirement authority",
                KIT_PARENT_LOGICAL_PATH,
                &authority_path,
                source,
            )
        })?;
    require_private_lease(&read.observation, &authority_path)?;
    let lease = FinalizationLeaseV2::from_json_slice(&read.bytes)
        .map_err(model_error_at(&authority_path))?;
    if lease
        .to_json_bytes()
        .map_err(model_error_at(&authority_path))?
        != read.bytes
    {
        return Err(recovery_required(
            &authority_path,
            "terminal retirement authority is not the canonical finalization-lease encoding",
        ));
    }
    if lease.transaction_id() != expected_transaction
        || lease.canonical_root_hash() != expected_root
        || lease.state() != FinalizationStateV2::WorkspaceRemoved
        || lease.generation() != 1
    {
        return Err(recovery_required(
            &authority_path,
            "terminal retirement authority does not bind the canonical generation-one workspace-removed lease",
        ));
    }
    Ok(RetirementAuthority {
        transaction_id: expected_transaction.clone(),
        lease,
        read,
        authority_name: authority_name.to_owned(),
        namespace_name: retirement_namespace_name(expected_transaction),
    })
}

fn validate_internal_lease(
    runtime: &TransactionRuntime,
    parent: &Dir,
    namespace_name: &str,
    namespace: &ExactDirectoryHandle,
    authority: &RetirementAuthority,
    namespace_path: &Path,
) -> Result<(), CodegenError> {
    let internal_name = authority.lease.record_name();
    let inventory = runtime
        .fs()
        .inventory_directory_exact_bounded(
            DirectoryEndpoint::new(
                parent,
                Path::new(namespace_name),
                &namespace.directory,
                namespace_path,
            ),
            &namespace.observation,
            1,
        )
        .map_err(|source| {
            transaction_io(
                "inventory terminal transaction namespace",
                KIT_PARENT_LOGICAL_PATH,
                namespace_path,
                source,
            )
        })?;
    if inventory.entries.len() != 1
        || inventory.entries[0].name != OsStr::new(&internal_name)
        || inventory.entries[0].kind != ExactDirectoryEntryKind::RegularFile
    {
        return Err(recovery_required(
            namespace_path,
            "transaction namespace is not the exact lone terminal-lease world",
        ));
    }
    let internal_path = namespace_path.join(&internal_name);
    let internal = runtime
        .fs()
        .read_regular_file_exact(
            &namespace.directory,
            Path::new(&internal_name),
            &internal_path,
            MAX_RECORD_ENVELOPE_BYTES,
        )
        .map_err(|source| {
            transaction_io(
                "authenticate internal finalization lease",
                KIT_PARENT_LOGICAL_PATH,
                &internal_path,
                source,
            )
        })?;
    validate_entry_observation(&inventory.entries[0], &internal.observation, &internal_path)?;
    validate_linked_alias(&authority.lease, &internal, &authority.read, &internal_path)
}

fn validate_linked_alias(
    lease: &FinalizationLeaseV2,
    internal: &ExactFileRead,
    external: &ExactFileRead,
    path: &Path,
) -> Result<(), CodegenError> {
    let canonical = lease.to_json_bytes().map_err(model_error_at(path))?;
    if internal.bytes != canonical
        || external.bytes != canonical
        || internal.observation.identity != external.observation.identity
        || internal.observation.byte_len != external.observation.byte_len
        || internal.observation.content_hash != external.observation.content_hash
        || internal.observation.mode != external.observation.mode
        || internal.observation.link_count != Some(2)
        || external.observation.link_count != Some(2)
    {
        return Err(recovery_required(
            path,
            "internal and external finalization leases are not exact authenticated two-link aliases",
        ));
    }
    require_private_lease(&internal.observation, path)
}

fn validate_armed_source(
    expected: &ExactFileObservation,
    actual: &ExactFileRead,
    path: &Path,
) -> Result<(), CodegenError> {
    if expected.link_count != Some(1)
        || actual.observation.identity != expected.identity
        || actual.observation.byte_len != expected.byte_len
        || actual.observation.content_hash != expected.content_hash
        || actual.observation.mode != expected.mode
        || actual.observation.link_count != Some(2)
    {
        return Err(recovery_required(
            path,
            "the terminal finalization source changed while retirement authority was armed",
        ));
    }
    Ok(())
}

fn verify_namespace_moved(
    runtime: &TransactionRuntime,
    kit: &Dir,
    canonical_path: &Path,
    retiring_path: &Path,
    retiring_name: &str,
    expected: &ExactDirectoryObservation,
) -> Result<(), io::Error> {
    match kit.symlink_metadata(CANONICAL_NAMESPACE_NAME) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} remains present", canonical_path.display()),
            ));
        }
    }
    let opened = runtime.fs().open_directory_exact(
        kit,
        Path::new(retiring_name),
        retiring_path,
        expected.mode.posix_mode.unwrap_or(0o700),
    )?;
    if opened.observation != *expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retiring namespace is not the exact moved directory",
        ));
    }
    Ok(())
}

fn open_bound_directory(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &Path,
    path: &Path,
    expected: &ExactDirectoryObservation,
) -> Result<ExactDirectoryHandle, CodegenError> {
    let opened = fs
        .open_directory_exact(
            parent,
            name,
            path,
            expected.mode.posix_mode.unwrap_or(0o700),
        )
        .map_err(|source| transaction_io("open exact directory", ".", path, source))?;
    if opened.observation.identity != expected.identity || opened.observation.mode != expected.mode
    {
        return Err(recovery_required(
            path,
            "directory does not match the exact terminal lifecycle authority",
        ));
    }
    Ok(opened)
}

fn validate_namespace_binding(
    actual: &ExactDirectoryHandle,
    expected: &super::journal::ExactDirectoryStateV2,
    path: &Path,
) -> Result<(), CodegenError> {
    let expected = exact_directory_observation(expected, actual.observation.link_count);
    if actual.observation != expected {
        return Err(recovery_required(
            path,
            "transaction namespace identity or mode changed during terminal retirement",
        ));
    }
    Ok(())
}

fn validate_entry_observation(
    entry: &ExactDirectoryEntry,
    observation: &ExactFileObservation,
    path: &Path,
) -> Result<(), CodegenError> {
    if entry.identity != observation.identity
        || entry.byte_len != observation.byte_len
        || entry.mode != observation.mode
        || entry.link_count != observation.link_count
    {
        return Err(recovery_required(
            path,
            "regular file changed between exact parent inventory and capability read",
        ));
    }
    Ok(())
}

fn validate_directory_entry(
    entry: &ExactDirectoryEntry,
    observation: &ExactDirectoryObservation,
    path: &Path,
) -> Result<(), CodegenError> {
    if entry.identity != observation.identity
        || entry.mode != observation.mode
        || entry.link_count != observation.link_count
    {
        return Err(recovery_required(
            path,
            "directory changed between exact parent inventory and capability open",
        ));
    }
    Ok(())
}

fn require_private_lease(
    observation: &ExactFileObservation,
    path: &Path,
) -> Result<(), CodegenError> {
    if observation.mode.readonly || cfg!(unix) && observation.mode.posix_mode != Some(0o600) {
        return Err(recovery_required(
            path,
            "terminal finalization lease does not have the exact private writable mode",
        ));
    }
    Ok(())
}

fn sync_kit(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
    kit: &Dir,
) -> Result<(), CodegenError> {
    let kit_parent = context.open_directory(KIT_GRANDPARENT_LOGICAL_PATH)?;
    let kit_path = context.project_root().join(KIT_PARENT_LOGICAL_PATH);
    sync_directory(
        runtime,
        DirectoryEndpoint::new(&kit_parent, Path::new("_kit"), kit, &kit_path),
    )
}

fn sync_kit_if_present(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
) -> Result<(), CodegenError> {
    match context.open_directory(KIT_PARENT_LOGICAL_PATH) {
        Ok(kit) => sync_kit(context, runtime, &kit),
        Err(CodegenError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sync_directory(
    runtime: &TransactionRuntime,
    endpoint: DirectoryEndpoint<'_>,
) -> Result<(), CodegenError> {
    let observation = runtime
        .fs()
        .observe_directory(endpoint)
        .map_err(|source| transaction_io("inspect directory", ".", endpoint.path, source))?;
    runtime
        .fs()
        .sync_parent(endpoint, &observation, ParentSyncKind::Journal)
        .map_err(|source| transaction_io("sync directory", ".", endpoint.path, source))
}

fn name_is_absent(parent: &Dir, name: &Path, path: &Path) -> Result<bool, CodegenError> {
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Ok(_) => Ok(false),
        Err(source) => Err(recovery_required(
            path,
            format!("could not classify the post-removal lifecycle name: {source}"),
        )),
    }
}

fn parse_retirement_name(
    name: &str,
    suffix: &str,
) -> Result<TransactionId, super::journal::JournalModelError> {
    let transaction = name
        .strip_prefix(RETIREMENT_PREFIX)
        .and_then(|rest| rest.strip_suffix(suffix))
        .ok_or_else(|| {
            super::journal::JournalModelError::new("noncanonical terminal-retirement name")
        })?;
    TransactionId::parse(transaction)
}

fn retirement_authority_name(transaction_id: &TransactionId) -> String {
    format!(
        "{RETIREMENT_PREFIX}{}{RETIREMENT_AUTHORITY_SUFFIX}",
        transaction_id.as_str()
    )
}

fn retirement_namespace_name(transaction_id: &TransactionId) -> String {
    format!(
        "{RETIREMENT_PREFIX}{}{RETIREMENT_NAMESPACE_SUFFIX}",
        transaction_id.as_str()
    )
}

fn recovery_required(path: impl Into<PathBuf>, reason: impl Into<String>) -> CodegenError {
    CodegenError::RecoveryRequired {
        journal_path: path.into(),
        reason: reason.into(),
    }
}
