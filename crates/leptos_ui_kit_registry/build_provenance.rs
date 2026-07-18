use serde_json::Value;

pub const CANONICAL_REPOSITORY: &str = "github.com/triesap/leptos_ui_kit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvenanceSource {
    Explicit,
    CargoVcs,
    Checkout,
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
pub enum ProvenanceError {
    InvalidRevision {
        source: ProvenanceSource,
        value: String,
    },
    InvalidCargoVcs(String),
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
        let value = serde_json::from_str::<Value>(input)
            .map_err(|error| ProvenanceError::InvalidCargoVcs(error.to_string()))?;
        let rev = value
            .pointer("/git/sha1")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProvenanceError::InvalidCargoVcs(
                    "expected package metadata field git.sha1".to_owned(),
                )
            })?;
        return normalize_revision(ProvenanceSource::CargoVcs, rev).map(|rev| {
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
        Err(ProvenanceError::InvalidRevision {
            source,
            value: value.to_owned(),
        })
    }
}
