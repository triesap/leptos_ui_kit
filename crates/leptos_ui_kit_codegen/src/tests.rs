use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use leptos_ui_kit_registry::{
    ConfigError, DEFAULT_KIT_CONFIG_PATH, KitConfig, SCHEMA_VERSION, TOOL_GIT_URL,
    ToolSourceConfig, canonical_kit_json, canonical_tool_config, desired_builtin_button_item,
    kit_config_to_json, kit_config_with_desired_item, parse_kit_json_str,
    read_built_in_registry_source, resolve_built_in_registry_items,
};
use serde::Serialize;

use super::*;

#[derive(Serialize)]
struct DemoData {
    value: &'static str,
}

#[test]
fn serializes_diagnostics_and_change_records_in_json_envelope() {
    let envelope = CommandEnvelope::new("add", CommandStatus::Planned, DemoData { value: "ok" })
        .with_diagnostics(vec![
            Diagnostic::new(DiagnosticLevel::Warning, "demo.warning", "heads up")
                .with_path(DEFAULT_KIT_CONFIG_PATH)
                .with_suggestion("Run leptos_ui_kit init."),
        ])
        .with_changes(vec![
            ChangeRecord::new(ChangeKind::CreateFile, "src/components/ui/button.rs", true)
                .with_item("builtin:button"),
        ]);

    let json = serde_json::to_string(&envelope).expect("serialize envelope");

    assert!(json.contains(r#""schemaVersion":"0.9.0-alpha""#));
    assert!(json.contains(r#""command":"add""#));
    assert!(json.contains(r#""status":"planned""#));
    assert!(json.contains(r#""level":"warning""#));
    assert!(json.contains(r#""kind":"create_file""#));
    assert!(json.contains(r#""data":{"value":"ok"}"#));
}

#[test]
fn init_plan_creates_missing_project_files_without_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");

    let plan = plan_init(root).expect("plan init");

    assert_eq!(plan.files.len(), 6);
    assert!(
        plan.files
            .iter()
            .any(|file| file.path == DEFAULT_KIT_CONFIG_PATH)
    );
    assert!(plan.files.iter().any(|file| file.path == "styles/kit.css"));
    assert!(plan.files.iter().any(|file| file.path == "index.html"));
    assert!(
        plan.files
            .iter()
            .any(|file| file.path == "src/components/mod.rs")
    );
    assert!(
        plan.files
            .iter()
            .any(|file| file.path == DEFAULT_KIT_LOCK_PATH)
    );
    assert!(!root.join(DEFAULT_KIT_CONFIG_PATH).exists());
}

#[test]
fn shared_library_init_skips_html_and_records_project_kind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src/components/ui/_kit")).expect("create kit directory");
    fs::write(
        root.join(DEFAULT_KIT_CONFIG_PATH),
        include_str!("../../../tests/fixtures/shared_library/src/components/ui/_kit/kit.json"),
    )
    .expect("write shared-library config");

    let plan = plan_init(root).expect("plan shared-library init");

    assert!(!plan.files.iter().any(|file| file.path == "index.html"));
    assert!(plan.files.iter().any(|file| file.path == "styles/kit.css"));
    let lock_file = plan
        .files
        .iter()
        .find(|file| file.path == DEFAULT_KIT_LOCK_PATH)
        .expect("shared install lock");
    let lock = parse_install_lock_str(&lock_file.content).expect("parse shared install lock");
    assert_eq!(lock.project.kind, "shared-library");
}

#[test]
fn init_plan_rejects_invalid_existing_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_kit_config(root, "{\"tailwind\":true}\n");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");

    let error = plan_init(root).expect_err("invalid config should fail");

    assert!(matches!(error, CodegenError::Config(_)));
}

#[test]
fn init_plan_reports_typed_missing_provenance_before_config_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");

    let error = plan_init_with_config_provider(root, || {
        Err(ConfigError::MissingToolProvenance {
            package: leptos_ui_kit_registry::TOOL_PACKAGE,
            binary: leptos_ui_kit_registry::TOOL_BINARY,
        })
    })
    .expect_err("missing provenance must prevent config generation");

    assert!(matches!(
        error,
        CodegenError::Config(ConfigError::MissingToolProvenance {
            package: leptos_ui_kit_registry::TOOL_PACKAGE,
            binary: leptos_ui_kit_registry::TOOL_BINARY,
        })
    ));
    assert!(!root.join(DEFAULT_KIT_CONFIG_PATH).exists());
}

#[test]
fn init_plan_does_not_require_compiled_provenance_for_an_existing_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    write_kit_config(root, canonical_kit_json().expect("canonical config"));
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");

    plan_init_with_config_provider(root, || {
        panic!("existing config must not request new compiled provenance")
    })
    .expect("plan from existing config");
}

#[test]
fn init_plan_uses_configured_stylesheet_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    let config = canonical_kit_json().expect("canonical config").replace(
        "\"css\": \"styles/kit.css\"",
        "\"css\": \"styles/custom.css\"",
    );
    write_kit_config(root, config);
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");

    let plan = plan_init(root).expect("plan init");

    assert!(
        plan.files
            .iter()
            .any(|file| file.path == "styles/custom.css")
    );
    assert!(!plan.files.iter().any(|file| file.path == "styles/kit.css"));
    let index = plan
        .files
        .iter()
        .find(|file| file.path == "index.html")
        .expect("index plan");
    assert!(index.content.contains("styles/custom.css"));
}

#[test]
fn init_write_creates_expected_files_and_persistent_coordination() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");

    let plan = apply_init(root).expect("apply init");

    assert!(!plan.is_empty());
    assert!(root.join(DEFAULT_KIT_CONFIG_PATH).is_file());
    assert!(root.join("styles/kit.css").is_file());
    assert!(root.join("src/components/mod.rs").is_file());
    assert!(root.join("src/components/ui/mod.rs").is_file());
    assert!(root.join(DEFAULT_KIT_LOCK_PATH).is_file());
    assert_exact_persistent_coordination(root);
    assert!(
        fs::read_to_string(root.join("index.html"))
            .expect("read index")
            .contains("styles/kit.css")
    );
}

#[test]
fn init_write_is_idempotent_when_files_are_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");

    apply_init(root).expect("first init");
    let second = apply_init(root).expect("second init");

    assert!(second.is_empty());
}

#[test]
fn lock_round_trips_deterministically() {
    let lock = InstallLock::empty(hash_bytes(b"components"));
    let first = lock_to_json(&lock).expect("serialize first");
    let parsed = parse_install_lock_str(&first).expect("parse lock");
    let second = lock_to_json(&parsed).expect("serialize second");

    assert_eq!(first, second);
    assert!(first.contains("\"schemaVersion\": \"0.9.0-alpha\""));
    assert!(first.contains("\"configHash\": \"sha256:"));
    assert!(!first.contains("null"));
}

#[test]
fn lock_rejects_malformed_hash_fields() {
    let mut lock = InstallLock::empty("sha256:not-a-real-hash".to_owned());

    let error = lock.validate().expect_err("config hash should fail");

    assert!(
        matches!(error, CodegenError::InvalidLock { reason, .. } if reason.contains("project.configHash"))
    );

    lock.project.config_hash = hash_bytes(b"components");
    lock.items.insert(
        "builtin:button".to_owned(),
        InstalledItem {
            id: "builtin:button".to_owned(),
            name: "button".to_owned(),
            source: "builtin".to_owned(),
            version: SCHEMA_VERSION.to_owned(),
            content_hash: "missing-prefix".to_owned(),
            files: Vec::new(),
            style_blocks: Vec::new(),
        },
    );

    let error = lock.validate().expect_err("content hash should fail");

    assert!(
        matches!(error, CodegenError::InvalidLock { reason, .. } if reason.contains("items[].contentHash"))
    );
}

#[test]
fn add_plan_records_generated_hashes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");

    let plan = plan_add(root, "button").expect("plan add");
    let source = read_built_in_registry_source("ui/button.rs").expect("registry source");
    let css = read_built_in_registry_source("styles/button.css").expect("registry css");
    let rust_target = plan
        .files
        .iter()
        .find(|file| file.path == "src/components/ui/button.rs")
        .expect("rust target");
    let installed_file = &plan.lock.items["builtin:button"].files[0];
    let installed_block = &plan.lock.items["builtin:button"].style_blocks[0];

    assert_eq!(rust_target.content, source);
    assert_eq!(installed_file.generated_hash, hash_bytes(source.as_bytes()));
    assert_eq!(
        installed_file.local_hash_at_install,
        hash_bytes(source.as_bytes())
    );
    assert_eq!(installed_block.generated_hash, hash_bytes(css.as_bytes()));
    assert_eq!(
        plan.lock.files_by_path.get("src/components/ui/button.rs"),
        Some(&"builtin:button".to_owned())
    );
    assert_eq!(
        plan.lock.style_blocks_by_id.get("button"),
        Some(&"builtin:button".to_owned())
    );
    assert_eq!(plan.cargo_plan.len(), 1);
    assert!(
        plan.cargo_plan
            .iter()
            .any(|entry| entry.crate_name == "leptos" && entry.features == ["csr".to_owned()])
    );
}

#[test]
fn add_plan_reports_button_changes_without_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");

    let plan = plan_add(root, "button").expect("plan add");
    let paths = plan
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();

    assert!(paths.contains(&"src/components/ui/button.rs"));
    assert!(paths.contains(&"src/components/ui/mod.rs"));
    assert!(paths.contains(&"styles/kit.css"));
    assert!(paths.contains(&DEFAULT_KIT_LOCK_PATH));
    assert_eq!(
        plan.files
            .iter()
            .filter(|file| file.path == "styles/kit.css")
            .count(),
        1
    );
    assert_eq!(
        plan.changes
            .iter()
            .filter(|change| change.path == "styles/kit.css")
            .count(),
        1
    );
    assert_eq!(plan.cargo_plan.len(), 1);
    assert!(!root.join("src/components/ui/button.rs").exists());
    assert!(root.join(DEFAULT_KIT_LOCK_PATH).is_file());
}

#[test]
fn add_plan_uses_configured_stylesheet_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    let config = canonical_kit_json().expect("canonical config").replace(
        "\"css\": \"styles/kit.css\"",
        "\"css\": \"styles/custom.css\"",
    );
    write_kit_config(root, config);
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");

    let plan = plan_add(root, "button").expect("plan add");

    assert!(
        plan.files
            .iter()
            .any(|file| file.path == "styles/custom.css")
    );
    assert!(!plan.files.iter().any(|file| file.path == "styles/kit.css"));
    assert_eq!(
        plan.lock.items["builtin:button"].style_blocks[0].css_path,
        "styles/custom.css"
    );
}

#[test]
fn item_planner_supports_nested_ui_targets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");
    let mut files = Vec::new();
    let mut changes = Vec::new();
    let mut lock = InstallLock::empty(hash_bytes(b"components"));
    let mut css_operations = Vec::new();
    let config = parse_kit_json_str(
        &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
    )
    .expect("parse config");
    let item = nested_registry_item();
    let context = PlanningContext::open(root).expect("open planning context");

    let item_id = plan_built_in_item(
        &context,
        &mut files,
        &mut changes,
        &mut lock,
        &config,
        &item,
        &mut css_operations,
    )
    .expect("plan item");
    let paths = files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    let ui_mod = files
        .iter()
        .find(|file| file.path == "src/components/ui/mod.rs")
        .expect("ui mod");

    assert_eq!(item_id, "builtin:nested");
    assert!(paths.contains(&"src/components/ui/nested/root.rs"));
    assert!(
        ui_mod
            .content
            .contains("pub use nested::root::NestedButton;")
    );
    assert_eq!(
        lock.files_by_path.get("src/components/ui/nested/root.rs"),
        Some(&"builtin:nested".to_owned())
    );
    assert!(css_operations.is_empty());
}

#[test]
fn add_tokens_updates_only_css_config_and_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");
    let components_mod =
        fs::read_to_string(root.join("src/components/mod.rs")).expect("read components mod");
    let ui_mod = fs::read_to_string(root.join("src/components/ui/mod.rs")).expect("read ui mod");

    let plan = plan_add(root, "tokens").expect("plan tokens");
    let paths = plan
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();

    assert!(paths.contains(&DEFAULT_KIT_CONFIG_PATH));
    assert!(paths.contains(&DEFAULT_KIT_LOCK_PATH));
    assert!(paths.contains(&"styles/kit.css"));
    assert!(!paths.contains(&"src/components/mod.rs"));
    assert!(!paths.contains(&"src/components/ui/mod.rs"));
    assert!(!paths.contains(&"src/components/ui/tokens.rs"));
    assert!(plan.lock.items["builtin:tokens"].files.is_empty());
    assert_eq!(plan.lock.items["builtin:tokens"].style_blocks.len(), 1);

    apply_add(root, "tokens").expect("apply tokens");
    assert_eq!(
        fs::read_to_string(root.join("src/components/mod.rs")).expect("read components mod"),
        components_mod
    );
    assert_eq!(
        fs::read_to_string(root.join("src/components/ui/mod.rs")).expect("read ui mod"),
        ui_mod
    );
    assert!(
        fs::read_to_string(root.join("styles/kit.css"))
            .expect("read css")
            .contains("/* leptos-ui-kit:start tokens */")
    );
    assert!(
        apply_add(root, "tokens")
            .expect("second tokens add")
            .is_empty()
    );
}

#[test]
fn add_write_installs_button_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");

    let plan = apply_add(root, "button").expect("apply add");

    assert!(!plan.is_empty());
    assert!(root.join("src/components/ui/button.rs").is_file());
    assert!(root.join("src/components/ui/spinner.rs").is_file());
    assert!(
        fs::read_to_string(root.join("src/components/ui/mod.rs"))
            .expect("read ui mod")
            .contains("pub use button::{Button, ButtonSize, ButtonType, ButtonVariant};")
    );
    assert!(
        fs::read_to_string(root.join("src/components/ui/mod.rs"))
            .expect("read ui mod")
            .contains("pub use spinner::{Spinner, SpinnerMode};")
    );
    assert!(
        fs::read_to_string(root.join("styles/kit.css"))
            .expect("read css")
            .contains("/* leptos-ui-kit:start button */")
    );
    assert!(
        fs::read_to_string(root.join("styles/kit.css"))
            .expect("read css")
            .contains("/* leptos-ui-kit:start spinner */")
    );
    let lock = parse_install_lock_str_at_path(
        &fs::read_to_string(root.join(DEFAULT_KIT_LOCK_PATH)).expect("read lock"),
        Path::new(DEFAULT_KIT_LOCK_PATH),
    )
    .expect("parse lock");
    assert!(lock.items.contains_key("builtin:tokens"));
    assert!(lock.items.contains_key("builtin:button"));
    assert!(lock.items.contains_key("builtin:spinner"));
    let config = parse_kit_json_str(
        &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
    )
    .expect("parse config");
    assert_eq!(config.items.len(), 3);
    assert!(config.items.iter().any(|item| item.item_name() == "tokens"));
    assert!(config.items.iter().any(|item| item.item_name() == "button"));
    assert!(
        config
            .items
            .iter()
            .any(|item| item.item_name() == "spinner")
    );
    assert_exact_persistent_coordination(root);
}

#[test]
fn sync_plan_installs_declared_button_without_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");
    write_desired_button_config(root);

    let plan = plan_sync(root).expect("plan sync");
    let paths = plan
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();

    assert!(paths.contains(&"src/components/ui/button.rs"));
    assert!(paths.contains(&"src/components/ui/mod.rs"));
    assert!(paths.contains(&"styles/kit.css"));
    assert!(paths.contains(&DEFAULT_KIT_LOCK_PATH));
    assert!(!root.join("src/components/ui/button.rs").exists());
    assert_eq!(
        plan.item_ids,
        vec![
            "builtin:tokens".to_owned(),
            "builtin:spinner".to_owned(),
            "builtin:button".to_owned()
        ]
    );
}

#[test]
fn sync_write_is_idempotent_when_declared_button_is_current() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");
    write_desired_button_config(root);

    let first = apply_sync(root).expect("first sync");
    let second = apply_sync(root).expect("second sync");

    assert!(!first.is_empty());
    assert!(second.is_empty());
    assert!(root.join("src/components/ui/button.rs").is_file());
}

#[test]
fn pinned_theme_migration_fixtures_match_the_compatibility_authority() {
    assert_eq!(PINNED_BUTTON_CSS.len(), 3_721);
    assert_eq!(PINNED_SPINNER_CSS.len(), 1_121);
    assert_eq!(
        hash_bytes(PINNED_BUTTON_CSS.as_bytes()),
        "sha256:b9414172fc55c4d62e8b4ccd21c9c5d6427729e2ed30e2d5e1c5b808945dee46"
    );
    assert_eq!(
        hash_bytes(PINNED_SPINNER_CSS.as_bytes()),
        "sha256:736f9458ba25973db7371e02732ee9f87e02fe7d9e6686e94d76f52cfc26cd6d"
    );
}

#[test]
fn sync_migrates_exact_pinned_button_installs_on_default_and_custom_paths() {
    for (case, css_path, with_app_overrides) in [
        ("default", "styles/kit.css", false),
        ("custom", "styles/custom.css", false),
        ("default-with-overrides", "styles/kit.css", true),
        ("custom-with-overrides", "styles/custom.css", true),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        setup_sync_project(root, css_path);
        apply_add(root, "button").unwrap_or_else(|error| panic!("{case}: install button: {error}"));
        reconstruct_pinned_button_install(root);
        if with_app_overrides {
            append_app_overrides(root, css_path);
        }

        let first = apply_sync(root)
            .unwrap_or_else(|error| panic!("{case}: migrate pinned install: {error}"));

        assert_successful_sync(
            case,
            root,
            css_path,
            &first,
            &["button"],
            with_app_overrides,
        );
    }
}

#[test]
fn sync_relocates_a_current_tracked_foundation_after_multiple_dependents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    setup_sync_project(root, "styles/kit.css");
    apply_add(root, "button").expect("install button");
    move_tokens_after_all_dependents(root, "styles/kit.css");

    let first = apply_sync(root).expect("relocate current tracked tokens");

    assert_successful_sync(
        "tracked-late-tokens",
        root,
        "styles/kit.css",
        &first,
        &["button"],
        false,
    );
}

#[test]
fn sync_inserts_exactly_one_foundation_for_multiple_component_families() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    setup_sync_project(root, "styles/kit.css");
    for item in ["button", "dialog", "tabs"] {
        apply_add(root, item).unwrap_or_else(|error| panic!("install {item}: {error}"));
    }
    remove_tokens_from_install(root);

    let first = apply_sync(root).expect("migrate multiple dependents");

    assert_successful_sync(
        "multiple-dependents",
        root,
        "styles/kit.css",
        &first,
        &["button", "dialog", "tabs"],
        false,
    );
}

#[test]
fn apply_sync_refuses_edited_pinned_blocks_atomically_on_every_css_path() {
    for (case, css_path) in [
        ("default", "styles/kit.css"),
        ("custom", "styles/custom.css"),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        setup_sync_project(root, css_path);
        apply_add(root, "button").unwrap_or_else(|error| panic!("{case}: install button: {error}"));
        reconstruct_pinned_button_install(root);
        let absolute_css_path = root.join(css_path);
        let edited_css = fs::read_to_string(&absolute_css_path)
            .expect("read pinned CSS")
            .replacen("display: inline-flex;", "display: flex;", 1);
        fs::write(&absolute_css_path, edited_css).expect("edit pinned CSS");
        let before = snapshot_project_files(root);

        let error = match apply_sync(root) {
            Ok(_) => panic!("{case}: edited block should conflict"),
            Err(error) => error,
        };

        assert_sync_unsafe_patch_path(case, error, css_path);
        assert_eq!(snapshot_project_files(root), before, "{case}");
        assert_exact_persistent_coordination(root);
    }
}

#[test]
fn apply_sync_refuses_config_lock_stylesheet_disagreement_atomically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    setup_sync_project(root, "styles/kit.css");
    apply_add(root, "button").expect("install button");
    let config_path = root.join(DEFAULT_KIT_CONFIG_PATH);
    let mut config = parse_kit_json_str(
        &fs::read_to_string(&config_path).expect("read config before path drift"),
    )
    .expect("parse config before path drift");
    config.styles.css = "styles/moved.css".to_owned();
    fs::write(
        &config_path,
        kit_config_to_json(&config).expect("serialize path-drifted config"),
    )
    .expect("write path-drifted config");
    let before = snapshot_project_files(root);

    let error = apply_sync(root).expect_err("cross-path state should conflict");

    assert_sync_unsafe_patch_path("cross-path", error, "styles/moved.css");
    assert_eq!(snapshot_project_files(root), before);
    assert!(!root.join("styles/moved.css").exists());
    assert_exact_persistent_coordination(root);
}

#[test]
fn add_write_is_idempotent_when_tracked_files_are_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");

    apply_add(root, "button").expect("first add");
    let second = apply_add(root, "button").expect("second add");

    assert!(second.is_empty());
}

#[test]
fn add_stamps_only_when_it_otherwise_rewrites_config() {
    let rewritten = tempfile::tempdir().expect("tempdir");
    let rewritten_root = rewritten.path();
    setup_empty_project(rewritten_root);
    apply_init(rewritten_root).expect("init rewritten case");
    write_alternate_tool_provenance(rewritten_root, false);

    let error =
        plan_add_with_config_writer(rewritten_root, "button", missing_config_write_provenance)
            .expect_err("unavailable provenance must reject add config rewrite");
    assert_missing_tool_provenance(error);
    let plan = plan_add(rewritten_root, "button").expect("plan add");
    let config = planned_kit_config(&plan.files).expect("planned config rewrite");
    assert_eq!(
        config.tool,
        canonical_tool_config().expect("compiled provenance")
    );

    let unchanged = tempfile::tempdir().expect("tempdir");
    let unchanged_root = unchanged.path();
    setup_empty_project(unchanged_root);
    apply_init(unchanged_root).expect("init unchanged case");
    apply_add(unchanged_root, "button").expect("install button");
    write_alternate_tool_provenance(unchanged_root, false);

    let plan =
        plan_add_with_config_writer(unchanged_root, "button", unexpected_config_write_provenance)
            .expect("plan no-op config add");
    assert!(planned_kit_config(&plan.files).is_none());
}

