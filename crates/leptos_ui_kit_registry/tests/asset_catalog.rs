use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[path = "../build_assets.rs"]
mod build_assets;

use build_assets::{
    ASSET_ROOTS, ASSET_SPECS, AssetCatalogError, AssetKind, AssetSpec, GENERATED_CATALOG_FILE,
    SNAPSHOT_DIRECTORY, generate_asset_catalog, validate_asset_specs,
};
use tempfile::{TempDir, tempdir};

#[test]
fn catalog_contains_the_exact_sorted_83_asset_inventory() {
    let fixture = AssetFixture::new();
    let generated = fixture.generate();

    assert_eq!(ASSET_SPECS.len(), 83);
    assert_eq!(generated.assets.len(), 83);
    let logical = generated
        .assets
        .iter()
        .map(|asset| asset.logical_path.as_str())
        .collect::<Vec<_>>();
    assert!(logical.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        logical.iter().copied().collect::<BTreeSet<_>>().len(),
        logical.len()
    );
    assert_eq!(
        logical.first().copied(),
        Some("registry/contracts/component-customization-v1.json")
    );
    assert_eq!(
        logical.last().copied(),
        Some("schema/0.9.0-alpha/theme-contract.schema.json")
    );
    assert!(
        !logical.contains(&"registry/contracts/theme-contract.schema.json"),
        "the package schema is the single embedded theme schema"
    );
    assert_eq!(
        fs::read_to_string(&generated.generated_path).expect("read generated catalog"),
        generated.rust_source
    );
    assert!(generated.generated_path.ends_with(GENERATED_CATALOG_FILE));
    assert!(generated.snapshot_root.ends_with(SNAPSHOT_DIRECTORY));

    for asset in &generated.assets {
        assert!(asset.content_hash.starts_with("sha256:"));
        assert_eq!(asset.content_hash.len(), 71);
        let snapshot = generated.snapshot_root.join(&asset.logical_path);
        assert_eq!(
            fs::read(&snapshot).expect("read asset snapshot").len(),
            asset.byte_len,
            "{}",
            asset.logical_path
        );
    }
}

#[test]
fn generated_records_use_stable_out_dir_snapshots() {
    let fixture = AssetFixture::new();
    let generated = fixture.generate();

    assert!(
        generated
            .rust_source
            .contains("pub(crate) const EMBEDDED_ASSET_COUNT: usize = 83;")
    );
    assert!(
        generated
            .rust_source
            .contains("pub(crate) const EMBEDDED_CATALOG_HASH: &str = \"sha256:")
    );
    assert!(
        generated
            .rust_source
            .contains("pub(crate) static EMBEDDED_ASSETS: &[EmbeddedAsset]")
    );
    assert!(generated.rust_source.contains(
        "include_bytes!(concat!(env!(\"OUT_DIR\"), \"/leptos_ui_kit_embedded_assets/registry/ui/button.rs\"))"
    ));
    for variant in ["Json", "Rust", "Css"] {
        assert!(
            generated
                .rust_source
                .contains(&format!("kind: EmbeddedAssetKind::{variant}"))
        );
    }

    let original = fs::read(fixture.package_root().join("registry/ui/button.rs"))
        .expect("read fixture source");
    fs::write(
        fixture.package_root().join("registry/ui/button.rs"),
        "changed after generation\n",
    )
    .expect("mutate source after generation");
    assert_eq!(
        fs::read(generated.snapshot_root.join("registry/ui/button.rs"))
            .expect("read stable snapshot"),
        original
    );
}

#[test]
fn absolute_source_and_output_paths_do_not_affect_catalogs_or_hashes() {
    let first = AssetFixture::new();
    let second = AssetFixture::new();
    let first_generated = first.generate();
    let second_generated = second.generate();

    assert_ne!(first.package_root(), second.package_root());
    assert_ne!(first.out_dir(), second.out_dir());
    assert_eq!(first_generated.assets, second_generated.assets);
    assert_eq!(first_generated.catalog_hash, second_generated.catalog_hash);
    assert_eq!(first_generated.rust_source, second_generated.rust_source);
    for root in [
        first.package_root(),
        first.out_dir(),
        second.package_root(),
        second.out_dir(),
    ] {
        assert!(
            !first_generated
                .rust_source
                .contains(&root.display().to_string()),
            "generated output leaked {}",
            root.display()
        );
    }
}

