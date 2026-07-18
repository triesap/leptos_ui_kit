use std::{collections::BTreeSet, fs, path::Path, process::Command};

use leptos_ui_kit_registry::{
    ConfigError, TOOL_BINARY, TOOL_GIT_URL, TOOL_PACKAGE, ToolSourceConfig, canonical_tool_config,
    load_built_in_registry_item, validate_built_in_registry_health,
};

// This pure resolver contract intentionally precedes wiring the helper into build.rs.
// It freezes the approved decision boundary for the production change that follows.
#[path = "../build_provenance.rs"]
mod build_provenance;

use build_provenance::{
    CheckoutProvenance, ProvenanceError, ProvenanceSource, ResolvedProvenance,
    is_canonical_repository, resolve_provenance,
};

const REV_A: &str = "0123456789abcdef0123456789abcdef01234567";
const REV_B: &str = "89abcdef0123456789abcdef0123456789abcdef";

const EXPECTED_PUBLIC_SCHEMA_PATHS: [&str; 4] = [
    "schema/0.9.0-alpha/kit.schema.json",
    "schema/0.9.0-alpha/registry-item.schema.json",
    "schema/0.9.0-alpha/registry.schema.json",
    "schema/0.9.0-alpha/theme-contract.schema.json",
];

const EXPECTED_PACKAGE_SUPPORT: [&str; 7] = [
    ".cargo_vcs_info.json",
    "Cargo.lock",
    "Cargo.toml",
    "Cargo.toml.orig",
    "README.md",
    "build.rs",
    "build_provenance.rs",
];

const EXPECTED_PACKAGE_SOURCES: [&str; 6] = [
    "src/config.rs",
    "src/detect.rs",
    "src/item.rs",
    "src/lib.rs",
    "src/registry_health.rs",
    "src/theme_contract.rs",
];

const EXPECTED_PACKAGE_TESTS: [&str; 6] = [
    "tests/fixtures/theme_refactor_compatibility.json",
    "tests/fixtures/theme_refactor_mapping.json",
    "tests/packaged_runtime_boundaries.rs",
    "tests/registry_schema.rs",
    "tests/theme_refactor_compatibility.rs",
    "tests/theme_refactor_mapping.rs",
];

const EXPECTED_REGISTRY_ASSETS: [&str; 65] = [
    "contracts/theme-contract.schema.json",
    "contracts/theme-v1.json",
    "foundation/tokens.json",
    "registry.json",
    "styles/anchor.css",
    "styles/button.css",
    "styles/collapsible.css",
    "styles/dialog.css",
    "styles/field.css",
    "styles/menu.css",
    "styles/spinner.css",
    "styles/status.css",
    "styles/tabs.css",
    "styles/tokens.css",
    "ui/anchor.json",
    "ui/anchor.rs",
    "ui/button.json",
    "ui/button.rs",
    "ui/collapsible.json",
    "ui/collapsible/content.rs",
    "ui/collapsible/mod.rs",
    "ui/collapsible/root.rs",
    "ui/collapsible/trigger.rs",
    "ui/dialog.json",
    "ui/dialog/close.rs",
    "ui/dialog/content.rs",
    "ui/dialog/description.rs",
    "ui/dialog/mod.rs",
    "ui/dialog/root.rs",
    "ui/dialog/title.rs",
    "ui/dialog/trigger.rs",
    "ui/field.json",
    "ui/field/label.rs",
    "ui/field/message.rs",
    "ui/field/mod.rs",
    "ui/field/native_select.rs",
    "ui/field/required.rs",
    "ui/field/root.rs",
    "ui/field/select_field.rs",
    "ui/field/slot.rs",
    "ui/field/surface.rs",
    "ui/field/text_area.rs",
    "ui/field/text_area_field.rs",
    "ui/field/text_field.rs",
    "ui/field/text_input.rs",
    "ui/menu.json",
    "ui/menu/content.rs",
    "ui/menu/item.rs",
    "ui/menu/item_indicator.rs",
    "ui/menu/mod.rs",
    "ui/menu/radio_item.rs",
    "ui/menu/root.rs",
    "ui/menu/trigger.rs",
    "ui/router-link.json",
    "ui/router_link.rs",
    "ui/spinner.json",
    "ui/spinner.rs",
    "ui/status.json",
    "ui/status.rs",
    "ui/tabs.json",
    "ui/tabs/list.rs",
    "ui/tabs/mod.rs",
    "ui/tabs/panel.rs",
    "ui/tabs/root.rs",
    "ui/tabs/trigger.rs",
];