#[test]
fn sync_stamps_only_when_it_otherwise_rewrites_config() {
    let rewritten = tempfile::tempdir().expect("tempdir");
    let rewritten_root = rewritten.path();
    setup_empty_project(rewritten_root);
    apply_init(rewritten_root).expect("init rewritten case");
    write_alternate_tool_provenance(rewritten_root, true);

    let error = plan_sync_with_config_writer(rewritten_root, missing_config_write_provenance)
        .expect_err("unavailable provenance must reject sync config rewrite");
    assert_missing_tool_provenance(error);
    let plan = plan_sync(rewritten_root).expect("plan closure rewrite");
    let config = planned_kit_config(&plan.files).expect("planned config rewrite");
    assert_eq!(
        config.tool,
        canonical_tool_config().expect("compiled provenance")
    );

    let unchanged = tempfile::tempdir().expect("tempdir");
    let unchanged_root = unchanged.path();
    setup_empty_project(unchanged_root);
    apply_init(unchanged_root).expect("init unchanged case");
    write_alternate_tool_provenance(unchanged_root, false);

    let plan = plan_sync_with_config_writer(unchanged_root, unexpected_config_write_provenance)
        .expect("plan no-op config sync");
    assert!(planned_kit_config(&plan.files).is_none());
}

#[test]
fn add_router_link_records_registry_dependencies_from_metadata() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");

    let plan = apply_add(root, "router-link").expect("add router link");
    let config = parse_kit_json_str(
        &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
    )
    .expect("parse config");
    let item_names = config
        .items
        .iter()
        .map(|item| item.item_name())
        .collect::<Vec<_>>();

    assert_eq!(item_names, ["tokens", "anchor", "router-link"]);
    assert_eq!(plan.item_id, "builtin:router-link");
    assert_eq!(
        plan.lock.items.keys().cloned().collect::<Vec<_>>(),
        vec![
            "builtin:anchor".to_owned(),
            "builtin:router-link".to_owned(),
            "builtin:tokens".to_owned()
        ]
    );
    assert!(root.join("src/components/ui/anchor.rs").is_file());
    assert!(root.join("src/components/ui/router_link.rs").is_file());
    assert!(root.join("styles/kit.css").is_file());
}

#[test]
fn add_plan_rejects_untracked_existing_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");
    fs::write(root.join("src/components/ui/button.rs"), "// local\n").expect("write local");

    let error = plan_add(root, "button").expect_err("untracked target should conflict");

    assert!(matches!(error, CodegenError::UnsafePatch { .. }));
}

#[test]
fn add_plan_rejects_tracked_local_edits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    apply_init(root).expect("init");
    apply_add(root, "button").expect("add");
    fs::write(root.join("src/components/ui/button.rs"), "// edited\n").expect("edit source");

    let error = plan_add(root, "button").expect_err("tracked edit should conflict");

    assert!(matches!(error, CodegenError::UnsafePatch { .. }));
}

fn write_kit_config(root: &Path, config: impl AsRef<[u8]>) {
    let path = root.join(DEFAULT_KIT_CONFIG_PATH);
    fs::create_dir_all(path.parent().expect("kit config parent")).expect("create kit dir");
    fs::write(path, config).expect("write config");
}

fn setup_empty_project(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
}

fn write_alternate_tool_provenance(root: &Path, request_button: bool) {
    let path = root.join(DEFAULT_KIT_CONFIG_PATH);
    let mut config =
        parse_kit_json_str(&fs::read_to_string(&path).expect("read config")).expect("parse config");
    if request_button {
        config = kit_config_with_desired_item(config, desired_builtin_button_item())
            .expect("request button");
    }
    let compiled = canonical_tool_config().expect("compiled provenance");
    let ToolSourceConfig::Git { rev, .. } = compiled.source;
    let alternate = if rev == "0000000000000000000000000000000000000000" {
        "1111111111111111111111111111111111111111"
    } else {
        "0000000000000000000000000000000000000000"
    };
    config.tool.source = ToolSourceConfig::Git {
        url: TOOL_GIT_URL.to_owned(),
        rev: alternate.to_owned(),
    };
    write_kit_config(root, kit_config_to_json(&config).expect("serialize config"));
}

fn planned_kit_config(files: &[PlannedFile]) -> Option<KitConfig> {
    files
        .iter()
        .find(|file| file.path == DEFAULT_KIT_CONFIG_PATH)
        .map(|file| parse_kit_json_str(&file.content).expect("parse planned config"))
}

fn missing_config_write_provenance(_config: KitConfig) -> Result<KitConfig, ConfigError> {
    Err(ConfigError::MissingToolProvenance {
        package: leptos_ui_kit_registry::TOOL_PACKAGE,
        binary: leptos_ui_kit_registry::TOOL_BINARY,
    })
}

fn unexpected_config_write_provenance(_config: KitConfig) -> Result<KitConfig, ConfigError> {
    panic!("unchanged config must not request compiled provenance")
}

fn assert_missing_tool_provenance(error: CodegenError) {
    assert!(matches!(
        error,
        CodegenError::Config(ConfigError::MissingToolProvenance {
            package: leptos_ui_kit_registry::TOOL_PACKAGE,
            binary: leptos_ui_kit_registry::TOOL_BINARY,
        })
    ));
}

fn write_desired_button_config(root: &Path) {
    let config = parse_kit_json_str(
        &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
    )
    .expect("parse config");
    let config = kit_config_with_desired_item(config, desired_builtin_button_item())
        .expect("add desired item");
    write_kit_config(root, kit_config_to_json(&config).expect("serialize config"));
}

fn setup_sync_project(root: &Path, css_path: &str) {
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
    if css_path != "styles/kit.css" {
        let config = canonical_kit_json().expect("canonical config").replace(
            "\"css\": \"styles/kit.css\"",
            &format!("\"css\": \"{css_path}\""),
        );
        write_kit_config(root, config);
    }
    apply_init(root).expect("init project");
}

fn reconstruct_pinned_button_install(root: &Path) {
    remove_tokens_from_install(root);
    replace_css_block_and_track_baseline(root, "button", PINNED_BUTTON_CSS);
    replace_css_block_and_track_baseline(root, "spinner", PINNED_SPINNER_CSS);
    restore_pinned_button_before_spinner_order(root);
}

fn restore_pinned_button_before_spinner_order(root: &Path) {
    let config = parse_kit_json_str(
        &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
    )
    .expect("parse config");
    let css_path = root.join(&config.styles.css);
    let css = fs::read_to_string(&css_path).expect("read CSS");
    let button = extract_managed_css_block_at_path(&css, &config.styles.css, "button")
        .expect("extract button")
        .expect("button block");
    let spinner = extract_managed_css_block_at_path(&css, &config.styles.css, "spinner")
        .expect("extract spinner")
        .expect("spinner block");
    let earliest = css
        .find(&button)
        .expect("button range")
        .min(css.find(&spinner).expect("spinner range"));
    let without_blocks = css.replacen(&button, "", 1).replacen(&spinner, "", 1);
    let mut legacy = String::with_capacity(css.len());
    legacy.push_str(&without_blocks[..earliest]);
    legacy.push_str(&button);
    legacy.push_str(&spinner);
    legacy.push_str(&without_blocks[earliest..]);
    fs::write(css_path, legacy).expect("restore legacy dependency order");
}

fn remove_tokens_from_install(root: &Path) {
    let config_path = root.join(DEFAULT_KIT_CONFIG_PATH);
    let mut config = parse_kit_json_str(&fs::read_to_string(&config_path).expect("read config"))
        .expect("parse config");
    config.items.retain(|item| item.item_name() != "tokens");
    let config_content = kit_config_to_json(&config).expect("serialize config");
    fs::write(&config_path, &config_content).expect("write legacy config");

    let css_path = root.join(&config.styles.css);
    let css = fs::read_to_string(&css_path).expect("read CSS");
    let tokens = extract_managed_css_block_at_path(&css, &config.styles.css, "tokens")
        .expect("extract tokens")
        .expect("tokens block");
    fs::write(&css_path, css.replacen(&tokens, "", 1)).expect("remove tokens CSS");

    let lock_path = root.join(DEFAULT_KIT_LOCK_PATH);
    let mut lock = parse_install_lock_str_at_path(
        &fs::read_to_string(&lock_path).expect("read lock"),
        Path::new(DEFAULT_KIT_LOCK_PATH),
    )
    .expect("parse lock");
    lock.items.remove("builtin:tokens");
    lock.style_blocks_by_id.remove("tokens");
    lock.project.config_hash = hash_bytes(config_content.as_bytes());
    fs::write(
        &lock_path,
        lock_to_json(&lock).expect("serialize legacy lock"),
    )
    .expect("write legacy lock");
}

fn replace_css_block_and_track_baseline(root: &Path, block_id: &str, replacement: &str) {
    let config = parse_kit_json_str(
        &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
    )
    .expect("parse config");
    let css_path = root.join(&config.styles.css);
    let css = fs::read_to_string(&css_path).expect("read CSS");
    let current = extract_managed_css_block_at_path(&css, &config.styles.css, block_id)
        .expect("extract managed block")
        .expect("managed block");
    fs::write(&css_path, css.replacen(&current, replacement, 1)).expect("write pinned CSS");

    let lock_path = root.join(DEFAULT_KIT_LOCK_PATH);
    let mut lock = parse_install_lock_str_at_path(
        &fs::read_to_string(&lock_path).expect("read lock"),
        Path::new(DEFAULT_KIT_LOCK_PATH),
    )
    .expect("parse lock");
    let item_id = lock
        .style_blocks_by_id
        .get(block_id)
        .expect("style block owner")
        .clone();
    let block = lock
        .items
        .get_mut(&item_id)
        .expect("installed item")
        .style_blocks
        .iter_mut()
        .find(|block| block.block_id == block_id)
        .expect("installed style block");
    block.generated_hash = hash_bytes(replacement.as_bytes());
    fs::write(
        &lock_path,
        lock_to_json(&lock).expect("serialize legacy lock"),
    )
    .expect("write legacy lock");
}

fn append_app_overrides(root: &Path, css_path: &str) {
    let mut css = fs::read_to_string(root.join(css_path)).expect("read CSS before override");
    css.push_str(APP_OVERRIDE_CSS);
    fs::write(root.join(css_path), css).expect("write application overrides");
}

fn move_tokens_after_all_dependents(root: &Path, css_path: &str) {
    let absolute_path = root.join(css_path);
    let css = fs::read_to_string(&absolute_path).expect("read CSS before relocation setup");
    let tokens = extract_managed_css_block_at_path(&css, css_path, "tokens")
        .expect("extract tokens")
        .expect("tokens block");
    let mut late = css.replacen(&tokens, "", 1);
    if !late.ends_with('\n') {
        late.push('\n');
    }
    late.push('\n');
    late.push_str(&tokens);
    fs::write(absolute_path, late).expect("place tokens after dependents");
}

fn assert_successful_sync(
    case: &str,
    root: &Path,
    css_path: &str,
    first: &SyncPlan,
    requested_items: &[&str],
    with_app_overrides: bool,
) {
    assert!(!first.is_empty(), "{case}: first sync must write");
    let css_files = first
        .files
        .iter()
        .filter(|file| file.path == css_path)
        .collect::<Vec<_>>();
    assert_eq!(css_files.len(), 1, "{case}: one stylesheet file plan");
    assert_eq!(
        css_files[0].action,
        PlannedFileAction::Update,
        "{case}: stylesheet update action"
    );
    let css_changes = first
        .changes
        .iter()
        .filter(|change| change.path == css_path)
        .collect::<Vec<_>>();
    assert_eq!(css_changes.len(), 1, "{case}: one stylesheet change");
    assert_eq!(css_changes[0].kind, ChangeKind::UpdateCssBlock, "{case}");
    assert_eq!(css_changes[0].item, None, "{case}: batch CSS ownership");

    let requested_names = requested_items
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let resolved = resolve_built_in_registry_items(&requested_names)
        .unwrap_or_else(|error| panic!("{case}: resolve expected closure: {error}"));
    let expected_desired = resolved
        .iter()
        .map(|item| desired_builtin_item(&item.item.name))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("{case}: build expected desired items: {error}"));
    let config_content = fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH))
        .unwrap_or_else(|error| panic!("{case}: read config: {error}"));
    let config = parse_kit_json_str(&config_content)
        .unwrap_or_else(|error| panic!("{case}: parse config: {error}"));
    assert_eq!(config.items, expected_desired, "{case}: canonical closure");
    assert_eq!(config.styles.css, css_path, "{case}: configured CSS path");
    assert_eq!(
        config_content,
        kit_config_to_json(&config).expect("serialize canonical config"),
        "{case}: canonical config bytes"
    );

    let css = fs::read_to_string(root.join(css_path))
        .unwrap_or_else(|error| panic!("{case}: read migrated CSS: {error}"));
    let ranges = inspect_managed_css_blocks_at_path(&css, css_path)
        .unwrap_or_else(|error| panic!("{case}: inspect CSS: {error}"));
    let expected_block_ids = resolved
        .iter()
        .flat_map(|item| item.targets.style_blocks.iter())
        .map(|style| style.id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ranges.keys().cloned().collect::<BTreeSet<_>>(),
        expected_block_ids,
        "{case}: exact managed block set"
    );
    assert_eq!(
        css.matches("/* leptos-ui-kit:start tokens */").count(),
        1,
        "{case}: exactly one foundation"
    );

    for item in &resolved {
        for style in &item.targets.style_blocks {
            let expected = read_built_in_registry_source(&style.source)
                .unwrap_or_else(|error| panic!("{case}: read {}: {error}", style.source));
            let actual = extract_managed_css_block_at_path(&css, css_path, &style.id)
                .unwrap_or_else(|error| panic!("{case}: extract {}: {error}", style.id))
                .unwrap_or_else(|| panic!("{case}: missing block {}", style.id));
            assert_eq!(actual, expected, "{case}: current block {}", style.id);
        }
    }
    assert_style_dependency_order(case, &resolved, &ranges);

    if with_app_overrides {
        assert_eq!(css.matches(APP_OVERRIDE_CSS).count(), 1, "{case}");
        let override_start = css
            .find(APP_OVERRIDE_CSS)
            .expect("application override block");
        assert!(
            ranges.values().all(|range| range.end <= override_start),
            "{case}: application overrides remain after all generated defaults"
        );
    } else {
        assert!(!css.contains(APP_OVERRIDE_CSS), "{case}");
    }

    let lock_content = fs::read_to_string(root.join(DEFAULT_KIT_LOCK_PATH))
        .unwrap_or_else(|error| panic!("{case}: read lock: {error}"));
    let lock = parse_install_lock_str_at_path(&lock_content, Path::new(DEFAULT_KIT_LOCK_PATH))
        .unwrap_or_else(|error| panic!("{case}: parse lock: {error}"));
    assert_eq!(lock, first.lock, "{case}: applied lock matches sync result");
    assert_eq!(
        lock_content,
        lock_to_json(&first.lock).expect("serialize applied lock"),
        "{case}: canonical lock bytes"
    );
    assert_eq!(
        lock,
        expected_install_lock(&config_content, css_path, &resolved),
        "{case}: complete registry-derived lock"
    );

    let second =
        apply_sync(root).unwrap_or_else(|error| panic!("{case}: second sync failed: {error}"));
    assert!(second.is_empty(), "{case}: second sync must be empty");
    assert!(second.files.is_empty(), "{case}: no second-sync files");
    assert!(second.changes.is_empty(), "{case}: no second-sync changes");
    assert_exact_persistent_coordination(root);
}

fn assert_style_dependency_order(
    case: &str,
    resolved: &[leptos_ui_kit_registry::ResolvedRegistryItem],
    ranges: &BTreeMap<String, ManagedCssBlockRange>,
) {
    let by_name = resolved
        .iter()
        .map(|item| (item.item.name.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    for dependent in resolved {
        for dependency_name in &dependent.item.registry_dependencies {
            let dependency = by_name[dependency_name.as_str()];
            for dependency_style in &dependency.targets.style_blocks {
                for dependent_style in &dependent.targets.style_blocks {
                    assert!(
                        ranges[&dependency_style.id].end <= ranges[&dependent_style.id].start,
                        "{case}: dependency {} must precede {}",
                        dependency_style.id,
                        dependent_style.id
                    );
                }
            }
        }
    }
}

fn expected_install_lock(
    config_content: &str,
    css_path: &str,
    resolved: &[leptos_ui_kit_registry::ResolvedRegistryItem],
) -> InstallLock {
    let mut expected = InstallLock::empty(hash_bytes(config_content.as_bytes()));
    for item in resolved {
        let item_id = built_in_item_id(&item.item.name);
        let files = item
            .targets
            .ui_files
            .iter()
            .map(|file| {
                let path = format!("src/components/ui/{}", file.path);
                let source =
                    read_built_in_registry_source(&file.source).expect("read expected Rust source");
                let generated_hash = hash_bytes(source.as_bytes());
                expected.files_by_path.insert(path.clone(), item_id.clone());
                InstalledFile {
                    path,
                    kind: "rust".to_owned(),
                    generated_hash: generated_hash.clone(),
                    local_hash_at_install: generated_hash,
                }
            })
            .collect::<Vec<_>>();
        let style_blocks = item
            .targets
            .style_blocks
            .iter()
            .map(|style| {
                let source =
                    read_built_in_registry_source(&style.source).expect("read expected CSS source");
                expected
                    .style_blocks_by_id
                    .insert(style.id.clone(), item_id.clone());
                InstalledStyleBlock {
                    css_path: css_path.to_owned(),
                    block_id: style.id.clone(),
                    generated_hash: hash_bytes(source.as_bytes()),
                }
            })
            .collect::<Vec<_>>();
        expected.items.insert(
            item_id.clone(),
            InstalledItem {
                id: item_id,
                name: item.item.name.clone(),
                source: "builtin".to_owned(),
                version: item.item.version.clone(),
                content_hash: item.content_hash.clone(),
                files,
                style_blocks,
            },
        );
    }
    expected
}

fn snapshot_project_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).expect("read snapshot directory") {
            let entry = entry.expect("read snapshot entry");
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, snapshot);
            } else if path.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .expect("snapshot path below project root")
                    .to_path_buf();
                snapshot.insert(relative, fs::read(path).expect("read snapshot file"));
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

fn assert_sync_unsafe_patch_path(case: &str, error: CodegenError, expected_path: &str) {
    match error {
        CodegenError::UnsafePatch { path, .. } => {
            assert_eq!(path, PathBuf::from(expected_path), "{case}")
        }
        other => panic!("{case}: expected unsafe patch, got {other}"),
    }
}

const PINNED_BUTTON_CSS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/theme_pre_refactor_06124efa/button.css"
));
const PINNED_SPINNER_CSS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/theme_pre_refactor_06124efa/spinner.css"
));

#[test]
fn packaged_css_fixtures_match_workspace_canonical_copies_when_present() {
    let canonical = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/theme_pre_refactor_06124efa");
    if !canonical
        .try_exists()
        .expect("inspect canonical fixture root")
    {
        return;
    }
    assert!(
        canonical.is_dir(),
        "canonical fixture root must be a directory"
    );
    assert_eq!(
        PINNED_BUTTON_CSS,
        fs::read_to_string(canonical.join("button.css")).expect("read canonical button CSS")
    );
    assert_eq!(
        PINNED_SPINNER_CSS,
        fs::read_to_string(canonical.join("spinner.css")).expect("read canonical spinner CSS")
    );
}

const APP_OVERRIDE_CSS: &str = r#"
/* application-owned theme overrides */
:root {
  --kit-color-primary: rebeccapurple;
  --kit-button-gap: 0.75rem;
}
"#;

fn managed_css_block(block_id: &str, declaration: &str) -> String {
    format!(
        "/* leptos-ui-kit:start {block_id} */\n.{block_id} {{ {declaration} }}\n/* leptos-ui-kit:end {block_id} */\n"
    )
}

fn managed_css_operation(
    block_id: &str,
    role: ManagedCssBlockRole,
    declaration: &str,
) -> ManagedCssOperation {
    ManagedCssOperation {
        item_id: format!("builtin:{block_id}"),
        block_id: block_id.to_owned(),
        role,
        generated: managed_css_block(block_id, declaration),
    }
}

fn managed_css_dependency(dependency: &str, dependent: &str) -> ManagedCssDependency {
    ManagedCssDependency {
        dependency_block_id: dependency.to_owned(),
        dependent_block_id: dependent.to_owned(),
    }
}

fn tracked_css_lock(css_path: &str, blocks: &[(&ManagedCssOperation, &str)]) -> InstallLock {
    let mut lock = InstallLock::empty(hash_bytes(b"config"));
    for (operation, baseline) in blocks {
        lock.items.insert(
            operation.item_id.clone(),
            InstalledItem {
                id: operation.item_id.clone(),
                name: operation.block_id.clone(),
                source: "builtin".to_owned(),
                version: SCHEMA_VERSION.to_owned(),
                content_hash: hash_bytes(operation.item_id.as_bytes()),
                files: Vec::new(),
                style_blocks: vec![InstalledStyleBlock {
                    css_path: css_path.to_owned(),
                    block_id: operation.block_id.clone(),
                    generated_hash: hash_bytes(baseline.as_bytes()),
                }],
            },
        );
        lock.style_blocks_by_id
            .insert(operation.block_id.clone(), operation.item_id.clone());
    }
    lock
}

