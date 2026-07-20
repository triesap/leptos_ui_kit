#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

#[path = "../../../tests/support/package_workspace.rs"]
mod package_workspace_support;

use package_workspace_support::{
    PACKAGE_NAMES, PACKAGE_WORKSPACE_LOCK, assert_full_revision, assert_local_package_metadata,
    assert_success, cargo_command, contains_bytes, extract_workspace, package_workspace,
    sanitize_command_environment, source_head, workspace_root,
};

const CONFIG_PATH: &str = "src/components/ui/_kit/kit.json";
const LOCK_PATH: &str = "src/components/ui/_kit/kit.lock.json";
const BUILT_IN_CATALOG: [(&str, &str, &str); 11] = [
    ("anchor", "ui", "ui/anchor.json"),
    ("button", "ui", "ui/button.json"),
    ("collapsible", "ui", "ui/collapsible.json"),
    ("dialog", "ui", "ui/dialog.json"),
    ("field", "ui", "ui/field.json"),
    ("menu", "ui", "ui/menu.json"),
    ("router-link", "ui", "ui/router-link.json"),
    ("spinner", "ui", "ui/spinner.json"),
    ("status", "ui", "ui/status.json"),
    ("tabs", "ui", "ui/tabs.json"),
    ("tokens", "foundation", "foundation/tokens.json"),
];

#[derive(Clone, Copy)]
enum EntryPoint {
    Direct,
    Wrapper,
    Cargo,
}

impl EntryPoint {
    const fn label(self) -> &'static str {
        match self {
            Self::Direct => "leptos_ui_kit",
            Self::Wrapper => "cargo-leptos_ui_kit",
            Self::Cargo => "cargo leptos_ui_kit",
        }
    }
}

struct JsonCommandOutput {
    stdout: String,
    value: Value,
}

struct RuntimeHarness<'a> {
    app_root: &'a Path,
    direct_bin: &'a Path,
    bin_dir: &'a Path,
    deleted_cargo_home: &'a Path,
    runtime_cargo_home: &'a Path,
    forbidden: &'a [PathBuf],
}

#[test]
#[ignore = "slow source-deletion acceptance; run with --ignored --exact"]
fn installed_binaries_run_after_package_source_and_build_state_are_deleted() {
    let source_root = workspace_root(env!("CARGO_MANIFEST_DIR"));
    let expected_rev = source_head(&source_root);
    let temporary = tempdir().expect("create packaged-runtime acceptance root");
    let temporary_root = temporary
        .path()
        .canonicalize()
        .expect("canonical packaged-runtime acceptance root");

    let build_state = temporary_root.join("deletable-build-state");
    let archive_target = build_state.join("archive-target");
    let extracted_workspace = build_state.join("workspace");
    let cargo_home = build_state.join("cargo-home");
    let target_dir = build_state.join("target");
    let removed_build_state = temporary_root.join("removed-build-state");
    let runtime_root = temporary_root.join("retained-runtime");
    let install_root = runtime_root.join("install");
    let app_root = runtime_root.join("app");
    let runtime_cargo_home = runtime_root.join("empty-cargo-home");

    fs::create_dir_all(&build_state).expect("create deletable build-state root");
    fs::create_dir_all(&runtime_root).expect("create retained runtime root");
    fs::create_dir(&runtime_cargo_home).expect("create empty runtime Cargo home");
    fs::create_dir_all(&cargo_home).expect("create isolated Cargo home");

    let archives = package_workspace(&source_root, &archive_target);
    let extracted_packages = extract_workspace(&archives, &extracted_workspace);
    assert_package_revisions(&extracted_packages, &expected_rev);

    fs::write(
        extracted_workspace.join("Cargo.lock"),
        PACKAGE_WORKSPACE_LOCK,
    )
    .expect("seed extracted package-only lockfile");
    assert_local_package_metadata(
        "source-deletion installation",
        &extracted_workspace,
        &cargo_home,
        &target_dir,
    );

    materialize_runtime_app(
        extracted_packages
            .get("leptos_ui_kit_cli")
            .expect("extracted CLI package"),
        &app_root,
    );
    install_cli(
        &extracted_workspace,
        &cargo_home,
        &target_dir,
        &install_root,
    );

    let bin_dir = install_root.join("bin");
    let direct_bin = bin_dir.join(format!("leptos_ui_kit{}", env::consts::EXE_SUFFIX));
    let cargo_bin = bin_dir.join(format!("cargo-leptos_ui_kit{}", env::consts::EXE_SUFFIX));
    assert_regular_file(&direct_bin);
    assert_regular_file(&cargo_bin);
    let direct_hash = file_hash(&direct_bin);
    let cargo_hash = file_hash(&cargo_bin);

    let forbidden = forbidden_build_paths(&[
        &source_root,
        &build_state,
        &archive_target,
        &extracted_workspace,
        &cargo_home,
        &target_dir,
        &removed_build_state,
        &install_root,
        &runtime_cargo_home,
        &direct_bin,
        &cargo_bin,
    ]);

    delete_build_state(
        &temporary_root,
        &build_state,
        &removed_build_state,
        &[
            &archive_target,
            &extracted_workspace,
            &cargo_home,
            &target_dir,
        ],
    );
    assert!(!archive_target.exists());
    assert!(!extracted_workspace.exists());
    assert!(!cargo_home.exists());
    assert!(!target_dir.exists());
    assert!(!removed_build_state.exists());
    assert_regular_file(&direct_bin);
    assert_regular_file(&cargo_bin);
    assert_eq!(file_hash(&direct_bin), direct_hash);
    assert_eq!(file_hash(&cargo_bin), cargo_hash);
    assert_empty_dir(&runtime_cargo_home);

    run_deleted_source_workflow(
        &app_root,
        &direct_bin,
        &bin_dir,
        &cargo_home,
        &runtime_cargo_home,
        &expected_rev,
        &forbidden,
    );

    assert!(
        !cargo_home.exists(),
        "runtime must not recreate Cargo state"
    );
    assert_empty_dir(&runtime_cargo_home);
    assert_eq!(file_hash(&direct_bin), direct_hash);
    assert_eq!(file_hash(&cargo_bin), cargo_hash);
}

