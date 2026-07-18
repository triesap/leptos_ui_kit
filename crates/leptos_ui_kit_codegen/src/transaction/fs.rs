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

pub(crate) struct CreatedFile {
    pub file: File,
    pub identity: (u64, u64),
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

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) struct DirectoryEndpoint<'a> {
    pub parent: &'a Dir,
    pub name: &'a Path,
    pub directory: &'a Dir,
    pub path: &'a Path,
}

#[allow(dead_code)]
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
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactIdentitySupport {
    Complete,
    Unsupported,
}

#[allow(dead_code)]
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

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactFileObservation {
    pub identity: (u64, u64),
    pub byte_len: u64,
    pub content_hash: String,
    pub mode: PreservedFileMode,
    pub link_count: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactFileRead {
    pub bytes: Vec<u8>,
    pub observation: ExactFileObservation,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExactDirectoryObservation {
    pub identity: (u64, u64),
    pub mode: PreservedFileMode,
    pub link_count: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct ExactDirectoryHandle {
    pub directory: Dir,
    pub observation: ExactDirectoryObservation,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactDirectoryEntryKind {
    RegularFile,
    Directory,
    Symlink,
    ReparsePoint,
    Other,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactDirectoryEntry {
    pub name: OsString,
    pub kind: ExactDirectoryEntryKind,
    pub identity: (u64, u64),
    pub byte_len: u64,
    pub mode: PreservedFileMode,
    pub link_count: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactDirectoryInventory {
    pub directory: ExactDirectoryObservation,
    pub entries: Vec<ExactDirectoryEntry>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct ExclusiveFileCopy {
    pub file: File,
    pub source: ExactFileObservation,
    pub copy: ExactFileObservation,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParentSyncKind {
    Target,
    Journal,
}

/// The filesystem disposition of one immutable no-clobber publication.
///
/// Store/recovery code must not collapse these variants into a generic error:
/// once the published hard link can be visible, rollback safety depends on
/// whether its parent-directory durability barrier completed. Observations are
/// the last exact states proven at the corresponding protocol boundary. A
/// `DurableWithPartialResidual` is conservative. Its observations describe
/// the last exact *linked* world before cleanup, not necessarily current path
/// state. When `partial_absent_in_process` is true, the unlink completed in
/// this process but its durability/final state was not proven; recovery must
/// therefore accept and classify both the pre-unlink two-alias world and the
/// post-unlink single-target world rather than trusting either link count as
/// current after a crash.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum ImmutablePublicationOutcome {
    NotPublished {
        partial: Option<ExactFileObservation>,
        source: io::Error,
    },
    VisibleDurabilityUnknown {
        partial: ExactFileObservation,
        published: Option<ExactFileObservation>,
        source: io::Error,
    },
    DurableWithPartialResidual {
        last_linked_published: ExactFileObservation,
        last_linked_partial: ExactFileObservation,
        partial_absent_in_process: bool,
        source: io::Error,
    },
    Durable {
        published: ExactFileObservation,
    },
}

/// The filesystem disposition of atomically publishing one fully prepared
/// private directory at an expected-absent logical name.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum DirectoryPublicationOutcome {
    NotPublished {
        candidate: Option<ExactDirectoryInventory>,
        source: io::Error,
    },
    VisibleDurabilityUnknown {
        candidate: ExactDirectoryInventory,
        published: Option<ExactDirectoryInventory>,
        source: io::Error,
    },
    Durable {
        published: ExactDirectoryInventory,
    },
}

#[cfg(windows)]
pub(crate) struct HandleDeleteError {
    pub file: File,
    pub source: io::Error,
}

pub(crate) trait FsOps: fmt::Debug + Send + Sync + UnwindSafe + RefUnwindSafe {
    fn before_create_directory(&self, path: &Path) -> io::Result<()>;
    fn after_create_directory(&self, path: &Path) -> io::Result<()>;
    fn before_open_coordination_file(&self, path: &Path) -> io::Result<()>;
    fn before_inspect_metadata(&self, path: &Path) -> io::Result<()>;
    fn before_read_handle(&self, path: &Path) -> io::Result<()>;
    #[allow(dead_code)]
    fn observe_regular_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
    ) -> io::Result<ExactFileObservation>;
    #[allow(dead_code)]
    fn read_regular_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        max_bytes: u64,
    ) -> io::Result<ExactFileRead>;
    #[allow(dead_code)]
    fn observe_directory(
        &self,
        endpoint: DirectoryEndpoint<'_>,
    ) -> io::Result<ExactDirectoryObservation>;
    #[allow(dead_code)]
    fn open_directory_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<ExactDirectoryHandle>;
    #[allow(dead_code)]
    fn create_directory_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<ExactDirectoryHandle>;
    #[allow(dead_code)]
    fn inventory_directory_exact(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
    ) -> io::Result<ExactDirectoryInventory>;
    fn create_new_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<CreatedFile>;
    #[allow(dead_code)]
    fn create_exclusive_copy(
        &self,
        source: HardLinkEndpoint<'_>,
        expected_source: &ExactFileObservation,
        destination: HardLinkEndpoint<'_>,
    ) -> io::Result<ExclusiveFileCopy>;
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
    fn set_path_mode(&self, parent: &Dir, name: &Path, path: &Path, mode: u32) -> io::Result<()>;
    fn set_directory_mode(&self, directory: &Dir, path: &Path, mode: u32) -> io::Result<()>;
    fn write_handle(&self, file: &mut File, path: &Path, content: &[u8]) -> io::Result<()>;
    fn sync_handle(&self, file: &File, path: &Path) -> io::Result<()>;
    #[allow(dead_code)]
    fn flush_file(&self, file: &File, path: &Path) -> io::Result<()>;
    fn sync_directory(&self, directory: &Dir, path: &Path) -> io::Result<()>;
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    fn publish_absent(
        &self,
        staged: HardLinkEndpoint<'_>,
        expected_stage: &ExactFileObservation,
        target: HardLinkEndpoint<'_>,
    ) -> io::Result<()>;
    #[allow(dead_code)]
    fn publish_immutable(
        &self,
        partial: HardLinkEndpoint<'_>,
        expected_partial: &ExactFileObservation,
        target: HardLinkEndpoint<'_>,
        target_parent: DirectoryEndpoint<'_>,
        expected_target_parent: &ExactDirectoryObservation,
    ) -> ImmutablePublicationOutcome;
    #[allow(dead_code)]
    fn rename_directory_noreplace(
        &self,
        candidate_parent: &Dir,
        candidate_name: &Path,
        candidate_path: &Path,
        target_parent: &Dir,
        target_name: &Path,
        target_path: &Path,
    ) -> io::Result<()>;
    #[allow(dead_code)]
    fn publish_directory_absent(
        &self,
        candidate: DirectoryEndpoint<'_>,
        expected_candidate: &ExactDirectoryInventory,
        target_parent: DirectoryEndpoint<'_>,
        expected_target_parent: &ExactDirectoryObservation,
        target_name: &Path,
        target_path: &Path,
    ) -> DirectoryPublicationOutcome;
    fn remove_file(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()>;
    #[allow(dead_code)]
    fn remove_file_exact(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        expected: &ExactFileObservation,
    ) -> io::Result<()>;
    #[cfg(windows)]
    fn remove_file_by_handle(&self, file: File, path: &Path) -> Result<(), HandleDeleteError>;
    fn remove_dir(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()>;
    #[allow(dead_code)]
    fn remove_empty_directory_exact(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
    ) -> io::Result<()>;
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
    #[allow(dead_code)]
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

    fn observe_regular_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
    ) -> io::Result<ExactFileObservation> {
        observe_regular_file_exact(parent, name, path)
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

    fn inventory_directory_exact(
        &self,
        endpoint: DirectoryEndpoint<'_>,
        expected: &ExactDirectoryObservation,
    ) -> io::Result<ExactDirectoryInventory> {
        inventory_directory_exact_impl(endpoint, expected)
    }

    fn create_new_file(
        &self,
        parent: &Dir,
        name: &Path,
        _path: &Path,
        mode: u32,
    ) -> io::Result<CreatedFile> {
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).write(true);
        options.follow(FollowSymlinks::No);
        options.nonblock(true);
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            use windows_sys::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{
                    DELETE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
                },
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
        let file = parent.open_with(name, &options)?;
        match opened_regular_file_identity(&file) {
            Ok(identity) => Ok(CreatedFile { file, identity }),
            Err(error) => {
                #[cfg(windows)]
                {
                    use fs_at::os::windows::FileExt;

                    let _ = file.into_std().delete_by_handle();
                }
                #[cfg(not(windows))]
                {
                    let _ = parent.remove_file(name);
                }
                Err(error)
            }
        }
    }

    fn create_exclusive_copy(
        &self,
        source: HardLinkEndpoint<'_>,
        expected_source: &ExactFileObservation,
        destination: HardLinkEndpoint<'_>,
    ) -> io::Result<ExclusiveFileCopy> {
        create_exclusive_file_copy(source, expected_source, destination)
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
        let identity = opened_regular_file_identity(&file)?;
        Ok(CreatedFile { file, identity })
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
        let identity = opened_regular_file_identity(&file)?;
        Ok(CreatedFile { file, identity })
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

    fn publish_immutable(
        &self,
        partial: HardLinkEndpoint<'_>,
        expected_partial: &ExactFileObservation,
        target: HardLinkEndpoint<'_>,
        target_parent: DirectoryEndpoint<'_>,
        expected_target_parent: &ExactDirectoryObservation,
    ) -> ImmutablePublicationOutcome {
        publish_immutable_exact(
            self,
            partial,
            expected_partial,
            target,
            target_parent,
            expected_target_parent,
        )
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

    fn publish_directory_absent(
        &self,
        candidate: DirectoryEndpoint<'_>,
        expected_candidate: &ExactDirectoryInventory,
        target_parent: DirectoryEndpoint<'_>,
        expected_target_parent: &ExactDirectoryObservation,
        target_name: &Path,
        target_path: &Path,
    ) -> DirectoryPublicationOutcome {
        publish_directory_absent_exact(
            self,
            candidate,
            expected_candidate,
            target_parent,
            expected_target_parent,
            target_name,
            target_path,
        )
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
    ) -> io::Result<()> {
        remove_exact_file(parent, name, path, expected, || Ok(()))
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
    ) -> io::Result<()> {
        remove_empty_directory_exact_impl(endpoint, expected, || Ok(()))
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
    identity: (u64, u64),
    byte_len: u64,
    mode: PreservedFileMode,
    link_count: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryMetadataState {
    identity: (u64, u64),
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
        identity: (MetadataExt::dev(metadata), MetadataExt::ino(metadata)),
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
        identity: (MetadataExt::dev(metadata), MetadataExt::ino(metadata)),
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

fn hash_file(file: &mut File) -> io::Result<(String, u64)> {
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

fn observe_regular_file_exact(
    parent: &Dir,
    name: &Path,
    path: &Path,
) -> io::Result<ExactFileObservation> {
    require_exact_identity_support()?;
    let path_before = regular_metadata_state(&parent.symlink_metadata(name)?, path)?;
    let mut file = open_regular_file_nofollow(parent, name)?;
    let handle_before = regular_metadata_state(&file.metadata()?, path)?;
    if handle_before != path_before {
        return Err(changed_during_observation(
            path,
            "the path and no-follow handle do not identify the same state",
        ));
    }

    let (content_hash, byte_len) = hash_file(&mut file)?;
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
    let mut hasher = Sha256::new();
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
        hasher.update(&buffer[..count]);
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

    Ok(ExactFileRead {
        bytes,
        observation: ExactFileObservation {
            identity: handle_after.identity,
            byte_len,
            content_hash: format!("sha256:{:x}", hasher.finalize()),
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

fn directory_names(directory: &Dir) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in directory.entries()? {
        names.push(entry?.file_name());
    }
    names.sort();
    Ok(names)
}

fn inventory_directory_exact_impl(
    endpoint: DirectoryEndpoint<'_>,
    expected: &ExactDirectoryObservation,
) -> io::Result<ExactDirectoryInventory> {
    require_exact_directory_state(endpoint, expected)?;
    let names_before = directory_names(endpoint.directory)?;
    let mut entries = Vec::with_capacity(names_before.len());
    for name in &names_before {
        let entry_path = endpoint.path.join(name);
        let metadata = endpoint.directory.symlink_metadata(name)?;
        entries.push(ExactDirectoryEntry {
            name: name.clone(),
            kind: directory_entry_kind(&metadata),
            identity: (MetadataExt::dev(&metadata), MetadataExt::ino(&metadata)),
            byte_len: metadata.len(),
            mode: preserved_mode(&metadata),
            link_count: Some(MetadataExt::nlink(&metadata)),
        });
        let current = endpoint.directory.symlink_metadata(name)?;
        let current_state = (
            directory_entry_kind(&current),
            MetadataExt::dev(&current),
            MetadataExt::ino(&current),
            current.len(),
            preserved_mode(&current),
            Some(MetadataExt::nlink(&current)),
        );
        let recorded = entries.last().expect("entry was just recorded");
        if current_state
            != (
                recorded.kind,
                recorded.identity.0,
                recorded.identity.1,
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
    let names_after = directory_names(endpoint.directory)?;
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
    let actual = observe_regular_file_exact(endpoint.parent, endpoint.name, endpoint.path)?;
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

#[derive(Debug)]
struct LinkedPublicationObservationError {
    partial: Option<ExactFileObservation>,
    published: Option<ExactFileObservation>,
    source: io::Error,
}

fn same_immutable_file_state(
    actual: &ExactFileObservation,
    expected: &ExactFileObservation,
) -> bool {
    actual.identity == expected.identity
        && actual.byte_len == expected.byte_len
        && actual.content_hash == expected.content_hash
        && actual.mode == expected.mode
}

fn same_directory_binding(
    actual: &ExactDirectoryObservation,
    expected: &ExactDirectoryObservation,
) -> bool {
    actual.identity == expected.identity && actual.mode == expected.mode
}

fn validate_linked_publication(
    partial: &ExactFileObservation,
    published: &ExactFileObservation,
    expected_partial: &ExactFileObservation,
    target_path: &Path,
) -> io::Result<()> {
    let expected_link_count = expected_partial
        .link_count
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "immutable partial is missing a representable exact link count",
            )
        })?;
    if partial != published
        || !same_immutable_file_state(partial, expected_partial)
        || partial.link_count != Some(expected_link_count)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not the exact hard-link publication of its recorded immutable partial",
                target_path.display()
            ),
        ));
    }
    Ok(())
}

fn observe_linked_publication<F>(
    fs: &F,
    partial: HardLinkEndpoint<'_>,
    target: HardLinkEndpoint<'_>,
    expected_partial: &ExactFileObservation,
) -> Result<(ExactFileObservation, ExactFileObservation), Box<LinkedPublicationObservationError>>
where
    F: FsOps + ?Sized,
{
    let published = match fs.observe_regular_file(target.parent, target.name, target.path) {
        Ok(published) => published,
        Err(source) => {
            return Err(Box::new(LinkedPublicationObservationError {
                partial: None,
                published: None,
                source,
            }));
        }
    };
    let observed_partial = match fs.observe_regular_file(partial.parent, partial.name, partial.path)
    {
        Ok(observed_partial) => observed_partial,
        Err(source) => {
            return Err(Box::new(LinkedPublicationObservationError {
                partial: None,
                published: Some(published),
                source,
            }));
        }
    };
    if let Err(source) =
        validate_linked_publication(&observed_partial, &published, expected_partial, target.path)
    {
        return Err(Box::new(LinkedPublicationObservationError {
            partial: Some(observed_partial),
            published: Some(published),
            source,
        }));
    }
    Ok((observed_partial, published))
}

fn require_name_absent(parent: &Dir, name: &Path, path: &Path) -> io::Result<()> {
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is unexpectedly occupied at an exact absent-name boundary",
                path.display()
            ),
        )),
        Err(error) => Err(error),
    }
}

fn recorded_file_absent_in_process(
    parent: &Dir,
    name: &Path,
    expected: &ExactFileObservation,
) -> bool {
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Ok(metadata) => {
            (MetadataExt::dev(&metadata), MetadataExt::ino(&metadata)) != expected.identity
        }
        Err(_) => false,
    }
}

fn publish_immutable_exact<F>(
    fs: &F,
    partial: HardLinkEndpoint<'_>,
    expected_partial: &ExactFileObservation,
    target: HardLinkEndpoint<'_>,
    target_parent: DirectoryEndpoint<'_>,
    expected_target_parent: &ExactDirectoryObservation,
) -> ImmutablePublicationOutcome
where
    F: FsOps + ?Sized,
{
    let partial_before = match (|| {
        require_exact_identity_support()?;
        if partial.name == target.name {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "immutable partial and publication names must differ",
            ));
        }
        ensure_same_parent(partial.parent, target.parent, partial.path, target.path)?;
        ensure_same_parent(
            partial.parent,
            target_parent.directory,
            partial.path,
            target_parent.path,
        )?;
        let observed_parent = fs.observe_directory(target_parent)?;
        if observed_parent != *expected_target_parent {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} no longer matches its recorded publication-parent state",
                    target_parent.path.display()
                ),
            ));
        }
        let observed_partial =
            fs.observe_regular_file(partial.parent, partial.name, partial.path)?;
        if observed_partial != *expected_partial || observed_partial.link_count != Some(1) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is not the exact independent single-link immutable partial",
                    partial.path.display()
                ),
            ));
        }
        Ok(observed_partial)
    })() {
        Ok(partial_before) => partial_before,
        Err(source) => {
            return ImmutablePublicationOutcome::NotPublished {
                partial: None,
                source,
            };
        }
    };

    if let Err(source) = fs.hard_link(&[], partial, target) {
        return match observe_linked_publication(fs, partial, target, expected_partial) {
            Ok((linked_partial, published)) => {
                ImmutablePublicationOutcome::VisibleDurabilityUnknown {
                    partial: linked_partial,
                    published: Some(published),
                    source,
                }
            }
            Err(_) => ImmutablePublicationOutcome::NotPublished {
                partial: Some(partial_before),
                source,
            },
        };
    }

    let (linked_partial, published_before_sync) =
        match observe_linked_publication(fs, partial, target, expected_partial) {
            Ok(observations) => observations,
            Err(error) => {
                return ImmutablePublicationOutcome::VisibleDurabilityUnknown {
                    partial: error.partial.unwrap_or(partial_before),
                    published: error.published,
                    source: error.source,
                };
            }
        };

    let linked_parent = match fs.observe_directory(target_parent) {
        Ok(linked_parent) if same_directory_binding(&linked_parent, expected_target_parent) => {
            linked_parent
        }
        Ok(_) => {
            return ImmutablePublicationOutcome::VisibleDurabilityUnknown {
                partial: linked_partial,
                published: Some(published_before_sync),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} changed identity or mode during immutable publication",
                        target_parent.path.display()
                    ),
                ),
            };
        }
        Err(source) => {
            return ImmutablePublicationOutcome::VisibleDurabilityUnknown {
                partial: linked_partial,
                published: Some(published_before_sync),
                source,
            };
        }
    };
    if let Err(source) = fs.sync_parent(target_parent, &linked_parent, ParentSyncKind::Target) {
        return ImmutablePublicationOutcome::VisibleDurabilityUnknown {
            partial: linked_partial,
            published: Some(published_before_sync),
            source,
        };
    }

    let (durable_partial, durable_published) =
        match observe_linked_publication(fs, partial, target, expected_partial) {
            Ok(observations) => observations,
            Err(error) => {
                return ImmutablePublicationOutcome::VisibleDurabilityUnknown {
                    partial: error.partial.unwrap_or(linked_partial),
                    published: error.published.or(Some(published_before_sync)),
                    source: error.source,
                };
            }
        };

    if let Err(source) =
        fs.remove_file_exact(partial.parent, partial.name, partial.path, &durable_partial)
    {
        return ImmutablePublicationOutcome::DurableWithPartialResidual {
            partial_absent_in_process: recorded_file_absent_in_process(
                partial.parent,
                partial.name,
                &durable_partial,
            ),
            last_linked_published: durable_published,
            last_linked_partial: durable_partial,
            source,
        };
    }
    if let Err(source) = require_name_absent(partial.parent, partial.name, partial.path) {
        return ImmutablePublicationOutcome::DurableWithPartialResidual {
            last_linked_published: durable_published,
            last_linked_partial: durable_partial,
            partial_absent_in_process: true,
            source,
        };
    }
    let cleanup_parent = match fs.observe_directory(target_parent) {
        Ok(cleanup_parent) if same_directory_binding(&cleanup_parent, expected_target_parent) => {
            cleanup_parent
        }
        Ok(_) => {
            return ImmutablePublicationOutcome::DurableWithPartialResidual {
                last_linked_published: durable_published,
                last_linked_partial: durable_partial,
                partial_absent_in_process: true,
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} changed identity or mode during immutable-partial cleanup",
                        target_parent.path.display()
                    ),
                ),
            };
        }
        Err(source) => {
            return ImmutablePublicationOutcome::DurableWithPartialResidual {
                last_linked_published: durable_published,
                last_linked_partial: durable_partial,
                partial_absent_in_process: true,
                source,
            };
        }
    };
    if let Err(source) = fs.sync_parent(target_parent, &cleanup_parent, ParentSyncKind::Target) {
        return ImmutablePublicationOutcome::DurableWithPartialResidual {
            last_linked_published: durable_published,
            last_linked_partial: durable_partial,
            partial_absent_in_process: true,
            source,
        };
    }
    if let Err(source) = require_name_absent(partial.parent, partial.name, partial.path) {
        return ImmutablePublicationOutcome::DurableWithPartialResidual {
            last_linked_published: durable_published,
            last_linked_partial: durable_partial,
            partial_absent_in_process: true,
            source,
        };
    }

    match fs.observe_regular_file(target.parent, target.name, target.path) {
        Ok(published) if published == *expected_partial => {
            ImmutablePublicationOutcome::Durable { published }
        }
        Ok(_) => ImmutablePublicationOutcome::DurableWithPartialResidual {
            last_linked_published: durable_published,
            last_linked_partial: durable_partial,
            partial_absent_in_process: true,
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} changed after immutable publication cleanup",
                    target.path.display()
                ),
            ),
        },
        Err(source) => ImmutablePublicationOutcome::DurableWithPartialResidual {
            last_linked_published: durable_published,
            last_linked_partial: durable_partial,
            partial_absent_in_process: true,
            source,
        },
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

