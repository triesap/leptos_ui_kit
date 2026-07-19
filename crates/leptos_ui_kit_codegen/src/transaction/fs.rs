use std::{
    ffi::OsString,
    fmt,
    io::{self, Read, Seek, SeekFrom, Write},
    panic::{RefUnwindSafe, UnwindSafe},
    path::{Component, Path},
};

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::PathBuf;

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, File, Metadata, OpenOptions};
use cap_std::io_lifetimes::AsFilelike;
use sha2::{Digest, Sha256};

use crate::PreservedFileMode;
use crate::path_safety::ObjectIdentity;

/// A live capability for a file whose capability-relative `create_new` open
/// completed successfully.
///
/// Exact metadata is optional because metadata acquisition is a distinct
/// post-create operation which can fail after the name has already been
/// mutated.  Keeping the handle in that world lets the transaction bind a
/// fresh namespace observation to the object it actually created instead of
/// guessing from the pathname.
#[derive(Debug)]
pub(crate) struct CreatedFile {
    pub file: File,
    exact_metadata: Option<ExactFileMetadataObservation>,
}

impl CreatedFile {
    fn unverified(file: File) -> Self {
        Self {
            file,
            exact_metadata: None,
        }
    }

    fn verified(file: File, exact_metadata: ExactFileMetadataObservation) -> Self {
        Self {
            file,
            exact_metadata: Some(exact_metadata),
        }
    }

    #[cfg(test)]
    pub(crate) const fn exact_metadata(&self) -> Option<ExactFileMetadataObservation> {
        self.exact_metadata
    }

    pub(crate) fn identity(&self) -> ObjectIdentity {
        self.exact_metadata
            .expect("verified created-file capability must retain exact metadata")
            .identity
    }
}

/// Closed outcome of an exclusive file creation.
///
/// `NotCreated` is the only variant which proves that this call did not create
/// a directory entry.  Both created variants retain the live handle.  The
/// unverified variant may still carry exact metadata when a later
/// post-success hook failed after metadata acquisition.
#[derive(Debug)]
pub(crate) enum ExclusiveCreateOutcome {
    NotCreated {
        source: io::Error,
    },
    CreatedUnverified {
        created: CreatedFile,
        source: io::Error,
    },
    CreatedVerified {
        created: CreatedFile,
    },
}

impl ExclusiveCreateOutcome {
    pub(crate) fn bind_empty<F>(
        self,
        fs: &F,
        parent: &Dir,
        name: &Path,
        path: &Path,
    ) -> Result<CreatedFile, ExclusiveCreateFailure>
    where
        F: FsOps + ?Sized,
    {
        match self {
            Self::CreatedVerified { created } => Ok(created),
            Self::NotCreated { source } => Err(ExclusiveCreateFailure::NotCreated(source)),
            Self::CreatedUnverified {
                mut created,
                source,
            } => match fs.observe_created_file_exact(parent, name, path, &mut created, 0) {
                Ok(_) => Ok(created),
                Err(rebind_source) => Err(ExclusiveCreateFailure::CreatedUnverified {
                    created,
                    source: io::Error::other(format!(
                        "exclusive create reported a post-create error ({source}) and its live \
                         capability could not be rebound to the empty owner name ({rebind_source})"
                    )),
                }),
            },
        }
    }
}

#[derive(Debug)]
pub(crate) enum ExclusiveFileCopyOutcome {
    NotCreated {
        source: io::Error,
    },
    CreatedUnverified {
        created: CreatedFile,
        source: io::Error,
    },
    CreatedVerified {
        copy: ExclusiveFileCopy,
    },
}

impl ExclusiveFileCopyOutcome {
    #[cfg(test)]
    fn into_verified(self) -> Result<ExclusiveFileCopy, ExclusiveCreateFailure> {
        match self {
            Self::CreatedVerified { copy } => Ok(copy),
            Self::NotCreated { source } => Err(ExclusiveCreateFailure::NotCreated(source)),
            Self::CreatedUnverified { created, source } => {
                Err(ExclusiveCreateFailure::CreatedUnverified { created, source })
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum ExclusiveCreateFailure {
    NotCreated(io::Error),
    CreatedUnverified {
        created: CreatedFile,
        source: io::Error,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct HardLinkEndpoint<'a> {
    pub parent: &'a Dir,
    pub name: &'a Path,
    pub path: &'a Path,
}

impl<'a> HardLinkEndpoint<'a> {
    pub(crate) fn new(parent: &'a Dir, name: &'a Path, path: &'a Path) -> Self {
        Self { parent, name, path }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DirectoryEndpoint<'a> {
    pub parent: &'a Dir,
    pub name: &'a Path,
    pub directory: &'a Dir,
    pub path: &'a Path,
}

impl<'a> DirectoryEndpoint<'a> {
    pub(crate) fn new(parent: &'a Dir, name: &'a Path, directory: &'a Dir, path: &'a Path) -> Self {
        Self {
            parent,
            name,
            directory,
            path,
        }
    }
}

/// Whether this dependency set can represent a filesystem object's complete,
/// stable identity on the current platform.
///
/// `cap-fs-ext` exposes a portable `(device, inode)` pair. That pair is exact
/// on the supported Unix targets, but its Windows inode component truncates a
/// ReFS file identifier to 64 bits. Transaction decisions that require exact
/// identity therefore fail closed on Windows until a full-width, safe handle
/// identity API is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactIdentitySupport {
    Complete,
    Unsupported,
}

pub(crate) const fn exact_identity_support() -> ExactIdentitySupport {
    #[cfg(windows)]
    {
        ExactIdentitySupport::Unsupported
    }
    #[cfg(not(windows))]
    {
        ExactIdentitySupport::Complete
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactFileObservation {
    pub identity: ObjectIdentity,
    pub byte_len: u64,
    pub content_hash: String,
    pub mode: PreservedFileMode,
    pub link_count: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExactFileMetadataObservation {
    pub identity: ObjectIdentity,
    pub byte_len: u64,
    pub mode: PreservedFileMode,
    pub link_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactFileRead {
    pub bytes: Vec<u8>,
    pub observation: ExactFileObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactFileBytesRead {
    pub bytes: Vec<u8>,
    pub observation: ExactFileMetadataObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExactDirectoryObservation {
    pub identity: ObjectIdentity,
    pub mode: PreservedFileMode,
    pub link_count: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct ExactDirectoryHandle {
    pub directory: Dir,
    pub observation: ExactDirectoryObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactDirectoryEntryKind {
    RegularFile,
    Directory,
    Symlink,
    ReparsePoint,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactDirectoryEntry {
    pub name: OsString,
    pub kind: ExactDirectoryEntryKind,
    pub identity: ObjectIdentity,
    pub byte_len: u64,
    pub mode: PreservedFileMode,
    pub link_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactDirectoryInventory {
    pub directory: ExactDirectoryObservation,
    pub entries: Vec<ExactDirectoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExactRelocationSource {
    File(ExactFileObservation),
    EmptyDirectory(ExactDirectoryObservation),
}

#[derive(Debug)]
pub(crate) struct ExclusiveFileCopy {
    pub file: File,
    pub source: ExactFileObservation,
    pub copy: ExactFileObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParentSyncKind {
    Target,
    Journal,
}

/// Typed result for a capability-relative atomic no-clobber relocation.
/// Cross-filesystem placement is a protocol incompatibility, not an ordinary
/// transient I/O failure, and callers must fail closed without copying.
#[derive(Debug)]
pub(crate) enum NoReplaceRelocationError {
    CrossDevice,
    Unsupported,
    Io(io::Error),
}

impl NoReplaceRelocationError {
    pub(crate) fn into_io(self) -> io::Error {
        match self {
            Self::CrossDevice => io::Error::new(
                io::ErrorKind::CrossesDevices,
                "transaction owner and destination are on different filesystems",
            ),
            Self::Unsupported => io::Error::new(
                io::ErrorKind::Unsupported,
                "atomic capability-relative no-replace relocation is unavailable on this platform",
            ),
            Self::Io(source) => source,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactRemovalPostName {
    Absent,
    Substituted,
    Unknown,
}

/// Phase-aware failure from an exact unlink/rmdir boundary.
///
/// Callers must never treat `Mutated` as an ordinary pre-mutation I/O error:
/// the expected object is already unlinked and the parent must be freshly
/// rebound, synced, and recaptured before journal progress is published.
#[derive(Debug)]
pub(crate) enum ExactRemovalError {
    NotMutated(io::Error),
    Mutated {
        post_name: ExactRemovalPostName,
        source: io::Error,
    },
}

impl ExactRemovalError {
    fn not_mutated(source: io::Error) -> Self {
        Self::NotMutated(source)
    }

    fn mutated(post_name: ExactRemovalPostName, source: io::Error) -> Self {
        Self::Mutated { post_name, source }
    }

    pub(crate) const fn post_name(&self) -> Option<ExactRemovalPostName> {
        match self {
            Self::NotMutated(_) => None,
            Self::Mutated { post_name, .. } => Some(*post_name),
        }
    }

    pub(crate) const fn mutation_may_have_completed(&self) -> bool {
        matches!(self, Self::Mutated { .. })
    }

    pub(crate) fn source_error(&self) -> &io::Error {
        match self {
            Self::NotMutated(source) | Self::Mutated { source, .. } => source,
        }
    }
}

impl fmt::Display for ExactRemovalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMutated(source) => write!(formatter, "exact removal did not mutate: {source}"),
            Self::Mutated { post_name, source } => write!(
                formatter,
                "exact removal mutated with post-name state {post_name:?}: {source}"
            ),
        }
    }
}

impl std::error::Error for ExactRemovalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source_error())
    }
}

#[cfg(windows)]
pub(crate) struct HandleDeleteError {
    pub file: File,
    pub source: io::Error,
}

pub(crate) trait FsOps: fmt::Debug + Send + Sync + UnwindSafe + RefUnwindSafe {
    #[cfg(test)]
    fn observe_transition(&self, key: super::runtime::TransitionKey);
    fn before_create_directory(&self, path: &Path) -> io::Result<()>;
    fn after_create_directory(&self, path: &Path) -> io::Result<()>;
    fn before_open_coordination_file(&self, path: &Path) -> io::Result<()>;
    fn before_inspect_metadata(&self, path: &Path) -> io::Result<()>;
    fn before_read_handle(&self, path: &Path) -> io::Result<()>;
    #[cfg(test)]
    fn observe_regular_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
    ) -> io::Result<ExactFileObservation>;
    fn observe_regular_file_bounded(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileObservation>;
    fn observe_regular_file_metadata(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileMetadataObservation>;
    fn observe_created_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        created: &mut CreatedFile,
        max_bytes: u64,
    ) -> io::Result<ExactFileObservation>;
    fn read_regular_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileRead>;
    fn read_regular_file_bytes_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileBytesRead>;
    fn observe_directory(
        &self,
        endpoint: DirectoryEndpoint<'_>,
    ) -> io::Result<ExactDirectoryObservation>;
    fn open_directory_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<ExactDirectoryHandle>;
    fn create_directory_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<ExactDirectoryHandle>;
    #[cfg(test)]
    fn inventory_directory_exact(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
    ) -> io::Result<ExactDirectoryInventory>;
    fn inventory_directory_exact_bounded(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
        max_entries: usize,
    ) -> io::Result<ExactDirectoryInventory>;
    fn create_new_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> ExclusiveCreateOutcome;
    fn create_exclusive_copy(
        &self,
        source: HardLinkEndpoint<'_>,
        expected_source: &ExactFileObservation,
        destination: HardLinkEndpoint<'_>,
    ) -> ExclusiveFileCopyOutcome;
    #[cfg(windows)]
    fn open_file_for_cleanup(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
    ) -> io::Result<CreatedFile>;
    #[cfg(windows)]
    fn open_candidate_owner(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
    ) -> io::Result<CreatedFile>;
    fn set_file_mode(&self, file: &File, path: &Path, mode: u32) -> io::Result<()>;
    fn set_preserved_file_mode(
        &self,
        file: &File,
        path: &Path,
        mode: PreservedFileMode,
    ) -> io::Result<()>;
    fn set_path_mode(&self, parent: &Dir, name: &Path, path: &Path, mode: u32) -> io::Result<()>;
    fn set_directory_mode(&self, directory: &Dir, path: &Path, mode: u32) -> io::Result<()>;
    fn write_handle(&self, file: &mut File, path: &Path, content: &[u8]) -> io::Result<()>;
    fn sync_handle(&self, file: &File, path: &Path) -> io::Result<()>;
    fn flush_file(&self, file: &File, path: &Path) -> io::Result<()>;
    fn sync_directory(&self, directory: &Dir, path: &Path) -> io::Result<()>;
    fn sync_parent(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
        kind: ParentSyncKind,
    ) -> io::Result<()>;
    fn try_lock(&self, file: &File, path: &Path) -> Result<(), std::fs::TryLockError>;
    fn hard_link(
        &self,
        pinned_directories: &[Dir],
        from: HardLinkEndpoint<'_>,
        to: HardLinkEndpoint<'_>,
    ) -> io::Result<()>;
    fn publish_absent(
        &self,
        staged: HardLinkEndpoint<'_>,
        expected_stage: &ExactFileObservation,
        target: HardLinkEndpoint<'_>,
    ) -> io::Result<()>;
    fn rename_directory_noreplace(
        &self,
        candidate_parent: &Dir,
        candidate_name: &Path,
        candidate_path: &Path,
        target_parent: &Dir,
        target_name: &Path,
        target_path: &Path,
    ) -> io::Result<()>;
    fn relocate_noreplace(
        &self,
        owner_parent: &Dir,
        owner_name: &Path,
        owner_path: &Path,
        destination_parent: &Dir,
        destination_name: &Path,
        destination_path: &Path,
        expected_source: &ExactRelocationSource,
    ) -> Result<(), NoReplaceRelocationError>;
    fn probe_noreplace_relocation(
        &self,
        parent: &Dir,
        path: &Path,
    ) -> Result<(), NoReplaceRelocationError>;
    fn remove_file(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()>;
    fn remove_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        expected: &ExactFileObservation,
    ) -> Result<(), ExactRemovalError>;
    fn remove_file_metadata_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        expected: &ExactFileMetadataObservation,
    ) -> Result<(), ExactRemovalError>;
    #[cfg(windows)]
    fn remove_file_by_handle(&self, file: File, path: &Path) -> Result<(), HandleDeleteError>;
    fn remove_dir(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()>;
    fn remove_empty_directory_exact(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
    ) -> Result<(), ExactRemovalError>;
    fn before_mutation_rebind(&self, path: &Path) -> io::Result<()>;
    fn before_final_revalidation(&self, path: &Path) -> io::Result<()>;
    fn after_final_revalidation(&self, path: &Path) -> io::Result<()>;
    fn rename(
        &self,
        from_parent: &Dir,
        from_name: &Path,
        from: &Path,
        to_parent: &Dir,
        to_name: &Path,
        to: &Path,
    ) -> io::Result<()>;
    fn replace_existing(
        &self,
        staged: HardLinkEndpoint<'_>,
        expected_stage: &ExactFileObservation,
        target: HardLinkEndpoint<'_>,
        expected_target: &ExactFileObservation,
    ) -> io::Result<()>;
    fn rename_journal(
        &self,
        from_parent: &Dir,
        from_name: &Path,
        from: &Path,
        to_parent: &Dir,
        to_name: &Path,
        to: &Path,
    ) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub(super) struct SystemFs;

impl FsOps for SystemFs {
    #[cfg(test)]
    fn observe_transition(&self, _key: super::runtime::TransitionKey) {}

    fn before_create_directory(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn after_create_directory(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn before_open_coordination_file(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn before_inspect_metadata(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn before_read_handle(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    #[cfg(test)]
    fn observe_regular_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
    ) -> io::Result<ExactFileObservation> {
        observe_regular_file_exact(parent, name, path)
    }

    fn observe_regular_file_bounded(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileObservation> {
        observe_regular_file_bounded_exact(parent, name, path, max_bytes)
    }

    fn observe_regular_file_metadata(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileMetadataObservation> {
        observe_regular_file_metadata_exact(parent, name, path, max_bytes)
    }

    fn observe_created_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        created: &mut CreatedFile,
        max_bytes: u64,
    ) -> io::Result<ExactFileObservation> {
        observe_created_file_exact_impl(parent, name, path, created, max_bytes)
    }

    fn read_regular_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileRead> {
        read_regular_file_exact_impl(parent, name, path, max_bytes)
    }

    fn read_regular_file_bytes_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileBytesRead> {
        read_regular_file_bytes_exact_impl(parent, name, path, max_bytes)
    }

    fn observe_directory(
        &self,
        endpoint: DirectoryEndpoint<'_>,
    ) -> io::Result<ExactDirectoryObservation> {
        observe_directory_exact(endpoint)
    }

    fn open_directory_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<ExactDirectoryHandle> {
        open_directory_exact_impl(parent, name, path, mode)
    }

    fn create_directory_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<ExactDirectoryHandle> {
        create_directory_exact_impl(parent, name, path, mode)
    }

    #[cfg(test)]
    fn inventory_directory_exact(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
    ) -> io::Result<ExactDirectoryInventory> {
        inventory_directory_exact_impl(endpoint, expected)
    }

    fn inventory_directory_exact_bounded(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
        max_entries: usize,
    ) -> io::Result<ExactDirectoryInventory> {
        inventory_directory_exact_bounded_impl(endpoint, expected, max_entries)
    }

    fn create_new_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> ExclusiveCreateOutcome {
        create_new_file_impl(parent, name, path, mode, || Ok(()))
    }

    fn create_exclusive_copy(
        &self,
        source: HardLinkEndpoint<'_>,
        expected_source: &ExactFileObservation,
        destination: HardLinkEndpoint<'_>,
    ) -> ExclusiveFileCopyOutcome {
        create_exclusive_file_copy(self, source, expected_source, destination)
    }

    #[cfg(windows)]
    fn open_file_for_cleanup(
        &self,
        parent: &Dir,
        name: &Path,
        _path: &Path,
    ) -> io::Result<CreatedFile> {
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        options.follow(FollowSymlinks::No);
        options.nonblock(true);
        use cap_std::fs::OpenOptionsExt;
        use windows_sys::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE},
            Storage::FileSystem::{DELETE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE},
        };

        options.access_mode(GENERIC_READ | GENERIC_WRITE | DELETE);
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
        let file = parent.open_with(name, &options)?;
        let metadata = opened_regular_file_metadata(&file)?;
        Ok(CreatedFile::verified(file, metadata))
    }

    #[cfg(windows)]
    fn open_candidate_owner(
        &self,
        parent: &Dir,
        name: &Path,
        _path: &Path,
    ) -> io::Result<CreatedFile> {
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        options.follow(FollowSymlinks::No);
        options.nonblock(true);
        use cap_std::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
        let file = parent.open_with(name, &options)?;
        let metadata = opened_regular_file_metadata(&file)?;
        Ok(CreatedFile::verified(file, metadata))
    }

    fn set_file_mode(&self, file: &File, _path: &Path, mode: u32) -> io::Result<()> {
        #[cfg(unix)]
        {
            use cap_std::fs::{Permissions, PermissionsExt};

            file.set_permissions(Permissions::from_mode(mode))
        }
        #[cfg(not(unix))]
        {
            let _ = (file, mode);
            Ok(())
        }
    }

    fn set_preserved_file_mode(
        &self,
        file: &File,
        _path: &Path,
        mode: PreservedFileMode,
    ) -> io::Result<()> {
        set_exact_file_mode(file, mode)
    }

    fn set_path_mode(&self, parent: &Dir, name: &Path, _path: &Path, mode: u32) -> io::Result<()> {
        #[cfg(unix)]
        {
            use cap_std::fs::{Permissions, PermissionsExt};

            parent.set_symlink_permissions(name, Permissions::from_mode(mode))
        }
        #[cfg(not(unix))]
        {
            let _ = (parent, name, mode);
            Ok(())
        }
    }

    fn set_directory_mode(&self, directory: &Dir, _path: &Path, mode: u32) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            Dir::reopen_dir(directory)?
                .into_std_file()
                .set_permissions(std::fs::Permissions::from_mode(mode))
        }
        #[cfg(not(unix))]
        {
            let _ = (directory, mode);
            Ok(())
        }
    }

    fn write_handle(&self, file: &mut File, _path: &Path, content: &[u8]) -> io::Result<()> {
        file.write_all(content)
    }

    fn sync_handle(&self, file: &File, _path: &Path) -> io::Result<()> {
        file.sync_all()
    }

    fn flush_file(&self, file: &File, _path: &Path) -> io::Result<()> {
        file.sync_all()
    }

    fn sync_directory(&self, directory: &Dir, _path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            Dir::reopen_dir(directory)?.into_std_file().sync_all()
        }
        #[cfg(not(unix))]
        {
            let _ = directory;
            Err(unsupported_parent_sync())
        }
    }

    fn sync_parent(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
        _kind: ParentSyncKind,
    ) -> io::Result<()> {
        #[cfg(unix)]
        {
            require_exact_directory_state(endpoint, expected)?;
            Dir::reopen_dir(endpoint.directory)?
                .into_std_file()
                .sync_all()?;
            require_exact_directory_state(endpoint, expected)
        }
        #[cfg(not(unix))]
        {
            let _ = (endpoint, expected);
            Err(unsupported_parent_sync())
        }
    }

    fn try_lock(&self, file: &File, _path: &Path) -> Result<(), std::fs::TryLockError> {
        file.as_filelike_view::<std::fs::File>().try_lock()
    }

    fn hard_link(
        &self,
        pinned_directories: &[Dir],
        from: HardLinkEndpoint<'_>,
        to: HardLinkEndpoint<'_>,
    ) -> io::Result<()> {
        let _ = (pinned_directories, from.path, to.path);
        from.parent.hard_link(from.name, to.parent, to.name)
    }

    fn publish_absent(
        &self,
        staged: HardLinkEndpoint<'_>,
        expected_stage: &ExactFileObservation,
        target: HardLinkEndpoint<'_>,
    ) -> io::Result<()> {
        ensure_same_parent(staged.parent, target.parent, staged.path, target.path)?;
        require_exact_file_state(staged, expected_stage)?;
        staged
            .parent
            .hard_link(staged.name, target.parent, target.name)
    }

    fn rename_directory_noreplace(
        &self,
        candidate_parent: &Dir,
        candidate_name: &Path,
        candidate_path: &Path,
        target_parent: &Dir,
        target_name: &Path,
        target_path: &Path,
    ) -> io::Result<()> {
        rename_directory_noreplace_impl(
            candidate_parent,
            candidate_name,
            candidate_path,
            target_parent,
            target_name,
            target_path,
        )
    }

    fn relocate_noreplace(
        &self,
        owner_parent: &Dir,
        owner_name: &Path,
        owner_path: &Path,
        destination_parent: &Dir,
        destination_name: &Path,
        destination_path: &Path,
        expected_source: &ExactRelocationSource,
    ) -> Result<(), NoReplaceRelocationError> {
        relocate_noreplace_impl(
            owner_parent,
            owner_name,
            owner_path,
            destination_parent,
            destination_name,
            destination_path,
            expected_source,
        )
    }

    fn probe_noreplace_relocation(
        &self,
        parent: &Dir,
        path: &Path,
    ) -> Result<(), NoReplaceRelocationError> {
        probe_noreplace_relocation_impl(parent, path)
    }

    fn remove_file(&self, parent: &Dir, name: &Path, _path: &Path) -> io::Result<()> {
        parent.remove_file(name)
    }

    fn remove_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        expected: &ExactFileObservation,
    ) -> Result<(), ExactRemovalError> {
        remove_exact_file(parent, name, path, expected, || Ok(()))
    }

    fn remove_file_metadata_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        expected: &ExactFileMetadataObservation,
    ) -> Result<(), ExactRemovalError> {
        remove_exact_file_metadata(parent, name, path, expected, || Ok(()))
    }

    #[cfg(windows)]
    fn remove_file_by_handle(&self, file: File, _path: &Path) -> Result<(), HandleDeleteError> {
        use fs_at::os::windows::FileExt;

        file.into_std()
            .delete_by_handle()
            .map_err(|(file, source)| HandleDeleteError {
                file: File::from_std(file),
                source,
            })
    }

    fn remove_dir(&self, parent: &Dir, name: &Path, _path: &Path) -> io::Result<()> {
        parent.remove_dir(name)
    }

    fn remove_empty_directory_exact(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
    ) -> Result<(), ExactRemovalError> {
        remove_empty_directory_exact_impl(endpoint, expected, || Ok(()))
    }

    fn before_mutation_rebind(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn before_final_revalidation(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn after_final_revalidation(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn rename(
        &self,
        from_parent: &Dir,
        from_name: &Path,
        _from: &Path,
        to_parent: &Dir,
        to_name: &Path,
        _to: &Path,
    ) -> io::Result<()> {
        from_parent.rename(from_name, to_parent, to_name)
    }

    fn replace_existing(
        &self,
        staged: HardLinkEndpoint<'_>,
        expected_stage: &ExactFileObservation,
        target: HardLinkEndpoint<'_>,
        expected_target: &ExactFileObservation,
    ) -> io::Result<()> {
        ensure_same_parent(staged.parent, target.parent, staged.path, target.path)?;
        require_exact_file_state(staged, expected_stage)?;
        require_exact_file_state(target, expected_target)?;
        staged
            .parent
            .rename(staged.name, target.parent, target.name)
    }

    fn rename_journal(
        &self,
        from_parent: &Dir,
        from_name: &Path,
        from: &Path,
        to_parent: &Dir,
        to_name: &Path,
        to: &Path,
    ) -> io::Result<()> {
        self.rename(from_parent, from_name, from, to_parent, to_name, to)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegularMetadataState {
    identity: ObjectIdentity,
    byte_len: u64,
    mode: PreservedFileMode,
    link_count: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryMetadataState {
    identity: ObjectIdentity,
    mode: PreservedFileMode,
    link_count: Option<u64>,
}

fn require_exact_identity_support() -> io::Result<()> {
    match exact_identity_support() {
        ExactIdentitySupport::Complete => Ok(()),
        ExactIdentitySupport::Unsupported => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exact transaction identity is unavailable on Windows because the current safe dependency API truncates ReFS file identifiers to 64 bits",
        )),
    }
}

#[cfg(not(unix))]
fn unsupported_parent_sync() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "durable parent-directory synchronization is not available through the current safe Windows filesystem API",
    )
}

fn preserved_mode(metadata: &Metadata) -> PreservedFileMode {
    PreservedFileMode {
        readonly: metadata.permissions().readonly(),
        posix_mode: posix_mode(metadata),
    }
}

#[cfg(unix)]
fn posix_mode(metadata: &Metadata) -> Option<u32> {
    use cap_std::fs::PermissionsExt;

    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn posix_mode(_metadata: &Metadata) -> Option<u32> {
    None
}

fn ensure_regular_metadata(metadata: &Metadata, path: &Path) -> io::Result<()> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a no-follow regular file", path.display()),
        ));
    }
    #[cfg(windows)]
    if cap_fs_ext::OsMetadataExt::file_attributes(metadata) & 0x0000_0400 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is a Windows reparse point", path.display()),
        ));
    }
    Ok(())
}

fn ensure_directory_metadata(metadata: &Metadata, path: &Path) -> io::Result<()> {
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a no-follow directory", path.display()),
        ));
    }
    #[cfg(windows)]
    if cap_fs_ext::OsMetadataExt::file_attributes(metadata) & 0x0000_0400 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is a Windows reparse point", path.display()),
        ));
    }
    Ok(())
}

fn regular_metadata_state(metadata: &Metadata, path: &Path) -> io::Result<RegularMetadataState> {
    ensure_regular_metadata(metadata, path)?;
    Ok(RegularMetadataState {
        identity: ObjectIdentity::from_u64(MetadataExt::dev(metadata), MetadataExt::ino(metadata)),
        byte_len: metadata.len(),
        mode: preserved_mode(metadata),
        link_count: Some(MetadataExt::nlink(metadata)),
    })
}

fn directory_metadata_state(
    metadata: &Metadata,
    path: &Path,
) -> io::Result<DirectoryMetadataState> {
    ensure_directory_metadata(metadata, path)?;
    Ok(DirectoryMetadataState {
        identity: ObjectIdentity::from_u64(MetadataExt::dev(metadata), MetadataExt::ino(metadata)),
        mode: preserved_mode(metadata),
        link_count: Some(MetadataExt::nlink(metadata)),
    })
}

fn open_regular_file_nofollow(parent: &Dir, name: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    options.follow(FollowSymlinks::No);
    options.nonblock(true);
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    parent.open_with(name, &options)
}

fn hash_file_bounded(file: &mut File, max_bytes: u64) -> io::Result<(String, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut byte_len = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        byte_len = byte_len
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("regular-file length overflow while hashing"))?;
        if byte_len > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("regular file grew beyond the bounded hash limit of {max_bytes} bytes"),
            ));
        }
    }
    Ok((format!("sha256:{:x}", hasher.finalize()), byte_len))
}