fn assert_package_revisions(packages: &BTreeMap<&str, PathBuf>, expected_rev: &str) {
    assert_full_revision(expected_rev);
    for name in PACKAGE_NAMES {
        let package = packages
            .get(name)
            .unwrap_or_else(|| panic!("missing extracted {name} package"));
        let metadata_path = package.join(".cargo_vcs_info.json");
        let metadata: Value = serde_json::from_slice(
            &fs::read(&metadata_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", metadata_path.display())),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", metadata_path.display()));
        assert_eq!(
            metadata.pointer("/git/sha1").and_then(Value::as_str),
            Some(expected_rev),
            "{name} archive revision"
        );
        let expected_path = format!("crates/{name}");
        assert_eq!(
            metadata.get("path_in_vcs").and_then(Value::as_str),
            Some(expected_path.as_str()),
            "{name} archive source path"
        );
        assert!(
            metadata.pointer("/git/dirty").is_none(),
            "{name} clean archive must omit Cargo's dirty marker"
        );
    }
}

fn materialize_runtime_app(extracted_cli: &Path, app_root: &Path) {
    let fixture = extracted_cli.join("tests/fixtures/homepage_trunk_csr");
    let expected = BTreeSet::from([
        "Cargo.toml.fixture".to_owned(),
        "index.html".to_owned(),
        "src/main.rs".to_owned(),
        "styles/app.css".to_owned(),
        "styles/themes.css".to_owned(),
    ]);
    assert_eq!(collect_files(&fixture), expected, "packaged app fixture");
    copy_dir(&fixture, app_root);
    fs::rename(
        app_root.join("Cargo.toml.fixture"),
        app_root.join("Cargo.toml"),
    )
    .expect("activate packaged app fixture manifest");
    assert!(!app_root.join("Cargo.toml.fixture").exists());
    assert_eq!(
        collect_files(app_root),
        BTreeSet::from([
            "Cargo.toml".to_owned(),
            "index.html".to_owned(),
            "src/main.rs".to_owned(),
            "styles/app.css".to_owned(),
            "styles/themes.css".to_owned(),
        ]),
        "fresh runtime app fixture"
    );
}

fn install_cli(workspace: &Path, cargo_home: &Path, target_dir: &Path, install_root: &Path) {
    let output = cargo_command(workspace, cargo_home, target_dir)
        .args([
            "install",
            "--path",
            "crates/leptos_ui_kit_cli",
            "--bins",
            "--locked",
            "--debug",
            "--root",
        ])
        .arg(install_root)
        .output()
        .expect("install extracted CLI package");
    assert_success("install extracted CLI package", &output);
}

fn delete_build_state(
    temporary_root: &Path,
    build_state: &Path,
    removed_build_state: &Path,
    children: &[&Path],
) {
    let temporary_root = temporary_root
        .canonicalize()
        .expect("canonical acceptance root before deletion");
    let build_state = build_state
        .canonicalize()
        .expect("canonical build-state root before deletion");
    assert_ne!(build_state, temporary_root);
    assert_eq!(build_state.parent(), Some(temporary_root.as_path()));
    for child in children {
        let child = child.canonicalize().unwrap_or_else(|error| {
            panic!("canonical deletion target {}: {error}", child.display())
        });
        assert_ne!(child, build_state);
        assert!(
            child.starts_with(&build_state),
            "deletion target escaped build-state root: {}",
            child.display()
        );
    }
    assert_eq!(removed_build_state.parent(), Some(temporary_root.as_path()));
    assert!(!removed_build_state.exists());

    fs::rename(&build_state, removed_build_state).expect("rename deletable build state");
    assert!(!build_state.exists());
    fs::remove_dir_all(removed_build_state).expect("remove renamed build state");
    assert!(!removed_build_state.exists());
}

