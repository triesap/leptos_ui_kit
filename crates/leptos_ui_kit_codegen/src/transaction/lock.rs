use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::CodegenError;
use crate::path_safety::PlanningContext;

use super::fs::{FsOps, SystemFs};

pub const DEFAULT_KIT_WRITE_LOCK_PATH: &str = "src/components/ui/_kit/.write.lock";

pub struct WriteLock {
    path: PathBuf,
    fs: Arc<dyn FsOps>,
    identity: LockIdentity,
    parent: cap_std::fs::Dir,
    name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LockIdentity {
    device: u64,
    inode: u64,
}

impl std::fmt::Debug for WriteLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WriteLock")
            .field("path", &self.path)
            .finish()
    }
}

impl WriteLock {
    pub fn acquire(project_root: &Path) -> Result<Self, CodegenError> {
        let context = PlanningContext::open(project_root)?;
        Self::acquire_with_context(&context, Arc::new(SystemFs))
    }

    #[cfg(test)]
    pub(crate) fn acquire_with(
        project_root: &Path,
        fs: Arc<dyn FsOps>,
    ) -> Result<Self, CodegenError> {
        let context = PlanningContext::open(project_root)?;
        Self::acquire_with_context(&context, fs)
    }

    pub(crate) fn acquire_with_context(
        context: &PlanningContext,
        fs: Arc<dyn FsOps>,
    ) -> Result<Self, CodegenError> {
        let lock_path = context.project_root().join(DEFAULT_KIT_WRITE_LOCK_PATH);
        context.ensure_parent(DEFAULT_KIT_WRITE_LOCK_PATH)?;
        context.validate_auxiliary_path(DEFAULT_KIT_WRITE_LOCK_PATH)?;
        let (lock_parent, lock_name) = context.open_parent(DEFAULT_KIT_WRITE_LOCK_PATH)?;
        if let Some(parent) = lock_path.parent() {
            fs.create_dir_all(parent)
                .map_err(|source| CodegenError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }

        match fs.create_new_file(&lock_parent, Path::new(&lock_name), &lock_path) {
            Ok(mut file) => {
                fs.write_handle(&mut file, &lock_path, b"locked\n")
                    .map_err(|source| CodegenError::Io {
                        path: lock_path.clone(),
                        source,
                    })?;
                let identity = LockIdentity::from_metadata(&file.metadata().map_err(|source| {
                    CodegenError::Io {
                        path: lock_path.clone(),
                        source,
                    }
                })?);
                Ok(Self {
                    path: lock_path,
                    fs,
                    identity,
                    parent: lock_parent,
                    name: lock_name,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(CodegenError::LockExists(lock_path))
            }
            Err(source) => Err(CodegenError::Io {
                path: lock_path,
                source,
            }),
        }
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let matches = self
            .parent
            .symlink_metadata(Path::new(&self.name))
            .ok()
            .filter(safe_regular_file)
            .is_some_and(|metadata| LockIdentity::from_metadata(&metadata) == self.identity);
        if matches {
            let _ = self
                .fs
                .remove_file(&self.parent, Path::new(&self.name), &self.path);
        }
    }
}

impl LockIdentity {
    fn from_metadata(metadata: &cap_std::fs::Metadata) -> Self {
        Self {
            device: cap_fs_ext::MetadataExt::dev(metadata),
            inode: cap_fs_ext::MetadataExt::ino(metadata),
        }
    }
}

fn safe_regular_file(metadata: &cap_std::fs::Metadata) -> bool {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    if cap_fs_ext::OsMetadataExt::file_attributes(metadata) & 0x0000_0400 != 0 {
        return false;
    }
    true
}
