#![forbid(unsafe_code)]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use tempfile::tempdir;

#[test]
fn logical_outputs_are_independent_of_project_root_and_binary_wrapper() {
    let dir = tempdir().expect("tempdir");
    let first = dir.path().join("logical-root-a");
    let second = dir.path().join("logical-root-b");
    copy_dir(&fixture_root(), &first);
    copy_dir(&fixture_root(), &second);
    let first = first.canonicalize().expect("canonical first project");
    let second = second.canonicalize().expect("canonical second project");
    let forbidden = build_path_sentinels();

    let first_outputs = capture_logical_workflow(&first, &forbidden);
    let second_outputs = capture_logical_workflow(&second, &forbidden);
    assert_eq!(first_outputs.len(), second_outputs.len());

    for ((first_label, first_output), (second_label, second_output)) in
        first_outputs.iter().zip(&second_outputs)
    {
        assert_eq!(first_label, second_label);
        assert_eq!(
            normalize_project_root(first_output, &first),
            normalize_project_root(second_output, &second),
            "{first_label} emitted root-dependent data outside the explicit project-root fields"
        );
    }

    let button_view = captured_json(&first_outputs, "view button JSON");
    assert_eq!(button_view["data"]["source_path"], "ui/button.json");
    assert_eq!(
        button_view["data"]["targets"]["ui_files"][0]["source"],
        "ui/button.rs"
    );
    assert_eq!(
        button_view["data"]["targets"]["style_blocks"][0]["source"],
        "styles/button.css"
    );
    assert!(
        captured_output(&first_outputs, "view button human")
            .contains("source_path: ui/button.json"),
        "human view must expose the logical manifest locator"
    );

    let button_source_view = captured_json(&first_outputs, "view button source JSON");
    assert_eq!(
        button_source_view["data"]["resolved"]["source_path"],
        "ui/button.json"
    );
    assert_eq!(
        button_source_view["data"]["sources"][0]["path"],
        "ui/button.rs"
    );
    assert_eq!(
        button_source_view["data"]["sources"][1]["path"],
        "styles/button.css"
    );
    let button_source_human = captured_output(&first_outputs, "view button source human");
    assert!(button_source_human.contains("--- ui/button.rs (rust) ---"));
    assert!(button_source_human.contains("--- styles/button.css (css) ---"));

    let tokens_source_view = captured_json(&first_outputs, "view tokens source JSON");
    assert_eq!(
        tokens_source_view["data"]["resolved"]["source_path"],
        "foundation/tokens.json"
    );
    assert_eq!(
        tokens_source_view["data"]["sources"][0]["path"],
        "styles/tokens.css"
    );

    let first_config = fs::read_to_string(first.join("src/components/ui/_kit/kit.json"))
        .expect("read first config");
    let second_config = fs::read_to_string(second.join("src/components/ui/_kit/kit.json"))
        .expect("read second config");
    let first_lock = fs::read_to_string(first.join("src/components/ui/_kit/kit.lock.json"))
        .expect("read first lock");
    let second_lock = fs::read_to_string(second.join("src/components/ui/_kit/kit.lock.json"))
        .expect("read second lock");

    assert_eq!(first_config, second_config, "config bytes must be stable");
    assert_eq!(first_lock, second_lock, "lock bytes must be stable");
    assert_no_build_path_leaks("generated kit config", &first_config, &forbidden);
    assert_no_build_path_leaks("generated install lock", &first_lock, &forbidden);

    let config = serde_json::from_str::<serde_json::Value>(&first_config)
        .expect("generated config is valid JSON");
    let rev = config["tool"]["source"]["rev"]
        .as_str()
        .expect("config tool source revision");
    assert_eq!(rev.len(), 40, "config must persist a full Git commit hash");
    assert!(
        rev.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "config revision must be lowercase hexadecimal: {rev}"
    );
    for path in [
        config["project"]["crateRoot"].as_str().expect("crate root"),
        config["project"]["srcDir"].as_str().expect("source dir"),
        config["project"]["indexHtml"].as_str().expect("index HTML"),
        config["install"]["uiDir"].as_str().expect("UI dir"),
        config["install"]["uiMod"].as_str().expect("UI module"),
        config["install"]["componentsMod"]
            .as_str()
            .expect("components module"),
        config["styles"]["css"].as_str().expect("stylesheet"),
    ] {
        assert_logical_path(path);
    }

    let lock = serde_json::from_str::<serde_json::Value>(&first_lock)
        .expect("generated lock is valid JSON");
    for (item_id, item) in lock["items"].as_object().expect("lock item map") {
        assert!(
            item_id.starts_with("builtin:"),
            "logical item id: {item_id}"
        );
        assert_eq!(item["id"], item_id.as_str());
        assert_eq!(item["source"], "builtin");
        for file in item["files"].as_array().expect("installed file list") {
            assert_logical_path(file["path"].as_str().expect("installed file path"));
        }
        for block in item["styleBlocks"]
            .as_array()
            .expect("installed style block list")
        {
            assert_logical_path(block["cssPath"].as_str().expect("installed CSS path"));
        }
    }
    for path in lock["filesByPath"]
        .as_object()
        .expect("files-by-path map")
        .keys()
    {
        assert_logical_path(path);
    }

    let direct_bin = cli_bin();
    let cargo_bin = cargo_cli_bin();
    for args in [
        &["--version"][..],
        &["--version", "--json"][..],
        &["view", "button"][..],
        &["view", "button", "--json"][..],
        &["view", "button", "--source"][..],
        &["view", "button", "--source", "--json"][..],
        &["view", "tokens", "--source"][..],
        &["view", "tokens", "--source", "--json"][..],
    ] {
        let direct = capture_success(&direct_bin, false, &first, args, &forbidden);
        let cargo = capture_success(&cargo_bin, true, &first, args, &forbidden);
        assert_eq!(
            direct, cargo,
            "direct and cargo-subcommand binaries diverged for {args:?}"
        );
    }
}

