#![forbid(unsafe_code)]

use std::{
    fs,
    process::{Command, Output},
};

use serde_json::{Value, json};
use tempfile::tempdir;

#[test]
fn json_failures_emit_one_envelope_to_stdout_without_plaintext_stderr() {
    let dir = tempdir().expect("tempdir");
    let cases = [
        (
            vec!["view", "button", "--tailwind", "--json"],
            2,
            "view",
            "unsupported",
            "cli.unsupported_flag",
        ),
        (
            vec!["--json", "view", "button", "--tailwind"],
            2,
            "view",
            "unsupported",
            "cli.unsupported_flag",
        ),
        (
            vec!["info", "--json"],
            1,
            "info",
            "error",
            "project.missing_manifest",
        ),
        (
            vec!["view", "missing-item", "--json"],
            12,
            "view",
            "error",
            "registry.load_failed",
        ),
    ];

    for (args, exit, command, status, diagnostic_code) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_leptos_ui_kit"))
            .current_dir(dir.path())
            .args(args)
            .output()
            .expect("run CLI");
        let value = assert_json_output(&output, exit);
        assert_eq!(value["command"], command);
        assert_eq!(value["status"], status);
        assert_eq!(value["data"], Value::Null);
        assert_eq!(
            value["changes"].as_array().map(Vec::len),
            Some(0),
            "ordinary errors cannot report changes"
        );
        assert_eq!(value["diagnostics"][0]["code"], diagnostic_code);
    }
}

#[test]
fn failing_json_doctor_is_one_completed_outcome() {
    let dir = tempdir().expect("tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_leptos_ui_kit"))
        .current_dir(dir.path())
        .args(["doctor", "--strict", "--json"])
        .output()
        .expect("run doctor");
    let value = assert_json_output(&output, 3);

    assert_eq!(value["command"], "doctor");
    assert_eq!(value["status"], "error");
    assert!(
        value["data"]["checks"]
            .as_array()
            .is_some_and(|checks| { checks.iter().any(|check| check["status"] == "fail") })
    );
    assert!(
        value["diagnostics"]
            .as_array()
            .is_some_and(|diagnostics| !diagnostics.is_empty())
    );
}

#[test]
fn cargo_wrapper_preserves_typed_json_usage_outcomes() {
    let dir = tempdir().expect("tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-leptos_ui_kit"))
        .current_dir(dir.path())
        .args(["leptos_ui_kit", "view", "button", "--tailwind", "--json"])
        .output()
        .expect("run cargo wrapper");
    let value = assert_json_output(&output, 2);

    assert_eq!(value["command"], "view");
    assert_eq!(value["status"], "unsupported");
    assert_eq!(value["diagnostics"][0]["code"], "cli.unsupported_flag");
}

#[test]
fn human_failures_use_stderr_and_keep_stdout_empty() {
    let dir = tempdir().expect("tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_leptos_ui_kit"))
        .current_dir(dir.path())
        .args(["view", "button", "--tailwind"])
        .output()
        .expect("run CLI");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported flag for view"));
}

#[test]
fn json_help_uses_exact_success_envelopes() {
    let dir = tempdir().expect("tempdir");
    let root = run_json(dir.path(), &["--help", "--json"], 0);
    assert_eq!(
        root,
        json!({
            "schemaVersion": "0.9.0-alpha",
            "command": "help",
            "status": "success",
            "diagnostics": [],
            "changes": [],
            "data": {
                "usage": "leptos_ui_kit\n\nusage: leptos_ui_kit <command> [options]\n\ncommands:\n  info                 inspect a supported Trunk CSR Leptos app\n  init                 create src/components/ui/_kit/kit.json and kit-managed app files\n  view <item>          show a registry item\n  add <item>           add a registry item to the app\n  sync                 reconcile installed items with src/components/ui/_kit/kit.json\n  doctor               validate generated source, CSS, lock metadata, and dependencies\n\nglobal options:\n  --cwd <path>         run against a different project root\n  --quiet              accepted for script compatibility\n  --verbose            accepted for script compatibility\n  --help               print help\n  --version            print version"
            }
        })
    );

    let view = run_json(dir.path(), &["view", "--help", "--json"], 0);
    assert_eq!(
        view,
        json!({
            "schemaVersion": "0.9.0-alpha",
            "command": "view",
            "status": "success",
            "diagnostics": [],
            "changes": [],
            "data": {
                "usage": "usage: leptos_ui_kit view <item> [--source] [--json]\n\nShows a built-in registry item and optionally its source files."
            }
        })
    );
}

