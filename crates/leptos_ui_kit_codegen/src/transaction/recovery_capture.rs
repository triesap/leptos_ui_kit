#![forbid(unsafe_code)]

//! Capability-relative, read-only capture for journal-v2 recovery policy.
//!
//! Every application target, stage, backup, logical directory, and private
//! directory candidate is observed through `TransactionRuntime::fs()`. Every
//! present transaction-created directory is inventoried exactly. Two complete
//! captures must compare equal before policy may authorize the transaction
//! engine. This module performs no filesystem mutation and parses no journal
//! namespace names.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};

use cap_std::fs::Dir;

use crate::{CodegenError, path_safety::PlanningContext};

use super::fs::{
    DirectoryEndpoint, ExactDirectoryEntry, ExactDirectoryEntryKind, ExactDirectoryObservation,
    ExactFileMetadataObservation, ExactFileObservation, FsOps,
};
use super::journal::{
    DirectoryDispositionV2, ExactDirectoryMetadataV2, ExactFileMetadataV2, ExactFileStateV2,
    JournalDirectoryV2, JournalSnapshotV2, ObjectIdentityV2, PreimageV2,
};
use super::recovery_policy::{
    ExactRecoveryInventory, ExactRecoveryObject, ExactRecoveryWorld, RecoveryObjectKey,
};
use super::runtime::TransactionRuntime;
use super::store::{exact_directory, exact_file};

const MAX_RECOVERY_DIRECTORY_ENTRIES: usize = 16_384;
const MAX_RECOVERY_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Produces the stable exact world consumed by recovery policy.
///
/// The project root is revalidated around each pass. Each pass independently
/// reopens every capability-relative parent. Equality includes all exact
/// object states and every created-directory inventory.
pub(super) fn capture_stable_recovery_world(
    context: &PlanningContext,
    runtime: &TransactionRuntime,
    snapshot: &JournalSnapshotV2,
    journal_path: &Path,
) -> Result<ExactRecoveryWorld, CodegenError> {
    context.revalidate_project_root_identity()?;
    validate_parent_coverage(snapshot, journal_path)?;
    let first = capture_once(context, runtime.fs(), snapshot, journal_path)?;
    context.revalidate_project_root_identity()?;
    let second = capture_once(context, runtime.fs(), snapshot, journal_path)?;
    context.revalidate_project_root_identity()?;
    require_equal_captures(first, second, journal_path)
}

