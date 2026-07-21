use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
#[cfg(unix)]
use cap_std::fs::DirBuilder;
use cap_std::{
    ambient_authority,
    fs::{Dir, File, Metadata, OpenOptions},
};

use crate::CodegenError;
use crate::path_safety::{ObjectIdentity, PlanningContext};

use super::fs::{CreatedFile, ExclusiveCreateFailure, FsOps, HardLinkEndpoint, SystemFs};
use super::journal::{
    TransactionId, parse_bootstrap_intent_name, parse_finalization_file_name,
    parse_transaction_directory_name,
};
use super::runtime::TransactionRuntime;

pub const DEFAULT_KIT_WRITE_LOCK_PATH: &str = "src/components/ui/_kit/.write.lock";
pub(crate) const DEFAULT_KIT_COORDINATION_IGNORE_PATH: &str = "src/components/ui/_kit/.gitignore";
pub(crate) const KIT_ADVISORY_LOCK_CONTENT: &[u8] = b"leptos-ui-kit advisory lock v1\n";
pub(crate) const LEGACY_KIT_COORDINATION_IGNORE_CONTENT: &[u8] = b"/.write.lock\n/.transactions/\n";
pub(crate) const KIT_COORDINATION_IGNORE_CONTENT: &[u8] = b"/.write.lock\n/.transactions/\n/.transactions.bootstrap-v2-*/\n/.transactions.retirement-v2-*/\n";

const LEGACY_WRITE_LOCK_CONTENT: &[u8] = b"locked\n";
const TRANSACTIONS_DIRECTORY_NAME: &str = ".transactions";
const LOCK_CANDIDATE_PREFIX: &str = "lock-bootstrap-";
const IGNORE_CANDIDATE_PREFIX: &str = "ignore-bootstrap-";
const TRANSACTION_JOURNAL_PREFIX: &str = "transaction-";
const TRANSACTION_JOURNAL_SUFFIX: &str = ".json";
const JOURNAL_UPDATE_PREFIX: &str = "journal-update-";
const LOCK_CANDIDATE_RANDOM_BYTES: usize = 16;
const LOCK_CANDIDATE_CREATE_ATTEMPTS: usize = 8;
const CLEANUP_QUIESCENCE_ATTEMPTS: usize = 8;
const MAX_COORDINATION_FILE_BYTES: u64 = 4 * 1024;
const KIT_DIRECTORY_PATH: &str = "src/components/ui/_kit";
const KIT_DIRECTORY_COMPONENTS: [&str; 4] = ["src", "components", "ui", "_kit"];

pub struct WriteLock {
    path: PathBuf,
    file: Option<std::fs::File>,
    identity: ObjectIdentity,
    project_root: PathBuf,
    project_identity: ObjectIdentity,
}

struct PinnedKitDirectories {
    project_root: PathBuf,
    directories: Vec<Dir>,
}

impl PinnedKitDirectories {
    fn open(context: &PlanningContext, kit_directory: &Dir) -> Result<Self, CodegenError> {
        let mut directories = vec![context.open_pinned_project_root()?];
        let mut logical_path = PathBuf::new();
        for component in KIT_DIRECTORY_COMPONENTS {
            logical_path.push(component);
            let parent = directories.last().expect("pinned chain has a root");
            let metadata =
                parent
                    .symlink_metadata(component)
                    .map_err(|source| CodegenError::Io {
                        path: context.project_root().join(&logical_path),
                        source,
                    })?;
            ensure_safe_directory_metadata(logical_path.to_string_lossy().as_ref(), &metadata)?;
            let directory =
                parent
                    .open_dir_nofollow(component)
                    .map_err(|source| CodegenError::UnsafePath {
                        path: logical_path.to_string_lossy().into_owned(),
                        reason: format!("failed to pin directory without following: {source}"),
                    })?;
            let opened = directory
                .dir_metadata()
                .map_err(|source| CodegenError::Io {
                    path: context.project_root().join(&logical_path),
                    source,
                })?;
            ensure_safe_directory_metadata(logical_path.to_string_lossy().as_ref(), &opened)?;
            if metadata_identity(&metadata) != metadata_identity(&opened) {
                return Err(CodegenError::UnsafePath {
                    path: logical_path.to_string_lossy().into_owned(),
                    reason: "directory changed while its no-follow pin was opened".to_owned(),
                });
            }
            directories.push(directory);
        }
        context.ensure_same_directory(
            KIT_DIRECTORY_PATH,
            kit_directory,
            directories.last().expect("pinned chain has _kit"),
        )?;
        Ok(Self {
            project_root: context.project_root().to_path_buf(),
            directories,
        })
    }

    fn revalidate(&self) -> Result<(), CodegenError> {
        let current_root =
            Dir::open_ambient_dir(&self.project_root, ambient_authority()).map_err(|source| {
                CodegenError::Io {
                    path: self.project_root.clone(),
                    source,
                }
            })?;
        let current_root_metadata =
            current_root
                .dir_metadata()
                .map_err(|source| CodegenError::Io {
                    path: self.project_root.clone(),
                    source,
                })?;
        let pinned_root_metadata =
            self.directories[0]
                .dir_metadata()
                .map_err(|source| CodegenError::Io {
                    path: self.project_root.clone(),
                    source,
                })?;
        ensure_safe_directory_metadata(".", &current_root_metadata)?;
        ensure_safe_directory_metadata(".", &pinned_root_metadata)?;
        if metadata_identity(&current_root_metadata) != metadata_identity(&pinned_root_metadata) {
            return Err(CodegenError::ProjectRootChanged {
                path: self.project_root.clone(),
                reason: "project root was detached while coordination state was held".to_owned(),
            });
        }

        let mut logical_path = PathBuf::new();
        for (index, component) in KIT_DIRECTORY_COMPONENTS.into_iter().enumerate() {
            logical_path.push(component);
            let metadata = self.directories[index]
                .symlink_metadata(component)
                .map_err(|source| CodegenError::Io {
                    path: self.project_root.join(&logical_path),
                    source,
                })?;
            let opened = self.directories[index + 1]
                .dir_metadata()
                .map_err(|source| CodegenError::Io {
                    path: self.project_root.join(&logical_path),
                    source,
                })?;
            ensure_safe_directory_metadata(logical_path.to_string_lossy().as_ref(), &metadata)?;
            ensure_safe_directory_metadata(logical_path.to_string_lossy().as_ref(), &opened)?;
            if metadata_identity(&metadata) != metadata_identity(&opened) {
                return Err(CodegenError::UnsafePath {
                    path: logical_path.to_string_lossy().into_owned(),
                    reason: "directory was detached while coordination state was held".to_owned(),
                });
            }
        }
        Ok(())
    }

    fn kit(&self) -> &Dir {
        self.directories.last().expect("pinned chain has _kit")
    }

    fn all(&self) -> &[Dir] {
        &self.directories
    }
}

impl std::fmt::Debug for WriteLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WriteLock")
            .field("path", &self.path)
            .finish()
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        drop(self.file.take());
    }
}

fn require_current_kit_directory(
    context: &PlanningContext,
    kit_directory: &Dir,
) -> Result<(), CodegenError> {
    context.revalidate_project_root_identity()?;
    let current = context.open_directory(KIT_DIRECTORY_PATH)?;
    context.ensure_same_directory(KIT_DIRECTORY_PATH, kit_directory, &current)
}

impl WriteLock {
    pub fn acquire(project_root: &Path) -> Result<Self, CodegenError> {
        let context = PlanningContext::open(project_root)?;
        Self::acquire_with_context(&context)
    }

    pub(crate) fn acquire_with_context(context: &PlanningContext) -> Result<Self, CodegenError> {
        Self::acquire_with_context_and_fs(context, Arc::new(SystemFs))
    }

    #[cfg(test)]
    pub(crate) fn acquire_with(
        project_root: &Path,
        fs: Arc<dyn FsOps>,
    ) -> Result<Self, CodegenError> {
        let context = PlanningContext::open(project_root)?;
        Self::acquire_with_context_and_fs(&context, fs)
    }

    pub(crate) fn acquire_with_context_and_fs(
        context: &PlanningContext,
        fs: Arc<dyn FsOps>,
    ) -> Result<Self, CodegenError> {
        let created_directories =
            context.ensure_parent_with(DEFAULT_KIT_WRITE_LOCK_PATH, |logical_path, created| {
                let path = context.project_root().join(logical_path);
                if created {
                    fs.after_create_directory(&path)
                } else {
                    fs.before_create_directory(&path)
                }
                .map_err(|source| CodegenError::Io { path, source })
            })?;
        let (opened_kit_directory, lock_name) = context.open_parent(DEFAULT_KIT_WRITE_LOCK_PATH)?;
        let pinned_kit = PinnedKitDirectories::open(context, &opened_kit_directory)?;
        let kit_directory = pinned_kit.kit();
        require_current_kit_directory(context, kit_directory)?;
        secure_coordination_directory_mode(
            fs.as_ref(),
            kit_directory,
            &context.project_root().join("src/components/ui/_kit"),
            created_directories
                .iter()
                .any(|path| path == "src/components/ui/_kit"),
        )?;
        require_current_kit_directory(context, kit_directory)?;
        sync_directory(
            fs.as_ref(),
            kit_directory,
            &context.project_root().join("src/components/ui/_kit"),
        )?;
        sync_created_directory_chain(context, fs.as_ref(), &created_directories)?;

        let mut opened = match open_existing_lock(context, fs.as_ref())? {
            Some(opened) => opened,
            None => publish_initialized_lock(
                context,
                fs.as_ref(),
                &pinned_kit,
                kit_directory,
                &lock_name,
            )?,
        };
        if !opened.locked {
            acquire_advisory_lock(fs.as_ref(), &opened.file, &opened.path)?;
            opened.locked = true;
        }
        opened = complete_coordination_bootstrap(
            context,
            fs.as_ref(),
            &pinned_kit,
            kit_directory,
            opened,
        )?;
        let (project_root, project_identity) = context.project_identity();

        let lock = Self {
            path: opened.path,
            file: Some(opened.file.into_std()),
            identity: opened.identity,
            project_root: project_root.to_path_buf(),
            project_identity,
        };
        Ok(lock)
    }

    pub(crate) fn validate_context(&self, context: &PlanningContext) -> Result<(), CodegenError> {
        self.validate_context_link_count(context, 1)
    }

    pub(super) fn validate_context_link_count(
        &self,
        context: &PlanningContext,
        expected_links: u64,
    ) -> Result<(), CodegenError> {
        let (project_root, project_identity) = context.project_identity();
        if project_root != self.project_root || project_identity != self.project_identity {
            return Err(CodegenError::ProjectRootChanged {
                path: project_root.to_path_buf(),
                reason: "write lock belongs to a different project identity".to_owned(),
            });
        }
        context.revalidate_project_root_identity()?;

        let held = self
            .file
            .as_ref()
            .ok_or_else(|| CodegenError::InvalidCoordinationState {
                path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
                reason: "the held advisory-lock handle is no longer available".to_owned(),
            })?;
        let cloned = held.try_clone().map_err(|source| CodegenError::Io {
            path: self.path.clone(),
            source,
        })?;
        let mut held = File::from_std(cloned);
        let held_identity = file_identity(&held, &self.path)?;
        if held_identity != self.identity {
            return Err(CodegenError::InvalidCoordinationState {
                path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
                reason: "the held advisory-lock handle no longer identifies the acquired inode"
                    .to_owned(),
            });
        }
        validate_cap_file_mode(&held, 0o600, DEFAULT_KIT_WRITE_LOCK_PATH)?;
        let marker = read_bounded_cap_file(&mut held, &self.path)?;
        validate_lock_marker(&marker)?;
        validate_lock_link_count_exact(&held, &self.path, expected_links)?;
        context.revalidate_auxiliary_identity(DEFAULT_KIT_WRITE_LOCK_PATH, held_identity)?;
        let current = context.open_auxiliary_file(DEFAULT_KIT_WRITE_LOCK_PATH, true)?;
        if file_identity(&current, &self.path)? != held_identity {
            return Err(CodegenError::InvalidCoordinationState {
                path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
                reason: "persistent advisory lock changed during lifecycle validation".to_owned(),
            });
        }
        validate_lock_link_count_exact(&current, &self.path, expected_links)
    }

    pub(super) fn validate_lifecycle_context(
        &self,
        context: &PlanningContext,
    ) -> Result<u64, CodegenError> {
        let held = self
            .file
            .as_ref()
            .ok_or_else(|| CodegenError::InvalidCoordinationState {
                path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
                reason: "the held advisory-lock handle is no longer available".to_owned(),
            })?;
        let held_capability =
            File::from_std(held.try_clone().map_err(|source| CodegenError::Io {
                path: self.path.clone(),
                source,
            })?);
        let links = held_capability
            .metadata()
            .map_err(|source| CodegenError::Io {
                path: self.path.clone(),
                source,
            })?
            .nlink();
        if !matches!(links, 1 | 2) {
            return Err(CodegenError::InvalidCoordinationState {
                path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
                reason: format!(
                    "persistent advisory lock has an unsupported lifecycle link count: {links}"
                ),
            });
        }
        self.validate_context_link_count(context, links)?;
        Ok(links)
    }

    pub(crate) fn identity(&self) -> ObjectIdentity {
        self.identity
    }

    pub(super) fn open_or_create_transaction_namespace(
        &self,
        context: &PlanningContext,
        runtime: &TransactionRuntime,
        transaction_id: &TransactionId,
    ) -> Result<Dir, CodegenError> {
        super::namespace_bootstrap::ensure_transaction_namespace(
            context,
            self,
            runtime,
            transaction_id,
        )
    }
}

struct OpenedLock {
    file: File,
    identity: ObjectIdentity,
    path: PathBuf,
    locked: bool,
}