fn unmanaged_css(existing: &str, logical_path: &str) -> String {
    let mut ranges = inspect_managed_css_blocks_at_path(existing, logical_path)
        .expect("inspect managed CSS")
        .into_values()
        .collect::<Vec<_>>();
    ranges.sort_by_key(|range| range.start);

    let mut output = String::new();
    let mut cursor = 0;
    for range in ranges {
        output.push_str(&existing[cursor..range.start]);
        cursor = range.end;
    }
    output.push_str(&existing[cursor..]);
    output
}

fn assert_unsafe_patch_path(error: CodegenError, expected_path: &str) {
    assert!(
        matches!(error, CodegenError::UnsafePatch { ref path, .. } if path == &PathBuf::from(expected_path)),
        "unexpected error: {error}"
    );
}

#[test]
fn css_batch_inserts_missing_foundation_before_earliest_dependent() {
    let tokens = managed_css_operation("tokens", ManagedCssBlockRole::Foundation, "color: black;");
    let spinner = managed_css_operation(
        "spinner",
        ManagedCssBlockRole::Component,
        "color: currentColor;",
    );
    let button = managed_css_operation(
        "button",
        ManagedCssBlockRole::Component,
        "display: inline-flex;",
    );
    let existing = format!(
        "/* application header */\n{}\n/* between generated blocks */\n{}\n:root {{ --kit-color-primary: rebeccapurple; }}\n",
        spinner.generated, button.generated
    );
    let lock = tracked_css_lock(
        "styles/custom.css",
        &[(&spinner, &spinner.generated), (&button, &button.generated)],
    );
    let dependencies = [
        managed_css_dependency("tokens", "spinner"),
        managed_css_dependency("tokens", "button"),
        managed_css_dependency("spinner", "button"),
    ];

    let reconciled = reconcile_managed_css_blocks_at_path(
        &existing,
        "styles/custom.css",
        &lock,
        &[tokens, spinner, button],
        &dependencies,
    )
    .expect("reconcile CSS");

    let ranges = inspect_managed_css_blocks_at_path(&reconciled, "styles/custom.css")
        .expect("inspect reconciled CSS");
    assert!(ranges["tokens"].start < ranges["spinner"].start);
    assert!(ranges["spinner"].start < ranges["button"].start);
    assert_eq!(reconciled.matches("leptos-ui-kit:start tokens").count(), 1);
    assert_eq!(
        unmanaged_css(&reconciled, "styles/custom.css"),
        unmanaged_css(&existing, "styles/custom.css")
    );
    assert!(
        reconciled
            .find("leptos-ui-kit:start tokens")
            .expect("tokens")
            < reconciled
                .find("--kit-color-primary: rebeccapurple")
                .expect("application override")
    );
}

#[test]
fn css_batch_relocates_safe_late_foundation_and_is_idempotent() {
    let tokens = managed_css_operation("tokens", ManagedCssBlockRole::Foundation, "color: black;");
    let spinner = managed_css_operation(
        "spinner",
        ManagedCssBlockRole::Component,
        "color: currentColor;",
    );
    let button = managed_css_operation(
        "button",
        ManagedCssBlockRole::Component,
        "display: inline-flex;",
    );
    let existing = format!(
        "{}/* first gap */\n{}/* application override */\n:root {{ --kit-button-gap: 0.75rem; }}\n{}",
        spinner.generated, button.generated, tokens.generated
    );
    let lock = tracked_css_lock(
        "styles/kit.css",
        &[
            (&tokens, &tokens.generated),
            (&spinner, &spinner.generated),
            (&button, &button.generated),
        ],
    );
    let operations = [tokens, spinner, button];
    let dependencies = [
        managed_css_dependency("tokens", "spinner"),
        managed_css_dependency("tokens", "button"),
        managed_css_dependency("spinner", "button"),
    ];

    let first = reconcile_managed_css_blocks_at_path(
        &existing,
        "styles/kit.css",
        &lock,
        &operations,
        &dependencies,
    )
    .expect("relocate foundation");
    let second = reconcile_managed_css_blocks_at_path(
        &first,
        "styles/kit.css",
        &lock,
        &operations,
        &dependencies,
    )
    .expect("idempotent reconciliation");
    let ranges = inspect_managed_css_blocks_at_path(&first, "styles/kit.css")
        .expect("inspect reconciled CSS");

    assert!(ranges["tokens"].start < ranges["spinner"].start);
    assert!(ranges["spinner"].start < ranges["button"].start);
    assert_eq!(first, second);
    assert_eq!(
        unmanaged_css(&first, "styles/kit.css"),
        unmanaged_css(&existing, "styles/kit.css")
    );
    assert!(
        first.find("leptos-ui-kit:start tokens").expect("tokens")
            < first.find("--kit-button-gap: 0.75rem").expect("override")
    );
}

#[test]
fn css_batch_relocates_foundation_that_matches_verified_old_baseline() {
    let old_tokens = managed_css_block("tokens", "color: gray;");
    let tokens = managed_css_operation("tokens", ManagedCssBlockRole::Foundation, "color: black;");
    let button = managed_css_operation(
        "button",
        ManagedCssBlockRole::Component,
        "display: inline-flex;",
    );
    let existing = format!("{}/* app */\n{}", button.generated, old_tokens);
    let lock = tracked_css_lock(
        "styles/kit.css",
        &[(&tokens, &old_tokens), (&button, &button.generated)],
    );

    let reconciled = reconcile_managed_css_blocks_at_path(
        &existing,
        "styles/kit.css",
        &lock,
        &[tokens.clone(), button],
        &[managed_css_dependency("tokens", "button")],
    )
    .expect("replace and relocate tracked baseline");

    assert!(reconciled.starts_with(&tokens.generated));
    assert!(!reconciled.contains("color: gray"));
    assert_eq!(
        unmanaged_css(&reconciled, "styles/kit.css"),
        unmanaged_css(&existing, "styles/kit.css")
    );
}

#[test]
fn css_batch_places_foundation_after_bounded_legal_preamble() {
    let tokens = managed_css_operation("tokens", ManagedCssBlockRole::Foundation, "color: black;");
    let preamble = "\u{feff} \n/* legal header */\n@CHARSET \"UTF-8\";\n@ImPoRt url(\"theme;a.css\") screen and (feature: \"a;b\");\n@import \"theme.css\" screen\\;print;\n@NaMeSpAcE svg url(data:image/svg+xml;utf8,<svg/>);\n\n";
    let application = "body { color: rebeccapurple; }\n";
    let existing = format!("{preamble}{application}");

    let reconciled = reconcile_managed_css_blocks_at_path(
        &existing,
        "styles/custom.css",
        &InstallLock::empty(hash_bytes(b"config")),
        std::slice::from_ref(&tokens),
        &[],
    )
    .expect("insert after preamble");

    assert!(reconciled.starts_with(preamble));
    assert_eq!(
        &reconciled[preamble.len()..preamble.len() + tokens.generated.len()],
        tokens.generated
    );
    assert!(reconciled.ends_with(application));
    assert_eq!(unmanaged_css(&reconciled, "styles/custom.css"), existing);
}

#[test]
fn css_batch_relocates_tracked_foundation_without_dependent_to_preamble_boundary() {
    let tokens = managed_css_operation("tokens", ManagedCssBlockRole::Foundation, "color: black;");
    let preamble = "\u{feff}/* license */\n@import url(\"base.css\");\n";
    let application = "body { color: rebeccapurple; }\n";
    let existing = format!("{preamble}{application}{}", tokens.generated);
    let lock = tracked_css_lock("styles/custom.css", &[(&tokens, &tokens.generated)]);

    let first = reconcile_managed_css_blocks_at_path(
        &existing,
        "styles/custom.css",
        &lock,
        std::slice::from_ref(&tokens),
        &[],
    )
    .expect("relocate foundation before ordinary CSS");
    let second = reconcile_managed_css_blocks_at_path(
        &first,
        "styles/custom.css",
        &lock,
        std::slice::from_ref(&tokens),
        &[],
    )
    .expect("idempotent no-dependent reconciliation");

    assert!(first.starts_with(&format!("{preamble}{}", tokens.generated)));
    assert!(first.ends_with(application));
    assert_eq!(first, second);
    assert_eq!(
        unmanaged_css(&first, "styles/custom.css"),
        unmanaged_css(&existing, "styles/custom.css")
    );
}

#[test]
fn css_batch_stops_preamble_before_ordinary_rules_and_other_at_rules() {
    let tokens = managed_css_operation("tokens", ManagedCssBlockRole::Foundation, "color: black;");
    for existing in [
        "body { color: red; }\n",
        "@media (prefers-color-scheme: dark) { body {} }\n",
    ] {
        let reconciled = reconcile_managed_css_blocks_at_path(
            existing,
            "styles/kit.css",
            &InstallLock::empty(hash_bytes(b"config")),
            std::slice::from_ref(&tokens),
            &[],
        )
        .expect("insert before ordinary CSS");

        assert!(reconciled.starts_with(&tokens.generated));
        assert!(reconciled.ends_with(existing));
    }
}

#[test]
fn css_batch_rejects_malformed_legal_preambles_at_configured_path() {
    let tokens = managed_css_operation("tokens", ManagedCssBlockRole::Foundation, "color: black;");
    for existing in [
        "/* unterminated",
        "@import \"unterminated;",
        "@namespace url(theme.css",
        "@charset \"UTF-8\"",
        "@import url(theme.css) \\",
        "@import url(theme.css));",
    ] {
        let error = reconcile_managed_css_blocks_at_path(
            existing,
            "styles/custom.css",
            &InstallLock::empty(hash_bytes(b"config")),
            std::slice::from_ref(&tokens),
            &[],
        )
        .expect_err("malformed preamble should fail");

        assert_unsafe_patch_path(error, "styles/custom.css");
    }
}

#[test]
fn css_batch_reorders_verified_non_foundation_dependency_inversions() {
    let spinner = managed_css_operation(
        "spinner",
        ManagedCssBlockRole::Component,
        "color: currentColor;",
    );
    let button = managed_css_operation(
        "button",
        ManagedCssBlockRole::Component,
        "display: inline-flex;",
    );
    let lock = tracked_css_lock(
        "styles/kit.css",
        &[(&spinner, &spinner.generated), (&button, &button.generated)],
    );
    let existing = format!(
        "{}/* application-owned gap */\n{}:root {{ --kit-button-gap: 0.75rem; }}\n",
        button.generated, spinner.generated
    );

    let reconciled = reconcile_managed_css_blocks_at_path(
        &existing,
        "styles/kit.css",
        &lock,
        &[spinner, button],
        &[managed_css_dependency("spinner", "button")],
    )
    .expect("inverted verified dependency should be reordered");
    let ranges = inspect_managed_css_blocks_at_path(&reconciled, "styles/kit.css")
        .expect("inspect reordered CSS");

    assert!(ranges["spinner"].start < ranges["button"].start);
    assert_eq!(
        unmanaged_css(&reconciled, "styles/kit.css"),
        unmanaged_css(&existing, "styles/kit.css")
    );
    assert!(
        ranges["button"].end
            < reconciled
                .find("--kit-button-gap: 0.75rem")
                .expect("application override")
    );
}

#[test]
fn css_batch_prevalidates_duplicate_and_unknown_operations_and_dependencies() {
    let tokens = managed_css_operation("tokens", ManagedCssBlockRole::Foundation, "color: black;");
    let button = managed_css_operation(
        "button",
        ManagedCssBlockRole::Component,
        "display: inline-flex;",
    );
    let empty_lock = InstallLock::empty(hash_bytes(b"config"));

    for (operations, dependencies) in [
        (vec![tokens.clone(), tokens.clone()], Vec::new()),
        (
            vec![tokens.clone()],
            vec![managed_css_dependency("tokens", "missing")],
        ),
        (
            vec![tokens.clone()],
            vec![managed_css_dependency("tokens", "tokens")],
        ),
        (
            vec![tokens.clone(), button],
            vec![
                managed_css_dependency("tokens", "button"),
                managed_css_dependency("tokens", "button"),
            ],
        ),
    ] {
        let error = reconcile_managed_css_blocks_at_path(
            "",
            "styles/custom.css",
            &empty_lock,
            &operations,
            &dependencies,
        )
        .expect_err("invalid batch metadata should fail before output");
        assert_unsafe_patch_path(error, "styles/custom.css");
    }
}

#[test]
fn css_batch_inserts_missing_dependency_before_existing_dependent() {
    let spinner = managed_css_operation(
        "spinner",
        ManagedCssBlockRole::Component,
        "color: currentColor;",
    );
    let button = managed_css_operation(
        "button",
        ManagedCssBlockRole::Component,
        "display: inline-flex;",
    );
    let lock = tracked_css_lock("styles/kit.css", &[(&button, &button.generated)]);
    let existing = button.generated.clone();

    let reconciled = reconcile_managed_css_blocks_at_path(
        &existing,
        "styles/kit.css",
        &lock,
        &[spinner, button],
        &[managed_css_dependency("spinner", "button")],
    )
    .expect("missing dependency should be inserted before dependent");
    let ranges = inspect_managed_css_blocks_at_path(&reconciled, "styles/kit.css")
        .expect("inspect migrated CSS");

    assert!(ranges["spinner"].start < ranges["button"].start);
}

#[test]
fn css_batch_allows_independent_existing_block_order() {
    let alpha = managed_css_operation("alpha", ManagedCssBlockRole::Component, "color: red;");
    let beta = managed_css_operation("beta", ManagedCssBlockRole::Component, "color: blue;");
    let existing = format!("{}{}", beta.generated, alpha.generated);
    let lock = tracked_css_lock(
        "styles/kit.css",
        &[(&alpha, &alpha.generated), (&beta, &beta.generated)],
    );

    let reconciled = reconcile_managed_css_blocks_at_path(
        &existing,
        "styles/kit.css",
        &lock,
        &[alpha, beta],
        &[],
    )
    .expect("independent order should remain valid");

    assert_eq!(reconciled, existing);
}

#[test]
fn css_batch_rejects_malformed_or_overlapping_marker_ranges() {
    let cases = [
        "/* leptos-ui-kit:start alpha */\n/* leptos-ui-kit:start beta */\n/* leptos-ui-kit:end beta */\n/* leptos-ui-kit:end alpha */\n",
        "/* leptos-ui-kit:start alpha */\n/* leptos-ui-kit:end beta */\n/* leptos-ui-kit:end alpha */\n",
        "/* leptos-ui-kit:start alpha */\n",
        "/* leptos-ui-kit:end alpha */\n",
        "/* leptos-ui-kit:start alpha*/\n/* leptos-ui-kit:end alpha */\n",
        "/* leptos-ui-kit:unknown alpha */\n",
        "/* leptos-ui-kit:start alpha */\n/* leptos-ui-kit:end alpha */\n/* leptos-ui-kit:start alpha */\n/* leptos-ui-kit:end alpha */\n",
    ];

    for existing in cases {
        let error = inspect_managed_css_blocks_at_path(existing, "styles/custom.css")
            .expect_err("malformed markers should fail");
        assert_unsafe_patch_path(error, "styles/custom.css");
    }
}

#[test]
fn css_batch_rejects_untracked_misowned_missing_and_edited_blocks() {
    let button = managed_css_operation(
        "button",
        ManagedCssBlockRole::Component,
        "display: inline-flex;",
    );

    let error = reconcile_managed_css_blocks_at_path(
        &button.generated,
        "styles/custom.css",
        &InstallLock::empty(hash_bytes(b"config")),
        std::slice::from_ref(&button),
        &[],
    )
    .expect_err("untracked exact block should fail");
    assert_unsafe_patch_path(error, "styles/custom.css");

    let mut misowned = tracked_css_lock("styles/custom.css", &[(&button, &button.generated)]);
    misowned
        .style_blocks_by_id
        .insert("button".to_owned(), "builtin:someone-else".to_owned());
    let error = reconcile_managed_css_blocks_at_path(
        &button.generated,
        "styles/custom.css",
        &misowned,
        std::slice::from_ref(&button),
        &[],
    )
    .expect_err("misowned block should fail");
    assert_unsafe_patch_path(error, "styles/custom.css");

    let missing = tracked_css_lock("styles/custom.css", &[(&button, &button.generated)]);
    let error = reconcile_managed_css_blocks_at_path(
        "",
        "styles/custom.css",
        &missing,
        std::slice::from_ref(&button),
        &[],
    )
    .expect_err("missing tracked block should fail");
    assert_unsafe_patch_path(error, "styles/custom.css");

    let old_button = managed_css_block("button", "display: block;");
    let edited_button = managed_css_block("button", "display: grid;");
    let edited_lock = tracked_css_lock("styles/custom.css", &[(&button, &old_button)]);
    let error = reconcile_managed_css_blocks_at_path(
        &edited_button,
        "styles/custom.css",
        &edited_lock,
        &[button],
        &[],
    )
    .expect_err("edited tracked block should fail");
    assert_unsafe_patch_path(error, "styles/custom.css");
}

#[test]
fn css_batch_rejects_config_and_lock_stylesheet_path_mismatch() {
    let button = managed_css_operation(
        "button",
        ManagedCssBlockRole::Component,
        "display: inline-flex;",
    );
    let lock = tracked_css_lock("styles/kit.css", &[(&button, &button.generated)]);
    let existing = button.generated.clone();

    let error =
        reconcile_managed_css_blocks_at_path(&existing, "styles/custom.css", &lock, &[button], &[])
            .expect_err("cross-path reconciliation should fail");

    assert_unsafe_patch_path(error, "styles/custom.css");
}

#[test]
fn path_aware_css_helpers_report_configured_logical_path() {
    let previous = managed_css_block("button", "color: red;");
    let edited = managed_css_block("button", "color: green;");
    let next = managed_css_block("button", "color: blue;");
    let error = patch_css_block_at_path(
        &edited,
        "styles/custom.css",
        "button",
        &next,
        Some(&hash_bytes(previous.as_bytes())),
    )
    .expect_err("edited block should fail");
    assert_unsafe_patch_path(error, "styles/custom.css");

    let error = extract_managed_css_block_at_path(
        "/* leptos-ui-kit:start button */\n",
        "styles/custom.css",
        "button",
    )
    .expect_err("missing end marker should fail");
    assert_unsafe_patch_path(error, "styles/custom.css");
}

#[test]
fn css_patcher_appends_managed_block() {
    let existing = ":root {\n  color-scheme: light;\n}\n";
    let block =
        "/* leptos-ui-kit:start button */\n.kit-button {}\n/* leptos-ui-kit:end button */\n";

    let patched = patch_css_block(existing, "button", block, None).expect("patch css");

    assert!(patched.starts_with(existing));
    assert!(patched.contains("/* leptos-ui-kit:start button */"));
    assert!(patched.contains(".kit-button {}"));
    assert!(patched.ends_with("/* leptos-ui-kit:end button */\n"));
}

#[test]
fn css_patcher_is_idempotent_for_existing_matching_block() {
    let block =
        "/* leptos-ui-kit:start button */\n.kit-button {}\n/* leptos-ui-kit:end button */\n";

    let patched = patch_css_block(block, "button", block, None).expect("patch css");

    assert_eq!(patched, block);
}

#[test]
fn css_patcher_replaces_tracked_generated_block() {
    let previous = "/* leptos-ui-kit:start button */\n.kit-button { color: red; }\n/* leptos-ui-kit:end button */\n";
    let next = "/* leptos-ui-kit:start button */\n.kit-button { color: blue; }\n/* leptos-ui-kit:end button */\n";
    let existing = format!("/* app */\n{previous}.other {{}}\n");

    let previous_hash = hash_bytes(previous.as_bytes());
    let patched =
        patch_css_block(&existing, "button", next, Some(&previous_hash)).expect("patch css");

    assert!(patched.contains("color: blue"));
    assert!(!patched.contains("color: red"));
    assert!(patched.contains(".other {}"));
}

#[test]
fn css_block_extractor_requires_exact_managed_markers() {
    let block =
        "/* leptos-ui-kit:start button */\n.kit-button {}\n/* leptos-ui-kit:end button */\n";
    let css = format!(":root {{}}\n\n{block}.app {{}}\n");

    let extracted = extract_managed_css_block(&css, "button").expect("extract block");

    assert_eq!(extracted, Some(block.to_owned()));
    assert_eq!(
        extract_managed_css_block(":root {}\n", "button").expect("missing block"),
        None
    );
    assert!(extract_managed_css_block(&format!("{block}{block}"), "button").is_err());
}

#[test]
fn css_patcher_rejects_edited_tracked_block() {
    let previous = "/* leptos-ui-kit:start button */\n.kit-button { color: red; }\n/* leptos-ui-kit:end button */\n";
    let edited = "/* leptos-ui-kit:start button */\n.kit-button { color: green; }\n/* leptos-ui-kit:end button */\n";
    let next = "/* leptos-ui-kit:start button */\n.kit-button { color: blue; }\n/* leptos-ui-kit:end button */\n";

    let previous_hash = hash_bytes(previous.as_bytes());
    let error =
        patch_css_block(edited, "button", next, Some(&previous_hash)).expect_err("should conflict");

    assert!(matches!(error, CodegenError::UnsafePatch { .. }));
}

