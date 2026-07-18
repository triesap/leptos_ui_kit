use std::{
    fmt,
    io::{self, Write},
    panic::{RefUnwindSafe, UnwindSafe},
    path::Path,
};

#[cfg(test)]
use std::fs;

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, File, OpenOptions};
use cap_std::io_lifetimes::AsFilelike;

#[cfg(test)]
use std::path::PathBuf;

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
    fn create_new_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        mode: u32,
    ) -> io::Result<CreatedFile>;
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
    fn sync_directory(&self, directory: &Dir, path: &Path) -> io::Result<()>;
    fn try_lock(&self, file: &File, path: &Path) -> Result<(), std::fs::TryLockError>;
    fn hard_link(
        &self,
        pinned_directories: &[Dir],
        from: HardLinkEndpoint<'_>,
        to: HardLinkEndpoint<'_>,
    ) -> io::Result<()>;
    fn remove_file(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()>;
    #[cfg(windows)]
    fn remove_file_by_handle(&self, file: File, path: &Path) -> Result<(), HandleDeleteError>;
    fn remove_dir(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()>;
    fn before_final_revalidation(&self, path: &Path) -> io::Result<()>;
    fn after_final_revalidation(&self, path: &Path) -> io::Result<()>;
    fn before_target_publication(&self, path: &Path) -> io::Result<()>;
    fn rename(
        &self,
        from_parent: &Dir,
        from_name: &Path,
        from: &Path,
        to_parent: &Dir,
        to_name: &Path,
        to: &Path,
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
                Storage::FileSystem::{DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE},
            };

            options.access_mode(GENERIC_READ | GENERIC_WRITE | DELETE);
            options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
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
            Storage::FileSystem::{DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE},
        };

        options.access_mode(GENERIC_READ | GENERIC_WRITE | DELETE);
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
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

    fn sync_directory(&self, directory: &Dir, _path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            Dir::reopen_dir(directory)?.into_std_file().sync_all()
        }
        #[cfg(not(unix))]
        {
            let _ = directory;
            Ok(())
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

    fn remove_file(&self, parent: &Dir, name: &Path, _path: &Path) -> io::Result<()> {
        parent.remove_file(name)
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

    fn before_final_revalidation(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn after_final_revalidation(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn before_target_publication(&self, _path: &Path) -> io::Result<()> {
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
    CreateNewFile,
    #[cfg(windows)]
    OpenCleanupFile,
    #[cfg(windows)]
    OpenCandidateOwner,
    SetFileMode,
    SetDirectoryMode,
    WriteHandle,
    SyncHandle,
    SyncDirectory,
    TryLock,
    HardLink,
    RemoveFile,
    #[cfg(windows)]
    RemoveFileByHandle,
    RemoveDirectory,
    BeforeFinalRevalidation,
    AfterFinalRevalidation,
    BeforeTargetPublication,
    Rename,
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
            #[cfg(unix)]
            Self::ReplaceParentWithSymlink { target, .. } => target,
            #[cfg(unix)]
            Self::ReplaceParentWithDirectory { target, .. } => target,
        }
    }

    fn apply(self) -> io::Result<()> {
        match self {
            Self::WriteFile { target, content } => fs::write(target, content),
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
    publication_mutation: std::sync::Mutex<Option<FinalRevalidationMutation>>,
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
            publication_mutation: std::sync::Mutex::new(None),
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

    pub(crate) fn mutate_before_target_publication(path: PathBuf, content: Vec<u8>) -> Self {
        let fs = Self::passthrough();
        *fs.publication_mutation.lock().expect("mutation lock") =
            Some(FinalRevalidationMutation::WriteFile {
                target: path,
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

    fn sync_directory(&self, directory: &Dir, path: &Path) -> io::Result<()> {
        self.before(FsOperation::SyncDirectory, path, None)?;
        SystemFs.sync_directory(directory, path)
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

    fn remove_file(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()> {
        self.before(FsOperation::RemoveFile, path, None)?;
        SystemFs.remove_file(parent, name, path)
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

    fn before_target_publication(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::BeforeTargetPublication, path, None)?;
        let mutation = {
            let mut mutation = self.publication_mutation.lock().expect("mutation lock");
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
