#![forbid(unsafe_code)]

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use tempfile::tempdir;

#[test]
fn workspace_lockfile_is_tracked_and_not_ignored() {
    let root = workspace_root();
    let lockfile = root.join("Cargo.lock");

    assert!(lockfile.is_file(), "workspace Cargo.lock should exist");

    let tracked = Command::new("git")
        .current_dir(&root)
        .args(["ls-files", "--error-unmatch", "Cargo.lock"])
        .output()
        .expect("run git ls-files");

    assert!(
        tracked.status.success(),
        "Cargo.lock should be tracked\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&tracked.stdout),
        String::from_utf8_lossy(&tracked.stderr)
    );

    let ignored = Command::new("git")
        .current_dir(&root)
        .args(["check-ignore", "-q", "Cargo.lock"])
        .status()
        .expect("run git check-ignore");

    assert_eq!(
        ignored.code(),
        Some(1),
        "Cargo.lock should not be ignored by Git"
    );
}

#[test]
fn cli_package_installs_from_workspace_lockfile() {
    let root = workspace_root();
    let install_root = tempdir().expect("install root");
    let target_root = tempdir().expect("target root");
    let cargo = rustup_tool("1.92.0", "cargo");
    let rustc = rustup_tool("1.92.0", "rustc");

    let output = Command::new(cargo)
        .current_dir(&root)
        .env("RUSTC", rustc)
        .env("CARGO_NET_GIT_FETCH_WITH_CLI", "true")
        .env("CARGO_TARGET_DIR", target_root.path().join("target"))
        .args([
            "install",
            "--path",
            "crates/leptos_ui_kit_cli",
            "--locked",
            "--debug",
            "--root",
        ])
        .arg(install_root.path())
        .output()
        .expect("run cargo install");

    assert!(
        output.status.success(),
        "cargo install failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(install_root.path().join("bin/leptos_ui_kit").is_file());
    assert!(
        install_root
            .path()
            .join("bin/cargo-leptos_ui_kit")
            .is_file()
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn rustup_tool(toolchain: &str, tool: &str) -> PathBuf {
    let output = Command::new("rustup")
        .args(["which", "--toolchain", toolchain, tool])
        .output()
        .expect("run rustup which");

    assert!(
        output.status.success(),
        "rustup which failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    PathBuf::from(String::from_utf8(output.stdout).expect("utf8 path").trim())
}
