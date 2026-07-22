use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use leptos_ui_kit_registry::{
    ConfigError, TOOL_BINARY, TOOL_GIT_URL, TOOL_PACKAGE, ToolSourceConfig, canonical_tool_config,
    load_built_in_registry_item, validate_built_in_registry_health,
};

// This pure resolver contract intentionally precedes wiring the helper into build.rs.
// It freezes the approved decision boundary for the production change that follows.
#[allow(dead_code)]
#[path = "../build_assets.rs"]
mod build_assets;
#[path = "../build_provenance.rs"]
mod build_provenance;

use build_assets::ASSET_SPECS;
use build_provenance::{
    CheckoutProvenance, EXPECTED_CRATE_PATH, GIT_REPOSITORY_OVERRIDE_ENV, GitRunner,
    ProvenanceError, ProvenanceSource, ResolvedProvenance, SystemGit, canonical_fetch_remote,
    explicit_revision, is_canonical_repository, probe_checkout, read_cargo_vcs, resolve_provenance,
};
use tempfile::tempdir;

const REV_A: &str = "0123456789abcdef0123456789abcdef01234567";
const REV_B: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn cargo_vcs_metadata(rev: &str) -> String {
    format!(r#"{{"git":{{"sha1":"{rev}"}},"path_in_vcs":"{EXPECTED_CRATE_PATH}"}}"#)
}

const EXPECTED_PUBLIC_SCHEMA_PATHS: [&str; 5] = [
    "schema/0.9.0-alpha/component-customization.schema.json",
    "schema/0.9.0-alpha/kit.schema.json",
    "schema/0.9.0-alpha/registry-item.schema.json",
    "schema/0.9.0-alpha/registry.schema.json",
    "schema/0.9.0-alpha/theme-contract.schema.json",
];

const CARGO_GENERATED_VCS_METADATA: &str = ".cargo_vcs_info.json";

const EXPECTED_PACKAGE_SUPPORT: [&str; 7] = [
    "Cargo.lock",
    "Cargo.toml",
    "Cargo.toml.orig",
    "README.md",
    "build.rs",
    "build_assets.rs",
    "build_provenance.rs",
];

const EXPECTED_PACKAGE_SOURCES: [&str; 10] = [
    "src/builtin_registry.rs",
    "src/component_customization.rs",
    "src/config.rs",
    "src/detect.rs",
    "src/embedded_assets.rs",
    "src/item.rs",
    "src/lib.rs",
    "src/registry_health.rs",
    "src/theme_contract.rs",
    "src/token_abi.rs",
];

const EXPECTED_PACKAGE_TESTS: [&str; 7] = [
    "tests/asset_catalog.rs",
    "tests/fixtures/theme_refactor_compatibility.json",
    "tests/fixtures/theme_refactor_mapping.json",
    "tests/packaged_runtime_boundaries.rs",
    "tests/registry_schema.rs",
    "tests/theme_refactor_compatibility.rs",
    "tests/theme_refactor_mapping.rs",
];

#[test]
fn provenance_precedence_is_explicit_then_cargo_then_checkout() {
    let cargo = cargo_vcs_metadata(REV_B);
    let checkout = CheckoutProvenance {
        remote: "https://github.com/triesap/leptos_ui_kit.git",
        rev: REV_B,
    };

    assert_eq!(
        resolve_provenance(Some(REV_A), Some(&cargo), Some(checkout)),
        Ok(Some(ResolvedProvenance {
            rev: REV_A.to_owned(),
            source: ProvenanceSource::Explicit,
        }))
    );
    assert_eq!(
        resolve_provenance(
            Some(REV_A),
            Some(r#"{"git":{"sha1":null,"dirty":true},"path_in_vcs":"wrong"}"#),
            Some(checkout),
        ),
        Ok(Some(ResolvedProvenance {
            rev: REV_A.to_owned(),
            source: ProvenanceSource::Explicit,
        })),
        "valid explicit provenance must bypass malformed lower-precedence metadata"
    );
    assert_eq!(
        resolve_provenance(None, Some(&cargo), Some(checkout)),
        Ok(Some(ResolvedProvenance {
            rev: REV_B.to_owned(),
            source: ProvenanceSource::CargoVcs,
        }))
    );
    assert_eq!(
        resolve_provenance(None, None, Some(checkout)),
        Ok(Some(ResolvedProvenance {
            rev: REV_B.to_owned(),
            source: ProvenanceSource::Checkout,
        }))
    );
}

#[test]
fn malformed_higher_precedence_provenance_never_falls_through() {
    let cargo = cargo_vcs_metadata(REV_B);
    let checkout = CheckoutProvenance {
        remote: "https://github.com/triesap/leptos_ui_kit.git",
        rev: REV_B,
    };
    assert!(matches!(
        resolve_provenance(Some("short"), Some(&cargo), Some(checkout)),
        Err(ProvenanceError::Revision {
            source: ProvenanceSource::Explicit,
            ..
        })
    ));
    assert!(matches!(
        resolve_provenance(
            Some("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"),
            Some(&cargo),
            Some(checkout)
        ),
        Err(ProvenanceError::Revision {
            source: ProvenanceSource::Explicit,
            ..
        })
    ));

    let invalid_cargo_vcs = [
        "not json".to_owned(),
        format!(r#"{{"git":{{"dirty":false}},"path_in_vcs":"{EXPECTED_CRATE_PATH}"}}"#),
        format!(r#"{{"git":{{"sha1":42,"dirty":false}},"path_in_vcs":"{EXPECTED_CRATE_PATH}"}}"#),
        cargo_vcs_metadata("short"),
        cargo_vcs_metadata(&"z".repeat(40)),
    ];
    for invalid in invalid_cargo_vcs {
        assert!(resolve_provenance(None, Some(&invalid), Some(checkout)).is_err());
    }

    assert!(matches!(
        resolve_provenance(
            None,
            None,
            Some(CheckoutProvenance {
                remote: "https://github.com/triesap/leptos_ui_kit.git",
                rev: "short",
            })
        ),
        Err(ProvenanceError::Revision {
            source: ProvenanceSource::Checkout,
            ..
        })
    ));
}

#[test]
fn explicit_revision_is_normalized_to_lowercase() {
    assert_eq!(
        resolve_provenance(Some(&REV_A.to_ascii_uppercase()), None, None),
        Ok(Some(ResolvedProvenance {
            rev: REV_A.to_owned(),
            source: ProvenanceSource::Explicit,
        }))
    );
}

#[test]
fn cargo_package_metadata_is_sufficient_outside_git() {
    let cargo = cargo_vcs_metadata(&REV_A.to_ascii_uppercase());
    assert_eq!(
        resolve_provenance(None, Some(&cargo), None),
        Ok(Some(ResolvedProvenance {
            rev: REV_A.to_owned(),
            source: ProvenanceSource::CargoVcs,
        }))
    );

    let explicitly_clean = format!(
        r#"{{"git":{{"sha1":"{REV_A}","dirty":false}},"path_in_vcs":"{EXPECTED_CRATE_PATH}"}}"#
    );
    assert_eq!(
        resolve_provenance(None, Some(&explicitly_clean), None),
        Ok(Some(ResolvedProvenance {
            rev: REV_A.to_owned(),
            source: ProvenanceSource::CargoVcs,
        }))
    );
}

#[test]
fn cargo_vcs_metadata_requires_exact_registry_path_and_clean_state() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(".cargo_vcs_info.json");
    let checkout = CheckoutProvenance {
        remote: "https://github.com/triesap/leptos_ui_kit.git",
        rev: REV_B,
    };
    let invalid = [
        (
            "missing path_in_vcs",
            format!(r#"{{"git":{{"sha1":"{REV_A}","dirty":false}}}}"#),
            "path_in_vcs",
        ),
        (
            "non-string path_in_vcs",
            format!(r#"{{"git":{{"sha1":"{REV_A}","dirty":false}},"path_in_vcs":42}}"#),
            "path_in_vcs",
        ),
        (
            "wrong path_in_vcs",
            format!(
                r#"{{"git":{{"sha1":"{REV_A}","dirty":false}},"path_in_vcs":"crates/leptos_ui_kit_cli"}}"#
            ),
            "path_in_vcs",
        ),
        (
            "non-boolean dirty",
            format!(
                r#"{{"git":{{"sha1":"{REV_A}","dirty":"false"}},"path_in_vcs":"{EXPECTED_CRATE_PATH}"}}"#
            ),
            "git.dirty",
        ),
        (
            "dirty archive",
            format!(
                r#"{{"git":{{"sha1":"{REV_A}","dirty":true}},"path_in_vcs":"{EXPECTED_CRATE_PATH}"}}"#
            ),
            "git.dirty",
        ),
    ];

    for (label, metadata, expected_field) in invalid {
        fs::write(&path, metadata).unwrap_or_else(|error| panic!("write {label}: {error}"));
        let input = read_cargo_vcs(&path)
            .unwrap_or_else(|error| panic!("read {label}: {error}"))
            .unwrap_or_else(|| panic!("{label} metadata must be present"));
        let error = resolve_provenance(None, Some(&input), Some(checkout)).unwrap_err();
        assert!(
            matches!(error, ProvenanceError::CargoVcs(_)),
            "{label}: {error:?}"
        );
        assert!(
            error.to_string().contains(expected_field),
            "{label}: {error}"
        );
    }
}

#[test]
fn checkout_fallback_requires_the_canonical_repository() {
    for remote in [
        "https://github.com/triesap/leptos_ui_kit",
        "https://github.com/triesap/leptos_ui_kit.git",
        "git@github.com:triesap/leptos_ui_kit.git",
        "ssh://git@github.com/triesap/leptos_ui_kit.git",
    ] {
        assert!(is_canonical_repository(remote), "{remote}");
    }
    for remote in [
        "https://github.com/example/leptos_ui_kit.git",
        "https://github.com/example/unrelated.git",
        "https://github.example.com/triesap/leptos_ui_kit.git",
        "git@example.com:triesap/leptos_ui_kit.git",
        "github.com/triesap/leptos_ui_kit",
        "http://github.com/triesap/leptos_ui_kit.git",
        "https://github.com/triesap/leptos_ui_kit.git.git",
    ] {
        assert!(!is_canonical_repository(remote), "{remote}");
        assert_eq!(
            resolve_provenance(None, None, Some(CheckoutProvenance { remote, rev: REV_A })),
            Ok(None)
        );
    }
    assert_eq!(resolve_provenance(None, None, None), Ok(None));
}

#[test]
fn cargo_fetch_head_requires_the_exact_revision_and_canonical_remote() {
    let canonical =
        format!("{REV_A}\t\t'{REV_A}' of https://github.com/triesap/leptos_ui_kit.git\n");
    assert_eq!(
        canonical_fetch_remote(&canonical, REV_A),
        Some("https://github.com/triesap/leptos_ui_kit.git".to_owned())
    );
    assert_eq!(canonical_fetch_remote(&canonical, REV_B), None);
    assert_eq!(
        canonical_fetch_remote(
            &format!("{REV_A}\t\t'{REV_A}' of https://github.com/example/leptos_ui_kit.git\n"),
            REV_A
        ),
        None
    );
    assert_eq!(
        canonical_fetch_remote(
            &format!(
                "{REV_A}\t\t'branch of interest' of ssh://git@github.com/triesap/leptos_ui_kit.git\n"
            ),
            REV_A
        ),
        Some("ssh://git@github.com/triesap/leptos_ui_kit.git".to_owned())
    );
    assert_eq!(canonical_fetch_remote("malformed\n", REV_A), None);
}

#[test]
fn hostile_parent_checkout_is_not_a_provenance_source() {
    assert_eq!(
        resolve_provenance(
            None,
            None,
            Some(CheckoutProvenance {
                remote: "https://github.com/example/unrelated.git",
                rev: REV_A,
            })
        ),
        Ok(None)
    );
}

#[test]
fn checkout_probe_accepts_a_cargo_git_cache_with_canonical_fetch_evidence() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    let cache = dir.path().join("cargo-git-db");
    let manifest_dir = root.join("crates/leptos_ui_kit_registry");
    fs::create_dir_all(&manifest_dir).expect("create manifest directory");
    fs::create_dir_all(&cache).expect("create Cargo Git cache");
    let fetch_head = cache.join("FETCH_HEAD");
    fs::write(
        &fetch_head,
        format!("{REV_A}\t\t'{REV_A}' of https://github.com/triesap/leptos_ui_kit.git\n"),
    )
    .expect("write Cargo fetch evidence");

    let mut git = FakeGit::canonical(&root, REV_A);
    git.outputs.insert(
        vec![
            "config".to_owned(),
            "--local".to_owned(),
            "--get-all".to_owned(),
            "remote.origin.url".to_owned(),
        ],
        format!("file://{}", cache.display()),
    );
    git.outputs.insert(
        vec!["rev-parse".to_owned(), "--is-bare-repository".to_owned()],
        "true".to_owned(),
    );
    git.outputs.insert(
        vec![
            "rev-parse".to_owned(),
            "--git-path".to_owned(),
            "FETCH_HEAD".to_owned(),
        ],
        fetch_head.display().to_string(),
    );

    let probe = probe_checkout(&manifest_dir, &mut git);
    assert_eq!(
        probe.checkout,
        Some(build_provenance::OwnedCheckoutProvenance {
            remote: "https://github.com/triesap/leptos_ui_kit.git".to_owned(),
            rev: REV_A.to_owned(),
        })
    );
    assert!(probe.rerun_paths.contains(&fetch_head));

    fs::write(
        &fetch_head,
        format!("{REV_B}\t\t'{REV_B}' of https://github.com/triesap/leptos_ui_kit.git\n"),
    )
    .expect("replace Cargo fetch evidence");
    let mut mismatched = git.clone_without_calls();
    assert_eq!(
        probe_checkout(&manifest_dir, &mut mismatched).checkout,
        None
    );

    fs::write(
        &fetch_head,
        format!("{REV_A}\t\t'{REV_A}' of https://github.com/example/leptos_ui_kit.git\n"),
    )
    .expect("replace Cargo fetch evidence");
    let mut hostile = git.clone_without_calls();
    assert_eq!(probe_checkout(&manifest_dir, &mut hostile).checkout, None);
}

#[cfg(unix)]
#[test]
fn checkout_probe_rejects_symlinked_cargo_fetch_evidence() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("checkout");
    let cache = dir.path().join("cargo-git-db");
    let manifest_dir = root.join("crates/leptos_ui_kit_registry");
    fs::create_dir_all(&manifest_dir).expect("create manifest directory");
    fs::create_dir_all(&cache).expect("create Cargo Git cache");
    let target = cache.join("actual-fetch-head");
    let fetch_head = cache.join("FETCH_HEAD");
    fs::write(
        &target,
        format!("{REV_A}\t\t'{REV_A}' of https://github.com/triesap/leptos_ui_kit.git\n"),
    )
    .expect("write Cargo fetch evidence");
    symlink(&target, &fetch_head).expect("symlink Cargo fetch evidence");

    let mut git = FakeGit::canonical(&root, REV_A);
    git.outputs.insert(
        vec![
            "config".to_owned(),
            "--local".to_owned(),
            "--get-all".to_owned(),
            "remote.origin.url".to_owned(),
        ],
        format!("file://{}", cache.display()),
    );
    git.outputs.insert(
        vec!["rev-parse".to_owned(), "--is-bare-repository".to_owned()],
        "true".to_owned(),
    );
    git.outputs.insert(
        vec![
            "rev-parse".to_owned(),
            "--git-path".to_owned(),
            "FETCH_HEAD".to_owned(),
        ],
        fetch_head.display().to_string(),
    );

    assert_eq!(probe_checkout(&manifest_dir, &mut git).checkout, None);
}

#[test]
fn compiled_tool_provenance_is_valid_or_explicitly_unavailable() {
    let source = env!("LEPTOS_UI_KIT_GIT_REV_SOURCE");
    let cargo_vcs_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".cargo_vcs_info.json");
    if cargo_vcs_path.is_file() && source != "explicit" {
        assert_eq!(
            source, "cargo-vcs",
            "a package build without an explicit override must identify its package-local Cargo VCS metadata as the provenance source"
        );
    }
    match (source, canonical_tool_config()) {
        ("explicit" | "cargo-vcs" | "checkout", Ok(tool)) => {
            assert_eq!(tool.package, TOOL_PACKAGE);
            assert_eq!(tool.binary, TOOL_BINARY);
            let ToolSourceConfig::Git { url, rev } = tool.source;
            assert_eq!(url, TOOL_GIT_URL);
            assert_eq!(rev.len(), 40);
            assert!(rev.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert_eq!(rev, rev.to_ascii_lowercase());
            if source == "cargo-vcs" {
                let metadata = serde_json::from_slice::<serde_json::Value>(
                    &fs::read(&cargo_vcs_path).unwrap_or_else(|error| {
                        panic!(
                            "cargo-vcs provenance requires {}: {error}",
                            cargo_vcs_path.display()
                        )
                    }),
                )
                .unwrap_or_else(|error| panic!("parse {}: {error}", cargo_vcs_path.display()));
                assert_eq!(
                    metadata
                        .pointer("/git/sha1")
                        .and_then(|value| value.as_str()),
                    Some(rev.as_str()),
                    "compiled cargo-vcs provenance must equal the package metadata"
                );
                assert_eq!(
                    metadata.get("path_in_vcs").and_then(|value| value.as_str()),
                    Some(EXPECTED_CRATE_PATH),
                    "compiled cargo-vcs provenance must come from the registry crate archive"
                );
                assert_eq!(
                    metadata.pointer("/git/dirty"),
                    None,
                    "compiled cargo-vcs provenance must come from an archive without Cargo's dirty marker"
                );
            }
        }
        ("unavailable", Err(ConfigError::MissingToolProvenance { package, binary })) => {
            assert_eq!(package, TOOL_PACKAGE);
            assert_eq!(binary, TOOL_BINARY);
        }
        (source, result) => panic!("inconsistent compiled provenance {source}: {result:?}"),
    }
}

#[test]
fn explicit_provenance_rejects_empty_and_non_unicode_values() {
    assert!(matches!(
        resolve_provenance(Some(""), None, None),
        Err(ProvenanceError::Revision {
            source: ProvenanceSource::Explicit,
            ..
        })
    ));
    assert_eq!(explicit_revision(None), Ok(None));
    assert_eq!(explicit_revision(Some(OsStr::new(REV_A))), Ok(Some(REV_A)));

    #[cfg(unix)]
    {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let value = OsString::from_vec(vec![0xff]);
        assert_eq!(
            explicit_revision(Some(&value)),
            Err(ProvenanceError::ExplicitEncoding)
        );
    }
}

#[test]
fn cargo_vcs_reader_distinguishes_absent_valid_and_invalid_files() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(".cargo_vcs_info.json");
    assert_eq!(read_cargo_vcs(&path), Ok(None));

    let input = cargo_vcs_metadata(&REV_A.to_ascii_uppercase());
    fs::write(&path, &input).expect("write Cargo VCS metadata");
    assert_eq!(read_cargo_vcs(&path), Ok(Some(input.clone())));
    assert_eq!(
        resolve_provenance(None, read_cargo_vcs(&path).unwrap().as_deref(), None),
        Ok(Some(ResolvedProvenance {
            rev: REV_A.to_owned(),
            source: ProvenanceSource::CargoVcs,
        }))
    );

    fs::write(&path, [0xff]).expect("write non-UTF-8 Cargo VCS metadata");
    assert!(matches!(
        read_cargo_vcs(&path),
        Err(ProvenanceError::CargoVcs(_))
    ));
    fs::remove_file(&path).expect("remove Cargo VCS metadata");
    fs::create_dir(&path).expect("create invalid Cargo VCS directory");
    assert!(matches!(
        read_cargo_vcs(&path),
        Err(ProvenanceError::CargoVcs(_))
    ));
}

#[cfg(unix)]
#[test]
fn cargo_vcs_reader_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().expect("tempdir");
    let target = dir.path().join("target.json");
    let path = dir.path().join(".cargo_vcs_info.json");
    fs::write(&target, cargo_vcs_metadata(REV_A)).expect("write target");
    symlink(&target, &path).expect("create symlink");
    assert!(matches!(
        read_cargo_vcs(&path),
        Err(ProvenanceError::CargoVcs(_))
    ));
}

#[test]
fn cargo_vcs_reader_never_walks_to_parent_metadata() {
    let dir = tempdir().expect("tempdir");
    let manifest_dir = dir.path().join("nested/crate");
    fs::create_dir_all(&manifest_dir).expect("create nested manifest directory");
    fs::write(
        dir.path().join(".cargo_vcs_info.json"),
        cargo_vcs_metadata(REV_A),
    )
    .expect("write parent metadata");

    assert_eq!(
        read_cargo_vcs(&manifest_dir.join(".cargo_vcs_info.json")),
        Ok(None)
    );
}

#[test]
fn checkout_probe_anchors_commands_and_reads_origin_before_head() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let manifest_dir = root.join("crates/leptos_ui_kit_registry");
    fs::create_dir_all(&manifest_dir).expect("create manifest directory");
    let git_dir = root.join(".git");
    fs::create_dir(&git_dir).expect("create standalone Git directory marker");
    let mut git = FakeGit::canonical(root, REV_A);

    let probe = probe_checkout(&manifest_dir, &mut git);
    let checkout = probe.checkout.expect("canonical checkout");
    assert_eq!(
        checkout.remote,
        "https://github.com/triesap/leptos_ui_kit.git"
    );
    assert_eq!(checkout.rev, REV_A);
    assert_eq!(
        resolve_provenance(None, None, Some(checkout.as_borrowed())),
        Ok(Some(ResolvedProvenance {
            rev: REV_A.to_owned(),
            source: ProvenanceSource::Checkout,
        }))
    );
    assert_eq!(git.calls[0].0, manifest_dir);
    assert!(git.calls[1..].iter().all(|(anchor, _)| anchor == root));

    let origin_index = git
        .calls
        .iter()
        .position(|(_, args)| args.first().is_some_and(|arg| arg == "config"))
        .expect("origin query");
    let first_head_index = git
        .calls
        .iter()
        .position(|(_, args)| {
            args.iter()
                .any(|arg| arg == "HEAD" || arg.starts_with("HEAD^"))
        })
        .expect("HEAD query");
    assert!(origin_index < first_head_index);
    assert!(!probe.rerun_paths.contains(&git_dir));
    for expected in [
        git_dir.join("config"),
        git_dir.join("config.worktree"),
        git_dir.join("HEAD"),
        git_dir.join("packed-refs"),
        git_dir.join("refs/heads/main"),
    ] {
        assert!(
            probe.rerun_paths.contains(&expected),
            "{}",
            expected.display()
        );
    }
}

