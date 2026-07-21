use std::{fmt, path::PathBuf};

use leptos_ui_kit_registry::{ConfigError, RegistryError};

#[derive(Debug)]
pub enum CodegenError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    FilesystemOperation {
        operation: &'static str,
        logical_path: String,
        path: PathBuf,
        source: std::io::Error,
    },
    Config(ConfigError),
    Registry(RegistryError),
    LockParse {
        path: PathBuf,
        source: serde_json::Error,
    },
    LockSerialize(serde_json::Error),
    InvalidLock {
        path: PathBuf,
        reason: String,
    },
    UnsafePatch {
        path: PathBuf,
        reason: String,
    },
    UnsafePath {
        path: String,
        reason: String,
    },
    PreimageConflict {
        path: String,
        reason: String,
    },
    ProjectRootChanged {
        path: PathBuf,
        reason: String,
    },
    DuplicatePath(String),
    WriteLockContended {
        path: String,
    },
    LegacyWriteLock {
        path: String,
    },
    InvalidCoordinationState {
        path: String,
        reason: String,
    },
    RecoveryRequired {
        journal_path: PathBuf,
        reason: String,
    },
    LockExists(PathBuf),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    f,
                    "filesystem access failed for {}: {source}",
                    path.display()
                )
            }
            Self::FilesystemOperation {
                operation,
                logical_path,
                source,
                ..
            } => write!(
                f,
                "filesystem operation {operation} failed for project path {logical_path}: {source}"
            ),
            Self::Config(error) => write!(f, "{error}"),
            Self::Registry(error) => write!(f, "{error}"),
            Self::LockParse { path, source } => {
                write!(f, "failed to parse {}: {source}", path.display())
            }
            Self::LockSerialize(error) => write!(f, "failed to serialize lock: {error}"),
            Self::InvalidLock { path, reason } => {
                write!(f, "invalid {}: {reason}", path.display())
            }
            Self::UnsafePatch { path, reason } => {
                write!(f, "cannot safely patch {}: {reason}", path.display())
            }
            Self::UnsafePath { path, reason } => {
                write!(f, "unsafe write path {path}: {reason}")
            }
            Self::PreimageConflict { path, reason } => {
                write!(f, "project path changed after planning at {path}: {reason}")
            }
            Self::ProjectRootChanged { path, reason } => {
                write!(f, "project root changed at {}: {reason}", path.display())
            }
            Self::DuplicatePath(path) => write!(f, "duplicate planned write path: {path}"),
            Self::WriteLockContended { path } => {
                write!(f, "project write lock is already held at {path}")
            }
            Self::LegacyWriteLock { path } => write!(
                f,
                "legacy write lock exists at {path}; verify no older leptos_ui_kit process is running, remove the file manually, and retry"
            ),
            Self::InvalidCoordinationState { path, reason } => {
                write!(
                    f,
                    "invalid installer coordination state at {path}: {reason}; verify no leptos_ui_kit process is running, inspect and repair or remove the entry manually, and retry"
                )
            }
            Self::RecoveryRequired {
                journal_path,
                reason,
            } => write!(
                f,
                "transaction recovery is required at {}: {reason}",
                journal_path.display()
            ),
            Self::LockExists(path) => write!(f, "write lock already exists: {}", path.display()),
        }
    }
}

impl std::error::Error for CodegenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::FilesystemOperation { source, .. } => Some(source),
            Self::LockParse { source, .. } => Some(source),
            Self::Config(source) => Some(source),
            Self::Registry(source) => Some(source),
            Self::LockSerialize(source) => Some(source),
            Self::InvalidLock { .. }
            | Self::UnsafePatch { .. }
            | Self::UnsafePath { .. }
            | Self::PreimageConflict { .. }
            | Self::ProjectRootChanged { .. }
            | Self::DuplicatePath(_)
            | Self::WriteLockContended { .. }
            | Self::LegacyWriteLock { .. }
            | Self::InvalidCoordinationState { .. }
            | Self::RecoveryRequired { .. }
            | Self::LockExists(_) => None,
        }
    }
}

impl From<ConfigError> for CodegenError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

impl From<RegistryError> for CodegenError {
    fn from(value: RegistryError) -> Self {
        Self::Registry(value)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        io,
        path::{Path, PathBuf},
    };

    use super::CodegenError;

    #[test]
    fn legacy_io_diagnostic_does_not_mislabel_every_operation_as_a_read() {
        let error = CodegenError::Io {
            path: PathBuf::from("styles/kit.css"),
            source: io::Error::other("simulated operation failure"),
        };
        let message = error.to_string();

        assert!(message.contains("filesystem access failed"));
        assert!(!message.contains("failed to read"));
        assert!(error.source().is_some());
    }

    #[test]
    fn operation_diagnostic_exposes_only_the_logical_path_in_display() {
        let physical = Path::new("/private/build/root/styles/kit.css");
        let error = CodegenError::FilesystemOperation {
            operation: "replace target",
            logical_path: "styles/kit.css".to_owned(),
            path: physical.to_path_buf(),
            source: io::Error::other("simulated replacement failure"),
        };
        let message = error.to_string();

        assert!(message.contains("replace target"));
        assert!(message.contains("styles/kit.css"));
        assert!(!message.contains("/private/build/root"));
        assert!(error.source().is_some());
    }
}