fn validate_opened_lock(
    context: &PlanningContext,
    fs: &dyn FsOps,
    opened: &mut OpenedLock,
) -> Result<(), CodegenError> {
    fs.before_read_handle(&opened.path)
        .map_err(|source| CodegenError::Io {
            path: opened.path.clone(),
            source,
        })?;
    let marker = read_bounded_cap_file(&mut opened.file, &opened.path)?;
    validate_lock_marker(&marker)?;
    validate_cap_file_mode(&opened.file, 0o600, DEFAULT_KIT_WRITE_LOCK_PATH)?;
    context.revalidate_auxiliary_identity(DEFAULT_KIT_WRITE_LOCK_PATH, opened.identity)
}

fn validate_completed_opened_lock(
    context: &PlanningContext,
    fs: &dyn FsOps,
    opened: &mut OpenedLock,
) -> Result<(), CodegenError> {
    validate_opened_lock(context, fs, opened)?;
    validate_single_link_lock(context, &opened.file, opened.identity, &opened.path)
}

fn validate_single_link_lock(
    context: &PlanningContext,
    held: &File,
    expected_identity: ObjectIdentity,
    path: &Path,
) -> Result<(), CodegenError> {
    context.revalidate_auxiliary_identity(DEFAULT_KIT_WRITE_LOCK_PATH, expected_identity)?;
    validate_lock_link_count(held, path)?;

    let current = context.open_auxiliary_file(DEFAULT_KIT_WRITE_LOCK_PATH, true)?;
    let current_identity = file_identity(&current, path)?;
    if current_identity != expected_identity {
        return Err(CodegenError::UnsafePath {
            path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
            reason: "persistent advisory lock changed while its link count was validated"
                .to_owned(),
        });
    }
    validate_lock_link_count(&current, path)?;
    context.revalidate_auxiliary_file(DEFAULT_KIT_WRITE_LOCK_PATH, &current)?;

    // Re-read through the advisory-lock handle after pathname revalidation so
    // an alias introduced during the check cannot be mistaken for completed
    // bootstrap state.
    validate_lock_link_count(held, path)
}

fn validate_lock_link_count(file: &File, path: &Path) -> Result<(), CodegenError> {
    validate_lock_link_count_exact(file, path, 1)
}

fn validate_lock_link_count_exact(
    file: &File,
    path: &Path,
    expected_links: u64,
) -> Result<(), CodegenError> {
    let metadata = file.metadata().map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    ensure_safe_regular_metadata(DEFAULT_KIT_WRITE_LOCK_PATH, &metadata)?;
    let links = metadata.nlink();
    if links == expected_links {
        return Ok(());
    }
    Err(CodegenError::InvalidCoordinationState {
        path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
        reason: if expected_links == 1 {
            format!(
                "persistent advisory lock must have exactly one hard link in this lifecycle phase; found {links}"
            )
        } else {
            format!(
                "persistent advisory lock must have exactly {expected_links} hard links in this lifecycle phase; found {links}"
            )
        },
    })
}

fn complete_coordination_bootstrap(
    context: &PlanningContext,
    fs: &dyn FsOps,
    pinned_kit: &PinnedKitDirectories,
    kit_directory: &Dir,
    mut opened: OpenedLock,
) -> Result<OpenedLock, CodegenError> {
    // A newly published lock may still have its private publisher alias at
    // this point, so the initial validation intentionally checks identity,
    // marker, and mode without enforcing the completed-state link invariant.
    validate_opened_lock(context, fs, &mut opened)?;
    let links = opened
        .file
        .metadata()
        .map_err(|source| CodegenError::Io {
            path: opened.path.clone(),
            source,
        })?
        .nlink();
    if links == 2 {
        if coordination_ignore_requires_migration(context, fs)? {
            super::coordination_migration::validate_acquisition_alias(
                context,
                fs,
                kit_directory,
                opened.identity,
            )?;
            validate_opened_lock(context, fs, &mut opened)?;
            return Ok(opened);
        }
        if super::namespace_bootstrap::validate_acquisition_alias_if_present(
            context,
            fs,
            kit_directory,
            opened.identity,
        )? {
            bootstrap_coordination_ignore(context, fs, pinned_kit, kit_directory)?;
            validate_opened_lock(context, fs, &mut opened)?;
            return Ok(opened);
        }
    }
    opened = cleanup_stale_candidates_before_planning(context, fs, kit_directory, opened)?;
    bootstrap_coordination_ignore(context, fs, pinned_kit, kit_directory)?;
    if coordination_ignore_requires_migration(context, fs)? {
        let links = opened
            .file
            .metadata()
            .map_err(|source| CodegenError::Io {
                path: opened.path.clone(),
                source,
            })?
            .nlink();
        if links == 2 {
            super::coordination_migration::validate_acquisition_alias(
                context,
                fs,
                kit_directory,
                opened.identity,
            )?;
            validate_opened_lock(context, fs, &mut opened)?;
        } else {
            validate_completed_opened_lock(context, fs, &mut opened)?;
        }
        return Ok(opened);
    }
    let links = opened
        .file
        .metadata()
        .map_err(|source| CodegenError::Io {
            path: opened.path.clone(),
            source,
        })?
        .nlink();
    if links == 2 {
        validate_opened_lock(context, fs, &mut opened)?;
    } else {
        validate_completed_opened_lock(context, fs, &mut opened)?;
    }
    Ok(opened)
}

#[cfg(not(windows))]
fn cleanup_stale_candidates_before_planning(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    opened: OpenedLock,
) -> Result<OpenedLock, CodegenError> {
    let StaleCandidateCleanupOutcome::Complete =
        cleanup_stale_lock_candidates(context, fs, kit_directory, opened.identity)?;
    context.revalidate_auxiliary_identity(DEFAULT_KIT_WRITE_LOCK_PATH, opened.identity)?;
    Ok(opened)
}

#[cfg(windows)]
fn cleanup_stale_candidates_before_planning(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    mut opened: OpenedLock,
) -> Result<OpenedLock, CodegenError> {
    for _ in 0..CLEANUP_QUIESCENCE_ATTEMPTS {
        match cleanup_stale_lock_candidates(context, fs, kit_directory, opened.identity)? {
            StaleCandidateCleanupOutcome::Complete => {
                context
                    .revalidate_auxiliary_identity(DEFAULT_KIT_WRITE_LOCK_PATH, opened.identity)?;
                return Ok(opened);
            }
            StaleCandidateCleanupOutcome::HeldLockAliasDeferred {
                transactions_identity,
            } => {
                let expected_identity = opened.identity;
                drop(opened);
                finish_deferred_held_lock_alias_cleanup(
                    context,
                    fs,
                    kit_directory,
                    transactions_identity,
                )?;
                opened = converge_on_expected_published_lock(context, fs, expected_identity)?;
                acquire_advisory_lock(fs, &opened.file, &opened.path)?;
                opened.locked = true;
                validate_opened_lock(context, fs, &mut opened)?;
            }
        }
    }
    Err(CodegenError::InvalidCoordinationState {
        path: "src/components/ui/_kit/.transactions".to_owned(),
        reason: "held-lock alias cleanup did not converge after closing and reacquiring the advisory lock"
            .to_owned(),
    })
}

#[cfg(windows)]
fn finish_deferred_held_lock_alias_cleanup(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions_identity: ObjectIdentity,
) -> Result<(), CodegenError> {
    for _ in 0..CLEANUP_QUIESCENCE_ATTEMPTS {
        match try_cleanup_transactions_directory_by_identity(
            fs,
            kit_directory,
            context,
            transactions_identity,
        ) {
            Ok(TransactionsDirectoryCleanupOutcome::Removed)
            | Ok(TransactionsDirectoryCleanupOutcome::Absent) => return Ok(()),
            Ok(TransactionsDirectoryCleanupOutcome::NotQuiescent(source))
                if windows_namespace_delete_pending_error(&source) =>
            {
                continue;
            }
            Ok(TransactionsDirectoryCleanupOutcome::NotQuiescent(_)) => return Ok(()),
            Ok(TransactionsDirectoryCleanupOutcome::Failed(source))
                if windows_namespace_delete_pending_error(&source) =>
            {
                continue;
            }
            Ok(TransactionsDirectoryCleanupOutcome::Failed(source)) => {
                best_effort_cleanup_transactions_directory_by_identity(
                    fs,
                    kit_directory,
                    context,
                    transactions_identity,
                );
                return Err(CodegenError::Io {
                    path: context
                        .project_root()
                        .join("src/components/ui/_kit/.transactions"),
                    source,
                });
            }
            Err(original) => {
                best_effort_cleanup_transactions_directory_by_identity(
                    fs,
                    kit_directory,
                    context,
                    transactions_identity,
                );
                return Err(original);
            }
        }
    }
    Err(CodegenError::InvalidCoordinationState {
        path: "src/components/ui/_kit/.transactions".to_owned(),
        reason: "Windows delete-pending cleanup did not converge after closing the held advisory-lock handle"
            .to_owned(),
    })
}