#[test]
fn system_git_sanitizes_every_repository_override() {
    let actual = GIT_REPOSITORY_OVERRIDE_ENV
        .into_iter()
        .collect::<BTreeSet<_>>();
    for required in [
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
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_WORK_TREE",
    ] {
        assert!(actual.contains(required), "missing override: {required}");
    }
    assert_eq!(actual.len(), GIT_REPOSITORY_OVERRIDE_ENV.len());
}

#[test]
fn checkout_probe_rejects_noncanonical_origin_without_reading_head() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let manifest_dir = root.join("crates/leptos_ui_kit_registry");
    fs::create_dir_all(&manifest_dir).expect("create manifest directory");
    let mut git = FakeGit::canonical(root, REV_A);
    git.outputs.insert(
        vec![
            "config".to_owned(),
            "--local".to_owned(),
            "--get-all".to_owned(),
            "remote.origin.url".to_owned(),
        ],
        "https://github.com/example/unrelated.git".to_owned(),
    );

    let probe = probe_checkout(&manifest_dir, &mut git);
    assert_eq!(probe.checkout, None);
    assert!(!git.calls.iter().any(|(_, args)| {
        args.iter()
            .any(|arg| arg == "HEAD" || arg.starts_with("HEAD^") || arg.starts_with("refs/"))
    }));
}

