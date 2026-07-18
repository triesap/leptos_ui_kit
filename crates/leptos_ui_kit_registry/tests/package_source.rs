#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

use flate2::read::GzDecoder;
use serde_json::Value;
use tar::Archive;
use tempfile::tempdir;

#[allow(dead_code)]
#[path = "../build_assets.rs"]
mod build_assets;
#[allow(dead_code)]
#[path = "../build_provenance.rs"]
mod build_provenance;

use build_assets::ASSET_SPECS;
use build_provenance::GIT_REPOSITORY_OVERRIDE_ENV;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PROVENANCE_REV_ENV: &str = "LEPTOS_UI_KIT_GIT_REV";
const PROVENANCE_SOURCE_ENV: &str = "LEPTOS_UI_KIT_GIT_REV_SOURCE";

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
const CODEGEN_FILES: [&str; 3] = [
    "src/lib.rs",
    "tests/fixtures/theme_pre_refactor_06124efa/button.css",
    "tests/fixtures/theme_pre_refactor_06124efa/spinner.css",
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
    let source_root = workspace_root();
    let temporary = tempdir().expect("create package-source acceptance root");
    let archive_target = temporary.path().join("archive-target");
    let archives = package_workspace(&source_root, &archive_target);
    let approved_rev = source_head(&source_root);

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
    assert_eq!(
        fs::read(clean_workspace.join("Cargo.lock")).expect("read clean extracted lock"),
        fs::read(hostile_workspace.join("Cargo.lock")).expect("read hostile extracted lock"),
        "clean and hostile package workspaces must resolve identical dependency locks"
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical workspace root")
}

fn package_workspace(source_root: &Path, target_dir: &Path) -> BTreeMap<&'static str, PathBuf> {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(source_root)
        .args([
            "package",
            "--workspace",
            "--allow-dirty",
            "--no-verify",
            "--locked",
            "--target-dir",
        ])
        .arg(target_dir)
        .env_remove(PROVENANCE_REV_ENV)
        .env_remove(PROVENANCE_SOURCE_ENV);
    for name in GIT_REPOSITORY_OVERRIDE_ENV {
        command.env_remove(name);
    }
    let output = command
        .output()
        .expect("run cargo package for workspace archives");
    assert_success("cargo package --workspace", &output);

    let package_dir = target_dir.join("package");
    let expected_names = PACKAGES
        .iter()
        .map(|spec| format!("{}-{VERSION}.crate", spec.name))
        .collect::<BTreeSet<_>>();
    let actual_names = fs::read_dir(&package_dir)
        .expect("read Cargo package output")
        .filter_map(|entry| {
            let entry = entry.expect("read Cargo package entry");
            let metadata = entry.metadata().expect("inspect Cargo package entry");
            if metadata.is_dir() {
                return None;
            }
            assert!(
                metadata.is_file(),
                "Cargo package outputs must be regular files"
            );
            Some(
                entry
                    .file_name()
                    .into_string()
                    .expect("UTF-8 Cargo package filename"),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_names, expected_names, "exact workspace archive set");

    PACKAGES
        .iter()
        .map(|spec| {
            (
                spec.name,
                package_dir.join(format!("{}-{VERSION}.crate", spec.name)),
            )
        })
        .collect()
}

fn extract_workspace(
    archives: &BTreeMap<&str, PathBuf>,
    workspace: &Path,
    source_root: &Path,
) -> BTreeSet<String> {
    let package_root = workspace.join("crates");
    let staging_root = workspace.join(".package-extract");
    fs::create_dir_all(&package_root).expect("create extracted package root");
    fs::create_dir_all(&staging_root).expect("create package extraction staging root");
    let mut revisions = BTreeSet::new();

    for spec in PACKAGES {
        let archive_path = archives
            .get(spec.name)
            .unwrap_or_else(|| panic!("missing archive for {}", spec.name));
        let file = fs::File::open(archive_path)
            .unwrap_or_else(|error| panic!("open {}: {error}", archive_path.display()));
        Archive::new(GzDecoder::new(file))
            .unpack(&staging_root)
            .unwrap_or_else(|error| panic!("extract {}: {error}", archive_path.display()));

        let archive_root = staging_root.join(package_directory(spec.name));
        let extracted = package_root.join(spec.name);
        fs::rename(&archive_root, &extracted).unwrap_or_else(|error| {
            panic!(
                "move {} to canonical package layout {}: {error}",
                archive_root.display(),
                extracted.display()
            )
        });
        let actual = collect_package_files(&extracted);
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
        assert_eq!(vcs["path_in_vcs"], spec.path_in_vcs);
        let rev = vcs["git"]["sha1"]
            .as_str()
            .unwrap_or_else(|| panic!("{} archive git.sha1", spec.name));
        assert_full_revision(rev);
        revisions.insert(rev.to_owned());
    }

    fs::remove_dir(&staging_root).expect("remove empty package extraction staging root");
    write_synthetic_manifest(workspace);
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

fn write_synthetic_manifest(workspace: &Path) {
    let members = PACKAGES
        .iter()
        .map(|spec| format!("    \"crates/{}\",", spec.name))
        .collect::<Vec<_>>()
        .join("\n");
    let patches = PACKAGES
        .iter()
        .filter(|spec| {
            matches!(
                spec.name,
                "leptos_ui_kit_registry" | "leptos_ui_kit_codegen" | "leptos_ui_kit_primitives"
            )
        })
        .map(|spec| format!("{} = {{ path = \"crates/{}\" }}", spec.name, spec.name))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        workspace.join("Cargo.toml"),
        format!(
            "[workspace]\nresolver = \"2\"\nmembers = [\n{members}\n]\n\n[patch.crates-io]\n{patches}\n"
        ),
    )
    .expect("write synthetic package workspace manifest");
}

fn package_directory(name: &str) -> String {
    format!("{name}-{VERSION}")
}

fn source_head(source_root: &Path) -> String {
    let output = git_command(source_root)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .expect("read package-source HEAD");
    assert_success("git rev-parse package-source HEAD", &output);
    let rev = String::from_utf8(output.stdout)
        .expect("UTF-8 package-source HEAD")
        .trim()
        .to_ascii_lowercase();
    assert_full_revision(&rev);
    rev
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
    fs::create_dir_all(cargo_home).expect("create isolated Cargo home");
    let lock = extracted_cargo(workspace, cargo_home, target_dir)
        .args(["generate-lockfile"])
        .output()
        .unwrap_or_else(|error| panic!("generate {label} lockfile: {error}"));
    assert_success(&format!("{label} lockfile generation"), &lock);

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

fn extracted_cargo(workspace: &Path, cargo_home: &Path, target_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(workspace)
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_TARGET_DIR", target_dir)
        .env_remove(PROVENANCE_REV_ENV)
        .env_remove(PROVENANCE_SOURCE_ENV)
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTFLAGS");
    for name in GIT_REPOSITORY_OVERRIDE_ENV {
        command.env_remove(name);
    }
    command
}

fn assert_local_package_metadata(
    label: &str,
    workspace: &Path,
    cargo_home: &Path,
    target_dir: &Path,
) {
    let output = extracted_cargo(workspace, cargo_home, target_dir)
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .unwrap_or_else(|error| panic!("read {label} package metadata: {error}"));
    assert_success(&format!("{label} package metadata"), &output);
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("parse {label} package metadata: {error}"));
    let packages = metadata["packages"]
        .as_array()
        .unwrap_or_else(|| panic!("{label} package metadata packages"));

    for spec in PACKAGES {
        let matches = packages
            .iter()
            .filter(|package| package["name"] == spec.name)
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "{label} must resolve exactly one local {} package",
            spec.name
        );
        assert!(
            matches[0]["source"].is_null(),
            "{label} resolved {} from a registry instead of its extracted archive",
            spec.name
        );
        let manifest = PathBuf::from(
            matches[0]["manifest_path"]
                .as_str()
                .unwrap_or_else(|| panic!("{label} {} manifest path", spec.name)),
        )
        .canonicalize()
        .unwrap_or_else(|error| panic!("canonical {} manifest: {error}", spec.name));
        assert_eq!(
            manifest,
            workspace
                .join("crates")
                .join(spec.name)
                .join("Cargo.toml")
                .canonicalize()
                .unwrap_or_else(|error| panic!(
                    "canonical expected {} manifest: {error}",
                    spec.name
                ))
        );
    }
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

fn git_command(directory: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(directory);
    for name in GIT_REPOSITORY_OVERRIDE_ENV {
        command.env_remove(name);
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn assert_full_revision(rev: &str) {
    assert_eq!(rev.len(), 40, "complete Git object ID: {rev}");
    assert!(
        rev.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "lowercase hexadecimal Git object ID: {rev}"
    );
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