fn changed_during_observation(path: &Path, detail: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{} changed during exact observation: {detail}",
            path.display()
        ),
    )
}

fn observe_regular_file_metadata_exact(
    parent: &Dir,
    name: &Path,
    path: &Path,
    max_bytes: u64,
) -> io::Result<ExactFileMetadataObservation> {
    require_exact_identity_support()?;
    let path_before = regular_metadata_state(&parent.symlink_metadata(name)?, path)?;
    if path_before.byte_len > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is {} bytes, exceeding the owner-residual limit of {max_bytes} bytes",
                path.display(),
                path_before.byte_len
            ),
        ));
    }
    let file = open_regular_file_nofollow(parent, name)?;
    let handle = regular_metadata_state(&file.metadata()?, path)?;
    let path_after = regular_metadata_state(&parent.symlink_metadata(name)?, path)?;
    if handle != path_before || path_after != path_before {
        return Err(changed_during_observation(
            path,
            "metadata-only owner observation did not remain identity/mode/length/link stable",
        ));
    }
    Ok(ExactFileMetadataObservation {
        identity: handle.identity,
        byte_len: handle.byte_len,
        mode: handle.mode,
        link_count: handle.link_count,
    })
}

fn observe_created_file_exact_impl(
    parent: &Dir,
    name: &Path,
    path: &Path,
    created: &mut CreatedFile,
    max_bytes: u64,
) -> io::Result<ExactFileObservation> {
    require_exact_identity_support()?;
    let handle_before = regular_metadata_state(&created.file.metadata()?, path)?;
    let path_before = regular_metadata_state(&parent.symlink_metadata(name)?, path)?;
    if created
        .exact_metadata
        .is_some_and(|metadata| metadata.identity != handle_before.identity)
        || path_before != handle_before
    {
        return Err(changed_during_observation(
            path,
            "the still-open created handle is no longer bound to its owner path",
        ));
    }
    created.exact_metadata = Some(exact_metadata_observation(handle_before));
    if handle_before.byte_len > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is {} bytes, exceeding its declared owner length of {max_bytes} bytes",
                path.display(),
                handle_before.byte_len
            ),
        ));
    }
    let (content_hash, byte_len) = hash_file_bounded(&mut created.file, max_bytes)?;
    if byte_len != handle_before.byte_len {
        return Err(changed_during_observation(
            path,
            "the live-handle hash length differs from the opened metadata",
        ));
    }
    let handle_after = regular_metadata_state(&created.file.metadata()?, path)?;
    let path_after = regular_metadata_state(&parent.symlink_metadata(name)?, path)?;
    if handle_after != handle_before || path_after != handle_before {
        return Err(changed_during_observation(
            path,
            "the created owner changed during live-handle hashing",
        ));
    }
    Ok(ExactFileObservation {
        identity: handle_after.identity,
        byte_len,
        content_hash,
        mode: handle_after.mode,
        link_count: handle_after.link_count,
    })
}

fn exact_metadata_observation(state: RegularMetadataState) -> ExactFileMetadataObservation {
    ExactFileMetadataObservation {
        identity: state.identity,
        byte_len: state.byte_len,
        mode: state.mode,
        link_count: state.link_count,
    }
}

#[cfg(test)]
fn observe_regular_file_exact(
    parent: &Dir,
    name: &Path,
    path: &Path,
) -> io::Result<ExactFileObservation> {
    observe_regular_file_bounded_exact(parent, name, path, u64::MAX)
}

fn observe_regular_file_bounded_exact(
    parent: &Dir,
    name: &Path,
    path: &Path,
    max_bytes: u64,
) -> io::Result<ExactFileObservation> {
    require_exact_identity_support()?;
    let path_before = regular_metadata_state(&parent.symlink_metadata(name)?, path)?;
    if path_before.byte_len > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is {} bytes, exceeding the bounded hash limit of {max_bytes} bytes",
                path.display(),
                path_before.byte_len
            ),
        ));
    }
    let mut file = open_regular_file_nofollow(parent, name)?;
    let handle_before = regular_metadata_state(&file.metadata()?, path)?;
    if handle_before != path_before {
        return Err(changed_during_observation(
            path,
            "the path and no-follow handle do not identify the same state",
        ));
    }

    let (content_hash, byte_len) = hash_file_bounded(&mut file, max_bytes)?;
    if byte_len != handle_before.byte_len {
        return Err(changed_during_observation(
            path,
            "the bytes read do not match the opened length",
        ));
    }

    let handle_after = regular_metadata_state(&file.metadata()?, path)?;
    let path_after = regular_metadata_state(&parent.symlink_metadata(name)?, path)?;
    if handle_after != handle_before || path_after != handle_before {
        return Err(changed_during_observation(
            path,
            "identity, length, mode, or link count changed",
        ));
    }

    Ok(ExactFileObservation {
        identity: handle_after.identity,
        byte_len,
        content_hash,
        mode: handle_after.mode,
        link_count: handle_after.link_count,
    })
}