#[test]
fn provenance_precedence_is_explicit_then_cargo_then_checkout() {
    let cargo = format!(r#"{{"git":{{"sha1":"{REV_B}"}}}}"#);
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
    let cargo = format!(r#"{{"git":{{"sha1":"{REV_B}"}}}}"#);
    let checkout = CheckoutProvenance {
        remote: "https://github.com/triesap/leptos_ui_kit.git",
        rev: REV_B,
    };
    assert!(matches!(
        resolve_provenance(Some("short"), Some(&cargo), Some(checkout)),
        Err(ProvenanceError::InvalidRevision {
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
        Err(ProvenanceError::InvalidRevision {
            source: ProvenanceSource::Explicit,
            ..
        })
    ));

    let invalid_cargo_vcs = [
        "not json".to_owned(),
        r#"{}"#.to_owned(),
        r#"{"git":{"sha1":42}}"#.to_owned(),
        r#"{"git":{"sha1":"short"}}"#.to_owned(),
        format!(r#"{{"git":{{"sha1":"{}"}}}}"#, "z".repeat(40)),
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
        Err(ProvenanceError::InvalidRevision {
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
    let cargo = format!(
        r#"{{"git":{{"sha1":"{}","dirty":false}},"path_in_vcs":"crates/leptos_ui_kit_registry"}}"#,
        REV_A.to_ascii_uppercase()
    );
    assert_eq!(
        resolve_provenance(None, Some(&cargo), None),
        Ok(Some(ResolvedProvenance {
            rev: REV_A.to_owned(),
            source: ProvenanceSource::CargoVcs,
        }))
    );
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
        "https://github.com/triesap/dev.git",
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
fn hostile_parent_checkout_is_not_a_provenance_source() {
    assert_eq!(
        resolve_provenance(
            None,
            None,
            Some(CheckoutProvenance {
                remote: "https://github.com/triesap/dev.git",
                rev: REV_A,
            })
        ),
        Ok(None)
    );
}

#[test]
fn compiled_tool_provenance_is_valid_or_explicitly_unavailable() {
    match canonical_tool_config() {
        Ok(tool) => {
            assert_eq!(tool.package, TOOL_PACKAGE);
            assert_eq!(tool.binary, TOOL_BINARY);
            let ToolSourceConfig::Git { url, rev } = tool.source;
            assert_eq!(url, TOOL_GIT_URL);
            assert_eq!(rev.len(), 40);
            assert!(rev.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
        Err(ConfigError::MissingToolProvenance { package, binary }) => {
            assert_eq!(package, TOOL_PACKAGE);
            assert_eq!(binary, TOOL_BINARY);
        }
        Err(error) => panic!("unexpected compiled provenance error: {error}"),
    }
}

#[test]
fn authoring_registry_inventory_is_exact_safe_and_portable() {
    let registry_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("registry");
    let mut actual = Vec::new();
    collect_files(&registry_root, &registry_root, &mut actual);
    actual.sort();

    assert_eq!(actual, EXPECTED_REGISTRY_ASSETS);
    let mut case_folded = BTreeSet::new();
    for path in actual {
        assert!(!path.starts_with('/'));
        assert!(!path.split('/').any(|segment| segment == ".."));
        assert!(case_folded.insert(path.to_ascii_lowercase()), "{path}");
    }
    validate_built_in_registry_health().expect("built-in registry health");
}

#[test]
fn approved_embedded_inventory_has_68_unique_logical_assets() {
    let logical_assets = EXPECTED_REGISTRY_ASSETS
        .iter()
        .filter(|path| **path != "contracts/theme-contract.schema.json")
        .map(|path| format!("registry/{path}"))
        .chain(EXPECTED_PUBLIC_SCHEMA_PATHS.map(str::to_owned))
        .collect::<BTreeSet<_>>();

    assert_eq!(logical_assets.len(), 68);
    assert!(logical_assets.contains("registry/registry.json"));
    assert!(logical_assets.contains("registry/contracts/theme-v1.json"));
    for schema in EXPECTED_PUBLIC_SCHEMA_PATHS {
        assert!(logical_assets.contains(schema), "{schema}");
    }
    assert!(!logical_assets.contains("registry/contracts/theme-contract.schema.json"));
}

#[test]
fn registry_crate_package_inventory_is_exact_and_freezes_missing_schema_debt() {
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

    let actual = String::from_utf8(output.stdout)
        .expect("UTF-8 cargo package list")
        .lines()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let expected = EXPECTED_PACKAGE_SUPPORT
        .iter()
        .map(|path| (*path).to_owned())
        .chain(
            EXPECTED_REGISTRY_ASSETS
                .iter()
                .map(|path| format!("registry/{path}")),
        )
        .chain(EXPECTED_PACKAGE_SOURCES.map(str::to_owned))
        .chain(EXPECTED_PACKAGE_TESTS.map(str::to_owned))
        .collect::<BTreeSet<_>>();

    assert_eq!(expected.len(), 84);
    assert_eq!(actual, expected);
    for schema in EXPECTED_PUBLIC_SCHEMA_PATHS {
        assert!(
            !actual.contains(schema),
            "unexpected packaged schema: {schema}"
        );
    }
}

#[test]
fn current_item_source_path_characterizes_the_absolute_path_debt() {
    let item = load_built_in_registry_item("button").expect("load button");
    let expected = Path::new(env!("CARGO_MANIFEST_DIR")).join("registry/ui/button.json");
    assert!(item.source_path.is_absolute());
    assert_eq!(item.source_path, expected);
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