fn capture_once(
    context: &PlanningContext,
    fs: &dyn FsOps,
    snapshot: &JournalSnapshotV2,
    journal_path: &Path,
) -> Result<ExactRecoveryWorld, CodegenError> {
    let mut objects = BTreeMap::new();
    let mut inventories = BTreeMap::new();
    let workspace_logical = format!(
        "src/components/ui/_kit/.transactions/{}",
        snapshot.project().workspace().name()
    );

    for entry in snapshot.entries() {
        let parent_logical = immediate_parent(entry.logical_path());
        insert_once(
            &mut objects,
            RecoveryObjectKey::Target(entry.ordinal()),
            observe_regular_child(
                context,
                fs,
                parent_logical,
                leaf_name(entry.logical_path()),
                &context.project_root().join(entry.logical_path()),
                journal_path,
            )?,
            journal_path,
        )?;
        insert_once(
            &mut objects,
            RecoveryObjectKey::StageOwner(entry.ordinal()),
            observe_regular_owner_child(
                context,
                fs,
                &workspace_logical,
                entry.stage().owner_name(),
                &context
                    .project_root()
                    .join(&workspace_logical)
                    .join(entry.stage().owner_name()),
                entry.stage().owner_current().as_present(),
                entry.planned().byte_len(),
                journal_path,
            )?,
            journal_path,
        )?;
        insert_once(
            &mut objects,
            RecoveryObjectKey::Stage(entry.ordinal()),
            observe_regular_child(
                context,
                fs,
                parent_logical,
                entry.stage().name(),
                &context
                    .project_root()
                    .join(parent_logical)
                    .join(entry.stage().name()),
                journal_path,
            )?,
            journal_path,
        )?;
        if let Some(backup) = entry.backup() {
            insert_once(
                &mut objects,
                RecoveryObjectKey::BackupOwner(entry.ordinal()),
                observe_regular_owner_child(
                    context,
                    fs,
                    &workspace_logical,
                    backup.owner_name(),
                    &context
                        .project_root()
                        .join(&workspace_logical)
                        .join(backup.owner_name()),
                    backup.owner_current().as_present(),
                    match entry.preimage() {
                        PreimageV2::Regular { exact } => exact.state().byte_len(),
                        PreimageV2::Absent => 0,
                    },
                    journal_path,
                )?,
                journal_path,
            )?;
            insert_once(
                &mut objects,
                RecoveryObjectKey::Backup(entry.ordinal()),
                observe_regular_child(
                    context,
                    fs,
                    parent_logical,
                    backup.name(),
                    &context
                        .project_root()
                        .join(parent_logical)
                        .join(backup.name()),
                    journal_path,
                )?,
                journal_path,
            )?;
        }
    }

    for directory in snapshot.directories() {
        let key = RecoveryObjectKey::Directory(directory.ordinal());
        let captured = observe_logical_directory(
            context,
            fs,
            directory.logical_path(),
            directory.disposition() == DirectoryDispositionV2::Create,
            journal_path,
        )?;
        insert_once(&mut objects, key, captured.object, journal_path)?;
        if let Some(inventory) = captured.inventory {
            insert_inventory_once(&mut inventories, key, inventory, journal_path)?;
        }

        if let Some(candidate_name) = directory.candidate_name() {
            let key = RecoveryObjectKey::DirectoryOwner(directory.ordinal());
            let candidate_path = context
                .project_root()
                .join(&workspace_logical)
                .join(candidate_name);
            let captured = observe_candidate_directory(
                context,
                fs,
                &workspace_logical,
                candidate_name,
                &candidate_path,
                directory,
                journal_path,
            )?;
            insert_once(&mut objects, key, captured.object, journal_path)?;
            if let Some(inventory) = captured.inventory {
                insert_inventory_once(&mut inventories, key, inventory, journal_path)?;
            }
        }
    }

    let expected_key_count = snapshot.entries().len() * 3
        + snapshot
            .entries()
            .iter()
            .filter(|entry| entry.backup().is_some())
            .count()
            * 2
        + snapshot.directories().len()
        + snapshot
            .directories()
            .iter()
            .filter(|directory| directory.candidate_name().is_some())
            .count();
    if objects.len() != expected_key_count {
        return Err(recovery_blocked(
            journal_path,
            "exact recovery capture has a missing, duplicate, or extra cohort key",
        ));
    }

    Ok(ExactRecoveryWorld::from_complete_capture(
        objects,
        inventories,
    ))
}

struct DirectoryCapture {
    object: ExactRecoveryObject,
    inventory: Option<ExactRecoveryInventory>,
}