#[test]
fn changing_one_asset_changes_only_its_hash_and_the_catalog_hash() {
    let fixture = AssetFixture::new();
    let first = fixture.generate();
    let changed_path = "registry/styles/button.css";
    let path = fixture.package_root().join(changed_path);
    let mut content = fs::read_to_string(&path).expect("read CSS fixture");
    content.push_str("\n/* catalog mutation */\n");
    fs::write(&path, content).expect("mutate CSS fixture");
    let second = fixture.generate();

    assert_ne!(first.catalog_hash, second.catalog_hash);
    for before in &first.assets {
        let after = second
            .assets
            .iter()
            .find(|asset| asset.logical_path == before.logical_path)
            .expect("same logical asset");
        if before.logical_path == changed_path {
            assert_ne!(before.content_hash, after.content_hash);
        } else {
            assert_eq!(before.content_hash, after.content_hash);
        }
    }
}

#[test]
fn specification_rejects_duplicate_source_and_logical_paths() {
    let duplicate_source = [
        AssetSpec::new("registry/a.rs", "registry/a.rs", AssetKind::Rust),
        AssetSpec::new("registry/a.rs", "registry/b.rs", AssetKind::Rust),
    ];
    assert!(matches!(
        validate_asset_specs(&duplicate_source, &["registry"]),
        Err(AssetCatalogError::DuplicateSource(path)) if path == "registry/a.rs"
    ));

    let duplicate_logical = [
        AssetSpec::new("registry/a.rs", "registry/shared.rs", AssetKind::Rust),
        AssetSpec::new("registry/b.rs", "registry/shared.rs", AssetKind::Rust),
    ];
    assert!(matches!(
        validate_asset_specs(&duplicate_logical, &["registry"]),
        Err(AssetCatalogError::DuplicateLogical(path)) if path == "registry/shared.rs"
    ));
}

#[test]
fn specification_rejects_unsafe_mismatched_and_casefolded_paths() {
    for path in [
        "../registry/a.rs",
        "/registry/a.rs",
        "registry\\a.rs",
        "registry//a.rs",
        "registry/.hidden.rs",
        "registry/con.rs",
        "registry/trailing.",
        "other/a.rs",
    ] {
        let specs = [AssetSpec::new(path, "registry/a.rs", AssetKind::Rust)];
        assert!(
            matches!(
                validate_asset_specs(&specs, &["registry"]),
                Err(AssetCatalogError::UnsafePath { .. })
            ),
            "{path}"
        );
    }

    let wrong_extension = [AssetSpec::new(
        "registry/a.css",
        "registry/a.css",
        AssetKind::Rust,
    )];
    assert!(matches!(
        validate_asset_specs(&wrong_extension, &["registry"]),
        Err(AssetCatalogError::UnsafePath { .. })
    ));

    let collision = [
        AssetSpec::new("registry/Button.rs", "registry/one.rs", AssetKind::Rust),
        AssetSpec::new("registry/button.rs", "registry/two.rs", AssetKind::Rust),
    ];
    assert!(matches!(
        validate_asset_specs(&collision, &["registry"]),
        Err(AssetCatalogError::CaseFoldCollision { .. })
    ));
}

#[test]
fn generator_rejects_missing_and_unexpected_inputs() {
    let missing = AssetFixture::new();
    fs::remove_file(missing.package_root().join("registry/ui/button.rs"))
        .expect("remove expected input");
    assert!(matches!(
        missing.try_generate(),
        Err(AssetCatalogError::MissingInput(path)) if path == "registry/ui/button.rs"
    ));

    let unexpected_file = AssetFixture::new();
    fs::write(
        unexpected_file
            .package_root()
            .join("registry/ui/unlisted.rs"),
        "unlisted\n",
    )
    .expect("write unexpected file");
    assert!(matches!(
        unexpected_file.try_generate(),
        Err(AssetCatalogError::UnexpectedInput(path)) if path == "registry/ui/unlisted.rs"
    ));

    let unexpected_directory = AssetFixture::new();
    fs::create_dir(
        unexpected_directory
            .package_root()
            .join("registry/unlisted"),
    )
    .expect("create unexpected directory");
    assert!(matches!(
        unexpected_directory.try_generate(),
        Err(AssetCatalogError::UnexpectedInput(path)) if path == "registry/unlisted"
    ));
}

