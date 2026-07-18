#![forbid(unsafe_code)]

use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    process::Output,
};

use serde_json::Value;
use tempfile::tempdir;

#[allow(dead_code)]
#[path = "../build_assets.rs"]
mod build_assets;
#[path = "../../../tests/support/package_workspace.rs"]
mod package_workspace_support;

use build_assets::ASSET_SPECS;
use package_workspace_support::{
    assert_full_revision, assert_local_package_metadata, assert_success,
    cargo_command as extracted_cargo, contains_bytes,
    extract_workspace as extract_package_workspace, git_command, package_workspace, source_head,
    workspace_root,
};

const PACKAGE_BASE: [&str; 5] = [
    ".cargo_vcs_info.json",
    "Cargo.lock",
    "Cargo.toml",
    "Cargo.toml.orig",
    "README.md",
];

const REGISTRY_SUPPORT: [&str; 3] = ["build.rs", "build_assets.rs", "build_provenance.rs"];

const REGISTRY_SOURCES: [&str; 8] = [
    "src/builtin_registry.rs",
    "src/config.rs",
    "src/detect.rs",
    "src/embedded_assets.rs",
    "src/item.rs",
    "src/lib.rs",
    "src/registry_health.rs",
    "src/theme_contract.rs",
];

const REGISTRY_TESTS: [&str; 7] = [
    "tests/asset_catalog.rs",
    "tests/fixtures/theme_refactor_compatibility.json",
    "tests/fixtures/theme_refactor_mapping.json",
    "tests/packaged_runtime_boundaries.rs",
    "tests/registry_schema.rs",
    "tests/theme_refactor_compatibility.rs",
    "tests/theme_refactor_mapping.rs",
];

const FACADE_FILES: [&str; 1] = ["src/lib.rs"];
const PRIMITIVES_FILES: [&str; 1] = ["src/lib.rs"];
const CODEGEN_FILES: [&str; 29] = [
    "src/command.rs",
    "src/digest.rs",
    "src/error.rs",
    "src/install_lock.rs",
    "src/lib.rs",
    "src/orchestration.rs",
    "src/patch/css.rs",
    "src/patch/html.rs",
    "src/patch/mod.rs",
    "src/patch/module.rs",
    "src/path_safety.rs",
    "src/planning/add.rs",
    "src/planning/files.rs",
    "src/planning/init.rs",
    "src/planning/mod.rs",
    "src/planning/sync.rs",
    "src/tests.rs",
    "src/transaction/fs.rs",
    "src/transaction/journal.rs",
    "src/transaction/lock.rs",
    "src/transaction/mod.rs",
    "src/transaction/replace.rs",
    "src/transaction/runtime.rs",
    "src/transaction/store.rs",
    "tests/fixtures/theme_pre_refactor_06124efa/button.css",
    "tests/fixtures/theme_pre_refactor_06124efa/spinner.css",
    "tests/public_api.rs",
    "tests/transaction_process.rs",
    "tests/transaction_security.rs",
];
const CLI_FILES: [&str; 11] = [
    "src/bin/cargo-leptos_ui_kit.rs",
    "src/lib.rs",
    "src/main.rs",
    "tests/fixtures/homepage_trunk_csr/Cargo.toml.fixture",
    "tests/fixtures/homepage_trunk_csr/index.html",
    "tests/fixtures/homepage_trunk_csr/src/main.rs",
    "tests/fixtures/homepage_trunk_csr/styles/app.css",
    "tests/fixtures/homepage_trunk_csr/styles/themes.css",
    "tests/fixtures/theme_pre_refactor_06124efa/button.css",
    "tests/fixtures/theme_pre_refactor_06124efa/spinner.css",
    "tests/workflow.rs",
];