fn read_regular_file_exact_impl(
    parent: &Dir,
    name: &Path,
    path: &Path,
    max_bytes: u64,
) -> io::Result<ExactFileRead> {
    let read = read_regular_file_bytes_exact_impl(parent, name, path, max_bytes)?;
    let metadata = read.observation;
    Ok(ExactFileRead {
        observation: ExactFileObservation {
            identity: metadata.identity,
            byte_len: metadata.byte_len,
            content_hash: format!("sha256:{:x}", Sha256::digest(&read.bytes)),
            mode: metadata.mode,
            link_count: metadata.link_count,
        },
        bytes: read.bytes,
    })
}

fn read_regular_file_bytes_exact_impl(
    parent: &Dir,
    name: &Path,
    path: &Path,
    max_bytes: u64,
) -> io::Result<ExactFileBytesRead> {
    require_exact_identity_support()?;
    let path_before = regular_metadata_state(&parent.symlink_metadata(name)?, path)?;
    if path_before.byte_len > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is {} bytes, exceeding the exact-read limit of {max_bytes} bytes",
                path.display(),
                path_before.byte_len
            ),
        ));
    }

    let mut file = open_regular_file_nofollow(parent, name)?;
    let handle_before = regular_metadata_state(&file.metadata()?, path)?;
    if handle_before != path_before {
        return Err(changed_during_observation(
            path,
            "the path and bounded-read handle do not identify the same state",
        ));
    }

    let capacity = usize::try_from(handle_before.byte_len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} does not fit this process address space", path.display()),
        )
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut buffer = [0_u8; 64 * 1024];
    let mut byte_len = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        byte_len = byte_len
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("regular-file length overflow while reading"))?;
        if byte_len > max_bytes {
            return Err(changed_during_observation(
                path,
                "the file grew beyond the bounded-read limit",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    if byte_len != handle_before.byte_len {
        return Err(changed_during_observation(
            path,
            "the bounded bytes do not match the opened length",
        ));
    }

    let handle_after = regular_metadata_state(&file.metadata()?, path)?;
    let path_after = regular_metadata_state(&parent.symlink_metadata(name)?, path)?;
    if handle_after != handle_before || path_after != handle_before {
        return Err(changed_during_observation(
            path,
            "identity, length, mode, or link count changed during bounded read",
        ));
    }

    Ok(ExactFileBytesRead {
        bytes,
        observation: ExactFileMetadataObservation {
            identity: handle_after.identity,
            byte_len,
            mode: handle_after.mode,
            link_count: handle_after.link_count,
        },
    })
}

fn observe_directory_exact(
    endpoint: DirectoryEndpoint<'_>,
) -> io::Result<ExactDirectoryObservation> {
    require_exact_identity_support()?;
    let path_before = directory_metadata_state(
        &endpoint.parent.symlink_metadata(endpoint.name)?,
        endpoint.path,
    )?;
    let handle_before =
        directory_metadata_state(&endpoint.directory.dir_metadata()?, endpoint.path)?;
    if handle_before != path_before {
        return Err(changed_during_observation(
            endpoint.path,
            "the path and opened directory do not identify the same state",
        ));
    }
    let path_after = directory_metadata_state(
        &endpoint.parent.symlink_metadata(endpoint.name)?,
        endpoint.path,
    )?;
    let handle_after =
        directory_metadata_state(&endpoint.directory.dir_metadata()?, endpoint.path)?;
    if path_after != handle_before || handle_after != handle_before {
        return Err(changed_during_observation(
            endpoint.path,
            "directory identity, mode, or link count changed",
        ));
    }
    Ok(ExactDirectoryObservation {
        identity: handle_after.identity,
        mode: handle_after.mode,
        link_count: handle_after.link_count,
    })
}

fn validate_requested_mode(mode: u32) -> io::Result<()> {
    if mode > 0o7777 {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("requested mode {mode:#o} contains non-permission bits"),
        ))
    } else {
        Ok(())
    }
}

fn require_directory_mode(
    path: &Path,
    observation: &ExactDirectoryObservation,
    mode: u32,
) -> io::Result<()> {
    validate_requested_mode(mode)?;
    #[cfg(unix)]
    if observation.mode.posix_mode != Some(mode) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} has mode {:?}, expected exact mode {mode:#o}",
                path.display(),
                observation.mode.posix_mode
            ),
        ));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, observation, mode);
    }
    Ok(())
}

fn set_exact_directory_mode(directory: &Dir, mode: u32) -> io::Result<()> {
    validate_requested_mode(mode)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        Dir::reopen_dir(directory)?
            .into_std_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (directory, mode);
        Ok(())
    }
}

fn open_directory_exact_impl(
    parent: &Dir,
    name: &Path,
    path: &Path,
    mode: u32,
) -> io::Result<ExactDirectoryHandle> {
    require_exact_identity_support()?;
    validate_requested_mode(mode)?;
    let directory = parent.open_dir_nofollow(name)?;
    let observation =
        observe_directory_exact(DirectoryEndpoint::new(parent, name, &directory, path))?;
    require_directory_mode(path, &observation, mode)?;
    Ok(ExactDirectoryHandle {
        directory,
        observation,
    })
}

fn create_directory_exact_impl(
    parent: &Dir,
    name: &Path,
    path: &Path,
    mode: u32,
) -> io::Result<ExactDirectoryHandle> {
    require_exact_identity_support()?;
    validate_requested_mode(mode)?;
    #[cfg(unix)]
    {
        use cap_std::fs::{DirBuilder, DirBuilderExt};

        let mut builder = DirBuilder::new();
        builder.mode(mode);
        parent.create_dir_with(name, &builder)?;
    }
    #[cfg(not(unix))]
    {
        parent.create_dir(name)?;
    }

    let directory = parent.open_dir_nofollow(name)?;
    set_exact_directory_mode(&directory, mode)?;
    let observation =
        observe_directory_exact(DirectoryEndpoint::new(parent, name, &directory, path))?;
    require_directory_mode(path, &observation, mode)?;
    Ok(ExactDirectoryHandle {
        directory,
        observation,
    })
}

fn directory_entry_kind(metadata: &Metadata) -> ExactDirectoryEntryKind {
    #[cfg(windows)]
    if cap_fs_ext::OsMetadataExt::file_attributes(metadata) & 0x0000_0400 != 0 {
        return ExactDirectoryEntryKind::ReparsePoint;
    }
    if metadata.file_type().is_symlink() {
        ExactDirectoryEntryKind::Symlink
    } else if metadata.is_file() {
        ExactDirectoryEntryKind::RegularFile
    } else if metadata.is_dir() {
        ExactDirectoryEntryKind::Directory
    } else {
        ExactDirectoryEntryKind::Other
    }
}

fn directory_names_bounded(directory: &Dir, max_entries: usize) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in directory.entries()? {
        if names.len() == max_entries {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("directory exceeds its bounded inventory limit of {max_entries} entries"),
            ));
        }
        names.push(entry?.file_name());
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
fn inventory_directory_exact_impl(
    endpoint: DirectoryEndpoint<'_>,
    expected: &ExactDirectoryObservation,
) -> io::Result<ExactDirectoryInventory> {
    inventory_directory_exact_bounded_impl(endpoint, expected, usize::MAX)
}

fn inventory_directory_exact_bounded_impl(
    endpoint: DirectoryEndpoint<'_>,
    expected: &ExactDirectoryObservation,
    max_entries: usize,
) -> io::Result<ExactDirectoryInventory> {
    require_exact_directory_state(endpoint, expected)?;
    let names_before = directory_names_bounded(endpoint.directory, max_entries)?;
    let mut entries = Vec::with_capacity(names_before.len());
    for name in &names_before {
        let entry_path = endpoint.path.join(name);
        let metadata = endpoint.directory.symlink_metadata(name)?;
        entries.push(ExactDirectoryEntry {
            name: name.clone(),
            kind: directory_entry_kind(&metadata),
            identity: ObjectIdentity::from_u64(
                MetadataExt::dev(&metadata),
                MetadataExt::ino(&metadata),
            ),
            byte_len: metadata.len(),
            mode: preserved_mode(&metadata),
            link_count: Some(MetadataExt::nlink(&metadata)),
        });
        let current = endpoint.directory.symlink_metadata(name)?;
        let current_state = (
            directory_entry_kind(&current),
            ObjectIdentity::from_u64(MetadataExt::dev(&current), MetadataExt::ino(&current)),
            current.len(),
            preserved_mode(&current),
            Some(MetadataExt::nlink(&current)),
        );
        let recorded = entries.last().expect("entry was just recorded");
        if current_state
            != (
                recorded.kind,
                recorded.identity,
                recorded.byte_len,
                recorded.mode,
                recorded.link_count,
            )
        {
            return Err(changed_during_observation(
                &entry_path,
                "directory child changed during inventory",
            ));
        }
    }
    let names_after = directory_names_bounded(endpoint.directory, max_entries)?;
    if names_after != names_before {
        return Err(changed_during_observation(
            endpoint.path,
            "directory children changed during inventory",
        ));
    }
    let directory = observe_directory_exact(endpoint)?;
    if directory != *expected {
        return Err(changed_during_observation(
            endpoint.path,
            "directory state changed during inventory",
        ));
    }
    Ok(ExactDirectoryInventory { directory, entries })
}

fn observation_matches_metadata(
    observation: &ExactFileObservation,
    metadata: RegularMetadataState,
) -> bool {
    observation.identity == metadata.identity
        && observation.byte_len == metadata.byte_len
        && observation.mode == metadata.mode
        && observation.link_count == metadata.link_count
}

fn require_exact_file_state(
    endpoint: HardLinkEndpoint<'_>,
    expected: &ExactFileObservation,
) -> io::Result<()> {
    let actual = observe_regular_file_bounded_exact(
        endpoint.parent,
        endpoint.name,
        endpoint.path,
        expected.byte_len,
    )?;
    if actual == *expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} no longer matches its recorded exact file state",
                endpoint.path.display()
            ),
        ))
    }
}

fn require_exact_directory_state(
    endpoint: DirectoryEndpoint<'_>,
    expected: &ExactDirectoryObservation,
) -> io::Result<()> {
    let actual = observe_directory_exact(endpoint)?;
    if actual == *expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} no longer matches its recorded exact directory state",
                endpoint.path.display()
            ),
        ))
    }
}

fn ensure_same_parent(from: &Dir, to: &Dir, from_path: &Path, to_path: &Path) -> io::Result<()> {
    if std::ptr::eq(from, to) {
        return Ok(());
    }
    require_exact_identity_support()?;
    let from_state = directory_metadata_state(&from.dir_metadata()?, from_path)?;
    let to_state = directory_metadata_state(&to.dir_metadata()?, to_path)?;
    if from_state.identity == to_state.identity {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} and {} do not share the same pinned parent directory",
                from_path.display(),
                to_path.display()
            ),
        ))
    }
}

fn require_single_component_name(name: &Path, path: &Path) -> io::Result<()> {
    let mut components = name.components();
    if matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} must use one direct child name for capability-relative publication",
                path.display()
            ),
        ))
    }
}

fn require_name_absent(parent: &Dir, name: &Path, path: &Path) -> io::Result<()> {
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is still present", path.display()),
        )),
    }
}

fn rename_directory_noreplace_impl(
    candidate_parent: &Dir,
    candidate_name: &Path,
    candidate_path: &Path,
    target_parent: &Dir,
    target_name: &Path,
    target_path: &Path,
) -> io::Result<()> {
    let metadata = candidate_parent.symlink_metadata(candidate_name)?;
    let state = directory_metadata_state(&metadata, candidate_path)?;
    let expected = ExactRelocationSource::EmptyDirectory(ExactDirectoryObservation {
        identity: state.identity,
        mode: state.mode,
        link_count: state.link_count,
    });
    relocate_noreplace_impl(
        candidate_parent,
        candidate_name,
        candidate_path,
        target_parent,
        target_name,
        target_path,
        &expected,
    )
    .map_err(NoReplaceRelocationError::into_io)
}

fn relocate_noreplace_impl(
    owner_parent: &Dir,
    owner_name: &Path,
    owner_path: &Path,
    destination_parent: &Dir,
    destination_name: &Path,
    destination_path: &Path,
    expected_source: &ExactRelocationSource,
) -> Result<(), NoReplaceRelocationError> {
    require_single_component_name(owner_name, owner_path).map_err(NoReplaceRelocationError::Io)?;
    require_single_component_name(destination_name, destination_path)
        .map_err(NoReplaceRelocationError::Io)?;
    require_exact_identity_support().map_err(NoReplaceRelocationError::Io)?;
    let owner_parent_state = directory_metadata_state(
        &owner_parent
            .dir_metadata()
            .map_err(NoReplaceRelocationError::Io)?,
        owner_path,
    )
    .map_err(NoReplaceRelocationError::Io)?;
    let destination_parent_state = directory_metadata_state(
        &destination_parent
            .dir_metadata()
            .map_err(NoReplaceRelocationError::Io)?,
        destination_path,
    )
    .map_err(NoReplaceRelocationError::Io)?;
    if owner_parent_state.identity.namespace() != destination_parent_state.identity.namespace() {
        return Err(NoReplaceRelocationError::CrossDevice);
    }

    match expected_source {
        ExactRelocationSource::File(expected) => {
            let actual = observe_regular_file_bounded_exact(
                owner_parent,
                owner_name,
                owner_path,
                expected.byte_len,
            )
            .map_err(NoReplaceRelocationError::Io)?;
            if &actual != expected {
                return Err(NoReplaceRelocationError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} no longer matches the exact file owner",
                        owner_path.display()
                    ),
                )));
            }
        }
        ExactRelocationSource::EmptyDirectory(expected) => {
            let opened = open_directory_exact_impl(
                owner_parent,
                owner_name,
                owner_path,
                expected.mode.posix_mode.unwrap_or(0o755),
            )
            .map_err(NoReplaceRelocationError::Io)?;
            let inventory = inventory_directory_exact_bounded_impl(
                DirectoryEndpoint::new(owner_parent, owner_name, &opened.directory, owner_path),
                expected,
                0,
            )
            .map_err(NoReplaceRelocationError::Io)?;
            if inventory.directory != *expected {
                return Err(NoReplaceRelocationError::Io(changed_during_observation(
                    owner_path,
                    "the exact empty directory owner changed before relocation",
                )));
            }
        }
    }

    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    let relocation = {
        rustix::fs::renameat_with(
            owner_parent,
            owner_name,
            destination_parent,
            destination_name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            if error == rustix::io::Errno::XDEV {
                NoReplaceRelocationError::CrossDevice
            } else {
                NoReplaceRelocationError::Io(io::Error::from(error))
            }
        })
    };
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    let relocation = {
        let _ = (
            owner_parent,
            owner_name,
            destination_parent,
            destination_name,
        );
        Err(NoReplaceRelocationError::Unsupported)
    };
    relocation?;

    match expected_source {
        ExactRelocationSource::File(expected) => {
            let placed = observe_regular_file_bounded_exact(
                destination_parent,
                destination_name,
                destination_path,
                expected.byte_len,
            )
            .map_err(NoReplaceRelocationError::Io)?;
            if &placed != expected {
                return Err(NoReplaceRelocationError::Io(changed_during_observation(
                    destination_path,
                    "relocation placed a file other than the exact durable owner",
                )));
            }
        }
        ExactRelocationSource::EmptyDirectory(expected) => {
            let opened = open_directory_exact_impl(
                destination_parent,
                destination_name,
                destination_path,
                expected.mode.posix_mode.unwrap_or(0o755),
            )
            .map_err(NoReplaceRelocationError::Io)?;
            inventory_directory_exact_bounded_impl(
                DirectoryEndpoint::new(
                    destination_parent,
                    destination_name,
                    &opened.directory,
                    destination_path,
                ),
                expected,
                0,
            )
            .map_err(NoReplaceRelocationError::Io)?;
        }
    }
    require_name_absent(owner_parent, owner_name, owner_path).map_err(NoReplaceRelocationError::Io)
}

fn probe_noreplace_relocation_impl(
    parent: &Dir,
    path: &Path,
) -> Result<(), NoReplaceRelocationError> {
    require_exact_identity_support().map_err(NoReplaceRelocationError::Io)?;
    directory_metadata_state(
        &parent
            .dir_metadata()
            .map_err(NoReplaceRelocationError::Io)?,
        path,
    )
    .map_err(NoReplaceRelocationError::Io)?;
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    {
        match rustix::fs::renameat_with(
            parent,
            Path::new(""),
            parent,
            Path::new(""),
            rustix::fs::RenameFlags::NOREPLACE,
        ) {
            Err(error) if error == rustix::io::Errno::NOENT => Ok(()),
            Err(_) => Err(NoReplaceRelocationError::Unsupported),
            Ok(()) => Err(NoReplaceRelocationError::Io(io::Error::other(
                "no-replace capability probe unexpectedly renamed an empty path",
            ))),
        }
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    {
        let _ = parent;
        Err(NoReplaceRelocationError::Unsupported)
    }
}

fn set_exact_file_mode(file: &File, mode: PreservedFileMode) -> io::Result<()> {
    #[cfg(unix)]
    {
        use cap_std::fs::{Permissions, PermissionsExt};

        let posix_mode = mode.posix_mode.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "exact Unix file mode is missing its POSIX permission bits",
            )
        })?;
        file.set_permissions(Permissions::from_mode(posix_mode))
    }
    #[cfg(not(unix))]
    {
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(mode.readonly);
        file.set_permissions(permissions)
    }
}

