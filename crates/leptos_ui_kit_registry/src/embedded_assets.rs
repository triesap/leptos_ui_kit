use std::{fmt, str::Utf8Error};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmbeddedAssetKind {
    Json,
    Rust,
    Css,
}

impl fmt::Display for EmbeddedAssetKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json => formatter.write_str("JSON"),
            Self::Rust => formatter.write_str("Rust"),
            Self::Css => formatter.write_str("CSS"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EmbeddedAsset {
    pub logical_path: &'static str,
    pub kind: EmbeddedAssetKind,
    pub content: &'static [u8],
    pub content_hash: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));

/// An immutable borrowed view of one logical catalog asset.
///
/// The view never exposes an authoring-tree or build-machine path. Production
/// views borrow the bytes emitted into the binary by the registry build.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AssetView<'a> {
    logical_path: &'a str,
    kind: EmbeddedAssetKind,
    content: &'a [u8],
    content_hash: &'a str,
}

impl<'a> AssetView<'a> {
    pub(crate) const fn logical_path(self) -> &'a str {
        self.logical_path
    }

    pub(crate) const fn kind(self) -> EmbeddedAssetKind {
        self.kind
    }

    #[cfg(test)]
    pub(crate) const fn content(self) -> &'a [u8] {
        self.content
    }

    #[cfg(test)]
    pub(crate) const fn content_hash(self) -> &'a str {
        self.content_hash
    }

    pub(crate) fn utf8(self) -> Result<&'a str, AssetProviderError> {
        std::str::from_utf8(self.content).map_err(|source| AssetProviderError::NonUtf8 {
            logical_path: self.logical_path.to_owned(),
            source,
        })
    }

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "point lookup is retained for the injected provider seam"
        )
    )]
    fn expect_kind(self, expected: EmbeddedAssetKind) -> Result<Self, AssetProviderError> {
        if self.kind == expected {
            Ok(self)
        } else {
            Err(AssetProviderError::KindMismatch {
                logical_path: self.logical_path.to_owned(),
                expected,
                actual: self.kind,
            })
        }
    }
}

/// A logical catalog lookup or validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "all fault variants are exercised through the injected provider seam"
    )
)]
pub(crate) enum AssetProviderError {
    InvalidLogicalPath {
        logical_path: String,
        reason: &'static str,
    },
    Missing {
        logical_path: String,
    },
    KindMismatch {
        logical_path: String,
        expected: EmbeddedAssetKind,
        actual: EmbeddedAssetKind,
    },
    NonUtf8 {
        logical_path: String,
        source: Utf8Error,
    },
    HashMismatch {
        logical_path: String,
        expected: String,
        actual: String,
    },
}

impl fmt::Display for AssetProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLogicalPath {
                logical_path,
                reason,
            } => write!(
                formatter,
                "invalid logical asset path {logical_path:?}: {reason}"
            ),
            Self::Missing { logical_path } => {
                write!(formatter, "embedded asset is missing: {logical_path}")
            }
            Self::KindMismatch {
                logical_path,
                expected,
                actual,
            } => write!(
                formatter,
                "embedded asset {logical_path} has kind {actual}, expected {expected}"
            ),
            Self::NonUtf8 { logical_path, .. } => {
                write!(formatter, "embedded asset is not UTF-8: {logical_path}")
            }
            Self::HashMismatch {
                logical_path,
                expected,
                actual,
            } => write!(
                formatter,
                "embedded asset {logical_path} has content hash {actual}, expected {expected}"
            ),
        }
    }
}

impl std::error::Error for AssetProviderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NonUtf8 { source, .. } => Some(source),
            Self::InvalidLogicalPath { .. }
            | Self::Missing { .. }
            | Self::KindMismatch { .. }
            | Self::HashMismatch { .. } => None,
        }
    }
}

pub(crate) type AssetIter<'a> =
    Box<dyn Iterator<Item = Result<AssetView<'a>, AssetProviderError>> + 'a>;

