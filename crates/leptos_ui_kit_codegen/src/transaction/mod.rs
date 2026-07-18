mod fs;
mod lock;
mod replace;

pub(crate) use lock::DEFAULT_KIT_COORDINATION_IGNORE_PATH;
pub use lock::{DEFAULT_KIT_WRITE_LOCK_PATH, WriteLock};
#[cfg(test)]
pub(crate) use lock::{KIT_ADVISORY_LOCK_CONTENT, KIT_COORDINATION_IGNORE_CONTENT};
pub(crate) use replace::apply_planned_files_locked;
pub use replace::write_file_atomic;

#[cfg(test)]
pub(crate) use fs::{FaultFs, FsEvent, FsOperation};
#[cfg(test)]
pub(crate) use replace::{apply_planned_files_with, apply_planned_files_with_snapshot};