fn create_exclusive_file_copy<F>(
    fs: &F,
    source: HardLinkEndpoint<'_>,
    expected_source: &ExactFileObservation,
    destination: HardLinkEndpoint<'_>,
) -> ExclusiveFileCopyOutcome
where
    F: FsOps + ?Sized,
{
    if let Err(source) = require_exact_identity_support() {
        return ExclusiveFileCopyOutcome::NotCreated { source };
    }

    let source_path_before = match source
        .parent
        .symlink_metadata(source.name)
        .and_then(|metadata| regular_metadata_state(&metadata, source.path))
    {
        Ok(metadata) => metadata,
        Err(source) => return ExclusiveFileCopyOutcome::NotCreated { source },
    };
    if !observation_matches_metadata(expected_source, source_path_before) {
        return ExclusiveFileCopyOutcome::NotCreated {
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} changed before it could be copied",
                    source.path.display()
                ),
            ),
        };
    }
    if let Err(source) = fs.before_read_handle(source.path) {
        return ExclusiveFileCopyOutcome::NotCreated { source };
    }
    let mut source_file = match open_regular_file_nofollow(source.parent, source.name) {
        Ok(file) => file,
        Err(source) => return ExclusiveFileCopyOutcome::NotCreated { source },
    };
    let source_handle_before = match source_file
        .metadata()
        .and_then(|metadata| regular_metadata_state(&metadata, source.path))
    {
        Ok(metadata) => metadata,
        Err(source) => return ExclusiveFileCopyOutcome::NotCreated { source },
    };
    if source_handle_before != source_path_before {
        return ExclusiveFileCopyOutcome::NotCreated {
            source: changed_during_observation(
                source.path,
                "the copy source path and handle differ",
            ),
        };
    }

    let mut destination_created = match fs
        .create_new_file(
            destination.parent,
            destination.name,
            destination.path,
            0o600,
        )
        .bind_empty(fs, destination.parent, destination.name, destination.path)
    {
        Ok(created) => created,
        Err(ExclusiveCreateFailure::NotCreated(source)) => {
            return ExclusiveFileCopyOutcome::NotCreated { source };
        }
        Err(ExclusiveCreateFailure::CreatedUnverified { created, source }) => {
            return ExclusiveFileCopyOutcome::CreatedUnverified { created, source };
        }
    };
    let destination_identity = destination_created.identity();

    let result = (|| -> io::Result<(ExactFileObservation, ExactFileObservation)> {
        if destination_identity == expected_source.identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} aliases its source instead of being an independent copy",
                    destination.path.display()
                ),
            ));
        }

        source_file.seek(SeekFrom::Start(0))?;
        let mut source_hasher = Sha256::new();
        let mut copied_len = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        while copied_len < expected_source.byte_len {
            let remaining = expected_source.byte_len - copied_len;
            let read_len = usize::try_from(remaining.min(buffer.len() as u64))
                .expect("bounded copy chunk fits usize");
            let count = source_file.read(&mut buffer[..read_len])?;
            if count == 0 {
                break;
            }
            fs.write_handle(
                &mut destination_created.file,
                destination.path,
                &buffer[..count],
            )?;
            source_hasher.update(&buffer[..count]);
            copied_len = copied_len
                .checked_add(count as u64)
                .ok_or_else(|| io::Error::other("regular-file length overflow while copying"))?;
        }
        let mut extra = [0_u8; 1];
        if source_file.read(&mut extra)? != 0 {
            return Err(changed_during_observation(
                source.path,
                "the copy source exceeds its durable declared length",
            ));
        }
        fs.flush_file(&destination_created.file, destination.path)?;
        fs.sync_handle(&destination_created.file, destination.path)?;

        let source_hash = format!("sha256:{:x}", source_hasher.finalize());
        let source_handle_after = regular_metadata_state(&source_file.metadata()?, source.path)?;
        let source_path_after =
            regular_metadata_state(&source.parent.symlink_metadata(source.name)?, source.path)?;
        if source_handle_after != source_handle_before
            || source_path_after != source_handle_before
            || copied_len != expected_source.byte_len
            || source_hash != expected_source.content_hash
        {
            return Err(changed_during_observation(
                source.path,
                "the source changed while the independent copy was populated",
            ));
        }

        fs.set_preserved_file_mode(
            &destination_created.file,
            destination.path,
            expected_source.mode,
        )?;
        fs.sync_handle(&destination_created.file, destination.path)?;
        fs.before_read_handle(destination.path)?;
        let (copy_hash, copy_len) =
            hash_file_bounded(&mut destination_created.file, expected_source.byte_len)?;
        destination_created.file.seek(SeekFrom::End(0))?;
        let copy_handle =
            regular_metadata_state(&destination_created.file.metadata()?, destination.path)?;
        let copy_path = regular_metadata_state(
            &destination.parent.symlink_metadata(destination.name)?,
            destination.path,
        )?;
        if copy_handle != copy_path || copy_handle.identity != destination_identity {
            return Err(changed_during_observation(
                destination.path,
                "the exclusive copy path and handle differ",
            ));
        }
        if copy_handle.link_count != Some(1) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is not an independent single-link copy",
                    destination.path.display()
                ),
            ));
        }
        if copy_len != expected_source.byte_len
            || copy_hash != expected_source.content_hash
            || copy_handle.mode != expected_source.mode
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} does not match the copied source bytes and mode",
                    destination.path.display()
                ),
            ));
        }

        Ok((
            ExactFileObservation {
                identity: source_handle_after.identity,
                byte_len: copied_len,
                content_hash: source_hash,
                mode: source_handle_after.mode,
                link_count: source_handle_after.link_count,
            },
            ExactFileObservation {
                identity: copy_handle.identity,
                byte_len: copy_len,
                content_hash: copy_hash,
                mode: copy_handle.mode,
                link_count: copy_handle.link_count,
            },
        ))
    })();

    match result {
        Ok((source, copy)) => ExclusiveFileCopyOutcome::CreatedVerified {
            copy: ExclusiveFileCopy {
                file: destination_created.file,
                source,
                copy,
            },
        },
        Err(source) => ExclusiveFileCopyOutcome::CreatedUnverified {
            created: destination_created,
            source,
        },
    }
}

fn remove_exact_file<F>(
    parent: &Dir,
    name: &Path,
    path: &Path,
    expected: &ExactFileObservation,
    before_unlink: F,
) -> Result<(), ExactRemovalError>
where
    F: FnOnce() -> io::Result<()>,
{
    let actual = observe_regular_file_bounded_exact(parent, name, path, expected.byte_len)
        .map_err(ExactRemovalError::not_mutated)?;
    if actual != *expected {
        return Err(ExactRemovalError::not_mutated(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not the exact recorded cleanup object",
                path.display()
            ),
        )));
    }

    let pinned =
        open_regular_file_nofollow(parent, name).map_err(ExactRemovalError::not_mutated)?;
    before_unlink().map_err(ExactRemovalError::not_mutated)?;
    let handle_state = regular_metadata_state(
        &pinned.metadata().map_err(ExactRemovalError::not_mutated)?,
        path,
    )
    .map_err(ExactRemovalError::not_mutated)?;
    let path_state = regular_metadata_state(
        &parent
            .symlink_metadata(name)
            .map_err(ExactRemovalError::not_mutated)?,
        path,
    )
    .map_err(ExactRemovalError::not_mutated)?;
    if handle_state != path_state || !observation_matches_metadata(expected, handle_state) {
        return Err(ExactRemovalError::not_mutated(changed_during_observation(
            path,
            "the cleanup path changed after exact validation",
        )));
    }
    parent
        .remove_file(name)
        .map_err(ExactRemovalError::not_mutated)?;

    let handle_after = regular_metadata_state(
        &pinned
            .metadata()
            .map_err(|source| ExactRemovalError::mutated(ExactRemovalPostName::Unknown, source))?,
        path,
    )
    .map_err(|source| ExactRemovalError::mutated(ExactRemovalPostName::Unknown, source))?;
    if handle_after.identity != expected.identity
        || handle_after.byte_len != expected.byte_len
        || handle_after.mode != expected.mode
    {
        return Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            changed_during_observation(path, "the pinned cleanup object changed across unlink"),
        ));
    }
    if let Some(expected_links) = expected.link_count
        && handle_after.link_count != expected_links.checked_sub(1)
    {
        return Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            changed_during_observation(
                path,
                "unlink did not decrement the pinned object's link count exactly once",
            ),
        ));
    }
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(metadata)
            if ObjectIdentity::from_u64(
                MetadataExt::dev(&metadata),
                MetadataExt::ino(&metadata),
            ) != expected.identity =>
        {
            // Another actor recreated the name after the exact object was
            // unlinked. It is not transaction-owned and must be preserved.
            Err(ExactRemovalError::mutated(
                ExactRemovalPostName::Substituted,
                changed_during_observation(
                    path,
                    "the cleanup name was recreated with a substituted object after exact unlink",
                ),
            ))
        }
        Ok(_) => Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            changed_during_observation(
                path,
                "the exact object still occupies its name after unlink",
            ),
        )),
        Err(error) => Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            error,
        )),
    }
}

fn remove_exact_file_metadata<F>(
    parent: &Dir,
    name: &Path,
    path: &Path,
    expected: &ExactFileMetadataObservation,
    before_unlink: F,
) -> Result<(), ExactRemovalError>
where
    F: FnOnce() -> io::Result<()>,
{
    let actual = observe_regular_file_metadata_exact(parent, name, path, expected.byte_len)
        .map_err(ExactRemovalError::not_mutated)?;
    if actual != *expected {
        return Err(ExactRemovalError::not_mutated(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not the exact metadata-bound owner residual",
                path.display()
            ),
        )));
    }
    let pinned =
        open_regular_file_nofollow(parent, name).map_err(ExactRemovalError::not_mutated)?;
    before_unlink().map_err(ExactRemovalError::not_mutated)?;
    let handle_state = regular_metadata_state(
        &pinned.metadata().map_err(ExactRemovalError::not_mutated)?,
        path,
    )
    .map_err(ExactRemovalError::not_mutated)?;
    let path_state = regular_metadata_state(
        &parent
            .symlink_metadata(name)
            .map_err(ExactRemovalError::not_mutated)?,
        path,
    )
    .map_err(ExactRemovalError::not_mutated)?;
    let expected_state = RegularMetadataState {
        identity: expected.identity,
        byte_len: expected.byte_len,
        mode: expected.mode,
        link_count: expected.link_count,
    };
    if handle_state != expected_state || path_state != expected_state {
        return Err(ExactRemovalError::not_mutated(changed_during_observation(
            path,
            "metadata-bound owner residual changed before unlink",
        )));
    }
    parent
        .remove_file(name)
        .map_err(ExactRemovalError::not_mutated)?;
    let handle_after = regular_metadata_state(
        &pinned
            .metadata()
            .map_err(|source| ExactRemovalError::mutated(ExactRemovalPostName::Unknown, source))?,
        path,
    )
    .map_err(|source| ExactRemovalError::mutated(ExactRemovalPostName::Unknown, source))?;
    if handle_after.identity != expected.identity
        || handle_after.byte_len != expected.byte_len
        || handle_after.mode != expected.mode
    {
        return Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            changed_during_observation(path, "metadata-bound owner residual changed across unlink"),
        ));
    }
    if let Some(expected_links) = expected.link_count
        && handle_after.link_count != expected_links.checked_sub(1)
    {
        return Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            changed_during_observation(
                path,
                "metadata-bound owner residual link count did not decrement exactly once",
            ),
        ));
    }
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(metadata)
            if ObjectIdentity::from_u64(
                MetadataExt::dev(&metadata),
                MetadataExt::ino(&metadata),
            ) != expected.identity =>
        {
            Err(ExactRemovalError::mutated(
                ExactRemovalPostName::Substituted,
                changed_during_observation(
                    path,
                    "the owner-residual name was recreated with a substituted object after exact unlink",
                ),
            ))
        }
        Ok(_) => Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            changed_during_observation(
                path,
                "metadata-bound owner residual still occupies its name after unlink",
            ),
        )),
        Err(error) => Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            error,
        )),
    }
}

fn remove_empty_directory_exact_impl<F>(
    endpoint: DirectoryEndpoint<'_>,
    expected: &ExactDirectoryObservation,
    before_unlink: F,
) -> Result<(), ExactRemovalError>
where
    F: FnOnce() -> io::Result<()>,
{
    require_exact_empty_directory(endpoint, expected).map_err(ExactRemovalError::not_mutated)?;

    before_unlink().map_err(ExactRemovalError::not_mutated)?;
    require_exact_empty_directory(endpoint, expected).map_err(ExactRemovalError::not_mutated)?;
    endpoint
        .parent
        .remove_dir(endpoint.name)
        .map_err(ExactRemovalError::not_mutated)?;

    let handle_after = directory_metadata_state(
        &endpoint
            .directory
            .dir_metadata()
            .map_err(|source| ExactRemovalError::mutated(ExactRemovalPostName::Unknown, source))?,
        endpoint.path,
    )
    .map_err(|source| ExactRemovalError::mutated(ExactRemovalPostName::Unknown, source))?;
    if handle_after.identity != expected.identity || handle_after.mode != expected.mode {
        return Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            changed_during_observation(
                endpoint.path,
                "the pinned directory changed across removal",
            ),
        ));
    }
    match endpoint.parent.symlink_metadata(endpoint.name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(metadata)
            if ObjectIdentity::from_u64(
                MetadataExt::dev(&metadata),
                MetadataExt::ino(&metadata),
            ) != expected.identity =>
        {
            Err(ExactRemovalError::mutated(
                ExactRemovalPostName::Substituted,
                changed_during_observation(
                    endpoint.path,
                    "the directory cleanup name was recreated with a substituted object after exact removal",
                ),
            ))
        }
        Ok(_) => Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            changed_during_observation(
                endpoint.path,
                "the exact directory still occupies its name after removal",
            ),
        )),
        Err(error) => Err(ExactRemovalError::mutated(
            ExactRemovalPostName::Unknown,
            error,
        )),
    }
}

fn require_exact_empty_directory(
    endpoint: DirectoryEndpoint<'_>,
    expected: &ExactDirectoryObservation,
) -> io::Result<()> {
    require_exact_directory_state(endpoint, expected)?;
    let mut entries = endpoint.directory.entries()?;
    if entries.next().transpose()?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::DirectoryNotEmpty,
            format!(
                "{} is not an exact empty directory",
                endpoint.path.display()
            ),
        ));
    }
    let after = observe_directory_exact(endpoint)?;
    if after != *expected {
        return Err(changed_during_observation(
            endpoint.path,
            "empty directory changed during exact observation",
        ));
    }
    Ok(())
}

fn create_new_file_impl(
    parent: &Dir,
    name: &Path,
    _path: &Path,
    mode: u32,
    before_identity: impl FnOnce() -> io::Result<()>,
) -> ExclusiveCreateOutcome {
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    options.follow(FollowSymlinks::No);
    options.nonblock(true);
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows_sys::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE},
            Storage::FileSystem::{DELETE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE},
        };

        options.access_mode(GENERIC_READ | GENERIC_WRITE | DELETE);
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;

        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let file = match parent.open_with(name, &options) {
        Ok(file) => file,
        Err(source) => return ExclusiveCreateOutcome::NotCreated { source },
    };
    let created = CreatedFile::unverified(file);
    if let Err(source) = before_identity() {
        return ExclusiveCreateOutcome::CreatedUnverified { created, source };
    }
    match opened_regular_file_metadata(&created.file) {
        Ok(metadata) => ExclusiveCreateOutcome::CreatedVerified {
            created: CreatedFile::verified(created.file, metadata),
        },
        Err(source) => ExclusiveCreateOutcome::CreatedUnverified { created, source },
    }
}

fn opened_regular_file_metadata(file: &File) -> io::Result<ExactFileMetadataObservation> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "controlled file is not a regular file",
        ));
    }
    #[cfg(windows)]
    if cap_fs_ext::OsMetadataExt::file_attributes(&metadata) & 0x0000_0400 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "controlled file is a Windows reparse point",
        ));
    }
    Ok(ExactFileMetadataObservation {
        identity: ObjectIdentity::from_u64(
            MetadataExt::dev(&metadata),
            MetadataExt::ino(&metadata),
        ),
        byte_len: metadata.len(),
        mode: preserved_mode(&metadata),
        link_count: Some(MetadataExt::nlink(&metadata)),
    })
}