fn rename_directory_noreplace_impl(
    candidate_parent: &Dir,
    candidate_name: &Path,
    candidate_path: &Path,
    target_parent: &Dir,
    target_name: &Path,
    target_path: &Path,
) -> io::Result<()> {
    require_single_component_name(candidate_name, candidate_path)?;
    require_single_component_name(target_name, target_path)?;
    ensure_same_parent(candidate_parent, target_parent, candidate_path, target_path)?;

    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    {
        rustix::fs::renameat_with(
            candidate_parent,
            candidate_name,
            target_parent,
            target_name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    {
        let _ = (candidate_parent, candidate_name, target_parent, target_name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "capability-relative atomic no-replace directory rename is unavailable on this platform",
        ))
    }
}

fn observe_published_directory<F>(
    fs: &F,
    candidate: DirectoryEndpoint<'_>,
    expected_candidate: &ExactDirectoryInventory,
    target_parent: DirectoryEndpoint<'_>,
    target_name: &Path,
    target_path: &Path,
) -> io::Result<ExactDirectoryInventory>
where
    F: FsOps + ?Sized,
{
    require_name_absent(candidate.parent, candidate.name, candidate.path)?;
    let published_endpoint = DirectoryEndpoint::new(
        target_parent.directory,
        target_name,
        candidate.directory,
        target_path,
    );
    let published =
        fs.inventory_directory_exact(published_endpoint, &expected_candidate.directory)?;
    if published != *expected_candidate {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} does not match the exact prepared directory candidate",
                target_path.display()
            ),
        ));
    }
    Ok(published)
}

