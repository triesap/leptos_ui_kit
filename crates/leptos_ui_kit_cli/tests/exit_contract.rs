#![forbid(unsafe_code)]

use std::process::{Command, Output};

use serde_json::Value;
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
