mod fs;
mod journal;
mod lock;
mod replace;
mod runtime;

pub(crate) use lock::DEFAULT_KIT_COORDINATION_IGNORE_PATH;
pub use lock::{DEFAULT_KIT_WRITE_LOCK_PATH, WriteLock};
#[cfg(test)]
pub(crate) use lock::{KIT_ADVISORY_LOCK_CONTENT, KIT_COORDINATION_IGNORE_CONTENT};
pub(crate) use replace::{apply_planned_files_locked, recover_pending_locked};
pub use replace::{check_pending_recovery, write_file_atomic};

#[cfg(test)]
pub(crate) use fs::{FaultFs, FsEvent, FsOperation};
#[cfg(test)]
pub(crate) use replace::{apply_planned_files_with, apply_planned_files_with_snapshot};