fn publish_directory_absent_exact<F>(
    fs: &F,
    candidate: DirectoryEndpoint<'_>,
    expected_candidate: &ExactDirectoryInventory,
    target_parent: DirectoryEndpoint<'_>,
    expected_target_parent: &ExactDirectoryObservation,
    target_name: &Path,
    target_path: &Path,
) -> DirectoryPublicationOutcome
where
    F: FsOps + ?Sized,
{
    let candidate_before = match (|| {
        require_exact_identity_support()?;
        require_single_component_name(candidate.name, candidate.path)?;
        require_single_component_name(target_name, target_path)?;
        if candidate.name == target_name {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "prepared directory candidate and publication names must differ",
            ));
        }
        ensure_same_parent(
            candidate.parent,
            target_parent.directory,
            candidate.path,
            target_path,
        )?;
        let observed_parent = fs.observe_directory(target_parent)?;
        if observed_parent != *expected_target_parent {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} no longer matches its recorded directory-publication parent",
                    target_parent.path.display()
                ),
            ));
        }
        let observed_candidate =
            fs.inventory_directory_exact(candidate, &expected_candidate.directory)?;
        if observed_candidate != *expected_candidate {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} no longer matches its exact prepared directory inventory",
                    candidate.path.display()
                ),
            ));
        }
        Ok(observed_candidate)
    })() {
        Ok(candidate_before) => candidate_before,
        Err(source) => {
            return DirectoryPublicationOutcome::NotPublished {
                candidate: None,
                source,
            };
        }
    };

    if let Err(source) = fs.rename_directory_noreplace(
        candidate.parent,
        candidate.name,
        candidate.path,
        target_parent.directory,
        target_name,
        target_path,
    ) {
        return match observe_published_directory(
            fs,
            candidate,
            expected_candidate,
            target_parent,
            target_name,
            target_path,
        ) {
            Ok(published) => DirectoryPublicationOutcome::VisibleDurabilityUnknown {
                candidate: candidate_before,
                published: Some(published),
                source,
            },
            Err(_) => DirectoryPublicationOutcome::NotPublished {
                candidate: Some(candidate_before),
                source,
            },
        };
    }

    let published_before_sync = match observe_published_directory(
        fs,
        candidate,
        expected_candidate,
        target_parent,
        target_name,
        target_path,
    ) {
        Ok(published) => published,
        Err(source) => {
            return DirectoryPublicationOutcome::VisibleDurabilityUnknown {
                candidate: candidate_before,
                published: None,
                source,
            };
        }
    };
    let published_parent = match fs.observe_directory(target_parent) {
        Ok(published_parent)
            if same_directory_binding(&published_parent, expected_target_parent) =>
        {
            published_parent
        }
        Ok(_) => {
            return DirectoryPublicationOutcome::VisibleDurabilityUnknown {
                candidate: candidate_before,
                published: Some(published_before_sync),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} changed identity or mode during directory publication",
                        target_parent.path.display()
                    ),
                ),
            };
        }
        Err(source) => {
            return DirectoryPublicationOutcome::VisibleDurabilityUnknown {
                candidate: candidate_before,
                published: Some(published_before_sync),
                source,
            };
        }
    };
    if let Err(source) = fs.sync_parent(target_parent, &published_parent, ParentSyncKind::Target) {
        return DirectoryPublicationOutcome::VisibleDurabilityUnknown {
            candidate: candidate_before,
            published: Some(published_before_sync),
            source,
        };
    }
    match observe_published_directory(
        fs,
        candidate,
        expected_candidate,
        target_parent,
        target_name,
        target_path,
    ) {
        Ok(published) => DirectoryPublicationOutcome::Durable { published },
        Err(source) => DirectoryPublicationOutcome::VisibleDurabilityUnknown {
            candidate: candidate_before,
            published: Some(published_before_sync),
            source,
        },
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

fn create_exclusive_file_copy(
    source: HardLinkEndpoint<'_>,
    expected_source: &ExactFileObservation,
    destination: HardLinkEndpoint<'_>,
) -> io::Result<ExclusiveFileCopy> {
    require_exact_identity_support()?;
    ensure_same_parent(
        source.parent,
        destination.parent,
        source.path,
        destination.path,
    )?;

    let source_path_before =
        regular_metadata_state(&source.parent.symlink_metadata(source.name)?, source.path)?;
    if !observation_matches_metadata(expected_source, source_path_before) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} changed before it could be copied",
                source.path.display()
            ),
        ));
    }
    let mut source_file = open_regular_file_nofollow(source.parent, source.name)?;
    let source_handle_before = regular_metadata_state(&source_file.metadata()?, source.path)?;
    if source_handle_before != source_path_before {
        return Err(changed_during_observation(
            source.path,
            "the copy source path and handle differ",
        ));
    }

    let creation_mode = expected_source.mode.posix_mode.unwrap_or(0o600);
    let CreatedFile {
        file: mut destination_file,
        identity: destination_identity,
    } = SystemFs.create_new_file(
        destination.parent,
        destination.name,
        destination.path,
        creation_mode,
    )?;

    let result = (|| {
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
        loop {
            let count = source_file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            destination_file.write_all(&buffer[..count])?;
            source_hasher.update(&buffer[..count]);
            copied_len = copied_len
                .checked_add(count as u64)
                .ok_or_else(|| io::Error::other("regular-file length overflow while copying"))?;
        }
        destination_file.flush()?;

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

        set_exact_file_mode(&destination_file, expected_source.mode)?;
        let (copy_hash, copy_len) = hash_file(&mut destination_file)?;
        destination_file.seek(SeekFrom::End(0))?;
        let copy_handle = regular_metadata_state(&destination_file.metadata()?, destination.path)?;
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

        Ok(ExclusiveFileCopy {
            file: destination_file,
            source: ExactFileObservation {
                identity: source_handle_after.identity,
                byte_len: copied_len,
                content_hash: source_hash,
                mode: source_handle_after.mode,
                link_count: source_handle_after.link_count,
            },
            copy: ExactFileObservation {
                identity: copy_handle.identity,
                byte_len: copy_len,
                content_hash: copy_hash,
                mode: copy_handle.mode,
                link_count: copy_handle.link_count,
            },
        })
    })();

    match result {
        Ok(copy) => Ok(copy),
        Err(source_error) => {
            let cleanup = remove_created_file_if_owned(
                destination.parent,
                destination.name,
                destination.path,
                destination_identity,
            );
            match cleanup {
                Ok(()) => Err(source_error),
                Err(cleanup_error) => Err(io::Error::other(format!(
                    "{source_error}; additionally failed to remove exclusive copy {}: {cleanup_error}",
                    destination.path.display()
                ))),
            }
        }
    }
}