fn open_existing_lock(
    context: &PlanningContext,
    fs: &dyn FsOps,
) -> Result<Option<OpenedLock>, CodegenError> {
    let path = context.project_root().join(DEFAULT_KIT_WRITE_LOCK_PATH);
    fs.before_open_coordination_file(&path)
        .map_err(|source| CodegenError::Io {
            path: path.clone(),
            source,
        })?;
    match context.open_auxiliary_file(DEFAULT_KIT_WRITE_LOCK_PATH, true) {
        Ok(file) => {
            inspect_metadata(fs, &path)?;
            let identity = file_identity(&file, &path)?;
            Ok(Some(OpenedLock {
                file,
                identity,
                path,
                locked: false,
            }))
        }
        Err(CodegenError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn publish_initialized_lock(
    context: &PlanningContext,
    fs: &dyn FsOps,
    pinned_kit: &PinnedKitDirectories,
    kit_directory: &Dir,
    lock_name: &str,
) -> Result<OpenedLock, CodegenError> {
    let lock_path = context.project_root().join(DEFAULT_KIT_WRITE_LOCK_PATH);
    let transactions = open_or_create_transactions_directory(context, fs, kit_directory)?;
    let mut candidate = match create_candidate(
        context,
        fs,
        kit_directory,
        &transactions,
        CandidateKind::Lock,
    ) {
        Ok(candidate) => candidate,
        Err(CodegenError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            drop(transactions.directory);
            return converge_on_published_lock(context, fs);
        }
        Err(error) => {
            let _ = cleanup_transactions_directory_after_drop(
                fs,
                kit_directory,
                context,
                transactions.identity,
                transactions.directory,
            );
            return Err(error);
        }
    };

    match acquire_private_candidate_lock(fs, &candidate.file, &candidate.path) {
        Ok(true) => {}
        Ok(false) => {
            drop(candidate);
            drop(transactions.directory);
            return converge_on_published_lock(context, fs);
        }
        Err(error) => {
            let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
            return Err(error);
        }
    }
    match candidate_is_current(context, fs, kit_directory, &transactions, &candidate) {
        Ok(true) => {}
        Ok(false) => {
            let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
            return converge_on_published_lock(context, fs);
        }
        Err(error) => {
            let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
            return Err(error);
        }
    }

    let prepare_result = (|| {
        require_current_candidate(context, fs, kit_directory, &transactions, &candidate)?;
        set_file_mode(fs, &candidate.file, 0o600, &candidate.path)?;
        require_current_candidate(context, fs, kit_directory, &transactions, &candidate)?;
        fs.write_handle(
            &mut candidate.file,
            &candidate.path,
            KIT_ADVISORY_LOCK_CONTENT,
        )
        .map_err(|source| CodegenError::Io {
            path: candidate.path.clone(),
            source,
        })?;
        require_current_candidate(context, fs, kit_directory, &transactions, &candidate)?;
        fs.sync_handle(&candidate.file, &candidate.path)
            .map_err(|source| CodegenError::Io {
                path: candidate.path.clone(),
                source,
            })?;
        require_current_candidate(context, fs, kit_directory, &transactions, &candidate)?;
        Ok(())
    })();
    if let Err(error) = prepare_result {
        let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
        return Err(error);
    }

    let publication_precheck = (|| {
        require_current_candidate(context, fs, kit_directory, &transactions, &candidate)?;
        validate_candidate_source_guard(&candidate)?;
        pinned_kit.revalidate()
    })();
    if let Err(error) = publication_precheck {
        let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
        return Err(error);
    }
    let candidate_source = match fs
        .read_regular_file_exact(
            &transactions.directory,
            Path::new(&candidate.name),
            &candidate.path,
            KIT_ADVISORY_LOCK_CONTENT.len() as u64,
        )
        .map_err(|source| CodegenError::Io {
            path: candidate.path.clone(),
            source,
        }) {
        Ok(source) => source,
        Err(error) => {
            let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
            return Err(error);
        }
    };
    if candidate_source.bytes != KIT_ADVISORY_LOCK_CONTENT
        || candidate_source.observation.identity != candidate.identity
        || candidate_source.observation.link_count != Some(1)
    {
        let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
        return Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
            reason: "lock publication candidate is not the exact single-link prepared owner"
                .to_owned(),
        });
    }
    match fs.hard_link(
        pinned_kit.all(),
        HardLinkEndpoint::new(
            &transactions.directory,
            Path::new(&candidate.name),
            &candidate.path,
        ),
        &candidate_source.observation,
        HardLinkEndpoint::new(kit_directory, Path::new(lock_name), &lock_path),
    ) {
        Ok(()) => {
            let publication_result = (|| {
                context.revalidate_auxiliary_identity(
                    DEFAULT_KIT_WRITE_LOCK_PATH,
                    candidate.identity,
                )?;
                sync_directory(fs, kit_directory, &lock_path)
            })();
            let cleanup_result = cleanup_published_candidate(
                context,
                fs,
                kit_directory,
                &transactions,
                &mut candidate,
            );
            let transactions_identity = transactions.identity;
            drop(transactions.directory);
            let directory_cleanup_result = try_cleanup_transactions_directory_by_identity(
                fs,
                kit_directory,
                context,
                transactions_identity,
            );

            if let Err(error) = publication_result {
                let lock_identity = candidate.identity;
                drop(candidate);
                best_effort_cleanup_failed_lock_publication(
                    context,
                    fs,
                    kit_directory,
                    lock_identity,
                );
                return Err(error);
            }
            if let Err(error) = cleanup_result {
                let lock_identity = candidate.identity;
                drop(candidate);
                best_effort_cleanup_failed_lock_publication(
                    context,
                    fs,
                    kit_directory,
                    lock_identity,
                );
                return Err(error);
            }

            match directory_cleanup_result {
                Ok(TransactionsDirectoryCleanupOutcome::Removed)
                | Ok(TransactionsDirectoryCleanupOutcome::Absent) => Ok(OpenedLock {
                    file: candidate.file,
                    identity: candidate.identity,
                    path: lock_path,
                    locked: true,
                }),
                Ok(TransactionsDirectoryCleanupOutcome::NotQuiescent(_)) => {
                    let identity = candidate.identity;
                    drop(candidate);
                    #[cfg(windows)]
                    finish_deferred_held_lock_alias_cleanup(
                        context,
                        fs,
                        kit_directory,
                        transactions_identity,
                    )?;
                    converge_on_expected_published_lock(context, fs, identity)
                }
                #[cfg(windows)]
                Ok(TransactionsDirectoryCleanupOutcome::Failed(source))
                    if windows_namespace_delete_pending_error(&source) =>
                {
                    let identity = candidate.identity;
                    drop(candidate);
                    finish_deferred_held_lock_alias_cleanup(
                        context,
                        fs,
                        kit_directory,
                        transactions_identity,
                    )?;
                    converge_on_expected_published_lock(context, fs, identity)
                }
                Ok(TransactionsDirectoryCleanupOutcome::Failed(source)) => {
                    let lock_identity = candidate.identity;
                    drop(candidate);
                    best_effort_cleanup_failed_lock_publication(
                        context,
                        fs,
                        kit_directory,
                        lock_identity,
                    );
                    Err(CodegenError::Io {
                        path: context
                            .project_root()
                            .join("src/components/ui/_kit/.transactions"),
                        source,
                    })
                }
                Err(error) => {
                    let lock_identity = candidate.identity;
                    drop(candidate);
                    best_effort_cleanup_failed_lock_publication(
                        context,
                        fs,
                        kit_directory,
                        lock_identity,
                    );
                    Err(error)
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            abandon_candidate_for_convergence(context, fs, kit_directory, transactions, candidate)?;
            converge_on_published_lock(context, fs)
        }
        Err(source) => {
            let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
            Err(CodegenError::Io {
                path: lock_path,
                source,
            })
        }
    }
}

fn converge_on_expected_published_lock(
    context: &PlanningContext,
    fs: &dyn FsOps,
    expected_identity: ObjectIdentity,
) -> Result<OpenedLock, CodegenError> {
    let opened = converge_on_published_lock(context, fs)?;
    if opened.identity != expected_identity {
        return Err(CodegenError::UnsafePath {
            path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
            reason: "published advisory lock changed while its bootstrap alias was finalized"
                .to_owned(),
        });
    }
    Ok(opened)
}

fn best_effort_cleanup_failed_lock_publication(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    expected_lock_identity: ObjectIdentity,
) {
    let Ok(Some(mut opened)) = open_existing_lock(context, fs) else {
        return;
    };
    if opened.identity != expected_lock_identity {
        return;
    }
    if acquire_advisory_lock(fs, &opened.file, &opened.path).is_err() {
        return;
    }
    opened.locked = true;
    if validate_opened_lock(context, fs, &mut opened).is_err() {
        return;
    }
    let _ = cleanup_stale_candidates_before_planning(context, fs, kit_directory, opened);
}

fn converge_on_published_lock(
    context: &PlanningContext,
    fs: &dyn FsOps,
) -> Result<OpenedLock, CodegenError> {
    open_existing_lock(context, fs)?.ok_or_else(|| CodegenError::InvalidCoordinationState {
        path: "src/components/ui/_kit/.transactions".to_owned(),
        reason: "a private bootstrap candidate was claimed or detached without a published advisory lock; inspect the installer coordination entries before retrying"
            .to_owned(),
    })
}

fn acquire_advisory_lock(fs: &dyn FsOps, file: &File, path: &Path) -> Result<(), CodegenError> {
    match fs.try_lock(file, path) {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Err(CodegenError::WriteLockContended {
            path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
        }),
        Err(std::fs::TryLockError::Error(source)) => Err(CodegenError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn acquire_private_candidate_lock(
    fs: &dyn FsOps,
    file: &File,
    path: &Path,
) -> Result<bool, CodegenError> {
    match fs.try_lock(file, path) {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(source)) => Err(CodegenError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateKind {
    Lock,
    Ignore,
}

impl CandidateKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Lock => LOCK_CANDIDATE_PREFIX,
            Self::Ignore => IGNORE_CANDIDATE_PREFIX,
        }
    }

    fn final_mode(self) -> u32 {
        match self {
            Self::Lock => 0o600,
            Self::Ignore => 0o644,
        }
    }

    fn content(self) -> &'static [u8] {
        match self {
            Self::Lock => KIT_ADVISORY_LOCK_CONTENT,
            Self::Ignore => KIT_COORDINATION_IGNORE_CONTENT,
        }
    }
}

struct TransactionsDirectory {
    directory: Dir,
    identity: ObjectIdentity,
    path: PathBuf,
}

struct Candidate {
    name: String,
    path: PathBuf,
    file: File,
    source_guard: Option<File>,
    identity: ObjectIdentity,
}

type FinishedCandidateHandles = (File, Option<File>, ObjectIdentity);

struct CandidateAlias {
    name: String,
    path: PathBuf,
    file: File,
    identity: ObjectIdentity,
    kind: CandidateKind,
}

struct CandidateInventory {
    name: String,
    path: PathBuf,
    identity: ObjectIdentity,
    kind: CandidateKind,
    recover_owner_mode: bool,
}

struct ClaimedCandidate {
    aliases: Vec<CandidateAlias>,
    identity: ObjectIdentity,
}

enum CandidateAliasCleanupOutcome {
    Removed,
    Absent,
    #[cfg(windows)]
    Retry,
}

enum CandidateAliasCleanupAttempt {
    Removed,
    Absent,
    #[cfg(windows)]
    Retry,
    Failed(std::io::Error),
}

enum TransactionsDirectoryCleanupOutcome {
    Removed,
    Absent,
    NotQuiescent(std::io::Error),
    Failed(std::io::Error),
}

enum StaleCandidateCleanupOutcome {
    Complete,
    #[cfg(windows)]
    #[allow(dead_code)]
    HeldLockAliasDeferred {
        transactions_identity: ObjectIdentity,
    },
}

#[cfg(not(windows))]
enum CandidateCleanupOutcome {
    Removed,
    Absent,
    Failed(std::io::Error),
}

fn open_or_create_transactions_directory(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
) -> Result<TransactionsDirectory, CodegenError> {
    let path = context
        .project_root()
        .join("src/components/ui/_kit/.transactions");
    let mut created_identity = None;
    let result = (|| {
        require_current_kit_directory(context, kit_directory)?;
        inspect_metadata(fs, &path)?;
        let mut created = false;
        let mut metadata = match kit_directory.symlink_metadata(TRANSACTIONS_DIRECTORY_NAME) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs.before_create_directory(&path)
                    .map_err(|source| CodegenError::Io {
                        path: path.clone(),
                        source,
                    })?;
                require_current_kit_directory(context, kit_directory)?;
                match create_private_directory(kit_directory, TRANSACTIONS_DIRECTORY_NAME, &path) {
                    Ok(()) => {
                        created = true;
                        let metadata = kit_directory
                            .symlink_metadata(TRANSACTIONS_DIRECTORY_NAME)
                            .map_err(|source| CodegenError::Io {
                                path: path.clone(),
                                source,
                            })?;
                        ensure_safe_directory_metadata(
                            "src/components/ui/_kit/.transactions",
                            &metadata,
                        )?;
                        created_identity = Some(metadata_identity(&metadata));
                    }
                    Err(CodegenError::Io { source, .. })
                        if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                inspect_metadata(fs, &path)?;
                kit_directory
                    .symlink_metadata(TRANSACTIONS_DIRECTORY_NAME)
                    .map_err(|source| CodegenError::Io {
                        path: path.clone(),
                        source,
                    })?
            }
            Err(source) => {
                return Err(CodegenError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        ensure_safe_directory_metadata("src/components/ui/_kit/.transactions", &metadata)?;
        if created_identity.is_some_and(|identity| metadata_identity(&metadata) != identity) {
            return Err(detached_transactions_directory());
        }
        recover_restrictive_transactions_directory_mode(
            context,
            fs,
            kit_directory,
            &path,
            &mut metadata,
        )?;
        let identity = metadata_identity(&metadata);
        let directory = kit_directory
            .open_dir_nofollow(TRANSACTIONS_DIRECTORY_NAME)
            .map_err(|source| CodegenError::UnsafePath {
                path: "src/components/ui/_kit/.transactions".to_owned(),
                reason: format!("failed to open directory without following: {source}"),
            })?;
        let transactions = TransactionsDirectory {
            directory,
            identity,
            path: path.clone(),
        };
        if created {
            fs.after_create_directory(&transactions.path)
                .map_err(|source| CodegenError::Io {
                    path: transactions.path.clone(),
                    source,
                })?;
        }
        require_current_transactions_directory(context, fs, kit_directory, &transactions)?;
        if created {
            fs.set_directory_mode(&transactions.directory, &transactions.path, 0o700)
                .map_err(|source| CodegenError::Io {
                    path: transactions.path.clone(),
                    source,
                })?;
        }
        require_current_transactions_directory(context, fs, kit_directory, &transactions)?;
        if created {
            sync_directory(fs, kit_directory, &transactions.path)?;
        }
        Ok(transactions)
    })();
    if result.is_err()
        && let Some(identity) = created_identity
    {
        best_effort_cleanup_created_transactions_directory_by_identity(
            fs,
            kit_directory,
            context,
            identity,
        );
    }
    result
}

fn best_effort_cleanup_created_transactions_directory_by_identity(
    fs: &dyn FsOps,
    kit_directory: &Dir,
    context: &PlanningContext,
    identity: ObjectIdentity,
) {
    let path = context
        .project_root()
        .join("src/components/ui/_kit/.transactions");
    let recovered = (|| {
        require_current_kit_directory(context, kit_directory)?;
        inspect_metadata(fs, &path)?;
        let mut metadata = kit_directory
            .symlink_metadata(TRANSACTIONS_DIRECTORY_NAME)
            .map_err(|source| CodegenError::Io {
                path: path.clone(),
                source,
            })?;
        ensure_safe_directory_metadata("src/components/ui/_kit/.transactions", &metadata)?;
        if metadata_identity(&metadata) != identity {
            return Err(detached_transactions_directory());
        }
        recover_restrictive_transactions_directory_mode(
            context,
            fs,
            kit_directory,
            &path,
            &mut metadata,
        )?;
        validate_directory_mode(&metadata, 0o700, "src/components/ui/_kit/.transactions")
    })();
    if recovered.is_ok() {
        best_effort_cleanup_transactions_directory_by_identity(
            fs,
            kit_directory,
            context,
            identity,
        );
    }
}

fn open_existing_transactions_directory(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
) -> Result<Option<TransactionsDirectory>, CodegenError> {
    let path = context
        .project_root()
        .join("src/components/ui/_kit/.transactions");
    require_current_kit_directory(context, kit_directory)?;
    inspect_metadata(fs, &path)?;
    let mut metadata = match kit_directory.symlink_metadata(TRANSACTIONS_DIRECTORY_NAME) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CodegenError::Io {
                path: path.clone(),
                source,
            });
        }
    };
    ensure_safe_directory_metadata("src/components/ui/_kit/.transactions", &metadata)?;
    recover_restrictive_transactions_directory_mode(
        context,
        fs,
        kit_directory,
        &path,
        &mut metadata,
    )?;
    validate_directory_mode(&metadata, 0o700, "src/components/ui/_kit/.transactions")?;
    let identity = metadata_identity(&metadata);
    let directory = kit_directory
        .open_dir_nofollow(TRANSACTIONS_DIRECTORY_NAME)
        .map_err(|source| CodegenError::UnsafePath {
            path: "src/components/ui/_kit/.transactions".to_owned(),
            reason: format!("failed to open directory without following: {source}"),
        })?;
    let transactions = TransactionsDirectory {
        directory,
        identity,
        path,
    };
    require_current_transactions_directory(context, fs, kit_directory, &transactions)?;
    Ok(Some(transactions))
}

#[cfg(unix)]
fn recover_restrictive_transactions_directory_mode(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    path: &Path,
    metadata: &mut Metadata,
) -> Result<(), CodegenError> {
    use cap_std::fs::PermissionsExt;

    let mode = metadata.permissions().mode() & 0o7777;
    if mode == 0o700 || mode & !0o700 != 0 {
        return Ok(());
    }
    let identity = metadata_identity(metadata);
    require_current_kit_directory(context, kit_directory)?;
    fs.set_path_mode(
        kit_directory,
        Path::new(TRANSACTIONS_DIRECTORY_NAME),
        path,
        0o700,
    )
    .map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    require_current_kit_directory(context, kit_directory)?;
    inspect_metadata(fs, path)?;
    let current = kit_directory
        .symlink_metadata(TRANSACTIONS_DIRECTORY_NAME)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    ensure_safe_directory_metadata("src/components/ui/_kit/.transactions", &current)?;
    validate_directory_mode(&current, 0o700, "src/components/ui/_kit/.transactions")?;
    if metadata_identity(&current) != identity {
        return Err(detached_transactions_directory());
    }
    *metadata = current;
    Ok(())
}

#[cfg(not(unix))]
fn recover_restrictive_transactions_directory_mode(
    _context: &PlanningContext,
    _fs: &dyn FsOps,
    _kit_directory: &Dir,
    _path: &Path,
    _metadata: &mut Metadata,
) -> Result<(), CodegenError> {
    Ok(())
}

fn match_current_transactions_directory(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
) -> Result<bool, CodegenError> {
    require_current_kit_directory(context, kit_directory)?;
    if !match_transactions_directory_identity(fs, kit_directory, transactions)? {
        return Ok(false);
    }
    let current = kit_directory
        .symlink_metadata(TRANSACTIONS_DIRECTORY_NAME)
        .map_err(|source| CodegenError::Io {
            path: transactions.path.clone(),
            source,
        })?;
    validate_directory_mode(&current, 0o700, "src/components/ui/_kit/.transactions")?;
    let opened = transactions
        .directory
        .dir_metadata()
        .map_err(|source| CodegenError::Io {
            path: transactions.path.clone(),
            source,
        })?;
    validate_directory_mode(&opened, 0o700, "src/components/ui/_kit/.transactions")?;
    Ok(true)
}

fn match_transactions_directory_identity(
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
) -> Result<bool, CodegenError> {
    inspect_metadata(fs, &transactions.path)?;
    let metadata = match kit_directory.symlink_metadata(TRANSACTIONS_DIRECTORY_NAME) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(CodegenError::Io {
                path: transactions.path.clone(),
                source,
            });
        }
    };
    ensure_safe_directory_metadata("src/components/ui/_kit/.transactions", &metadata)?;
    if metadata_identity(&metadata) != transactions.identity {
        return Err(detached_transactions_directory());
    }
    let opened = transactions
        .directory
        .dir_metadata()
        .map_err(|source| CodegenError::Io {
            path: transactions.path.clone(),
            source,
        })?;
    ensure_safe_directory_metadata("src/components/ui/_kit/.transactions", &opened)?;
    if metadata_identity(&opened) != transactions.identity {
        return Err(detached_transactions_directory());
    }
    Ok(true)
}

fn require_current_transactions_directory(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
) -> Result<(), CodegenError> {
    if match_current_transactions_directory(context, fs, kit_directory, transactions)? {
        Ok(())
    } else {
        Err(detached_transactions_directory())
    }
}

fn detached_transactions_directory() -> CodegenError {
    CodegenError::UnsafePath {
        path: "src/components/ui/_kit/.transactions".to_owned(),
        reason: "directory changed while held by the coordination bootstrap".to_owned(),
    }
}

fn create_candidate(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    kind: CandidateKind,
) -> Result<Candidate, CodegenError> {
    for _ in 0..LOCK_CANDIDATE_CREATE_ATTEMPTS {
        let name = random_candidate_name(kind)?;
        let path = transactions.path.join(&name);
        require_current_transactions_directory(context, fs, kit_directory, transactions)?;
        match fs
            .create_new_file(&transactions.directory, Path::new(&name), &path, 0o600)
            .bind_empty(fs, &transactions.directory, Path::new(&name), &path)
        {
            Ok(created) => {
                let (file, source_guard, identity) =
                    finish_candidate_handles(fs, &transactions.directory, &name, &path, created)?;
                return Ok(Candidate {
                    name,
                    path,
                    file,
                    source_guard,
                    identity,
                });
            }
            Err(ExclusiveCreateFailure::NotCreated(source))
                if source.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                continue;
            }
            Err(ExclusiveCreateFailure::NotCreated(source)) => {
                return Err(CodegenError::Io { path, source });
            }
            Err(ExclusiveCreateFailure::CreatedUnverified { created, source }) => {
                let _candidate_capability = created;
                return Err(CodegenError::RecoveryRequired {
                    journal_path: path,
                    reason: format!(
                        "coordination-candidate creation changed the namespace but its live owner \
                         capability could not be rebound: {source}"
                    ),
                });
            }
        }
    }
    Err(CodegenError::Io {
        path: transactions.path.clone(),
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique coordination candidate",
        ),
    })
}