#[test]
fn checkout_probe_treats_missing_git_or_wrong_layout_as_unavailable() {
    let dir = tempdir().expect("tempdir");
    let manifest_dir = dir.path().join("crates/leptos_ui_kit_registry");
    fs::create_dir_all(&manifest_dir).expect("create manifest directory");
    let mut missing = FakeGit::default();
    assert_eq!(probe_checkout(&manifest_dir, &mut missing).checkout, None);

    let hostile_manifest = dir
        .path()
        .join("domains/project/crates/leptos_ui_kit_registry");
    fs::create_dir_all(&hostile_manifest).expect("create hostile manifest directory");
    let mut wrong_layout = FakeGit::default();
    wrong_layout.outputs.insert(
        vec!["rev-parse".to_owned(), "--show-toplevel".to_owned()],
        dir.path().display().to_string(),
    );
    assert_eq!(
        probe_checkout(&hostile_manifest, &mut wrong_layout).checkout,
        None
    );
    assert_eq!(wrong_layout.calls.len(), 1);
}

#[test]
fn system_git_rejects_a_real_hostile_parent_checkout() {
    let dir = tempdir().expect("tempdir");
    init_git_repository(dir.path(), "https://github.com/example/unrelated.git", true);
    let manifest_dir = dir
        .path()
        .join("nested/repository/crates/leptos_ui_kit_registry");
    fs::create_dir_all(&manifest_dir).expect("create hostile nested manifest directory");

    let probe = probe_checkout(&manifest_dir, &mut SystemGit);
    assert_eq!(probe.checkout, None);
}