#[test]
fn homepage_fixture_cli_workflow_smoke() {
    let dir = tempdir().expect("tempdir");
    let project = dir.path().join("homepage");
    copy_dir(&fixture_root(), &project);

    assert_success(&project, &["info", "--json"]);
    assert_success(&project, &["init", "--dry-run", "--json"]);
    assert_success(&project, &["init"]);
    assert_success(&project, &["view", "anchor", "--json"]);
    assert_success(&project, &["view", "anchor", "--source", "--json"]);
    assert_success(&project, &["view", "button", "--json"]);
    assert_success(&project, &["view", "button", "--source", "--json"]);
    assert_success(&project, &["view", "collapsible", "--json"]);
    assert_success(&project, &["view", "collapsible", "--source", "--json"]);
    assert_success(&project, &["view", "dialog", "--json"]);
    assert_success(&project, &["view", "dialog", "--source", "--json"]);
    assert_success(&project, &["view", "field", "--json"]);
    assert_success(&project, &["view", "field", "--source", "--json"]);
    assert_success(&project, &["view", "menu", "--json"]);
    assert_success(&project, &["view", "menu", "--source", "--json"]);
    assert_success(&project, &["view", "router-link", "--json"]);
    assert_success(&project, &["view", "router-link", "--source", "--json"]);
    assert_success(&project, &["view", "spinner", "--json"]);
    assert_success(&project, &["view", "spinner", "--source", "--json"]);
    assert_success(&project, &["view", "status", "--json"]);
    assert_success(&project, &["view", "status", "--source", "--json"]);
    assert_success(&project, &["view", "tabs", "--json"]);
    assert_success(&project, &["view", "tabs", "--source", "--json"]);
    assert_success(&project, &["add", "anchor", "--dry-run", "--json"]);
    assert_success(&project, &["add", "anchor"]);
    assert_success(&project, &["add", "button", "--dry-run", "--json"]);
    assert_success(&project, &["add", "button"]);
    assert_success(&project, &["add", "collapsible", "--dry-run", "--json"]);
    assert_success(&project, &["add", "collapsible"]);
    assert_success(&project, &["add", "dialog", "--dry-run", "--json"]);
    assert_success(&project, &["add", "dialog"]);
    assert_success(&project, &["add", "field", "--dry-run", "--json"]);
    assert_success(&project, &["add", "field"]);
    assert_success(&project, &["add", "menu", "--dry-run", "--json"]);
    assert_success(&project, &["add", "menu"]);
    assert_success(&project, &["add", "router-link", "--dry-run", "--json"]);
    assert_success(&project, &["add", "router-link"]);
    assert_success(&project, &["add", "spinner", "--dry-run", "--json"]);
    assert_success(&project, &["add", "spinner"]);
    assert_success(&project, &["add", "status", "--dry-run", "--json"]);
    assert_success(&project, &["add", "status"]);
    assert_success(&project, &["add", "tabs", "--dry-run", "--json"]);
    assert_success(&project, &["add", "tabs"]);
    assert_success(&project, &["sync", "--dry-run", "--json"]);
    assert_success(&project, &["sync"]);
    assert_success(&project, &["doctor", "--strict", "--json"]);
    assert_cargo_subcommand_success(&project, &["doctor", "--strict", "--json"]);
    assert_cargo_check(&project, None);
    assert_cargo_check(&project, Some("wasm32-unknown-unknown"));

    assert!(project.join("src/components/ui/anchor.rs").is_file());
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
    assert!(project.join("src/components/ui/field/mod.rs").is_file());
    assert!(project.join("src/components/ui/field/label.rs").is_file());
    assert!(
        project
            .join("src/components/ui/field/text_input.rs")
            .is_file()
    );
    assert!(
        project
            .join("src/components/ui/field/text_area.rs")
            .is_file()
    );
    assert!(
        project
            .join("src/components/ui/field/text_field.rs")
            .is_file()
    );
    assert!(
        project
            .join("src/components/ui/field/text_area_field.rs")
            .is_file()
    );
    assert!(
        project
            .join("src/components/ui/field/native_select.rs")
            .is_file()
    );
    assert!(
        project
            .join("src/components/ui/field/select_field.rs")
            .is_file()
    );
    assert!(project.join("src/components/ui/menu/mod.rs").is_file());
    assert!(project.join("src/components/ui/menu/content.rs").is_file());
    assert!(project.join("src/components/ui/router_link.rs").is_file());
    assert!(project.join("src/components/ui/spinner.rs").is_file());
    assert!(project.join("src/components/ui/status.rs").is_file());
    assert!(project.join("src/components/ui/tabs/mod.rs").is_file());
    assert!(project.join("src/components/ui/tabs/root.rs").is_file());
    assert!(project.join("src/components/ui/_kit/kit.json").is_file());
    assert!(
        project
            .join("src/components/ui/_kit/kit.lock.json")
            .is_file()
    );

    let index = fs::read_to_string(project.join("index.html")).expect("read fixture index");
    let kit_css = index.find("styles/kit.css").expect("kit stylesheet link");
    let themes_css = index
        .find("styles/themes.css")
        .expect("themes stylesheet link");
    let app_css = index.find("styles/app.css").expect("app stylesheet link");
    assert!(kit_css < themes_css && themes_css < app_css);
    assert!(index.contains("dark-theme-portal-root"));
    assert!(index.contains("data-ui-theme=\"dark\""));
    assert!(
        fs::read_to_string(project.join("styles/themes.css"))
            .expect("read fixture theme stylesheet")
            .contains(".preview-pane[data-ui-theme=\"dark\"]")
    );
    assert!(
        fs::read_to_string(project.join("src/main.rs"))
            .expect("read fixture source")
            .contains("portal_mount=portal_mount")
    );
}

