use std::collections::{BTreeMap, BTreeSet};

use leptos_ui_kit_registry::{
    ComponentCustomizationScope, load_built_in_component_customization_contract,
    load_built_in_registry_item, load_built_in_registry_root, load_built_in_theme_contract,
    read_built_in_registry_source,
};
use serde::Deserialize;

const MAPPING_FIXTURE: &str = include_str!("fixtures/theme_refactor_mapping.json");
const COMPATIBILITY_FIXTURE: &str = include_str!("fixtures/theme_refactor_compatibility.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MappingFixture {
    fixture_version: u32,
    row_count: usize,
    rows_by_stylesheet: BTreeMap<String, usize>,
    rows: Vec<MappingRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(deny_unknown_fields)]
struct MappingRow {
    stylesheet: String,
    selector: String,
    property: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompatibilityFixture {
    external_custom_properties: Vec<String>,
    runtime_geometry_properties: Vec<String>,
}

#[test]
fn component_mapping_table_matches_complete_theme_fallback_semantics() {
    let fixture: MappingFixture =
        serde_json::from_str(MAPPING_FIXTURE).expect("parse component mapping fixture");
    let compatibility: CompatibilityFixture =
        serde_json::from_str(COMPATIBILITY_FIXTURE).expect("parse pinned compatibility fixture");

    assert_eq!(fixture.fixture_version, 1);
    assert_eq!(fixture.row_count, 256);
    assert_eq!(fixture.rows.len(), fixture.row_count);
    assert_eq!(
        fixture.rows_by_stylesheet,
        BTreeMap::from([
            ("alert.css".to_owned(), 4),
            ("anchor.css".to_owned(), 8),
            ("avatar.css".to_owned(), 1),
            ("badge.css".to_owned(), 3),
            ("button.css".to_owned(), 31),
            ("card.css".to_owned(), 5),
            ("checkbox.css".to_owned(), 8),
            ("collapsible.css".to_owned(), 13),
            ("dialog.css".to_owned(), 48),
            ("field.css".to_owned(), 45),
            ("menu.css".to_owned(), 38),
            ("progress.css".to_owned(), 3),
            ("radio.css".to_owned(), 6),
            ("separator.css".to_owned(), 3),
            ("skeleton.css".to_owned(), 2),
            ("spinner.css".to_owned(), 7),
            ("status.css".to_owned(), 4),
            ("switch.css".to_owned(), 7),
            ("tabs.css".to_owned(), 20),
        ])
    );
    assert_strictly_sorted_unique(&fixture.rows);

    let actual = current_component_mapping_rows();
    assert_eq!(actual.len(), fixture.row_count);
    assert_eq!(
        actual, fixture.rows,
        "complete component declaration mapping drifted"
    );

    let mut current_names = BTreeSet::new();
    for row in &actual {
        current_names.extend(custom_property_names(&row.property));
        current_names.extend(custom_property_names(&row.value));
    }

    let pinned_external = compatibility
        .external_custom_properties
        .into_iter()
        .collect::<BTreeSet<_>>();
    let runtime_geometry = compatibility
        .runtime_geometry_properties
        .into_iter()
        .collect::<BTreeSet<_>>();
    let contract_names = load_built_in_theme_contract()
        .expect("load theme contract")
        .tokens
        .into_iter()
        .map(|token| token.name)
        .collect::<BTreeSet<_>>();
    let customization = load_built_in_component_customization_contract()
        .expect("load component customization contract");
    let customization_names = customization
        .properties
        .iter()
        .map(|property| property.name.clone())
        .collect::<BTreeSet<_>>();
    let component_radius_names = customization
        .properties
        .iter()
        .filter(|property| property.scope == ComponentCustomizationScope::Component)
        .map(|property| property.name.clone())
        .collect::<BTreeSet<_>>();
    let semantic_radius_names = customization
        .properties
        .iter()
        .filter(|property| property.scope == ComponentCustomizationScope::Semantic)
        .map(|property| property.name.clone())
        .collect::<BTreeSet<_>>();
    let geometry_critical_names = customization
        .properties
        .iter()
        .filter(|property| property.geometry_critical)
        .map(|property| property.name.clone())
        .collect::<BTreeSet<_>>();

    let missing_external = pinned_external
        .difference(&current_names)
        .collect::<Vec<_>>();
    assert!(
        missing_external.is_empty(),
        "historical overrides missing from complete declaration values: {missing_external:?}"
    );
    assert!(runtime_geometry.is_disjoint(&contract_names));
    assert!(runtime_geometry.is_subset(&current_names));
    let missing_customization = customization_names
        .difference(&current_names)
        .collect::<Vec<_>>();
    assert!(
        missing_customization.is_empty(),
        "declared component customization properties missing from CSS: {missing_customization:?}"
    );

    let approved_names = pinned_external
        .union(&runtime_geometry)
        .cloned()
        .collect::<BTreeSet<_>>()
        .union(&contract_names)
        .cloned()
        .collect::<BTreeSet<_>>()
        .union(&customization_names)
        .cloned()
        .collect::<BTreeSet<_>>();
    let unapproved = current_names
        .difference(&approved_names)
        .collect::<Vec<_>>();
    assert!(
        unapproved.is_empty(),
        "unapproved component tokens: {unapproved:?}"
    );
    assert_eq!(current_names.len(), 251);
    assert!(current_names.contains("--kit-button-radius"));
    assert!(current_names.contains("--kit-spinner-radius"));

    for row in actual.iter().filter(|row| row.property == "border-radius") {
        let names = custom_property_names(&row.value);
        assert!(
            !names.is_disjoint(&component_radius_names),
            "border-radius declaration lacks an exact component property: {row:?}"
        );
        if !names.is_disjoint(&geometry_critical_names) {
            assert!(
                names.is_disjoint(&semantic_radius_names),
                "geometry-critical radius inherits a semantic/global radius: {row:?}"
            );
        }
    }

    assert_required_corrected_rows(&actual);
    assert_runtime_geometry_rows(&actual, &runtime_geometry);
}

fn current_component_mapping_rows() -> Vec<MappingRow> {
    let root = load_built_in_registry_root().expect("load registry root");
    let mut style_sources = BTreeSet::new();

    for entry in root.items {
        let item = load_built_in_registry_item(&entry.name)
            .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
        if item.item.name == "tokens" {
            continue;
        }
        for style in item.item.styles {
            style_sources.insert(style.source);
        }
    }

    let mut rows = Vec::new();
    for source in style_sources {
        let stylesheet = source
            .rsplit_once('/')
            .map_or(source.as_str(), |(_, file_name)| file_name);
        let css = read_built_in_registry_source(&source)
            .unwrap_or_else(|error| panic!("read {source}: {error}"));
        rows.extend(
            parse_qualified_rules(stylesheet, &css)
                .unwrap_or_else(|error| panic!("parse {source}: {error}")),
        );
    }

    rows.sort();
    assert_strictly_sorted_unique(&rows);
    rows
}

fn parse_qualified_rules(stylesheet: &str, input: &str) -> Result<Vec<MappingRow>, String> {
    let bytes = input.as_bytes();
    let mut rows = Vec::new();
    let mut cursor = 0;

    while cursor < bytes.len() {
        skip_whitespace_and_comments(bytes, &mut cursor)?;
        if cursor == bytes.len() {
            break;
        }

        let open = find_next_unquoted(bytes, cursor, b'{')?
            .ok_or_else(|| "qualified rule is missing an opening brace".to_owned())?;
        let prelude = normalize_css_fragment(&input[cursor..open])?;
        let close = find_matching_brace(bytes, open)?;
        let body = &input[open + 1..close];
        cursor = close + 1;

        if prelude.starts_with("@layer") {
            rows.extend(parse_qualified_rules(stylesheet, body)?);
            continue;
        }
        if prelude.starts_with('@') {
            continue;
        }

        let selectors = split_top_level(&prelude, b',')?
            .into_iter()
            .map(normalize_css_fragment)
            .collect::<Result<Vec<_>, _>>()?;
        for declaration in split_top_level(body, b';')? {
            let declaration = strip_css_comments(declaration)?.trim().to_owned();
            if declaration.is_empty() {
                continue;
            }
            let Some(colon) = find_top_level_byte(&declaration, b':')? else {
                return Err(format!("declaration lacks a colon: {declaration}"));
            };
            let property = declaration[..colon].trim().to_owned();
            let value = normalize_css_fragment(&declaration[colon + 1..])?;
            let in_scope = property.starts_with("--kit-")
                || value.contains("var(--kit-")
                || (stylesheet == "spinner.css" && property == "color" && value == "currentColor");
            if !in_scope {
                continue;
            }

            for selector in &selectors {
                rows.push(MappingRow {
                    stylesheet: stylesheet.to_owned(),
                    selector: selector.clone(),
                    property: property.clone(),
                    value: value.clone(),
                });
            }
        }
    }

    Ok(rows)
}

fn skip_whitespace_and_comments(bytes: &[u8], cursor: &mut usize) -> Result<(), String> {
    loop {
        while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
            *cursor += 1;
        }
        if bytes
            .get(*cursor..)
            .is_some_and(|rest| rest.starts_with(b"/*"))
        {
            *cursor = comment_end(bytes, *cursor)?;
        } else {
            return Ok(());
        }
    }
}

