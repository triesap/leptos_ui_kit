use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use leptos_ui_kit_registry::{
    KIT_SCHEMA_URL, REGISTRY_ITEM_SCHEMA_URL, REGISTRY_SCHEMA_URL, RegistryItem, RegistryRoot,
};
use serde_json::{Value, json};

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(
        &fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

fn compile_draft_2020_12_schema(path: &Path) -> (Value, jsonschema::Validator) {
    let schema = read_json(path);
    jsonschema::draft202012::meta::validate(&schema).unwrap_or_else(|error| {
        panic!(
            "{} is not valid Draft 2020-12 JSON Schema: {error}",
            path.display()
        )
    });
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap_or_else(|error| panic!("failed to compile {}: {error}", path.display()));
    (schema, validator)
}

fn assert_valid(validator: &jsonschema::Validator, instance: &Value, path: &Path) {
    let errors = validator
        .iter_errors(instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{} failed schema validation:\n{}",
        path.display(),
        errors.join("\n")
    );
}

fn collect_json_paths(root: &Path, directory: &Path, paths: &mut BTreeSet<String>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
    {
        let entry = entry.expect("read registry directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect_json_paths(root, &path, paths);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            let relative = path
                .strip_prefix(root)
                .expect("registry document should be under registry root");
            paths.insert(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn package_registry_schemas_are_valid_draft_2020_12() {
    let schema_root = package_root().join("schema/0.9.0-alpha");
    for (file_name, expected_id) in [
        ("registry.schema.json", REGISTRY_SCHEMA_URL),
        ("registry-item.schema.json", REGISTRY_ITEM_SCHEMA_URL),
    ] {
        let path = schema_root.join(file_name);
        let (schema, _) = compile_draft_2020_12_schema(&path);

        assert_eq!(
            schema["$schema"],
            json!("https://json-schema.org/draft/2020-12/schema")
        );
        assert_eq!(schema["$id"], json!(expected_id));
    }
}

#[test]
fn kit_schema_accepts_app_and_shared_library_shapes() {
    let root = package_root();
    let schema_path = root.join("schema/0.9.0-alpha/kit.schema.json");
    let (schema, validator) = compile_draft_2020_12_schema(&schema_path);
    assert_eq!(schema["$id"], json!(KIT_SCHEMA_URL));

    let shared_path =
        root.join("../../tests/fixtures/shared_library/src/components/ui/_kit/kit.json");
    let shared = read_json(&shared_path);
    assert_valid(&validator, &shared, &shared_path);

    let mut app = shared.clone();
    app["project"]["kind"] = json!("single-crate-trunk-csr");
    app["project"]["indexHtml"] = json!("index.html");
    assert_valid(&validator, &app, Path::new("<app-kit-config>"));

    let mut shared_with_html = shared.clone();
    shared_with_html["project"]["indexHtml"] = json!("index.html");
    assert!(!validator.is_valid(&shared_with_html));

    let mut app_without_html = app;
    app_without_html["project"]
        .as_object_mut()
        .expect("project object")
        .remove("indexHtml");
    assert!(!validator.is_valid(&app_without_html));
}

#[test]
fn package_registry_schemas_validate_every_built_in_document() {
    let root = package_root();
    let schema_root = root.join("schema/0.9.0-alpha");
    let registry_root = root.join("registry");
    let (_, root_validator) =
        compile_draft_2020_12_schema(&schema_root.join("registry.schema.json"));
    let (_, item_validator) =
        compile_draft_2020_12_schema(&schema_root.join("registry-item.schema.json"));

    let root_path = registry_root.join("registry.json");
    let raw_root = read_json(&root_path);
    assert_valid(&root_validator, &raw_root, &root_path);
    let typed_root: RegistryRoot =
        serde_json::from_value(raw_root).expect("deserialize schema-valid registry root");
    typed_root
        .validate()
        .expect("validate registry root in Rust");

    let mut listed_paths = BTreeSet::new();
    for entry in &typed_root.items {
        assert!(
            listed_paths.insert(entry.path.clone()),
            "duplicate root path"
        );
        let path = registry_root.join(&entry.path);
        let raw_item = read_json(&path);
        assert_valid(&item_validator, &raw_item, &path);
        assert_eq!(raw_item["name"], json!(entry.name));

        let typed_item: RegistryItem =
            serde_json::from_value(raw_item).expect("deserialize schema-valid registry item");
        typed_item
            .validate()
            .expect("validate registry item in Rust");
    }

    let mut discovered_paths = BTreeSet::new();
    for directory in ["foundation", "ui"] {
        collect_json_paths(
            &registry_root,
            &registry_root.join(directory),
            &mut discovered_paths,
        );
    }
    assert_eq!(listed_paths, discovered_paths);
}

#[test]
fn package_registry_schemas_reject_structurally_invalid_documents() {
    let root = package_root();
    let schema_root = root.join("schema/0.9.0-alpha");
    let registry_root = root.join("registry");
    let (_, root_validator) =
        compile_draft_2020_12_schema(&schema_root.join("registry.schema.json"));
    let (_, item_validator) =
        compile_draft_2020_12_schema(&schema_root.join("registry-item.schema.json"));

    let raw_root = read_json(&registry_root.join("registry.json"));
    for invalid_name in ["1button", "-button"] {
        let mut invalid = raw_root.clone();
        invalid["items"][0]["name"] = json!(invalid_name);
        assert!(
            !root_validator.is_valid(&invalid),
            "accepted {invalid_name}"
        );
    }
    for invalid_path in [
        "",
        "/ui/button.json",
        "ui\\button.json",
        "ui//button.json",
        "ui/./button.json",
        "ui/../button.json",
        ".hidden/button.json",
        "ui/button",
        "ui/button.rs",
    ] {
        let mut invalid = raw_root.clone();
        invalid["items"][0]["path"] = json!(invalid_path);
        assert!(
            !root_validator.is_valid(&invalid),
            "accepted root path {invalid_path:?}"
        );
    }

    let raw_item = read_json(&registry_root.join("ui/button.json"));
    for (pointer, invalid_value) in [
        ("/title", json!(" \n\t")),
        ("/description", json!(" \n\t")),
        ("/files/0/source", json!("ui/button")),
        ("/files/0/source", json!("ui/button.css")),
        ("/files/0/source", json!("ui/../button.rs")),
        ("/styles/0/source", json!("styles/button")),
        ("/styles/0/source", json!("styles/button.rs")),
        ("/styles/0/source", json!("styles/.hidden/button.css")),
    ] {
        let mut invalid = raw_item.clone();
        *invalid.pointer_mut(pointer).expect("fixture pointer") = invalid_value;
        assert!(!item_validator.is_valid(&invalid), "accepted {pointer}");
    }

    let mut duplicate_dependency = raw_item;
    duplicate_dependency["registryDependencies"] = json!(["tokens", "tokens"]);
    assert!(!item_validator.is_valid(&duplicate_dependency));

    let mut invalid_behavior = read_json(&registry_root.join("ui/button.json"));
    invalid_behavior["accessibility"]["behaviors"][0]["name"] = json!("1behavior");
    assert!(!item_validator.is_valid(&invalid_behavior));
}
