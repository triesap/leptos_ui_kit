#![forbid(unsafe_code)]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use tempfile::tempdir;

#[test]
fn homepage_fixture_cli_workflow_smoke() {
    let dir = tempdir().expect("tempdir");
    let project = dir.path().join("homepage");
    copy_dir(&fixture_root(), &project);

    assert_success(&project, &["info", "--json"]);
    assert_success(&project, &["init", "--dry-run", "--json"]);
    assert_success(&project, &["init"]);
    assert_success(&project, &["view", "button", "--json"]);
    assert_success(&project, &["view", "button", "--source", "--json"]);
    assert_success(&project, &["view", "collapsible", "--json"]);
    assert_success(&project, &["view", "collapsible", "--source", "--json"]);
    assert_success(&project, &["view", "dialog", "--json"]);
    assert_success(&project, &["view", "dialog", "--source", "--json"]);
    assert_success(&project, &["view", "tabs", "--json"]);
    assert_success(&project, &["view", "tabs", "--source", "--json"]);
    assert_success(&project, &["add", "button", "--dry-run", "--json"]);
    assert_success(&project, &["add", "button"]);
    assert_success(&project, &["add", "collapsible", "--dry-run", "--json"]);
    assert_success(&project, &["add", "collapsible"]);
    assert_success(&project, &["add", "dialog", "--dry-run", "--json"]);
    assert_success(&project, &["add", "dialog"]);
    assert_success(&project, &["add", "tabs", "--dry-run", "--json"]);
    assert_success(&project, &["add", "tabs"]);
    assert_success(&project, &["sync", "--dry-run", "--json"]);
    assert_success(&project, &["sync"]);
    assert_success(&project, &["doctor", "--strict", "--json"]);
    assert_cargo_subcommand_success(&project, &["doctor", "--strict", "--json"]);
    assert_cargo_check(&project);

    assert!(project.join("src/components/ui/button.rs").is_file());
    assert!(
        project
            .join("src/components/ui/collapsible/mod.rs")
            .is_file()
    );
    assert!(
        project
            .join("src/components/ui/collapsible/root.rs")
            .is_file()
    );
    assert!(project.join("src/components/ui/dialog/mod.rs").is_file());
    assert!(
        project
            .join("src/components/ui/dialog/content.rs")
            .is_file()
    );
    assert!(project.join("src/components/ui/tabs/mod.rs").is_file());
    assert!(project.join("src/components/ui/tabs/root.rs").is_file());
    assert!(project.join("components.lock.json").is_file());
    assert!(!project.join("src/components/ui/_kit").exists());
}

fn assert_cargo_check(project: &Path) {
    let rustc = rustup_tool("1.92.0", "rustc");
    let output = Command::new("rustup")
        .current_dir(project)
        .env("CARGO_TARGET_DIR", project.join(".target"))
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTFLAGS")
        .env("RUSTC", rustc)
        .env_remove("RUSTDOC")
        .args([
            "run",
            "1.92.0",
            "cargo",
            "check",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .output()
        .expect("run cargo check");

    assert!(
        output.status.success(),
        "cargo check failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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

fn assert_success(project: &Path, args: &[&str]) {
    let output = Command::new(cli_bin())
        .current_dir(project)
        .args(args)
        .output()
        .expect("run leptos_ui_kit");

    assert!(
        output.status.success(),
        "leptos_ui_kit {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_cargo_subcommand_success(project: &Path, args: &[&str]) {
    let bin = cargo_cli_bin();
    let bin_dir = bin.parent().expect("cargo cli bin parent");
    let mut paths = vec![bin_dir.to_path_buf()];
    if let Some(path) = env::var_os("PATH") {
        paths.extend(env::split_paths(&path));
    }
    let path = env::join_paths(paths).expect("join path");

    let output = Command::new("cargo")
        .current_dir(project)
        .env("PATH", path)
        .arg("leptos_ui_kit")
        .args(args)
        .output()
        .expect("run cargo leptos_ui_kit");

    assert!(
        output.status.success(),
        "cargo leptos_ui_kit {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cli_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_leptos_ui_kit")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_leptos_ui_kit should be set by Cargo")
}

fn cargo_cli_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_cargo-leptos_ui_kit")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_cargo-leptos_ui_kit should be set by Cargo")
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/homepage_trunk_csr")
        .canonicalize()
        .expect("canonical fixture root")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create destination");
    for entry in fs::read_dir(from).expect("read fixture") {
        let entry = entry.expect("fixture entry");
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &destination);
        } else {
            fs::copy(&source, &destination).expect("copy fixture file");
        }
    }
}