fn capture_logical_workflow(project: &Path, forbidden: &[PathBuf]) -> Vec<(&'static str, String)> {
    let bin = cli_bin();
    let mut outputs = Vec::new();
    for (label, args) in [
        ("version human", &["--version"][..]),
        ("version JSON", &["--version", "--json"][..]),
        ("view button human", &["view", "button"][..]),
        ("view button JSON", &["view", "button", "--json"][..]),
        (
            "view button source human",
            &["view", "button", "--source"][..],
        ),
        (
            "view button source JSON",
            &["view", "button", "--source", "--json"][..],
        ),
        (
            "view tokens source human",
            &["view", "tokens", "--source"][..],
        ),
        (
            "view tokens source JSON",
            &["view", "tokens", "--source", "--json"][..],
        ),
        ("info human", &["info"][..]),
        ("info JSON", &["info", "--json"][..]),
        ("init dry-run human", &["init", "--dry-run"][..]),
        ("init dry-run JSON", &["init", "--dry-run", "--json"][..]),
        ("init write JSON", &["init", "--json"][..]),
        ("add dry-run human", &["add", "button", "--dry-run"][..]),
        (
            "add dry-run JSON",
            &["add", "button", "--dry-run", "--json"][..],
        ),
        ("add write JSON", &["add", "button", "--json"][..]),
        ("sync dry-run human", &["sync", "--dry-run"][..]),
        ("sync dry-run JSON", &["sync", "--dry-run", "--json"][..]),
        ("sync write JSON", &["sync", "--json"][..]),
        ("info installed human", &["info"][..]),
        ("info installed JSON", &["info", "--json"][..]),
        ("doctor human", &["doctor", "--strict"][..]),
        ("doctor JSON", &["doctor", "--strict", "--json"][..]),
    ] {
        let output = capture_success(&bin, false, project, args, forbidden);
        if args.contains(&"--json") {
            serde_json::from_str::<serde_json::Value>(&output)
                .unwrap_or_else(|error| panic!("{label} did not emit valid JSON: {error}"));
        }
        outputs.push((label, output));
    }
    outputs
}