fn remove_created_file_if_owned(
    parent: &Dir,
    name: &Path,
    path: &Path,
    expected_identity: (u64, u64),
) -> io::Result<()> {
    let metadata = parent.symlink_metadata(name)?;
    let state = regular_metadata_state(&metadata, path)?;
    if state.identity != expected_identity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} was substituted before exclusive-copy cleanup",
                path.display()
            ),
        ));
    }
    parent.remove_file(name)
}

fn remove_exact_file<F>(
    parent: &Dir,
    name: &Path,
    path: &Path,
    expected: &ExactFileObservation,
    before_unlink: F,
) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    let actual = observe_regular_file_exact(parent, name, path)?;
    if actual != *expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not the exact recorded cleanup object",
                path.display()
            ),
        ));
    }

    let pinned = open_regular_file_nofollow(parent, name)?;
    before_unlink()?;
    let handle_state = regular_metadata_state(&pinned.metadata()?, path)?;
    let path_state = regular_metadata_state(&parent.symlink_metadata(name)?, path)?;
    if handle_state != path_state || !observation_matches_metadata(expected, handle_state) {
        return Err(changed_during_observation(
            path,
            "the cleanup path changed after exact validation",
        ));
    }
    parent.remove_file(name)?;

    let handle_after = regular_metadata_state(&pinned.metadata()?, path)?;
    if handle_after.identity != expected.identity
        || handle_after.byte_len != expected.byte_len
        || handle_after.mode != expected.mode
    {
        return Err(changed_during_observation(
            path,
            "the pinned cleanup object changed across unlink",
        ));
    }
    if let Some(expected_links) = expected.link_count
        && handle_after.link_count != expected_links.checked_sub(1)
    {
        return Err(changed_during_observation(
            path,
            "unlink did not decrement the pinned object's link count exactly once",
        ));
    }
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(metadata)
            if (MetadataExt::dev(&metadata), MetadataExt::ino(&metadata)) != expected.identity =>
        {
            // Another actor recreated the name after the exact object was
            // unlinked. It is not transaction-owned and must be preserved.
            Ok(())
        }
        Ok(_) => Err(changed_during_observation(
            path,
            "the exact object still occupies its name after unlink",
        )),
        Err(error) => Err(error),
    }
}