/// Supplies immutable assets by stable logical path.
///
/// Implementations enumerate assets in strictly increasing logical-path order.
/// Lookup and enumeration validate each declared content hash. Callers that
/// consume source text should use `utf8_asset`, which additionally verifies the
/// expected asset kind and UTF-8 encoding.
pub(crate) trait AssetProvider {
    fn asset_count(&self) -> usize;

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "point lookup is retained for the injected provider seam"
        )
    )]
    fn asset(&self, logical_path: &str) -> Result<AssetView<'_>, AssetProviderError>;

    fn assets(&self) -> AssetIter<'_>;

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "point lookup is retained for the injected provider seam"
        )
    )]
    fn asset_of_kind(
        &self,
        logical_path: &str,
        expected: EmbeddedAssetKind,
    ) -> Result<AssetView<'_>, AssetProviderError> {
        self.asset(logical_path)?.expect_kind(expected)
    }

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "point lookup is retained for the injected provider seam"
        )
    )]
    fn utf8_asset(
        &self,
        logical_path: &str,
        expected: EmbeddedAssetKind,
    ) -> Result<&str, AssetProviderError> {
        self.asset_of_kind(logical_path, expected)?.utf8()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct EmbeddedAssetProvider;

static EMBEDDED_ASSET_PROVIDER: EmbeddedAssetProvider = EmbeddedAssetProvider;

pub(crate) fn embedded_asset_provider() -> &'static EmbeddedAssetProvider {
    debug_assert!(EMBEDDED_CATALOG_HASH.starts_with("sha256:"));
    &EMBEDDED_ASSET_PROVIDER
}

pub(crate) fn embedded_asset_inventory()
-> impl ExactSizeIterator<Item = (&'static str, EmbeddedAssetKind)> {
    EMBEDDED_ASSETS
        .iter()
        .map(|asset| (asset.logical_path, asset.kind))
}

impl AssetProvider for EmbeddedAssetProvider {
    fn asset_count(&self) -> usize {
        EMBEDDED_ASSET_COUNT
    }

    fn asset(&self, logical_path: &str) -> Result<AssetView<'_>, AssetProviderError> {
        validate_logical_path(logical_path)?;
        let asset = EMBEDDED_ASSETS
            .binary_search_by(|asset| asset.logical_path.cmp(logical_path))
            .ok()
            .map(|index| &EMBEDDED_ASSETS[index])
            .ok_or_else(|| AssetProviderError::Missing {
                logical_path: logical_path.to_owned(),
            })?;
        validate_asset(asset_view(asset))
    }

    fn assets(&self) -> AssetIter<'_> {
        Box::new(EMBEDDED_ASSETS.iter().map(asset_view).map(validate_asset))
    }
}

fn asset_view(asset: &EmbeddedAsset) -> AssetView<'_> {
    AssetView {
        logical_path: asset.logical_path,
        kind: asset.kind,
        content: asset.content,
        content_hash: asset.content_hash,
    }
}

fn validate_asset(asset: AssetView<'_>) -> Result<AssetView<'_>, AssetProviderError> {
    let actual = sha256(asset.content);
    if actual == asset.content_hash {
        Ok(asset)
    } else {
        Err(AssetProviderError::HashMismatch {
            logical_path: asset.logical_path.to_owned(),
            expected: asset.content_hash.to_owned(),
            actual,
        })
    }
}

fn sha256(content: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(content))
}

pub(crate) fn validate_logical_path(logical_path: &str) -> Result<(), AssetProviderError> {
    if logical_path.is_empty() || logical_path.starts_with('/') || logical_path.contains('\\') {
        return Err(invalid_logical_path(
            logical_path,
            "expected a non-empty forward-slash relative path",
        ));
    }
    for segment in logical_path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.starts_with('.') {
            return Err(invalid_logical_path(
                logical_path,
                "contains an empty, dot, parent, or hidden segment",
            ));
        }
        if segment.ends_with('.') || segment.ends_with(' ') {
            return Err(invalid_logical_path(
                logical_path,
                "contains a segment ending in a dot or space",
            ));
        }
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(invalid_logical_path(
                logical_path,
                "contains a non-portable character",
            ));
        }
    }
    Ok(())
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "point lookup is retained for the injected provider seam"
    )
)]
fn invalid_logical_path(logical_path: &str, reason: &'static str) -> AssetProviderError {
    AssetProviderError::InvalidLogicalPath {
        logical_path: logical_path.to_owned(),
        reason,
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct InMemoryAsset {
    logical_path: String,
    kind: EmbeddedAssetKind,
    content: Vec<u8>,
    content_hash: String,
}

#[cfg(test)]
impl InMemoryAsset {
    fn view(&self) -> AssetView<'_> {
        AssetView {
            logical_path: &self.logical_path,
            kind: self.kind,
            content: &self.content,
            content_hash: &self.content_hash,
        }
    }
}

/// Mutable test fixture for exercising consumers through the immutable
/// `AssetProvider` interface. Production code cannot construct this provider.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct InMemoryAssetProvider {
    assets: Vec<InMemoryAsset>,
}

#[cfg(test)]
impl InMemoryAssetProvider {
    pub(crate) fn from_embedded() -> Self {
        Self {
            assets: EMBEDDED_ASSETS
                .iter()
                .map(|asset| InMemoryAsset {
                    logical_path: asset.logical_path.to_owned(),
                    kind: asset.kind,
                    content: asset.content.to_vec(),
                    content_hash: asset.content_hash.to_owned(),
                })
                .collect(),
        }
    }

