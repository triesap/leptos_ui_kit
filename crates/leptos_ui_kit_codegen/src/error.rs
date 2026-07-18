use std::{fmt, path::PathBuf};

use leptos_ui_kit_registry::{ConfigError, RegistryError};

#[derive(Debug)]
pub enum CodegenError {
    Io {
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
    LockExists(PathBuf),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "failed to read {}: {source}", path.display()),
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
            Self::LockExists(path) => write!(f, "write lock already exists: {}", path.display()),
        }
    }
}

impl std::error::Error for CodegenError {}

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