#[cfg(windows)]
fn finish_candidate_handles(
    fs: &dyn FsOps,
    parent: &Dir,
    name: &str,
    path: &Path,
    created: CreatedFile,
) -> Result<FinishedCandidateHandles, CodegenError> {
    let identity = created.identity();
    let source_guard = created.file;
    let owner = match fs.open_candidate_owner(parent, Path::new(name), path) {
        Ok(owner) => owner,
        Err(source) => {
            let original = CodegenError::Io {
                path: path.to_path_buf(),
                source,
            };
            let _ = remove_file_by_handle_with_retry(fs, source_guard, path);
            return Err(original);
        }
    };
    if owner.identity() != identity {
        let original = CodegenError::UnsafePath {
            path: path.to_string_lossy().into_owned(),
            reason: "coordination candidate changed while its Windows owner was opened".to_owned(),
        };
        let _ = remove_file_by_handle_with_retry(fs, source_guard, path);
        return Err(original);
    }
    Ok((owner.file, Some(source_guard), identity))
}

#[cfg(windows)]
fn remove_file_by_handle_with_retry(
    fs: &dyn FsOps,
    file: File,
    path: &Path,
) -> std::io::Result<()> {
    match fs.remove_file_by_handle(file, path) {
        Ok(()) => Ok(()),
        Err(first) => {
            let original = first.source;
            let _ = fs.remove_file_by_handle(first.file, path);
            Err(original)
        }
    }
}

#[cfg(not(windows))]
fn finish_candidate_handles(
    _fs: &dyn FsOps,
    _parent: &Dir,
    _name: &str,
    _path: &Path,
    created: CreatedFile,
) -> Result<FinishedCandidateHandles, CodegenError> {
    let identity = created.identity();
    Ok((created.file, None, identity))
}

fn random_candidate_name(kind: CandidateKind) -> Result<String, CodegenError> {
    let mut random = [0_u8; LOCK_CANDIDATE_RANDOM_BYTES];
    getrandom::fill(&mut random).map_err(|error| CodegenError::Io {
        path: PathBuf::from("src/components/ui/_kit/.transactions"),
        source: std::io::Error::other(format!("random coordination-candidate name: {error}")),
    })?;
    let mut name = String::with_capacity(kind.prefix().len() + random.len() * 2);
    name.push_str(kind.prefix());
    for byte in random {
        use std::fmt::Write as _;
        write!(&mut name, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(name)
}

fn candidate_is_current(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    candidate: &Candidate,
) -> Result<bool, CodegenError> {
    if !match_current_transactions_directory(context, fs, kit_directory, transactions)? {
        return Ok(false);
    }
    inspect_metadata(fs, &candidate.path)?;
    let metadata = match transactions.directory.symlink_metadata(&candidate.name) {
        Ok(metadata) => metadata,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || windows_namespace_delete_pending_error(&error) =>
        {
            return Ok(false);
        }
        Err(source) => {
            return Err(CodegenError::Io {
                path: candidate.path.clone(),
                source,
            });
        }
    };
    ensure_safe_regular_metadata(candidate.path.to_string_lossy().as_ref(), &metadata)?;
    if metadata_identity(&metadata) != candidate.identity {
        return Err(CodegenError::UnsafePath {
            path: candidate.path.to_string_lossy().into_owned(),
            reason: "coordination candidate identity changed".to_owned(),
        });
    }
    Ok(true)
}

fn require_current_candidate(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    candidate: &Candidate,
) -> Result<(), CodegenError> {
    if candidate_is_current(context, fs, kit_directory, transactions, candidate)? {
        Ok(())
    } else {
        Err(CodegenError::UnsafePath {
            path: candidate.path.to_string_lossy().into_owned(),
            reason: "coordination candidate was detached before mutation".to_owned(),
        })
    }
}

fn validate_candidate_source_guard(candidate: &Candidate) -> Result<(), CodegenError> {
    #[cfg(windows)]
    {
        let guard = candidate.source_guard.as_ref().ok_or_else(|| {
            CodegenError::InvalidCoordinationState {
                path: candidate.path.to_string_lossy().into_owned(),
                reason: "Windows coordination candidate is missing its source guard".to_owned(),
            }
        })?;
        if file_identity(guard, &candidate.path)? != candidate.identity {
            return Err(CodegenError::UnsafePath {
                path: candidate.path.to_string_lossy().into_owned(),
                reason: "Windows coordination candidate source guard changed identity".to_owned(),
            });
        }
    }
    #[cfg(not(windows))]
    if candidate.source_guard.is_some() {
        return Err(CodegenError::InvalidCoordinationState {
            path: candidate.path.to_string_lossy().into_owned(),
            reason: "unexpected coordination candidate source guard".to_owned(),
        });
    }
    Ok(())
}

fn cleanup_stale_lock_candidates(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    held_lock_identity: ObjectIdentity,
) -> Result<StaleCandidateCleanupOutcome, CodegenError> {
    'quiescence: for _ in 0..CLEANUP_QUIESCENCE_ATTEMPTS {
        let Some(transactions) = open_existing_transactions_directory(context, fs, kit_directory)?
        else {
            return Ok(StaleCandidateCleanupOutcome::Complete);
        };

        let mut names = Vec::new();
        for entry in transactions
            .directory
            .entries()
            .map_err(|source| CodegenError::Io {
                path: transactions.path.clone(),
                source,
            })?
        {
            let entry = entry.map_err(|source| CodegenError::Io {
                path: transactions.path.clone(),
                source,
            })?;
            let name = entry.file_name();
            if transaction_journal_name(&name)
                || journal_update_name(&name)
                || journal_v2_authority_name(&name)
            {
                continue;
            }
            if candidate_kind(&name).is_none() {
                return Err(invalid_transactions_entry(&name));
            }
            names.push(name);
        }
        names.sort();

        let mut inventory_by_identity = BTreeMap::<ObjectIdentity, Vec<CandidateInventory>>::new();
        for name in names {
            let kind = candidate_kind(&name).ok_or_else(|| invalid_transactions_entry(&name))?;
            let name = name
                .into_string()
                .map_err(|name| invalid_transactions_entry(&name))?;
            let candidate_path = transactions.path.join(&name);
            inspect_metadata(fs, &candidate_path)?;
            let metadata = match transactions.directory.symlink_metadata(&name) {
                Ok(metadata) => metadata,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        || windows_namespace_delete_pending_error(&error) =>
                {
                    continue 'quiescence;
                }
                Err(source) => {
                    return Err(CodegenError::Io {
                        path: candidate_path,
                        source,
                    });
                }
            };
            ensure_safe_regular_metadata(candidate_path.to_string_lossy().as_ref(), &metadata)?;
            let identity = metadata_identity(&metadata);
            let recover_owner_mode = validate_candidate_inventory_mode(
                kind,
                coordination_metadata_mode(&metadata),
                identity == held_lock_identity,
                &candidate_path,
            )?;
            inventory_by_identity
                .entry(identity)
                .or_default()
                .push(CandidateInventory {
                    name,
                    path: candidate_path,
                    identity,
                    kind,
                    recover_owner_mode,
                });
        }

        for (identity, aliases) in &inventory_by_identity {
            let kind = aliases
                .first()
                .expect("an inventoried identity has at least one alias")
                .kind;
            if *identity == held_lock_identity && kind != CandidateKind::Lock {
                return Err(CodegenError::InvalidCoordinationState {
                    path: aliases[0].path.to_string_lossy().into_owned(),
                    reason: "non-lock candidate aliases the persistent advisory lock".to_owned(),
                });
            }
            if aliases.iter().any(|alias| alias.kind != kind) {
                return Err(CodegenError::InvalidCoordinationState {
                    path: aliases[0].path.to_string_lossy().into_owned(),
                    reason: "candidate aliases for one inode use conflicting lifecycle kinds"
                        .to_owned(),
                });
            }
        }

        for (identity, aliases) in &inventory_by_identity {
            if !aliases.iter().any(|alias| alias.recover_owner_mode) {
                continue;
            }
            let expected_links = aliases.len() as u64 + u64::from(*identity == held_lock_identity);
            for alias in aliases {
                require_current_transactions_directory(context, fs, kit_directory, &transactions)?;
                inspect_metadata(fs, &alias.path)?;
                let metadata = match transactions.directory.symlink_metadata(&alias.name) {
                    Ok(metadata) => metadata,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            || windows_namespace_delete_pending_error(&error) =>
                    {
                        continue 'quiescence;
                    }
                    Err(source) => {
                        return Err(CodegenError::Io {
                            path: alias.path.clone(),
                            source,
                        });
                    }
                };
                ensure_safe_regular_metadata(alias.path.to_string_lossy().as_ref(), &metadata)?;
                if metadata_identity(&metadata) != *identity {
                    return Err(CodegenError::UnsafePath {
                        path: alias.path.to_string_lossy().into_owned(),
                        reason: "coordination candidate changed before owner-mode recovery"
                            .to_owned(),
                    });
                }
                if metadata.nlink() != expected_links {
                    return Err(CodegenError::InvalidCoordinationState {
                        path: alias.path.to_string_lossy().into_owned(),
                        reason: "owner-mode recovery cannot prove that every hard-link alias is installer-owned"
                            .to_owned(),
                    });
                }
            }

            let representative = aliases
                .first()
                .expect("an inventoried identity has at least one alias");
            match fs.set_path_mode(
                &transactions.directory,
                Path::new(&representative.name),
                &representative.path,
                0o600,
            ) {
                Ok(()) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        || windows_namespace_delete_pending_error(&error) =>
                {
                    continue 'quiescence;
                }
                Err(source) => {
                    return Err(CodegenError::Io {
                        path: representative.path.clone(),
                        source,
                    });
                }
            }

            for alias in aliases {
                require_current_transactions_directory(context, fs, kit_directory, &transactions)?;
                inspect_metadata(fs, &alias.path)?;
                let metadata = match transactions.directory.symlink_metadata(&alias.name) {
                    Ok(metadata) => metadata,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            || windows_namespace_delete_pending_error(&error) =>
                    {
                        continue 'quiescence;
                    }
                    Err(source) => {
                        return Err(CodegenError::Io {
                            path: alias.path.clone(),
                            source,
                        });
                    }
                };
                ensure_safe_regular_metadata(alias.path.to_string_lossy().as_ref(), &metadata)?;
                if metadata_identity(&metadata) != alias.identity {
                    return Err(CodegenError::UnsafePath {
                        path: alias.path.to_string_lossy().into_owned(),
                        reason: "coordination candidate changed during owner-mode recovery"
                            .to_owned(),
                    });
                }
                if metadata.nlink() != expected_links {
                    return Err(CodegenError::InvalidCoordinationState {
                        path: alias.path.to_string_lossy().into_owned(),
                        reason: "hard-link aliases changed during owner-mode recovery".to_owned(),
                    });
                }
                validate_optional_mode(coordination_metadata_mode(&metadata), 0o600, &alias.path)?;
            }
        }

        let mut claimed = Vec::with_capacity(inventory_by_identity.len());
        for (identity, inventory) in inventory_by_identity {
            let mut aliases = Vec::with_capacity(inventory.len());
            for candidate in inventory {
                require_current_transactions_directory(context, fs, kit_directory, &transactions)?;
                inspect_metadata(fs, &candidate.path)?;
                let metadata = match transactions.directory.symlink_metadata(&candidate.name) {
                    Ok(metadata) => metadata,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            || windows_namespace_delete_pending_error(&error) =>
                    {
                        continue 'quiescence;
                    }
                    Err(source) => {
                        return Err(CodegenError::Io {
                            path: candidate.path,
                            source,
                        });
                    }
                };
                validate_inventoried_candidate_metadata(&candidate, &metadata, held_lock_identity)?;

                let mut options = OpenOptions::new();
                options.read(true).write(true);
                options.follow(FollowSymlinks::No);
                options.nonblock(true);
                let file = match transactions.directory.open_with(&candidate.name, &options) {
                    Ok(file) => file,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            || windows_namespace_delete_pending_error(&error) =>
                    {
                        continue 'quiescence;
                    }
                    Err(source) => {
                        return Err(CodegenError::Io {
                            path: candidate.path.clone(),
                            source,
                        });
                    }
                };
                let opened_identity = file_identity(&file, &candidate.path)?;
                if opened_identity != candidate.identity {
                    return Err(CodegenError::UnsafePath {
                        path: candidate.path.to_string_lossy().into_owned(),
                        reason: "coordination candidate changed while its claim handle was opened"
                            .to_owned(),
                    });
                }
                inspect_metadata(fs, &candidate.path)?;
                let current = match transactions.directory.symlink_metadata(&candidate.name) {
                    Ok(metadata) => metadata,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            || windows_namespace_delete_pending_error(&error) =>
                    {
                        continue 'quiescence;
                    }
                    Err(source) => {
                        return Err(CodegenError::Io {
                            path: candidate.path.clone(),
                            source,
                        });
                    }
                };
                validate_inventoried_candidate_metadata(&candidate, &current, held_lock_identity)?;
                aliases.push(CandidateAlias {
                    name: candidate.name,
                    path: candidate.path,
                    file,
                    identity: candidate.identity,
                    kind: candidate.kind,
                });
            }
            claimed.push(ClaimedCandidate { aliases, identity });
        }

        for candidate in &mut claimed {
            for alias in &candidate.aliases {
                if alias.identity != candidate.identity
                    || file_identity(&alias.file, &alias.path)? != candidate.identity
                {
                    return Err(CodegenError::UnsafePath {
                        path: alias.path.to_string_lossy().into_owned(),
                        reason: "claimed coordination candidate handle changed identity".to_owned(),
                    });
                }
            }
            let representative = candidate
                .aliases
                .first_mut()
                .expect("an inventoried identity has at least one claimed alias");
            let aliases_held_lock = candidate.identity == held_lock_identity;
            if !aliases_held_lock {
                match fs.try_lock(&representative.file, &representative.path) {
                    Err(std::fs::TryLockError::WouldBlock) => {
                        return Err(CodegenError::WriteLockContended {
                            path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
                        });
                    }
                    Err(std::fs::TryLockError::Error(source)) => {
                        return Err(CodegenError::Io {
                            path: representative.path.clone(),
                            source,
                        });
                    }
                    Ok(()) => {}
                }
            }
            validate_claimed_candidate(
                fs,
                &mut representative.file,
                &representative.path,
                representative.kind,
                aliases_held_lock,
            )?;
        }

        require_current_transactions_directory(context, fs, kit_directory, &transactions)?;
        let mut removed_any = false;
        #[cfg(not(windows))]
        for candidate in &claimed {
            for alias in &candidate.aliases {
                match cleanup_claimed_candidate_alias(
                    context,
                    fs,
                    kit_directory,
                    &transactions,
                    alias,
                    candidate.identity,
                    candidate.identity == held_lock_identity,
                )? {
                    CandidateAliasCleanupOutcome::Removed => {
                        removed_any = true;
                    }
                    CandidateAliasCleanupOutcome::Absent => {}
                }
            }
        }
        #[cfg(windows)]
        let restart = {
            let mut restart = false;
            'removal: for candidate in &claimed {
                for alias in &candidate.aliases {
                    match cleanup_claimed_candidate_alias(
                        context,
                        fs,
                        kit_directory,
                        &transactions,
                        alias,
                        candidate.identity,
                        candidate.identity == held_lock_identity,
                    )? {
                        CandidateAliasCleanupOutcome::Removed => {
                            removed_any = true;
                        }
                        CandidateAliasCleanupOutcome::Absent => {}
                        CandidateAliasCleanupOutcome::Retry => {
                            restart = true;
                            break 'removal;
                        }
                    }
                }
            }
            restart
        };
        if removed_any {
            sync_directory(fs, &transactions.directory, &transactions.path)?;
        }
        #[cfg(windows)]
        if restart {
            return Err(CodegenError::WriteLockContended {
                path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
            });
        }

        require_current_transactions_directory(context, fs, kit_directory, &transactions)?;
        let mut candidate_remains = false;
        for entry in transactions
            .directory
            .entries()
            .map_err(|source| CodegenError::Io {
                path: transactions.path.clone(),
                source,
            })?
        {
            let entry = entry.map_err(|source| CodegenError::Io {
                path: transactions.path.clone(),
                source,
            })?;
            let name = entry.file_name();
            if candidate_kind(&name).is_some() {
                candidate_remains = true;
                continue;
            }
            if !transaction_journal_name(&name)
                && !journal_update_name(&name)
                && !journal_v2_authority_name(&name)
            {
                return Err(invalid_transactions_entry(&name));
            }
        }
        if candidate_remains {
            continue 'quiescence;
        }
        return Ok(StaleCandidateCleanupOutcome::Complete);
    }

    Err(CodegenError::InvalidCoordinationState {
        path: "src/components/ui/_kit/.transactions".to_owned(),
        reason: "candidate inventory did not quiesce during bounded cleanup".to_owned(),
    })
}