fn remove_empty_directory_exact_impl<F>(
    endpoint: DirectoryEndpoint<'_>,
    expected: &ExactDirectoryObservation,
    before_unlink: F,
) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    let initial = inventory_directory_exact_impl(endpoint, expected)?;
    if !initial.entries.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::DirectoryNotEmpty,
            format!(
                "{} is not an exact empty directory",
                endpoint.path.display()
            ),
        ));
    }

    before_unlink()?;
    let final_inventory = inventory_directory_exact_impl(endpoint, expected)?;
    if !final_inventory.entries.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::DirectoryNotEmpty,
            format!(
                "{} gained children at the exact removal boundary",
                endpoint.path.display()
            ),
        ));
    }
    endpoint.parent.remove_dir(endpoint.name)?;

    let handle_after =
        directory_metadata_state(&endpoint.directory.dir_metadata()?, endpoint.path)?;
    if handle_after.identity != expected.identity || handle_after.mode != expected.mode {
        return Err(changed_during_observation(
            endpoint.path,
            "the pinned directory changed across removal",
        ));
    }
    match endpoint.parent.symlink_metadata(endpoint.name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(metadata)
            if (MetadataExt::dev(&metadata), MetadataExt::ino(&metadata)) != expected.identity =>
        {
            Ok(())
        }
        Ok(_) => Err(changed_during_observation(
            endpoint.path,
            "the exact directory still occupies its name after removal",
        )),
        Err(error) => Err(error),
    }
}

