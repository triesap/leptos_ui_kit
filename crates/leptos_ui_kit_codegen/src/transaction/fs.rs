use std::{
    fmt,
    io::{self, Write},
    panic::{RefUnwindSafe, UnwindSafe},
    path::Path,
};

#[cfg(test)]
use std::fs;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, File, OpenOptions};

#[cfg(test)]
use std::path::PathBuf;

pub(crate) trait FsOps: fmt::Debug + Send + Sync + UnwindSafe + RefUnwindSafe {
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn create_new_file(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<File>;
    fn write_handle(&self, file: &mut File, path: &Path, content: &[u8]) -> io::Result<()>;
    fn write_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        content: &[u8],
    ) -> io::Result<File>;
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
    fn remove_file(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub(super) struct SystemFs;

impl FsOps for SystemFs {
    fn create_dir_all(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn create_new_file(&self, parent: &Dir, name: &Path, _path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        options.follow(FollowSymlinks::No);
        options.nonblock(true);
        let file = parent.open_with(name, &options)?;
        ensure_opened_regular_file(&file)?;
        Ok(file)
    }

    fn write_handle(&self, file: &mut File, _path: &Path, content: &[u8]) -> io::Result<()> {
        file.write_all(content)
    }

    fn write_file(
        &self,
        parent: &Dir,
        name: &Path,
        _path: &Path,
        content: &[u8],
    ) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.create(true).write(true).truncate(true);
        options.follow(FollowSymlinks::No);
        options.nonblock(true);
        let mut file = parent.open_with(name, &options)?;
        ensure_opened_regular_file(&file)?;
        file.write_all(content)?;
        Ok(file)
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

    fn remove_file(&self, parent: &Dir, name: &Path, _path: &Path) -> io::Result<()> {
        parent.remove_file(name)
    }
}

fn ensure_opened_regular_file(file: &File) -> io::Result<()> {
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
    Ok(())
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsOperation {
    CreateDirAll,
    CreateNewFile,
    WriteHandle,
    WriteFile,
    BeforeFinalRevalidation,
    AfterFinalRevalidation,
    Rename,
    RemoveFile,
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
    fail: std::sync::Mutex<Option<(FsOperation, usize)>>,
    counts: std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    events: std::sync::Mutex<Vec<FsEvent>>,
    final_revalidation_mutation: std::sync::Mutex<Option<FinalRevalidationMutation>>,
    post_revalidation_mutation: std::sync::Mutex<Option<FinalRevalidationMutation>>,
}

#[cfg(test)]
impl FaultFs {
    pub(crate) fn passthrough() -> Self {
        Self {
            fail: std::sync::Mutex::new(None),
            counts: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            events: std::sync::Mutex::new(Vec::new()),
            final_revalidation_mutation: std::sync::Mutex::new(None),
            post_revalidation_mutation: std::sync::Mutex::new(None),
        }
    }

    pub(crate) fn fail_nth(operation: FsOperation, ordinal: usize) -> Self {
        assert!(ordinal > 0, "fault ordinal is one-based");
        let fs = Self::passthrough();
        *fs.fail.lock().expect("fault lock") = Some((operation, ordinal));
        fs
    }

    pub(crate) fn events(&self) -> Vec<FsEvent> {
        self.events.lock().expect("event lock").clone()
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
            .is_some_and(|(target, target_ordinal)| {
                target == operation && target_ordinal == ordinal
            })
        {
            Err(io::Error::other(format!(
                "injected {operation:?} failure at ordinal {ordinal}"
            )))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
impl FsOps for FaultFs {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::CreateDirAll, path, None)?;
        SystemFs.create_dir_all(path)
    }

    fn create_new_file(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<File> {
        self.before(FsOperation::CreateNewFile, path, None)?;
        SystemFs.create_new_file(parent, name, path)
    }

    fn write_handle(&self, file: &mut File, path: &Path, content: &[u8]) -> io::Result<()> {
        self.before(FsOperation::WriteHandle, path, None)?;
        SystemFs.write_handle(file, path, content)
    }

    fn write_file(
        &self,
        parent: &Dir,
        name: &Path,
        path: &Path,
        content: &[u8],
    ) -> io::Result<File> {
        self.before(FsOperation::WriteFile, path, None)?;
        SystemFs.write_file(parent, name, path, content)
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

    fn remove_file(&self, parent: &Dir, name: &Path, path: &Path) -> io::Result<()> {
        self.before(FsOperation::RemoveFile, path, None)?;
        SystemFs.remove_file(parent, name, path)
    }
}
