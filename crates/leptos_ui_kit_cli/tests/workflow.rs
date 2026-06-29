#![forbid(unsafe_code)]

use std::{
    fs,
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
    assert_success(&project, &["add", "button", "--dry-run", "--json"]);
    assert_success(&project, &["add", "button"]);
    assert_success(&project, &["doctor", "--strict", "--json"]);

    assert!(project.join("src/components/ui/button.rs").is_file());
    assert!(
        project
            .join(".leptos-ui/baselines/builtin-button/button.rs")
            .is_file()
    );
}

fn assert_success(project: &Path, args: &[&str]) {
    let output = Command::new(cli_bin())
        .current_dir(project)
        .args(args)
        .output()
        .expect("run leptos-ui");

    assert!(
        output.status.success(),
        "leptos-ui {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cli_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_leptos-ui")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_leptos-ui should be set by Cargo")
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