#[test]
fn module_patchers_insert_required_exports() {
    let components = patch_components_mod(Some("// existing\n")).expect("patch components");
    let ui = patch_ui_mod(
        Some("// generated exports\n"),
        &[
            UiModuleExport::new(
                "button",
                vec![
                    "Button".to_owned(),
                    "ButtonSize".to_owned(),
                    "ButtonType".to_owned(),
                    "ButtonVariant".to_owned(),
                ],
            ),
            UiModuleExport::with_path(
                "collapsible",
                "collapsible::root",
                vec!["CollapsibleRoot".to_owned()],
            ),
        ],
    )
    .expect("patch ui mod");

    assert_eq!(components, "// existing\npub mod ui;\n");
    assert_eq!(
        ui,
        "// generated exports\npub mod button;\npub use button::{Button, ButtonSize, ButtonType, ButtonVariant};\npub mod collapsible;\npub use collapsible::root::CollapsibleRoot;\n"
    );
    assert_eq!(
        patch_ui_mod(
            Some(&ui),
            &[
                UiModuleExport::new(
                    "button",
                    vec![
                        "Button".to_owned(),
                        "ButtonSize".to_owned(),
                        "ButtonType".to_owned(),
                        "ButtonVariant".to_owned(),
                    ],
                ),
                UiModuleExport::with_path(
                    "collapsible",
                    "collapsible::root",
                    vec!["CollapsibleRoot".to_owned()],
                ),
            ],
        )
        .expect("idempotent enough"),
        ui
    );
}

#[test]
fn ui_module_patcher_accepts_formatted_grouped_exports() {
    let existing = "pub mod menu;\npub use menu::{\n    MenuContent, MenuDirection, MenuItem, MenuItemIndicator, MenuItemKind, MenuLoop, MenuRoot,\n    MenuTrigger,\n};\n";
    let patched = patch_ui_mod(
        Some(existing),
        &[UiModuleExport::new(
            "menu",
            vec![
                "MenuContent".to_owned(),
                "MenuDirection".to_owned(),
                "MenuItem".to_owned(),
                "MenuItemIndicator".to_owned(),
                "MenuItemKind".to_owned(),
                "MenuLoop".to_owned(),
                "MenuRoot".to_owned(),
                "MenuTrigger".to_owned(),
            ],
        )],
    )
    .expect("formatted grouped export should be idempotent");

    assert_eq!(patched, existing);
}

#[test]
fn ui_module_patcher_consolidates_stale_grouped_exports() {
    let existing = "pub mod field;\npub use field::{\n    FieldLabel, FieldMessage, FieldRequired, FieldRoot, FieldSurface, NativeSelect, SelectIcon,\n    TextArea, TextInput, TextInputType,\n};\npub mod router_link;\npub use router_link::RouterLink;\npub use field::{FieldLabel, FieldMessage, FieldRequired, FieldRoot, FieldSurface, NativeSelect, SelectField, SelectIcon, TextArea, TextAreaField, TextField, TextInput, TextInputType};\n";
    let patched = patch_ui_mod(
        Some(existing),
        &[UiModuleExport::new(
            "field",
            vec![
                "FieldLabel".to_owned(),
                "FieldMessage".to_owned(),
                "FieldRequired".to_owned(),
                "FieldRoot".to_owned(),
                "FieldSurface".to_owned(),
                "NativeSelect".to_owned(),
                "SelectField".to_owned(),
                "SelectIcon".to_owned(),
                "TextArea".to_owned(),
                "TextAreaField".to_owned(),
                "TextField".to_owned(),
                "TextInput".to_owned(),
                "TextInputType".to_owned(),
            ],
        )],
    )
    .expect("stale grouped export should be consolidated");

    assert_eq!(
        patched,
        "pub mod field;\npub use field::{\n    FieldLabel, FieldMessage, FieldRequired, FieldRoot, FieldSurface, NativeSelect, SelectField,\n    SelectIcon, TextArea, TextAreaField, TextField, TextInput, TextInputType,\n};\npub mod router_link;\npub use router_link::RouterLink;\n\n"
    );
}

#[test]
fn ui_module_patcher_consolidates_stale_single_exports() {
    let existing = "pub mod spinner;\npub use spinner::Spinner;\n";
    let patched = patch_ui_mod(
        Some(existing),
        &[UiModuleExport::new(
            "spinner",
            vec!["Spinner".to_owned(), "SpinnerMode".to_owned()],
        )],
    )
    .expect("stale single export should be consolidated");

    assert_eq!(
        patched,
        "pub mod spinner;\npub use spinner::{Spinner, SpinnerMode};\n"
    );
}

#[test]
fn ui_module_patcher_removes_obsolete_grouped_exports() {
    let existing = "pub mod field;\npub use field::{FieldLabel, FieldRoot, FieldSlot, SelectField, SelectFieldSlot};\n";
    let patched = patch_ui_mod(
        Some(existing),
        &[UiModuleExport::new(
            "field",
            vec![
                "FieldLabel".to_owned(),
                "FieldRoot".to_owned(),
                "FieldSlot".to_owned(),
                "SelectField".to_owned(),
            ],
        )],
    )
    .expect("obsolete grouped export should be removed");

    assert_eq!(
        patched,
        "pub mod field;\npub use field::{FieldLabel, FieldRoot, FieldSlot, SelectField};\n"
    );
}

#[test]
fn ui_module_patcher_emits_rustfmt_stable_single_exports() {
    let ui = patch_ui_mod(
        Some("// generated exports\n"),
        &[UiModuleExport::new("spinner", vec!["Spinner".to_owned()])],
    )
    .expect("patch ui mod");

    assert_eq!(
        ui,
        "// generated exports\npub mod spinner;\npub use spinner::Spinner;\n"
    );
    assert_eq!(
        patch_ui_mod(
            Some(&ui),
            &[UiModuleExport::new("spinner", vec!["Spinner".to_owned()])],
        )
        .expect("formatted single export should be idempotent"),
        ui
    );
}

#[test]
fn component_module_patcher_rejects_private_conflict() {
    let error = patch_components_mod(Some("mod ui;\n")).expect_err("private conflict");

    assert!(matches!(error, CodegenError::UnsafePatch { .. }));
}

#[test]
fn path_safety_accepts_mvp_paths() {
    let paths = vec![
        DEFAULT_KIT_CONFIG_PATH.to_owned(),
        "index.html".to_owned(),
        "styles/kit.css".to_owned(),
        "src/components/mod.rs".to_owned(),
        "src/components/ui/button.rs".to_owned(),
        "src/components/ui/nested/root.rs".to_owned(),
        DEFAULT_KIT_LOCK_PATH.to_owned(),
    ];

    validate_planned_write_paths(&paths).expect("paths should pass");
}

#[test]
fn path_safety_rejects_unsafe_paths() {
    for path in [
        "../evil.rs",
        "/tmp/evil.rs",
        "C:\\evil.rs",
        "\\\\server\\share\\evil.rs",
        ".hidden",
        "src/components/ui/../../evil.rs",
        "src/components/ui/Button Rs",
        "src/lib.rs",
    ] {
        assert!(validate_logical_write_path(path).is_err(), "{path}");
    }
}

#[test]
fn path_safety_does_not_expose_internal_coordination_hidden_names() {
    for path in [
        DEFAULT_KIT_WRITE_LOCK_PATH,
        DEFAULT_KIT_COORDINATION_IGNORE_PATH,
        "src/components/ui/.write.lock",
        "src/components/ui/.gitignore",
        "src/components/ui/arbitrary/.write.lock",
        "src/components/ui/arbitrary/.gitignore",
        "styles/.write.lock",
        "styles/.gitignore",
    ] {
        assert!(
            validate_logical_write_path(path).is_err(),
            "public logical-path validation accepted {path}"
        );
    }
}

#[cfg(unix)]
#[test]
fn project_write_validation_preserves_the_supplied_root_shape() {
    let parent = tempfile::tempdir().expect("tempdir");
    let real_root = parent.path().join("real");
    let alias = parent.path().join("alias");
    fs::create_dir(&real_root).expect("create real root");
    fs::create_dir(real_root.join("styles")).expect("create styles");
    std::os::unix::fs::symlink(&real_root, &alias).expect("create root alias");

    let validated = validate_project_write_path(&alias, "styles/kit.css")
        .expect("validate through stable alias");

    assert_eq!(validated, alias.join("styles/kit.css"));
}

#[test]
fn path_safety_rejects_casefold_duplicate_paths() {
    let paths = vec![
        "src/components/ui/button.rs".to_owned(),
        "src/components/ui/Button.rs".to_owned(),
    ];

    let error = validate_planned_write_paths(&paths).expect_err("duplicate should fail");

    assert!(matches!(error, CodegenError::DuplicatePath(_)));
}

#[cfg(unix)]
#[test]
fn path_safety_rejects_symlink_escape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src/components")).expect("create components");
    let outside = tempfile::tempdir().expect("outside");
    std::os::unix::fs::symlink(outside.path(), root.join("src/components/ui")).expect("symlink");

    let error = validate_project_write_path(root, "src/components/ui/button.rs")
        .expect_err("symlink escape should fail");

    assert!(matches!(error, CodegenError::UnsafePath { .. }));
}

#[test]
fn advisory_write_lock_contention_is_typed_and_nonblocking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let _first = WriteLock::acquire(root).expect("first lock");
    let error = WriteLock::acquire(root).expect_err("second lock should contend");

    assert!(matches!(
        error,
        CodegenError::WriteLockContended { ref path }
            if path == DEFAULT_KIT_WRITE_LOCK_PATH
    ));
}

#[test]
fn first_use_publishes_only_an_initialized_and_locked_inode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let fs = Arc::new(FaultFs::passthrough());

    let lock = WriteLock::acquire_with(root, fs.clone()).expect("publish first-use lock");
    let operations = fs
        .events()
        .iter()
        .map(|event| event.operation)
        .collect::<Vec<_>>();
    let position = |operation| {
        operations
            .iter()
            .position(|candidate| *candidate == operation)
            .unwrap_or_else(|| panic!("missing {operation:?} bootstrap operation"))
    };

    assert!(
        position(FsOperation::TryLock) < position(FsOperation::WriteHandle)
            && position(FsOperation::WriteHandle) < position(FsOperation::SyncHandle)
            && position(FsOperation::SyncHandle) < position(FsOperation::HardLink),
        "candidate must be privately locked, written, and synced before publication: {operations:?}"
    );
    #[cfg(windows)]
    assert!(
        position(FsOperation::CreateNewFile) < position(FsOperation::OpenCandidateOwner)
            && position(FsOperation::OpenCandidateOwner) < position(FsOperation::TryLock)
            && position(FsOperation::HardLink) < position(FsOperation::RemoveFileByHandle),
        "Windows candidate guard must exist before its owner and be consumed only after publication: {operations:?}"
    );
    assert_exact_persistent_coordination(root);
    assert!(!root.join("src/components/ui/_kit/.transactions").exists());
    drop(lock);
}

const LOCK_STAGE_ROLE_ENV: &str = "LEPTOS_UI_KIT_LOCK_STAGE_ROLE";
const LOCK_STAGE_PROJECT_ENV: &str = "LEPTOS_UI_KIT_LOCK_STAGE_PROJECT";
const LOCK_STAGE_CONTROL_ENV: &str = "LEPTOS_UI_KIT_LOCK_STAGE_CONTROL";
const TRANSACTION_CRASH_OPERATION_ENV: &str = "LEPTOS_UI_KIT_TRANSACTION_CRASH_OPERATION";
const TRANSACTION_CRASH_ORDINAL_ENV: &str = "LEPTOS_UI_KIT_TRANSACTION_CRASH_ORDINAL";

#[test]
fn visible_unlocked_candidate_converges_on_the_published_lock() {
    let sandbox = tempfile::tempdir().expect("stage-race sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    fs::create_dir(&project).expect("create stage-race project");
    fs::create_dir(&control).expect("create stage-race control");

    let mut creator = spawn_lock_stage_worker("paused-create", &project, &control);
    wait_for_worker_path(&control.join("candidate-visible"), &mut creator);
    assert!(!project.join(DEFAULT_KIT_WRITE_LOCK_PATH).exists());
    let visible_candidates = transaction_candidate_paths(&project);
    assert_eq!(visible_candidates.len(), 1);
    let visible_identity = coordination_file_identity(&visible_candidates[0]);
    assert_eq!(
        fs::metadata(&visible_candidates[0])
            .expect("visible candidate metadata")
            .len(),
        0
    );

    #[cfg(not(windows))]
    let holder = WriteLock::acquire(&project).expect("publisher claims unlocked stale candidate");
    #[cfg(windows)]
    let contender = WriteLock::acquire(&project)
        .expect_err("the live Windows source guard must block candidate reclamation");
    #[cfg(windows)]
    assert!(matches!(contender, CodegenError::WriteLockContended { .. }));
    let published_identity = coordination_lock_identity(&project);
    #[cfg(not(windows))]
    {
        assert_exact_persistent_coordination(&project);
        let claimed_candidates = transaction_candidate_paths(&project);
        assert!(claimed_candidates.is_empty());
    }
    #[cfg(windows)]
    {
        let guarded_candidates = transaction_candidate_paths(&project);
        assert_eq!(guarded_candidates.len(), 1);
        assert_eq!(
            coordination_file_identity(&guarded_candidates[0]),
            visible_identity
        );
    }

    fs::write(control.join("release-creator"), b"release\n").expect("release creator");
    #[cfg(not(windows))]
    wait_for_worker_path(&control.join("creator-contended"), &mut creator);
    #[cfg(windows)]
    wait_for_worker_path(&control.join("creator-acquired"), &mut creator);
    creator.wait_success();
    #[cfg(not(windows))]
    drop(holder);

    assert_eq!(coordination_lock_identity(&project), published_identity);
    assert_ne!(visible_identity, published_identity);
    assert_only_verified_coordination_residuals(&project);
    assert_exact_persistent_coordination(&project);

    let lock = WriteLock::acquire(&project).expect("reacquire converged lock");
    assert_eq!(coordination_lock_identity(&project), published_identity);
    drop(lock);
}

#[test]
fn privately_locked_first_use_candidate_blocks_until_it_converges() {
    let sandbox = tempfile::tempdir().expect("active-candidate sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    fs::create_dir(&project).expect("create active-candidate project");
    fs::create_dir(&control).expect("create active-candidate control");

    let mut creator = spawn_lock_stage_worker("paused-private-lock", &project, &control);
    wait_for_worker_path(&control.join("candidate-locked"), &mut creator);
    let candidates = transaction_candidate_paths(&project);
    assert_eq!(candidates.len(), 1);
    let candidate_identity = coordination_file_identity(&candidates[0]);

    let contender = WriteLock::acquire(&project)
        .expect_err("publisher must not plan while another candidate is privately locked");
    assert!(matches!(contender, CodegenError::WriteLockContended { .. }));
    let published_identity = coordination_lock_identity(&project);
    let candidates = transaction_candidate_paths(&project);
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        coordination_file_identity(&candidates[0]),
        candidate_identity
    );

    fs::write(control.join("release-private-lock"), b"release\n")
        .expect("release private candidate");
    wait_for_worker_path(&control.join("private-creator-acquired"), &mut creator);
    creator.wait_success();

    assert_eq!(coordination_lock_identity(&project), published_identity);
    assert!(transaction_candidate_paths(&project).is_empty());
    assert_only_verified_coordination_residuals(&project);
    assert_exact_persistent_coordination(&project);
}

#[test]
fn killed_writer_after_lock_publication_recovers_exact_coordination_state() {
    let sandbox = tempfile::tempdir().expect("post-publication crash sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    fs::create_dir(&project).expect("create crash project");
    fs::create_dir(&control).expect("create crash control");

    let mut publisher = spawn_lock_stage_worker("paused-hardlink", &project, &control);
    wait_for_worker_path(&control.join("lock-published"), &mut publisher);
    let lock_path = project.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    let lock_identity = coordination_file_identity(&lock_path);
    let candidates = transaction_candidate_paths(&project);
    assert_eq!(candidates.len(), 1, "published lock must retain one alias");
    assert_eq!(coordination_file_identity(&candidates[0]), lock_identity);
    assert_eq!(
        fs::read(&candidates[0]).expect("read published candidate alias"),
        KIT_ADVISORY_LOCK_CONTENT
    );
    let contender = WriteLock::acquire(&project).expect_err("publisher must hold final inode");
    assert!(matches!(contender, CodegenError::WriteLockContended { .. }));

    publisher.kill_and_wait();

    let recovered = WriteLock::acquire(&project).expect("recover published candidate alias");
    assert_eq!(coordination_lock_identity(&project), lock_identity);
    drop(recovered);
    assert!(transaction_candidate_paths(&project).is_empty());
    assert_only_verified_coordination_residuals(&project);
    assert_exact_persistent_coordination(&project);
}

#[test]
fn killed_bootstrap_creation_and_ignore_transitions_recover_exactly() {
    let baseline = tempfile::tempdir().expect("directory-transition baseline");
    let baseline_fs = Arc::new(FaultFs::passthrough());
    let baseline_lock = WriteLock::acquire_with(baseline.path(), baseline_fs.clone())
        .expect("baseline coordination bootstrap");
    drop(baseline_lock);
    let directory_creations = baseline_fs
        .events()
        .into_iter()
        .filter(|event| event.operation == FsOperation::CreateDirectory)
        .count();
    assert_eq!(
        directory_creations, 6,
        "every current coordination-directory creation must have a crash role"
    );

    let mut cases = (1..=directory_creations)
        .map(|ordinal| {
            (
                format!("paused-directory-{ordinal}"),
                format!("directory-created-{ordinal}"),
            )
        })
        .collect::<Vec<_>>();
    cases.extend([
        ("paused-create".to_owned(), "candidate-visible".to_owned()),
        (
            "paused-ignore-create".to_owned(),
            "ignore-candidate-created".to_owned(),
        ),
        (
            "paused-ignore-hardlink".to_owned(),
            "ignore-published".to_owned(),
        ),
    ]);

    for (role, ready) in cases {
        let sandbox = tempfile::tempdir().expect("crash transition sandbox");
        let project = sandbox.path().join("project");
        let control = sandbox.path().join("control");
        fs::create_dir(&project).expect("create crash transition project");
        fs::create_dir(&control).expect("create crash transition control");

        let mut worker = spawn_lock_stage_worker(&role, &project, &control);
        wait_for_worker_path(&control.join(&ready), &mut worker);
        worker.kill_and_wait();

        let recovered = WriteLock::acquire(&project)
            .unwrap_or_else(|error| panic!("recover {role} transition: {error}"));
        drop(recovered);
        assert_only_verified_coordination_residuals(&project);
        assert_exact_persistent_coordination(&project);
    }
}

fn seed_stale_lock_candidate(root: &Path, ordinal: u128, mode: u32) -> PathBuf {
    let transactions = root.join("src/components/ui/_kit/.transactions");
    if !transactions.exists() {
        fs::create_dir(&transactions).expect("create stale-candidate transaction directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&transactions, fs::Permissions::from_mode(0o700))
                .expect("set stale-candidate transaction mode");
        }
    }
    let candidate = transactions.join(format!("lock-bootstrap-{ordinal:032x}"));
    fs::write(&candidate, b"").expect("seed stale lock candidate");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&candidate, fs::Permissions::from_mode(mode))
            .expect("set stale lock candidate mode");
    }
    #[cfg(not(unix))]
    let _ = mode;
    candidate
}

#[test]
fn stale_cleanup_removes_every_duplicate_hard_link_alias_before_success() {
    let dir = tempfile::tempdir().expect("duplicate-alias tempdir");
    let root = dir.path();
    let initial = WriteLock::acquire(root).expect("bootstrap persistent coordination");
    let lock_identity = coordination_lock_identity(root);
    drop(initial);

    let first = seed_stale_lock_candidate(root, 1, 0o600);
    let second = first
        .parent()
        .expect("candidate parent")
        .join(format!("lock-bootstrap-{:032x}", 2_u128));
    fs::hard_link(&first, &second).expect("create duplicate candidate alias");
    assert_eq!(
        coordination_file_identity(&first),
        coordination_file_identity(&second)
    );

    let recovered = WriteLock::acquire(root).expect("clean duplicate candidate aliases");
    assert_eq!(coordination_lock_identity(root), lock_identity);
    drop(recovered);

    assert!(transaction_candidate_paths(root).is_empty());
    assert!(!root.join("src/components/ui/_kit/.transactions").exists());
    assert_only_verified_coordination_residuals(root);
    assert_exact_persistent_coordination(root);
}