const PACKAGES: [PackageSpec; 5] = [
    PackageSpec {
        name: "leptos_ui_kit",
        path_in_vcs: "crates/leptos_ui_kit",
        files: &FACADE_FILES,
    },
    PackageSpec {
        name: "leptos_ui_kit_cli",
        path_in_vcs: "crates/leptos_ui_kit_cli",
        files: &CLI_FILES,
    },
    PackageSpec {
        name: "leptos_ui_kit_codegen",
        path_in_vcs: "crates/leptos_ui_kit_codegen",
        files: &CODEGEN_FILES,
    },
    PackageSpec {
        name: "leptos_ui_kit_primitives",
        path_in_vcs: "crates/leptos_ui_kit_primitives",
        files: &PRIMITIVES_FILES,
    },
    PackageSpec {
        name: "leptos_ui_kit_registry",
        path_in_vcs: "crates/leptos_ui_kit_registry",
        files: &[],
    },
];

#[derive(Clone, Copy)]
struct PackageSpec {
    name: &'static str,
    path_in_vcs: &'static str,
    files: &'static [&'static str],
}

#[test]
#[ignore = "slow package-source acceptance; run with --ignored --exact"]
fn packaged_sources_build_with_cargo_vcs_provenance_outside_and_inside_hostile_git() {
    let source_root = workspace_root(env!("CARGO_MANIFEST_DIR"));
    let temporary = tempdir().expect("create package-source acceptance root");
    let archive_target = temporary.path().join("archive-target");
    let archives = package_workspace(&source_root, &archive_target);
    let approved_rev = source_head(&source_root);
    let approved_lock = fs::read(source_root.join("Cargo.lock"))
        .expect("read the tracked package-source dependency lock");

    let clean_workspace = temporary.path().join("clean/workspace");
    let clean_revisions = extract_workspace(&archives, &clean_workspace, &source_root);
    assert_eq!(clean_revisions, BTreeSet::from([approved_rev.clone()]));
    assert_outside_git(&clean_workspace);

    let hostile_parent = temporary.path().join("hostile-parent");
    let hostile_rev = initialize_hostile_parent(&hostile_parent);
    assert_ne!(hostile_rev, approved_rev);
    let hostile_workspace = hostile_parent.clone();
    let hostile_revisions = extract_workspace(&archives, &hostile_workspace, &source_root);
    assert_eq!(hostile_revisions, BTreeSet::from([approved_rev.clone()]));
    assert_hostile_parent(&hostile_workspace, &hostile_parent);

    let cargo_home = temporary.path().join("isolated-cargo-home");
    fs::create_dir_all(&cargo_home).expect("create isolated Cargo home");
    seed_extracted_lock(&clean_workspace, &approved_lock);
    seed_extracted_lock(&hostile_workspace, &approved_lock);
    run_extracted_suite(
        "clean extracted workspace",
        &clean_workspace,
        &cargo_home,
        &temporary.path().join("clean-target"),
        &approved_rev,
        &[temporary.path()],
    );
    run_extracted_suite(
        "hostile-parent extracted workspace",
        &hostile_workspace,
        &cargo_home,
        &temporary.path().join("hostile-target"),
        &approved_rev,
        &[temporary.path()],
    );
    for (label, workspace) in [("clean", &clean_workspace), ("hostile", &hostile_workspace)] {
        assert_eq!(
            fs::read(workspace.join("Cargo.lock"))
                .unwrap_or_else(|error| panic!("read {label} extracted lock: {error}")),
            approved_lock,
            "{label} package workspace changed the approved dependency lock"
        );
    }
}

