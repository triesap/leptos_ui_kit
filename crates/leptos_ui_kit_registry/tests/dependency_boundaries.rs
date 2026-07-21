#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

const CSS_PARSER_PACKAGE: &str = "cssparser";
const AUDIT_OWNER: &str = "leptos_ui_kit_registry";
const PUBLISHABLE_PACKAGES: [&str; 6] = [
    "leptos_ui_kit",
    "leptos_ui_kit_cli",
    "leptos_ui_kit_codegen",
    "leptos_ui_kit_codegen_platform",
    "leptos_ui_kit_primitives",
    "leptos_ui_kit_registry",
];

#[test]
fn css_parser_is_direct_dev_only_and_absent_from_runtime_graphs() {
    let workspace = workspace_root();
    let output = Command::new(env!("CARGO"))
        .current_dir(&workspace)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--all-features",
            "--manifest-path",
        ])
        .arg(workspace.join("Cargo.toml"))
        .output()
        .expect("run cargo metadata for dependency-boundary audit");
    assert!(
        output.status.success(),
        "cargo metadata failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("parse cargo metadata");
    let packages = metadata["packages"].as_array().expect("metadata packages");

    let owner = packages
        .iter()
        .find(|package| package["name"] == AUDIT_OWNER)
        .expect("registry package metadata");
    let parser_dependencies = owner["dependencies"]
        .as_array()
        .expect("registry dependencies")
        .iter()
        .filter(|dependency| dependency["name"] == CSS_PARSER_PACKAGE)
        .collect::<Vec<_>>();
    assert_eq!(
        parser_dependencies.len(),
        1,
        "{CSS_PARSER_PACKAGE} must be one direct dependency of {AUDIT_OWNER}"
    );
    assert_eq!(
        parser_dependencies[0]["kind"], "dev",
        "{CSS_PARSER_PACKAGE} must be a dev-only dependency of {AUDIT_OWNER}"
    );

    let package_names = packages
        .iter()
        .map(|package| {
            (
                package["id"].as_str().expect("package id").to_owned(),
                package["name"].as_str().expect("package name").to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let workspace_ids = packages
        .iter()
        .filter_map(|package| {
            let name = package["name"].as_str()?;
            PUBLISHABLE_PACKAGES.contains(&name).then(|| {
                (
                    name.to_owned(),
                    package["id"]
                        .as_str()
                        .expect("workspace package id")
                        .to_owned(),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        workspace_ids.len(),
        PUBLISHABLE_PACKAGES.len(),
        "resolved metadata must contain every publishable package"
    );

    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolved metadata nodes")
        .iter()
        .map(|node| {
            (
                node["id"].as_str().expect("node id").to_owned(),
                node.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    for (name, id) in workspace_ids {
        let reachable = runtime_reachable(&id, &nodes);
        let parser_paths = reachable
            .iter()
            .filter(|reachable_id| {
                package_names
                    .get(*reachable_id)
                    .is_some_and(|package| package == CSS_PARSER_PACKAGE)
            })
            .collect::<Vec<_>>();
        assert!(
            parser_paths.is_empty(),
            "{name} has a normal/build dependency path to {CSS_PARSER_PACKAGE}: {parser_paths:?}"
        );
    }
}

fn runtime_reachable(root: &str, nodes: &BTreeMap<String, Value>) -> BTreeSet<String> {
    let mut pending = vec![root.to_owned()];
    let mut reachable = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !reachable.insert(id.clone()) {
            continue;
        }
        let node = nodes
            .get(&id)
            .unwrap_or_else(|| panic!("missing resolved node {id}"));
        for dependency in node["deps"].as_array().expect("resolved node dependencies") {
            let has_runtime_kind = dependency["dep_kinds"]
                .as_array()
                .expect("dependency kinds")
                .iter()
                .any(|kind| {
                    kind["kind"].is_null()
                        || matches!(kind["kind"].as_str(), Some("normal" | "build"))
                });
            if has_runtime_kind {
                pending.push(
                    dependency["pkg"]
                        .as_str()
                        .expect("resolved dependency package id")
                        .to_owned(),
                );
            }
        }
    }
    reachable
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical workspace root")
}