#[test]
fn stale_cleanup_rescans_and_removes_a_candidate_that_arrives_during_cleanup() {
    let sandbox = tempfile::tempdir().expect("late-candidate sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    fs::create_dir(&project).expect("create late-candidate project");
    fs::create_dir(&control).expect("create late-candidate control");
    let initial = WriteLock::acquire(&project).expect("bootstrap persistent coordination");
    let lock_identity = coordination_lock_identity(&project);
    drop(initial);
    seed_stale_lock_candidate(&project, 10, 0o600);

    let ready = control.join("first-candidate-claimed");
    let release = control.join("release-first-candidate");
    let worker_project = project.clone();
    let worker_ready = ready.clone();
    let worker_release = release.clone();
    let worker = thread::spawn(move || {
        let fs = Arc::new(FaultFs::pause_after_success(
            FsOperation::TryLock,
            2,
            worker_ready,
            worker_release,
        ));
        let lock = WriteLock::acquire_with(&worker_project, fs)
            .expect("cleanup writer acquires after rescan");
        drop(lock);
    });

    wait_for_stage_path(&ready);
    seed_stale_lock_candidate(&project, 11, 0o600);
    fs::write(&release, b"release\n").expect("release cleanup writer");
    worker.join().expect("join cleanup writer");

    assert_eq!(coordination_lock_identity(&project), lock_identity);
    assert!(transaction_candidate_paths(&project).is_empty());
    assert!(
        !project
            .join("src/components/ui/_kit/.transactions")
            .exists()
    );
    assert_only_verified_coordination_residuals(&project);
    assert_exact_persistent_coordination(&project);
}

#[cfg(unix)]
#[test]
fn stale_cleanup_recovers_owner_restrictive_modes_but_rejects_external_aliases() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    for (ordinal, mode) in [(20_u128, 0o000), (21, 0o200), (22, 0o400)] {
        let dir = tempfile::tempdir().expect("restrictive-candidate tempdir");
        let root = dir.path();
        let initial = WriteLock::acquire(root).expect("bootstrap persistent coordination");
        let lock_identity = coordination_lock_identity(root);
        drop(initial);
        seed_stale_lock_candidate(root, ordinal, mode);

        let recovered = WriteLock::acquire(root)
            .unwrap_or_else(|error| panic!("recover mode {mode:04o} candidate: {error}"));
        assert_eq!(coordination_lock_identity(root), lock_identity);
        drop(recovered);
        assert!(!root.join("src/components/ui/_kit/.transactions").exists());
        assert_only_verified_coordination_residuals(root);
        assert_exact_persistent_coordination(root);
    }

    let dir = tempfile::tempdir().expect("external-alias tempdir");
    let root = dir.path();
    let initial = WriteLock::acquire(root).expect("bootstrap persistent coordination");
    let lock_identity = coordination_lock_identity(root);
    drop(initial);
    let candidate = seed_stale_lock_candidate(root, 23, 0o000);
    let external = root.join("application-owned-alias");
    fs::hard_link(&candidate, &external).expect("create external hard-link alias");
    let candidate_identity = coordination_file_identity(&candidate);
    assert_eq!(coordination_file_identity(&external), candidate_identity);
    assert_eq!(
        fs::metadata(&candidate)
            .expect("candidate metadata")
            .nlink(),
        2
    );

    let error = WriteLock::acquire(root)
        .expect_err("external hard-link alias must block owner-mode recovery");
    assert!(matches!(
        error,
        CodegenError::InvalidCoordinationState { .. }
    ));
    assert_eq!(coordination_lock_identity(root), lock_identity);
    assert_eq!(coordination_file_identity(&candidate), candidate_identity);
    assert_eq!(coordination_file_identity(&external), candidate_identity);
    assert_eq!(
        fs::metadata(&candidate)
            .expect("candidate metadata after rejection")
            .permissions()
            .mode()
            & 0o7777,
        0o000
    );
    assert_eq!(
        fs::metadata(&external)
            .expect("external metadata after rejection")
            .permissions()
            .mode()
            & 0o7777,
        0o000
    );
}

#[test]
fn persistent_stale_candidate_removal_failure_surfaces_then_recovers() {
    let dir = tempfile::tempdir().expect("persistent-removal tempdir");
    let root = dir.path();
    let initial = WriteLock::acquire(root).expect("bootstrap persistent coordination");
    let lock_identity = coordination_lock_identity(root);
    drop(initial);
    let candidate = seed_stale_lock_candidate(root, 30, 0o600);
    #[cfg(windows)]
    let removal_operation = FsOperation::RemoveFileByHandle;
    #[cfg(not(windows))]
    let removal_operation = FsOperation::RemoveFile;
    let fs = Arc::new(FaultFs::fail_from(removal_operation, 1));

    let error = WriteLock::acquire_with(root, fs)
        .expect_err("persistent stale-candidate removal fault must surface");
    assert!(matches!(error, CodegenError::Io { .. }));
    assert_eq!(coordination_lock_identity(root), lock_identity);
    assert!(
        candidate.exists(),
        "failed removal must preserve the candidate"
    );

    let recovered = WriteLock::acquire(root).expect("next writer recovers stale candidate");
    assert_eq!(coordination_lock_identity(root), lock_identity);
    drop(recovered);
    assert!(!root.join("src/components/ui/_kit/.transactions").exists());
    assert_only_verified_coordination_residuals(root);
    assert_exact_persistent_coordination(root);
}

#[cfg(unix)]
#[test]
fn transaction_directory_substitution_never_mutates_the_detached_candidate() {
    let sandbox = tempfile::tempdir().expect("transaction substitution sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    fs::create_dir(&project).expect("create substitution project");
    fs::create_dir(&control).expect("create substitution control");

    let mut worker = spawn_lock_stage_worker("paused-detach", &project, &control);
    wait_for_worker_path(&control.join("detach-candidate-visible"), &mut worker);
    let transactions = project.join("src/components/ui/_kit/.transactions");
    let moved = project.join("src/components/ui/_kit/.transactions-moved");
    let candidates = transaction_candidate_paths(&project);
    assert_eq!(candidates.len(), 1);
    let candidate_name = candidates[0]
        .file_name()
        .expect("candidate name")
        .to_owned();
    let candidate_identity = coordination_file_identity(&candidates[0]);
    fs::rename(&transactions, &moved).expect("detach transaction directory");
    fs::create_dir(&transactions).expect("create replacement transaction directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&transactions, fs::Permissions::from_mode(0o700))
            .expect("set replacement transaction mode");
    }

    fs::write(control.join("release-detach"), b"release\n").expect("release detached worker");
    wait_for_worker_path(&control.join("detach-rejected"), &mut worker);
    worker.wait_success();

    let moved_candidate = moved.join(candidate_name);
    assert_eq!(
        coordination_file_identity(&moved_candidate),
        candidate_identity
    );
    assert_eq!(
        fs::read(moved_candidate).expect("read detached candidate"),
        b""
    );
    assert!(transaction_candidate_paths(&project).is_empty());
    assert!(!project.join(DEFAULT_KIT_WRITE_LOCK_PATH).exists());
}

#[cfg(unix)]
#[test]
fn kit_ancestor_substitution_never_mutates_detached_candidates() {
    use std::os::unix::fs::PermissionsExt;

    for (case, ancestor, moved_name, candidate_tail) in [
        (
            "kit",
            "src/components/ui/_kit",
            "_kit-moved",
            ".transactions",
        ),
        ("ui", "src/components/ui", "ui-moved", "_kit/.transactions"),
    ] {
        let sandbox = tempfile::tempdir().expect("ancestor substitution sandbox");
        let project = sandbox.path().join("project");
        let control = sandbox.path().join("control");
        fs::create_dir(&project).expect("create substitution project");
        fs::create_dir(&control).expect("create substitution control");

        let mut worker = spawn_lock_stage_worker("paused-detach", &project, &control);
        wait_for_worker_path(&control.join("detach-candidate-visible"), &mut worker);
        let candidates = transaction_candidate_paths(&project);
        assert_eq!(candidates.len(), 1, "{case}");
        let candidate_name = candidates[0]
            .file_name()
            .expect("candidate name")
            .to_owned();
        let candidate_identity = coordination_file_identity(&candidates[0]);
        let ancestor_path = project.join(ancestor);
        let moved = ancestor_path
            .parent()
            .expect("ancestor parent")
            .join(moved_name);
        fs::rename(&ancestor_path, &moved).expect("detach coordination ancestor");
        let replacement_transactions = project.join("src/components/ui/_kit/.transactions");
        fs::create_dir_all(&replacement_transactions).expect("create replacement chain");
        fs::set_permissions(
            project.join("src/components/ui/_kit"),
            fs::Permissions::from_mode(0o700),
        )
        .expect("set replacement kit mode");
        fs::set_permissions(&replacement_transactions, fs::Permissions::from_mode(0o700))
            .expect("set replacement transaction mode");

        fs::write(control.join("release-detach"), b"release\n").expect("release detached worker");
        wait_for_worker_path(&control.join("detach-rejected"), &mut worker);
        worker.wait_success();

        let moved_candidate = moved.join(candidate_tail).join(candidate_name);
        assert_eq!(
            coordination_file_identity(&moved_candidate),
            candidate_identity,
            "{case}"
        );
        assert_eq!(
            fs::read(moved_candidate).expect("read detached candidate"),
            b"",
            "{case}"
        );
        assert!(transaction_candidate_paths(&project).is_empty(), "{case}");
        assert!(
            !project.join(DEFAULT_KIT_WRITE_LOCK_PATH).exists(),
            "{case}"
        );
    }
}

#[cfg(unix)]
#[test]
fn candidate_leaf_substitution_is_rejected_before_publication_or_cleanup() {
    let sandbox = tempfile::tempdir().expect("candidate substitution sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    fs::create_dir(&project).expect("create substitution project");
    fs::create_dir(&control).expect("create substitution control");

    let mut worker = spawn_lock_stage_worker("paused-source", &project, &control);
    wait_for_worker_path(&control.join("source-candidate-synced"), &mut worker);
    let candidates = transaction_candidate_paths(&project);
    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    let moved = project.join("src/components/ui/_kit/detached-candidate");
    let identity = coordination_file_identity(candidate);
    fs::rename(candidate, &moved).expect("detach initialized candidate leaf");
    fs::write(candidate, b"attacker replacement\n").expect("write replacement candidate leaf");

    fs::write(control.join("release-source"), b"release\n").expect("release source worker");
    wait_for_worker_path(&control.join("source-rejected"), &mut worker);
    worker.wait_success();

    assert_eq!(coordination_file_identity(&moved), identity);
    assert_eq!(
        fs::read(&moved).expect("read detached initialized candidate"),
        KIT_ADVISORY_LOCK_CONTENT
    );
    assert_eq!(
        fs::read(candidate).expect("read attacker replacement"),
        b"attacker replacement\n"
    );
    assert!(!project.join(DEFAULT_KIT_WRITE_LOCK_PATH).exists());
}

#[cfg(windows)]
#[test]
fn windows_source_guard_prevents_candidate_leaf_substitution() {
    let sandbox = tempfile::tempdir().expect("candidate guard sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    fs::create_dir(&project).expect("create candidate guard project");
    fs::create_dir(&control).expect("create candidate guard control");

    let mut worker = spawn_lock_stage_worker("paused-source-guard", &project, &control);
    wait_for_worker_path(&control.join("source-guard-held"), &mut worker);
    let candidates = transaction_candidate_paths(&project);
    assert_eq!(candidates.len(), 1);
    let moved = project.join("src/components/ui/_kit/detached-candidate");
    let error = fs::rename(&candidates[0], &moved)
        .expect_err("Windows source guard must deny candidate rename");
    assert!(matches!(error.raw_os_error(), Some(5) | Some(32)));
    assert!(!moved.exists());

    fs::write(control.join("release-source-guard"), b"release\n")
        .expect("release source guard worker");
    wait_for_worker_path(&control.join("source-guard-acquired"), &mut worker);
    worker.wait_success();

    assert_only_verified_coordination_residuals(&project);
    assert_exact_persistent_coordination(&project);
}

#[test]
fn concurrent_destination_creation_is_no_clobber_and_converges() {
    let sandbox = tempfile::tempdir().expect("destination race sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    fs::create_dir(&project).expect("create destination race project");
    fs::create_dir(&control).expect("create destination race control");

    let mut worker = spawn_lock_stage_worker("paused-destination", &project, &control);
    wait_for_worker_path(&control.join("destination-candidate-synced"), &mut worker);
    let lock_path = project.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    fs::write(&lock_path, KIT_ADVISORY_LOCK_CONTENT).expect("publish competing destination");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))
            .expect("set competing destination mode");
    }
    let identity = coordination_file_identity(&lock_path);

    fs::write(control.join("release-destination"), b"release\n")
        .expect("release destination race worker");
    wait_for_worker_path(&control.join("destination-converged"), &mut worker);
    worker.wait_success();

    assert_eq!(coordination_file_identity(&lock_path), identity);
    assert_eq!(
        fs::read(&lock_path).expect("read competing destination"),
        KIT_ADVISORY_LOCK_CONTENT
    );
    assert!(transaction_candidate_paths(&project).is_empty());
    assert_only_verified_coordination_residuals(&project);
    assert_exact_persistent_coordination(&project);
}

#[test]
fn simultaneous_publishers_tolerate_a_live_peer_alias_while_the_loser_converges() {
    let sandbox = tempfile::tempdir().expect("two-publisher sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    fs::create_dir(&project).expect("create two-publisher project");
    fs::create_dir(&control).expect("create two-publisher control");

    let mut winner = spawn_lock_stage_worker("two-publisher-winner", &project, &control);
    let mut loser = spawn_lock_stage_worker("two-publisher-loser", &project, &control);
    wait_for_worker_path(&control.join("winner-candidate-synced"), &mut winner);
    wait_for_worker_path(&control.join("loser-candidate-synced"), &mut loser);
    assert_eq!(
        transaction_candidate_paths(&project).len(),
        2,
        "both private publishers must be staged before publication"
    );

    fs::write(control.join("release-winner-publication"), b"release\n")
        .expect("release winning publisher");
    wait_for_worker_path(&control.join("winner-lock-published"), &mut winner);
    let published_identity = coordination_lock_identity(&project);
    let published_aliases = transaction_candidate_paths(&project);
    assert_eq!(
        published_aliases.len(),
        2,
        "winner must retain its source alias while the loser converges"
    );
    assert!(
        published_aliases
            .iter()
            .any(|alias| coordination_file_identity(alias) == published_identity),
        "one staged alias must identify the published lock"
    );

    fs::write(control.join("release-loser-publication"), b"release\n")
        .expect("release losing publisher");
    wait_for_worker_path(&control.join("loser-contended"), &mut loser);
    loser.wait_success();
    let remaining_aliases = transaction_candidate_paths(&project);
    assert_eq!(
        remaining_aliases.len(),
        1,
        "loser must remove only its own source alias"
    );
    assert_eq!(
        coordination_file_identity(&remaining_aliases[0]),
        published_identity,
        "winner's published source alias must remain untouched"
    );

    fs::write(control.join("release-winner-after-loser"), b"release\n")
        .expect("release winner after loser converges");
    wait_for_worker_path(&control.join("winner-acquired"), &mut winner);
    winner.wait_success();

    assert_eq!(coordination_lock_identity(&project), published_identity);
    assert!(transaction_candidate_paths(&project).is_empty());
    assert_only_verified_coordination_residuals(&project);
    assert_exact_persistent_coordination(&project);
}

#[cfg(windows)]
#[test]
fn windows_coordination_bootstrap_supports_long_project_paths() {
    let sandbox = tempfile::tempdir().expect("long-path sandbox");
    let mut project = sandbox.path().to_path_buf();
    for index in 0..24 {
        project.push(format!("project-segment-{index:02}"));
    }
    fs::create_dir_all(&project).expect("create long project path");
    assert!(
        project.as_os_str().len() > 260,
        "test path must exceed MAX_PATH"
    );

    let lock = WriteLock::acquire(&project).expect("bootstrap below long project path");
    drop(lock);

    assert_only_verified_coordination_residuals(&project);
    assert_exact_persistent_coordination(&project);
}

#[cfg(windows)]
#[test]
fn windows_pinned_directory_chain_denies_ancestor_substitution() {
    let sandbox = tempfile::tempdir().expect("pinned-chain sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    fs::create_dir(&project).expect("create pinned-chain project");
    fs::create_dir(&control).expect("create pinned-chain control");

    let mut worker = spawn_lock_stage_worker("paused-directory-guards", &project, &control);
    wait_for_worker_path(&control.join("directory-guards-held"), &mut worker);
    for (source, destination) in [
        (
            "src/components/ui/_kit/.transactions",
            "src/components/ui/_kit/.transactions-moved",
        ),
        ("src/components/ui/_kit", "src/components/ui/_kit-moved"),
        ("src/components/ui", "src/components/ui-moved"),
    ] {
        let error = fs::rename(project.join(source), project.join(destination))
            .expect_err("pinned Windows directory must deny rename");
        assert!(matches!(error.raw_os_error(), Some(5) | Some(32)));
    }

    fs::write(control.join("release-directory-guards"), b"release\n")
        .expect("release pinned-chain worker");
    wait_for_worker_path(&control.join("directory-guards-acquired"), &mut worker);
    worker.wait_success();

    assert_only_verified_coordination_residuals(&project);
    assert_exact_persistent_coordination(&project);
}

#[test]
fn transaction_lock_stage_worker() {
    let Some(role) = env::var_os(LOCK_STAGE_ROLE_ENV) else {
        return;
    };
    let project = PathBuf::from(
        env::var_os(LOCK_STAGE_PROJECT_ENV).expect("stage worker project environment"),
    );
    let control = PathBuf::from(
        env::var_os(LOCK_STAGE_CONTROL_ENV).expect("stage worker control environment"),
    );
    let role = role.to_str().expect("UTF-8 stage worker role");
    if role == "transaction-crash" {
        let operation = parse_transaction_crash_operation(
            &env::var(TRANSACTION_CRASH_OPERATION_ENV).expect("crash operation environment"),
        );
        let ordinal = env::var(TRANSACTION_CRASH_ORDINAL_ENV)
            .expect("crash ordinal environment")
            .parse::<usize>()
            .expect("crash ordinal");
        let fs = Arc::new(FaultFs::pause_after_success(
            operation,
            ordinal,
            control.join("transaction-crash-ready"),
            control.join("release-transaction-crash"),
        ));
        let (files, changes) = two_file_update_plan();
        let result = apply_planned_files_with(&project, &files, &changes, fs);
        panic!("transaction crash worker passed its barrier: {result:?}");
    }
    if let Some(ordinal) = role.strip_prefix("paused-directory-") {
        let ordinal = ordinal.parse::<usize>().expect("directory pause ordinal");
        let fs = Arc::new(FaultFs::pause_after_success(
            FsOperation::CreateDirectory,
            ordinal,
            control.join(format!("directory-created-{ordinal}")),
            control.join(format!("release-directory-{ordinal}")),
        ));
        let lock = WriteLock::acquire_with(&project, fs).expect("directory pause resumes");
        drop(lock);
        return;
    }
    match role {
        "paused-create" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::CreateNewFile,
                1,
                control.join("candidate-visible"),
                control.join("release-creator"),
            ));
            #[cfg(not(windows))]
            {
                let error = WriteLock::acquire_with(&project, fs)
                    .expect_err("paused unlocked creator must converge as a contender");
                assert!(matches!(error, CodegenError::WriteLockContended { .. }));
                fs::write(control.join("creator-contended"), b"contended\n")
                    .expect("signal creator contention");
            }
            #[cfg(windows)]
            {
                let lock = WriteLock::acquire_with(&project, fs)
                    .expect("guarded creator must converge on the published lock");
                fs::write(control.join("creator-acquired"), b"acquired\n")
                    .expect("signal creator acquisition");
                drop(lock);
            }
        }
        "paused-private-lock" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::TryLock,
                1,
                control.join("candidate-locked"),
                control.join("release-private-lock"),
            ));
            let lock = WriteLock::acquire_with(&project, fs)
                .expect("private candidate owner converges on the published inode");
            fs::write(control.join("private-creator-acquired"), b"acquired\n")
                .expect("signal private creator acquisition");
            drop(lock);
        }
        "paused-hardlink" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::HardLink,
                1,
                control.join("lock-published"),
                control.join("release-publisher"),
            ));
            let lock = WriteLock::acquire_with(&project, fs).expect("publisher resumes");
            drop(lock);
        }
        "paused-ignore-create" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::CreateNewFile,
                2,
                control.join("ignore-candidate-created"),
                control.join("release-ignore-candidate"),
            ));
            let lock = WriteLock::acquire_with(&project, fs).expect("ignore candidate resumes");
            drop(lock);
        }
        "paused-ignore-hardlink" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::HardLink,
                2,
                control.join("ignore-published"),
                control.join("release-ignore-publication"),
            ));
            let lock = WriteLock::acquire_with(&project, fs).expect("ignore publication resumes");
            drop(lock);
        }
        "paused-detach" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::CreateNewFile,
                1,
                control.join("detach-candidate-visible"),
                control.join("release-detach"),
            ));
            let error = WriteLock::acquire_with(&project, fs)
                .expect_err("detached transaction directory must fail closed");
            assert!(matches!(
                error,
                CodegenError::UnsafePath { .. }
                    | CodegenError::PreimageConflict { .. }
                    | CodegenError::ProjectRootChanged { .. }
            ));
            fs::write(control.join("detach-rejected"), b"rejected\n")
                .expect("signal detached rejection");
        }
        "paused-source" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::SyncHandle,
                1,
                control.join("source-candidate-synced"),
                control.join("release-source"),
            ));
            let error = WriteLock::acquire_with(&project, fs)
                .expect_err("substituted candidate leaf must fail closed");
            assert!(matches!(error, CodegenError::UnsafePath { .. }));
            fs::write(control.join("source-rejected"), b"rejected\n")
                .expect("signal source rejection");
        }
        "paused-source-guard" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::SyncHandle,
                1,
                control.join("source-guard-held"),
                control.join("release-source-guard"),
            ));
            let lock = WriteLock::acquire_with(&project, fs)
                .expect("guarded candidate publishes after rejected substitution");
            fs::write(control.join("source-guard-acquired"), b"acquired\n")
                .expect("signal source guard acquisition");
            drop(lock);
        }
        "paused-destination" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::SyncHandle,
                1,
                control.join("destination-candidate-synced"),
                control.join("release-destination"),
            ));
            let lock = WriteLock::acquire_with(&project, fs)
                .expect("publisher converges on competing valid destination");
            fs::write(control.join("destination-converged"), b"converged\n")
                .expect("signal destination convergence");
            drop(lock);
        }
        "two-publisher-winner" => {
            let fs = Arc::new(FaultFs::pause_after_successes(vec![
                (
                    FsOperation::SyncHandle,
                    1,
                    control.join("winner-candidate-synced"),
                    control.join("release-winner-publication"),
                ),
                (
                    FsOperation::HardLink,
                    1,
                    control.join("winner-lock-published"),
                    control.join("release-winner-after-loser"),
                ),
            ]));
            let lock = WriteLock::acquire_with(&project, fs).expect("winning publisher resumes");
            fs::write(control.join("winner-acquired"), b"acquired\n")
                .expect("signal winning publisher acquisition");
            drop(lock);
        }
        "two-publisher-loser" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::SyncHandle,
                1,
                control.join("loser-candidate-synced"),
                control.join("release-loser-publication"),
            ));
            let error = WriteLock::acquire_with(&project, fs)
                .expect_err("losing publisher must converge as a contender");
            assert!(matches!(error, CodegenError::WriteLockContended { .. }));
            fs::write(control.join("loser-contended"), b"contended\n")
                .expect("signal losing publisher contention");
        }
        "paused-directory-guards" => {
            let fs = Arc::new(FaultFs::pause_after_success(
                FsOperation::CreateNewFile,
                1,
                control.join("directory-guards-held"),
                control.join("release-directory-guards"),
            ));
            let lock =
                WriteLock::acquire_with(&project, fs).expect("pinned-chain publisher resumes");
            fs::write(control.join("directory-guards-acquired"), b"acquired\n")
                .expect("signal pinned-chain acquisition");
            drop(lock);
        }
        other => panic!("unknown lock-stage worker role {other}"),
    }
}