fn extract_workspace(
    archives: &std::collections::BTreeMap<&str, PathBuf>,
    workspace: &Path,
    source_root: &Path,
) -> BTreeSet<String> {
    let extracted_packages = extract_package_workspace(archives, workspace);
    let mut revisions = BTreeSet::new();

    for spec in PACKAGES {
        let extracted = extracted_packages
            .get(spec.name)
            .unwrap_or_else(|| panic!("missing extracted package for {}", spec.name));
        let actual = collect_package_files(extracted);
        let expected = expected_inventory(spec);
        assert_eq!(actual, expected, "exact {} archive inventory", spec.name);

        for path in &actual {
            let bytes = fs::read(extracted.join(path))
                .unwrap_or_else(|error| panic!("read {} {path}: {error}", spec.name));
            assert!(
                !contains_bytes(&bytes, source_root.to_string_lossy().as_bytes()),
                "{} archive leaks source checkout path in {path}",
                spec.name
            );
        }

        let vcs_path = extracted.join(".cargo_vcs_info.json");
        let vcs: Value = serde_json::from_slice(
            &fs::read(&vcs_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", vcs_path.display())),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", vcs_path.display()));
        assert_eq!(
            vcs.get("path_in_vcs").and_then(Value::as_str),
            Some(spec.path_in_vcs),
            "{} archive path_in_vcs",
            spec.name
        );
        assert!(
            vcs.pointer("/git/dirty").is_none(),
            "{} clean archive must omit Cargo's dirty marker",
            spec.name
        );
        let rev = vcs["git"]["sha1"]
            .as_str()
            .unwrap_or_else(|| panic!("{} archive git.sha1", spec.name));
        assert_full_revision(rev);
        revisions.insert(rev.to_owned());
    }

    revisions
}

fn expected_inventory(spec: PackageSpec) -> BTreeSet<String> {
    let mut expected = PACKAGE_BASE
        .iter()
        .chain(spec.files.iter())
        .map(|path| (*path).to_owned())
        .collect::<BTreeSet<_>>();
    if spec.name == "leptos_ui_kit_registry" {
        expected.extend(REGISTRY_SUPPORT.map(str::to_owned));
        expected.extend(REGISTRY_SOURCES.map(str::to_owned));
        expected.extend(REGISTRY_TESTS.map(str::to_owned));
        expected.extend(ASSET_SPECS.map(|asset| asset.source_path.to_owned()));
        assert_eq!(expected.len(), 91);
    }
    expected
}

fn collect_package_files(root: &Path) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    collect_package_files_from(root, root, &mut files);
    files
}

fn collect_package_files_from(root: &Path, directory: &Path, files: &mut BTreeSet<String>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .expect("read package entries");
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("inspect {}: {error}", path.display()));
        assert!(!metadata.file_type().is_symlink(), "{}", path.display());
        if metadata.is_dir() {
            collect_package_files_from(root, &path, files);
        } else {
            assert!(metadata.is_file(), "{}", path.display());
            let relative = path.strip_prefix(root).expect("package-relative path");
            assert!(
                relative
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
            );
            files.insert(logical_path(relative));
        }
    }
}

fn logical_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_str().expect("UTF-8 packaged path"))
        .collect::<Vec<_>>()
        .join("/")
}

fn assert_outside_git(workspace: &Path) {
    let output = git_command(workspace)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .expect("probe clean extraction for ambient Git");
    assert!(
        !output.status.success(),
        "clean extraction unexpectedly belongs to {}",
        String::from_utf8_lossy(&output.stdout).trim()
    );
}

fn initialize_hostile_parent(parent: &Path) -> String {
    fs::create_dir_all(parent).expect("create hostile parent");
    let init = git_command(parent)
        .args(["init", "--quiet"])
        .output()
        .expect("initialize hostile Git parent");
    assert_success("git init hostile parent", &init);
    let remote = git_command(parent)
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/example/unrelated.git",
        ])
        .output()
        .expect("configure hostile parent origin");
    assert_success("git remote add hostile origin", &remote);
    fs::write(parent.join("hostile.txt"), "unrelated parent repository\n")
        .expect("write hostile parent fixture");
    let add = git_command(parent)
        .args(["add", "hostile.txt"])
        .output()
        .expect("stage hostile parent fixture");
    assert_success("git add hostile fixture", &add);
    let commit = git_command(parent)
        .env("GIT_AUTHOR_NAME", "Package Source Test")
        .env("GIT_AUTHOR_EMAIL", "package-source@example.invalid")
        .env("GIT_COMMITTER_NAME", "Package Source Test")
        .env("GIT_COMMITTER_EMAIL", "package-source@example.invalid")
        .args([
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "--no-verify",
            "--no-gpg-sign",
            "-m",
            "hostile parent",
        ])
        .output()
        .expect("commit hostile parent fixture");
    assert_success("git commit hostile fixture", &commit);
    source_head(parent)
}

