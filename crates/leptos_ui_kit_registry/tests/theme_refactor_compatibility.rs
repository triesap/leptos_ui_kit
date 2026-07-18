use std::collections::{BTreeMap, BTreeSet};

use leptos_ui_kit_registry::{
    THEME_CONTRACT_VERSION, load_built_in_registry_item, load_built_in_registry_root,
    load_built_in_theme_contract, read_built_in_registry_source,
};
use serde::Deserialize;

const FIXTURE_JSON: &str = include_str!("fixtures/theme_refactor_compatibility.json");
const AUTHORITY_COMMIT: &str = "06124efa6a73b6e211565a856b7c4000f7e12f3f";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompatibilityFixture {
    fixture_version: u32,
    authority: Authority,
    counts: Counts,
    classes: Classes,
    export_groups: Vec<ExportGroup>,
    external_custom_properties: Vec<String>,
    runtime_geometry_properties: Vec<String>,
    canonical_tokens: Vec<CanonicalToken>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Authority {
    repository: String,
    commit: String,
    package_version: String,
    semantic_contract: SemanticContractAuthority,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SemanticContractAuthority {
    version: String,
    source: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Counts {
    css_selector_classes: usize,
    rust_emitted_only_classes: usize,
    static_classes: usize,
    exports: usize,
    external_custom_properties: usize,
    runtime_geometry_properties: usize,
    canonical_tokens: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Classes {
    css_selectors: Vec<String>,
    rust_emitted_only: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportGroup {
    item: String,
    target: String,
    module: String,
    symbols: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalToken {
    name: String,
    #[serde(rename = "default")]
    default_value: String,
}

#[test]
fn pinned_theme_refactor_compatibility_surface_remains_available() {
    let fixture: CompatibilityFixture =
        serde_json::from_str(FIXTURE_JSON).expect("parse compatibility fixture");

    assert_eq!(fixture.fixture_version, 1);
    assert_eq!(fixture.authority.repository, "leptos_ui_kit");
    assert_eq!(fixture.authority.commit, AUTHORITY_COMMIT);
    assert_eq!(fixture.authority.package_version, "0.1.0");
    assert_eq!(
        fixture.authority.semantic_contract.version,
        THEME_CONTRACT_VERSION
    );
    assert_eq!(
        fixture.authority.semantic_contract.source,
        "registry/contracts/theme-v1.json"
    );

    assert_eq!(fixture.counts.css_selector_classes, 46);
    assert_eq!(fixture.counts.rust_emitted_only_classes, 3);
    assert_eq!(fixture.counts.static_classes, 49);
    assert_eq!(fixture.counts.exports, 55);
    assert_eq!(fixture.counts.external_custom_properties, 208);
    assert_eq!(fixture.counts.runtime_geometry_properties, 2);
    assert_eq!(fixture.counts.canonical_tokens, 43);

    assert_sorted_unique("CSS selector classes", &fixture.classes.css_selectors);
    assert_sorted_unique(
        "Rust-emitted-only classes",
        &fixture.classes.rust_emitted_only,
    );
    assert_sorted_unique(
        "external custom properties",
        &fixture.external_custom_properties,
    );
    assert_sorted_unique(
        "runtime geometry properties",
        &fixture.runtime_geometry_properties,
    );

    assert_eq!(
        fixture.classes.css_selectors.len(),
        fixture.counts.css_selector_classes
    );
    assert_eq!(
        fixture.classes.rust_emitted_only.len(),
        fixture.counts.rust_emitted_only_classes
    );
    assert_eq!(
        fixture.external_custom_properties.len(),
        fixture.counts.external_custom_properties
    );
    assert_eq!(
        fixture.runtime_geometry_properties.len(),
        fixture.counts.runtime_geometry_properties
    );

    let pinned_css_classes = fixture
        .classes
        .css_selectors
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let pinned_rust_only_classes = fixture
        .classes
        .rust_emitted_only
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert!(
        pinned_css_classes.is_disjoint(&pinned_rust_only_classes),
        "pinned CSS and Rust-only class classifications must be disjoint"
    );
    assert_eq!(
        pinned_css_classes.union(&pinned_rust_only_classes).count(),
        fixture.counts.static_classes
    );

    let pinned_external_properties = fixture
        .external_custom_properties
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let pinned_runtime_geometry = fixture
        .runtime_geometry_properties
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert!(
        pinned_external_properties.is_disjoint(&pinned_runtime_geometry),
        "external overrides and local runtime geometry must be disjoint"
    );

    let fixture_exports = validate_and_flatten_fixture_exports(&fixture);
    assert_eq!(fixture_exports.len(), fixture.counts.exports);

    let token_names = fixture
        .canonical_tokens
        .iter()
        .map(|token| token.name.clone())
        .collect::<Vec<_>>();
    assert_sorted_unique("canonical token names", &token_names);
    assert_eq!(token_names.len(), fixture.counts.canonical_tokens);
    let fixture_token_defaults = fixture
        .canonical_tokens
        .iter()
        .map(|token| (token.name.clone(), token.default_value.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fixture_token_defaults.len(),
        fixture.counts.canonical_tokens,
        "canonical token names must be unique"
    );

    let root = load_built_in_registry_root().expect("load built-in registry root");
    let mut current_css_classes = BTreeSet::new();
    let mut current_rust_classes = BTreeSet::new();
    let mut current_custom_properties = BTreeSet::new();
    let mut current_exports = BTreeSet::new();

    for entry in root.items {
        let resolved = load_built_in_registry_item(&entry.name)
            .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));

        for file in &resolved.item.files {
            let source = read_built_in_registry_source(&file.source)
                .unwrap_or_else(|error| panic!("read {}: {error}", file.source));
            current_rust_classes.extend(rust_class_emission_tokens(&source));

            let module = generated_module_path(&file.target.path);
            for symbol in &file.target.exports {
                assert!(
                    current_exports.insert((
                        resolved.item.name.clone(),
                        file.target.path.clone(),
                        module.clone(),
                        symbol.clone(),
                    )),
                    "duplicate current export {}::{symbol}",
                    module
                );
            }
        }

        for style in &resolved.item.styles {
            let css = read_built_in_registry_source(&style.source)
                .unwrap_or_else(|error| panic!("read {}: {error}", style.source));
            current_css_classes.extend(css_selector_classes(&css));
            current_custom_properties.extend(css_custom_property_names(&css));
        }
    }

    assert_required_subset(
        "pinned CSS selector classes",
        &pinned_css_classes,
        &current_css_classes,
    );
    let pinned_rust_class_tokens = pinned_rust_only_classes
        .iter()
        .map(|class| {
            class
                .strip_prefix('.')
                .unwrap_or_else(|| panic!("class must start with a dot: {class}"))
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_required_subset(
        "pinned Rust-emitted classes",
        &pinned_rust_class_tokens,
        &current_rust_classes,
    );
    assert_required_subset(
        "pinned registry exports",
        &fixture_exports,
        &current_exports,
    );
    assert_required_subset(
        "pinned external custom properties",
        &pinned_external_properties,
        &current_custom_properties,
    );
    assert_required_subset(
        "pinned runtime geometry properties",
        &pinned_runtime_geometry,
        &current_custom_properties,
    );

    let contract = load_built_in_theme_contract().expect("load semantic theme contract");
    let contract_defaults = contract
        .tokens
        .into_iter()
        .map(|token| (token.name, token.default_value))
        .collect::<BTreeMap<_, _>>();
    let tokens_css = read_built_in_registry_source("styles/tokens.css").expect("read tokens CSS");
    let css_defaults = css_custom_property_defaults(&tokens_css);

    assert_eq!(fixture_token_defaults, contract_defaults);
    assert_eq!(fixture_token_defaults, css_defaults);
}

fn validate_and_flatten_fixture_exports(
    fixture: &CompatibilityFixture,
) -> BTreeSet<(String, String, String, String)> {
    for pair in fixture.export_groups.windows(2) {
        let left = (&pair[0].item, &pair[0].target, &pair[0].module);
        let right = (&pair[1].item, &pair[1].target, &pair[1].module);
        assert!(left < right, "export groups must be sorted and unique");
    }

    let mut exports = BTreeSet::new();
    for group in &fixture.export_groups {
        assert_eq!(
            group.module,
            generated_module_path(&group.target),
            "fixture module path drift for {}",
            group.item
        );
        assert_sorted_unique(&format!("{} export symbols", group.item), &group.symbols);
        for symbol in &group.symbols {
            assert!(
                exports.insert((
                    group.item.clone(),
                    group.target.clone(),
                    group.module.clone(),
                    symbol.clone(),
                )),
                "duplicate fixture export {}::{symbol}",
                group.module
            );
        }
    }
    exports
}

fn generated_module_path(target: &str) -> String {
    let without_extension = target
        .strip_suffix(".rs")
        .unwrap_or_else(|| panic!("Rust target must end in .rs: {target}"));
    let module = without_extension
        .strip_suffix("/mod")
        .unwrap_or(without_extension)
        .replace('/', "::");
    format!("components::ui::{module}")
}

fn assert_sorted_unique(label: &str, values: &[String]) {
    for pair in values.windows(2) {
        assert!(
            pair[0] < pair[1],
            "{label} must be strictly sorted and unique: {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }
}

fn assert_required_subset<T>(label: &str, required: &BTreeSet<T>, current: &BTreeSet<T>)
where
    T: Ord + std::fmt::Debug,
{
    let missing = required.difference(current).collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "{label} missing from current source: {missing:?}"
    );
}

fn css_selector_classes(input: &str) -> BTreeSet<String> {
    let code = mask_css_comments_and_strings(input);
    let mut classes = BTreeSet::new();
    let mut prelude = Vec::new();

    for byte in code {
        match byte {
            b'{' => {
                let trimmed = trim_ascii_start(&prelude);
                if !trimmed.starts_with(b"@") {
                    classes.extend(class_selector_tokens(trimmed));
                }
                prelude.clear();
            }
            b'}' | b';' => prelude.clear(),
            _ => prelude.push(byte),
        }
    }

    classes
}

fn class_selector_tokens(input: &[u8]) -> BTreeSet<String> {
    let mut classes = BTreeSet::new();
    let mut index = 0;
    while index + 5 < input.len() {
        if input[index] != b'.' || !input[index + 1..].starts_with(b"kit-") {
            index += 1;
            continue;
        }
        if index > 0 && is_css_identifier_byte(input[index - 1]) {
            index += 1;
            continue;
        }

        let mut end = index + 5;
        while end < input.len()
            && (input[end].is_ascii_lowercase()
                || input[end].is_ascii_digit()
                || input[end] == b'-')
        {
            end += 1;
        }
        if end == index + 5
            || input
                .get(end)
                .is_some_and(|byte| is_css_identifier_byte(*byte))
        {
            index += 1;
            continue;
        }

        classes.insert(String::from_utf8(input[index..end].to_vec()).expect("ASCII class"));
        index = end;
    }
    classes
}

fn css_custom_property_names(input: &str) -> BTreeSet<String> {
    let code = mask_css_comments_and_strings(input);
    let mut names = BTreeSet::new();
    let mut index = 0;

    while index + 6 <= code.len() {
        if !code[index..].starts_with(b"--kit-")
            || (index > 0 && is_css_identifier_byte(code[index - 1]))
        {
            index += 1;
            continue;
        }

        let mut end = index + 6;
        while end < code.len()
            && (code[end].is_ascii_lowercase() || code[end].is_ascii_digit() || code[end] == b'-')
        {
            end += 1;
        }
        if end == index + 6
            || code
                .get(end)
                .is_some_and(|byte| is_css_identifier_byte(*byte))
        {
            index += 1;
            continue;
        }

        names.insert(String::from_utf8(code[index..end].to_vec()).expect("ASCII property"));
        index = end;
    }

    names
}

fn css_custom_property_defaults(input: &str) -> BTreeMap<String, String> {
    let code = mask_css_comments_and_strings(input);
    let mut defaults = BTreeMap::new();
    let mut index = 0;

    while index + 6 <= code.len() {
        if !code[index..].starts_with(b"--kit-")
            || (index > 0 && is_css_identifier_byte(code[index - 1]))
        {
            index += 1;
            continue;
        }

        let mut name_end = index + 6;
        while name_end < code.len()
            && (code[name_end].is_ascii_lowercase()
                || code[name_end].is_ascii_digit()
                || code[name_end] == b'-')
        {
            name_end += 1;
        }
        if name_end == index + 6
            || code
                .get(name_end)
                .is_some_and(|byte| is_css_identifier_byte(*byte))
        {
            index += 1;
            continue;
        }

        let mut value_start = name_end;
        while code.get(value_start).is_some_and(u8::is_ascii_whitespace) {
            value_start += 1;
        }
        if code.get(value_start) != Some(&b':') {
            index = name_end;
            continue;
        }
        value_start += 1;

        let mut value_end = value_start;
        let mut parentheses = 0usize;
        while value_end < code.len() {
            match code[value_end] {
                b'(' => parentheses += 1,
                b')' => parentheses = parentheses.saturating_sub(1),
                b';' if parentheses == 0 => break,
                _ => {}
            }
            value_end += 1;
        }
        assert!(
            value_end < code.len(),
            "custom property declaration lacks semicolon"
        );

        let name = String::from_utf8(code[index..name_end].to_vec()).expect("ASCII property");
        let raw_value = String::from_utf8(code[value_start..value_end].to_vec())
            .expect("UTF-8 custom property value");
        let value = raw_value.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            defaults.insert(name.clone(), value).is_none(),
            "duplicate custom property declaration: {name}"
        );
        index = value_end + 1;
    }

    defaults
}

fn mask_css_comments_and_strings(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut masked = bytes.to_vec();
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;

    while index < bytes.len() {
        if let Some(delimiter) = quote {
            masked[index] = b' ';
            if escaped {
                escaped = false;
            } else if bytes[index] == b'\\' {
                escaped = true;
            } else if bytes[index] == delimiter {
                quote = None;
            }
            index += 1;
            continue;
        }

        if matches!(bytes[index], b'\'' | b'"') {
            quote = Some(bytes[index]);
            masked[index] = b' ';
            index += 1;
            continue;
        }

        if bytes[index..].starts_with(b"/*") {
            masked[index] = b' ';
            masked[index + 1] = b' ';
            index += 2;
            while index < bytes.len() && !bytes[index..].starts_with(b"*/") {
                if bytes[index] != b'\n' {
                    masked[index] = b' ';
                }
                index += 1;
            }
            assert!(index + 1 < bytes.len(), "unterminated CSS comment");
            masked[index] = b' ';
            masked[index + 1] = b' ';
            index += 2;
            continue;
        }

        index += 1;
    }

    assert!(quote.is_none(), "unterminated CSS string");
    masked
}

fn rust_string_literals(input: &str) -> Vec<String> {
    let bytes = input.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index = skip_nested_rust_comment(bytes, index);
            continue;
        }

        if bytes[index] == b'r' {
            if let Some((literal, next)) = parse_raw_rust_string(bytes, index) {
                literals.push(literal);
                index = next;
                continue;
            }
        }

        if bytes[index] == b'"' {
            let (literal, next) = parse_normal_rust_string(bytes, index);
            literals.push(literal);
            index = next;
            continue;
        }

        index += 1;
    }

    literals
}

fn rust_class_emission_tokens(input: &str) -> BTreeSet<String> {
    input
        .lines()
        .filter(|line| line.contains("class"))
        .flat_map(rust_string_literals)
        .flat_map(|literal| static_class_tokens(&literal))
        .collect()
}

fn skip_nested_rust_comment(bytes: &[u8], mut index: usize) -> usize {
    let mut depth = 0usize;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"/*") {
            depth += 1;
            index += 2;
        } else if bytes[index..].starts_with(b"*/") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
        } else {
            index += 1;
        }
    }
    panic!("unterminated Rust block comment");
}

fn parse_raw_rust_string(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let mut delimiter = start + 1;
    while bytes.get(delimiter) == Some(&b'#') {
        delimiter += 1;
    }
    if bytes.get(delimiter) != Some(&b'"') {
        return None;
    }

    let hashes = delimiter - start - 1;
    let content_start = delimiter + 1;
    let mut end = content_start;
    while end < bytes.len() {
        if bytes[end] == b'"'
            && bytes
                .get(end + 1..end + 1 + hashes)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            let literal = String::from_utf8(bytes[content_start..end].to_vec())
                .expect("UTF-8 raw Rust string");
            return Some((literal, end + 1 + hashes));
        }
        end += 1;
    }
    panic!("unterminated raw Rust string");
}

fn parse_normal_rust_string(bytes: &[u8], start: usize) -> (String, usize) {
    let mut content = Vec::new();
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                return (
                    String::from_utf8(content).expect("UTF-8 Rust string"),
                    index + 1,
                );
            }
            b'\\' => {
                index += 1;
                assert!(index < bytes.len(), "unterminated Rust string escape");
                content.push(bytes[index]);
                index += 1;
            }
            byte => {
                content.push(byte);
                index += 1;
            }
        }
    }
    panic!("unterminated Rust string");
}

fn static_class_tokens(literal: &str) -> Vec<String> {
    literal
        .split_ascii_whitespace()
        .filter(|token| {
            token.starts_with("kit-")
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
        .map(str::to_owned)
        .collect()
}

fn is_css_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn trim_ascii_start(mut input: &[u8]) -> &[u8] {
    while input.first().is_some_and(u8::is_ascii_whitespace) {
        input = &input[1..];
    }
    input
}