fn comment_end(bytes: &[u8], start: usize) -> Result<usize, String> {
    let mut cursor = start + 2;
    while cursor + 1 < bytes.len() {
        if bytes[cursor..].starts_with(b"*/") {
            return Ok(cursor + 2);
        }
        cursor += 1;
    }
    Err("unterminated CSS comment".to_owned())
}

fn find_next_unquoted(bytes: &[u8], start: usize, needle: u8) -> Result<Option<usize>, String> {
    let mut cursor = start;
    let mut quote = None;
    let mut escaped = false;
    while cursor < bytes.len() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if bytes[cursor] == b'\\' {
                escaped = true;
            } else if bytes[cursor] == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if bytes[cursor..].starts_with(b"/*") {
            cursor = comment_end(bytes, cursor)?;
            continue;
        }
        if matches!(bytes[cursor], b'\'' | b'"') {
            quote = Some(bytes[cursor]);
        } else if bytes[cursor] == needle {
            return Ok(Some(cursor));
        }
        cursor += 1;
    }
    if quote.is_some() || escaped {
        Err("unterminated CSS string".to_owned())
    } else {
        Ok(None)
    }
}

fn find_matching_brace(bytes: &[u8], open: usize) -> Result<usize, String> {
    let mut cursor = open + 1;
    let mut depth = 1usize;
    let mut quote = None;
    let mut escaped = false;
    while cursor < bytes.len() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if bytes[cursor] == b'\\' {
                escaped = true;
            } else if bytes[cursor] == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if bytes[cursor..].starts_with(b"/*") {
            cursor = comment_end(bytes, cursor)?;
            continue;
        }
        match bytes[cursor] {
            b'\'' | b'"' => quote = Some(bytes[cursor]),
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(cursor);
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    Err("unterminated CSS rule".to_owned())
}

fn split_top_level(input: &str, separator: u8) -> Result<Vec<&str>, String> {
    let bytes = input.as_bytes();
    let mut output = Vec::new();
    let mut start = 0;
    let mut cursor = 0;
    let mut parentheses = 0usize;
    let mut brackets = 0usize;
    let mut quote = None;
    let mut escaped = false;

    while cursor < bytes.len() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if bytes[cursor] == b'\\' {
                escaped = true;
            } else if bytes[cursor] == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if bytes[cursor..].starts_with(b"/*") {
            cursor = comment_end(bytes, cursor)?;
            continue;
        }
        match bytes[cursor] {
            b'\'' | b'"' => quote = Some(bytes[cursor]),
            b'(' => parentheses += 1,
            b')' => parentheses = parentheses.saturating_sub(1),
            b'[' => brackets += 1,
            b']' => brackets = brackets.saturating_sub(1),
            byte if byte == separator && parentheses == 0 && brackets == 0 => {
                output.push(&input[start..cursor]);
                start = cursor + 1;
            }
            _ => {}
        }
        cursor += 1;
    }
    if quote.is_some() || escaped || parentheses != 0 || brackets != 0 {
        return Err("unterminated CSS string or grouping".to_owned());
    }
    output.push(&input[start..]);
    Ok(output)
}