pub(crate) fn current_regular_file_identity(
    parent: &Dir,
    name: &Path,
) -> io::Result<ObjectIdentity> {
    let metadata = parent.symlink_metadata(name)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "controlled path is not a regular file",
        ));
    }
    #[cfg(windows)]
    if cap_fs_ext::OsMetadataExt::file_attributes(&metadata) & 0x0000_0400 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "controlled path is a Windows reparse point",
        ));
    }
    Ok(ObjectIdentity::from_u64(
        MetadataExt::dev(&metadata),
        MetadataExt::ino(&metadata),
    ))
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsOperation {
    BootstrapWorkspace {
        after: bool,
    },
    PublishWorkspaceOwnership {
        after: bool,
    },
    AdoptBootstrapFinalizationSlot {
        after: bool,
    },
    PrepareJournalPartial {
        sequence: u64,
        after: bool,
    },
    PublishJournalRecord {
        sequence: u64,
        after: bool,
    },
    LinkJournalAlias {
        sequence: u64,
        after: bool,
    },
    AdoptJournalPublication {
        sequence: u64,
        after: bool,
    },
    PrepareDirectoryOwner {
        ordinal: u32,
        after: bool,
    },
    DiscardDirectoryOwner {
        ordinal: u32,
        after: bool,
    },
    PlaceDirectoryOwner {
        ordinal: u32,
        after: bool,
    },
    CancelDirectoryPlacement {
        ordinal: u32,
        after: bool,
    },
    PrepareStage {
        ordinal: u32,
        after: bool,
    },
    DiscardStageOwner {
        ordinal: u32,
        after: bool,
    },
    PlaceStage {
        ordinal: u32,
        after: bool,
    },
    CancelStagePlacement {
        ordinal: u32,
        after: bool,
    },
    PrepareBackup {
        ordinal: u32,
        after: bool,
    },
    DiscardBackupOwner {
        ordinal: u32,
        after: bool,
    },
    PlaceBackup {
        ordinal: u32,
        after: bool,
    },
    CancelBackupPlacement {
        ordinal: u32,
        after: bool,
    },
    ReplaceTarget {
        ordinal: u32,
        after: bool,
    },
    RollbackCreatedTarget {
        ordinal: u32,
        after: bool,
    },
    RollbackBackup {
        ordinal: u32,
        after: bool,
    },
    CommitBoundary {
        sequence: u64,
        after: bool,
    },
    CleanupStage {
        rollback: bool,
        owned: bool,
        ordinal: u32,
        after: bool,
    },
    CleanupBackup {
        rollback: bool,
        owned: bool,
        ordinal: u32,
        after: bool,
    },
    CleanupCreatedDirectory {
        rollback: bool,
        ordinal: u32,
        after: bool,
    },
    CleanupDirectoryCandidate {
        rollback: bool,
        ordinal: u32,
        after: bool,
    },
    PublishFinalizationLease {
        rollback: bool,
        generation: u64,
        after: bool,
    },
    PrepareFinalizationPartial {
        rollback: bool,
        generation: u64,
        after: bool,
    },
    LinkFinalizationAlias {
        rollback: bool,
        generation: u64,
        after: bool,
    },
    CertifyFinalizationPartial {
        rollback: bool,
        generation: u64,
        after: bool,
    },
    AdoptFinalizationStage {
        rollback: bool,
        generation: u64,
        stage: super::runtime::FinalizationAdoptionStage,
        after: bool,
    },
    PublishFinalizationProgress {
        rollback: bool,
        generation: u64,
        after: bool,
    },
    RemoveWorkspaceBootstrapIntent {
        rollback: bool,
        after: bool,
    },
    RemoveWorkspaceBootstrapOwner {
        rollback: bool,
        after: bool,
    },
    RemovePublishedJournalHistory {
        rollback: bool,
        sequence: u64,
        after: bool,
    },
    RemovePartialJournalHistory {
        rollback: bool,
        sequence: u64,
        after: bool,
    },
    RemoveTransactionWorkspace {
        rollback: bool,
        after: bool,
    },
    RemoveFinalizationLease {
        rollback: bool,
        generation: u64,
        after: bool,
    },
    CleanupFinalizationPartial {
        rollback: bool,
        generation: u64,
        after: bool,
    },
    CreateDirectory,
    OpenCoordinationFile,
    InspectMetadata,
    ReadHandle,
    ObserveRegularFile,
    ObserveRegularFileBounded,
    ObserveRegularFileMetadata,
    ObserveCreatedFileExact,
    ReadRegularFileExact,
    ObserveDirectory,
    OpenDirectoryExact,
    CreateDirectoryExact,
    InventoryDirectoryExact,
    InventoryDirectoryExactBounded,
    CreateNewFile,
    AcquireCreatedFileIdentity,
    CreateExclusiveCopy,
    #[cfg(windows)]
    OpenCleanupFile,
    #[cfg(windows)]
    OpenCandidateOwner,
    SetFileMode,
    SetDirectoryMode,
    WriteHandle,
    SyncHandle,
    FlushFile,
    SyncDirectory,
    SyncTargetParent,
    SyncJournalParent,
    TryLock,
    HardLink,
    PublishAbsent,
    RenameDirectoryNoReplace,
    RelocateNoReplace,
    ProbeRelocateNoReplace,
    RemoveFile,
    RemoveFileExact,
    RemoveFileMetadataExact,
    BeforeExactUnlink,
    #[cfg(windows)]
    RemoveFileByHandle,
    RemoveDirectory,
    RemoveDirectoryExact,
    BeforeMutationRebind,
    BeforeFinalRevalidation,
    AfterFinalRevalidation,
    Rename,
    ReplaceExisting,
    RenameJournal,
}

#[cfg(test)]
impl FsOperation {
    pub(crate) const fn is_semantic_transition(self) -> bool {
        matches!(
            self,
            Self::BootstrapWorkspace { .. }
                | Self::PublishWorkspaceOwnership { .. }
                | Self::AdoptBootstrapFinalizationSlot { .. }
                | Self::PrepareJournalPartial { .. }
                | Self::PublishJournalRecord { .. }
                | Self::LinkJournalAlias { .. }
                | Self::AdoptJournalPublication { .. }
                | Self::PrepareDirectoryOwner { .. }
                | Self::DiscardDirectoryOwner { .. }
                | Self::PlaceDirectoryOwner { .. }
                | Self::CancelDirectoryPlacement { .. }
                | Self::PrepareStage { .. }
                | Self::DiscardStageOwner { .. }
                | Self::PlaceStage { .. }
                | Self::CancelStagePlacement { .. }
                | Self::PrepareBackup { .. }
                | Self::DiscardBackupOwner { .. }
                | Self::PlaceBackup { .. }
                | Self::CancelBackupPlacement { .. }
                | Self::ReplaceTarget { .. }
                | Self::RollbackCreatedTarget { .. }
                | Self::RollbackBackup { .. }
                | Self::CommitBoundary { .. }
                | Self::CleanupStage { .. }
                | Self::CleanupBackup { .. }
                | Self::CleanupCreatedDirectory { .. }
                | Self::CleanupDirectoryCandidate { .. }
                | Self::PublishFinalizationLease { .. }
                | Self::PrepareFinalizationPartial { .. }
                | Self::LinkFinalizationAlias { .. }
                | Self::CertifyFinalizationPartial { .. }
                | Self::AdoptFinalizationStage { .. }
                | Self::PublishFinalizationProgress { .. }
                | Self::RemoveWorkspaceBootstrapIntent { .. }
                | Self::RemoveWorkspaceBootstrapOwner { .. }
                | Self::RemovePublishedJournalHistory { .. }
                | Self::RemovePartialJournalHistory { .. }
                | Self::RemoveTransactionWorkspace { .. }
                | Self::RemoveFinalizationLease { .. }
                | Self::CleanupFinalizationPartial { .. }
        )
    }
}