#[test]
fn generator_rejects_non_utf8_content_and_malformed_json() {
    let non_utf8 = AssetFixture::new();
    fs::write(
        non_utf8.package_root().join("registry/ui/button.rs"),
        [0xff],
    )
    .expect("write non-UTF-8 input");
    assert!(matches!(
        non_utf8.try_generate(),
        Err(AssetCatalogError::NonUtf8Content(path)) if path == "registry/ui/button.rs"
    ));

    let malformed_json = AssetFixture::new();
    fs::write(
        malformed_json
            .package_root()
            .join("registry/ui/button.json"),
        "not JSON\n",
    )
    .expect("write malformed JSON");
    assert!(matches!(
        malformed_json.try_generate(),
        Err(AssetCatalogError::InvalidJson { path, .. }) if path == "registry/ui/button.json"
    ));
}

#[test]
fn generator_rejects_a_directory_at_an_expected_file_path() {
    let fixture = AssetFixture::new();
    let path = fixture.package_root().join("registry/ui/button.rs");
    fs::remove_file(&path).expect("remove expected file");
    fs::create_dir(&path).expect("replace expected file with directory");
    assert!(matches!(
        fixture.try_generate(),
        Err(AssetCatalogError::UnexpectedType(path)) if path == "registry/ui/button.rs"
    ));
}

#[cfg(unix)]
#[test]
fn generator_rejects_file_directory_and_root_symlinks() {
    use std::os::unix::fs::symlink;

    let file = AssetFixture::new();
    let file_path = file.package_root().join("registry/ui/button.rs");
    fs::remove_file(&file_path).expect("remove expected file");
    symlink("anchor.rs", &file_path).expect("create file symlink");
    assert!(matches!(
        file.try_generate(),
        Err(AssetCatalogError::Symlink(path)) if path == "registry/ui/button.rs"
    ));

    let directory = AssetFixture::new();
    let directory_path = directory.package_root().join("registry/ui/dialog");
    let moved_path = directory.package_root().join("dialog-target");
    fs::rename(&directory_path, &moved_path).expect("move expected directory");
    symlink(&moved_path, &directory_path).expect("create directory symlink");
    assert!(matches!(
        directory.try_generate(),
        Err(AssetCatalogError::Symlink(path)) if path == "registry/ui/dialog"
    ));

    let root = AssetFixture::new();
    let root_path = root.package_root().join("registry");
    let moved_root = root.package_root().join("registry-target");
    fs::rename(&root_path, &moved_root).expect("move asset root");
    symlink(&moved_root, &root_path).expect("create root symlink");
    assert!(matches!(
        root.try_generate(),
        Err(AssetCatalogError::Symlink(path)) if path == "registry"
    ));
}

#[cfg(unix)]
#[test]
fn generator_rejects_non_utf8_names_and_special_files() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, process::Command};

    let non_utf8 = AssetFixture::new();
    let path = non_utf8
        .package_root()
        .join("registry/ui")
        .join(OsString::from_vec(vec![0xff]));
    match fs::write(&path, "invalid name\n") {
        Ok(()) => assert!(matches!(
            non_utf8.try_generate(),
            Err(AssetCatalogError::NonUtf8Path(parent)) if parent == "registry/ui"
        )),
        Err(error) if error.raw_os_error() == Some(92) => {
            // macOS rejects the invalid byte sequence before traversal can observe it.
        }
        Err(error) => panic!("failed to create non-UTF-8 filename fixture: {error}"),
    }

    let special = AssetFixture::new();
    let path = special.package_root().join("registry/ui/special.rs");
    let output = Command::new("mkfifo")
        .arg(&path)
        .output()
        .expect("run mkfifo for special asset input");
    assert!(
        output.status.success(),
        "mkfifo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(matches!(
        special.try_generate(),
        Err(AssetCatalogError::UnexpectedType(path)) if path == "registry/ui/special.rs"
    ));
}