    pub(crate) fn insert(
        &mut self,
        logical_path: impl Into<String>,
        kind: EmbeddedAssetKind,
        content: impl Into<Vec<u8>>,
    ) -> Result<(), AssetProviderError> {
        let logical_path = logical_path.into();
        validate_logical_path(&logical_path)?;
        let content = content.into();
        let asset = InMemoryAsset {
            content_hash: sha256(&content),
            logical_path,
            kind,
            content,
        };
        match self
            .assets
            .binary_search_by(|current| current.logical_path.cmp(&asset.logical_path))
        {
            Ok(index) => self.assets[index] = asset,
            Err(index) => self.assets.insert(index, asset),
        }
        Ok(())
    }

    pub(crate) fn remove(&mut self, logical_path: &str) -> Result<(), AssetProviderError> {
        let index = self.asset_index(logical_path)?;
        self.assets.remove(index);
        Ok(())
    }

    pub(crate) fn set_bytes(
        &mut self,
        logical_path: &str,
        content: impl Into<Vec<u8>>,
    ) -> Result<(), AssetProviderError> {
        let index = self.asset_index(logical_path)?;
        let content = content.into();
        self.assets[index].content_hash = sha256(&content);
        self.assets[index].content = content;
        Ok(())
    }

    pub(crate) fn set_kind(
        &mut self,
        logical_path: &str,
        kind: EmbeddedAssetKind,
    ) -> Result<(), AssetProviderError> {
        let index = self.asset_index(logical_path)?;
        self.assets[index].kind = kind;
        Ok(())
    }

    pub(crate) fn set_declared_hash(
        &mut self,
        logical_path: &str,
        content_hash: impl Into<String>,
    ) -> Result<(), AssetProviderError> {
        let index = self.asset_index(logical_path)?;
        self.assets[index].content_hash = content_hash.into();
        Ok(())
    }

    fn asset_index(&self, logical_path: &str) -> Result<usize, AssetProviderError> {
        validate_logical_path(logical_path)?;
        self.assets
            .binary_search_by(|asset| asset.logical_path.as_str().cmp(logical_path))
            .map_err(|_| AssetProviderError::Missing {
                logical_path: logical_path.to_owned(),
            })
    }
}

#[cfg(test)]
impl AssetProvider for InMemoryAssetProvider {
    fn asset_count(&self) -> usize {
        self.assets.len()
    }

