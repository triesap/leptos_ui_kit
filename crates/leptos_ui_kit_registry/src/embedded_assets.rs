#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmbeddedAssetKind {
    Json,
    Rust,
    Css,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EmbeddedAsset {
    pub logical_path: &'static str,
    pub kind: EmbeddedAssetKind,
    pub content: &'static [u8],
    pub content_hash: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use sha2::{Digest, Sha256};

    use super::{EMBEDDED_ASSET_COUNT, EMBEDDED_ASSETS, EMBEDDED_CATALOG_HASH, EmbeddedAssetKind};

    #[test]
    fn generated_catalog_is_exact_sorted_and_content_addressed() {
        assert_eq!(EMBEDDED_ASSET_COUNT, 68);
        assert_eq!(EMBEDDED_ASSETS.len(), EMBEDDED_ASSET_COUNT);
        assert!(EMBEDDED_CATALOG_HASH.starts_with("sha256:"));
        assert_eq!(EMBEDDED_CATALOG_HASH.len(), "sha256:".len() + 64);

        let paths = EMBEDDED_ASSETS
            .iter()
            .map(|asset| asset.logical_path)
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
        for asset in EMBEDDED_ASSETS {
            assert!(!asset.content.is_empty(), "{}", asset.logical_path);
            std::str::from_utf8(asset.content)
                .unwrap_or_else(|error| panic!("{} is not UTF-8: {error}", asset.logical_path));
            assert_eq!(
                asset.content_hash,
                format!("sha256:{:x}", Sha256::digest(asset.content)),
                "{}",
                asset.logical_path
            );
            match asset.kind {
                EmbeddedAssetKind::Json => {
                    kinds[0] += 1;
                    serde_json::from_slice::<serde_json::Value>(asset.content).unwrap_or_else(
                        |error| panic!("{} is not valid JSON: {error}", asset.logical_path),
                    );
                }
                EmbeddedAssetKind::Rust => kinds[1] += 1,
                EmbeddedAssetKind::Css => kinds[2] += 1,
            }
        }
        assert_eq!(kinds, [17, 41, 10]);
    }
}