#[test]
fn existing_empty_lock_is_rejected_without_timed_handoff_or_replacement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let lock_path = root.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("create lock parent");
    fs::write(&lock_path, b"").expect("seed in-progress marker");
    let identity = coordination_lock_identity(root);
    let fs = Arc::new(FaultFs::passthrough());

    let error =
        WriteLock::acquire_with(root, fs).expect_err("pre-existing empty state must fail closed");

    assert!(matches!(
        error,
        CodegenError::InvalidCoordinationState { ref path, .. }
            if path == DEFAULT_KIT_WRITE_LOCK_PATH
    ));
    assert_eq!(coordination_lock_identity(root), identity);
    assert_eq!(fs::read(lock_path).expect("read in-progress marker"), b"");
    assert!(!root.join(DEFAULT_KIT_COORDINATION_IGNORE_PATH).exists());
}

#[cfg(unix)]
#[test]
fn advisory_lock_bootstrap_rejects_parent_and_final_symlink_indirection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src/components")).expect("component parent");
    std::os::unix::fs::symlink(outside.path(), root.join("src/components/ui"))
        .expect("parent symlink");

    let error = WriteLock::acquire(root).expect_err("parent symlink must fail");
    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert!(
        fs::read_dir(outside.path())
            .expect("outside entries")
            .next()
            .is_none()
    );

    fs::remove_file(root.join("src/components/ui")).expect("remove parent symlink");
    let lock_path = root.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("create lock parent");
    let referent = outside.path().join("sentinel-referent");
    fs::write(&referent, b"outside\n").expect("write referent");
    std::os::unix::fs::symlink(&referent, &lock_path).expect("final symlink");

    let error = WriteLock::acquire(root).expect_err("final symlink must fail");
    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(fs::read(referent).expect("read referent"), b"outside\n");
}

#[test]
fn advisory_lock_drop_preserves_and_releases_the_persistent_inode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let lock = WriteLock::acquire(root).expect("acquire lock");
    let identity = coordination_lock_identity(root);
    assert_exact_persistent_coordination(root);

    drop(lock);

    assert_eq!(coordination_lock_identity(root), identity);
    assert_exact_persistent_coordination(root);
    let reacquired = WriteLock::acquire(root).expect("reacquire released inode");
    assert_eq!(coordination_lock_identity(root), identity);
    drop(reacquired);
}

#[test]
fn atomic_write_preserves_the_persistent_advisory_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");

    write_file_atomic(root, "styles/kit.css", b":root {}\n").expect("write css");
    let identity = coordination_lock_identity(root);

    assert_eq!(
        fs::read_to_string(root.join("styles/kit.css")).expect("read css"),
        ":root {}\n"
    );
    assert_exact_persistent_coordination(root);
    let lock = WriteLock::acquire(root).expect("atomic writer released lock");
    assert_eq!(coordination_lock_identity(root), identity);
    drop(lock);
}

#[test]
fn advisory_write_lock_uses_exact_format_and_preserves_debug_shape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let lock = WriteLock::acquire(root).expect("lock");

    assert_eq!(
        fs::read(root.join(DEFAULT_KIT_WRITE_LOCK_PATH)).expect("read advisory lock"),
        KIT_ADVISORY_LOCK_CONTENT
    );
    assert_eq!(
        fs::read(root.join(DEFAULT_KIT_COORDINATION_IGNORE_PATH))
            .expect("read coordination ignore"),
        KIT_COORDINATION_IGNORE_CONTENT
    );
    assert_eq!(
        format!("{lock:?}"),
        format!(
            "WriteLock {{ path: {:?} }}",
            fs::canonicalize(root)
                .expect("canonical project root")
                .join(DEFAULT_KIT_WRITE_LOCK_PATH)
        )
    );
}

#[test]
fn bootstrap_faults_leave_only_verified_coordination_residuals() {
    let baseline = tempfile::tempdir().expect("baseline tempdir");
    let baseline_fs = Arc::new(FaultFs::passthrough());
    let baseline_lock =
        WriteLock::acquire_with(baseline.path(), baseline_fs.clone()).expect("baseline bootstrap");
    drop(baseline_lock);
    let mut operation_counts = Vec::<(FsOperation, usize)>::new();
    for event in baseline_fs.events() {
        if let Some((_, count)) = operation_counts
            .iter_mut()
            .find(|(operation, _)| *operation == event.operation)
        {
            *count += 1;
        } else {
            operation_counts.push((event.operation, 1));
        }
    }

    for (operation, count) in operation_counts {
        for ordinal in 1..=count {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path();
            let fault_fs = Arc::new(FaultFs::fail_nth(operation, ordinal));

            let result = WriteLock::acquire_with(root, fault_fs);
            drop(result);

            assert!(
                std::panic::catch_unwind(|| assert_only_verified_coordination_residuals(root))
                    .is_ok(),
                "invalid bootstrap residual after {operation:?} {ordinal}"
            );
        }
    }
}

#[test]
fn persistent_bootstrap_faults_surface_and_the_next_writer_recovers() {
    let baseline = tempfile::tempdir().expect("baseline tempdir");
    let baseline_fs = Arc::new(FaultFs::passthrough());
    let baseline_lock =
        WriteLock::acquire_with(baseline.path(), baseline_fs.clone()).expect("baseline bootstrap");
    drop(baseline_lock);
    let mut operation_counts = Vec::<(FsOperation, usize)>::new();
    for event in baseline_fs.events() {
        if let Some((_, count)) = operation_counts
            .iter_mut()
            .find(|(operation, _)| *operation == event.operation)
        {
            *count += 1;
        } else {
            operation_counts.push((event.operation, 1));
        }
    }

    for (operation, count) in operation_counts {
        for ordinal in 1..=count {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path();
            let fault_fs = Arc::new(FaultFs::fail_from(operation, ordinal));

            let result = WriteLock::acquire_with(root, fault_fs);
            assert!(
                result.is_err(),
                "persistent {operation:?} fault at ordinal {ordinal} was masked"
            );
            drop(result);

            let recovered = WriteLock::acquire(root).unwrap_or_else(|error| {
                panic!(
                    "next writer failed to recover persistent {operation:?} fault at ordinal {ordinal}: {error}"
                )
            });
            drop(recovered);
            assert_only_verified_coordination_residuals(root);
            assert_exact_persistent_coordination(root);
        }
    }
}

#[test]
fn apply_rejects_legacy_write_lock_before_reading_invalid_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    setup_empty_project(root);
    write_kit_config(root, b"{\"tailwind\":true}\n");
    let lock_path = root.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("create lock parent");
    fs::write(&lock_path, b"locked\n").expect("write legacy lock");
    let identity = coordination_lock_identity(root);

    let error = apply_init(root).expect_err("legacy lock must win before invalid config parsing");

    assert!(matches!(
        error,
        CodegenError::LegacyWriteLock { ref path }
            if path == DEFAULT_KIT_WRITE_LOCK_PATH
    ));
    assert!(error.to_string().contains("remove the file manually"));
    assert_eq!(coordination_lock_identity(root), identity);
    assert_eq!(fs::read(lock_path).expect("read legacy lock"), b"locked\n");
    assert!(!root.join(DEFAULT_KIT_COORDINATION_IGNORE_PATH).exists());
}

#[test]
fn empty_preplanned_cohort_still_bootstraps_and_acquires() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fs = Arc::new(FaultFs::passthrough());

    apply_planned_files_with(dir.path(), &[], &[], fs.clone()).expect("empty apply");

    assert!(
        fs.events()
            .iter()
            .any(|event| event.operation == FsOperation::TryLock)
    );
    assert_exact_persistent_coordination(dir.path());
}

#[test]
fn planning_context_reuses_one_coherent_cached_observation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("index.html"), "first\n").expect("seed index");
    let context = PlanningContext::open(root).expect("open planning context");

    assert_eq!(
        context.read_string("index.html").expect("first read"),
        "first\n"
    );
    fs::write(root.join("index.html"), "second\n").expect("mutate index");
    assert_eq!(
        context.read_string("index.html").expect("cached read"),
        "first\n"
    );
    assert!(matches!(
        context.finish_snapshot().preimage("index.html"),
        Some(PathPreimage::RegularFile { content_hash, .. })
            if content_hash == &hash_content_bytes(b"first\n")
    ));
}

#[cfg(unix)]
#[test]
fn same_context_project_root_detachment_conflicts_at_every_revalidation_boundary() {
    for boundary in ["cohort", "final target"] {
        let parent = tempfile::tempdir().expect("root-detachment tempdir");
        let project = parent.path().join("project");
        let detached = parent.path().join("detached-project");
        fs::create_dir(&project).expect("create project root");
        fs::create_dir(project.join("styles")).expect("create styles");
        fs::write(project.join("styles/kit.css"), "before\n").expect("seed target");
        let context = PlanningContext::open(&project).expect("open planning context");
        context
            .observe_path("styles/kit.css")
            .expect("observe target");
        let snapshot = context.finish_snapshot();

        fs::rename(&project, &detached).expect("detach project root");
        fs::create_dir(&project).expect("create replacement project root");

        let result = if boundary == "cohort" {
            snapshot.revalidate_all(&context)
        } else {
            snapshot.revalidate_path(&context, "styles/kit.css")
        };

        assert!(
            matches!(result, Err(CodegenError::ProjectRootChanged { .. })),
            "{boundary} boundary must reject the detached project root: {result:?}"
        );
        assert_eq!(
            fs::read(detached.join("styles/kit.css")).expect("read detached target"),
            b"before\n"
        );
        assert!(!project.join("styles/kit.css").exists());
    }
}

#[cfg(unix)]
#[test]
fn same_context_project_root_alias_retarget_conflicts_at_every_revalidation_boundary() {
    for boundary in ["cohort", "final target"] {
        let parent = tempfile::tempdir().expect("root-alias-retarget tempdir");
        let first = parent.path().join("first");
        let second = parent.path().join("second");
        let alias = parent.path().join("project");
        for root in [&first, &second] {
            fs::create_dir(root).expect("create aliased project root");
            fs::create_dir(root.join("styles")).expect("create aliased styles directory");
        }
        fs::write(first.join("styles/kit.css"), "first\n").expect("seed first target");
        fs::write(second.join("styles/kit.css"), "second\n").expect("seed second target");
        std::os::unix::fs::symlink(&first, &alias).expect("create project-root alias");
        let context = PlanningContext::open(&alias).expect("open aliased planning context");
        context
            .observe_path("styles/kit.css")
            .expect("observe aliased target");
        let snapshot = context.finish_snapshot();

        fs::remove_file(&alias).expect("remove original project-root alias");
        std::os::unix::fs::symlink(&second, &alias).expect("retarget project-root alias");

        let result = if boundary == "cohort" {
            snapshot.revalidate_all(&context)
        } else {
            snapshot.revalidate_path(&context, "styles/kit.css")
        };

        assert!(
            matches!(result, Err(CodegenError::ProjectRootChanged { .. })),
            "{boundary} boundary must reject the retargeted project-root alias: {result:?}"
        );
        assert_eq!(
            fs::read(first.join("styles/kit.css")).expect("read first target"),
            b"first\n"
        );
        assert_eq!(
            fs::read(second.join("styles/kit.css")).expect("read second target"),
            b"second\n"
        );
    }
}

#[test]
fn same_context_expected_ancestor_detachment_conflicts_at_every_revalidation_boundary() {
    for boundary in ["cohort", "final target"] {
        let directory = tempfile::tempdir().expect("ancestor-detachment tempdir");
        let root = directory.path();
        fs::create_dir(root.join("styles")).expect("create styles");
        let context = PlanningContext::open(root).expect("open planning context");
        context
            .observe_path("styles/kit.css")
            .expect("observe absent target");
        let snapshot = context.finish_snapshot();

        fs::rename(root.join("styles"), root.join("detached-styles"))
            .expect("detach expected ancestor");

        let result = if boundary == "cohort" {
            snapshot.revalidate_all(&context)
        } else {
            snapshot.revalidate_path(&context, "styles/kit.css")
        };

        assert!(
            matches!(result, Err(CodegenError::PreimageConflict { .. })),
            "{boundary} boundary must reject the detached ancestor: {result:?}"
        );
        assert!(!root.join("styles").exists());
        assert!(!root.join("styles/kit.css").exists());
    }
}

#[test]
fn whole_cohort_preimage_conflict_aborts_before_target_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    setup_empty_project(root);
    let plan = plan_init(root).expect("plan init");
    fs::write(root.join("index.html"), "changed after planning\n").expect("race index");
    let fault_fs = Arc::new(FaultFs::passthrough());

    let error = apply_planned_files_with_snapshot(
        root,
        &plan.files,
        &plan.changes,
        &plan.snapshot,
        fault_fs.clone(),
    )
    .expect_err("stale cohort must conflict");

    assert!(
        matches!(error, CodegenError::PreimageConflict { ref path, .. } if path == "index.html")
    );
    assert!(
        !fault_fs
            .events()
            .iter()
            .any(|event| { event.operation == FsOperation::Rename })
    );
    assert!(!root.join(DEFAULT_KIT_CONFIG_PATH).exists());
}

#[test]
fn malformed_cohort_missing_a_later_preimage_aborts_before_target_transaction_io() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir(root.join("styles")).expect("styles");
    fs::write(root.join("styles/first.css"), "first-before\n").expect("seed first");
    let snapshot = capture_plan_snapshot(root, ["styles/first.css"]).expect("capture first only");
    let files = vec![
        PlannedFile {
            path: "styles/first.css".to_owned(),
            action: PlannedFileAction::Update,
            content: "first-planned\n".to_owned(),
        },
        PlannedFile {
            path: "styles/second.css".to_owned(),
            action: PlannedFileAction::Create,
            content: "second-planned\n".to_owned(),
        },
    ];
    let changes = vec![
        ChangeRecord::new(ChangeKind::UpdateFile, "styles/first.css", true),
        ChangeRecord::new(ChangeKind::CreateFile, "styles/second.css", true),
    ];
    let fault_fs = Arc::new(FaultFs::passthrough());

    let error =
        apply_planned_files_with_snapshot(root, &files, &changes, &snapshot, fault_fs.clone())
            .expect_err("missing second preimage must reject the whole cohort");

    assert!(
        matches!(error, CodegenError::PreimageConflict { ref path, .. } if path == "styles/second.css")
    );
    assert!(
        !fault_fs
            .events()
            .iter()
            .any(|event| { event.operation == FsOperation::Rename })
    );
    assert_eq!(
        fs::read(root.join("styles/first.css")).expect("first target"),
        b"first-before\n"
    );
    assert!(!root.join("styles/second.css").exists());
    assert_exact_persistent_coordination(root);
}

#[test]
fn exact_preimages_conflict_on_same_length_change_deletion_and_appearance() {
    for (case, initial, mutation) in [
        (
            "same-length content change",
            Some("aaaaa\n"),
            Some("bbbbb\n"),
        ),
        ("deletion", Some("before\n"), None),
        ("absent target appearance", None, Some("appeared\n")),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("styles")).expect("styles");
        let target = root.join("styles/kit.css");
        if let Some(initial) = initial {
            fs::write(&target, initial).expect("seed target");
        }
        let snapshot = capture_plan_snapshot(root, ["styles/kit.css"]).expect("capture snapshot");
        match mutation {
            Some(content) => fs::write(&target, content).expect("mutate target"),
            None => fs::remove_file(&target).expect("delete target"),
        }
        let files = vec![PlannedFile {
            path: "styles/kit.css".to_owned(),
            action: if initial.is_some() {
                PlannedFileAction::Update
            } else {
                PlannedFileAction::Create
            },
            content: "planned\n".to_owned(),
        }];
        let changes = vec![ChangeRecord::new(
            ChangeKind::UpdateFile,
            "styles/kit.css",
            true,
        )];
        let fault_fs = Arc::new(FaultFs::passthrough());

        let error =
            apply_planned_files_with_snapshot(root, &files, &changes, &snapshot, fault_fs.clone())
                .expect_err(case);

        assert!(
            matches!(error, CodegenError::PreimageConflict { .. }),
            "{case}: {error}"
        );
        assert!(
            !fault_fs
                .events()
                .iter()
                .any(|event| { event.operation == FsOperation::Rename }),
            "{case}"
        );
    }
}

#[test]
fn target_swap_after_cohort_validation_is_caught_before_rename() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");
    let target = root.join("styles/kit.css");
    fs::write(&target, "before\n").expect("seed target");
    let snapshot = capture_plan_snapshot(root, ["styles/kit.css"]).expect("capture snapshot");
    let files = vec![PlannedFile {
        path: "styles/kit.css".to_owned(),
        action: PlannedFileAction::Update,
        content: "planned\n".to_owned(),
    }];
    let changes = vec![ChangeRecord::new(
        ChangeKind::UpdateFile,
        "styles/kit.css",
        true,
    )];
    let fault_fs = Arc::new(FaultFs::mutate_before_final_revalidation(
        target.clone(),
        b"raced\n".to_vec(),
    ));

    let error =
        apply_planned_files_with_snapshot(root, &files, &changes, &snapshot, fault_fs.clone())
            .expect_err("final target swap must conflict");

    assert!(
        matches!(error, CodegenError::PreimageConflict { ref path, .. } if path == "styles/kit.css")
    );
    assert_eq!(fs::read(&target).expect("read raced target"), b"raced\n");
    assert!(
        !fault_fs
            .events()
            .iter()
            .any(|event| event.operation == FsOperation::Rename)
    );
}

#[test]
fn target_changes_after_final_revalidation_are_caught_before_rename() {
    for (case, initial) in [
        ("existing target mutation", Some("before\n")),
        ("absent target appearance", None),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("styles")).expect("styles");
        let target = root.join("styles/kit.css");
        if let Some(initial) = initial {
            fs::write(&target, initial).expect("seed target");
        }
        let snapshot = capture_plan_snapshot(root, ["styles/kit.css"]).expect("capture snapshot");
        let files = vec![PlannedFile {
            path: "styles/kit.css".to_owned(),
            action: if initial.is_some() {
                PlannedFileAction::Update
            } else {
                PlannedFileAction::Create
            },
            content: "planned\n".to_owned(),
        }];
        let changes = vec![ChangeRecord::new(
            if initial.is_some() {
                ChangeKind::UpdateFile
            } else {
                ChangeKind::CreateFile
            },
            "styles/kit.css",
            true,
        )];
        let fault_fs = Arc::new(FaultFs::mutate_after_final_revalidation(
            target.clone(),
            b"raced-after\n".to_vec(),
        ));

        let error =
            apply_planned_files_with_snapshot(root, &files, &changes, &snapshot, fault_fs.clone())
                .expect_err(case);

        assert!(
            matches!(error, CodegenError::PreimageConflict { ref path, .. } if path == "styles/kit.css"),
            "{case}: {error}"
        );
        assert_eq!(
            fs::read(&target).expect("read raced target"),
            b"raced-after\n",
            "{case}"
        );
        assert!(
            !fault_fs
                .events()
                .iter()
                .any(|event| event.operation == FsOperation::Rename),
            "{case}"
        );
    }
}

#[test]
fn expected_absent_publication_never_clobbers_a_last_moment_creator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");
    let target = root.join("styles/kit.css");
    let snapshot = capture_plan_snapshot(root, ["styles/kit.css"]).expect("capture absence");
    let files = vec![PlannedFile {
        path: "styles/kit.css".to_owned(),
        action: PlannedFileAction::Create,
        content: "planned\n".to_owned(),
    }];
    let changes = vec![ChangeRecord::new(
        ChangeKind::CreateFile,
        "styles/kit.css",
        true,
    )];
    let fault_fs = Arc::new(FaultFs::mutate_before_target_publication(
        target.clone(),
        b"raced-at-publication\n".to_vec(),
    ));

    let error =
        apply_planned_files_with_snapshot(root, &files, &changes, &snapshot, fault_fs.clone())
            .expect_err("no-clobber publication must lose to the creator");

    assert!(matches!(error, CodegenError::PreimageConflict { .. }));
    assert_eq!(
        fs::read(&target).expect("raced target"),
        b"raced-at-publication\n"
    );
    assert!(fault_fs.events().iter().any(|event| {
        event.operation == FsOperation::HardLink
            && event.destination.as_deref() == Some(target.as_path())
    }));
}

