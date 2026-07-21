#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use sha2::{Digest, Sha256};

const HOMEPAGE_FIXTURE_HASH: &str =
    "sha256:2a9cef901aaa9b3eb80eeb871fa6f4bc3d2f3de829061f6f07b7e41d0dfc8a3b";
const LEGACY_CSS_FIXTURE_HASH: &str =
    "sha256:c8f6e65600002ab6348bade77b9f2029c101ff9ec7468bca775a57ff2de604ae";

#[test]
fn package_local_homepage_fixture_matches_required_canonical_source() {
    let workspace = workspace_root();
    let canonical_root = workspace.join("tests/fixtures/homepage_trunk_csr");
    assert!(
        canonical_root.is_dir(),
        "required canonical homepage fixture is missing: {}",
        canonical_root.display()
    );

    let package_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/homepage_trunk_csr");
    let mut package = fixture_snapshot(&package_root);
    let package_manifest = package
        .remove(Path::new("Cargo.toml.fixture"))
        .expect("package-local fixture manifest");
    assert!(
        package
            .insert(PathBuf::from("Cargo.toml"), package_manifest)
            .is_none(),
        "package-local fixture must contain one manifest"
    );
    let canonical = fixture_snapshot(&canonical_root);

    assert_eq!(
        package, canonical,
        "package-local homepage fixture differs from its canonical source"
    );
    assert_eq!(
        snapshot_hash(&canonical),
        HOMEPAGE_FIXTURE_HASH,
        "canonical homepage fixture changed; review parity and update the pinned hash"
    );
}

#[test]
fn package_local_legacy_css_fixture_matches_required_canonical_source() {
    let workspace = workspace_root();
    let canonical_root = workspace.join("tests/fixtures/theme_pre_refactor_06124efa");
    assert!(
        canonical_root.is_dir(),
        "required canonical legacy CSS fixture is missing: {}",
        canonical_root.display()
    );
    let package_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/theme_pre_refactor_06124efa");
    let canonical = fixture_snapshot(&canonical_root);
    let package = fixture_snapshot(&package_root);

    assert_eq!(
        package, canonical,
        "package-local CLI legacy CSS fixture differs from its canonical source"
    );
    assert_eq!(
        snapshot_hash(&canonical),
        LEGACY_CSS_FIXTURE_HASH,
        "canonical legacy CSS fixture changed; review parity and update the pinned hash"
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical workspace root")
}

fn fixture_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
            .collect::<Result<Vec<_>, _>>()
            .expect("read fixture entries");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("inspect fixture entry");
            assert!(!metadata.file_type().is_symlink(), "{}", path.display());
            if metadata.is_dir() {
                visit(root, &path, snapshot);
            } else {
                assert!(metadata.is_file(), "{}", path.display());
                snapshot.insert(
                    path.strip_prefix(root)
                        .expect("fixture path below root")
                        .to_path_buf(),
                    fs::read(&path).expect("read fixture file"),
                );
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

fn snapshot_hash(snapshot: &BTreeMap<PathBuf, Vec<u8>>) -> String {
    let mut digest = Sha256::new();
    for (path, bytes) in snapshot {
        let logical_path = path
            .components()
            .map(|component| match component {
                Component::Normal(value) => value.to_str().expect("UTF-8 fixture path"),
                _ => panic!("non-portable fixture path: {}", path.display()),
            })
            .collect::<Vec<_>>()
            .join("/");
        digest.update((logical_path.len() as u64).to_be_bytes());
        digest.update(logical_path.as_bytes());
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("sha256:{:x}", digest.finalize())
}