#[test]
fn system_git_accepts_canonical_spellings_and_handles_unavailable_checkouts() {
    let dir = tempdir().expect("tempdir");
    let manifest_dir = dir.path().join("crates/leptos_ui_kit_registry");
    fs::create_dir_all(&manifest_dir).expect("create manifest directory");

    assert_eq!(probe_checkout(&manifest_dir, &mut SystemGit).checkout, None);
    init_git_repository(dir.path(), "", false);
    assert_eq!(probe_checkout(&manifest_dir, &mut SystemGit).checkout, None);
    run_git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:triesap/leptos_ui_kit.git",
        ],
    );
    assert_eq!(
        probe_checkout(&manifest_dir, &mut SystemGit).checkout,
        None,
        "an unborn canonical checkout has unavailable provenance"
    );

    commit_git_fixture(dir.path());
    let expected_rev = run_git_output(dir.path(), &["rev-parse", "HEAD"]);
    for remote in [
        "https://github.com/triesap/leptos_ui_kit",
        "https://github.com/triesap/leptos_ui_kit.git",
        "git@github.com:triesap/leptos_ui_kit.git",
        "ssh://git@github.com/triesap/leptos_ui_kit.git",
    ] {
        run_git(dir.path(), &["remote", "set-url", "origin", remote]);
        let probe = probe_checkout(&manifest_dir, &mut SystemGit);
        let checkout = probe.checkout.expect("canonical checkout provenance");
        assert_eq!(checkout.remote, remote);
        assert_eq!(checkout.rev, expected_rev);
    }
}