fn observe_regular_child(
    context: &PlanningContext,
    fs: &dyn FsOps,
    parent_logical: &str,
    name: &str,
    diagnostic_path: &Path,
    journal_path: &Path,
) -> Result<ExactRecoveryObject, CodegenError> {
    let parent = match open_logical_directory(context, parent_logical)? {
        Some(parent) => parent,
        None => return Ok(ExactRecoveryObject::Missing),
    };
    match fs.observe_regular_file(&parent, Path::new(name), diagnostic_path) {
        Ok(observation) => exact_file(&observation)
            .map(ExactRecoveryObject::File)
            .map_err(|error| model_error(journal_path, diagnostic_path, error.reason())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ExactRecoveryObject::Missing),
        Err(error) => Err(observation_error(
            "observe exact recovery regular file",
            diagnostic_path,
            error,
        )),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "recovery capture keeps every capability, bound name, limit, and diagnostic path explicit"
)]
fn observe_regular_owner_child(
    context: &PlanningContext,
    fs: &dyn FsOps,
    parent_logical: &str,
    name: &str,
    diagnostic_path: &Path,
    expected: Option<&ExactFileStateV2>,
    max_bytes: u64,
    journal_path: &Path,
) -> Result<ExactRecoveryObject, CodegenError> {
    let parent = match open_logical_directory(context, parent_logical)? {
        Some(parent) => parent,
        None => return Ok(ExactRecoveryObject::Missing),
    };
    if expected.is_some() {
        return match fs.observe_regular_file_bounded(
            &parent,
            Path::new(name),
            diagnostic_path,
            max_bytes,
        ) {
            Ok(observation) => exact_file(&observation)
                .map(ExactRecoveryObject::File)
                .map_err(|error| model_error(journal_path, diagnostic_path, error.reason())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(ExactRecoveryObject::Missing)
            }
            Err(error) => Err(observation_error(
                "observe bounded exact recovery owner file",
                diagnostic_path,
                error,
            )),
        };
    }
    match fs.observe_regular_file_metadata(&parent, Path::new(name), diagnostic_path, max_bytes) {
        Ok(observation) => file_metadata_object(&observation, journal_path, diagnostic_path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ExactRecoveryObject::Missing),
        Err(error) => Err(observation_error(
            "observe metadata-only recovery owner file",
            diagnostic_path,
            error,
        )),
    }
}

fn file_metadata_object(
    observation: &ExactFileMetadataObservation,
    journal_path: &Path,
    diagnostic_path: &Path,
) -> Result<ExactRecoveryObject, CodegenError> {
    let link_count = observation.link_count.ok_or_else(|| {
        recovery_blocked(
            journal_path,
            format!(
                "metadata-only owner {} has no exact link count",
                diagnostic_path.display()
            ),
        )
    })?;
    ExactFileMetadataV2::new(
        ObjectIdentityV2::from_parts(
            observation.identity.namespace(),
            observation.identity.object(),
        ),
        observation.byte_len,
        observation.mode.readonly,
        observation.mode.posix_mode,
        link_count,
    )
    .map(ExactRecoveryObject::FileMetadata)
    .map_err(|error| model_error(journal_path, diagnostic_path, error.reason()))
}

fn observe_logical_directory(
    context: &PlanningContext,
    fs: &dyn FsOps,
    logical_path: &str,
    inventory_required: bool,
    journal_path: &Path,
) -> Result<DirectoryCapture, CodegenError> {
    let directory = match open_logical_directory(context, logical_path)? {
        Some(directory) => directory,
        None => {
            return Ok(DirectoryCapture {
                object: ExactRecoveryObject::Missing,
                inventory: None,
            });
        }
    };
    let parent_logical = immediate_parent(logical_path);
    let parent = open_logical_directory(context, parent_logical)?.ok_or_else(|| {
        recovery_blocked(
            journal_path,
            format!("present recovery directory {logical_path} has a missing parent"),
        )
    })?;
    let diagnostic_path = context.project_root().join(logical_path);
    let endpoint = DirectoryEndpoint::new(
        &parent,
        Path::new(leaf_name(logical_path)),
        &directory,
        &diagnostic_path,
    );
    let observation = fs.observe_directory(endpoint).map_err(|error| {
        observation_error("observe exact recovery directory", &diagnostic_path, error)
    })?;
    let object = exact_directory(&observation)
        .map(ExactRecoveryObject::Directory)
        .map_err(|error| model_error(journal_path, &diagnostic_path, error.reason()))?;
    let inventory = inventory_required
        .then(|| capture_inventory(fs, endpoint, &observation, &diagnostic_path, journal_path))
        .transpose()?;
    Ok(DirectoryCapture { object, inventory })
}

