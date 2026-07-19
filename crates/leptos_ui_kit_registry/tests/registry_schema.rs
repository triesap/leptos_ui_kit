use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use leptos_ui_kit_registry::{
    KIT_SCHEMA_URL, REGISTRY_ITEM_SCHEMA_URL, REGISTRY_SCHEMA_URL, THEME_CONTRACT_SCHEMA_URL,
    canonical_kit_config, load_built_in_theme_contract, parse_kit_json_str,
    parse_registry_item_str, parse_registry_root_str, parse_theme_contract_str,
    validate_registry_graph, validate_registry_manifest_identity,
};
use serde_json::{Value, json};

const SCHEMAS: [(&str, &str); 4] = [
    ("kit.schema.json", KIT_SCHEMA_URL),
    ("registry.schema.json", REGISTRY_SCHEMA_URL),
    ("registry-item.schema.json", REGISTRY_ITEM_SCHEMA_URL),
    ("theme-contract.schema.json", THEME_CONTRACT_SCHEMA_URL),
];

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn schema_root() -> PathBuf {
    package_root().join("schema/0.9.0-alpha")
}

fn registry_root() -> PathBuf {
    package_root().join("registry")
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

fn assert_valid(validator: &jsonschema::Validator, instance: &Value, description: &str) {
    let errors = validator
        .iter_errors(instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{description} failed schema validation:\n{}",
        errors.join("\n")
    );
}

fn assert_invalid(validator: &jsonschema::Validator, instance: &Value, description: &str) {
    assert!(
        !validator.is_valid(instance),
        "{description} unexpectedly passed schema validation"
    );
}

fn json_string(value: &Value) -> String {
    serde_json::to_string(value).expect("serialize JSON fixture")
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
fn all_package_schemas_are_valid_draft_2020_12() {
    for (file_name, expected_id) in SCHEMAS {
        let path = schema_root().join(file_name);
        let (schema, _) = compile_draft_2020_12_schema(&path);

        assert_eq!(
            schema["$schema"],
            json!("https://json-schema.org/draft/2020-12/schema")
        );
        assert_eq!(schema["$id"], json!(expected_id));
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
    }
}

#[test]
fn schemas_and_public_parsers_accept_every_canonical_document() {
    let (_, kit_validator) = compile_draft_2020_12_schema(&schema_root().join("kit.schema.json"));
    let kit = serde_json::to_value(canonical_kit_config().expect("canonical kit config"))
        .expect("serialize canonical kit config");
    assert_valid(&kit_validator, &kit, "canonical kit.json");
    parse_kit_json_str(&json_string(&kit)).expect("parse canonical kit.json");

    let (_, root_validator) =
        compile_draft_2020_12_schema(&schema_root().join("registry.schema.json"));
    let (_, item_validator) =
        compile_draft_2020_12_schema(&schema_root().join("registry-item.schema.json"));
    let root_path = registry_root().join("registry.json");
    let raw_root = read_json(&root_path);
    assert_valid(&root_validator, &raw_root, "built-in registry root");
    let typed_root =
        parse_registry_root_str(&json_string(&raw_root)).expect("parse built-in registry root");

    let mut listed_paths = BTreeSet::new();
    let mut typed_items = Vec::new();
    for entry in &typed_root.items {
        assert!(
            listed_paths.insert(entry.path.clone()),
            "duplicate root path"
        );
        let path = registry_root().join(&entry.path);
        let raw_item = read_json(&path);
        assert_valid(&item_validator, &raw_item, &entry.path);
        let typed_item =
            parse_registry_item_str(&json_string(&raw_item)).expect("parse built-in item");
        validate_registry_manifest_identity(&typed_root, &entry.path, &typed_item)
            .expect("registry root and manifest identity");
        typed_items.push(typed_item);
    }
    validate_registry_graph(&typed_items).expect("validate complete registry graph");

    let mut discovered_paths = BTreeSet::new();
    for directory in ["foundation", "ui"] {
        collect_json_paths(
            &registry_root(),
            &registry_root().join(directory),
            &mut discovered_paths,
        );
    }
    assert_eq!(listed_paths, discovered_paths);

    let (_, theme_validator) =
        compile_draft_2020_12_schema(&schema_root().join("theme-contract.schema.json"));
    let theme =
        serde_json::to_value(load_built_in_theme_contract().expect("load built-in theme contract"))
            .expect("serialize built-in theme contract");
    assert_valid(&theme_validator, &theme, "built-in theme contract");
    parse_theme_contract_str(&json_string(&theme)).expect("parse built-in theme contract");
}

#[test]
fn schemas_reject_structurally_invalid_documents() {
    let (_, kit_validator) = compile_draft_2020_12_schema(&schema_root().join("kit.schema.json"));
    let kit = serde_json::to_value(canonical_kit_config().expect("canonical kit config"))
        .expect("serialize canonical kit config");
    let mut missing_project = kit.clone();
    missing_project
        .as_object_mut()
        .expect("kit object")
        .remove("project");
    assert_invalid(&kit_validator, &missing_project, "kit missing project");
    let mut unknown_kit_field = kit;
    unknown_kit_field["legacyAlias"] = json!(true);
    assert_invalid(&kit_validator, &unknown_kit_field, "kit with unknown field");
    let mut keyword_install_path =
        serde_json::to_value(canonical_kit_config().expect("canonical kit config"))
            .expect("serialize canonical kit config");
    keyword_install_path["install"]["uiDir"] = json!("src/components/async");
    keyword_install_path["install"]["uiMod"] = json!("src/components/async/mod.rs");
    assert_invalid(
        &kit_validator,
        &keyword_install_path,
        "kit with Rust keyword install path",
    );

    let (_, root_validator) =
        compile_draft_2020_12_schema(&schema_root().join("registry.schema.json"));
    let raw_root = read_json(&registry_root().join("registry.json"));
    for invalid_name in ["1button", "-button", "button-", "button--group", "async"] {
        let mut invalid = raw_root.clone();
        invalid["items"][0]["name"] = json!(invalid_name);
        assert_invalid(
            &root_validator,
            &invalid,
            &format!("registry name {invalid_name:?}"),
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
        assert_invalid(
            &root_validator,
            &invalid,
            &format!("registry path {invalid_path:?}"),
        );
    }

    let (_, item_validator) =
        compile_draft_2020_12_schema(&schema_root().join("registry-item.schema.json"));
    let raw_item = read_json(&registry_root().join("ui/button.json"));
    for (pointer, invalid_value) in [
        ("/title", json!(" \n\t")),
        ("/description", json!(" \n\t")),
        ("/files/0/source", json!("ui/button")),
        ("/files/0/source", json!("ui/button.css")),
        ("/files/0/source", json!("ui/../button.rs")),
        ("/styles/0/source", json!("styles/button")),
        ("/styles/0/source", json!("styles/button.rs")),
        ("/styles/0/source", json!("styles/.hidden/button.css")),
        ("/cargoPlan/0/source/kind", json!("path")),
        ("/cargoPlan/0/source/version", Value::Null),
        ("/files/0/target/path", json!("async.rs")),
        ("/files/0/target/exports/0", json!("Self")),
    ] {
        let mut invalid = raw_item.clone();
        *invalid.pointer_mut(pointer).expect("fixture pointer") = invalid_value;
        assert_invalid(&item_validator, &invalid, pointer);
    }
    let mut cross_variant_source = raw_item;
    cross_variant_source["cargoPlan"][0]["source"]["url"] =
        json!("https://github.com/triesap/leptos_ui_kit");
    assert_invalid(
        &item_validator,
        &cross_variant_source,
        "cross-variant Cargo source",
    );
    for collection in ["files", "styles", "cargoPlan"] {
        let mut duplicate_entry = read_json(&registry_root().join("ui/button.json"));
        let entry = duplicate_entry[collection][0].clone();
        duplicate_entry[collection]
            .as_array_mut()
            .expect("registry item collection")
            .push(entry);
        assert_invalid(
            &item_validator,
            &duplicate_entry,
            &format!("exact duplicate {collection} entry"),
        );
    }

    let (_, theme_validator) =
        compile_draft_2020_12_schema(&schema_root().join("theme-contract.schema.json"));
    let raw_theme = read_json(&registry_root().join("contracts/theme-v1.json"));
    let mut empty_theme = raw_theme.clone();
    empty_theme["tokens"] = json!([]);
    assert_invalid(&theme_validator, &empty_theme, "empty theme contract");
    let mut wrong_category = raw_theme;
    wrong_category["tokens"][0]["category"] = json!("typography");
    assert_invalid(
        &theme_validator,
        &wrong_category,
        "unsupported theme category",
    );
    for invalid_name in ["--kit-color-", "--kit-color--surface", "--other-color"] {
        let mut invalid_token_name = read_json(&registry_root().join("contracts/theme-v1.json"));
        invalid_token_name["tokens"][0]["name"] = json!(invalid_name);
        assert_invalid(
            &theme_validator,
            &invalid_token_name,
            &format!("invalid theme token name {invalid_name:?}"),
        );
    }
    let mut exact_duplicate_token = read_json(&registry_root().join("contracts/theme-v1.json"));
    let first_token = exact_duplicate_token["tokens"][0].clone();
    exact_duplicate_token["tokens"]
        .as_array_mut()
        .expect("theme token array")
        .push(first_token);
    assert_invalid(
        &theme_validator,
        &exact_duplicate_token,
        "exact duplicate theme token",
    );
}

#[test]
fn public_semantic_layer_rejects_constraints_standard_schemas_do_not_express() {
    let (_, kit_validator) = compile_draft_2020_12_schema(&schema_root().join("kit.schema.json"));
    let mut mismatched_install =
        serde_json::to_value(canonical_kit_config().expect("canonical kit config"))
            .expect("serialize canonical kit config");
    mismatched_install["install"]["uiMod"] = json!("src/components/other/mod.rs");
    assert_valid(
        &kit_validator,
        &mismatched_install,
        "structurally valid mismatched install paths",
    );
    parse_kit_json_str(&json_string(&mismatched_install))
        .expect_err("semantic kit parser must reject mismatched install paths");

    let (_, root_validator) =
        compile_draft_2020_12_schema(&schema_root().join("registry.schema.json"));
    let raw_root = read_json(&registry_root().join("registry.json"));
    let mut duplicate_name = raw_root.clone();
    duplicate_name["items"][1]["name"] = duplicate_name["items"][0]["name"].clone();
    assert_valid(
        &root_validator,
        &duplicate_name,
        "structurally distinct duplicate-name root entries",
    );
    parse_registry_root_str(&json_string(&duplicate_name))
        .expect_err("semantic root parser must reject duplicate names");

    let typed_root =
        parse_registry_root_str(&json_string(&raw_root)).expect("parse valid registry root");

    let (_, item_validator) =
        compile_draft_2020_12_schema(&schema_root().join("registry-item.schema.json"));
    let raw_item = read_json(&registry_root().join("ui/button.json"));
    let mut mismatched_style = raw_item.clone();
    mismatched_style["styles"][0]["target"]["id"] = json!("spinner");
    assert_valid(
        &item_validator,
        &mismatched_style,
        "structurally valid mismatched UI style identity",
    );
    parse_registry_item_str(&json_string(&mismatched_style))
        .expect_err("semantic item parser must reject mismatched UI style identity");

    let mut duplicate_crate = raw_item.clone();
    let mut second_entry = duplicate_crate["cargoPlan"][0].clone();
    second_entry["features"] = json!(["csr", "nightly"]);
    duplicate_crate["cargoPlan"]
        .as_array_mut()
        .expect("Cargo plan array")
        .push(second_entry);
    assert_valid(
        &item_validator,
        &duplicate_crate,
        "structurally distinct duplicate crate entries",
    );
    parse_registry_item_str(&json_string(&duplicate_crate))
        .expect_err("semantic item parser must reject duplicate crates");

    let parsed_button =
        parse_registry_item_str(&json_string(&raw_item)).expect("parse valid button item");
    validate_registry_manifest_identity(&typed_root, "ui/anchor.json", &parsed_button)
        .expect_err("cross-document root/manifest identity must match");

    let mut unknown_dependency = raw_item;
    unknown_dependency["registryDependencies"] = json!(["tokens", "missing"]);
    assert_valid(
        &item_validator,
        &unknown_dependency,
        "structurally valid unknown dependency",
    );
    let unknown_dependency = parse_registry_item_str(&json_string(&unknown_dependency))
        .expect("document-local semantics permit unresolved dependency");
    let tokens = read_json(&registry_root().join("foundation/tokens.json"));
    let tokens =
        parse_registry_item_str(&json_string(&tokens)).expect("parse tokens foundation item");
    validate_registry_graph(&[unknown_dependency, tokens])
        .expect_err("graph semantics must reject unknown dependencies");

    let (_, theme_validator) =
        compile_draft_2020_12_schema(&schema_root().join("theme-contract.schema.json"));
    let mut duplicate_token = read_json(&registry_root().join("contracts/theme-v1.json"));
    let mut second_token = duplicate_token["tokens"][0].clone();
    second_token["description"] = json!("A different description.");
    duplicate_token["tokens"]
        .as_array_mut()
        .expect("theme token array")
        .push(second_token);
    assert_valid(
        &theme_validator,
        &duplicate_token,
        "structurally distinct duplicate theme token",
    );
    parse_theme_contract_str(&json_string(&duplicate_token))
        .expect_err("semantic theme parser must reject duplicate token names");
}

#[test]
fn jsonschema_is_a_development_only_dependency() {
    let manifest = fs::read_to_string(package_root().join("Cargo.toml"))
        .expect("read registry crate manifest");
    let manifest = toml::from_str::<toml::Value>(&manifest).expect("parse Cargo.toml");

    assert!(
        manifest
            .get("dependencies")
            .and_then(|dependencies| dependencies.get("jsonschema"))
            .is_none(),
        "jsonschema must not be a production dependency"
    );
    assert!(
        manifest
            .get("dev-dependencies")
            .and_then(|dependencies| dependencies.get("jsonschema"))
            .is_some(),
        "jsonschema should remain development-only"
    );
}