#[test]
fn json_usage_failure_matches_the_exact_golden() {
    let dir = tempdir().expect("tempdir");
    let value = run_json(dir.path(), &["view", "button", "--tailwind", "--json"], 2);

    assert_eq!(
        value,
        json!({
            "schemaVersion": "0.9.0-alpha",
            "command": "view",
            "status": "unsupported",
            "diagnostics": [{
                "level": "error",
                "code": "cli.unsupported_flag",
                "message": "unsupported flag for view: --tailwind",
                "suggestion": "Run the command with --help to inspect supported arguments."
            }],
            "changes": [],
            "data": null
        })
    );
}

#[test]
fn process_exit_matrix_covers_planned_applied_unchanged_warning_conflict_and_unsafe() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    write_project(root);

    assert_eq!(
        run_json(root, &["init", "--dry-run", "--json"], 0)["status"],
        "planned"
    );
    assert_eq!(run_json(root, &["init", "--json"], 0)["status"], "success");
    assert_eq!(
        run_json(root, &["init", "--json"], 0)["status"],
        "no_change"
    );
    assert_eq!(
        run_json(root, &["add", "button", "--json"], 0)["status"],
        "success"
    );
    let button = root.join("src/components/ui/button.rs");
    let button_source = fs::read_to_string(&button).expect("read installed button");
    fs::write(&button, format!("{button_source}\n// local drift\n")).expect("write source drift");
    assert_eq!(
        run_json(root, &["doctor", "--json"], 0)["status"],
        "warning"
    );
    fs::write(&button, button_source).expect("restore button");

    let anchor = root.join("src/components/ui/anchor.rs");
    fs::write(&anchor, "pub fn app_owned_anchor() {}\n").expect("write conflict");
    let conflict = run_json(root, &["add", "anchor", "--json"], 10);
    assert_eq!(conflict["status"], "conflict");
    assert_eq!(conflict["diagnostics"][0]["code"], "project.patch_conflict");
    assert_eq!(
        conflict["diagnostics"][0]["path"],
        "src/components/ui/anchor.rs"
    );

    let config_path = root.join("src/components/ui/_kit/kit.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).expect("read config"))
        .expect("parse config");
    config["styles"]["css"] = Value::String("../escape.css".to_owned());
    fs::write(
        &config_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&config).expect("serialize config")
        ),
    )
    .expect("write unsafe config");
    let unsafe_path = run_json(root, &["info", "--json"], 11);
    assert_eq!(unsafe_path["status"], "error");
    assert_eq!(unsafe_path["diagnostics"][0]["code"], "config.unsafe_path");
    assert_eq!(
        unsafe_path["diagnostics"][0]["path"],
        "src/components/ui/_kit/kit.json"
    );
    assert!(
        !unsafe_path.to_string().contains("../escape.css"),
        "unsafe physical input must not become a public locator"
    );
}

fn write_project(root: &std::path::Path) {
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "exit-contract"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
"#,
    )
    .expect("write Cargo.toml");
    fs::create_dir(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<!doctype html>\n<html><head></head><body></body></html>\n",
    )
    .expect("write index");
}

fn run_json(root: &std::path::Path, args: &[&str], exit: i32) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_leptos_ui_kit"))
        .current_dir(root)
        .args(args)
        .output()
        .expect("run CLI");
    assert_json_output(&output, exit)
}

fn assert_json_output(output: &Output, exit: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "unexpected exit\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "expected no stderr, got:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must contain exactly one JSON value: {error}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}