fn observe_candidate_directory(
    context: &PlanningContext,
    fs: &dyn FsOps,
    parent_logical: &str,
    candidate_name: &str,
    diagnostic_path: &Path,
    model: &JournalDirectoryV2,
    journal_path: &Path,
) -> Result<DirectoryCapture, CodegenError> {
    let parent = match open_logical_directory(context, parent_logical)? {
        Some(parent) => parent,
        None => {
            return Ok(DirectoryCapture {
                object: ExactRecoveryObject::Missing,
                inventory: None,
            });
        }
    };

    let mut accepted_modes = BTreeSet::new();
    if let Some(current) = model.candidate_current().as_present() {
        accepted_modes.insert(current.mode().posix_mode().unwrap_or(0o755));
    }
    accepted_modes.insert(0o700);
    accepted_modes.insert(model.planned_mode().posix_mode().unwrap_or(0o755));
    let metadata_only = model.candidate_current().is_missing();

    let mut last_error = None;
    for mode in accepted_modes {
        match fs.open_directory_exact(&parent, Path::new(candidate_name), diagnostic_path, mode) {
            Ok(opened) => {
                let endpoint = DirectoryEndpoint::new(
                    &parent,
                    Path::new(candidate_name),
                    &opened.directory,
                    diagnostic_path,
                );
                let inventory = capture_empty_owner_inventory(
                    fs,
                    endpoint,
                    &opened.observation,
                    diagnostic_path,
                    journal_path,
                )?;
                let exact = exact_directory(&opened.observation)
                    .map_err(|error| model_error(journal_path, diagnostic_path, error.reason()))?;
                let object = if metadata_only {
                    let link_count = opened.observation.link_count.ok_or_else(|| {
                        recovery_blocked(
                            journal_path,
                            "metadata-only directory owner has no exact link count",
                        )
                    })?;
                    ExactDirectoryMetadataV2::new(exact, link_count)
                        .map(ExactRecoveryObject::DirectoryMetadata)
                        .map_err(|error| {
                            model_error(journal_path, diagnostic_path, error.reason())
                        })?
                } else {
                    ExactRecoveryObject::Directory(exact)
                };
                return Ok(DirectoryCapture {
                    object,
                    inventory: Some(inventory),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(DirectoryCapture {
                    object: ExactRecoveryObject::Missing,
                    inventory: None,
                });
            }
            Err(error) => last_error = Some(error),
        }
    }

    Err(observation_error(
        "open exact recovery directory candidate",
        diagnostic_path,
        last_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "directory candidate has no permitted exact mode",
            )
        }),
    ))
}

fn capture_empty_owner_inventory(
    fs: &dyn FsOps,
    endpoint: DirectoryEndpoint<'_>,
    expected_directory: &ExactDirectoryObservation,
    diagnostic_path: &Path,
    journal_path: &Path,
) -> Result<ExactRecoveryInventory, CodegenError> {
    let inventory = fs
        .inventory_directory_exact_bounded(endpoint, expected_directory, 0)
        .map_err(|error| {
            observation_error(
                "inventory exact recovery owner directory",
                diagnostic_path,
                error,
            )
        })?;
    if inventory.directory != *expected_directory {
        return Err(recovery_blocked(
            journal_path,
            "directory owner changed during its exact empty-inventory capture",
        ));
    }
    if !inventory.entries.is_empty() {
        return Err(recovery_blocked(
            journal_path,
            "directory owner residual is nonempty; recovery will preserve it",
        ));
    }
    Ok(BTreeMap::new())
}

fn capture_inventory(
    fs: &dyn FsOps,
    endpoint: DirectoryEndpoint<'_>,
    expected_directory: &ExactDirectoryObservation,
    diagnostic_path: &Path,
    journal_path: &Path,
) -> Result<ExactRecoveryInventory, CodegenError> {
    let inventory = fs
        .inventory_directory_exact_bounded(
            endpoint,
            expected_directory,
            MAX_RECOVERY_DIRECTORY_ENTRIES,
        )
        .map_err(|error| {
            observation_error("inventory exact recovery directory", diagnostic_path, error)
        })?;
    if inventory.directory != *expected_directory {
        return Err(recovery_blocked(
            journal_path,
            format!(
                "directory {} changed while its exact inventory was captured",
                diagnostic_path.display()
            ),
        ));
    }
    if inventory.entries.len() > MAX_RECOVERY_DIRECTORY_ENTRIES {
        return Err(recovery_blocked(
            journal_path,
            format!(
                "directory {} exceeds the bounded recovery inventory limit of {MAX_RECOVERY_DIRECTORY_ENTRIES} entries",
                diagnostic_path.display()
            ),
        ));
    }

    let mut captured = BTreeMap::new();
    for entry in inventory.entries {
        let name = entry.name.to_str().ok_or_else(|| {
            recovery_blocked(
                journal_path,
                format!(
                    "directory {} contains a non-UTF-8 child",
                    diagnostic_path.display()
                ),
            )
        })?;
        require_direct_child_name(name, journal_path, diagnostic_path)?;
        let child_path = diagnostic_path.join(name);
        let object = capture_inventory_child(
            fs,
            endpoint.directory,
            name,
            &child_path,
            &entry,
            journal_path,
        )?;
        if captured.insert(name.to_owned(), object).is_some() {
            return Err(recovery_blocked(
                journal_path,
                format!(
                    "directory {} contains a duplicate child name",
                    diagnostic_path.display()
                ),
            ));
        }
    }
    Ok(captured)
}