#[test]
fn authoring_registry_inventory_is_exact_safe_and_portable() {
    let registry_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("registry");
    let mut actual = Vec::new();
    collect_files(&registry_root, &registry_root, &mut actual);
    actual.sort();

    let expected = ASSET_SPECS
        .iter()
        .filter_map(|spec| spec.source_path.strip_prefix("registry/"))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), 88);
    let mut case_folded = BTreeSet::new();
    for path in actual {
        assert!(!path.starts_with('/'));
        assert!(!path.split('/').any(|segment| segment == ".."));
        assert!(case_folded.insert(path.to_ascii_lowercase()), "{path}");
    }
    validate_built_in_registry_health().expect("built-in registry health");
}

#[test]
fn approved_embedded_inventory_has_95_unique_logical_assets() {
    let logical_assets = ASSET_SPECS
        .iter()
        .map(|spec| spec.logical_path.to_owned())
        .collect::<BTreeSet<_>>();

    assert_eq!(logical_assets.len(), 95);
    assert!(logical_assets.contains("registry/registry.json"));
    assert!(logical_assets.contains("registry/contracts/theme-v1.json"));
    assert!(logical_assets.contains("registry/contracts/component-customization-v1.json"));
    for schema in EXPECTED_PUBLIC_SCHEMA_PATHS {
        assert!(logical_assets.contains(schema), "{schema}");
    }
    assert!(!logical_assets.contains("registry/contracts/theme-contract.schema.json"));
}

