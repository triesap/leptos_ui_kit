use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::CodegenError;

use super::fs::{FsOps, SystemFs};

pub const DEFAULT_KIT_WRITE_LOCK_PATH: &str = "src/components/ui/_kit/.write.lock";

pub struct WriteLock {
    path: PathBuf,
    fs: Arc<dyn FsOps>,
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
        Self::acquire_with(project_root, Arc::new(SystemFs))
    }

    pub(crate) fn acquire_with(
        project_root: &Path,
        fs: Arc<dyn FsOps>,
    ) -> Result<Self, CodegenError> {
        let lock_path = project_root.join(DEFAULT_KIT_WRITE_LOCK_PATH);
        if let Some(parent) = lock_path.parent() {
            fs.create_dir_all(parent)
                .map_err(|source| CodegenError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }

        match fs.create_new_file(&lock_path) {
            Ok(mut file) => {
                fs.write_handle(&mut file, &lock_path, b"locked\n")
                    .map_err(|source| CodegenError::Io {
                        path: lock_path.clone(),
                        source,
                    })?;
                Ok(Self {
                    path: lock_path,
                    fs,
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
        let _ = self.fs.remove_file(&self.path);
    }
}