    fn asset(&self, logical_path: &str) -> Result<AssetView<'_>, AssetProviderError> {
        let index = self.asset_index(logical_path)?;
        validate_asset(self.assets[index].view())
    }

    fn assets(&self) -> AssetIter<'_> {
        Box::new(
            self.assets
                .iter()
                .map(InMemoryAsset::view)
                .map(validate_asset),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        AssetProvider, AssetProviderError, EMBEDDED_ASSET_COUNT, EMBEDDED_ASSETS,
        EMBEDDED_CATALOG_HASH, EmbeddedAssetKind, InMemoryAssetProvider, embedded_asset_provider,
    };

    #[test]
    fn generated_catalog_is_exact_sorted_and_content_addressed() {
        assert_eq!(EMBEDDED_ASSET_COUNT, 72);
        assert_eq!(EMBEDDED_ASSETS.len(), EMBEDDED_ASSET_COUNT);
        assert!(EMBEDDED_CATALOG_HASH.starts_with("sha256:"));
        assert_eq!(EMBEDDED_CATALOG_HASH.len(), "sha256:".len() + 64);

        let provider = embedded_asset_provider();
        assert_eq!(provider.asset_count(), EMBEDDED_ASSET_COUNT);
        let assets = provider
            .assets()
            .collect::<Result<Vec<_>, _>>()
            .expect("valid embedded catalog");
        let paths = assets
            .iter()
            .map(|asset| asset.logical_path())
            .collect::<Vec<_>>();
        assert!(paths.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            paths.iter().copied().collect::<BTreeSet<_>>().len(),
            EMBEDDED_ASSET_COUNT
        );
        assert_eq!(
            paths
                .iter()
                .map(|path| path.to_ascii_lowercase())
                .collect::<BTreeSet<_>>()
                .len(),
            EMBEDDED_ASSET_COUNT
        );

        let mut kinds = [0_usize; 3];
        for asset in assets {
            assert!(!asset.content().is_empty(), "{}", asset.logical_path());
            asset
                .utf8()
                .unwrap_or_else(|error| panic!("{}: {error}", asset.logical_path()));
            assert!(asset.content_hash().starts_with("sha256:"));
            match asset.kind() {
                EmbeddedAssetKind::Json => {
                    kinds[0] += 1;
                    serde_json::from_slice::<serde_json::Value>(asset.content()).unwrap_or_else(
                        |error| panic!("{} is not valid JSON: {error}", asset.logical_path()),
                    );
                }
                EmbeddedAssetKind::Rust => kinds[1] += 1,
                EmbeddedAssetKind::Css => kinds[2] += 1,
            }
        }
        assert_eq!(kinds, [20, 42, 10]);
    }

    #[test]
    fn production_lookup_borrows_the_generated_bytes() {
        let provider = embedded_asset_provider();
        let asset = provider
            .utf8_asset("registry/ui/button.rs", EmbeddedAssetKind::Rust)
            .expect("embedded Rust source");
        let generated = EMBEDDED_ASSETS
            .iter()
            .find(|asset| asset.logical_path == "registry/ui/button.rs")
            .expect("generated Rust source");

        assert_eq!(asset.as_ptr(), generated.content.as_ptr());
        assert_eq!(asset.len(), generated.content.len());
    }

    #[test]
    fn lookup_rejects_missing_unsafe_and_wrong_kind_paths() {
        let provider = embedded_asset_provider();
        assert!(matches!(
            provider.asset("registry/ui/missing.rs"),
            Err(AssetProviderError::Missing { logical_path })
                if logical_path == "registry/ui/missing.rs"
        ));
        assert!(matches!(
            provider.asset("../registry/ui/button.rs"),
            Err(AssetProviderError::InvalidLogicalPath { logical_path, .. })
                if logical_path == "../registry/ui/button.rs"
        ));
        assert!(matches!(
            provider.utf8_asset("registry/ui/button.rs", EmbeddedAssetKind::Css),
            Err(AssetProviderError::KindMismatch {
                logical_path,
                expected: EmbeddedAssetKind::Css,
                actual: EmbeddedAssetKind::Rust,
            }) if logical_path == "registry/ui/button.rs"
        ));
    }

    #[test]
    fn in_memory_provider_injects_missing_bytes_kind_and_hash_faults() {
        let mut provider = InMemoryAssetProvider::from_embedded();
        provider
            .remove("registry/ui/button.rs")
            .expect("remove asset");
        assert!(matches!(
            provider.asset("registry/ui/button.rs"),
            Err(AssetProviderError::Missing { logical_path })
                if logical_path == "registry/ui/button.rs"
        ));

        provider
            .set_bytes("registry/ui/button.json", [0xff])
            .expect("replace bytes");
        assert!(matches!(
            provider.utf8_asset("registry/ui/button.json", EmbeddedAssetKind::Json),
            Err(AssetProviderError::NonUtf8 { logical_path, .. })
                if logical_path == "registry/ui/button.json"
        ));

        provider
            .set_kind("registry/styles/button.css", EmbeddedAssetKind::Rust)
            .expect("replace kind");
        assert!(matches!(
            provider.utf8_asset("registry/styles/button.css", EmbeddedAssetKind::Css),
            Err(AssetProviderError::KindMismatch {
                logical_path,
                expected: EmbeddedAssetKind::Css,
                actual: EmbeddedAssetKind::Rust,
            }) if logical_path == "registry/styles/button.css"
        ));

        provider
            .set_declared_hash("registry/registry.json", "sha256:incorrect")
            .expect("replace hash");
        assert!(matches!(
            provider.asset("registry/registry.json"),
            Err(AssetProviderError::HashMismatch { logical_path, .. })
                if logical_path == "registry/registry.json"
        ));
    }

    #[test]
    fn in_memory_provider_enumerates_exactly_and_deterministically() {
        let mut provider = InMemoryAssetProvider::default();
        provider
            .insert("schema/z.json", EmbeddedAssetKind::Json, b"{}")
            .expect("insert z");
        provider
            .insert("registry/a.rs", EmbeddedAssetKind::Rust, b"source")
            .expect("insert a");
        provider
            .insert("registry/b.css", EmbeddedAssetKind::Css, b"styles")
            .expect("insert b");
        provider
            .insert("registry/a.rs", EmbeddedAssetKind::Rust, b"replacement")
            .expect("replace a");

        let assets = provider
            .assets()
            .collect::<Result<Vec<_>, _>>()
            .expect("valid in-memory assets");
        assert_eq!(provider.asset_count(), 3);
        assert_eq!(
            assets
                .iter()
                .map(|asset| asset.logical_path())
                .collect::<Vec<_>>(),
            ["registry/a.rs", "registry/b.css", "schema/z.json"]
        );
        assert_eq!(assets[0].content(), b"replacement");
    }
}