#[cfg(test)]
fn semantic_operation(key: super::runtime::TransitionKey) -> FsOperation {
    use super::runtime::{
        CleanupObjectKind, JournalRecordKind, PreparationArtifactKind, RollbackAction,
        TransactionOutcome, TransitionKey, TransitionWindow,
    };

    let after = |window| window == TransitionWindow::After;
    let rollback = |outcome| outcome == TransactionOutcome::Rollback;
    match key {
        TransitionKey::BootstrapWorkspace { window } => FsOperation::BootstrapWorkspace {
            after: after(window),
        },
        TransitionKey::PublishWorkspaceOwnership { window } => {
            FsOperation::PublishWorkspaceOwnership {
                after: after(window),
            }
        }
        TransitionKey::AdoptBootstrapFinalizationSlot { window } => {
            FsOperation::AdoptBootstrapFinalizationSlot {
                after: after(window),
            }
        }
        TransitionKey::PrepareJournalPartial { sequence, window } => {
            FsOperation::PrepareJournalPartial {
                sequence,
                after: after(window),
            }
        }
        TransitionKey::PublishJournalRecord { sequence, window } => {
            FsOperation::PublishJournalRecord {
                sequence,
                after: after(window),
            }
        }
        TransitionKey::LinkJournalAlias { sequence, window } => FsOperation::LinkJournalAlias {
            sequence,
            after: after(window),
        },
        TransitionKey::AdoptJournalPublication { sequence, window } => {
            FsOperation::AdoptJournalPublication {
                sequence,
                after: after(window),
            }
        }
        TransitionKey::OwnerPrepared {
            artifact,
            ordinal,
            window,
        } => match artifact {
            PreparationArtifactKind::Directory => FsOperation::PrepareDirectoryOwner {
                ordinal,
                after: after(window),
            },
            PreparationArtifactKind::Stage => FsOperation::PrepareStage {
                ordinal,
                after: after(window),
            },
            PreparationArtifactKind::Backup => FsOperation::PrepareBackup {
                ordinal,
                after: after(window),
            },
        },
        TransitionKey::DiscardOwner {
            artifact,
            ordinal,
            window,
        } => match artifact {
            PreparationArtifactKind::Directory => FsOperation::DiscardDirectoryOwner {
                ordinal,
                after: after(window),
            },
            PreparationArtifactKind::Stage => FsOperation::DiscardStageOwner {
                ordinal,
                after: after(window),
            },
            PreparationArtifactKind::Backup => FsOperation::DiscardBackupOwner {
                ordinal,
                after: after(window),
            },
        },
        TransitionKey::Placement {
            artifact,
            ordinal,
            window,
        } => match artifact {
            PreparationArtifactKind::Directory => FsOperation::PlaceDirectoryOwner {
                ordinal,
                after: after(window),
            },
            PreparationArtifactKind::Stage => FsOperation::PlaceStage {
                ordinal,
                after: after(window),
            },
            PreparationArtifactKind::Backup => FsOperation::PlaceBackup {
                ordinal,
                after: after(window),
            },
        },
        TransitionKey::CancelPlacement {
            artifact,
            ordinal,
            window,
        } => match artifact {
            PreparationArtifactKind::Directory => FsOperation::CancelDirectoryPlacement {
                ordinal,
                after: after(window),
            },
            PreparationArtifactKind::Stage => FsOperation::CancelStagePlacement {
                ordinal,
                after: after(window),
            },
            PreparationArtifactKind::Backup => FsOperation::CancelBackupPlacement {
                ordinal,
                after: after(window),
            },
        },
        TransitionKey::ReplaceTarget { ordinal, window } => FsOperation::ReplaceTarget {
            ordinal,
            after: after(window),
        },
        TransitionKey::RollbackTarget {
            action,
            ordinal,
            window,
        } => match action {
            RollbackAction::RemoveCreatedTarget => FsOperation::RollbackCreatedTarget {
                ordinal,
                after: after(window),
            },
            RollbackAction::RestoreBackup => FsOperation::RollbackBackup {
                ordinal,
                after: after(window),
            },
        },
        TransitionKey::CommitBoundary { sequence, window } => FsOperation::CommitBoundary {
            sequence,
            after: after(window),
        },
        TransitionKey::CleanupObject {
            outcome,
            kind,
            ordinal,
            window,
        } => match kind {
            CleanupObjectKind::OwnedStage => FsOperation::CleanupStage {
                rollback: rollback(outcome),
                owned: true,
                ordinal,
                after: after(window),
            },
            CleanupObjectKind::PlacedStage => FsOperation::CleanupStage {
                rollback: rollback(outcome),
                owned: false,
                ordinal,
                after: after(window),
            },
            CleanupObjectKind::OwnedBackup => FsOperation::CleanupBackup {
                rollback: rollback(outcome),
                owned: true,
                ordinal,
                after: after(window),
            },
            CleanupObjectKind::PlacedBackup => FsOperation::CleanupBackup {
                rollback: rollback(outcome),
                owned: false,
                ordinal,
                after: after(window),
            },
            CleanupObjectKind::CreatedDirectory => FsOperation::CleanupCreatedDirectory {
                rollback: rollback(outcome),
                ordinal,
                after: after(window),
            },
            CleanupObjectKind::OwnedDirectory => FsOperation::CleanupDirectoryCandidate {
                rollback: rollback(outcome),
                ordinal,
                after: after(window),
            },
        },
        TransitionKey::PublishFinalizationLease {
            outcome,
            generation,
            window,
        } => FsOperation::PublishFinalizationLease {
            rollback: rollback(outcome),
            generation,
            after: after(window),
        },
        TransitionKey::PrepareFinalizationPartial {
            outcome,
            generation,
            window,
        } => FsOperation::PrepareFinalizationPartial {
            rollback: rollback(outcome),
            generation,
            after: after(window),
        },
        TransitionKey::LinkFinalizationAlias {
            outcome,
            generation,
            window,
        } => FsOperation::LinkFinalizationAlias {
            rollback: rollback(outcome),
            generation,
            after: after(window),
        },
        TransitionKey::CertifyFinalizationPartial {
            outcome,
            generation,
            window,
        } => FsOperation::CertifyFinalizationPartial {
            rollback: rollback(outcome),
            generation,
            after: after(window),
        },
        TransitionKey::AdoptFinalizationStage {
            outcome,
            generation,
            stage,
            window,
        } => FsOperation::AdoptFinalizationStage {
            rollback: rollback(outcome),
            generation,
            stage,
            after: after(window),
        },
        TransitionKey::PublishFinalizationProgress {
            outcome,
            generation,
            window,
        } => FsOperation::PublishFinalizationProgress {
            rollback: rollback(outcome),
            generation,
            after: after(window),
        },
        TransitionKey::RemoveWorkspaceBootstrapIntent { outcome, window } => {
            FsOperation::RemoveWorkspaceBootstrapIntent {
                rollback: rollback(outcome),
                after: after(window),
            }
        }
        TransitionKey::RemoveWorkspaceBootstrapOwner { outcome, window } => {
            FsOperation::RemoveWorkspaceBootstrapOwner {
                rollback: rollback(outcome),
                after: after(window),
            }
        }
        TransitionKey::RemoveJournalHistory {
            outcome,
            kind,
            sequence,
            window,
        } => match kind {
            JournalRecordKind::Published => FsOperation::RemovePublishedJournalHistory {
                rollback: rollback(outcome),
                sequence,
                after: after(window),
            },
            JournalRecordKind::Partial => FsOperation::RemovePartialJournalHistory {
                rollback: rollback(outcome),
                sequence,
                after: after(window),
            },
        },
        TransitionKey::RemoveTransactionWorkspace { outcome, window } => {
            FsOperation::RemoveTransactionWorkspace {
                rollback: rollback(outcome),
                after: after(window),
            }
        }
        TransitionKey::RemoveFinalizationLease {
            outcome,
            generation,
            window,
        } => FsOperation::RemoveFinalizationLease {
            rollback: rollback(outcome),
            generation,
            after: after(window),
        },
        TransitionKey::CleanupFinalizationPartial {
            outcome,
            generation,
            window,
        } => FsOperation::CleanupFinalizationPartial {
            rollback: rollback(outcome),
            generation,
            after: after(window),
        },
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FsEvent {
    pub operation: FsOperation,
    pub path: PathBuf,
    pub destination: Option<PathBuf>,
}

#[cfg(test)]
#[derive(Debug)]
enum FinalRevalidationMutation {
    WriteFile {
        target: PathBuf,
        content: Vec<u8>,
    },
    ReplaceFile {
        target: PathBuf,
        moved_target: PathBuf,
        content: Vec<u8>,
    },
    ReplaceFileOnChildMutation {
        trigger_parent: PathBuf,
        target: PathBuf,
        moved_target: PathBuf,
        content: Vec<u8>,
    },
    #[cfg(unix)]
    ReplaceParentWithSymlink {
        target: PathBuf,
        parent: PathBuf,
        moved_parent: PathBuf,
        referent: PathBuf,
    },
    #[cfg(unix)]
    ReplaceParentWithDirectory {
        target: PathBuf,
        parent: PathBuf,
        moved_parent: PathBuf,
    },
    #[cfg(unix)]
    ReplaceParentWithDirectoryOnChildMutation {
        trigger_parent: PathBuf,
        parent: PathBuf,
        moved_parent: PathBuf,
    },
}

#[cfg(test)]
#[derive(Debug)]
struct PauseAfterSuccess {
    operation: FsOperation,
    ordinal: usize,
    ready: PathBuf,
    release: PathBuf,
}

#[cfg(test)]
impl FinalRevalidationMutation {
    fn matches_trigger(&self, path: &Path) -> bool {
        match self {
            Self::WriteFile { target, .. } | Self::ReplaceFile { target, .. } => target == path,
            Self::ReplaceFileOnChildMutation { trigger_parent, .. } => {
                path.parent() == Some(trigger_parent.as_path())
            }
            #[cfg(unix)]
            Self::ReplaceParentWithSymlink { target, .. }
            | Self::ReplaceParentWithDirectory { target, .. } => target == path,
            #[cfg(unix)]
            Self::ReplaceParentWithDirectoryOnChildMutation { trigger_parent, .. } => {
                path.parent() == Some(trigger_parent.as_path())
            }
        }
    }

    fn apply(self) -> io::Result<()> {
        match self {
            Self::WriteFile { target, content } => fs::write(target, content),
            Self::ReplaceFile {
                target,
                moved_target,
                content,
            }
            | Self::ReplaceFileOnChildMutation {
                target,
                moved_target,
                content,
                ..
            } => {
                fs::rename(&target, moved_target)?;
                fs::write(target, content)
            }
            #[cfg(unix)]
            Self::ReplaceParentWithSymlink {
                parent,
                moved_parent,
                referent,
                ..
            } => {
                fs::rename(&parent, moved_parent)?;
                std::os::unix::fs::symlink(referent, parent)
            }
            #[cfg(unix)]
            Self::ReplaceParentWithDirectory {
                parent,
                moved_parent,
                ..
            }
            | Self::ReplaceParentWithDirectoryOnChildMutation {
                parent,
                moved_parent,
                ..
            } => {
                fs::rename(&parent, moved_parent)?;
                fs::create_dir(parent)
            }
        }
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FaultFs {
    fail: std::sync::Mutex<Option<FaultMode>>,
    fail_after_success: std::sync::Mutex<Option<FaultMode>>,
    crash: std::sync::Mutex<Option<FaultMode>>,
    counts: std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    events: std::sync::Mutex<Vec<FsEvent>>,
    pauses_after_success: Vec<PauseAfterSuccess>,
    mutation_rebind_mutation: std::sync::Mutex<Option<FinalRevalidationMutation>>,
    final_revalidation_mutation: std::sync::Mutex<Option<FinalRevalidationMutation>>,
    post_revalidation_mutation: std::sync::Mutex<Option<FinalRevalidationMutation>>,
    exact_unlink_mutation: std::sync::Mutex<Option<FinalRevalidationMutation>>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
enum FaultMode {
    Once {
        operation: FsOperation,
        ordinal: usize,
    },
    From {
        operation: FsOperation,
        ordinal: usize,
    },
}

#[cfg(test)]
impl FaultMode {
    fn matches(self, operation: FsOperation, ordinal: usize) -> bool {
        match self {
            Self::Once {
                operation: target,
                ordinal: target_ordinal,
            } => target == operation && target_ordinal == ordinal,
            Self::From {
                operation: target,
                ordinal: target_ordinal,
            } => target == operation && ordinal >= target_ordinal,
        }
    }
}

#[cfg(test)]
impl FaultFs {
    pub(crate) fn passthrough() -> Self {
        Self {
            fail: std::sync::Mutex::new(None),
            fail_after_success: std::sync::Mutex::new(None),
            crash: std::sync::Mutex::new(None),
            counts: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            events: std::sync::Mutex::new(Vec::new()),
            pauses_after_success: Vec::new(),
            mutation_rebind_mutation: std::sync::Mutex::new(None),
            final_revalidation_mutation: std::sync::Mutex::new(None),
            post_revalidation_mutation: std::sync::Mutex::new(None),
            exact_unlink_mutation: std::sync::Mutex::new(None),
        }
    }

    pub(crate) fn fail_nth(operation: FsOperation, ordinal: usize) -> Self {
        assert!(ordinal > 0, "fault ordinal is one-based");
        let fs = Self::passthrough();
        *fs.fail.lock().expect("fault lock") = Some(FaultMode::Once { operation, ordinal });
        fs
    }

    pub(crate) fn fail_from(operation: FsOperation, ordinal: usize) -> Self {
        assert!(ordinal > 0, "fault ordinal is one-based");
        let fs = Self::passthrough();
        *fs.fail.lock().expect("fault lock") = Some(FaultMode::From { operation, ordinal });
        fs
    }

    pub(crate) fn fail_after_success_nth(operation: FsOperation, ordinal: usize) -> Self {
        assert!(ordinal > 0, "fault ordinal is one-based");
        let fs = Self::passthrough();
        *fs.fail_after_success.lock().expect("fault lock") =
            Some(FaultMode::Once { operation, ordinal });
        fs
    }

    /// Panics at one exact semantic protocol transition, modelling abrupt
    /// process death without allowing in-process error reconciliation to run.
    pub(crate) fn crash_nth(operation: FsOperation, ordinal: usize) -> Self {
        assert!(
            operation.is_semantic_transition(),
            "semantic crashes require a protocol transition key"
        );
        assert!(ordinal > 0, "crash ordinal is one-based");
        let fs = Self::passthrough();
        *fs.crash.lock().expect("crash lock") = Some(FaultMode::Once { operation, ordinal });
        fs
    }

    pub(crate) fn fail_nth_and_crash_nth(
        failed_operation: FsOperation,
        failed_ordinal: usize,
        crash_operation: FsOperation,
        crash_ordinal: usize,
    ) -> Self {
        assert!(failed_ordinal > 0, "fault ordinal is one-based");
        assert!(
            crash_operation.is_semantic_transition(),
            "semantic crashes require a protocol transition key"
        );
        assert!(crash_ordinal > 0, "crash ordinal is one-based");
        let fs = Self::passthrough();
        *fs.fail.lock().expect("fault lock") = Some(FaultMode::Once {
            operation: failed_operation,
            ordinal: failed_ordinal,
        });
        *fs.crash.lock().expect("crash lock") = Some(FaultMode::Once {
            operation: crash_operation,
            ordinal: crash_ordinal,
        });
        fs
    }

    pub(crate) fn events(&self) -> Vec<FsEvent> {
        self.events.lock().expect("event lock").clone()
    }

    pub(crate) fn pause_after_success(
        operation: FsOperation,
        ordinal: usize,
        ready: PathBuf,
        release: PathBuf,
    ) -> Self {
        assert!(ordinal > 0, "pause ordinal is one-based");
        let mut fs = Self::passthrough();
        fs.pauses_after_success.push(PauseAfterSuccess {
            operation,
            ordinal,
            ready,
            release,
        });
        fs
    }

    pub(crate) fn pause_after_successes(
        pauses: Vec<(FsOperation, usize, PathBuf, PathBuf)>,
    ) -> Self {
        let mut fs = Self::passthrough();
        fs.pauses_after_success = pauses
            .into_iter()
            .map(|(operation, ordinal, ready, release)| {
                assert!(ordinal > 0, "pause ordinal is one-based");
                PauseAfterSuccess {
                    operation,
                    ordinal,
                    ready,
                    release,
                }
            })
            .collect();
        fs
    }

    pub(crate) fn mutate_before_final_revalidation(path: PathBuf, content: Vec<u8>) -> Self {
        let fs = Self::passthrough();
        *fs.final_revalidation_mutation
            .lock()
            .expect("mutation lock") = Some(FinalRevalidationMutation::WriteFile {
            target: path,
            content,
        });
        fs
    }

    pub(crate) fn mutate_before_mutation_rebind(path: PathBuf, content: Vec<u8>) -> Self {
        let fs = Self::passthrough();
        *fs.mutation_rebind_mutation.lock().expect("mutation lock") =
            Some(FinalRevalidationMutation::WriteFile {
                target: path,
                content,
            });
        fs
    }

    #[cfg(unix)]
    pub(crate) fn replace_parent_with_directory_before_mutation_rebind(
        target: PathBuf,
        parent: PathBuf,
        moved_parent: PathBuf,
    ) -> Self {
        let fs = Self::passthrough();
        *fs.mutation_rebind_mutation.lock().expect("mutation lock") =
            Some(FinalRevalidationMutation::ReplaceParentWithDirectory {
                target,
                parent,
                moved_parent,
            });
        fs
    }

    pub(crate) fn mutate_after_final_revalidation(path: PathBuf, content: Vec<u8>) -> Self {
        let fs = Self::passthrough();
        *fs.post_revalidation_mutation.lock().expect("mutation lock") =
            Some(FinalRevalidationMutation::WriteFile {
                target: path,
                content,
            });
        fs
    }

    pub(crate) fn substitute_before_exact_unlink(
        target: PathBuf,
        moved_target: PathBuf,
        content: Vec<u8>,
    ) -> Self {
        let fs = Self::passthrough();
        *fs.exact_unlink_mutation.lock().expect("mutation lock") =
            Some(FinalRevalidationMutation::ReplaceFile {
                target,
                moved_target,
                content,
            });
        fs
    }

    pub(crate) fn substitute_file_before_child_mutation(
        trigger_parent: PathBuf,
        target: PathBuf,
        moved_target: PathBuf,
        content: Vec<u8>,
    ) -> Self {
        let fs = Self::passthrough();
        *fs.mutation_rebind_mutation.lock().expect("mutation lock") =
            Some(FinalRevalidationMutation::ReplaceFileOnChildMutation {
                trigger_parent,
                target,
                moved_target,
                content,
            });
        fs
    }

    #[cfg(unix)]
    pub(crate) fn replace_parent_with_directory_before_child_mutation(
        parent: PathBuf,
        moved_parent: PathBuf,
    ) -> Self {
        let fs = Self::passthrough();
        *fs.mutation_rebind_mutation.lock().expect("mutation lock") = Some(
            FinalRevalidationMutation::ReplaceParentWithDirectoryOnChildMutation {
                trigger_parent: parent.clone(),
                parent,
                moved_parent,
            },
        );
        fs
    }

    #[cfg(unix)]
    pub(crate) fn replace_parent_before_final_revalidation(
        target: PathBuf,
        parent: PathBuf,
        moved_parent: PathBuf,
        referent: PathBuf,
    ) -> Self {
        let fs = Self::passthrough();
        *fs.final_revalidation_mutation
            .lock()
            .expect("mutation lock") = Some(FinalRevalidationMutation::ReplaceParentWithSymlink {
            target,
            parent,
            moved_parent,
            referent,
        });
        fs
    }

    #[cfg(unix)]
    pub(crate) fn replace_parent_after_final_revalidation(
        target: PathBuf,
        parent: PathBuf,
        moved_parent: PathBuf,
        referent: PathBuf,
    ) -> Self {
        let fs = Self::passthrough();
        *fs.post_revalidation_mutation.lock().expect("mutation lock") =
            Some(FinalRevalidationMutation::ReplaceParentWithSymlink {
                target,
                parent,
                moved_parent,
                referent,
            });
        fs
    }

    #[cfg(unix)]
    pub(crate) fn replace_parent_with_directory_after_final_revalidation(
        target: PathBuf,
        parent: PathBuf,
        moved_parent: PathBuf,
    ) -> Self {
        let fs = Self::passthrough();
        *fs.post_revalidation_mutation.lock().expect("mutation lock") =
            Some(FinalRevalidationMutation::ReplaceParentWithDirectory {
                target,
                parent,
                moved_parent,
            });
        fs
    }

    fn before(
        &self,
        operation: FsOperation,
        path: &Path,
        destination: Option<&Path>,
    ) -> io::Result<()> {
        let ordinal = self.record(operation, path, destination);
        if self
            .fail
            .lock()
            .expect("fault lock")
            .is_some_and(|mode| mode.matches(operation, ordinal))
        {
            Err(io::Error::other(format!(
                "injected {operation:?} failure at ordinal {ordinal}"
            )))
        } else {
            Ok(())
        }
    }

    fn record(&self, operation: FsOperation, path: &Path, destination: Option<&Path>) -> usize {
        self.events.lock().expect("event lock").push(FsEvent {
            operation,
            path: path.to_path_buf(),
            destination: destination.map(Path::to_path_buf),
        });
        let key = format!("{operation:?}");
        {
            let mut counts = self.counts.lock().expect("count lock");
            let count = counts.entry(key).or_default();
            *count += 1;
            *count
        }
    }

    fn observe_semantic_transition(&self, key: super::runtime::TransitionKey) {
        let operation = semantic_operation(key);
        let path = Path::new("<semantic-transaction-transition>");
        let ordinal = self.record(operation, path, None);
        if self
            .crash
            .lock()
            .expect("crash lock")
            .is_some_and(|mode| mode.matches(operation, ordinal))
        {
            panic!("injected crash at {operation:?} occurrence {ordinal}");
        }
        self.after_success(operation, path)
            .unwrap_or_else(|source| panic!("semantic transition barrier failed: {source}"));
    }

    fn after_success(&self, operation: FsOperation, path: &Path) -> io::Result<()> {
        let ordinal = self
            .counts
            .lock()
            .expect("count lock")
            .get(&format!("{operation:?}"))
            .copied()
            .unwrap_or_default();
        if self
            .fail_after_success
            .lock()
            .expect("fault lock")
            .is_some_and(|mode| mode.matches(operation, ordinal))
        {
            return Err(io::Error::other(format!(
                "injected post-success {operation:?} failure at ordinal {ordinal}"
            )));
        }
        let Some(pause) = self
            .pauses_after_success
            .iter()
            .find(|pause| pause.operation == operation && pause.ordinal == ordinal)
        else {
            return Ok(());
        };
        fs::write(&pause.ready, path.to_string_lossy().as_bytes())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !pause.release.exists() {
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out waiting for {}", pause.release.display()),
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(())
    }

    fn before_exact_unlink(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::BeforeExactUnlink, path, None)?;
        let mutation = {
            let mut mutation = self.exact_unlink_mutation.lock().expect("mutation lock");
            if mutation
                .as_ref()
                .is_some_and(|mutation| mutation.matches_trigger(path))
            {
                mutation.take()
            } else {
                None
            }
        };
        if let Some(mutation) = mutation {
            mutation.apply()?;
        }
        Ok(())
    }
}

#[cfg(test)]
impl FsOps for FaultFs {
    fn observe_transition(&self, key: super::runtime::TransitionKey) {
        self.observe_semantic_transition(key);
    }

    fn before_create_directory(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::CreateDirectory, path, None)
    }

    fn after_create_directory(&self, path: &Path) -> io::Result<()> {
        self.after_success(FsOperation::CreateDirectory, path)
    }

    fn before_open_coordination_file(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::OpenCoordinationFile, path, None)
    }

    fn before_inspect_metadata(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::InspectMetadata, path, None)
    }

    fn before_read_handle(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::ReadHandle, path, None)
    }

    #[cfg(test)]
    fn observe_regular_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
    ) -> io::Result<ExactFileObservation> {
        self.before(FsOperation::ObserveRegularFile, path, None)?;
        let observation = SystemFs.observe_regular_file(parent, name, path)?;
        self.after_success(FsOperation::ObserveRegularFile, path)?;
        Ok(observation)
    }

    fn observe_regular_file_bounded(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileObservation> {
        self.before(FsOperation::ObserveRegularFileBounded, path, None)?;
        let observation = SystemFs.observe_regular_file_bounded(parent, name, path, max_bytes)?;
        self.after_success(FsOperation::ObserveRegularFileBounded, path)?;
        Ok(observation)
    }

    fn observe_regular_file_metadata(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileMetadataObservation> {
        self.before(FsOperation::ObserveRegularFileMetadata, path, None)?;
        let observation = SystemFs.observe_regular_file_metadata(parent, name, path, max_bytes)?;
        self.after_success(FsOperation::ObserveRegularFileMetadata, path)?;
        Ok(observation)
    }

    fn observe_created_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        created: &mut CreatedFile,
        max_bytes: u64,
    ) -> io::Result<ExactFileObservation> {
        self.before(FsOperation::ObserveCreatedFileExact, path, None)?;
        let observation =
            SystemFs.observe_created_file_exact(parent, name, path, created, max_bytes)?;
        self.after_success(FsOperation::ObserveCreatedFileExact, path)?;
        Ok(observation)
    }

    fn read_regular_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileRead> {
        self.before(FsOperation::ReadRegularFileExact, path, None)?;
        let read = SystemFs.read_regular_file_exact(parent, name, path, max_bytes)?;
        self.after_success(FsOperation::ReadRegularFileExact, path)?;
        Ok(read)
    }

    fn read_regular_file_bytes_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileBytesRead> {
        self.before(FsOperation::ReadRegularFileExact, path, None)?;
        let read = SystemFs.read_regular_file_bytes_exact(parent, name, path, max_bytes)?;
        self.after_success(FsOperation::ReadRegularFileExact, path)?;
        Ok(read)
    }

    fn observe_directory(
        &self,
        endpoint: DirectoryEndpoint<'_>,
    ) -> io::Result<ExactDirectoryObservation> {
        self.before(FsOperation::ObserveDirectory, endpoint.path, None)?;
        let observation = SystemFs.observe_directory(endpoint)?;
        self.after_success(FsOperation::ObserveDirectory, endpoint.path)?;
        Ok(observation)
    }

    fn open_directory_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<ExactDirectoryHandle> {
        self.before(FsOperation::OpenDirectoryExact, path, None)?;
        let directory = SystemFs.open_directory_exact(parent, name, path, mode)?;
        self.after_success(FsOperation::OpenDirectoryExact, path)?;
        Ok(directory)
    }

    fn create_directory_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<ExactDirectoryHandle> {
        self.before(FsOperation::CreateDirectoryExact, path, None)?;
        let directory = SystemFs.create_directory_exact(parent, name, path, mode)?;
        self.after_success(FsOperation::CreateDirectoryExact, path)?;
        Ok(directory)
    }

    #[cfg(test)]
    fn inventory_directory_exact(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
    ) -> io::Result<ExactDirectoryInventory> {
        self.before(FsOperation::InventoryDirectoryExact, endpoint.path, None)?;
        let inventory = SystemFs.inventory_directory_exact(endpoint, expected)?;
        self.after_success(FsOperation::InventoryDirectoryExact, endpoint.path)?;
        Ok(inventory)
    }

    fn inventory_directory_exact_bounded(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
        max_entries: usize,
    ) -> io::Result<ExactDirectoryInventory> {
        self.before(
            FsOperation::InventoryDirectoryExactBounded,
            endpoint.path,
            None,
        )?;
        let inventory =
            SystemFs.inventory_directory_exact_bounded(endpoint, expected, max_entries)?;
        self.after_success(FsOperation::InventoryDirectoryExactBounded, endpoint.path)?;
        Ok(inventory)
    }

    fn create_new_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> ExclusiveCreateOutcome {
        if let Err(source) = self.before(FsOperation::CreateNewFile, path, None) {
            return ExclusiveCreateOutcome::NotCreated { source };
        }
        match create_new_file_impl(parent, name, path, mode, || {
            self.before(FsOperation::AcquireCreatedFileIdentity, path, None)
        }) {
            ExclusiveCreateOutcome::CreatedVerified { created } => {
                match self.after_success(FsOperation::CreateNewFile, path) {
                    Ok(()) => ExclusiveCreateOutcome::CreatedVerified { created },
                    Err(source) => ExclusiveCreateOutcome::CreatedUnverified { created, source },
                }
            }
            outcome => outcome,
        }
    }

    fn create_exclusive_copy(
        &self,
        source: HardLinkEndpoint<'_>,
        expected_source: &ExactFileObservation,
        destination: HardLinkEndpoint<'_>,
    ) -> ExclusiveFileCopyOutcome {
        if let Err(source) = self.before(
            FsOperation::CreateExclusiveCopy,
            source.path,
            Some(destination.path),
        ) {
            return ExclusiveFileCopyOutcome::NotCreated { source };
        }
        let outcome = create_exclusive_file_copy(self, source, expected_source, destination);
        let ExclusiveFileCopyOutcome::CreatedVerified { copy } = outcome else {
            return outcome;
        };
        match self.after_success(FsOperation::CreateExclusiveCopy, source.path) {
            Ok(()) => ExclusiveFileCopyOutcome::CreatedVerified { copy },
            Err(source) => ExclusiveFileCopyOutcome::CreatedUnverified {
                created: CreatedFile::verified(
                    copy.file,
                    ExactFileMetadataObservation {
                        identity: copy.copy.identity,
                        byte_len: copy.copy.byte_len,
                        mode: copy.copy.mode,
                        link_count: copy.copy.link_count,
                    },
                ),
                source,
            },
        }
    }

    #[cfg(windows)]
    fn open_file_for_cleanup(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
    ) -> io::Result<CreatedFile> {
        self.before(FsOperation::OpenCleanupFile, path, None)?;
        SystemFs.open_file_for_cleanup(parent, name, path)
    }

    #[cfg(windows)]
    fn open_candidate_owner(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
    ) -> io::Result<CreatedFile> {
        self.before(FsOperation::OpenCandidateOwner, path, None)?;
        SystemFs.open_candidate_owner(parent, name, path)
    }

    fn set_file_mode(&self, file: &File, path: &Path, mode: u32) -> io::Result<()> {
        self.before(FsOperation::SetFileMode, path, None)?;
        SystemFs.set_file_mode(file, path, mode)
    }

    fn set_preserved_file_mode(
        &self,
        file: &File,
        path: &Path,
        mode: PreservedFileMode,
    ) -> io::Result<()> {
        self.before(FsOperation::SetFileMode, path, None)?;
        SystemFs.set_preserved_file_mode(file, path, mode)
    }

    fn set_path_mode(&self, parent: &Dir, name: &Path, path: &Path, mode: u32) -> io::Result<()> {
        self.before(FsOperation::SetFileMode, path, None)?;
        SystemFs.set_path_mode(parent, name, path, mode)
    }

    fn set_directory_mode(&self, directory: &Dir, path: &Path, mode: u32) -> io::Result<()> {
        self.before(FsOperation::SetDirectoryMode, path, None)?;
        SystemFs.set_directory_mode(directory, path, mode)
    }

    fn write_handle(&self, file: &mut File, path: &Path, content: &[u8]) -> io::Result<()> {
        self.before(FsOperation::WriteHandle, path, None)?;
        SystemFs.write_handle(file, path, content)
    }

    fn sync_handle(&self, file: &File, path: &Path) -> io::Result<()> {
        self.before(FsOperation::SyncHandle, path, None)?;
        SystemFs.sync_handle(file, path)?;
        self.after_success(FsOperation::SyncHandle, path)
    }

    fn flush_file(&self, file: &File, path: &Path) -> io::Result<()> {
        self.before(FsOperation::FlushFile, path, None)?;
        SystemFs.flush_file(file, path)?;
        self.after_success(FsOperation::FlushFile, path)
    }

    fn sync_directory(&self, directory: &Dir, path: &Path) -> io::Result<()> {
        self.before(FsOperation::SyncDirectory, path, None)?;
        SystemFs.sync_directory(directory, path)
    }

    fn sync_parent(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
        kind: ParentSyncKind,
    ) -> io::Result<()> {
        let operation = match kind {
            ParentSyncKind::Target => FsOperation::SyncTargetParent,
            ParentSyncKind::Journal => FsOperation::SyncJournalParent,
        };
        self.before(operation, endpoint.path, None)?;
        SystemFs.sync_parent(endpoint, expected, kind)?;
        self.after_success(operation, endpoint.path)
    }

    fn try_lock(&self, file: &File, path: &Path) -> Result<(), std::fs::TryLockError> {
        self.before(FsOperation::TryLock, path, None)
            .map_err(std::fs::TryLockError::Error)?;
        SystemFs.try_lock(file, path)?;
        self.after_success(FsOperation::TryLock, path)
            .map_err(std::fs::TryLockError::Error)
    }

    fn hard_link(
        &self,
        pinned_directories: &[Dir],
        from: HardLinkEndpoint<'_>,
        to: HardLinkEndpoint<'_>,
    ) -> io::Result<()> {
        self.before(FsOperation::HardLink, from.path, Some(to.path))?;
        SystemFs.hard_link(pinned_directories, from, to)?;
        self.after_success(FsOperation::HardLink, from.path)
    }

    fn publish_absent(
        &self,
        staged: HardLinkEndpoint<'_>,
        expected_stage: &ExactFileObservation,
        target: HardLinkEndpoint<'_>,
    ) -> io::Result<()> {
        self.before(FsOperation::PublishAbsent, staged.path, Some(target.path))?;
        SystemFs.publish_absent(staged, expected_stage, target)?;
        self.after_success(FsOperation::PublishAbsent, staged.path)
    }

    fn rename_directory_noreplace(
        &self,
        candidate_parent: &Dir,
        candidate_name: &Path,
        candidate_path: &Path,
        target_parent: &Dir,
        target_name: &Path,
        target_path: &Path,
    ) -> io::Result<()> {
        self.before(
            FsOperation::RenameDirectoryNoReplace,
            candidate_path,
            Some(target_path),
        )?;
        SystemFs.rename_directory_noreplace(
            candidate_parent,
            candidate_name,
            candidate_path,
            target_parent,
            target_name,
            target_path,
        )?;
        self.after_success(FsOperation::RenameDirectoryNoReplace, candidate_path)
    }

    fn relocate_noreplace(
        &self,
        owner_parent: &Dir,
        owner_name: &Path,
        owner_path: &Path,
        destination_parent: &Dir,
        destination_name: &Path,
        destination_path: &Path,
        expected_source: &ExactRelocationSource,
    ) -> Result<(), NoReplaceRelocationError> {
        self.before(
            FsOperation::RelocateNoReplace,
            owner_path,
            Some(destination_path),
        )
        .map_err(NoReplaceRelocationError::Io)?;
        SystemFs.relocate_noreplace(
            owner_parent,
            owner_name,
            owner_path,
            destination_parent,
            destination_name,
            destination_path,
            expected_source,
        )?;
        self.after_success(FsOperation::RelocateNoReplace, owner_path)
            .map_err(NoReplaceRelocationError::Io)
    }

    fn probe_noreplace_relocation(
        &self,
        parent: &Dir,
        path: &Path,
    ) -> Result<(), NoReplaceRelocationError> {
        self.before(FsOperation::ProbeRelocateNoReplace, path, None)
            .map_err(NoReplaceRelocationError::Io)?;
        SystemFs.probe_noreplace_relocation(parent, path)?;
        self.after_success(FsOperation::ProbeRelocateNoReplace, path)
            .map_err(NoReplaceRelocationError::Io)
    }

    fn remove_file(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()> {
        self.before(FsOperation::RemoveFile, path, None)?;
        SystemFs.remove_file(parent, name, path)
    }

    fn remove_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        expected: &ExactFileObservation,
    ) -> Result<(), ExactRemovalError> {
        self.before(FsOperation::RemoveFileExact, path, None)
            .map_err(ExactRemovalError::not_mutated)?;
        remove_exact_file(parent, name, path, expected, || {
            self.before_exact_unlink(path)
        })?;
        self.after_success(FsOperation::RemoveFileExact, path)
            .map_err(|source| ExactRemovalError::mutated(ExactRemovalPostName::Absent, source))
    }

    fn remove_file_metadata_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        expected: &ExactFileMetadataObservation,
    ) -> Result<(), ExactRemovalError> {
        self.before(FsOperation::RemoveFileMetadataExact, path, None)
            .map_err(ExactRemovalError::not_mutated)?;
        remove_exact_file_metadata(parent, name, path, expected, || {
            self.before_exact_unlink(path)
        })?;
        self.after_success(FsOperation::RemoveFileMetadataExact, path)
            .map_err(|source| ExactRemovalError::mutated(ExactRemovalPostName::Absent, source))
    }

    #[cfg(windows)]
    fn remove_file_by_handle(&self, file: File, path: &Path) -> Result<(), HandleDeleteError> {
        if let Err(source) = self.before(FsOperation::RemoveFileByHandle, path, None) {
            return Err(HandleDeleteError { file, source });
        }
        SystemFs.remove_file_by_handle(file, path)
    }

    fn remove_dir(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()> {
        self.before(FsOperation::RemoveDirectory, path, None)?;
        SystemFs.remove_dir(parent, name, path)
    }

    fn remove_empty_directory_exact(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
    ) -> Result<(), ExactRemovalError> {
        self.before(FsOperation::RemoveDirectoryExact, endpoint.path, None)
            .map_err(ExactRemovalError::not_mutated)?;
        remove_empty_directory_exact_impl(endpoint, expected, || {
            self.before_exact_unlink(endpoint.path)
        })?;
        self.after_success(FsOperation::RemoveDirectoryExact, endpoint.path)
            .map_err(|source| ExactRemovalError::mutated(ExactRemovalPostName::Absent, source))
    }

    fn before_mutation_rebind(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::BeforeMutationRebind, path, None)?;
        let mutation = {
            let mut mutation = self.mutation_rebind_mutation.lock().expect("mutation lock");
            if mutation
                .as_ref()
                .is_some_and(|mutation| mutation.matches_trigger(path))
            {
                mutation.take()
            } else {
                None
            }
        };
        if let Some(mutation) = mutation {
            mutation.apply()?;
        }
        Ok(())
    }

    fn before_final_revalidation(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::BeforeFinalRevalidation, path, None)?;
        let mutation = {
            let mut mutation = self
                .final_revalidation_mutation
                .lock()
                .expect("mutation lock");
            if mutation
                .as_ref()
                .is_some_and(|mutation| mutation.matches_trigger(path))
            {
                mutation.take()
            } else {
                None
            }
        };
        if let Some(mutation) = mutation {
            mutation.apply()?;
        }
        Ok(())
    }

    fn after_final_revalidation(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::AfterFinalRevalidation, path, None)?;
        let mutation = {
            let mut mutation = self
                .post_revalidation_mutation
                .lock()
                .expect("mutation lock");
            if mutation
                .as_ref()
                .is_some_and(|mutation| mutation.matches_trigger(path))
            {
                mutation.take()
            } else {
                None
            }
        };
        if let Some(mutation) = mutation {
            mutation.apply()?;
        }
        Ok(())
    }

    fn rename(
        &self,
        from_parent: &Dir,
        from_name: &Path,
        from: &Path,
        to_parent: &Dir,
        to_name: &Path,
        to: &Path,
    ) -> io::Result<()> {
        self.before(FsOperation::Rename, from, Some(to))?;
        SystemFs.rename(from_parent, from_name, from, to_parent, to_name, to)
    }

    fn replace_existing(
        &self,
        staged: HardLinkEndpoint<'_>,
        expected_stage: &ExactFileObservation,
        target: HardLinkEndpoint<'_>,
        expected_target: &ExactFileObservation,
    ) -> io::Result<()> {
        self.before(FsOperation::ReplaceExisting, staged.path, Some(target.path))?;
        SystemFs.replace_existing(staged, expected_stage, target, expected_target)?;
        self.after_success(FsOperation::ReplaceExisting, staged.path)
    }

    fn rename_journal(
        &self,
        from_parent: &Dir,
        from_name: &Path,
        from: &Path,
        to_parent: &Dir,
        to_name: &Path,
        to: &Path,
    ) -> io::Result<()> {
        self.before(FsOperation::RenameJournal, from, Some(to))?;
        SystemFs.rename_journal(from_parent, from_name, from, to_parent, to_name, to)
    }
}