fn opened_regular_file_identity(file: &File) -> io::Result<(u64, u64)> {
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
    Ok((MetadataExt::dev(&metadata), MetadataExt::ino(&metadata)))
}

pub(crate) fn current_regular_file_identity(parent: &Dir, name: &Path) -> io::Result<(u64, u64)> {
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
    Ok((MetadataExt::dev(&metadata), MetadataExt::ino(&metadata)))
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsOperation {
    CreateDirectory,
    OpenCoordinationFile,
    InspectMetadata,
    ReadHandle,
    ObserveRegularFile,
    ReadRegularFileExact,
    ObserveDirectory,
    OpenDirectoryExact,
    CreateDirectoryExact,
    InventoryDirectoryExact,
    CreateNewFile,
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
    PublishImmutable,
    RenameDirectoryNoReplace,
    PublishDirectoryAbsent,
    RemoveFile,
    RemoveFileExact,
    BeforeExactUnlink,
    #[cfg(windows)]
    RemoveFileByHandle,
    RemoveDirectory,
    RemoveDirectoryExact,
    BeforeFinalRevalidation,
    AfterFinalRevalidation,
    Rename,
    ReplaceExisting,
    RenameJournal,
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
    fn target(&self) -> &Path {
        match self {
            Self::WriteFile { target, .. } => target,
            Self::ReplaceFile { target, .. } => target,
            #[cfg(unix)]
            Self::ReplaceParentWithSymlink { target, .. } => target,
            #[cfg(unix)]
            Self::ReplaceParentWithDirectory { target, .. } => target,
        }
    }

    fn apply(self) -> io::Result<()> {
        match self {
            Self::WriteFile { target, content } => fs::write(target, content),
            Self::ReplaceFile {
                target,
                moved_target,
                content,
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
    counts: std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    events: std::sync::Mutex<Vec<FsEvent>>,
    pauses_after_success: Vec<PauseAfterSuccess>,
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
impl FaultFs {
    pub(crate) fn passthrough() -> Self {
        Self {
            fail: std::sync::Mutex::new(None),
            counts: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            events: std::sync::Mutex::new(Vec::new()),
            pauses_after_success: Vec::new(),
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
        self.events.lock().expect("event lock").push(FsEvent {
            operation,
            path: path.to_path_buf(),
            destination: destination.map(Path::to_path_buf),
        });
        let key = format!("{operation:?}");
        let ordinal = {
            let mut counts = self.counts.lock().expect("count lock");
            let count = counts.entry(key).or_default();
            *count += 1;
            *count
        };
        if self
            .fail
            .lock()
            .expect("fault lock")
            .is_some_and(|mode| match mode {
                FaultMode::Once {
                    operation: target,
                    ordinal: target_ordinal,
                } => target == operation && target_ordinal == ordinal,
                FaultMode::From {
                    operation: target,
                    ordinal: target_ordinal,
                } => target == operation && ordinal >= target_ordinal,
            })
        {
            Err(io::Error::other(format!(
                "injected {operation:?} failure at ordinal {ordinal}"
            )))
        } else {
            Ok(())
        }
    }

    fn after_success(&self, operation: FsOperation, path: &Path) -> io::Result<()> {
        let ordinal = self
            .counts
            .lock()
            .expect("count lock")
            .get(&format!("{operation:?}"))
            .copied()
            .unwrap_or_default();
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
                .is_some_and(|mutation| mutation.target() == path)
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

    fn create_new_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<CreatedFile> {
        self.before(FsOperation::CreateNewFile, path, None)?;
        let file = SystemFs.create_new_file(parent, name, path, mode)?;
        self.after_success(FsOperation::CreateNewFile, path)?;
        Ok(file)
    }

    fn create_exclusive_copy(
        &self,
        source: HardLinkEndpoint<'_>,
        expected_source: &ExactFileObservation,
        destination: HardLinkEndpoint<'_>,
    ) -> io::Result<ExclusiveFileCopy> {
        self.before(
            FsOperation::CreateExclusiveCopy,
            source.path,
            Some(destination.path),
        )?;
        let copy = SystemFs.create_exclusive_copy(source, expected_source, destination)?;
        self.after_success(FsOperation::CreateExclusiveCopy, source.path)?;
        Ok(copy)
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

    fn publish_immutable(
        &self,
        partial: HardLinkEndpoint<'_>,
        expected_partial: &ExactFileObservation,
        target: HardLinkEndpoint<'_>,
        target_parent: DirectoryEndpoint<'_>,
        expected_target_parent: &ExactDirectoryObservation,
    ) -> ImmutablePublicationOutcome {
        if let Err(source) = self.before(
            FsOperation::PublishImmutable,
            partial.path,
            Some(target.path),
        ) {
            return ImmutablePublicationOutcome::NotPublished {
                partial: None,
                source,
            };
        }
        publish_immutable_exact(
            self,
            partial,
            expected_partial,
            target,
            target_parent,
            expected_target_parent,
        )
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

    fn publish_directory_absent(
        &self,
        candidate: DirectoryEndpoint<'_>,
        expected_candidate: &ExactDirectoryInventory,
        target_parent: DirectoryEndpoint<'_>,
        expected_target_parent: &ExactDirectoryObservation,
        target_name: &Path,
        target_path: &Path,
    ) -> DirectoryPublicationOutcome {
        if let Err(source) = self.before(
            FsOperation::PublishDirectoryAbsent,
            candidate.path,
            Some(target_path),
        ) {
            return DirectoryPublicationOutcome::NotPublished {
                candidate: None,
                source,
            };
        }
        publish_directory_absent_exact(
            self,
            candidate,
            expected_candidate,
            target_parent,
            expected_target_parent,
            target_name,
            target_path,
        )
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
    ) -> io::Result<()> {
        self.before(FsOperation::RemoveFileExact, path, None)?;
        remove_exact_file(parent, name, path, expected, || {
            self.before_exact_unlink(path)
        })?;
        self.after_success(FsOperation::RemoveFileExact, path)
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
    ) -> io::Result<()> {
        self.before(FsOperation::RemoveDirectoryExact, endpoint.path, None)?;
        remove_empty_directory_exact_impl(endpoint, expected, || {
            self.before_exact_unlink(endpoint.path)
        })?;
        self.after_success(FsOperation::RemoveDirectoryExact, endpoint.path)
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
                .is_some_and(|mutation| mutation.target() == path)
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
                .is_some_and(|mutation| mutation.target() == path)
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
        DirectoryEndpoint, DirectoryPublicationOutcome, ExactDirectoryEntryKind,
        ExactIdentitySupport, FsOperation, FsOps, HardLinkEndpoint, ImmutablePublicationOutcome,
        ParentSyncKind, SystemFs, exact_identity_support,
    };

    struct Fixture {
        _temporary: TempDir,
        root: Dir,
        files: Dir,
        files_path: PathBuf,
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
    fn immutable_publication_reports_visibility_durability_and_partial_cleanup() {
        let fixture = Fixture::new();
        fixture.write("partial", b"immutable bytes\n");
        let partial_path = fixture.path("partial");
        let target_path = fixture.path("target");
        let partial = SystemFs
            .observe_regular_file(&fixture.files, Path::new("partial"), &partial_path)
            .expect("observe immutable partial");
        let parent_endpoint = DirectoryEndpoint::new(
            &fixture.root,
            Path::new("files"),
            &fixture.files,
            &fixture.files_path,
        );
        let parent = SystemFs
            .observe_directory(parent_endpoint)
            .expect("observe publication parent");
        let outcome = SystemFs.publish_immutable(
            fixture.endpoint("partial", &partial_path),
            &partial,
            fixture.endpoint("target", &target_path),
            parent_endpoint,
            &parent,
        );
        let published = match outcome {
            ImmutablePublicationOutcome::Durable { published } => published,
            other => panic!("expected durable publication, got {other:?}"),
        };
        assert_eq!(published, partial);
        assert!(!partial_path.exists());
        assert_eq!(
            std::fs::read(&target_path).expect("read durable target"),
            b"immutable bytes\n"
        );

        let occupied = Fixture::new();
        occupied.write("partial", b"immutable bytes\n");
        occupied.write("target", b"application bytes\n");
        let partial_path = occupied.path("partial");
        let target_path = occupied.path("target");
        let partial = SystemFs
            .observe_regular_file(&occupied.files, Path::new("partial"), &partial_path)
            .expect("observe no-clobber partial");
        let parent_endpoint = DirectoryEndpoint::new(
            &occupied.root,
            Path::new("files"),
            &occupied.files,
            &occupied.files_path,
        );
        let parent = SystemFs
            .observe_directory(parent_endpoint)
            .expect("observe no-clobber parent");
        match SystemFs.publish_immutable(
            occupied.endpoint("partial", &partial_path),
            &partial,
            occupied.endpoint("target", &target_path),
            parent_endpoint,
            &parent,
        ) {
            ImmutablePublicationOutcome::NotPublished {
                partial: Some(observed),
                source,
            } => {
                assert_eq!(observed, partial);
                assert_eq!(source.kind(), std::io::ErrorKind::AlreadyExists);
            }
            other => panic!("expected no-clobber refusal, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(&target_path).expect("read preserved application target"),
            b"application bytes\n"
        );
        assert!(partial_path.exists());

        let uncertain = Fixture::new();
        uncertain.write("partial", b"uncertain bytes\n");
        let partial_path = uncertain.path("partial");
        let target_path = uncertain.path("target");
        let partial = SystemFs
            .observe_regular_file(&uncertain.files, Path::new("partial"), &partial_path)
            .expect("observe uncertain partial");
        let parent_endpoint = DirectoryEndpoint::new(
            &uncertain.root,
            Path::new("files"),
            &uncertain.files,
            &uncertain.files_path,
        );
        let parent = SystemFs
            .observe_directory(parent_endpoint)
            .expect("observe uncertain parent");
        let fault = super::FaultFs::fail_nth(FsOperation::SyncTargetParent, 1);
        match fault.publish_immutable(
            uncertain.endpoint("partial", &partial_path),
            &partial,
            uncertain.endpoint("target", &target_path),
            parent_endpoint,
            &parent,
        ) {
            ImmutablePublicationOutcome::VisibleDurabilityUnknown {
                partial,
                published: Some(published),
                source,
            } => {
                assert_eq!(partial.identity, published.identity);
                assert!(source.to_string().contains("SyncTargetParent"));
            }
            other => panic!("expected visible durability uncertainty, got {other:?}"),
        }
        assert!(partial_path.exists());
        assert!(target_path.exists());

        let residual = Fixture::new();
        residual.write("partial", b"residual bytes\n");
        let partial_path = residual.path("partial");
        let target_path = residual.path("target");
        let partial = SystemFs
            .observe_regular_file(&residual.files, Path::new("partial"), &partial_path)
            .expect("observe residual partial");
        let parent_endpoint = DirectoryEndpoint::new(
            &residual.root,
            Path::new("files"),
            &residual.files,
            &residual.files_path,
        );
        let parent = SystemFs
            .observe_directory(parent_endpoint)
            .expect("observe residual parent");
        let fault = super::FaultFs::fail_nth(FsOperation::RemoveFileExact, 1);
        match fault.publish_immutable(
            residual.endpoint("partial", &partial_path),
            &partial,
            residual.endpoint("target", &target_path),
            parent_endpoint,
            &parent,
        ) {
            ImmutablePublicationOutcome::DurableWithPartialResidual {
                last_linked_published,
                last_linked_partial,
                partial_absent_in_process,
                source,
            } => {
                assert_eq!(last_linked_partial.identity, last_linked_published.identity);
                assert!(!partial_absent_in_process);
                assert!(source.to_string().contains("RemoveFileExact"));
            }
            other => panic!("expected durable publication with residual, got {other:?}"),
        }
        assert!(partial_path.exists());
        assert!(target_path.exists());

        let cleanup_sync = Fixture::new();
        cleanup_sync.write("partial", b"cleanup sync bytes\n");
        let partial_path = cleanup_sync.path("partial");
        let target_path = cleanup_sync.path("target");
        let partial = SystemFs
            .observe_regular_file(&cleanup_sync.files, Path::new("partial"), &partial_path)
            .expect("observe cleanup-sync partial");
        let parent_endpoint = DirectoryEndpoint::new(
            &cleanup_sync.root,
            Path::new("files"),
            &cleanup_sync.files,
            &cleanup_sync.files_path,
        );
        let parent = SystemFs
            .observe_directory(parent_endpoint)
            .expect("observe cleanup-sync parent");
        let fault = super::FaultFs::fail_nth(FsOperation::SyncTargetParent, 2);
        match fault.publish_immutable(
            cleanup_sync.endpoint("partial", &partial_path),
            &partial,
            cleanup_sync.endpoint("target", &target_path),
            parent_endpoint,
            &parent,
        ) {
            ImmutablePublicationOutcome::DurableWithPartialResidual {
                partial_absent_in_process,
                source,
                ..
            } => {
                assert!(partial_absent_in_process);
                assert!(source.to_string().contains("SyncTargetParent"));
            }
            other => panic!("expected conservative cleanup-sync residual, got {other:?}"),
        }
        assert!(!partial_path.exists());
        assert!(target_path.exists());
    }

    #[test]
    fn prepared_directory_publication_is_atomic_no_clobber_and_typed_for_durability() {
        let fixture = Fixture::new();
        let candidate_path = fixture.path("candidate");
        let target_path = fixture.path("journal");
        let candidate = SystemFs
            .create_directory_exact(
                &fixture.files,
                Path::new("candidate"),
                &candidate_path,
                0o700,
            )
            .expect("create prepared directory candidate");
        std::fs::write(candidate_path.join("record.json"), b"{\"prepared\":true}\n")
            .expect("write prepared record");
        let candidate_endpoint = DirectoryEndpoint::new(
            &fixture.files,
            Path::new("candidate"),
            &candidate.directory,
            &candidate_path,
        );
        let candidate_observation = SystemFs
            .observe_directory(candidate_endpoint)
            .expect("observe prepared candidate");
        let candidate_inventory = SystemFs
            .inventory_directory_exact(candidate_endpoint, &candidate_observation)
            .expect("inventory prepared candidate");
        let parent_endpoint = DirectoryEndpoint::new(
            &fixture.root,
            Path::new("files"),
            &fixture.files,
            &fixture.files_path,
        );
        let parent = SystemFs
            .observe_directory(parent_endpoint)
            .expect("observe directory-publication parent");
        let published = match SystemFs.publish_directory_absent(
            candidate_endpoint,
            &candidate_inventory,
            parent_endpoint,
            &parent,
            Path::new("journal"),
            &target_path,
        ) {
            DirectoryPublicationOutcome::Durable { published } => published,
            other => panic!("expected durable directory publication, got {other:?}"),
        };
        assert_eq!(published, candidate_inventory);
        assert!(!candidate_path.exists());
        assert_eq!(
            std::fs::read(target_path.join("record.json")).expect("read published record"),
            b"{\"prepared\":true}\n"
        );

        let occupied = Fixture::new();
        let candidate_path = occupied.path("candidate");
        let target_path = occupied.path("journal");
        let candidate = SystemFs
            .create_directory_exact(
                &occupied.files,
                Path::new("candidate"),
                &candidate_path,
                0o700,
            )
            .expect("create no-clobber candidate");
        std::fs::create_dir(&target_path).expect("create existing logical directory");
        let candidate_endpoint = DirectoryEndpoint::new(
            &occupied.files,
            Path::new("candidate"),
            &candidate.directory,
            &candidate_path,
        );
        let candidate_inventory = SystemFs
            .inventory_directory_exact(candidate_endpoint, &candidate.observation)
            .expect("inventory no-clobber candidate");
        let parent_endpoint = DirectoryEndpoint::new(
            &occupied.root,
            Path::new("files"),
            &occupied.files,
            &occupied.files_path,
        );
        let parent = SystemFs
            .observe_directory(parent_endpoint)
            .expect("observe no-clobber directory parent");
        match SystemFs.publish_directory_absent(
            candidate_endpoint,
            &candidate_inventory,
            parent_endpoint,
            &parent,
            Path::new("journal"),
            &target_path,
        ) {
            DirectoryPublicationOutcome::NotPublished {
                candidate: Some(observed),
                source,
            } => {
                assert_eq!(observed, candidate_inventory);
                assert_eq!(source.kind(), std::io::ErrorKind::AlreadyExists);
            }
            other => panic!("expected atomic no-clobber refusal, got {other:?}"),
        }
        assert!(candidate_path.is_dir());
        assert!(target_path.is_dir());

        let uncertain = Fixture::new();
        let candidate_path = uncertain.path("candidate");
        let target_path = uncertain.path("journal");
        let candidate = SystemFs
            .create_directory_exact(
                &uncertain.files,
                Path::new("candidate"),
                &candidate_path,
                0o700,
            )
            .expect("create uncertain candidate");
        let candidate_endpoint = DirectoryEndpoint::new(
            &uncertain.files,
            Path::new("candidate"),
            &candidate.directory,
            &candidate_path,
        );
        let candidate_inventory = SystemFs
            .inventory_directory_exact(candidate_endpoint, &candidate.observation)
            .expect("inventory uncertain candidate");
        let parent_endpoint = DirectoryEndpoint::new(
            &uncertain.root,
            Path::new("files"),
            &uncertain.files,
            &uncertain.files_path,
        );
        let parent = SystemFs
            .observe_directory(parent_endpoint)
            .expect("observe uncertain directory parent");
        let fault = super::FaultFs::fail_nth(FsOperation::SyncTargetParent, 1);
        match fault.publish_directory_absent(
            candidate_endpoint,
            &candidate_inventory,
            parent_endpoint,
            &parent,
            Path::new("journal"),
            &target_path,
        ) {
            DirectoryPublicationOutcome::VisibleDurabilityUnknown {
                published: Some(published),
                source,
                ..
            } => {
                assert_eq!(published, candidate_inventory);
                assert!(source.to_string().contains("SyncTargetParent"));
            }
            other => panic!("expected directory durability uncertainty, got {other:?}"),
        }
        assert!(!candidate_path.exists());
        assert!(target_path.is_dir());

        let refused = Fixture::new();
        let candidate_path = refused.path("candidate");
        let target_path = refused.path("journal");
        let candidate = SystemFs
            .create_directory_exact(
                &refused.files,
                Path::new("candidate"),
                &candidate_path,
                0o700,
            )
            .expect("create refused candidate");
        let candidate_endpoint = DirectoryEndpoint::new(
            &refused.files,
            Path::new("candidate"),
            &candidate.directory,
            &candidate_path,
        );
        let candidate_inventory = SystemFs
            .inventory_directory_exact(candidate_endpoint, &candidate.observation)
            .expect("inventory refused candidate");
        let parent_endpoint = DirectoryEndpoint::new(
            &refused.root,
            Path::new("files"),
            &refused.files,
            &refused.files_path,
        );
        let parent = SystemFs
            .observe_directory(parent_endpoint)
            .expect("observe refused directory parent");
        let fault = super::FaultFs::fail_nth(FsOperation::RenameDirectoryNoReplace, 1);
        match fault.publish_directory_absent(
            candidate_endpoint,
            &candidate_inventory,
            parent_endpoint,
            &parent,
            Path::new("journal"),
            &target_path,
        ) {
            DirectoryPublicationOutcome::NotPublished {
                candidate: Some(observed),
                source,
            } => {
                assert_eq!(observed, candidate_inventory);
                assert!(source.to_string().contains("RenameDirectoryNoReplace"));
            }
            other => panic!("expected pre-rename refusal, got {other:?}"),
        }
        assert!(candidate_path.is_dir());
        assert!(!target_path.exists());
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
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
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
        assert_eq!(error.kind(), std::io::ErrorKind::DirectoryNotEmpty);

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
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
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