#[cfg(unix)]
#[test]
fn parent_swap_after_cohort_validation_is_caught_before_rename() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let root = dir.path();
    let parent = root.join("styles");
    fs::create_dir(&parent).expect("styles");
    let target = parent.join("kit.css");
    fs::write(&target, "before\n").expect("seed target");
    let outside_target = outside.path().join("kit.css");
    fs::write(&outside_target, "outside\n").expect("seed outside target");
    let snapshot = capture_plan_snapshot(root, ["styles/kit.css"]).expect("capture snapshot");
    let files = vec![PlannedFile {
        path: "styles/kit.css".to_owned(),
        action: PlannedFileAction::Update,
        content: "planned\n".to_owned(),
    }];
    let changes = vec![ChangeRecord::new(
        ChangeKind::UpdateFile,
        "styles/kit.css",
        true,
    )];
    let fault_fs = Arc::new(FaultFs::replace_parent_before_final_revalidation(
        target,
        parent,
        root.join("styles-before-swap"),
        outside.path().to_path_buf(),
    ));

    let error =
        apply_planned_files_with_snapshot(root, &files, &changes, &snapshot, fault_fs.clone())
            .expect_err("final parent swap must conflict");

    assert!(matches!(error, CodegenError::PreimageConflict { .. }));
    assert_eq!(
        fs::read(outside_target).expect("outside target"),
        b"outside\n"
    );
    assert!(
        !fault_fs
            .events()
            .iter()
            .any(|event| event.operation == FsOperation::Rename)
    );
}

#[cfg(unix)]
#[test]
fn parent_swap_after_final_revalidation_is_caught_before_rename() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let root = dir.path();
    let parent = root.join("styles");
    fs::create_dir(&parent).expect("styles");
    let target = parent.join("kit.css");
    fs::write(&target, "before\n").expect("seed target");
    let outside_target = outside.path().join("kit.css");
    fs::write(&outside_target, "outside\n").expect("seed outside target");
    let snapshot = capture_plan_snapshot(root, ["styles/kit.css"]).expect("capture snapshot");
    let files = vec![PlannedFile {
        path: "styles/kit.css".to_owned(),
        action: PlannedFileAction::Update,
        content: "planned\n".to_owned(),
    }];
    let changes = vec![ChangeRecord::new(
        ChangeKind::UpdateFile,
        "styles/kit.css",
        true,
    )];
    let fault_fs = Arc::new(FaultFs::replace_parent_after_final_revalidation(
        target,
        parent,
        root.join("styles-after-validation"),
        outside.path().to_path_buf(),
    ));

    let error =
        apply_planned_files_with_snapshot(root, &files, &changes, &snapshot, fault_fs.clone())
            .expect_err("post-validation parent swap must conflict");

    assert!(matches!(
        error,
        CodegenError::PreimageConflict { .. } | CodegenError::UnsafePath { .. }
    ));
    assert_eq!(
        fs::read(outside_target).expect("outside target"),
        b"outside\n"
    );
    assert!(
        !fault_fs
            .events()
            .iter()
            .any(|event| event.operation == FsOperation::Rename)
    );
}

#[cfg(unix)]
#[test]
fn newly_created_parent_identity_is_bound_through_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir(root.join("styles")).expect("styles");
    let logical_path = "styles/nested/kit.css";
    let target = root.join(logical_path);
    let snapshot = capture_plan_snapshot(root, [logical_path]).expect("capture absent target");
    let files = vec![PlannedFile {
        path: logical_path.to_owned(),
        action: PlannedFileAction::Create,
        content: "planned\n".to_owned(),
    }];
    let changes = vec![ChangeRecord::new(
        ChangeKind::CreateFile,
        logical_path,
        true,
    )];
    let parent = root.join("styles/nested");
    let moved_parent = root.join("styles/staged-parent");
    let fault_fs = Arc::new(
        FaultFs::replace_parent_with_directory_after_final_revalidation(
            target,
            parent.clone(),
            moved_parent.clone(),
        ),
    );

    let error =
        apply_planned_files_with_snapshot(root, &files, &changes, &snapshot, fault_fs.clone())
            .expect_err("replacement parent with an ordinary directory must conflict");

    assert!(matches!(error, CodegenError::RecoveryRequired { .. }));
    assert!(parent.is_dir(), "substituted parent is preserved");
    assert!(!parent.join("kit.css").exists());
    let staged_paths = fs::read_dir(&moved_parent)
        .expect("read detached stage parent")
        .map(|entry| entry.expect("stage entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".leptos-ui-kit-stage-"))
        })
        .collect::<Vec<_>>();
    assert!(staged_paths.is_empty(), "rollback removes detached stages");
    assert!(
        !fault_fs
            .events()
            .iter()
            .any(|event| event.operation == FsOperation::Rename)
    );
}

#[cfg(unix)]
#[test]
fn mode_only_preimage_change_conflicts_before_target_writes() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");
    let target = root.join("styles/kit.css");
    fs::write(&target, "before\n").expect("seed target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).expect("seed mode");
    let snapshot = capture_plan_snapshot(root, ["styles/kit.css"]).expect("capture snapshot");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("change mode");
    let files = vec![PlannedFile {
        path: "styles/kit.css".to_owned(),
        action: PlannedFileAction::Update,
        content: "planned\n".to_owned(),
    }];
    let changes = vec![ChangeRecord::new(
        ChangeKind::UpdateFile,
        "styles/kit.css",
        true,
    )];
    let fault_fs = Arc::new(FaultFs::passthrough());

    let error =
        apply_planned_files_with_snapshot(root, &files, &changes, &snapshot, fault_fs.clone())
            .expect_err("mode-only change must conflict");

    assert!(matches!(error, CodegenError::PreimageConflict { .. }));
    assert!(
        !fault_fs
            .events()
            .iter()
            .any(|event| { event.operation == FsOperation::Rename })
    );
}

#[cfg(unix)]
#[test]
fn retargeted_project_alias_conflicts_after_coordination_but_before_target_io() {
    let parent = tempfile::tempdir().expect("tempdir");
    let first = parent.path().join("first");
    let second = parent.path().join("second");
    let alias = parent.path().join("alias");
    fs::create_dir(&first).expect("first root");
    fs::create_dir(&second).expect("second root");
    fs::create_dir_all(first.join("styles")).expect("first styles");
    fs::write(first.join("styles/kit.css"), "first\n").expect("first target");
    fs::create_dir_all(second.join("styles")).expect("second styles");
    fs::write(second.join("styles/kit.css"), "second\n").expect("second target");
    std::os::unix::fs::symlink(&first, &alias).expect("create alias");
    let snapshot = capture_plan_snapshot(&alias, ["styles/kit.css"]).expect("capture snapshot");
    fs::remove_file(&alias).expect("remove alias");
    std::os::unix::fs::symlink(&second, &alias).expect("retarget alias");
    let files = vec![PlannedFile {
        path: "styles/kit.css".to_owned(),
        action: PlannedFileAction::Update,
        content: "planned\n".to_owned(),
    }];
    let changes = vec![ChangeRecord::new(
        ChangeKind::UpdateFile,
        "styles/kit.css",
        true,
    )];
    let fault_fs = Arc::new(FaultFs::passthrough());

    let error =
        apply_planned_files_with_snapshot(&alias, &files, &changes, &snapshot, fault_fs.clone())
            .expect_err("retargeted root must conflict");

    assert!(matches!(error, CodegenError::ProjectRootChanged { .. }));
    assert!(
        !fault_fs
            .events()
            .iter()
            .any(|event| { event.operation == FsOperation::Rename })
    );
    assert_eq!(
        fs::read(second.join("styles/kit.css")).expect("second target"),
        b"second\n"
    );
    assert_exact_persistent_coordination(&second);
}

#[test]
fn transaction_stages_the_complete_sorted_cohort_and_commits_the_install_lock_last() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");
    let files = vec![
        PlannedFile {
            path: DEFAULT_KIT_LOCK_PATH.to_owned(),
            action: PlannedFileAction::Create,
            content: "lock\n".to_owned(),
        },
        PlannedFile {
            path: "styles/first.css".to_owned(),
            action: PlannedFileAction::Create,
            content: "first\n".to_owned(),
        },
        PlannedFile {
            path: "styles/second.css".to_owned(),
            action: PlannedFileAction::Create,
            content: "second\n".to_owned(),
        },
    ];
    let changes = vec![
        ChangeRecord::new(ChangeKind::WriteLockFile, DEFAULT_KIT_LOCK_PATH, true),
        ChangeRecord::new(ChangeKind::CreateFile, "styles/first.css", true),
        ChangeRecord::new(ChangeKind::CreateFile, "styles/second.css", true),
    ];
    let fault_fs = Arc::new(FaultFs::passthrough());

    apply_planned_files_with(root, &files, &changes, fault_fs.clone()).expect("apply cohort");

    let events = fault_fs.events();
    let last_stage = events
        .iter()
        .rposition(|event| {
            event.operation == FsOperation::CreateNewFile
                && event
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".leptos-ui-kit-stage-"))
        })
        .expect("complete cohort staging");
    let first_publication = events
        .iter()
        .position(|event| {
            event.operation == FsOperation::HardLink
                && event
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".leptos-ui-kit-stage-"))
        })
        .expect("first no-clobber publication");
    assert!(last_stage < first_publication);
    let publications = events
        .into_iter()
        .filter(|event| {
            event.operation == FsOperation::HardLink
                && event
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".leptos-ui-kit-stage-"))
        })
        .collect::<Vec<FsEvent>>();
    assert_eq!(publications.len(), 3);
    assert_eq!(
        publications[0].destination.as_deref(),
        Some(root.join("styles/first.css").as_path())
    );
    assert_eq!(
        publications[1].destination.as_deref(),
        Some(root.join("styles/second.css").as_path())
    );
    assert_eq!(
        publications[2].destination.as_deref(),
        Some(root.join(DEFAULT_KIT_LOCK_PATH).as_path())
    );
}

#[test]
fn action_and_install_lock_mismatches_fail_before_transaction_preparation() {
    for case in ["action", "duplicate-lock", "missing-lock", "stale-lock"] {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("styles")).expect("styles");
        let target_path = if case == "missing-lock" {
            DEFAULT_KIT_LOCK_PATH
        } else {
            "styles/kit.css"
        };
        let files = vec![PlannedFile {
            path: target_path.to_owned(),
            action: if case == "action" {
                PlannedFileAction::Update
            } else {
                PlannedFileAction::Create
            },
            content: "planned\n".to_owned(),
        }];
        let changes = match case {
            "action" => vec![ChangeRecord::new(
                ChangeKind::CreateFile,
                "styles/kit.css",
                true,
            )],
            "duplicate-lock" => vec![
                ChangeRecord::new(ChangeKind::WriteLockFile, "styles/kit.css", true),
                ChangeRecord::new(ChangeKind::WriteLockFile, "styles/kit.css", true),
            ],
            "missing-lock" => vec![ChangeRecord::new(
                ChangeKind::CreateFile,
                DEFAULT_KIT_LOCK_PATH,
                true,
            )],
            "stale-lock" => vec![ChangeRecord::new(
                ChangeKind::WriteLockFile,
                "styles/missing.lock",
                true,
            )],
            _ => unreachable!("known case"),
        };

        let error =
            apply_planned_files_with(root, &files, &changes, Arc::new(FaultFs::passthrough()))
                .expect_err(case);

        assert!(
            matches!(error, CodegenError::PreimageConflict { .. }),
            "{case}: {error}"
        );
        assert!(!root.join(target_path).exists(), "{case}");
        assert!(
            !root.join("src/components/ui/_kit/.transactions").exists(),
            "{case}"
        );
    }
}

#[test]
fn atomic_write_ignores_a_predictable_legacy_temporary_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");
    let predictable = root.join("styles/kit.leptos-ui-kit.tmp");
    fs::write(&predictable, b"preexisting\n").expect("seed predictable path");

    write_file_atomic(root, "styles/kit.css", b"replacement\n").expect("atomic write");

    assert_eq!(
        fs::read(root.join("styles/kit.css")).expect("read target"),
        b"replacement\n"
    );
    assert_eq!(
        fs::read(predictable).expect("legacy temporary remains application-owned"),
        b"preexisting\n"
    );
}

#[test]
fn atomic_write_preserves_arbitrary_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");
    let bytes = [0, 0xff, b'\n'];

    write_file_atomic(root, "styles/data.css", &bytes).expect("atomic byte write");

    assert_eq!(
        fs::read(root.join("styles/data.css")).expect("read bytes"),
        bytes
    );
}

#[test]
fn existing_targets_are_backed_up_before_the_first_commit_and_cleaned_after_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");
    fs::write(root.join("styles/first.css"), b"first-before\n").expect("seed first");
    fs::write(root.join("styles/second.css"), b"second-before\n").expect("seed second");
    let files = vec![
        PlannedFile {
            path: "styles/second.css".to_owned(),
            action: PlannedFileAction::Update,
            content: "second-after\n".to_owned(),
        },
        PlannedFile {
            path: "styles/first.css".to_owned(),
            action: PlannedFileAction::Update,
            content: "first-after\n".to_owned(),
        },
    ];
    let changes = vec![
        ChangeRecord::new(ChangeKind::UpdateFile, "styles/second.css", true),
        ChangeRecord::new(ChangeKind::UpdateFile, "styles/first.css", true),
    ];
    let fault_fs = Arc::new(FaultFs::passthrough());

    apply_planned_files_with(root, &files, &changes, fault_fs.clone()).expect("apply updates");

    let events = fault_fs.events();
    let backup_creations = events
        .iter()
        .filter(|event| {
            event.operation == FsOperation::CreateNewFile
                && event
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".leptos-ui-kit-backup-"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        backup_creations.len(),
        2,
        "one independent backup per target"
    );
    let last_backup = events
        .iter()
        .rposition(|event| {
            event.operation == FsOperation::CreateNewFile
                && event
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".leptos-ui-kit-backup-"))
        })
        .expect("independent backup creation");
    let first_rename = events
        .iter()
        .position(|event| event.operation == FsOperation::Rename)
        .expect("first rename");
    assert!(last_backup < first_rename);
    assert!(!events.iter().any(|event| {
        event.operation == FsOperation::HardLink
            && event.destination.as_ref().is_some_and(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".leptos-ui-kit-backup-"))
            })
    }));
    assert_eq!(
        events[first_rename].destination.as_deref(),
        Some(root.join("styles/first.css").as_path())
    );
    assert_eq!(
        fs::read(root.join("styles/first.css")).expect("first result"),
        b"first-after\n"
    );
    assert_eq!(
        fs::read(root.join("styles/second.css")).expect("second result"),
        b"second-after\n"
    );
    assert!(
        !fs::read_dir(root.join("styles"))
            .expect("styles entries")
            .any(|entry| {
                entry
                    .expect("style entry")
                    .file_name()
                    .to_str()
                    .is_some_and(|name| {
                        name.starts_with(".leptos-ui-kit-stage-")
                            || name.starts_with(".leptos-ui-kit-backup-")
                    })
            })
    );
}

#[test]
fn absent_publication_failure_rolls_back_the_complete_cohort_and_removes_transaction_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");
    let files = vec![
        PlannedFile {
            path: "styles/first.css".to_owned(),
            action: PlannedFileAction::Create,
            content: "first\n".to_owned(),
        },
        PlannedFile {
            path: "styles/second.css".to_owned(),
            action: PlannedFileAction::Create,
            content: "second\n".to_owned(),
        },
    ];
    let changes = vec![
        ChangeRecord::new(ChangeKind::CreateFile, "styles/first.css", true),
        ChangeRecord::new(ChangeKind::CreateFile, "styles/second.css", true),
    ];
    let fault_fs = Arc::new(FaultFs::fail_nth(FsOperation::HardLink, 4));

    let error = apply_planned_files_with(root, &files, &changes, fault_fs)
        .expect_err("second no-clobber publication must fail");

    assert!(matches!(
        error,
        CodegenError::FilesystemOperation {
            operation: "publish absent target without clobber",
            ..
        }
    ));
    assert!(!root.join("styles/first.css").exists());
    assert!(!root.join("styles/second.css").exists());
    let stages = fs::read_dir(root.join("styles"))
        .expect("read stage parent")
        .map(|entry| entry.expect("stage entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".leptos-ui-kit-stage-"))
        })
        .collect::<Vec<_>>();
    assert!(stages.is_empty());
    assert!(!root.join("src/components/ui/_kit/.transactions").exists());
    assert_exact_persistent_coordination(root);
}

#[test]
fn failed_rollback_retains_a_strict_recovery_required_journal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");
    fs::write(root.join("styles/first.css"), b"first-before\n").expect("seed first");
    fs::write(root.join("styles/second.css"), b"second-before\n").expect("seed second");
    let files = vec![
        PlannedFile {
            path: "styles/first.css".to_owned(),
            action: PlannedFileAction::Update,
            content: "first-after\n".to_owned(),
        },
        PlannedFile {
            path: "styles/second.css".to_owned(),
            action: PlannedFileAction::Update,
            content: "second-after\n".to_owned(),
        },
    ];
    let changes = vec![
        ChangeRecord::new(ChangeKind::UpdateFile, "styles/first.css", true),
        ChangeRecord::new(ChangeKind::UpdateFile, "styles/second.css", true),
    ];
    let fault_fs = Arc::new(FaultFs::fail_from(FsOperation::Rename, 2));

    let error = apply_planned_files_with(root, &files, &changes, fault_fs)
        .expect_err("commit and rollback renames must fail persistently");

    assert!(matches!(error, CodegenError::RecoveryRequired { .. }));
    assert_eq!(
        fs::read(root.join("styles/first.css")).expect("first target"),
        b"first-after\n"
    );
    assert_eq!(
        fs::read(root.join("styles/second.css")).expect("second target"),
        b"second-before\n"
    );
    let transactions = root.join("src/components/ui/_kit/.transactions");
    let journals = fs::read_dir(&transactions)
        .expect("recovery directory")
        .map(|entry| entry.expect("journal entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("transaction-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    assert_eq!(journals.len(), 1);
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&journals[0]).expect("read journal"))
            .expect("strict valid journal JSON");
    assert_eq!(
        value.get("version").and_then(|value| value.as_u64()),
        Some(2)
    );
    assert_eq!(
        value
            .pointer("/state/kind")
            .and_then(|value| value.as_str()),
        Some("rollingBack")
    );
    assert_eq!(
        value
            .get("entries")
            .and_then(|value| value.as_array())
            .map(Vec::len),
        Some(2)
    );
    let transaction_id = value
        .get("transactionId")
        .and_then(serde_json::Value::as_str)
        .expect("transaction identifier");
    let first_entry = value
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| entries.first())
        .expect("first journal entry");
    assert_eq!(
        first_entry
            .get("stageName")
            .and_then(serde_json::Value::as_str),
        Some(format!(".leptos-ui-kit-stage-{transaction_id}-00000000").as_str())
    );
    let backup_name = first_entry
        .get("backupName")
        .and_then(serde_json::Value::as_str)
        .expect("transaction-bound backup name");
    assert_eq!(
        backup_name,
        format!(".leptos-ui-kit-backup-{transaction_id}-00000000")
    );
    assert_ne!(
        coordination_file_identity(&root.join("styles/first.css")),
        coordination_file_identity(&root.join("styles").join(backup_name)),
        "recovery backup must be an independent inode"
    );
    let recovered = WriteLock::acquire(root).expect("next writer recovers durable transaction");
    drop(recovered);
    assert_eq!(
        fs::read(root.join("styles/first.css")).expect("recovered first target"),
        b"first-before\n"
    );
    assert_eq!(
        fs::read(root.join("styles/second.css")).expect("recovered second target"),
        b"second-before\n"
    );
    assert!(!transactions.exists());
    assert_exact_persistent_coordination(root);
}

#[test]
fn committed_cleanup_failure_is_finish_only_and_preserves_desired_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("styles")).expect("styles");
    fs::write(root.join("styles/kit.css"), b"before\n").expect("seed target");
    let bootstrap = WriteLock::acquire(root).expect("bootstrap coordination");
    drop(bootstrap);
    let files = vec![PlannedFile {
        path: "styles/kit.css".to_owned(),
        action: PlannedFileAction::Update,
        content: "after\n".to_owned(),
    }];
    let changes = vec![ChangeRecord::new(
        ChangeKind::UpdateFile,
        "styles/kit.css",
        true,
    )];

    let error = apply_planned_files_with(
        root,
        &files,
        &changes,
        Arc::new(FaultFs::fail_nth(FsOperation::RemoveFile, 2)),
    )
    .expect_err("committed cleanup failure");

    assert!(matches!(error, CodegenError::RecoveryRequired { .. }));
    assert_eq!(
        fs::read(root.join("styles/kit.css")).expect("desired target"),
        b"after\n"
    );
    let journal = transaction_journal_paths(root)
        .pop()
        .expect("committed journal");
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(journal).expect("journal bytes")).expect("journal JSON");
    assert_eq!(
        value
            .pointer("/state/kind")
            .and_then(serde_json::Value::as_str),
        Some("applied")
    );

    let recovered = WriteLock::acquire(root).expect("finish-only recovery");
    drop(recovered);
    assert_eq!(
        fs::read(root.join("styles/kit.css")).expect("recovered target"),
        b"after\n"
    );
    assert!(transaction_journal_paths(root).is_empty());
}