fn capture_inventory_child(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &str,
    diagnostic_path: &Path,
    entry: &ExactDirectoryEntry,
    journal_path: &Path,
) -> Result<ExactRecoveryObject, CodegenError> {
    match entry.kind {
        ExactDirectoryEntryKind::RegularFile => {
            let read = fs
                .read_regular_file_exact(
                    parent,
                    Path::new(name),
                    diagnostic_path,
                    MAX_RECOVERY_FILE_BYTES,
                )
                .map_err(|error| {
                    observation_error("read exact recovery inventory file", diagnostic_path, error)
                })?;
            require_file_entry_matches(entry, &read.observation, journal_path, diagnostic_path)?;
            exact_file(&read.observation)
                .map(ExactRecoveryObject::File)
                .map_err(|error| model_error(journal_path, diagnostic_path, error.reason()))
        }
        ExactDirectoryEntryKind::Directory => {
            let mode = entry.mode.posix_mode.unwrap_or(0o755);
            let opened = fs
                .open_directory_exact(parent, Path::new(name), diagnostic_path, mode)
                .map_err(|error| {
                    observation_error(
                        "open exact recovery inventory directory",
                        diagnostic_path,
                        error,
                    )
                })?;
            require_directory_entry_matches(
                entry,
                &opened.observation,
                journal_path,
                diagnostic_path,
            )?;
            exact_directory(&opened.observation)
                .map(ExactRecoveryObject::Directory)
                .map_err(|error| model_error(journal_path, diagnostic_path, error.reason()))
        }
        ExactDirectoryEntryKind::Symlink => Err(unsafe_inventory_kind(
            journal_path,
            diagnostic_path,
            "symbolic link",
        )),
        #[cfg(windows)]
        ExactDirectoryEntryKind::ReparsePoint => Err(unsafe_inventory_kind(
            journal_path,
            diagnostic_path,
            "Windows reparse point",
        )),
        ExactDirectoryEntryKind::Other => Err(unsafe_inventory_kind(
            journal_path,
            diagnostic_path,
            "unsupported filesystem object",
        )),
    }
}

fn require_file_entry_matches(
    entry: &ExactDirectoryEntry,
    observed: &ExactFileObservation,
    journal_path: &Path,
    diagnostic_path: &Path,
) -> Result<(), CodegenError> {
    if entry.kind == ExactDirectoryEntryKind::RegularFile
        && entry.identity == observed.identity
        && entry.byte_len == observed.byte_len
        && entry.mode == observed.mode
        && entry.link_count == observed.link_count
    {
        Ok(())
    } else {
        Err(recovery_blocked(
            journal_path,
            format!(
                "inventory file {} changed between exact directory inventory and no-follow read",
                diagnostic_path.display()
            ),
        ))
    }
}

fn require_directory_entry_matches(
    entry: &ExactDirectoryEntry,
    observed: &ExactDirectoryObservation,
    journal_path: &Path,
    diagnostic_path: &Path,
) -> Result<(), CodegenError> {
    if entry.kind == ExactDirectoryEntryKind::Directory
        && entry.identity == observed.identity
        && entry.mode == observed.mode
        && entry.link_count == observed.link_count
    {
        Ok(())
    } else {
        Err(recovery_blocked(
            journal_path,
            format!(
                "inventory directory {} changed between exact inventory and no-follow open",
                diagnostic_path.display()
            ),
        ))
    }
}