#[cfg(all(test, unix))]
mod exact_state_tests {
    use std::path::{Path, PathBuf};

    use cap_std::{ambient_authority, fs::Dir};
    use tempfile::TempDir;

    use super::{
        DirectoryEndpoint, ExactDirectoryEntryKind, ExactIdentitySupport, ExclusiveCreateOutcome,
        ExclusiveFileCopyOutcome, FaultFs, FsOperation, FsOps, HardLinkEndpoint, ParentSyncKind,
        SystemFs, exact_identity_support,
    };

    struct Fixture {
        _temporary: TempDir,
        root: Dir,
        files: Dir,
        files_path: PathBuf,
    }

    #[test]
    fn mutation_rebind_hook_is_semantically_faultable() {
        let fixture = Fixture::new();
        fixture.write("target", b"planned bytes\n");
        let target_path = fixture.path("target");
        let fault = super::FaultFs::mutate_before_mutation_rebind(
            target_path.clone(),
            b"concurrent bytes\n".to_vec(),
        );

        fault
            .before_mutation_rebind(&target_path)
            .expect("run mutation rebind hook");

        assert_eq!(
            std::fs::read(&target_path).expect("read mutated target"),
            b"concurrent bytes\n"
        );
        assert_eq!(
            fault.events()[0].operation,
            FsOperation::BeforeMutationRebind
        );
    }

    #[test]
    fn identity_acquisition_failure_retains_the_live_exclusive_create_capability() {
        let fixture = Fixture::new();
        let path = fixture.path("identity-failure");
        let fault = super::FaultFs::fail_nth(FsOperation::AcquireCreatedFileIdentity, 1);

        let ExclusiveCreateOutcome::CreatedUnverified {
            mut created,
            source,
        } = fault.create_new_file(&fixture.files, Path::new("identity-failure"), &path, 0o600)
        else {
            panic!("post-open identity failure must retain an unverified capability");
        };

        assert!(source.to_string().contains("AcquireCreatedFileIdentity"));
        assert!(created.exact_metadata().is_none());
        let observed = fault
            .observe_created_file_exact(
                &fixture.files,
                Path::new("identity-failure"),
                &path,
                &mut created,
                0,
            )
            .expect("bind the retained empty file through its live handle");
        assert_eq!(observed.identity, created.identity());
        assert_eq!(observed.byte_len, 0);
        assert_eq!(observed.link_count, Some(1));
        assert_eq!(std::fs::read(path).expect("read residual"), b"");
    }