fn assert_hostile_parent(workspace: &Path, hostile_parent: &Path) {
    let output = git_command(workspace)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .expect("probe hostile extraction parent");
    assert_success("git rev-parse hostile parent", &output);
    let actual = PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("UTF-8 hostile parent path")
            .trim(),
    )
    .canonicalize()
    .expect("canonical hostile parent result");
    assert_eq!(
        actual,
        hostile_parent
            .canonicalize()
            .expect("canonical hostile parent")
    );
}

fn run_extracted_suite(
    label: &str,
    workspace: &Path,
    cargo_home: &Path,
    target_dir: &Path,
    expected_rev: &str,
    forbidden_paths: &[&Path],
) {
    assert_local_package_metadata(label, workspace, cargo_home, target_dir);

    let tests = extracted_cargo(workspace, cargo_home, target_dir)
        .args(["test", "--workspace", "--all-targets", "--locked"])
        .output()
        .unwrap_or_else(|error| panic!("run {label} all-target tests: {error}"));
    assert_success(&format!("{label} all-target tests"), &tests);

    let direct = run_version(workspace, cargo_home, target_dir, "leptos_ui_kit", &[]);
    let cargo_wrapper = run_version(
        workspace,
        cargo_home,
        target_dir,
        "cargo-leptos_ui_kit",
        &["leptos_ui_kit"],
    );
    assert_eq!(
        direct.stdout, cargo_wrapper.stdout,
        "{label} binary wrappers"
    );
    assert_version_output(label, &direct, expected_rev, forbidden_paths);
    assert_version_output(label, &cargo_wrapper, expected_rev, forbidden_paths);
}

fn seed_extracted_lock(workspace: &Path, approved_lock: &[u8]) {
    // Cargo packages carry closure-specific locks. The combined extracted
    // workspace must use the tracked all-package graph validated by --locked.
    fs::write(workspace.join("Cargo.lock"), approved_lock)
        .unwrap_or_else(|error| panic!("seed {} workspace lock: {error}", workspace.display()));
}

fn run_version(
    workspace: &Path,
    cargo_home: &Path,
    target_dir: &Path,
    binary: &str,
    prefix: &[&str],
) -> Output {
    let mut command = extracted_cargo(workspace, cargo_home, target_dir);
    command.args([
        "run",
        "--quiet",
        "-p",
        "leptos_ui_kit_cli",
        "--bin",
        binary,
        "--locked",
        "--",
    ]);
    command.args(prefix).args(["--version", "--json"]);
    command
        .output()
        .unwrap_or_else(|error| panic!("run extracted {binary}: {error}"))
}

fn assert_version_output(label: &str, output: &Output, expected_rev: &str, forbidden: &[&Path]) {
    assert_success(&format!("{label} version"), output);
    let stdout = String::from_utf8(output.stdout.clone()).expect("UTF-8 extracted version stdout");
    let stderr = String::from_utf8(output.stderr.clone()).expect("UTF-8 extracted version stderr");
    let value: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("parse {label} version JSON: {error}\n{stdout}"));
    assert_eq!(value["command"], "version");
    assert_eq!(value["status"], "success");
    assert_eq!(value["data"]["source"]["kind"], "git");
    assert_eq!(value["data"]["source"]["rev"], expected_rev);
    for path in forbidden {
        let path = path.to_string_lossy();
        assert!(
            !stdout.contains(path.as_ref()),
            "{label} stdout leaked {path}"
        );
        assert!(
            !stderr.contains(path.as_ref()),
            "{label} stderr leaked {path}"
        );
    }
}