#[test]
fn registry_crate_package_inventory_is_exact_and_self_contained() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = manifest_dir.join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .args([
            "package",
            "-p",
            "leptos_ui_kit_registry",
            "--allow-dirty",
            "--list",
            "--manifest-path",
        ])
        .arg(&manifest_path)
        .output()
        .expect("run cargo package --list");
    assert!(
        output.status.success(),
        "cargo package --list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut actual = String::from_utf8(output.stdout)
        .expect("UTF-8 cargo package list")
        .lines()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    // Cargo adds VCS metadata when packaging from Git, but a package can be
    // repackaged from its extracted archive without that generated input.
    // It is provenance metadata rather than an authored runtime asset.
    actual.remove(CARGO_GENERATED_VCS_METADATA);
    let expected = EXPECTED_PACKAGE_SUPPORT
        .iter()
        .map(|path| (*path).to_owned())
        .chain(ASSET_SPECS.map(|spec| spec.source_path.to_owned()))
        .chain(EXPECTED_PACKAGE_SOURCES.map(str::to_owned))
        .chain(EXPECTED_PACKAGE_TESTS.map(str::to_owned))
        .collect::<BTreeSet<_>>();

    assert_eq!(expected.len(), 119);
    assert_eq!(actual, expected);
    for schema in EXPECTED_PUBLIC_SCHEMA_PATHS {
        assert!(actual.contains(schema), "missing packaged schema: {schema}");
    }
    assert!(!actual.contains("tests/public_schema_parity.rs"));
}