fn captured_json(outputs: &[(&str, String)], label: &str) -> serde_json::Value {
    serde_json::from_str(captured_output(outputs, label))
        .unwrap_or_else(|error| panic!("invalid {label}: {error}"))
}

fn captured_output<'a>(outputs: &'a [(&str, String)], label: &str) -> &'a str {
    outputs
        .iter()
        .find_map(|(candidate, output)| (*candidate == label).then_some(output))
        .map(String::as_str)
        .unwrap_or_else(|| panic!("missing captured output {label}"))
}

fn capture_success(
    binary: &Path,
    cargo_wrapper: bool,
    project: &Path,
    args: &[&str],
    forbidden: &[PathBuf],
) -> String {
    let mut command = Command::new(binary);
    command.current_dir(project);
    if cargo_wrapper {
        command.arg("leptos_ui_kit");
    }
    let output = command.args(args).output().expect("run CLI command");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");

    assert!(
        output.status.success(),
        "{} {:?} failed\nstdout:\n{}\nstderr:\n{}",
        binary.display(),
        args,
        stdout,
        stderr
    );
    assert_no_build_path_leaks("command stdout", &stdout, forbidden);
    assert_no_build_path_leaks("command stderr", &stderr, forbidden);
    stdout
}

fn normalize_project_root(output: &str, project: &Path) -> String {
    output.replace(&project.to_string_lossy().into_owned(), "<project-root>")
}

fn build_path_sentinels() -> Vec<PathBuf> {
    let cli_bin = cli_bin();
    let cargo_bin = cargo_cli_bin();
    let mut paths = vec![workspace_root(), PathBuf::from(env!("CARGO_MANIFEST_DIR"))];
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    for value in [
        option_env!("OUT_DIR").map(PathBuf::from),
        cargo_home,
        env::var_os("CARGO_TARGET_DIR").map(PathBuf::from),
        cli_bin.parent().map(Path::to_path_buf),
        cli_bin
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        cargo_bin.parent().map(Path::to_path_buf),
        cargo_bin
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
    ]
    .into_iter()
    .flatten()
    {
        if value.is_absolute() && !paths.contains(&value) {
            paths.push(value);
        }
    }
    paths
}

fn assert_no_build_path_leaks(label: &str, output: &str, forbidden: &[PathBuf]) {
    for path in forbidden {
        let path = path.to_string_lossy();
        assert!(
            !output.contains(path.as_ref()),
            "{label} leaked private build path {path}:\n{output}"
        );
    }
}

fn assert_logical_path(path: &str) {
    assert!(!path.is_empty(), "logical path must not be empty");
    assert!(!Path::new(path).is_absolute(), "absolute lock path: {path}");
    assert!(!path.contains('\\'), "backslash in logical path: {path}");
    assert!(
        !path.split('/').any(|segment| segment == ".."),
        "parent traversal in logical path: {path}"
    );
}

fn assert_cargo_check(project: &Path, target: Option<&str>) {
    let rustc = rustup_tool("1.92.0", "rustc");
    let mut command = Command::new("rustup");
    command
        .current_dir(project)
        .env("CARGO_TARGET_DIR", project.join(".target"))
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTFLAGS")
        .env("RUSTC", rustc)
        .env_remove("RUSTDOC")
        .args(["run", "1.92.0", "cargo", "check"]);
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    let output = command.output().expect("run cargo check");

    assert!(
        output.status.success(),
        "cargo check {target:?} failed\nstdout:\n{}\nstderr:\n{}",
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
    test_binary("CARGO_BIN_EXE_leptos_ui_kit", "leptos_ui_kit")
}

fn cargo_cli_bin() -> PathBuf {
    test_binary("CARGO_BIN_EXE_cargo-leptos_ui_kit", "cargo-leptos_ui_kit")
}

fn test_binary(env_var: &str, name: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(env_var).map(PathBuf::from) {
        return path;
    }

    let binary = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::current_exe()
        .expect("current test binary")
        .parent()
        .and_then(Path::parent)
        .expect("target debug directory")
        .join(binary);
    assert!(
        path.is_file(),
        "{env_var} was not set and fallback binary was missing at {}",
        path.display()
    );
    path
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/homepage_trunk_csr")
        .canonicalize()
        .expect("canonical fixture root")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical workspace root")
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
