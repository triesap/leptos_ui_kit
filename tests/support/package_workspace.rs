use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use flate2::read::GzDecoder;
use serde_json::Value;
use tar::Archive;

#[allow(dead_code)]
#[path = "../../crates/leptos_ui_kit_registry/build_provenance.rs"]
mod build_provenance;

use build_provenance::GIT_REPOSITORY_OVERRIDE_ENV;

pub const PACKAGE_NAMES: [&str; 5] = [
    "leptos_ui_kit",
    "leptos_ui_kit_cli",
    "leptos_ui_kit_codegen",
    "leptos_ui_kit_primitives",
    "leptos_ui_kit_registry",
];

const PATCHED_PACKAGE_NAMES: [&str; 3] = [
    "leptos_ui_kit_registry",
    "leptos_ui_kit_codegen",
    "leptos_ui_kit_primitives",
];

const PROVENANCE_REV_ENV: &str = "LEPTOS_UI_KIT_GIT_REV";
const PROVENANCE_SOURCE_ENV: &str = "LEPTOS_UI_KIT_GIT_REV_SOURCE";

pub fn workspace_root(manifest_dir: &str) -> PathBuf {
    Path::new(manifest_dir)
        .join("../..")
        .canonicalize()
        .expect("canonical workspace root")
}

pub fn package_workspace(source_root: &Path, target_dir: &Path) -> BTreeMap<&'static str, PathBuf> {
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
    sanitize_git_environment(&mut command);
    let output = command
        .output()
        .expect("run cargo package for workspace archives");
    assert_success("cargo package --workspace", &output);

    let package_dir = target_dir.join("package");
    let expected_names = PACKAGE_NAMES
        .iter()
        .map(|name| format!("{name}-{}.crate", env!("CARGO_PKG_VERSION")))
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

    PACKAGE_NAMES
        .iter()
        .map(|name| {
            (
                *name,
                package_dir.join(format!("{name}-{}.crate", env!("CARGO_PKG_VERSION"))),
            )
        })
        .collect()
}

pub fn extract_workspace(
    archives: &BTreeMap<&str, PathBuf>,
    workspace: &Path,
) -> BTreeMap<&'static str, PathBuf> {
    let package_root = workspace.join("crates");
    let staging_root = workspace.join(".package-extract");
    fs::create_dir_all(&package_root).expect("create extracted package root");
    fs::create_dir_all(&staging_root).expect("create package extraction staging root");
    let mut extracted_packages = BTreeMap::new();

    for name in PACKAGE_NAMES {
        let archive_path = archives
            .get(name)
            .unwrap_or_else(|| panic!("missing archive for {name}"));
        let file = fs::File::open(archive_path)
            .unwrap_or_else(|error| panic!("open {}: {error}", archive_path.display()));
        Archive::new(GzDecoder::new(file))
            .unpack(&staging_root)
            .unwrap_or_else(|error| panic!("extract {}: {error}", archive_path.display()));

        let archive_root = staging_root.join(format!("{name}-{}", env!("CARGO_PKG_VERSION")));
        let extracted = package_root.join(name);
        fs::rename(&archive_root, &extracted).unwrap_or_else(|error| {
            panic!(
                "move {} to canonical package layout {}: {error}",
                archive_root.display(),
                extracted.display()
            )
        });
        extracted_packages.insert(name, extracted);
    }

    fs::remove_dir(&staging_root).expect("remove empty package extraction staging root");
    write_synthetic_manifest(workspace);
    extracted_packages
}

pub fn cargo_command(workspace: &Path, cargo_home: &Path, target_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(workspace)
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_TARGET_DIR", target_dir)
        .env_remove(PROVENANCE_REV_ENV)
        .env_remove(PROVENANCE_SOURCE_ENV);
    sanitize_command_environment(&mut command);
    command
}

pub fn assert_local_package_metadata(
    label: &str,
    workspace: &Path,
    cargo_home: &Path,
    target_dir: &Path,
) {
    let output = cargo_command(workspace, cargo_home, target_dir)
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .unwrap_or_else(|error| panic!("read {label} package metadata: {error}"));
    assert_success(&format!("{label} package metadata"), &output);
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("parse {label} package metadata: {error}"));
    let packages = metadata["packages"]
        .as_array()
        .unwrap_or_else(|| panic!("{label} package metadata packages"));

    for name in PACKAGE_NAMES {
        let matches = packages
            .iter()
            .filter(|package| package["name"] == name)
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "{label} must resolve exactly one local {name} package"
        );
        assert!(
            matches[0]["source"].is_null(),
            "{label} resolved {name} from a registry instead of its extracted archive"
        );
        let manifest = PathBuf::from(
            matches[0]["manifest_path"]
                .as_str()
                .unwrap_or_else(|| panic!("{label} {name} manifest path")),
        )
        .canonicalize()
        .unwrap_or_else(|error| panic!("canonical {name} manifest: {error}"));
        assert_eq!(
            manifest,
            workspace
                .join("crates")
                .join(name)
                .join("Cargo.toml")
                .canonicalize()
                .unwrap_or_else(|error| panic!("canonical expected {name} manifest: {error}"))
        );
    }
}

pub fn git_command(directory: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(directory);
    sanitize_git_environment(&mut command);
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

pub fn sanitize_command_environment(command: &mut Command) {
    command
        .env_remove(PROVENANCE_REV_ENV)
        .env_remove(PROVENANCE_SOURCE_ENV)
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTFLAGS");
    sanitize_git_environment(command);
}

pub fn source_head(source_root: &Path) -> String {
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

pub fn assert_full_revision(rev: &str) {
    assert_eq!(rev.len(), 40, "complete Git object ID: {rev}");
    assert!(
        rev.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "lowercase hexadecimal Git object ID: {rev}"
    );
}

pub fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[allow(dead_code)]
pub fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn write_synthetic_manifest(workspace: &Path) {
    let members = PACKAGE_NAMES
        .iter()
        .map(|name| format!("    \"crates/{name}\","))
        .collect::<Vec<_>>()
        .join("\n");
    let patches = PATCHED_PACKAGE_NAMES
        .iter()
        .map(|name| format!("{name} = {{ path = \"crates/{name}\" }}"))
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

fn sanitize_git_environment(command: &mut Command) {
    for name in GIT_REPOSITORY_OVERRIDE_ENV {
        command.env_remove(name);
    }
}