fn find_top_level_byte(input: &str, needle: u8) -> Result<Option<usize>, String> {
    let bytes = input.as_bytes();
    let mut parentheses = 0usize;
    let mut brackets = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut cursor = 0;
    while cursor < bytes.len() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if bytes[cursor] == b'\\' {
                escaped = true;
            } else if bytes[cursor] == delimiter {
                quote = None;
            }
        } else {
            match bytes[cursor] {
                b'\'' | b'"' => quote = Some(bytes[cursor]),
                b'(' => parentheses += 1,
                b')' => parentheses = parentheses.saturating_sub(1),
                b'[' => brackets += 1,
                b']' => brackets = brackets.saturating_sub(1),
                byte if byte == needle && parentheses == 0 && brackets == 0 => {
                    return Ok(Some(cursor));
                }
                _ => {}
            }
        }
        cursor += 1;
    }
    if quote.is_some() || escaped || parentheses != 0 || brackets != 0 {
        Err("unterminated CSS string or grouping".to_owned())
    } else {
        Ok(None)
    }
}

fn strip_css_comments(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    let mut quote = None;
    let mut escaped = false;
    while cursor < bytes.len() {
        if let Some(delimiter) = quote {
            output.push(bytes[cursor] as char);
            if escaped {
                escaped = false;
            } else if bytes[cursor] == b'\\' {
                escaped = true;
            } else if bytes[cursor] == delimiter {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if bytes[cursor..].starts_with(b"/*") {
            cursor = comment_end(bytes, cursor)?;
            output.push(' ');
            continue;
        }
        if matches!(bytes[cursor], b'\'' | b'"') {
            quote = Some(bytes[cursor]);
        }
        output.push(bytes[cursor] as char);
        cursor += 1;
    }
    if quote.is_some() || escaped {
        Err("unterminated CSS string".to_owned())
    } else {
        Ok(output)
    }
}

fn normalize_css_fragment(input: &str) -> Result<String, String> {
    let without_comments = strip_css_comments(input)?;
    let bytes = without_comments.as_bytes();
    let mut output = String::with_capacity(without_comments.len());
    let mut quote = None;
    let mut escaped = false;
    let mut pending_space = false;

    for byte in bytes {
        if let Some(delimiter) = quote {
            output.push(*byte as char);
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(*byte, b'\'' | b'"') {
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            pending_space = false;
            quote = Some(*byte);
            output.push(*byte as char);
        } else if byte.is_ascii_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            pending_space = false;
            output.push(*byte as char);
        }
    }

    Ok(output.trim().to_owned())
}

fn custom_property_names(input: &str) -> BTreeSet<String> {
    let bytes = input.as_bytes();
    let mut names = BTreeSet::new();
    let mut cursor = 0;
    while cursor + 6 <= bytes.len() {
        if !bytes[cursor..].starts_with(b"--kit-") {
            cursor += 1;
            continue;
        }
        let mut end = cursor + 6;
        while end < bytes.len()
            && (bytes[end].is_ascii_lowercase()
                || bytes[end].is_ascii_digit()
                || bytes[end] == b'-')
        {
            end += 1;
        }
        names.insert(String::from_utf8(bytes[cursor..end].to_vec()).expect("ASCII token"));
        cursor = end;
    }
    names
}

fn assert_strictly_sorted_unique(rows: &[MappingRow]) {
    for pair in rows.windows(2) {
        assert!(pair[0] < pair[1], "mapping rows must be sorted and unique");
    }
}

fn assert_required_corrected_rows(rows: &[MappingRow]) {
    let rows = rows.iter().cloned().collect::<BTreeSet<_>>();
    let expected = [
        MappingRow {
            stylesheet: "menu.css".to_owned(),
            selector: ".kit-menu-item".to_owned(),
            property: "border-radius".to_owned(),
            value: "var( --kit-menu-item-radius, var( --kit-radius-control, var(--kit-radius-default, calc(var(--kit-radius-md) - 2px)) ) )".to_owned(),
        },
        border_row(
            "collapsible.css",
            ".kit-collapsible-trigger",
            "--kit-collapsible-trigger-border-color",
        ),
        border_row(
            "dialog.css",
            ".kit-dialog-trigger",
            "--kit-dialog-trigger-border-color",
        ),
        border_row(
            "dialog.css",
            ".kit-dialog-close",
            "--kit-dialog-trigger-border-color",
        ),
        border_row(
            "menu.css",
            ".kit-menu-trigger",
            "--kit-menu-trigger-border-color",
        ),
        border_row(
            "tabs.css",
            ".kit-tabs-trigger",
            "--kit-tabs-trigger-border-color",
        ),
    ];
    for row in expected {
        assert!(
            rows.contains(&row),
            "missing corrected mapping row: {row:?}"
        );
        if row.property == "border" {
            assert!(!row.value.contains("1px"));
        }
    }
    assert!(
        rows.iter()
            .all(|row| !row.value.contains("calc(var(--kit-menu-item-radius"))
    );
}

fn border_row(stylesheet: &str, selector: &str, color_property: &str) -> MappingRow {
    MappingRow {
        stylesheet: stylesheet.to_owned(),
        selector: selector.to_owned(),
        property: "border".to_owned(),
        value: format!(
            "var(--kit-border-width) solid var({color_property}, var(--kit-color-border))"
        ),
    }
}

fn assert_runtime_geometry_rows(rows: &[MappingRow], geometry: &BTreeSet<String>) {
    let actual = rows
        .iter()
        .filter(|row| {
            geometry.contains(&row.property)
                || geometry.iter().any(|property| row.value.contains(property))
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from([
        geometry_row(".kit-menu-content", "--kit-menu-content-translate-x", "0"),
        geometry_row(".kit-menu-content", "--kit-menu-content-translate-y", "0"),
        geometry_row(
            ".kit-menu-content[data-state=\"closed\"][data-side=\"bottom\"]",
            "--kit-menu-content-translate-y",
            "0.25rem",
        ),
        geometry_row(
            ".kit-menu-content[data-state=\"closed\"][data-side=\"top\"]",
            "--kit-menu-content-translate-y",
            "-0.25rem",
        ),
        geometry_row(
            ".kit-menu-content[data-state=\"closed\"][data-side=\"right\"]",
            "--kit-menu-content-translate-x",
            "0.25rem",
        ),
        geometry_row(
            ".kit-menu-content[data-state=\"closed\"][data-side=\"left\"]",
            "--kit-menu-content-translate-x",
            "-0.25rem",
        ),
        MappingRow {
            stylesheet: "menu.css".to_owned(),
            selector: ".kit-menu-content".to_owned(),
            property: "transform".to_owned(),
            value: "translate( var(--kit-menu-content-translate-x), var(--kit-menu-content-translate-y) )"
                .to_owned(),
        },
    ]);
    assert_eq!(
        actual, expected,
        "runtime menu geometry must remain local and exact"
    );
}

fn geometry_row(selector: &str, property: &str, value: &str) -> MappingRow {
    MappingRow {
        stylesheet: "menu.css".to_owned(),
        selector: selector.to_owned(),
        property: property.to_owned(),
        value: value.to_owned(),
    }
}