#[test]
fn rollback_removes_every_transaction_created_target_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let files = vec![
        PlannedFile {
            path: "src/components/ui/generated/first.rs".to_owned(),
            action: PlannedFileAction::Create,
            content: "pub struct First;\n".to_owned(),
        },
        PlannedFile {
            path: "src/components/ui/generated/nested/second.rs".to_owned(),
            action: PlannedFileAction::Create,
            content: "pub struct Second;\n".to_owned(),
        },
    ];
    let changes = vec![
        ChangeRecord::new(
            ChangeKind::CreateFile,
            "src/components/ui/generated/first.rs",
            true,
        ),
        ChangeRecord::new(
            ChangeKind::CreateFile,
            "src/components/ui/generated/nested/second.rs",
            true,
        ),
    ];
    let fault_fs = Arc::new(FaultFs::fail_nth(FsOperation::HardLink, 4));

    let error = apply_planned_files_with(root, &files, &changes, fault_fs)
        .expect_err("second no-clobber publication fails");

    assert!(matches!(
        error,
        CodegenError::FilesystemOperation {
            operation: "publish absent target without clobber",
            ..
        }
    ));
    assert!(!root.join("src/components/ui/generated").exists());
    assert!(!root.join("src/components/ui/_kit/.transactions").exists());
    assert_exact_persistent_coordination(root);
}

#[test]
fn third_state_application_edits_block_recovery_without_mutating_evidence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let journal = seed_failed_rollback_journal(root);
    fs::write(root.join("styles/first.css"), b"application edit\n").expect("write third state");
    let before = snapshot_project_files(root);

    let error = WriteLock::acquire(root).expect_err("third state blocks recovery");

    assert!(matches!(error, CodegenError::RecoveryRequired { .. }));
    assert_eq!(snapshot_project_files(root), before);
    assert_eq!(
        fs::read(root.join("styles/first.css")).expect("third-state target"),
        b"application edit\n"
    );
    assert!(journal.exists());
}

#[test]
fn invalid_wrong_project_and_unsupported_journals_block_without_mutation() {
    for case in ["corrupt", "wrong-project", "unsupported", "unknown-field"] {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let journal = seed_failed_rollback_journal(root);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&journal).expect("read journal"))
                .expect("parse seeded journal");
        match case {
            "corrupt" => fs::write(&journal, b"not json\n").expect("corrupt journal"),
            "wrong-project" => {
                let device = value
                    .pointer_mut("/project/device")
                    .expect("project device");
                *device =
                    serde_json::Value::from(device.as_u64().expect("numeric project device") + 1);
                fs::write(
                    &journal,
                    serde_json::to_vec_pretty(&value).expect("serialize wrong project"),
                )
                .expect("write wrong project");
            }
            "unsupported" => {
                value["version"] = serde_json::Value::from(1);
                fs::write(
                    &journal,
                    serde_json::to_vec_pretty(&value).expect("serialize version"),
                )
                .expect("write version");
            }
            "unknown-field" => {
                value["unexpected"] = serde_json::Value::Bool(true);
                fs::write(
                    &journal,
                    serde_json::to_vec_pretty(&value).expect("serialize unknown field"),
                )
                .expect("write unknown field");
            }
            _ => unreachable!("known invalid-journal case"),
        }
        let before = snapshot_project_files(root);

        let error = WriteLock::acquire(root).expect_err(case);

        assert!(matches!(
            error,
            CodegenError::InvalidCoordinationState { .. }
        ));
        assert_eq!(snapshot_project_files(root), before, "{case}");
    }
}

#[test]
fn dry_run_reports_pending_recovery_without_mutating_the_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    seed_failed_rollback_journal(root);
    let before = snapshot_project_files(root);

    let error = plan_init(root).expect_err("dry run reports pending recovery");

    assert!(matches!(error, CodegenError::RecoveryRequired { .. }));
    assert_eq!(snapshot_project_files(root), before);
}

fn seed_failed_rollback_journal(root: &Path) -> PathBuf {
    let (files, changes) = setup_two_file_update(root);
    let fault_fs = Arc::new(FaultFs::fail_from(FsOperation::Rename, 2));
    let error = apply_planned_files_with(root, &files, &changes, fault_fs)
        .expect_err("seed failed rollback");
    assert!(matches!(error, CodegenError::RecoveryRequired { .. }));
    let journals = transaction_journal_paths(root);
    assert_eq!(journals.len(), 1);
    journals.into_iter().next().expect("journal path")
}

#[test]
fn every_transaction_io_fault_avoids_partial_application_state() {
    let baseline = tempfile::tempdir().expect("baseline tempdir");
    let (files, changes) = setup_two_file_update(baseline.path());
    let baseline_fs = Arc::new(FaultFs::passthrough());
    apply_planned_files_with(baseline.path(), &files, &changes, baseline_fs.clone())
        .expect("baseline transaction");
    let operations = [
        FsOperation::CreateDirectory,
        FsOperation::CreateNewFile,
        FsOperation::WriteHandle,
        FsOperation::SetFileMode,
        FsOperation::SyncHandle,
        FsOperation::SyncDirectory,
        FsOperation::HardLink,
        FsOperation::BeforeFinalRevalidation,
        FsOperation::AfterFinalRevalidation,
        FsOperation::BeforeTargetPublication,
        FsOperation::RenameJournal,
        FsOperation::Rename,
        FsOperation::RemoveFile,
        FsOperation::RemoveDirectory,
    ];

    for operation in operations {
        let count = baseline_fs
            .events()
            .iter()
            .filter(|event| event.operation == operation)
            .count();
        for ordinal in 1..=count {
            let directory = tempfile::tempdir().expect("fault tempdir");
            let root = directory.path();
            let (files, changes) = setup_two_file_update(root);
            let fault_fs = Arc::new(FaultFs::fail_nth(operation, ordinal));

            let result = apply_planned_files_with(root, &files, &changes, fault_fs);

            let first = fs::read(root.join("styles/first.css")).expect("first target");
            let second = fs::read(root.join("styles/second.css")).expect("second target");
            let exact_before = first == b"first-before\n" && second == b"second-before\n";
            let exact_after = first == b"first-after\n" && second == b"second-after\n";
            assert!(
                exact_before || exact_after,
                "partial state after {operation:?} fault {ordinal}: {result:?}"
            );
            if exact_before {
                assert!(result.is_err(), "rolled-back fault must surface");
            }
            for journal in transaction_journal_paths(root) {
                let _: serde_json::Value =
                    serde_json::from_slice(&fs::read(&journal).expect("read retained journal"))
                        .unwrap_or_else(|error| {
                            panic!(
                                "invalid retained journal after {operation:?} {ordinal}: {error}"
                            )
                        });
            }
        }
    }
}

#[test]
fn killed_writers_recover_after_every_transaction_mutation_boundary() {
    let baseline = tempfile::tempdir().expect("baseline tempdir");
    let (files, changes) = setup_two_file_update(baseline.path());
    let lock = WriteLock::acquire(baseline.path()).expect("bootstrap baseline coordination");
    drop(lock);
    let baseline_fs = Arc::new(FaultFs::passthrough());
    apply_planned_files_with(baseline.path(), &files, &changes, baseline_fs.clone())
        .expect("baseline transaction");
    let operations = [
        FsOperation::CreateDirectory,
        FsOperation::CreateNewFile,
        FsOperation::SetFileMode,
        FsOperation::SetDirectoryMode,
        FsOperation::WriteHandle,
        FsOperation::SyncHandle,
        FsOperation::SyncDirectory,
        FsOperation::HardLink,
        FsOperation::RenameJournal,
        FsOperation::Rename,
        FsOperation::RemoveFile,
        FsOperation::RemoveDirectory,
    ];
    let mut barriers = 0;

    for operation in operations {
        let count = baseline_fs
            .events()
            .iter()
            .filter(|event| event.operation == operation)
            .count();
        for ordinal in 1..=count {
            barriers += 1;
            let directory = tempfile::tempdir().expect("crash tempdir");
            let root = directory.path();
            let control = root.join("crash-control");
            fs::create_dir(&control).expect("create crash control directory");
            setup_two_file_update(root);
            let lock = WriteLock::acquire(root).expect("bootstrap crash coordination");
            drop(lock);
            let mut worker = spawn_transaction_crash_worker(operation, ordinal, root, &control);
            let barrier_path = control.join("transaction-crash-ready");
            wait_for_worker_path(&barrier_path, &mut worker);
            let mutation_path = fs::read_to_string(&barrier_path).expect("read mutation path");
            worker.kill_and_wait();

            let recovered = WriteLock::acquire(root).unwrap_or_else(|error| {
                panic!(
                    "fresh writer failed after {operation:?} mutation {ordinal} at {mutation_path}: {error}"
                )
            });
            drop(recovered);
            let first = fs::read(root.join("styles/first.css")).expect("first target");
            let second = fs::read(root.join("styles/second.css")).expect("second target");
            let exact_before = first == b"first-before\n" && second == b"second-before\n";
            let exact_after = first == b"first-after\n" && second == b"second-after\n";
            assert!(
                exact_before || exact_after,
                "partial state after {operation:?} mutation {ordinal}"
            );
            assert!(
                transaction_journal_paths(root).is_empty(),
                "retained journal after recovering {operation:?} mutation {ordinal}"
            );
            assert_exact_persistent_coordination(root);
        }
    }

    assert!(
        barriers >= 20,
        "expected a complete transaction barrier matrix"
    );
}

#[test]
fn transaction_io_errors_name_the_operation_and_logical_project_path() {
    for (operation, ordinal, expected_operation) in [
        (FsOperation::WriteHandle, 3, "write transaction stage"),
        (FsOperation::Rename, 1, "replace target"),
    ] {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        fs::create_dir_all(root.join("styles")).expect("styles");
        fs::write(root.join("styles/kit.css"), b"before\n").expect("seed target");
        let lock = WriteLock::acquire(root).expect("bootstrap coordination");
        drop(lock);
        let files = vec![PlannedFile {
            path: "styles/kit.css".to_owned(),
            action: PlannedFileAction::Update,
            content: "after\n".to_owned(),
        }];
        let changes = vec![ChangeRecord::new(
            ChangeKind::UpdateFile,
            "styles/kit.css",
            true,
        )];
        let fault_fs = Arc::new(FaultFs::fail_nth(operation, ordinal));

        let error = apply_planned_files_with(root, &files, &changes, fault_fs)
            .expect_err(expected_operation);

        assert!(matches!(
            error,
            CodegenError::FilesystemOperation {
                operation: actual_operation,
                logical_path,
                ..
            } if actual_operation == expected_operation && logical_path == "styles/kit.css"
        ));
        assert_eq!(
            fs::read(root.join("styles/kit.css")).expect("unchanged target"),
            b"before\n"
        );
    }
}

fn setup_two_file_update(root: &Path) -> (Vec<PlannedFile>, Vec<ChangeRecord>) {
    fs::create_dir_all(root.join("styles")).expect("styles");
    fs::write(root.join("styles/first.css"), b"first-before\n").expect("seed first");
    fs::write(root.join("styles/second.css"), b"second-before\n").expect("seed second");
    two_file_update_plan()
}

fn two_file_update_plan() -> (Vec<PlannedFile>, Vec<ChangeRecord>) {
    (
        vec![
            PlannedFile {
                path: "styles/first.css".to_owned(),
                action: PlannedFileAction::Update,
                content: "first-after\n".to_owned(),
            },
            PlannedFile {
                path: "styles/second.css".to_owned(),
                action: PlannedFileAction::Update,
                content: "second-after\n".to_owned(),
            },
        ],
        vec![
            ChangeRecord::new(ChangeKind::UpdateFile, "styles/first.css", true),
            ChangeRecord::new(ChangeKind::UpdateFile, "styles/second.css", true),
        ],
    )
}

fn transaction_crash_operation_name(operation: FsOperation) -> &'static str {
    match operation {
        FsOperation::CreateDirectory => "create-directory",
        FsOperation::CreateNewFile => "create-new-file",
        FsOperation::SetFileMode => "set-file-mode",
        FsOperation::SetDirectoryMode => "set-directory-mode",
        FsOperation::WriteHandle => "write-handle",
        FsOperation::SyncHandle => "sync-handle",
        FsOperation::SyncDirectory => "sync-directory",
        FsOperation::HardLink => "hard-link",
        FsOperation::RemoveFile => "remove-file",
        FsOperation::RemoveDirectory => "remove-directory",
        FsOperation::Rename => "rename",
        FsOperation::RenameJournal => "rename-journal",
        other => panic!("unsupported transaction crash operation {other:?}"),
    }
}

fn parse_transaction_crash_operation(value: &str) -> FsOperation {
    match value {
        "create-directory" => FsOperation::CreateDirectory,
        "create-new-file" => FsOperation::CreateNewFile,
        "set-file-mode" => FsOperation::SetFileMode,
        "set-directory-mode" => FsOperation::SetDirectoryMode,
        "write-handle" => FsOperation::WriteHandle,
        "sync-handle" => FsOperation::SyncHandle,
        "sync-directory" => FsOperation::SyncDirectory,
        "hard-link" => FsOperation::HardLink,
        "remove-file" => FsOperation::RemoveFile,
        "remove-directory" => FsOperation::RemoveDirectory,
        "rename" => FsOperation::Rename,
        "rename-journal" => FsOperation::RenameJournal,
        other => panic!("unknown transaction crash operation {other}"),
    }
}

fn transaction_journal_paths(root: &Path) -> Vec<PathBuf> {
    let directory = root.join("src/components/ui/_kit/.transactions");
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .map(|entry| entry.expect("transaction entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("transaction-") && name.ends_with(".json"))
        })
        .collect()
}

fn assert_exact_persistent_coordination(root: &Path) {
    assert_eq!(
        fs::read(root.join(DEFAULT_KIT_WRITE_LOCK_PATH)).expect("read advisory lock"),
        KIT_ADVISORY_LOCK_CONTENT
    );
    assert_eq!(
        fs::read(root.join(DEFAULT_KIT_COORDINATION_IGNORE_PATH))
            .expect("read coordination ignore"),
        KIT_COORDINATION_IGNORE_CONTENT
    );
}

fn spawn_lock_stage_worker(role: &str, project: &Path, control: &Path) -> LockStageWorker {
    let child = Command::new(env::current_exe().expect("current unit-test executable"))
        .args([
            "--exact",
            "tests::transaction_lock_stage_worker",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(LOCK_STAGE_ROLE_ENV, role)
        .env(LOCK_STAGE_PROJECT_ENV, project)
        .env(LOCK_STAGE_CONTROL_ENV, control)
        .spawn()
        .expect("spawn lock-stage worker");
    LockStageWorker { child: Some(child) }
}

fn spawn_transaction_crash_worker(
    operation: FsOperation,
    ordinal: usize,
    project: &Path,
    control: &Path,
) -> LockStageWorker {
    let child = Command::new(env::current_exe().expect("current unit-test executable"))
        .args([
            "--exact",
            "tests::transaction_lock_stage_worker",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(LOCK_STAGE_ROLE_ENV, "transaction-crash")
        .env(LOCK_STAGE_PROJECT_ENV, project)
        .env(LOCK_STAGE_CONTROL_ENV, control)
        .env(
            TRANSACTION_CRASH_OPERATION_ENV,
            transaction_crash_operation_name(operation),
        )
        .env(TRANSACTION_CRASH_ORDINAL_ENV, ordinal.to_string())
        .spawn()
        .expect("spawn transaction crash worker");
    LockStageWorker { child: Some(child) }
}

struct LockStageWorker {
    child: Option<Child>,
}

impl LockStageWorker {
    fn wait_success(&mut self) {
        let mut child = self.child.take().expect("live lock-stage worker");
        let status = child.wait().expect("wait for lock-stage worker");
        assert!(status.success(), "lock-stage worker failed: {status}");
    }

    fn kill_and_wait(&mut self) {
        let mut child = self.child.take().expect("live lock-stage worker");
        child.kill().expect("kill lock-stage worker");
        let _ = child.wait().expect("reap killed lock-stage worker");
    }
}

impl Drop for LockStageWorker {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_for_worker_path(path: &Path, worker: &mut LockStageWorker) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        if let Some(status) = worker
            .child
            .as_mut()
            .expect("live lock-stage worker")
            .try_wait()
            .expect("poll lock-stage worker")
        {
            panic!(
                "lock-stage worker exited before {}: {status}",
                path.display()
            );
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_stage_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn transaction_candidate_paths(root: &Path) -> Vec<PathBuf> {
    let directory = root.join("src/components/ui/_kit/.transactions");
    let mut paths = match fs::read_dir(&directory) {
        Ok(entries) => entries
            .map(|entry| entry.expect("read transaction candidate").path())
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("read transaction candidates: {error}"),
    };
    paths.sort();
    paths
}

fn coordination_file_identity(path: &Path) -> (u64, u64) {
    let parent_path = path.parent().expect("coordination file parent");
    let name = path.file_name().expect("coordination file name");
    let parent = cap_std::fs::Dir::open_ambient_dir(parent_path, cap_std::ambient_authority())
        .unwrap_or_else(|error| {
            panic!(
                "open coordination parent {}: {error}",
                parent_path.display()
            )
        });
    let metadata = parent
        .symlink_metadata(Path::new(name))
        .unwrap_or_else(|error| panic!("inspect coordination file {}: {error}", path.display()));
    (
        cap_fs_ext::MetadataExt::dev(&metadata),
        cap_fs_ext::MetadataExt::ino(&metadata),
    )
}

fn coordination_lock_identity(root: &Path) -> (u64, u64) {
    coordination_file_identity(&root.join(DEFAULT_KIT_WRITE_LOCK_PATH))
}

fn assert_only_verified_coordination_residuals(root: &Path) {
    let allowed_directories = BTreeSet::from([
        "src",
        "src/components",
        "src/components/ui",
        "src/components/ui/_kit",
    ]);
    let allowed_files = BTreeMap::from([
        (DEFAULT_KIT_WRITE_LOCK_PATH, KIT_ADVISORY_LOCK_CONTENT),
        (
            DEFAULT_KIT_COORDINATION_IGNORE_PATH,
            KIT_COORDINATION_IGNORE_CONTENT,
        ),
    ]);
    let mut pending = vec![root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read coordination residual directory") {
            let entry = entry.expect("read coordination residual entry");
            let path = entry.path();
            let logical_path = path
                .strip_prefix(root)
                .expect("residual remains below root")
                .to_string_lossy()
                .replace('\\', "/");
            let metadata = fs::symlink_metadata(&path).expect("residual metadata");

            if metadata.is_dir() {
                assert!(
                    allowed_directories.contains(logical_path.as_str()),
                    "unexpected residual directory {logical_path}"
                );
                #[cfg(unix)]
                if logical_path == "src/components/ui/_kit" {
                    use std::os::unix::fs::PermissionsExt;

                    assert_eq!(
                        metadata.permissions().mode() & 0o7777,
                        0o700,
                        "coordination residual directory mode"
                    );
                }
                pending.push(path);
                continue;
            }

            assert!(metadata.is_file(), "non-file residual {logical_path}");
            let expected = allowed_files
                .get(logical_path.as_str())
                .unwrap_or_else(|| panic!("unexpected residual file {logical_path}"));
            assert_eq!(
                fs::read(path).expect("read coordination residual"),
                *expected,
                "invalid residual contents for {logical_path}"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let expected_mode = if logical_path == DEFAULT_KIT_WRITE_LOCK_PATH {
                    0o600
                } else {
                    0o644
                };
                assert_eq!(
                    metadata.permissions().mode() & 0o7777,
                    expected_mode,
                    "coordination residual file mode for {logical_path}"
                );
            }
        }
    }
}

fn nested_registry_item() -> leptos_ui_kit_registry::ResolvedRegistryItem {
    leptos_ui_kit_registry::ResolvedRegistryItem {
        source_kind: leptos_ui_kit_registry::RegistrySourceKind::BuiltIn,
        source_path: PathBuf::from("registry/ui/nested.json"),
        content_hash: hash_bytes(b"nested"),
        targets: leptos_ui_kit_registry::ResolvedRegistryTargets {
            ui_files: vec![leptos_ui_kit_registry::ResolvedUiTarget {
                source: "ui/button.rs".to_owned(),
                path: "nested/root.rs".to_owned(),
            }],
            style_blocks: Vec::new(),
        },
        item: leptos_ui_kit_registry::RegistryItem {
            schema: leptos_ui_kit_registry::REGISTRY_ITEM_SCHEMA_URL.to_owned(),
            schema_version: leptos_ui_kit_registry::SCHEMA_VERSION.to_owned(),
            name: "nested".to_owned(),
            kind: leptos_ui_kit_registry::RegistryItemKind::Ui,
            version: leptos_ui_kit_registry::SCHEMA_VERSION.to_owned(),
            title: "Nested".to_owned(),
            description: "Nested".to_owned(),
            leptos: leptos_ui_kit_registry::RegistryLeptos {
                version: leptos_ui_kit_registry::LEPTOS_VERSION.to_owned(),
                router_version: leptos_ui_kit_registry::LEPTOS_ROUTER_VERSION.to_owned(),
                render_mode: leptos_ui_kit_registry::RenderMode::Csr,
            },
            accessibility: leptos_ui_kit_registry::RegistryAccessibility::default(),
            files: vec![leptos_ui_kit_registry::RegistryItemFile {
                source: "ui/button.rs".to_owned(),
                target: leptos_ui_kit_registry::RegistryFileTarget {
                    kind: leptos_ui_kit_registry::RegistryFileTargetKind::Ui,
                    path: "nested/root.rs".to_owned(),
                    exports: vec!["NestedButton".to_owned()],
                },
            }],
            styles: Vec::new(),
            registry_dependencies: Vec::new(),
            cargo_plan: Vec::new(),
            extra: BTreeMap::new(),
        },
    }
}
