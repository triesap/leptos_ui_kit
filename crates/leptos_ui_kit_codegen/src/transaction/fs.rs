use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    panic::{RefUnwindSafe, UnwindSafe},
    path::Path,
};

#[cfg(test)]
use std::path::PathBuf;

pub(crate) trait FsOps: fmt::Debug + Send + Sync + UnwindSafe + RefUnwindSafe {
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn create_new_file(&self, path: &Path) -> io::Result<File>;
    fn write_handle(&self, file: &mut File, path: &Path, content: &[u8]) -> io::Result<()>;
    fn write_file(&self, path: &Path, content: &[u8]) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub(super) struct SystemFs;

impl FsOps for SystemFs {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn create_new_file(&self, path: &Path) -> io::Result<File> {
        OpenOptions::new().create_new(true).write(true).open(path)
    }

    fn write_handle(&self, file: &mut File, _path: &Path, content: &[u8]) -> io::Result<()> {
        file.write_all(content)
    }

    fn write_file(&self, path: &Path, content: &[u8]) -> io::Result<()> {
        fs::write(path, content)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsOperation {
    CreateDirAll,
    CreateNewFile,
    WriteHandle,
    WriteFile,
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
pub(crate) struct FaultFs {
    fail: std::sync::Mutex<Option<(FsOperation, usize)>>,
    counts: std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    events: std::sync::Mutex<Vec<FsEvent>>,
}

#[cfg(test)]
impl FaultFs {
    pub(crate) fn passthrough() -> Self {
        Self {
            fail: std::sync::Mutex::new(None),
            counts: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            events: std::sync::Mutex::new(Vec::new()),
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

    fn create_new_file(&self, path: &Path) -> io::Result<File> {
        self.before(FsOperation::CreateNewFile, path, None)?;
        SystemFs.create_new_file(path)
    }

    fn write_handle(&self, file: &mut File, path: &Path, content: &[u8]) -> io::Result<()> {
        self.before(FsOperation::WriteHandle, path, None)?;
        SystemFs.write_handle(file, path, content)
    }

    fn write_file(&self, path: &Path, content: &[u8]) -> io::Result<()> {
        self.before(FsOperation::WriteFile, path, None)?;
        SystemFs.write_file(path, content)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.before(FsOperation::Rename, from, Some(to))?;
        SystemFs.rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.before(FsOperation::RemoveFile, path, None)?;
        SystemFs.remove_file(path)
    }
}