fn run_deleted_source_workflow(
    app_root: &Path,
    direct_bin: &Path,
    bin_dir: &Path,
    deleted_cargo_home: &Path,
    runtime_cargo_home: &Path,
    expected_rev: &str,
    forbidden: &[PathBuf],
) {
    let runtime = RuntimeHarness {
        app_root,
        direct_bin,
        bin_dir,
        deleted_cargo_home,
        runtime_cargo_home,
        forbidden,
    };
    let cargo_manifest_before =
        fs::read(app_root.join("Cargo.toml")).expect("read app manifest before workflow");
    let direct_version = runtime.run_json(EntryPoint::Direct, &["--version", "--json"]);
    let wrapper_version = runtime.run_json(EntryPoint::Wrapper, &["--version", "--json"]);
    let cargo_version = runtime.run_json(EntryPoint::Cargo, &["--version", "--json"]);
    assert_eq!(direct_version.stdout, wrapper_version.stdout);
    assert_eq!(direct_version.stdout, cargo_version.stdout);
    assert_version(&direct_version.value, expected_rev);

    let direct_tokens = runtime.run_json(
        EntryPoint::Direct,
        &["view", "tokens", "--source", "--json"],
    );
    let cargo_tokens =
        runtime.run_json(EntryPoint::Cargo, &["view", "tokens", "--source", "--json"]);
    let wrapper_tokens = runtime.run_json(
        EntryPoint::Wrapper,
        &["view", "tokens", "--source", "--json"],
    );
    assert_eq!(direct_tokens.stdout, wrapper_tokens.stdout);
    assert_eq!(direct_tokens.stdout, cargo_tokens.stdout);
    assert_source_view(
        &direct_tokens.value,
        "foundation/tokens.json",
        &["styles/tokens.css"],
    );

    let direct_button = runtime.run_json(
        EntryPoint::Direct,
        &["view", "button", "--source", "--json"],
    );
    let cargo_button =
        runtime.run_json(EntryPoint::Cargo, &["view", "button", "--source", "--json"]);
    let wrapper_button = runtime.run_json(
        EntryPoint::Wrapper,
        &["view", "button", "--source", "--json"],
    );
    assert_eq!(direct_button.stdout, wrapper_button.stdout);
    assert_eq!(direct_button.stdout, cargo_button.stdout);
    assert_source_view(
        &direct_button.value,
        "ui/button.json",
        &["ui/button.rs", "styles/button.css"],
    );

    assert_catalog_views(&runtime);

    let direct_info = runtime.run_json(EntryPoint::Direct, &["info", "--json"]);
    let cargo_info = runtime.run_json(EntryPoint::Cargo, &["info", "--json"]);
    assert_eq!(direct_info.stdout, cargo_info.stdout);
    assert_info(&direct_info.value, false);

    let init = runtime.run_json(EntryPoint::Direct, &["init", "--json"]);
    assert_envelope(&init.value, "init", "success");
    assert_object_keys(&init.value["data"], "init data", &[]);
    assert_nonempty_changes(&init.value, "init");

    let add_tokens = runtime.run_json(EntryPoint::Cargo, &["add", "tokens", "--json"]);
    assert_add(&add_tokens.value, "builtin:tokens", "tokens");
    assert_nonempty_changes(&add_tokens.value, "add tokens");

    let add_button = runtime.run_json(EntryPoint::Direct, &["add", "button", "--json"]);
    assert_add(&add_button.value, "builtin:button", "button");
    assert_nonempty_changes(&add_button.value, "add button");

    let first_sync = runtime.run_json(EntryPoint::Cargo, &["sync", "--json"]);
    assert_sync(&first_sync.value, "no_change");
    assert_eq!(first_sync.value["changes"], Value::Array(Vec::new()));

    let config_before =
        fs::read(app_root.join(CONFIG_PATH)).expect("read config before idempotency");
    let lock_before = fs::read(app_root.join(LOCK_PATH)).expect("read lock before idempotency");
    let tree_before = snapshot_tree(app_root);
    let second_sync = runtime.run_json(EntryPoint::Direct, &["sync", "--json"]);
    assert_sync(&second_sync.value, "no_change");
    assert_eq!(second_sync.value["changes"], Value::Array(Vec::new()));
    assert!(
        second_sync.value["data"].get("files").is_none(),
        "public sync DTO must not expose planned files"
    );
    assert!(
        second_sync.value["data"].get("changes").is_none(),
        "changes must exist only at the envelope level"
    );
    assert_eq!(
        fs::read(app_root.join(CONFIG_PATH)).expect("read config after idempotency"),
        config_before
    );
    assert_eq!(
        fs::read(app_root.join(LOCK_PATH)).expect("read lock after idempotency"),
        lock_before
    );
    assert_eq!(snapshot_tree(app_root), tree_before);

    let doctor = runtime.run_json(EntryPoint::Cargo, &["doctor", "--strict", "--json"]);
    assert_doctor(&doctor.value);

    let installed_info = runtime.run_json(EntryPoint::Direct, &["info", "--json"]);
    assert_info(&installed_info.value, true);

    assert_generated_state(app_root, expected_rev, forbidden);
    assert_eq!(
        fs::read(app_root.join("Cargo.toml")).expect("read app manifest after workflow"),
        cargo_manifest_before,
        "the CLI must not mutate the application Cargo manifest"
    );
}

impl RuntimeHarness<'_> {
    fn run_json(&self, entry_point: EntryPoint, args: &[&str]) -> JsonCommandOutput {
        let mut command = match entry_point {
            EntryPoint::Direct => Command::new(self.direct_bin),
            EntryPoint::Wrapper => {
                let mut command = Command::new(
                    self.bin_dir
                        .join(format!("cargo-leptos_ui_kit{}", env::consts::EXE_SUFFIX)),
                );
                command.arg("leptos_ui_kit");
                command
            }
            EntryPoint::Cargo => {
                let mut command = Command::new(env!("CARGO"));
                command.arg("leptos_ui_kit");
                command
            }
        };
        sanitize_command_environment(&mut command);
        command
            .current_dir(self.app_root)
            .env("CARGO_HOME", self.runtime_cargo_home)
            .env_remove("CARGO_TARGET_DIR")
            .env("PATH", runtime_path(self.bin_dir))
            .args(args);
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("run {} {args:?}: {error}", entry_point.label()));
        assert_success(&format!("{} {args:?}", entry_point.label()), &output);
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 runtime stdout");
        let stderr = String::from_utf8(output.stderr).expect("UTF-8 runtime stderr");
        assert_no_path_leaks("runtime stdout", &stdout, self.forbidden);
        assert_no_path_leaks("runtime stderr", &stderr, self.forbidden);
        assert!(
            stderr.is_empty(),
            "{} {args:?} emitted unexpected JSON-mode stderr:\n{stderr}",
            entry_point.label()
        );
        assert!(
            !self.deleted_cargo_home.exists(),
            "{} {args:?} recreated deleted Cargo home",
            entry_point.label()
        );
        assert_empty_dir(self.runtime_cargo_home);
        let value = serde_json::from_str::<Value>(&stdout).unwrap_or_else(|error| {
            panic!(
                "parse {} {args:?} JSON: {error}\n{stdout}",
                entry_point.label()
            )
        });
        JsonCommandOutput { stdout, value }
    }
}