    #[test]
    fn post_success_failure_retains_the_verified_exclusive_create_metadata() {
        let fixture = Fixture::new();
        let path = fixture.path("post-success-failure");
        let fault = super::FaultFs::fail_after_success_nth(FsOperation::CreateNewFile, 1);

        let ExclusiveCreateOutcome::CreatedUnverified {
            mut created,
            source,
        } = fault.create_new_file(
            &fixture.files,
            Path::new("post-success-failure"),
            &path,
            0o600,
        )
        else {
            panic!("post-success failure must retain the created capability");
        };

        assert!(source.to_string().contains("post-success CreateNewFile"));
        let metadata = created
            .exact_metadata()
            .expect("metadata acquired before the injected post-success error");
        let observed = fault
            .observe_created_file_exact(
                &fixture.files,
                Path::new("post-success-failure"),
                &path,
                &mut created,
                0,
            )
            .expect("rebind the retained verified handle");
        assert_eq!(observed.identity, metadata.identity);
        assert_eq!(observed.byte_len, metadata.byte_len);
        assert_eq!(observed.mode, metadata.mode);
        assert_eq!(observed.link_count, metadata.link_count);
        assert_eq!(std::fs::read(path).expect("read residual"), b"");
    }

    #[test]
    fn created_capability_pins_identity_across_controlled_metadata_changes() {
        let fixture = Fixture::new();
        let path = fixture.path("controlled-metadata-change");
        let ExclusiveCreateOutcome::CreatedVerified { mut created } = super::SystemFs
            .create_new_file(
                &fixture.files,
                Path::new("controlled-metadata-change"),
                &path,
                0o600,
            )
        else {
            panic!("exclusive create must retain a verified capability");
        };
        let identity = created.identity();

        super::SystemFs
            .write_handle(&mut created.file, &path, b"owned bytes\n")
            .expect("write through retained capability");
        super::SystemFs
            .flush_file(&created.file, &path)
            .expect("flush retained capability");
        let observed = super::SystemFs
            .observe_created_file_exact(
                &fixture.files,
                Path::new("controlled-metadata-change"),
                &path,
                &mut created,
                12,
            )
            .expect("metadata evolution must not look like identity substitution");

        assert_eq!(observed.identity, identity);
        assert_eq!(observed.byte_len, 12);
        assert_eq!(
            std::fs::read(path).expect("read created file"),
            b"owned bytes\n"
        );
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let files_path = temporary.path().join("files");
            std::fs::create_dir(&files_path).expect("create files directory");
            let root = Dir::open_ambient_dir(temporary.path(), ambient_authority())
                .expect("open temporary root");
            let files = root.open_dir("files").expect("open files directory");
            Self {
                _temporary: temporary,
                root,
                files,
                files_path,
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.files_path.join(name)
        }

        fn write(&self, name: &str, content: &[u8]) {
            std::fs::write(self.path(name), content).expect("write fixture file");
        }

        fn endpoint<'a>(&'a self, name: &'a str, path: &'a Path) -> HardLinkEndpoint<'a> {
            HardLinkEndpoint::new(&self.files, Path::new(name), path)
        }
    }

    #[test]
    fn exclusive_copy_retains_live_owner_after_post_success_failure() {
        let fixture = Fixture::new();
        fixture.write("source", b"retained copy bytes\n");
        let source_path = fixture.path("source");
        let source = SystemFs
            .observe_regular_file(&fixture.files, Path::new("source"), &source_path)
            .expect("observe source");
        let copy_path = fixture.path("copy-post-success");
        let fault = FaultFs::fail_after_success_nth(FsOperation::CreateExclusiveCopy, 1);

        let ExclusiveFileCopyOutcome::CreatedUnverified {
            mut created,
            source: failure,
        } = fault.create_exclusive_copy(
            fixture.endpoint("source", &source_path),
            &source,
            fixture.endpoint("copy-post-success", &copy_path),
        )
        else {
            panic!("post-success copy failure must retain the created owner capability");
        };

        assert!(
            failure
                .to_string()
                .contains("post-success CreateExclusiveCopy")
        );
        let observed = SystemFs
            .observe_created_file_exact(
                &fixture.files,
                Path::new("copy-post-success"),
                &copy_path,
                &mut created,
                source.byte_len,
            )
            .expect("rebind the fully populated residual through its live owner handle");
        assert_eq!(observed.content_hash, source.content_hash);
        assert_eq!(observed.byte_len, source.byte_len);
        assert_eq!(observed.mode, source.mode);
    }

    #[test]
    fn exclusive_copy_retains_live_owner_after_population_failure() {
        let fixture = Fixture::new();
        fixture.write("source", b"partial copy bytes\n");
        let source_path = fixture.path("source");
        let source = SystemFs
            .observe_regular_file(&fixture.files, Path::new("source"), &source_path)
            .expect("observe source");
        let copy_path = fixture.path("copy-partial");
        let fault = FaultFs::fail_nth(FsOperation::WriteHandle, 1);

        let ExclusiveFileCopyOutcome::CreatedUnverified {
            mut created,
            source: failure,
        } = fault.create_exclusive_copy(
            fixture.endpoint("source", &source_path),
            &source,
            fixture.endpoint("copy-partial", &copy_path),
        )
        else {
            panic!("copy population failure must retain the created owner capability");
        };

        assert!(failure.to_string().contains("injected WriteHandle"));
        let observed = SystemFs
            .observe_created_file_exact(
                &fixture.files,
                Path::new("copy-partial"),
                &copy_path,
                &mut created,
                source.byte_len,
            )
            .expect("rebind the partial residual through its live owner handle");
        assert!(observed.byte_len < source.byte_len);
        assert_eq!(observed.link_count, Some(1));
    }

    #[test]
    fn exact_observation_copy_publish_sync_and_cleanup_preserve_recorded_state() {
        assert_eq!(exact_identity_support(), ExactIdentitySupport::Complete);
        let fixture = Fixture::new();
        fixture.write("source", b"independent recovery bytes\n");

        let directory = SystemFs
            .observe_directory(DirectoryEndpoint::new(
                &fixture.root,
                Path::new("files"),
                &fixture.files,
                &fixture.files_path,
            ))
            .expect("observe pinned directory");
        let source_path = fixture.path("source");
        let source = SystemFs
            .observe_regular_file(&fixture.files, Path::new("source"), &source_path)
            .expect("observe source");
        assert_eq!(
            source.content_hash,
            crate::hash_content_bytes(b"independent recovery bytes\n")
        );
        assert_eq!(source.byte_len, 27);
        assert_eq!(source.link_count, Some(1));

        let copy_path = fixture.path("copy");
        let copy = SystemFs
            .create_exclusive_copy(
                fixture.endpoint("source", &source_path),
                &source,
                fixture.endpoint("copy", &copy_path),
            )
            .into_verified()
            .expect("create independent copy");
        assert_eq!(copy.source, source);
        assert_ne!(copy.copy.identity, source.identity);
        assert_eq!(copy.copy.content_hash, source.content_hash);
        assert_eq!(copy.copy.mode, source.mode);
        assert_eq!(copy.copy.link_count, Some(1));
        SystemFs
            .flush_file(&copy.file, &copy_path)
            .expect("durably flush copy");
        assert_eq!(
            SystemFs
                .observe_regular_file(&fixture.files, Path::new("copy"), &copy_path)
                .expect("reobserve copy"),
            copy.copy
        );
        let directory_after_copy = SystemFs
            .observe_directory(DirectoryEndpoint::new(
                &fixture.root,
                Path::new("files"),
                &fixture.files,
                &fixture.files_path,
            ))
            .expect("reobserve target parent after copy creation");
        assert_eq!(directory_after_copy.identity, directory.identity);
        SystemFs
            .sync_parent(
                DirectoryEndpoint::new(
                    &fixture.root,
                    Path::new("files"),
                    &fixture.files,
                    &fixture.files_path,
                ),
                &directory_after_copy,
                ParentSyncKind::Target,
            )
            .expect("sync target parent");

        let published_path = fixture.path("published");
        SystemFs
            .publish_absent(
                fixture.endpoint("copy", &copy_path),
                &copy.copy,
                fixture.endpoint("published", &published_path),
            )
            .expect("publish absent target without clobbering");
        let linked_copy = SystemFs
            .observe_regular_file(&fixture.files, Path::new("copy"), &copy_path)
            .expect("observe linked copy");
        let published = SystemFs
            .observe_regular_file(&fixture.files, Path::new("published"), &published_path)
            .expect("observe published target");
        assert_eq!(linked_copy.identity, published.identity);
        assert_eq!(linked_copy.link_count, Some(2));
        SystemFs
            .remove_file_exact(&fixture.files, Path::new("copy"), &copy_path, &linked_copy)
            .expect("remove only the recorded copy name");
        assert_eq!(
            std::fs::read(published_path).expect("read published target"),
            b"independent recovery bytes\n"
        );
    }

    #[test]
    fn absent_publication_is_atomic_no_clobber_and_semantically_faultable() {
        let fixture = Fixture::new();
        fixture.write("stage", b"staged\n");
        fixture.write("target", b"application edit\n");
        let stage_path = fixture.path("stage");
        let target_path = fixture.path("target");
        let stage = SystemFs
            .observe_regular_file(&fixture.files, Path::new("stage"), &stage_path)
            .expect("observe stage");

        let error = SystemFs
            .publish_absent(
                fixture.endpoint("stage", &stage_path),
                &stage,
                fixture.endpoint("target", &target_path),
            )
            .expect_err("existing target must win");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&target_path).expect("read target"),
            b"application edit\n"
        );

        let fault = super::FaultFs::fail_nth(FsOperation::PublishAbsent, 1);
        let injected_target_path = fixture.path("injected-target");
        let error = fault
            .publish_absent(
                fixture.endpoint("stage", &stage_path),
                &stage,
                fixture.endpoint("injected-target", &injected_target_path),
            )
            .expect_err("semantic publication fault");
        assert!(error.to_string().contains("PublishAbsent"));
        assert!(!injected_target_path.exists());
        assert_eq!(fault.events()[0].operation, FsOperation::PublishAbsent);
    }

    #[test]
    fn replacement_and_exact_removal_refuse_stale_recorded_states() {
        let fixture = Fixture::new();
        fixture.write("stage", b"desired\n");
        fixture.write("target", b"previous\n");
        let stage_path = fixture.path("stage");
        let target_path = fixture.path("target");
        let stage = SystemFs
            .observe_regular_file(&fixture.files, Path::new("stage"), &stage_path)
            .expect("observe stage");
        let target = SystemFs
            .observe_regular_file(&fixture.files, Path::new("target"), &target_path)
            .expect("observe target");
        SystemFs
            .replace_existing(
                fixture.endpoint("stage", &stage_path),
                &stage,
                fixture.endpoint("target", &target_path),
                &target,
            )
            .expect("replace existing target");
        let installed = SystemFs
            .observe_regular_file(&fixture.files, Path::new("target"), &target_path)
            .expect("observe installed target");
        assert_eq!(installed.identity, stage.identity);
        assert_eq!(
            installed.content_hash,
            crate::hash_content_bytes(b"desired\n")
        );
        assert!(!stage_path.exists());

        fixture.write("cleanup", b"recorded\n");
        let cleanup_path = fixture.path("cleanup");
        let cleanup = SystemFs
            .observe_regular_file(&fixture.files, Path::new("cleanup"), &cleanup_path)
            .expect("observe cleanup object");
        std::fs::remove_file(&cleanup_path).expect("remove recorded object");
        fixture.write("cleanup", b"substitute\n");
        let error = SystemFs
            .remove_file_exact(
                &fixture.files,
                Path::new("cleanup"),
                &cleanup_path,
                &cleanup,
            )
            .expect_err("stale exact state must not remove substitute");
        assert_eq!(error.source_error().kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read(cleanup_path).expect("read substitute"),
            b"substitute\n"
        );
    }

    #[test]
    fn bounded_exact_read_returns_bound_bytes_and_is_semantically_faultable() {
        let fixture = Fixture::new();
        fixture.write("record", b"bounded journal bytes\n");
        let record_path = fixture.path("record");

        let read = SystemFs
            .read_regular_file_exact(&fixture.files, Path::new("record"), &record_path, 64)
            .expect("bounded exact read");
        assert_eq!(read.bytes, b"bounded journal bytes\n");
        assert_eq!(read.observation.byte_len, read.bytes.len() as u64);
        assert_eq!(
            read.observation.content_hash,
            crate::hash_content_bytes(&read.bytes)
        );

        let error = SystemFs
            .read_regular_file_exact(&fixture.files, Path::new("record"), &record_path, 4)
            .expect_err("oversized exact read");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let alias_path = fixture.path("record-alias");
        std::os::unix::fs::symlink("record", &alias_path).expect("create record symlink");
        let error = SystemFs
            .read_regular_file_exact(&fixture.files, Path::new("record-alias"), &alias_path, 64)
            .expect_err("no-follow exact read rejects symlinks");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);

        let fault = super::FaultFs::fail_nth(FsOperation::ReadRegularFileExact, 1);
        let error = fault
            .read_regular_file_exact(&fixture.files, Path::new("record"), &record_path, 64)
            .expect_err("semantic exact-read fault");
        assert!(error.to_string().contains("ReadRegularFileExact"));
        assert_eq!(
            fault.events()[0].operation,
            FsOperation::ReadRegularFileExact
        );
    }

    #[test]
    fn exact_directory_create_open_inventory_and_empty_removal_bind_identity_and_mode() {
        let fixture = Fixture::new();
        let workspace_path = fixture.path("workspace");
        let created = SystemFs
            .create_directory_exact(
                &fixture.files,
                Path::new("workspace"),
                &workspace_path,
                0o700,
            )
            .expect("exclusive exact directory creation");
        assert_eq!(created.observation.mode.posix_mode, Some(0o700));
        let opened = SystemFs
            .open_directory_exact(
                &fixture.files,
                Path::new("workspace"),
                &workspace_path,
                0o700,
            )
            .expect("exact directory opening");
        assert_eq!(opened.observation, created.observation);
        let error = SystemFs
            .create_directory_exact(
                &fixture.files,
                Path::new("workspace"),
                &workspace_path,
                0o700,
            )
            .expect_err("exact directory creation is exclusive");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        let endpoint = DirectoryEndpoint::new(
            &fixture.files,
            Path::new("workspace"),
            &created.directory,
            &workspace_path,
        );
        let empty = SystemFs
            .inventory_directory_exact(endpoint, &created.observation)
            .expect("empty exact inventory");
        assert!(empty.entries.is_empty());

        std::fs::write(workspace_path.join("record.json"), b"{}\n").expect("write inventory child");
        let after_child = SystemFs
            .observe_directory(endpoint)
            .expect("observe workspace after child creation");
        let inventory = SystemFs
            .inventory_directory_exact(endpoint, &after_child)
            .expect("exact child inventory");
        assert_eq!(inventory.entries.len(), 1);
        assert_eq!(inventory.entries[0].name, "record.json");
        assert_eq!(
            inventory.entries[0].kind,
            ExactDirectoryEntryKind::RegularFile
        );
        let error = SystemFs
            .remove_empty_directory_exact(endpoint, &after_child)
            .expect_err("nonempty exact directory is preserved");
        assert_eq!(
            error.source_error().kind(),
            std::io::ErrorKind::DirectoryNotEmpty
        );

        std::fs::remove_file(workspace_path.join("record.json")).expect("remove inventory child");
        let empty_again = SystemFs
            .observe_directory(endpoint)
            .expect("observe empty workspace");
        let boundary_fault = super::FaultFs::fail_nth(FsOperation::BeforeExactUnlink, 1);
        boundary_fault
            .remove_empty_directory_exact(endpoint, &empty_again)
            .expect_err("semantic exact-directory boundary fault");
        assert!(workspace_path.is_dir());
        SystemFs
            .remove_empty_directory_exact(endpoint, &empty_again)
            .expect("remove exact empty workspace");
        assert!(!workspace_path.exists());

        let fault = super::FaultFs::fail_nth(FsOperation::CreateDirectoryExact, 1);
        let fault_path = fixture.path("fault-workspace");
        fault
            .create_directory_exact(
                &fixture.files,
                Path::new("fault-workspace"),
                &fault_path,
                0o700,
            )
            .expect_err("semantic exact-directory creation fault");
        assert!(!fault_path.exists());
    }

    #[test]
    fn exact_unlink_substitution_hook_preserves_the_replacement_name() {
        let fixture = Fixture::new();
        fixture.write("cleanup", b"recorded owner\n");
        let cleanup_path = fixture.path("cleanup");
        let moved_path = fixture.path("moved-owner");
        let expected = SystemFs
            .observe_regular_file(&fixture.files, Path::new("cleanup"), &cleanup_path)
            .expect("observe cleanup owner");
        let fault = super::FaultFs::substitute_before_exact_unlink(
            cleanup_path.clone(),
            moved_path.clone(),
            b"substitute\n".to_vec(),
        );

        let error = fault
            .remove_file_exact(
                &fixture.files,
                Path::new("cleanup"),
                &cleanup_path,
                &expected,
            )
            .expect_err("boundary substitution must be rejected");
        assert_eq!(error.source_error().kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read(&cleanup_path).expect("read preserved substitute"),
            b"substitute\n"
        );
        assert_eq!(
            std::fs::read(moved_path).expect("read detached original"),
            b"recorded owner\n"
        );
        assert!(
            fault
                .events()
                .iter()
                .any(|event| event.operation == FsOperation::BeforeExactUnlink)
        );
    }
}