#[test]
fn built_in_item_source_path_is_a_stable_logical_locator() {
    let item = load_built_in_registry_item("button").expect("load button");
    assert_eq!(item.source_path, Path::new("ui/button.json"));
    assert!(!item.source_path.is_absolute());
}

fn collect_files(root: &Path, directory: &Path, output: &mut Vec<String>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .expect("read registry directory entries");
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).expect("inspect registry asset");
        assert!(!metadata.file_type().is_symlink(), "{}", path.display());
        if metadata.is_dir() {
            collect_files(root, &path, output);
        } else {
            assert!(metadata.is_file(), "{}", path.display());
            output.push(logical_path(root, &path));
        }
    }
}

fn logical_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("registry-relative path")
        .components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .expect("UTF-8 registry asset name")
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Debug, Default)]
struct FakeGit {
    outputs: BTreeMap<Vec<String>, String>,
    calls: Vec<(PathBuf, Vec<String>)>,
}

impl FakeGit {
    fn canonical(root: &Path, rev: &str) -> Self {
        let git_dir = root.join(".git");
        let mut outputs = BTreeMap::new();
        outputs.insert(
            vec!["rev-parse".to_owned(), "--show-toplevel".to_owned()],
            root.display().to_string(),
        );
        for name in [
            "config",
            "config.worktree",
            "HEAD",
            "packed-refs",
            "refs/heads/main",
        ] {
            outputs.insert(
                vec![
                    "rev-parse".to_owned(),
                    "--git-path".to_owned(),
                    name.to_owned(),
                ],
                git_dir.join(name).display().to_string(),
            );
        }
        outputs.insert(
            vec![
                "config".to_owned(),
                "--local".to_owned(),
                "--get-all".to_owned(),
                "remote.origin.url".to_owned(),
            ],
            "https://github.com/triesap/leptos_ui_kit.git".to_owned(),
        );
        outputs.insert(
            vec![
                "symbolic-ref".to_owned(),
                "-q".to_owned(),
                "HEAD".to_owned(),
            ],
            "refs/heads/main".to_owned(),
        );
        outputs.insert(
            vec![
                "rev-parse".to_owned(),
                "--verify".to_owned(),
                "HEAD^{commit}".to_owned(),
            ],
            rev.to_owned(),
        );
        Self {
            outputs,
            calls: Vec::new(),
        }
    }

    fn clone_without_calls(&self) -> Self {
        Self {
            outputs: self.outputs.clone(),
            calls: Vec::new(),
        }
    }
}

impl GitRunner for FakeGit {
    fn output(&mut self, anchor: &Path, args: &[&str]) -> Option<String> {
        let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        self.calls.push((anchor.to_path_buf(), args.clone()));
        self.outputs.get(&args).cloned()
    }
}

fn init_git_repository(root: &Path, remote: &str, commit: bool) {
    run_git(root, &["init"]);
    run_git(root, &["config", "user.name", "Leptos UI Kit Tests"]);
    run_git(root, &["config", "user.email", "tests@example.invalid"]);
    run_git(root, &["config", "commit.gpgsign", "false"]);
    if !remote.is_empty() {
        run_git(root, &["remote", "add", "origin", remote]);
    }
    if commit {
        commit_git_fixture(root);
    }
}

fn commit_git_fixture(root: &Path) {
    fs::write(root.join("fixture.txt"), "fixture\n").expect("write Git fixture");
    run_git(root, &["add", "fixture.txt"]);
    run_git(root, &["commit", "-m", "test fixture"]);
}

fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_git_output(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git fixture output")
        .trim()
        .to_owned()
}