fn invalid_transactions_entry(name: &OsStr) -> CodegenError {
    CodegenError::InvalidCoordinationState {
        path: format!(
            "src/components/ui/_kit/.transactions/{}",
            name.to_string_lossy()
        ),
        reason: "unexpected bootstrap entry".to_owned(),
    }
}

fn candidate_kind(name: &OsStr) -> Option<CandidateKind> {
    let name = name.to_str()?;
    [CandidateKind::Lock, CandidateKind::Ignore]
        .into_iter()
        .find(|kind| {
            name.strip_prefix(kind.prefix()).is_some_and(|suffix| {
                suffix.len() == LOCK_CANDIDATE_RANDOM_BYTES * 2
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
}

fn transaction_journal_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.strip_prefix(TRANSACTION_JOURNAL_PREFIX)
        .and_then(|value| value.strip_suffix(TRANSACTION_JOURNAL_SUFFIX))
        .is_some_and(|suffix| {
            suffix.len() == LOCK_CANDIDATE_RANDOM_BYTES * 2
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn journal_update_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.strip_prefix(JOURNAL_UPDATE_PREFIX)
        .is_some_and(|suffix| {
            suffix.len() == LOCK_CANDIDATE_RANDOM_BYTES * 2
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn journal_v2_authority_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    parse_transaction_directory_name(name).is_ok()
        || parse_bootstrap_intent_name(name).is_ok()
        || parse_finalization_file_name(name).is_ok()
}

#[cfg(unix)]
fn coordination_metadata_mode(metadata: &Metadata) -> Option<u32> {
    use cap_std::fs::PermissionsExt;

    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn coordination_metadata_mode(_metadata: &Metadata) -> Option<u32> {
    None
}

fn validate_candidate_inventory_mode(
    kind: CandidateKind,
    actual: Option<u32>,
    aliases_held_lock: bool,
    path: &Path,
) -> Result<bool, CodegenError> {
    if aliases_held_lock {
        if kind != CandidateKind::Lock {
            return Err(CodegenError::InvalidCoordinationState {
                path: path.to_string_lossy().into_owned(),
                reason: "non-lock candidate aliases the persistent advisory lock".to_owned(),
            });
        }
        validate_optional_mode(actual, 0o600, path)?;
        return Ok(false);
    }

    let Some(actual) = actual else {
        return Ok(false);
    };
    if actual == kind.final_mode() || actual == 0o600 {
        return Ok(false);
    }
    if actual & !0o600 == 0 {
        return Ok(true);
    }
    Err(CodegenError::InvalidCoordinationState {
        path: path.to_string_lossy().into_owned(),
        reason: "coordination candidate has an unsupported lifecycle mode".to_owned(),
    })
}

fn validate_inventoried_candidate_metadata(
    candidate: &CandidateInventory,
    metadata: &Metadata,
    held_lock_identity: ObjectIdentity,
) -> Result<(), CodegenError> {
    ensure_safe_regular_metadata(candidate.path.to_string_lossy().as_ref(), metadata)?;
    if metadata_identity(metadata) != candidate.identity {
        return Err(CodegenError::UnsafePath {
            path: candidate.path.to_string_lossy().into_owned(),
            reason: "coordination candidate changed after inventory".to_owned(),
        });
    }
    if validate_candidate_inventory_mode(
        candidate.kind,
        coordination_metadata_mode(metadata),
        candidate.identity == held_lock_identity,
        &candidate.path,
    )? {
        return Err(CodegenError::InvalidCoordinationState {
            path: candidate.path.to_string_lossy().into_owned(),
            reason: "coordination candidate became owner-inaccessible after mode recovery"
                .to_owned(),
        });
    }
    Ok(())
}

fn cleanup_claimed_candidate_alias(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    alias: &CandidateAlias,
    identity: ObjectIdentity,
    aliases_held_lock: bool,
) -> Result<CandidateAliasCleanupOutcome, CodegenError> {
    let first_attempt = try_cleanup_claimed_candidate_alias(
        context,
        fs,
        kit_directory,
        transactions,
        alias,
        identity,
        aliases_held_lock,
    );
    let outcome = match first_attempt {
        Ok(outcome) => outcome,
        Err(original) => {
            let _ = try_cleanup_claimed_candidate_alias(
                context,
                fs,
                kit_directory,
                transactions,
                alias,
                identity,
                aliases_held_lock,
            );
            return Err(original);
        }
    };
    match outcome {
        CandidateAliasCleanupAttempt::Removed => Ok(CandidateAliasCleanupOutcome::Removed),
        CandidateAliasCleanupAttempt::Absent => Ok(CandidateAliasCleanupOutcome::Absent),
        #[cfg(windows)]
        CandidateAliasCleanupAttempt::Retry => Ok(CandidateAliasCleanupOutcome::Retry),
        CandidateAliasCleanupAttempt::Failed(source) => {
            let original = CodegenError::Io {
                path: alias.path.clone(),
                source,
            };
            let _ = try_cleanup_claimed_candidate_alias(
                context,
                fs,
                kit_directory,
                transactions,
                alias,
                identity,
                aliases_held_lock,
            );
            Err(original)
        }
    }
}

fn try_cleanup_claimed_candidate_alias(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    alias: &CandidateAlias,
    identity: ObjectIdentity,
    aliases_held_lock: bool,
) -> Result<CandidateAliasCleanupAttempt, CodegenError> {
    require_current_transactions_directory(context, fs, kit_directory, transactions)?;
    if alias.identity != identity || file_identity(&alias.file, &alias.path)? != identity {
        return Err(CodegenError::UnsafePath {
            path: alias.path.to_string_lossy().into_owned(),
            reason: "claimed coordination candidate handle changed identity".to_owned(),
        });
    }
    inspect_metadata(fs, &alias.path)?;
    let metadata = match transactions.directory.symlink_metadata(&alias.name) {
        Ok(metadata) => metadata,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || windows_namespace_delete_pending_error(&error) =>
        {
            return Ok(CandidateAliasCleanupAttempt::Absent);
        }
        Err(source) => {
            return Err(CodegenError::Io {
                path: alias.path.clone(),
                source,
            });
        }
    };
    ensure_safe_regular_metadata(alias.path.to_string_lossy().as_ref(), &metadata)?;
    if metadata_identity(&metadata) != identity {
        return Err(CodegenError::UnsafePath {
            path: alias.path.to_string_lossy().into_owned(),
            reason: "coordination candidate identity changed before cleanup".to_owned(),
        });
    }
    if validate_candidate_inventory_mode(
        alias.kind,
        coordination_metadata_mode(&metadata),
        aliases_held_lock,
        &alias.path,
    )? {
        return Err(CodegenError::InvalidCoordinationState {
            path: alias.path.to_string_lossy().into_owned(),
            reason: "coordination candidate became owner-inaccessible before cleanup".to_owned(),
        });
    }

    #[cfg(windows)]
    {
        let opened = match fs.open_file_for_cleanup(
            &transactions.directory,
            Path::new(&alias.name),
            &alias.path,
        ) {
            Ok(opened) => opened,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || windows_namespace_delete_pending_error(&error) =>
            {
                return Ok(CandidateAliasCleanupAttempt::Absent);
            }
            Err(error) if transient_windows_sharing_error(&error) => {
                return Ok(CandidateAliasCleanupAttempt::Retry);
            }
            Err(source) => return Ok(CandidateAliasCleanupAttempt::Failed(source)),
        };
        let cleanup_identity = opened.identity();
        let cleanup_file = opened.file;
        if cleanup_identity != identity || file_identity(&cleanup_file, &alias.path)? != identity {
            return Err(CodegenError::UnsafePath {
                path: alias.path.to_string_lossy().into_owned(),
                reason: "coordination candidate changed while its cleanup handle was opened"
                    .to_owned(),
            });
        }
        inspect_metadata(fs, &alias.path)?;
        let current = match transactions.directory.symlink_metadata(&alias.name) {
            Ok(metadata) => metadata,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || windows_namespace_delete_pending_error(&error) =>
            {
                return Ok(CandidateAliasCleanupAttempt::Absent);
            }
            Err(source) => {
                return Err(CodegenError::Io {
                    path: alias.path.clone(),
                    source,
                });
            }
        };
        ensure_safe_regular_metadata(alias.path.to_string_lossy().as_ref(), &current)?;
        if metadata_identity(&current) != identity {
            return Err(CodegenError::UnsafePath {
                path: alias.path.to_string_lossy().into_owned(),
                reason: "coordination candidate changed before handle-relative cleanup".to_owned(),
            });
        }
        match fs.remove_file_by_handle(cleanup_file, &alias.path) {
            Ok(()) => Ok(CandidateAliasCleanupAttempt::Removed),
            Err(error) if error.source.kind() == std::io::ErrorKind::NotFound => {
                Ok(CandidateAliasCleanupAttempt::Absent)
            }
            Err(error) if transient_windows_sharing_error(&error.source) => {
                Ok(CandidateAliasCleanupAttempt::Retry)
            }
            Err(error) => Ok(CandidateAliasCleanupAttempt::Failed(error.source)),
        }
    }

    #[cfg(not(windows))]
    match fs.remove_file(&transactions.directory, Path::new(&alias.name), &alias.path) {
        Ok(()) => Ok(CandidateAliasCleanupAttempt::Removed),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(CandidateAliasCleanupAttempt::Absent)
        }
        Err(source) => Ok(CandidateAliasCleanupAttempt::Failed(source)),
    }
}

fn cleanup_transactions_directory_after_drop(
    fs: &dyn FsOps,
    kit_directory: &Dir,
    context: &PlanningContext,
    identity: ObjectIdentity,
    directory: Dir,
) -> Result<(), CodegenError> {
    drop(directory);
    cleanup_transactions_directory_after_drop_by_identity(fs, kit_directory, context, identity)
}

fn cleanup_transactions_directory_after_drop_by_identity(
    fs: &dyn FsOps,
    kit_directory: &Dir,
    context: &PlanningContext,
    identity: ObjectIdentity,
) -> Result<(), CodegenError> {
    let first_attempt =
        try_cleanup_transactions_directory_by_identity(fs, kit_directory, context, identity);
    let outcome = match first_attempt {
        Ok(outcome) => outcome,
        Err(original) => {
            best_effort_cleanup_transactions_directory_by_identity(
                fs,
                kit_directory,
                context,
                identity,
            );
            return Err(original);
        }
    };
    match outcome {
        TransactionsDirectoryCleanupOutcome::Removed
        | TransactionsDirectoryCleanupOutcome::Absent => Ok(()),
        TransactionsDirectoryCleanupOutcome::NotQuiescent(source)
        | TransactionsDirectoryCleanupOutcome::Failed(source) => {
            best_effort_cleanup_transactions_directory_by_identity(
                fs,
                kit_directory,
                context,
                identity,
            );
            Err(CodegenError::Io {
                path: context
                    .project_root()
                    .join("src/components/ui/_kit/.transactions"),
                source,
            })
        }
    }
}

fn best_effort_cleanup_transactions_directory_by_identity(
    fs: &dyn FsOps,
    kit_directory: &Dir,
    context: &PlanningContext,
    identity: ObjectIdentity,
) {
    let _ = try_cleanup_transactions_directory_by_identity(fs, kit_directory, context, identity);
}

fn try_cleanup_transactions_directory_by_identity(
    fs: &dyn FsOps,
    kit_directory: &Dir,
    context: &PlanningContext,
    identity: ObjectIdentity,
) -> Result<TransactionsDirectoryCleanupOutcome, CodegenError> {
    let path = context
        .project_root()
        .join("src/components/ui/_kit/.transactions");
    require_current_kit_directory(context, kit_directory)?;
    inspect_metadata(fs, &path)?;
    let metadata = match kit_directory.symlink_metadata(TRANSACTIONS_DIRECTORY_NAME) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TransactionsDirectoryCleanupOutcome::Absent);
        }
        Err(source) => {
            return Err(CodegenError::Io {
                path: path.clone(),
                source,
            });
        }
    };
    ensure_safe_directory_metadata("src/components/ui/_kit/.transactions", &metadata)?;
    validate_directory_mode(&metadata, 0o700, "src/components/ui/_kit/.transactions")?;
    if metadata_identity(&metadata) != identity {
        return Err(detached_transactions_directory());
    }
    require_current_kit_directory(context, kit_directory)?;
    match fs.remove_dir(kit_directory, Path::new(TRANSACTIONS_DIRECTORY_NAME), &path) {
        Ok(()) => {
            sync_directory(fs, kit_directory, &path)?;
            Ok(TransactionsDirectoryCleanupOutcome::Removed)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(TransactionsDirectoryCleanupOutcome::Absent)
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::DirectoryNotEmpty
                || transient_windows_sharing_error(&error) =>
        {
            Ok(TransactionsDirectoryCleanupOutcome::NotQuiescent(error))
        }
        Err(source) => Ok(TransactionsDirectoryCleanupOutcome::Failed(source)),
    }
}

fn bootstrap_coordination_ignore(
    context: &PlanningContext,
    fs: &dyn FsOps,
    pinned_kit: &PinnedKitDirectories,
    kit_directory: &Dir,
) -> Result<(), CodegenError> {
    let full_path = context
        .project_root()
        .join(DEFAULT_KIT_COORDINATION_IGNORE_PATH);
    if let Some(mut file) = open_existing_coordination_file(context, fs)? {
        return validate_coordination_ignore(context, fs, &mut file, &full_path);
    }

    let transactions = open_or_create_transactions_directory(context, fs, kit_directory)?;
    let mut candidate = match create_candidate(
        context,
        fs,
        kit_directory,
        &transactions,
        CandidateKind::Ignore,
    ) {
        Ok(candidate) => candidate,
        Err(error) => {
            let _ = cleanup_transactions_directory_after_drop(
                fs,
                kit_directory,
                context,
                transactions.identity,
                transactions.directory,
            );
            return Err(error);
        }
    };
    match acquire_private_candidate_lock(fs, &candidate.file, &candidate.path) {
        Ok(true) => {}
        Ok(false) => {
            let path = candidate.path.to_string_lossy().into_owned();
            drop(candidate);
            drop(transactions.directory);
            return Err(CodegenError::InvalidCoordinationState {
                path,
                reason: "new ignore candidate was unexpectedly claimed".to_owned(),
            });
        }
        Err(error) => {
            let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
            return Err(error);
        }
    }
    match candidate_is_current(context, fs, kit_directory, &transactions, &candidate) {
        Ok(true) => {}
        Ok(false) => {
            let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
            return Err(CodegenError::InvalidCoordinationState {
                path: DEFAULT_KIT_COORDINATION_IGNORE_PATH.to_owned(),
                reason: "ignore candidate disappeared before publication".to_owned(),
            });
        }
        Err(error) => {
            let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
            return Err(error);
        }
    }

    let prepare_result = (|| {
        require_current_candidate(context, fs, kit_directory, &transactions, &candidate)?;
        set_file_mode(fs, &candidate.file, 0o644, &candidate.path)?;
        require_current_candidate(context, fs, kit_directory, &transactions, &candidate)?;
        fs.write_handle(
            &mut candidate.file,
            &candidate.path,
            KIT_COORDINATION_IGNORE_CONTENT,
        )
        .map_err(|source| CodegenError::Io {
            path: candidate.path.clone(),
            source,
        })?;
        require_current_candidate(context, fs, kit_directory, &transactions, &candidate)?;
        fs.sync_handle(&candidate.file, &candidate.path)
            .map_err(|source| CodegenError::Io {
                path: candidate.path.clone(),
                source,
            })?;
        require_current_candidate(context, fs, kit_directory, &transactions, &candidate)
    })();
    if let Err(error) = prepare_result {
        let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
        return Err(error);
    }

    let publication_precheck = (|| {
        require_current_candidate(context, fs, kit_directory, &transactions, &candidate)?;
        validate_candidate_source_guard(&candidate)?;
        pinned_kit.revalidate()
    })();
    if let Err(error) = publication_precheck {
        let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
        return Err(error);
    }
    let candidate_source = match fs
        .read_regular_file_exact(
            &transactions.directory,
            Path::new(&candidate.name),
            &candidate.path,
            KIT_COORDINATION_IGNORE_CONTENT.len() as u64,
        )
        .map_err(|source| CodegenError::Io {
            path: candidate.path.clone(),
            source,
        }) {
        Ok(source) => source,
        Err(error) => {
            let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
            return Err(error);
        }
    };
    if candidate_source.bytes != KIT_COORDINATION_IGNORE_CONTENT
        || candidate_source.observation.identity != candidate.identity
        || candidate_source.observation.link_count != Some(1)
    {
        let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
        return Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_COORDINATION_IGNORE_PATH.to_owned(),
            reason: "ignore publication candidate is not the exact single-link prepared owner"
                .to_owned(),
        });
    }
    match fs.hard_link(
        pinned_kit.all(),
        HardLinkEndpoint::new(
            &transactions.directory,
            Path::new(&candidate.name),
            &candidate.path,
        ),
        &candidate_source.observation,
        HardLinkEndpoint::new(kit_directory, Path::new(".gitignore"), &full_path),
    ) {
        Ok(()) => {
            let publication_result = (|| {
                context.revalidate_auxiliary_identity(
                    DEFAULT_KIT_COORDINATION_IGNORE_PATH,
                    candidate.identity,
                )?;
                sync_directory(fs, kit_directory, &full_path)
            })();
            let cleanup_result = cleanup_published_candidate(
                context,
                fs,
                kit_directory,
                &transactions,
                &mut candidate,
            );
            let transactions_identity = transactions.identity;
            drop(candidate);
            drop(transactions.directory);
            let directory_cleanup_result = try_cleanup_transactions_directory_by_identity(
                fs,
                kit_directory,
                context,
                transactions_identity,
            );
            publication_result?;
            cleanup_result?;
            match directory_cleanup_result {
                Ok(TransactionsDirectoryCleanupOutcome::Removed)
                | Ok(TransactionsDirectoryCleanupOutcome::Absent)
                | Ok(TransactionsDirectoryCleanupOutcome::NotQuiescent(_)) => {}
                Ok(TransactionsDirectoryCleanupOutcome::Failed(source)) => {
                    best_effort_cleanup_transactions_directory_by_identity(
                        fs,
                        kit_directory,
                        context,
                        transactions_identity,
                    );
                    return Err(CodegenError::Io {
                        path: context
                            .project_root()
                            .join("src/components/ui/_kit/.transactions"),
                        source,
                    });
                }
                Err(error) => {
                    best_effort_cleanup_transactions_directory_by_identity(
                        fs,
                        kit_directory,
                        context,
                        transactions_identity,
                    );
                    return Err(error);
                }
            }
            let mut file = open_existing_coordination_file(context, fs)?.ok_or_else(|| {
                CodegenError::PreimageConflict {
                    path: DEFAULT_KIT_COORDINATION_IGNORE_PATH.to_owned(),
                    reason: "published ignore file disappeared".to_owned(),
                }
            })?;
            validate_coordination_ignore(context, fs, &mut file, &full_path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            abandon_candidate_for_convergence(context, fs, kit_directory, transactions, candidate)?;
            let mut file = open_existing_coordination_file(context, fs)?.ok_or_else(|| {
                CodegenError::PreimageConflict {
                    path: DEFAULT_KIT_COORDINATION_IGNORE_PATH.to_owned(),
                    reason: "concurrently published ignore file disappeared".to_owned(),
                }
            })?;
            validate_coordination_ignore(context, fs, &mut file, &full_path)
        }
        Err(source) => {
            let _ = abandon_candidate(context, fs, kit_directory, transactions, candidate);
            Err(CodegenError::Io {
                path: full_path,
                source,
            })
        }
    }
}

fn inspect_metadata(fs: &dyn FsOps, path: &Path) -> Result<(), CodegenError> {
    fs.before_inspect_metadata(path)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn set_file_mode(fs: &dyn FsOps, file: &File, mode: u32, path: &Path) -> Result<(), CodegenError> {
    fs.set_file_mode(file, path, mode)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(windows))]
fn cleanup_candidate(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    name: &str,
    path: &Path,
    identity: ObjectIdentity,
) -> Result<(), CodegenError> {
    let first_attempt = try_cleanup_candidate(
        context,
        fs,
        kit_directory,
        transactions,
        name,
        path,
        identity,
    );
    let outcome = match first_attempt {
        Ok(outcome) => outcome,
        Err(original) => {
            let _ = try_cleanup_candidate(
                context,
                fs,
                kit_directory,
                transactions,
                name,
                path,
                identity,
            );
            return Err(original);
        }
    };
    match outcome {
        CandidateCleanupOutcome::Removed | CandidateCleanupOutcome::Absent => Ok(()),
        CandidateCleanupOutcome::Failed(source) => {
            let original = CodegenError::Io {
                path: path.to_path_buf(),
                source,
            };
            let _ = try_cleanup_candidate(
                context,
                fs,
                kit_directory,
                transactions,
                name,
                path,
                identity,
            );
            Err(original)
        }
    }
}

#[cfg(not(windows))]
fn try_cleanup_candidate(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    name: &str,
    path: &Path,
    identity: ObjectIdentity,
) -> Result<CandidateCleanupOutcome, CodegenError> {
    require_current_transactions_directory(context, fs, kit_directory, transactions)?;
    inspect_metadata(fs, path)?;
    let metadata = match transactions.directory.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || windows_namespace_delete_pending_error(&error) =>
        {
            return Ok(CandidateCleanupOutcome::Absent);
        }
        Err(source) => {
            return Err(CodegenError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    ensure_safe_regular_metadata(path.to_string_lossy().as_ref(), &metadata)?;
    if metadata_identity(&metadata) != identity {
        return Err(CodegenError::UnsafePath {
            path: path.to_string_lossy().into_owned(),
            reason: "coordination candidate identity changed before cleanup".to_owned(),
        });
    }
    match fs.remove_file(&transactions.directory, Path::new(name), path) {
        Ok(()) => Ok(CandidateCleanupOutcome::Removed),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || windows_namespace_delete_pending_error(&error) =>
        {
            Ok(CandidateCleanupOutcome::Absent)
        }
        Err(source) => Ok(CandidateCleanupOutcome::Failed(source)),
    }
}

fn abandon_candidate(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: TransactionsDirectory,
    mut candidate: Candidate,
) -> Result<(), CodegenError> {
    let cleanup_result =
        cleanup_candidate_source(context, fs, kit_directory, &transactions, &mut candidate);
    let sync_result = sync_directory(fs, &transactions.directory, &transactions.path);
    drop(candidate);
    let directory_cleanup_result = cleanup_transactions_directory_after_drop(
        fs,
        kit_directory,
        context,
        transactions.identity,
        transactions.directory,
    );
    cleanup_result?;
    sync_result?;
    directory_cleanup_result
}

fn abandon_candidate_for_convergence(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: TransactionsDirectory,
    mut candidate: Candidate,
) -> Result<(), CodegenError> {
    let cleanup_result =
        cleanup_candidate_source(context, fs, kit_directory, &transactions, &mut candidate);
    let sync_result = sync_directory(fs, &transactions.directory, &transactions.path);
    drop(candidate);
    let directory_cleanup_result = cleanup_shared_transactions_directory_after_drop_by_identity(
        fs,
        kit_directory,
        context,
        transactions.identity,
        transactions.directory,
    );
    cleanup_result?;
    sync_result?;
    directory_cleanup_result
}

fn cleanup_shared_transactions_directory_after_drop_by_identity(
    fs: &dyn FsOps,
    kit_directory: &Dir,
    context: &PlanningContext,
    identity: ObjectIdentity,
    directory: Dir,
) -> Result<(), CodegenError> {
    drop(directory);
    match try_cleanup_transactions_directory_by_identity(fs, kit_directory, context, identity) {
        Ok(TransactionsDirectoryCleanupOutcome::Removed)
        | Ok(TransactionsDirectoryCleanupOutcome::Absent)
        | Ok(TransactionsDirectoryCleanupOutcome::NotQuiescent(_)) => Ok(()),
        Ok(TransactionsDirectoryCleanupOutcome::Failed(source)) => {
            best_effort_cleanup_transactions_directory_by_identity(
                fs,
                kit_directory,
                context,
                identity,
            );
            Err(CodegenError::Io {
                path: context
                    .project_root()
                    .join("src/components/ui/_kit/.transactions"),
                source,
            })
        }
        Err(original) => {
            best_effort_cleanup_transactions_directory_by_identity(
                fs,
                kit_directory,
                context,
                identity,
            );
            Err(original)
        }
    }
}

fn cleanup_published_candidate(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    candidate: &mut Candidate,
) -> Result<(), CodegenError> {
    let cleanup_result =
        cleanup_candidate_source(context, fs, kit_directory, transactions, candidate);
    let sync_result = sync_directory(fs, &transactions.directory, &transactions.path);
    cleanup_result?;
    sync_result
}

#[cfg(not(windows))]
fn cleanup_candidate_source(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    candidate: &mut Candidate,
) -> Result<(), CodegenError> {
    cleanup_candidate(
        context,
        fs,
        kit_directory,
        transactions,
        &candidate.name,
        &candidate.path,
        candidate.identity,
    )
}

#[cfg(windows)]
fn cleanup_candidate_source(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    candidate: &mut Candidate,
) -> Result<(), CodegenError> {
    let first =
        try_cleanup_candidate_source_by_handle(context, fs, kit_directory, transactions, candidate);
    if let Err(original) = first {
        if candidate.source_guard.is_some() {
            let _ = try_cleanup_candidate_source_by_handle(
                context,
                fs,
                kit_directory,
                transactions,
                candidate,
            );
        }
        return Err(original);
    }
    Ok(())
}

#[cfg(windows)]
fn try_cleanup_candidate_source_by_handle(
    context: &PlanningContext,
    fs: &dyn FsOps,
    kit_directory: &Dir,
    transactions: &TransactionsDirectory,
    candidate: &mut Candidate,
) -> Result<(), CodegenError> {
    require_current_candidate(context, fs, kit_directory, transactions, candidate)?;
    validate_candidate_source_guard(candidate)?;
    let guard =
        candidate
            .source_guard
            .take()
            .ok_or_else(|| CodegenError::InvalidCoordinationState {
                path: candidate.path.to_string_lossy().into_owned(),
                reason: "Windows coordination candidate source guard was already consumed"
                    .to_owned(),
            })?;
    remove_file_by_handle_with_retry(fs, guard, &candidate.path).map_err(|source| {
        CodegenError::Io {
            path: candidate.path.clone(),
            source,
        }
    })?;
    match transactions.directory.symlink_metadata(&candidate.name) {
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || windows_namespace_delete_pending_error(&error) =>
        {
            Ok(())
        }
        Err(source) => Err(CodegenError::Io {
            path: candidate.path.clone(),
            source,
        }),
        Ok(metadata) => {
            ensure_safe_regular_metadata(candidate.path.to_string_lossy().as_ref(), &metadata)?;
            if metadata_identity(&metadata) != candidate.identity {
                return Err(CodegenError::UnsafePath {
                    path: candidate.path.to_string_lossy().into_owned(),
                    reason: "coordination candidate changed after handle-relative cleanup"
                        .to_owned(),
                });
            }
            // The classic Windows disposition fallback keeps the verified name
            // delete-pending until the owner handle closes. The subsequent
            // directory cleanup detects that state and forces the lock-owner
            // close/reopen/reacquire path before planning can begin.
            Ok(())
        }
    }
}

fn validate_claimed_candidate(
    fs: &dyn FsOps,
    file: &mut File,
    path: &Path,
    kind: CandidateKind,
    aliases_held_lock: bool,
) -> Result<(), CodegenError> {
    let mode = coordination_file_mode(file, path)?;
    if aliases_held_lock {
        if kind != CandidateKind::Lock {
            return Err(CodegenError::InvalidCoordinationState {
                path: path.to_string_lossy().into_owned(),
                reason: "non-lock candidate aliases the persistent advisory lock".to_owned(),
            });
        }
        validate_optional_mode(mode, 0o600, path)?;
    } else if !recognized_candidate_mode(kind, mode) {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.to_string_lossy().into_owned(),
            reason: "coordination candidate has an unsupported lifecycle mode".to_owned(),
        });
    }
    fs.before_read_handle(path)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let content = read_bounded_cap_file(file, path)?;
    let valid_content = if aliases_held_lock {
        content == KIT_ADVISORY_LOCK_CONTENT
    } else {
        kind.content().starts_with(&content)
    };
    if !valid_content {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.to_string_lossy().into_owned(),
            reason: "coordination candidate has unsupported lifecycle contents".to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn coordination_file_mode(file: &File, path: &Path) -> Result<Option<u32>, CodegenError> {
    use cap_std::fs::PermissionsExt;

    Ok(Some(
        file.metadata()
            .map_err(|source| CodegenError::Io {
                path: path.to_path_buf(),
                source,
            })?
            .permissions()
            .mode()
            & 0o7777,
    ))
}

#[cfg(not(unix))]
fn coordination_file_mode(_file: &File, _path: &Path) -> Result<Option<u32>, CodegenError> {
    Ok(None)
}

fn recognized_candidate_mode(kind: CandidateKind, actual: Option<u32>) -> bool {
    let Some(actual) = actual else {
        return true;
    };
    let restrictive_creation_mode = actual & !0o600 == 0;
    restrictive_creation_mode || actual == kind.final_mode()
}

fn validate_optional_mode(
    actual: Option<u32>,
    expected: u32,
    path: &Path,
) -> Result<(), CodegenError> {
    #[cfg(unix)]
    {
        match actual {
            Some(actual) => validate_mode(actual, expected, path.to_string_lossy().as_ref()),
            None => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (actual, expected, path);
        Ok(())
    }
}

fn open_existing_coordination_file(
    context: &PlanningContext,
    fs: &dyn FsOps,
) -> Result<Option<File>, CodegenError> {
    let path = context
        .project_root()
        .join(DEFAULT_KIT_COORDINATION_IGNORE_PATH);
    fs.before_open_coordination_file(&path)
        .map_err(|source| CodegenError::Io {
            path: path.clone(),
            source,
        })?;
    match context.open_auxiliary_file(DEFAULT_KIT_COORDINATION_IGNORE_PATH, false) {
        Ok(file) => Ok(Some(file)),
        Err(CodegenError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn validate_coordination_ignore(
    context: &PlanningContext,
    fs: &dyn FsOps,
    file: &mut File,
    path: &Path,
) -> Result<(), CodegenError> {
    validate_cap_file_mode(file, 0o644, DEFAULT_KIT_COORDINATION_IGNORE_PATH)?;
    fs.before_read_handle(path)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let content = read_bounded_cap_file(file, path)?;
    if content != KIT_COORDINATION_IGNORE_CONTENT
        && content != LEGACY_KIT_COORDINATION_IGNORE_CONTENT
    {
        return Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_COORDINATION_IGNORE_PATH.to_owned(),
            reason: "expected the exact installer-owned ignore rules".to_owned(),
        });
    }
    let metadata = file.metadata().map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.nlink() != 1 {
        return Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_COORDINATION_IGNORE_PATH.to_owned(),
            reason: "installer-owned ignore rules must have exactly one hard link".to_owned(),
        });
    }
    context.revalidate_auxiliary_file(DEFAULT_KIT_COORDINATION_IGNORE_PATH, file)
}

pub(super) fn coordination_ignore_requires_migration(
    context: &PlanningContext,
    fs: &dyn FsOps,
) -> Result<bool, CodegenError> {
    let path = context
        .project_root()
        .join(DEFAULT_KIT_COORDINATION_IGNORE_PATH);
    let Some(mut file) = open_existing_coordination_file(context, fs)? else {
        return Ok(false);
    };
    validate_cap_file_mode(&file, 0o644, DEFAULT_KIT_COORDINATION_IGNORE_PATH)?;
    fs.before_read_handle(&path)
        .map_err(|source| CodegenError::Io {
            path: path.clone(),
            source,
        })?;
    let content = read_bounded_cap_file(&mut file, &path)?;
    let metadata = file.metadata().map_err(|source| CodegenError::Io {
        path: path.clone(),
        source,
    })?;
    if metadata.nlink() != 1 {
        return Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_COORDINATION_IGNORE_PATH.to_owned(),
            reason: "installer-owned ignore rules must have exactly one hard link".to_owned(),
        });
    }
    context.revalidate_auxiliary_file(DEFAULT_KIT_COORDINATION_IGNORE_PATH, &file)?;
    match content.as_slice() {
        KIT_COORDINATION_IGNORE_CONTENT => Ok(false),
        LEGACY_KIT_COORDINATION_IGNORE_CONTENT => Ok(true),
        _ => Err(CodegenError::InvalidCoordinationState {
            path: DEFAULT_KIT_COORDINATION_IGNORE_PATH.to_owned(),
            reason: "expected the exact installer-owned ignore rules".to_owned(),
        }),
    }
}

#[cfg(windows)]
fn transient_windows_sharing_error(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(32)
}

#[cfg(not(windows))]
fn transient_windows_sharing_error(_error: &std::io::Error) -> bool {
    false
}

#[cfg(windows)]
fn windows_namespace_delete_pending_error(error: &std::io::Error) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_DELETE_PENDING};

    matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_ACCESS_DENIED as i32 || code == ERROR_DELETE_PENDING as i32
    )
}

#[cfg(not(windows))]
fn windows_namespace_delete_pending_error(_error: &std::io::Error) -> bool {
    false
}

fn validate_lock_marker(content: &[u8]) -> Result<(), CodegenError> {
    if content == KIT_ADVISORY_LOCK_CONTENT {
        return Ok(());
    }
    if content == LEGACY_WRITE_LOCK_CONTENT {
        return Err(CodegenError::LegacyWriteLock {
            path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
        });
    }
    Err(CodegenError::InvalidCoordinationState {
        path: DEFAULT_KIT_WRITE_LOCK_PATH.to_owned(),
        reason: "lock contents do not match a supported persistent format".to_owned(),
    })
}

fn read_bounded_cap_file(file: &mut File, path: &Path) -> Result<Vec<u8>, CodegenError> {
    let length = file
        .metadata()
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    if length > MAX_COORDINATION_FILE_BYTES {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.to_string_lossy().into_owned(),
            reason: "coordination contents exceed the supported size".to_owned(),
        });
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut content = Vec::with_capacity(length as usize);
    file.take(MAX_COORDINATION_FILE_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if content.len() as u64 > MAX_COORDINATION_FILE_BYTES {
        return Err(CodegenError::InvalidCoordinationState {
            path: path.to_string_lossy().into_owned(),
            reason: "coordination contents exceed the supported size".to_owned(),
        });
    }
    Ok(content)
}

fn file_identity(file: &File, path: &Path) -> Result<ObjectIdentity, CodegenError> {
    let metadata = file.metadata().map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    ensure_safe_regular_metadata(path.to_string_lossy().as_ref(), &metadata)?;
    Ok(metadata_identity(&metadata))
}

fn metadata_identity(metadata: &Metadata) -> ObjectIdentity {
    ObjectIdentity::from_u64(MetadataExt::dev(metadata), MetadataExt::ino(metadata))
}

fn ensure_safe_directory_metadata(path: &str, metadata: &Metadata) -> Result<(), CodegenError> {
    ensure_not_indirection(path, metadata)?;
    if !metadata.is_dir() {
        return Err(CodegenError::UnsafePath {
            path: path.to_owned(),
            reason: "controlled bootstrap entry is not a directory".to_owned(),
        });
    }
    Ok(())
}

fn ensure_safe_regular_metadata(path: &str, metadata: &Metadata) -> Result<(), CodegenError> {
    ensure_not_indirection(path, metadata)?;
    if !metadata.is_file() {
        return Err(CodegenError::UnsafePath {
            path: path.to_owned(),
            reason: "controlled bootstrap entry is not a regular file".to_owned(),
        });
    }
    Ok(())
}

fn ensure_not_indirection(path: &str, metadata: &Metadata) -> Result<(), CodegenError> {
    if metadata.file_type().is_symlink() {
        return Err(CodegenError::UnsafePath {
            path: path.to_owned(),
            reason: "symbolic links are rejected".to_owned(),
        });
    }
    #[cfg(windows)]
    if cap_fs_ext::OsMetadataExt::file_attributes(metadata) & 0x0000_0400 != 0 {
        return Err(CodegenError::UnsafePath {
            path: path.to_owned(),
            reason: "Windows reparse points are rejected".to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_directory(parent: &Dir, name: &str, path: &Path) -> Result<(), CodegenError> {
    use cap_std::fs::DirBuilderExt;

    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    parent
        .create_dir_with(name, &builder)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn create_private_directory(parent: &Dir, name: &str, path: &Path) -> Result<(), CodegenError> {
    parent.create_dir(name).map_err(|source| CodegenError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn secure_coordination_directory_mode(
    fs: &dyn FsOps,
    directory: &Dir,
    path: &Path,
    _created: bool,
) -> Result<(), CodegenError> {
    fs.set_directory_mode(directory, path, 0o700)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn validate_cap_file_mode(
    file: &File,
    expected: u32,
    logical_path: &str,
) -> Result<(), CodegenError> {
    use cap_std::fs::PermissionsExt;

    let actual = file
        .metadata()
        .map_err(|source| CodegenError::Io {
            path: PathBuf::from(logical_path),
            source,
        })?
        .permissions()
        .mode()
        & 0o7777;
    validate_mode(actual, expected, logical_path)
}

#[cfg(not(unix))]
fn validate_cap_file_mode(
    _file: &File,
    _expected: u32,
    _logical_path: &str,
) -> Result<(), CodegenError> {
    Ok(())
}

#[cfg(unix)]
fn validate_directory_mode(
    metadata: &Metadata,
    expected: u32,
    logical_path: &str,
) -> Result<(), CodegenError> {
    use cap_std::fs::PermissionsExt;

    validate_mode(
        metadata.permissions().mode() & 0o7777,
        expected,
        logical_path,
    )
}

#[cfg(not(unix))]
fn validate_directory_mode(
    _metadata: &Metadata,
    _expected: u32,
    _logical_path: &str,
) -> Result<(), CodegenError> {
    Ok(())
}

#[cfg(unix)]
fn validate_mode(actual: u32, expected: u32, logical_path: &str) -> Result<(), CodegenError> {
    if actual == expected {
        return Ok(());
    }
    Err(CodegenError::InvalidCoordinationState {
        path: logical_path.to_owned(),
        reason: format!("expected POSIX mode {expected:04o}, found {actual:04o}"),
    })
}

fn sync_directory(fs: &dyn FsOps, directory: &Dir, path: &Path) -> Result<(), CodegenError> {
    fs.sync_directory(directory, path)
        .map_err(|source| CodegenError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn sync_created_directory_chain(
    context: &PlanningContext,
    fs: &dyn FsOps,
    created_directories: &[String],
) -> Result<(), CodegenError> {
    let mut paths = created_directories.to_vec();
    for path in created_directories {
        paths.push(
            path.rsplit_once('/')
                .map_or_else(String::new, |(parent, _)| parent.to_owned()),
        );
    }
    paths.sort_by(|left, right| {
        Path::new(right)
            .components()
            .count()
            .cmp(&Path::new(left).components().count())
            .then_with(|| left.cmp(right))
    });
    paths.dedup();
    for path in paths {
        let directory = context.open_directory(&path)?;
        sync_directory(fs, &directory, &context.project_root().join(&path))?;
    }
    Ok(())
}

#[cfg(test)]
mod held_lock_validation_tests {
    use super::*;
    use std::fs;
    use std::io::Write as _;

    fn acquire(root: &Path) -> (PlanningContext, WriteLock) {
        let context = PlanningContext::open(root).expect("planning context");
        let lock = WriteLock::acquire_with_context(&context).expect("write lock");
        (context, lock)
    }

    fn overwrite_held_lock(lock: &WriteLock, content: &[u8]) {
        let held = lock.file.as_ref().expect("held lock handle");
        let mut writer = held.try_clone().expect("clone held lock handle");
        writer
            .seek(SeekFrom::Start(0))
            .expect("seek held lock handle");
        writer.set_len(0).expect("truncate held lock handle");
        writer.write_all(content).expect("write held lock handle");
        writer.sync_all().expect("sync held lock handle");
    }

    #[test]
    fn held_lock_validation_accepts_the_acquired_project_and_inode() {
        let directory = tempfile::tempdir().expect("temporary project");
        let (context, lock) = acquire(directory.path());

        let held = File::from_std(
            lock.file
                .as_ref()
                .expect("held lock handle")
                .try_clone()
                .expect("clone held lock handle"),
        );
        assert_eq!(
            held.metadata().expect("held lock metadata").nlink(),
            1,
            "acquisition must remove its publisher alias before returning",
        );
        lock.validate_context(&context)
            .expect("unchanged held lock must validate");
    }

    #[test]
    fn lock_acquisition_leaves_the_transaction_namespace_absent() {
        let directory = tempfile::tempdir().expect("temporary project");
        let (context, lock) = acquire(directory.path());
        let namespace_path = directory
            .path()
            .join("src/components/ui/_kit/.transactions");

        assert!(
            !namespace_path.exists(),
            "lock acquisition must not create the transaction namespace",
        );

        let runtime = TransactionRuntime::system();
        let transaction_id =
            TransactionId::parse("00112233445566778899aabbccddeeff").expect("transaction id");
        let namespace = lock
            .open_or_create_transaction_namespace(&context, &runtime, &transaction_id)
            .expect("create the transaction namespace on first transactional write");
        assert!(namespace_path.is_dir());
        drop(namespace);
        super::super::namespace_bootstrap::recover_namespace_bootstrap(&context, &lock, &runtime)
            .expect("cancel namespace without ordinary workspace ownership");
        assert!(!namespace_path.exists());
    }

    #[test]
    fn unattested_canonical_namespace_evidence_is_rejected() {
        let directory = tempfile::tempdir().expect("temporary project");
        let (context, lock) = acquire(directory.path());
        let namespace_path = directory
            .path()
            .join("src/components/ui/_kit/.transactions");
        fs::create_dir(&namespace_path).expect("create hostile canonical namespace");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(&namespace_path, fs::Permissions::from_mode(0o700))
                .expect("set exact private namespace mode");
        }
        fs::write(namespace_path.join("evidence"), b"preserve").expect("write evidence");
        let runtime = TransactionRuntime::system();
        let transaction_id =
            TransactionId::parse("00112233445566778899aabbccddeeff").expect("transaction id");
        let error = lock
            .open_or_create_transaction_namespace(&context, &runtime, &transaction_id)
            .expect_err("unattested canonical namespace must fail closed");
        assert!(
            error.to_string().contains("without lifecycle"),
            "unexpected unattested namespace diagnostic: {error:?}"
        );
        assert_eq!(
            fs::read(namespace_path.join("evidence")).expect("preserved evidence"),
            b"preserve",
        );
    }

    #[test]
    fn held_lock_validation_rejects_a_different_project() {
        let first = tempfile::tempdir().expect("first temporary project");
        let second = tempfile::tempdir().expect("second temporary project");
        let (_, lock) = acquire(first.path());
        let other = PlanningContext::open(second.path()).expect("other planning context");

        let error = lock
            .validate_context(&other)
            .expect_err("a lock from another project must fail closed");
        assert!(matches!(error, CodegenError::ProjectRootChanged { .. }));
        assert!(lock.file.is_some());
    }

    #[test]
    fn held_lock_validation_rejects_changed_contents_without_consuming_the_handle() {
        let directory = tempfile::tempdir().expect("temporary project");
        let (context, lock) = acquire(directory.path());

        overwrite_held_lock(&lock, b"tampered\n");
        let error = lock
            .validate_context(&context)
            .expect_err("changed lock contents must fail closed");
        assert!(matches!(
            error,
            CodegenError::InvalidCoordinationState { path, .. }
                if path == DEFAULT_KIT_WRITE_LOCK_PATH
        ));
        assert!(lock.file.is_some());

        overwrite_held_lock(&lock, KIT_ADVISORY_LOCK_CONTENT);
        lock.validate_context(&context)
            .expect("restored contents prove the held handle was preserved");
    }

    #[test]
    fn held_lock_validation_rejects_an_extra_hard_link_without_consuming_the_handle() {
        let directory = tempfile::tempdir().expect("temporary project");
        let (context, lock) = acquire(directory.path());
        let path = directory.path().join(DEFAULT_KIT_WRITE_LOCK_PATH);
        let alias = directory.path().join("write-lock-hard-link-alias");
        fs::hard_link(&path, &alias).expect("create hard-link alias");

        let validation = lock.validate_context(&context);
        fs::remove_file(&alias).expect("remove hard-link alias");

        let error = validation.expect_err("a multiply linked lock must fail closed");
        assert!(matches!(
            error,
            CodegenError::InvalidCoordinationState { path, reason }
                if path == DEFAULT_KIT_WRITE_LOCK_PATH
                    && reason.contains("exactly one hard link")
        ));
        assert!(lock.file.is_some());
        lock.validate_context(&context)
            .expect("removing the alias proves the held handle was preserved");
    }

    #[cfg(unix)]
    #[test]
    fn held_lock_validation_rejects_wrong_mode_without_consuming_the_handle() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("temporary project");
        let (context, lock) = acquire(directory.path());
        let held = lock.file.as_ref().expect("held lock handle");
        held.set_permissions(fs::Permissions::from_mode(0o644))
            .expect("set unsafe lock mode");

        let error = lock
            .validate_context(&context)
            .expect_err("wrong lock mode must fail closed");
        assert!(matches!(
            error,
            CodegenError::InvalidCoordinationState { path, .. }
                if path == DEFAULT_KIT_WRITE_LOCK_PATH
        ));
        assert!(lock.file.is_some());

        held.set_permissions(fs::Permissions::from_mode(0o600))
            .expect("restore safe lock mode");
        lock.validate_context(&context)
            .expect("restored mode proves the held handle was preserved");
    }

    #[cfg(unix)]
    #[test]
    fn held_lock_validation_rejects_missing_symlinked_and_replaced_paths() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = tempfile::tempdir().expect("temporary project");
        let (context, lock) = acquire(directory.path());
        let path = directory.path().join(DEFAULT_KIT_WRITE_LOCK_PATH);
        fs::remove_file(&path).expect("detach held lock inode");

        let missing = lock
            .validate_context(&context)
            .expect_err("a missing lock path must fail closed");
        assert!(
            matches!(
                missing,
                CodegenError::InvalidCoordinationState { ref reason, .. }
                    if reason.contains("found 0")
            ),
            "unexpected missing-lock diagnostic: {missing:?}"
        );
        assert!(lock.file.is_some());

        let referent = directory.path().join("replacement-lock");
        fs::write(&referent, KIT_ADVISORY_LOCK_CONTENT).expect("write symlink referent");
        fs::set_permissions(&referent, fs::Permissions::from_mode(0o600))
            .expect("secure symlink referent");
        symlink(&referent, &path).expect("symlink lock path");
        let symlinked = lock
            .validate_context(&context)
            .expect_err("a symlinked lock path must fail closed");
        assert!(matches!(
            symlinked,
            CodegenError::InvalidCoordinationState { ref reason, .. }
                if reason.contains("found 0")
        ));
        assert!(lock.file.is_some());

        fs::remove_file(&path).expect("remove lock symlink");
        fs::write(&path, KIT_ADVISORY_LOCK_CONTENT).expect("write replacement lock");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("secure replacement lock");
        let replaced = lock
            .validate_context(&context)
            .expect_err("a replaced lock inode must fail closed");
        assert!(matches!(
            replaced,
            CodegenError::InvalidCoordinationState { ref reason, .. }
                if reason.contains("found 0")
        ));
        assert!(lock.file.is_some());
    }
}