#[test]
fn rerun_inventory_covers_every_asset_and_directory() {
    let fixture = AssetFixture::new();
    let generated = fixture.generate();

    for spec in ASSET_SPECS {
        assert!(
            generated
                .rerun_paths
                .contains(&fixture.package_root().join(spec.source_path)),
            "{}",
            spec.source_path
        );
    }
    for directory in [
        "registry",
        "registry/contracts",
        "registry/foundation",
        "registry/styles",
        "registry/ui",
        "registry/ui/dialog",
        "schema",
        "schema/0.9.0-alpha",
    ] {
        assert!(
            generated
                .rerun_paths
                .contains(&fixture.package_root().join(directory)),
            "{directory}"
        );
    }
}

struct AssetFixture {
    _temp: TempDir,
    package_root: PathBuf,
    out_dir: PathBuf,
}

impl AssetFixture {
    fn new() -> Self {
        let temp = tempdir().expect("create asset fixture root");
        let package_root = temp.path().join("package");
        let out_dir = temp.path().join("out");
        fs::create_dir_all(&package_root).expect("create fixture package root");
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for spec in ASSET_SPECS {
            let source = source_root.join(spec.source_path);
            let target = package_root.join(spec.source_path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).expect("create fixture asset directory");
            }
            fs::copy(&source, &target).unwrap_or_else(|error| {
                panic!(
                    "failed to copy {} to {}: {error}",
                    source.display(),
                    target.display()
                )
            });
        }
        Self {
            _temp: temp,
            package_root,
            out_dir,
        }
    }

    fn package_root(&self) -> &Path {
        &self.package_root
    }

    fn out_dir(&self) -> &Path {
        &self.out_dir
    }

    fn generate(&self) -> build_assets::GeneratedCatalog {
        self.try_generate().expect("generate asset catalog")
    }

    fn try_generate(&self) -> Result<build_assets::GeneratedCatalog, AssetCatalogError> {
        generate_asset_catalog(&self.package_root, &self.out_dir)
    }
}

#[test]
fn production_specification_is_valid() {
    validate_asset_specs(&ASSET_SPECS, &ASSET_ROOTS).expect("valid production asset specification");
}

#[test]
fn package_include_matches_assets_and_excludes_source_only_tests() {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(package_root.join("Cargo.toml")).expect("read Cargo.toml");
    let manifest = toml::from_str::<toml::Value>(&manifest).expect("parse Cargo.toml");
    let include = manifest["package"]["include"]
        .as_array()
        .expect("package.include")
        .iter()
        .map(|value| value.as_str().expect("string package.include entry"))
        .collect::<BTreeSet<_>>();

    assert!(
        include
            .iter()
            .all(|path| !path.contains('*') && !path.contains('?')),
        "package.include entries must be exact paths"
    );
    assert!(!include.contains("tests/public_schema_parity.rs"));
    let packaged_assets = include
        .iter()
        .copied()
        .filter(|path| path.starts_with("registry/") || path.starts_with("schema/"))
        .collect::<BTreeSet<_>>();
    let expected_assets = ASSET_SPECS
        .iter()
        .map(|spec| spec.source_path)
        .collect::<BTreeSet<_>>();
    assert_eq!(packaged_assets, expected_assets);

    let workspace_schema_reference = ["..", "..", "schema"].join("/");
    for source in include.iter().filter(|path| path.ends_with(".rs")) {
        let input = fs::read_to_string(package_root.join(source))
            .unwrap_or_else(|error| panic!("failed to read packaged source {source}: {error}"));
        assert!(
            !input.contains(&workspace_schema_reference),
            "packaged source depends on workspace schema: {source}"
        );
    }
}
