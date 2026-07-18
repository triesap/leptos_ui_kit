use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fmt, fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

pub const CANONICAL_REPOSITORY: &str = "github.com/triesap/leptos_ui_kit";
pub const EXPECTED_CRATE_PATH: &str = "crates/leptos_ui_kit_registry";
pub const GIT_REPOSITORY_OVERRIDE_ENV: [&str; 18] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_SYSTEM",
    "GIT_DIR",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NAMESPACE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_WORK_TREE",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvenanceSource {
    Explicit,
    CargoVcs,
    Checkout,
}

impl ProvenanceSource {
    pub const fn as_env_value(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::CargoVcs => "cargo-vcs",
            Self::Checkout => "checkout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProvenance {
    pub rev: String,
    pub source: ProvenanceSource,
}

#[derive(Debug, Clone, Copy)]
pub struct CheckoutProvenance<'a> {
    pub remote: &'a str,
    pub rev: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedCheckoutProvenance {
    pub remote: String,
    pub rev: String,
}

impl OwnedCheckoutProvenance {
    pub fn as_borrowed(&self) -> CheckoutProvenance<'_> {
        CheckoutProvenance {
            remote: &self.remote,
            rev: &self.rev,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutProbe {
    pub checkout: Option<OwnedCheckoutProvenance>,
    pub rerun_paths: BTreeSet<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvenanceError {
    ExplicitEncoding,
    Revision {
        source: ProvenanceSource,
        value: String,
    },
    CargoVcs(String),
}

impl fmt::Display for ProvenanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExplicitEncoding => formatter.write_str(
                "LEPTOS_UI_KIT_GIT_REV must be valid UTF-8 containing a complete 40-character hexadecimal object ID",
            ),
            Self::Revision { source, value } => write!(
                formatter,
                "invalid {} Git revision {value:?}; expected a complete 40-character hexadecimal object ID",
                source.as_env_value()
            ),
            Self::CargoVcs(message) => {
                write!(formatter, "invalid .cargo_vcs_info.json: {message}")
            }
        }
    }
}

impl std::error::Error for ProvenanceError {}

pub trait GitRunner {
    fn output(&mut self, anchor: &Path, args: &[&str]) -> Option<String>;
}

#[derive(Debug, Default)]
pub struct SystemGit;

impl GitRunner for SystemGit {
    fn output(&mut self, anchor: &Path, args: &[&str]) -> Option<String> {
        let mut command = Command::new("git");
        command.arg("-C").arg(anchor).args(args);
        for name in GIT_REPOSITORY_OVERRIDE_ENV {
            command.env_remove(name);
        }
        command
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0");

        let output = command.output().ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout)
            .ok()
            .map(|value| value.trim().to_owned())
    }
}

pub fn explicit_revision(value: Option<&OsStr>) -> Result<Option<&str>, ProvenanceError> {
    value
        .map(|value| value.to_str().ok_or(ProvenanceError::ExplicitEncoding))
        .transpose()
}

pub fn read_cargo_vcs(path: &Path) -> Result<Option<String>, ProvenanceError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ProvenanceError::CargoVcs(format!(
                "failed to inspect {}: {error}",
                path.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ProvenanceError::CargoVcs(format!(
            "{} must be a regular file",
            path.display()
        )));
    }

    let bytes = fs::read(path).map_err(|error| {
        ProvenanceError::CargoVcs(format!("failed to read {}: {error}", path.display()))
    })?;
    String::from_utf8(bytes).map(Some).map_err(|error| {
        ProvenanceError::CargoVcs(format!(
            "{} must contain valid UTF-8: {error}",
            path.display()
        ))
    })
}

pub fn probe_checkout(manifest_dir: &Path, git: &mut impl GitRunner) -> CheckoutProbe {
    let expected_repository_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(manifest_dir);
    let mut rerun_paths = BTreeSet::new();
    record_repository_marker(&expected_repository_root.join(".git"), &mut rerun_paths);

    let Some(repository_root) = git.output(manifest_dir, &["rev-parse", "--show-toplevel"]) else {
        return CheckoutProbe {
            checkout: None,
            rerun_paths,
        };
    };
    let repository_root = PathBuf::from(repository_root);
    record_repository_marker(&repository_root.join(".git"), &mut rerun_paths);
    if !is_expected_repository_layout(manifest_dir, &repository_root) {
        return CheckoutProbe {
            checkout: None,
            rerun_paths,
        };
    }

    record_git_path(&repository_root, git, "config", &mut rerun_paths);
    record_git_path(&repository_root, git, "config.worktree", &mut rerun_paths);
    let Some(remote) = git.output(
        &repository_root,
        &["config", "--local", "--get-all", "remote.origin.url"],
    ) else {
        return CheckoutProbe {
            checkout: None,
            rerun_paths,
        };
    };
    if !is_canonical_repository(&remote) {
        return CheckoutProbe {
            checkout: None,
            rerun_paths,
        };
    }

    record_git_path(&repository_root, git, "HEAD", &mut rerun_paths);
    record_git_path(&repository_root, git, "packed-refs", &mut rerun_paths);
    if let Some(reference) = git.output(&repository_root, &["symbolic-ref", "-q", "HEAD"]) {
        record_git_path(&repository_root, git, &reference, &mut rerun_paths);
    }
    let checkout = git
        .output(
            &repository_root,
            &["rev-parse", "--verify", "HEAD^{commit}"],
        )
        .map(|rev| OwnedCheckoutProvenance { remote, rev });

    CheckoutProbe {
        checkout,
        rerun_paths,
    }
}

