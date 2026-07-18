mod fs;
mod lock;
mod replace;

pub use lock::{DEFAULT_KIT_WRITE_LOCK_PATH, WriteLock};
pub(crate) use replace::apply_planned_files;
pub use replace::write_file_atomic;

#[cfg(test)]
pub(crate) use fs::{FaultFs, FsEvent, FsOperation};
#[cfg(test)]
pub(crate) use replace::apply_planned_files_with;