fn validate_parent_coverage(
    snapshot: &JournalSnapshotV2,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    let directories = snapshot
        .directories()
        .iter()
        .map(|directory| directory.logical_path())
        .collect::<BTreeSet<_>>();
    for entry in snapshot.entries() {
        let parent = immediate_parent(entry.logical_path());
        if !parent.is_empty() && !directories.contains(parent) {
            return Err(recovery_blocked(
                journal_path,
                format!(
                    "transaction target {} has a parent outside the exact directory cohort",
                    entry.logical_path()
                ),
            ));
        }
    }
    for directory in snapshot.directories() {
        let parent = immediate_parent(directory.logical_path());
        let is_bound_workspace_parent =
            directory.logical_path() == "src/components/ui/_kit/.transactions";
        if !parent.is_empty() && !directories.contains(parent) && !is_bound_workspace_parent {
            return Err(recovery_blocked(
                journal_path,
                format!(
                    "transaction directory {} has a parent outside the exact directory cohort",
                    directory.logical_path()
                ),
            ));
        }
    }
    Ok(())
}

fn open_logical_directory(
    context: &PlanningContext,
    logical_path: &str,
) -> Result<Option<Dir>, CodegenError> {
    match context.open_directory(logical_path) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if codegen_not_found(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn insert_once(
    objects: &mut BTreeMap<RecoveryObjectKey, ExactRecoveryObject>,
    key: RecoveryObjectKey,
    value: ExactRecoveryObject,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    if objects.insert(key, value).is_none() {
        Ok(())
    } else {
        Err(recovery_blocked(
            journal_path,
            "transaction model contains a duplicate exact recovery object key",
        ))
    }
}

fn insert_inventory_once(
    inventories: &mut BTreeMap<RecoveryObjectKey, ExactRecoveryInventory>,
    key: RecoveryObjectKey,
    value: ExactRecoveryInventory,
    journal_path: &Path,
) -> Result<(), CodegenError> {
    if inventories.insert(key, value).is_none() {
        Ok(())
    } else {
        Err(recovery_blocked(
            journal_path,
            "transaction model contains a duplicate exact recovery inventory key",
        ))
    }
}

fn require_equal_captures(
    first: ExactRecoveryWorld,
    second: ExactRecoveryWorld,
    journal_path: &Path,
) -> Result<ExactRecoveryWorld, CodegenError> {
    if first == second {
        Ok(second)
    } else {
        Err(recovery_blocked(
            journal_path,
            "target, parent, stage, backup, directory, or inventory changed during stable recovery capture",
        ))
    }
}

fn require_direct_child_name(
    name: &str,
    journal_path: &Path,
    directory_path: &Path,
) -> Result<(), CodegenError> {
    if !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
    {
        Ok(())
    } else {
        Err(recovery_blocked(
            journal_path,
            format!(
                "directory {} contains a non-child inventory name",
                directory_path.display()
            ),
        ))
    }
}

fn unsafe_inventory_kind(journal_path: &Path, diagnostic_path: &Path, kind: &str) -> CodegenError {
    recovery_blocked(
        journal_path,
        format!(
            "recovery inventory child {} is a {kind}; journal evidence was preserved",
            diagnostic_path.display()
        ),
    )
}

fn codegen_not_found(error: &CodegenError) -> bool {
    matches!(error, CodegenError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
}

fn immediate_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn leaf_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn observation_error(operation: &'static str, path: &Path, source: io::Error) -> CodegenError {
    CodegenError::FilesystemOperation {
        operation,
        logical_path: path.display().to_string(),
        path: path.to_path_buf(),
        source,
    }
}

fn model_error(journal_path: &Path, object_path: &Path, reason: &str) -> CodegenError {
    recovery_blocked(
        journal_path,
        format!(
            "exact recovery object {} cannot be represented safely: {reason}",
            object_path.display()
        ),
    )
}

fn recovery_blocked(path: &Path, reason: impl Into<String>) -> CodegenError {
    CodegenError::RecoveryRequired {
        journal_path: path.to_path_buf(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::fs::ExactObjectIdentity;
    use super::super::journal::{
        DirectoryModeV2, ExactDirectoryStateV2, ExactFileStateV2, FileStateV2, ObjectIdentityV2,
        Sha256Digest,
    };
    use super::*;
    use crate::PreservedFileMode;

    fn file_observation(object: u64) -> ExactFileObservation {
        ExactFileObservation {
            identity: ExactObjectIdentity::from_unix(1, object),
            byte_len: object,
            content_hash: format!("sha256:{object:064x}"),
            mode: PreservedFileMode {
                readonly: false,
                posix_mode: if cfg!(unix) { Some(0o644) } else { None },
            },
            link_count: Some(1),
        }
    }

    fn file_entry(object: u64) -> ExactDirectoryEntry {
        let observation = file_observation(object);
        ExactDirectoryEntry {
            name: "child".into(),
            kind: ExactDirectoryEntryKind::RegularFile,
            identity: observation.identity,
            byte_len: observation.byte_len,
            mode: observation.mode,
            link_count: observation.link_count,
        }
    }

    fn world(object: u64) -> ExactRecoveryWorld {
        let ordinal = super::super::journal::ArtifactOrdinal::new(0).expect("ordinal");
        let exact = ExactFileStateV2::new(
            ObjectIdentityV2::new(1, object),
            FileStateV2::new(
                Sha256Digest::parse(&format!("sha256:{object:064x}")).expect("digest"),
                object,
                false,
                if cfg!(unix) { Some(0o644) } else { None },
            )
            .expect("file state"),
            1,
        )
        .expect("exact file");
        ExactRecoveryWorld::from_complete_capture(
            BTreeMap::from([(
                RecoveryObjectKey::Target(ordinal),
                ExactRecoveryObject::File(exact),
            )]),
            BTreeMap::new(),
        )
    }

    #[test]
    fn stable_capture_requires_equal_objects_and_inventories() {
        let first = world(1);
        let same = first.clone();
        assert_eq!(
            require_equal_captures(first, same.clone(), Path::new("journal"))
                .expect("equal capture"),
            same
        );
        assert!(require_equal_captures(same, world(2), Path::new("journal")).is_err());
    }

    #[test]
    fn inventory_file_must_match_its_exact_no_follow_read() {
        let entry = file_entry(7);
        let observation = file_observation(7);
        require_file_entry_matches(
            &entry,
            &observation,
            Path::new("journal"),
            Path::new("child"),
        )
        .expect("matching inventory file");

        let mut substituted = observation;
        substituted.identity = ExactObjectIdentity::from_unix(1, 8);
        assert!(
            require_file_entry_matches(
                &entry,
                &substituted,
                Path::new("journal"),
                Path::new("child"),
            )
            .is_err()
        );
    }

    #[test]
    fn directory_entry_must_match_exact_opened_capability() {
        let mode = PreservedFileMode {
            readonly: false,
            posix_mode: if cfg!(unix) { Some(0o755) } else { None },
        };
        let entry = ExactDirectoryEntry {
            name: "directory".into(),
            kind: ExactDirectoryEntryKind::Directory,
            identity: ExactObjectIdentity::from_unix(1, 2),
            byte_len: 0,
            mode,
            link_count: Some(2),
        };
        let observed = ExactDirectoryObservation {
            identity: entry.identity,
            mode,
            link_count: entry.link_count,
        };
        require_directory_entry_matches(
            &entry,
            &observed,
            Path::new("journal"),
            Path::new("directory"),
        )
        .expect("matching directory");

        let _model = ExactDirectoryStateV2::new(
            ObjectIdentityV2::new(1, 2),
            DirectoryModeV2::new(false, mode.posix_mode).expect("mode"),
            2,
        )
        .expect("exact directory");
    }
}