fn record_repository_marker(path: &Path, paths: &mut BTreeSet<PathBuf>) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() => {
            paths.insert(path.to_path_buf());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            paths.insert(path.to_path_buf());
        }
        Ok(_) | Err(_) => {}
    }
}

fn record_git_path(
    manifest_dir: &Path,
    git: &mut impl GitRunner,
    name: &str,
    paths: &mut BTreeSet<PathBuf>,
) {
    let args = ["rev-parse", "--git-path", name];
    if let Some(path) = git.output(manifest_dir, &args) {
        let path = PathBuf::from(path);
        paths.insert(if path.is_absolute() {
            path
        } else {
            manifest_dir.join(path)
        });
    }
}

fn is_expected_repository_layout(manifest_dir: &Path, repository_root: &Path) -> bool {
    let Ok(manifest_dir) = fs::canonicalize(manifest_dir) else {
        return false;
    };
    let Ok(expected_manifest_dir) = fs::canonicalize(repository_root.join(EXPECTED_CRATE_PATH))
    else {
        return false;
    };
    manifest_dir == expected_manifest_dir
}

pub fn resolve_provenance(
    explicit: Option<&str>,
    cargo_vcs_json: Option<&str>,
    checkout: Option<CheckoutProvenance<'_>>,
) -> Result<Option<ResolvedProvenance>, ProvenanceError> {
    if let Some(rev) = explicit {
        return normalize_revision(ProvenanceSource::Explicit, rev).map(|rev| {
            Some(ResolvedProvenance {
                rev,
                source: ProvenanceSource::Explicit,
            })
        });
    }

    if let Some(input) = cargo_vcs_json {
        return resolve_cargo_vcs(input).map(|rev| {
            Some(ResolvedProvenance {
                rev,
                source: ProvenanceSource::CargoVcs,
            })
        });
    }

    if let Some(checkout) = checkout
        && is_canonical_repository(checkout.remote)
    {
        return normalize_revision(ProvenanceSource::Checkout, checkout.rev).map(|rev| {
            Some(ResolvedProvenance {
                rev,
                source: ProvenanceSource::Checkout,
            })
        });
    }

    Ok(None)
}

fn resolve_cargo_vcs(input: &str) -> Result<String, ProvenanceError> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| ProvenanceError::CargoVcs(error.to_string()))?;
    let path_in_vcs = value
        .get("path_in_vcs")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProvenanceError::CargoVcs(
                "expected package metadata field path_in_vcs to be a string".to_owned(),
            )
        })?;
    if path_in_vcs != EXPECTED_CRATE_PATH {
        return Err(ProvenanceError::CargoVcs(format!(
            "expected package metadata field path_in_vcs to equal {EXPECTED_CRATE_PATH:?}, got {path_in_vcs:?}"
        )));
    }

    match value.pointer("/git/dirty") {
        None | Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => {
            return Err(ProvenanceError::CargoVcs(
                "package metadata field git.dirty must not be true".to_owned(),
            ));
        }
        Some(_) => {
            return Err(ProvenanceError::CargoVcs(
                "package metadata field git.dirty must be a boolean when present".to_owned(),
            ));
        }
    }

    let rev = value
        .pointer("/git/sha1")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProvenanceError::CargoVcs("expected package metadata field git.sha1".to_owned())
        })?;
    normalize_revision(ProvenanceSource::CargoVcs, rev)
}

pub fn is_canonical_repository(remote: &str) -> bool {
    let remote = remote.trim().trim_end_matches('/').to_ascii_lowercase();
    if let Some(path) = remote
        .strip_prefix("https://")
        .or_else(|| remote.strip_prefix("ssh://git@"))
    {
        return strip_single_git_suffix(path).is_some_and(|path| path == CANONICAL_REPOSITORY);
    }

    remote
        .strip_prefix("git@github.com:")
        .and_then(strip_single_git_suffix)
        .is_some_and(|path| path == "triesap/leptos_ui_kit")
}

fn strip_single_git_suffix(path: &str) -> Option<&str> {
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!path.ends_with(".git")).then_some(path)
}

pub fn normalize_revision(
    source: ProvenanceSource,
    value: &str,
) -> Result<String, ProvenanceError> {
    let value = value.trim();
    if value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err(ProvenanceError::Revision {
            source,
            value: value.to_owned(),
        })
    }
}