fn assert_version(value: &Value, expected_rev: &str) {
    assert_envelope(value, "version", "success");
    assert_object_keys(
        &value["data"],
        "version data",
        &["binary", "package", "schemaVersion", "source", "version"],
    );
    assert_eq!(value["data"]["package"], "leptos_ui_kit_cli");
    assert_eq!(value["data"]["binary"], "leptos_ui_kit");
    assert_eq!(value["data"]["version"], "0.1.0");
    assert_eq!(value["data"]["schemaVersion"], "0.9.0-alpha");
    assert_object_keys(
        &value["data"]["source"],
        "version source",
        &["kind", "rev", "url"],
    );
    assert_eq!(value["data"]["source"]["kind"], "git");
    assert_eq!(
        value["data"]["source"]["url"],
        "https://github.com/triesap/leptos_ui_kit"
    );
    assert_eq!(value["data"]["source"]["rev"], expected_rev);
    assert_full_revision(
        value["data"]["source"]["rev"]
            .as_str()
            .expect("version revision"),
    );
}

fn assert_source_view(value: &Value, source_path: &str, expected_sources: &[&str]) {
    assert_envelope(value, "view", "success");
    assert_object_keys(&value["data"], "source view data", &["resolved", "sources"]);
    let expected_name = source_path
        .rsplit('/')
        .next()
        .expect("registry manifest name")
        .strip_suffix(".json")
        .expect("registry manifest extension");
    let expected_kind = if source_path.starts_with("foundation/") {
        "foundation"
    } else {
        "ui"
    };
    assert_registry_item(
        &value["data"]["resolved"],
        expected_name,
        expected_kind,
        source_path,
    );
    let actual = value["data"]["sources"]
        .as_array()
        .expect("source view sources")
        .iter()
        .map(|source| {
            assert_object_keys(source, "source view source", &["content", "kind", "path"]);
            let path = source["path"].as_str().expect("source view logical path");
            assert_logical_path(path);
            assert_eq!(
                source["kind"],
                if path.ends_with(".rs") { "rust" } else { "css" }
            );
            assert!(
                source["content"]
                    .as_str()
                    .is_some_and(|content| !content.is_empty()),
                "source view content for {path}"
            );
            path
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected_sources);
}

fn assert_envelope(value: &Value, command: &str, status: &str) {
    assert_object_keys(
        value,
        "command envelope",
        &[
            "changes",
            "command",
            "data",
            "diagnostics",
            "schemaVersion",
            "status",
        ],
    );
    assert_eq!(value["schemaVersion"], "0.9.0-alpha");
    assert_eq!(value["command"], command);
    assert_eq!(value["status"], status);
    assert_eq!(value["diagnostics"], Value::Array(Vec::new()));
    assert!(value["changes"].is_array());
    assert!(value.get("data").is_some());
}

fn assert_nonempty_changes(value: &Value, label: &str) {
    let changes = value["changes"]
        .as_array()
        .unwrap_or_else(|| panic!("{label} envelope changes"));
    assert!(!changes.is_empty(), "{label} must report its writes");
    for change in changes {
        let expected_keys = if change.get("item").is_some() {
            &["item", "kind", "path", "tracked"][..]
        } else {
            &["kind", "path", "tracked"][..]
        };
        assert_object_keys(change, &format!("{label} change"), expected_keys);
        let path = change["path"].as_str().expect("change logical path");
        assert_logical_path(path);
        assert!(change["kind"].is_string(), "{label} change kind");
        assert!(change["tracked"].is_boolean(), "{label} tracked flag");
    }
}

fn assert_catalog_views(runtime: &RuntimeHarness<'_>) {
    let mut observed = BTreeSet::new();
    for (index, (name, kind, source_path)) in BUILT_IN_CATALOG.iter().enumerate() {
        let entry_point = if index % 2 == 0 {
            EntryPoint::Direct
        } else {
            EntryPoint::Cargo
        };
        let output = runtime.run_json(entry_point, &["view", name, "--json"]);
        assert_envelope(&output.value, "view", "success");
        assert_registry_item(&output.value["data"], name, kind, source_path);
        observed.insert(
            output.value["data"]["name"]
                .as_str()
                .expect("catalog item name")
                .to_owned(),
        );
    }
    assert_eq!(
        observed,
        BUILT_IN_CATALOG
            .iter()
            .map(|(name, _, _)| (*name).to_owned())
            .collect(),
        "source-independent embedded catalog"
    );
}

fn assert_registry_item(value: &Value, name: &str, kind: &str, source_path: &str) {
    assert_object_keys(
        value,
        &format!("registry item {name}"),
        &[
            "cargoPlan",
            "contentHash",
            "description",
            "kind",
            "name",
            "registryDependencies",
            "sourceKind",
            "sourcePath",
            "targets",
            "title",
            "version",
        ],
    );
    assert_eq!(value["sourceKind"], "built-in");
    assert_eq!(value["sourcePath"], source_path);
    assert_logical_path(
        value["sourcePath"]
            .as_str()
            .expect("registry item source path"),
    );
    assert_eq!(value["name"], name);
    assert_eq!(value["kind"], kind);
    assert_eq!(value["version"], "0.9.0-alpha");
    assert_sha256_hash(&value["contentHash"], &format!("{name}.contentHash"));
    assert!(
        value["title"]
            .as_str()
            .is_some_and(|title| !title.is_empty()),
        "{name} title"
    );
    assert!(
        value["description"]
            .as_str()
            .is_some_and(|description| !description.is_empty()),
        "{name} description"
    );
    assert!(
        value["registryDependencies"]
            .as_array()
            .is_some_and(|dependencies| dependencies.iter().all(Value::is_string)),
        "{name} registry dependencies"
    );

    assert_object_keys(
        &value["targets"],
        &format!("{name} targets"),
        &["styleBlocks", "uiFiles"],
    );
    for file in value["targets"]["uiFiles"]
        .as_array()
        .expect("registry UI targets")
    {
        assert_object_keys(file, &format!("{name} UI target"), &["path", "source"]);
        assert_logical_path(file["source"].as_str().expect("registry UI source"));
        assert_logical_path(file["path"].as_str().expect("registry UI target path"));
    }
    for block in value["targets"]["styleBlocks"]
        .as_array()
        .expect("registry style targets")
    {
        assert_object_keys(block, &format!("{name} style target"), &["id", "source"]);
        assert_logical_path(block["source"].as_str().expect("registry style source"));
        assert!(
            block["id"].as_str().is_some_and(|id| !id.is_empty()),
            "{name} style block ID"
        );
    }
    for dependency in value["cargoPlan"].as_array().expect("registry Cargo plan") {
        assert_cargo_requirement(dependency);
    }
}

fn assert_cargo_requirement(value: &Value) {
    assert_object_keys(
        value,
        "Cargo requirement",
        &["crate", "features", "required", "source"],
    );
    assert!(
        value["crate"]
            .as_str()
            .is_some_and(|crate_name| !crate_name.is_empty()),
        "Cargo requirement crate"
    );
    assert!(
        value["features"]
            .as_array()
            .is_some_and(|features| features.iter().all(Value::is_string)),
        "Cargo requirement features"
    );
    assert!(value["required"].is_boolean(), "Cargo requirement required");
    match value["source"]["kind"].as_str() {
        Some("version") => {
            assert_object_keys(&value["source"], "version source", &["kind", "version"]);
            assert!(value["source"]["version"].is_string());
        }
        Some("git") => {
            assert_object_keys(&value["source"], "Git source", &["kind", "rev", "url"]);
            assert!(value["source"]["url"].is_string());
            assert_full_revision(
                value["source"]["rev"]
                    .as_str()
                    .expect("Git requirement revision"),
            );
        }
        source => panic!("unsupported Cargo requirement source: {source:?}"),
    }
}

fn assert_add(value: &Value, item_id: &str, item_name: &str) {
    assert_envelope(value, "add", "success");
    assert_object_keys(
        &value["data"],
        "add data",
        &["contentHash", "dependencies", "itemId", "itemName"],
    );
    assert_eq!(value["data"]["itemId"], item_id);
    assert_eq!(value["data"]["itemName"], item_name);
    assert_sha256_hash(
        &value["data"]["contentHash"],
        &format!("{item_name}.contentHash"),
    );
    for dependency in value["data"]["dependencies"]
        .as_array()
        .expect("add dependencies")
    {
        assert_cargo_requirement(dependency);
    }
}

fn assert_sync(value: &Value, status: &str) {
    assert_envelope(value, "sync", status);
    assert_object_keys(&value["data"], "sync data", &["dependencies", "itemIds"]);
    let item_ids = value["data"]["itemIds"]
        .as_array()
        .expect("sync item IDs")
        .iter()
        .map(|item| item.as_str().expect("sync item ID"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        item_ids,
        BTreeSet::from(["builtin:button", "builtin:spinner", "builtin:tokens"])
    );
    for dependency in value["data"]["dependencies"]
        .as_array()
        .expect("sync dependencies")
    {
        assert_cargo_requirement(dependency);
    }
}

fn assert_info(value: &Value, installed: bool) {
    assert_envelope(value, "info", "success");
    assert_object_keys(
        &value["data"],
        "info data",
        &[
            "cargoManifest",
            "configPath",
            "indexHtml",
            "installed",
            "projectKind",
            "projectRoot",
            "registryAvailable",
            "renderMode",
            "renderModeContract",
            "renderModeSelection",
            "sourceRoot",
            "stylesheet",
            "workspaceMode",
        ],
    );
    assert_eq!(value["data"]["projectRoot"], ".");
    assert_eq!(value["data"]["projectKind"], "single-crate-trunk-csr");
    assert_eq!(
        value["data"]["workspaceMode"],
        "single-package-workspace-root"
    );
    assert_eq!(value["data"]["cargoManifest"], "Cargo.toml");
    assert_eq!(value["data"]["sourceRoot"], "src");
    assert_eq!(value["data"]["indexHtml"], "index.html");
    assert_eq!(value["data"]["stylesheet"], "styles/kit.css");
    assert_eq!(value["data"]["renderMode"], "csr");
    assert_eq!(
        value["data"]["renderModeContract"],
        serde_json::json!({"kind": "selected", "mode": "csr"})
    );
    assert_eq!(
        value["data"]["renderModeSelection"],
        serde_json::json!({"kind": "selected", "mode": "csr"})
    );
    assert_eq!(value["data"]["registryAvailable"], true);
    if installed {
        assert_eq!(value["data"]["configPath"], CONFIG_PATH);
        let summary = &value["data"]["installed"];
        assert_object_keys(
            summary,
            "installed summary",
            &["filePaths", "itemIds", "lockPath", "styleBlockIds"],
        );
        assert_eq!(summary["lockPath"], LOCK_PATH);
        assert_string_set(
            &summary["itemIds"],
            &["builtin:button", "builtin:spinner", "builtin:tokens"],
            "installed item IDs",
        );
        assert_string_set(
            &summary["filePaths"],
            &[
                "src/components/ui/button.rs",
                "src/components/ui/spinner.rs",
            ],
            "installed file paths",
        );
        assert_string_set(
            &summary["styleBlockIds"],
            &["button", "spinner", "tokens"],
            "installed style block IDs",
        );
    } else {
        assert!(value["data"]["configPath"].is_null());
        assert!(value["data"]["installed"].is_null());
    }
}

fn assert_doctor(value: &Value) {
    assert_envelope(value, "doctor", "success");
    assert_object_keys(
        &value["data"],
        "doctor data",
        &["check", "checks", "projectRoot", "strict", "trunkBuild"],
    );
    assert_eq!(value["data"]["projectRoot"], ".");
    assert_eq!(value["data"]["strict"], true);
    assert_eq!(value["data"]["check"], false);
    assert_eq!(value["data"]["trunkBuild"], false);
    let checks = value["data"]["checks"].as_array().expect("doctor checks");
    assert!(!checks.is_empty(), "strict doctor must report its checks");
    for check in checks {
        let expected_keys = if check.get("path").is_some() {
            &["message", "name", "path", "status"][..]
        } else {
            &["message", "name", "status"][..]
        };
        assert_object_keys(check, "doctor check", expected_keys);
        assert_eq!(check["status"], "pass");
        assert!(check["name"].is_string());
        assert!(check["message"].is_string());
        if let Some(path) = check.get("path").and_then(Value::as_str) {
            assert_logical_path(path);
        }
    }
    let registry_checks = checks
        .iter()
        .filter(|check| check["name"] == "registry")
        .collect::<Vec<_>>();
    assert_eq!(registry_checks.len(), 1, "embedded registry health check");
    assert_eq!(
        registry_checks[0]["message"],
        "built-in registry runtime health is valid"
    );
}

fn assert_string_set(value: &Value, expected: &[&str], label: &str) {
    let actual = value
        .as_array()
        .unwrap_or_else(|| panic!("{label} must be an array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("{label} entry must be a string"))
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected.iter().copied().collect(), "{label}");
}

fn assert_object_keys(value: &Value, label: &str, expected: &[&str]) {
    let actual = value
        .as_object()
        .unwrap_or_else(|| panic!("{label} must be an object"))
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected.iter().copied().collect(), "{label} keys");
}

fn assert_generated_state(app_root: &Path, expected_rev: &str, forbidden: &[PathBuf]) {
    let config_bytes = fs::read(app_root.join(CONFIG_PATH)).expect("read generated config");
    let lock_bytes = fs::read(app_root.join(LOCK_PATH)).expect("read generated lock");
    let config_text = String::from_utf8(config_bytes.clone()).expect("UTF-8 generated config");
    let lock_text = String::from_utf8(lock_bytes.clone()).expect("UTF-8 generated lock");
    assert_no_path_leaks("generated config", &config_text, forbidden);
    assert_no_path_leaks("generated lock", &lock_text, forbidden);

    let config: Value = serde_json::from_str(&config_text).expect("parse generated config");
    assert_eq!(
        config["$schema"],
        "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/kit.schema.json"
    );
    assert_eq!(config["schemaVersion"], "0.9.0-alpha");
    assert_eq!(config["tool"]["package"], "leptos_ui_kit_cli");
    assert_eq!(config["tool"]["binary"], "leptos_ui_kit");
    assert_eq!(config["tool"]["source"]["kind"], "git");
    assert_eq!(config["tool"]["source"]["rev"], expected_rev);
    assert_eq!(
        config["tool"]["source"]["url"],
        "https://github.com/triesap/leptos_ui_kit"
    );
    assert_eq!(config["project"]["kind"], "single-crate-trunk-csr");
    assert_eq!(config["project"]["crateRoot"], ".");
    assert_eq!(config["project"]["srcDir"], "src");
    assert_eq!(config["project"]["indexHtml"], "index.html");
    assert_eq!(config["leptos"]["version"], "0.9.0-alpha");
    assert_eq!(config["leptos"]["routerVersion"], "0.9.0-alpha");
    assert_eq!(config["leptos"]["renderMode"], "csr");
    assert_eq!(config["install"]["uiDir"], "src/components/ui");
    assert_eq!(config["install"]["uiMod"], "src/components/ui/mod.rs");
    assert_eq!(config["install"]["componentsMod"], "src/components/mod.rs");
    assert_eq!(config["styles"]["mode"], "pure-css");
    assert_eq!(config["styles"]["css"], "styles/kit.css");
    assert_eq!(config["registry"]["source"], "builtin");
    let desired = config["items"]
        .as_array()
        .expect("desired config items")
        .iter()
        .map(|item| {
            assert_eq!(item["source"], "builtin");
            item["name"].as_str().expect("desired item name")
        })
        .collect::<Vec<_>>();
    assert_eq!(desired, ["tokens", "spinner", "button"]);
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

    let lock: Value = serde_json::from_str(&lock_text).expect("parse generated lock");
    assert_eq!(lock["schemaVersion"], "0.9.0-alpha");
    assert_eq!(lock["kitVersion"], "0.9.0-alpha");
    assert_eq!(
        lock["project"]["configHash"],
        sha256_hash(&config_bytes),
        "lock must identify the exact generated config bytes"
    );
    assert_sha256_hash(&lock["project"]["configHash"], "project.configHash");
    assert_eq!(lock["project"]["crateRoot"], ".");
    assert_eq!(lock["project"]["kind"], "single-crate-trunk-csr");
    let items = lock["items"].as_object().expect("lock item map");
    assert_eq!(
        items.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from(["builtin:button", "builtin:spinner", "builtin:tokens"])
    );
    let expected_items = [
        (
            "builtin:button",
            "button",
            Some("src/components/ui/button.rs"),
            "button",
        ),
        (
            "builtin:spinner",
            "spinner",
            Some("src/components/ui/spinner.rs"),
            "spinner",
        ),
        ("builtin:tokens", "tokens", None, "tokens"),
    ];
    let css_path = config["styles"]["css"]
        .as_str()
        .expect("configured stylesheet");
    let css_bytes = fs::read(app_root.join(css_path)).expect("read managed stylesheet");
    let css_text = String::from_utf8(css_bytes).expect("UTF-8 managed stylesheet");
    assert_no_path_leaks("managed stylesheet", &css_text, forbidden);
    let mut block_positions = BTreeMap::new();

    for (item_id, expected_name, expected_file, expected_block) in expected_items {
        let item = items
            .get(item_id)
            .unwrap_or_else(|| panic!("missing installed item {item_id}"));
        assert_eq!(item["id"], item_id);
        assert_eq!(item["name"], expected_name);
        assert_eq!(item["source"], "builtin");
        assert_eq!(item["version"], "0.9.0-alpha");
        assert_sha256_hash(&item["contentHash"], &format!("{item_id}.contentHash"));

        let files = item["files"].as_array().expect("installed item files");
        assert_eq!(files.len(), usize::from(expected_file.is_some()));
        if let Some(expected_file) = expected_file {
            let file = &files[0];
            assert_eq!(file["path"], expected_file);
            assert_eq!(file["kind"], "rust");
            assert_logical_path(expected_file);
            let disk_bytes = fs::read(app_root.join(expected_file))
                .unwrap_or_else(|error| panic!("read installed file {expected_file}: {error}"));
            let disk_hash = sha256_hash(&disk_bytes);
            assert_eq!(file["generatedHash"], disk_hash);
            assert_eq!(file["localHashAtInstall"], disk_hash);
            assert_sha256_hash(
                &file["generatedHash"],
                &format!("{item_id}.{expected_file}.generatedHash"),
            );
            assert_sha256_hash(
                &file["localHashAtInstall"],
                &format!("{item_id}.{expected_file}.localHashAtInstall"),
            );
        }

        let blocks = item["styleBlocks"]
            .as_array()
            .expect("installed style blocks");
        assert_eq!(blocks.len(), 1, "{item_id} style block count");
        let block = &blocks[0];
        assert_eq!(block["cssPath"], css_path);
        assert_eq!(block["blockId"], expected_block);
        assert_logical_path(css_path);
        assert_sha256_hash(
            &block["generatedHash"],
            &format!("{item_id}.{expected_block}.generatedHash"),
        );

        let start_marker = format!("/* leptos-ui-kit:start {expected_block} */");
        let end_marker = format!("/* leptos-ui-kit:end {expected_block} */");
        assert_eq!(css_text.matches(&start_marker).count(), 1);
        assert_eq!(css_text.matches(&end_marker).count(), 1);
        let start = css_text
            .find(&start_marker)
            .unwrap_or_else(|| panic!("missing {expected_block} CSS start marker"));
        let end_start = css_text
            .find(&end_marker)
            .unwrap_or_else(|| panic!("missing {expected_block} CSS end marker"));
        assert!(
            start < end_start,
            "{expected_block} CSS markers are reversed"
        );
        let mut end = end_start + end_marker.len();
        if css_text.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        }
        assert_eq!(
            block["generatedHash"],
            sha256_hash(&css_text.as_bytes()[start..end]),
            "{expected_block} lock hash must match its exact managed CSS block"
        );
        block_positions.insert(expected_block, start);
    }

    assert!(block_positions["tokens"] < block_positions["spinner"]);
    assert!(block_positions["spinner"] < block_positions["button"]);

    let files_by_path = lock["filesByPath"]
        .as_object()
        .expect("files-by-path map")
        .iter()
        .map(|(path, item_id)| {
            (
                path.as_str(),
                item_id.as_str().expect("files-by-path item id"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        files_by_path,
        BTreeMap::from([
            ("src/components/ui/button.rs", "builtin:button"),
            ("src/components/ui/spinner.rs", "builtin:spinner"),
        ])
    );
    for path in files_by_path.keys() {
        assert_logical_path(path);
    }

    let style_blocks_by_id = lock["styleBlocksById"]
        .as_object()
        .expect("style-blocks-by-id map")
        .iter()
        .map(|(block_id, item_id)| {
            (
                block_id.as_str(),
                item_id.as_str().expect("style-block item id"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        style_blocks_by_id,
        BTreeMap::from([
            ("button", "builtin:button"),
            ("spinner", "builtin:spinner"),
            ("tokens", "builtin:tokens"),
        ])
    );

    for (relative, bytes) in snapshot_tree(app_root) {
        assert_no_path_leaks_bytes(
            &format!("application file {}", relative.display()),
            &bytes,
            forbidden,
        );
    }
}

fn assert_sha256_hash(value: &Value, label: &str) {
    let hash = value
        .as_str()
        .unwrap_or_else(|| panic!("{label} must be a string"));
    let Some(digest) = hash.strip_prefix("sha256:") else {
        panic!("{label} must use the sha256: prefix: {hash}");
    };
    assert_eq!(digest.len(), 64, "{label} digest length: {hash}");
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{label} must be lowercase hexadecimal: {hash}"
    );
}

fn sha256_hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn forbidden_build_paths(explicit: &[&Path]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut candidates = explicit
        .iter()
        .map(|path| Some((*path).to_path_buf()))
        .collect::<Vec<_>>();
    candidates.extend([
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))),
        option_env!("OUT_DIR").map(PathBuf::from),
        env::var_os("CARGO_HOME").map(PathBuf::from),
        env::var_os("CARGO_TARGET_DIR").map(PathBuf::from),
        env::current_exe().ok(),
    ]);
    for path in candidates.into_iter().flatten() {
        if path.is_absolute() && !paths.contains(&path) {
            if let Ok(canonical) = path.canonicalize()
                && !paths.contains(&canonical)
            {
                paths.push(canonical);
            }
            paths.push(path);
        }
    }
    paths
}

fn assert_no_path_leaks(label: &str, output: &str, forbidden: &[PathBuf]) {
    for path in forbidden {
        let raw = path.to_string_lossy();
        let slash = raw.replace('\\', "/");
        let encoded = serde_json::to_string(raw.as_ref()).expect("encode forbidden path");
        let encoded = encoded.trim_matches('"');
        for candidate in [raw.as_ref(), slash.as_str(), encoded] {
            assert!(
                !candidate.is_empty() && !output.contains(candidate),
                "{label} leaked private path {candidate}:\n{output}"
            );
        }
    }
}

fn assert_no_path_leaks_bytes(label: &str, output: &[u8], forbidden: &[PathBuf]) {
    for path in forbidden {
        let raw = path.to_string_lossy();
        let slash = raw.replace('\\', "/");
        let encoded = serde_json::to_string(raw.as_ref()).expect("encode forbidden path");
        let encoded = encoded.trim_matches('"');
        for candidate in [raw.as_ref(), slash.as_str(), encoded] {
            assert!(!candidate.is_empty(), "forbidden path must not be empty");
            assert!(
                !contains_bytes(output, candidate.as_bytes()),
                "{label} leaked private path {candidate}"
            );
        }
    }
}

fn assert_logical_path(path: &str) {
    assert!(!path.is_empty(), "logical path must not be empty");
    assert!(!Path::new(path).is_absolute(), "absolute path: {path}");
    assert!(!path.contains('\\'), "backslash in logical path: {path}");
    assert!(
        !path.split('/').any(|segment| segment == ".."),
        "parent traversal in logical path: {path}"
    );
}

fn runtime_path(bin_dir: &Path) -> std::ffi::OsString {
    let mut paths = vec![bin_dir.to_path_buf()];
    if let Some(path) = env::var_os("PATH") {
        paths.extend(env::split_paths(&path));
    }
    env::join_paths(paths).expect("build runtime PATH")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copied fixture directory");
    let mut entries = fs::read_dir(from)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", from.display()))
        .collect::<Result<Vec<_>, _>>()
        .expect("read fixture entries");
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let source = entry.path();
        let destination = to.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source).expect("inspect fixture entry");
        assert!(!metadata.file_type().is_symlink(), "{}", source.display());
        if metadata.is_dir() {
            copy_dir(&source, &destination);
        } else {
            assert!(metadata.is_file(), "{}", source.display());
            fs::copy(&source, &destination).expect("copy fixture file");
        }
    }
}

fn collect_files(root: &Path) -> BTreeSet<String> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeSet<String>) {
        let mut entries = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
            .collect::<Result<Vec<_>, _>>()
            .expect("read directory entries");
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("inspect file inventory entry");
            assert!(!metadata.file_type().is_symlink(), "{}", path.display());
            if metadata.is_dir() {
                visit(root, &path, files);
            } else {
                assert!(metadata.is_file(), "{}", path.display());
                let relative = path.strip_prefix(root).expect("inventory-relative path");
                assert!(
                    relative
                        .components()
                        .all(|component| matches!(component, Component::Normal(_)))
                );
                files.insert(
                    relative
                        .components()
                        .map(|component| {
                            component
                                .as_os_str()
                                .to_str()
                                .expect("UTF-8 inventory path")
                        })
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
        }
    }

    let mut files = BTreeSet::new();
    visit(root, root, &mut files);
    files
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).expect("read snapshot directory") {
            let entry = entry.expect("read snapshot entry");
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, snapshot);
            } else {
                assert!(path.is_file(), "snapshot entries must be regular files");
                snapshot.insert(
                    path.strip_prefix(root)
                        .expect("snapshot-relative path")
                        .to_path_buf(),
                    fs::read(&path).expect("read snapshot file"),
                );
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

fn assert_regular_file(path: &Path) {
    let metadata = fs::symlink_metadata(path)
        .unwrap_or_else(|error| panic!("inspect {}: {error}", path.display()));
    assert!(!metadata.file_type().is_symlink(), "{}", path.display());
    assert!(metadata.is_file(), "{}", path.display());
}

fn assert_empty_dir(path: &Path) {
    let mut entries = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("read empty directory {}: {error}", path.display()));
    assert!(
        entries.next().is_none(),
        "{} must remain empty",
        path.display()
    );
}

fn file_hash(path: &Path) -> Vec<u8> {
    Sha256::digest(
        fs::read(path).unwrap_or_else(|error| panic!("hash {}: {error}", path.display())),
    )
    .to_vec()
}
