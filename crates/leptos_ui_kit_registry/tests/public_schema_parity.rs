use std::{fs, path::Path};

const SCHEMAS: [&str; 4] = [
    "kit.schema.json",
    "registry-item.schema.json",
    "registry.schema.json",
    "theme-contract.schema.json",
];

#[test]
fn package_schemas_equal_the_public_mirrors_as_json_values() {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/0.9.0-alpha");
    let public_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/0.9.0-alpha");

    for name in SCHEMAS {
        let package_path = package_root.join(name);
        let public_path = public_root.join(name);
        let package = read_json(&package_path);
        let public = read_json(&public_path);

        assert_eq!(package, public, "schema mirror drift: {name}");
        assert_eq!(
            package["$id"], public["$id"],
            "schema identity drift: {name}"
        );
    }
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}
