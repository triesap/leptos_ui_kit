use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const TOKEN_CONTRACT_SCHEMA_URL: &str =
    "https://triesap.github.io/leptos_ui_kit/schema/0.2.0/token-contract.schema.json";
pub const THEME_INTEGRATION_SCHEMA_URL: &str =
    "https://triesap.github.io/leptos_ui_kit/schema/0.2.0/theme-integration.schema.json";
pub const CONTRACT_ID: &str = "leptos-ui-kit";
pub const CONTRACT_ABI_VERSION: u32 = 1;
pub const CONTRACT_REVISION: u32 = 2;

struct TokenDef {
    path: &'static str,
    token_type: &'static str,
    property: &'static str,
    domain: &'static str,
    default_json: &'static str,
}

macro_rules! token {
    ($path:literal, $type:literal, $property:literal, $domain:literal, $default:literal) => {
        TokenDef {
            path: $path,
            token_type: $type,
            property: $property,
            domain: $domain,
            default_json: $default,
        }
    };
}

macro_rules! color {
    ($path:literal, $property:literal, $default:literal) => {
        token!($path, "color", $property, "theme", $default)
    };
}

const TOKENS: &[TokenDef] = &[
    color!("color.canvas", "--kit-color-canvas", "\"#f8fafc\""),
    color!("color.surface", "--kit-color-surface", "\"#ffffff\""),
    color!(
        "color.surface-raised",
        "--kit-color-surface-raised",
        "\"#ffffff\""
    ),
    color!(
        "color.surface-hover",
        "--kit-color-surface-hover",
        "\"#f3f4f6\""
    ),
    color!(
        "color.surface-active",
        "--kit-color-surface-active",
        "\"#e5e7eb\""
    ),
    color!("color.text", "--kit-color-text", "\"#111827\""),
    color!(
        "color.text-secondary",
        "--kit-color-text-secondary",
        "\"#374151\""
    ),
    color!("color.text-muted", "--kit-color-text-muted", "\"#4b5563\""),
    color!("color.border", "--kit-color-border", "\"#d1d5db\""),
    color!(
        "color.border-strong",
        "--kit-color-border-strong",
        "\"#9ca3af\""
    ),
    color!("color.primary", "--kit-color-primary", "\"#111827\""),
    color!(
        "color.primary-hover",
        "--kit-color-primary-hover",
        "\"#1f2937\""
    ),
    color!(
        "color.primary-foreground",
        "--kit-color-primary-foreground",
        "\"#ffffff\""
    ),
    color!(
        "color.selection-indicator",
        "--kit-color-selection-indicator",
        "\"#ffffff\""
    ),
    color!("color.secondary", "--kit-color-secondary", "\"#ffffff\""),
    color!(
        "color.secondary-hover",
        "--kit-color-secondary-hover",
        "\"#f3f4f6\""
    ),
    color!(
        "color.secondary-foreground",
        "--kit-color-secondary-foreground",
        "\"#111827\""
    ),
    color!("color.accent", "--kit-color-accent", "\"#2563eb\""),
    color!(
        "color.accent-hover",
        "--kit-color-accent-hover",
        "\"#1d4ed8\""
    ),
    color!(
        "color.accent-foreground",
        "--kit-color-accent-foreground",
        "\"#ffffff\""
    ),
    color!("color.info", "--kit-color-info", "\"#0284c7\""),
    color!(
        "color.info-foreground",
        "--kit-color-info-foreground",
        "\"#ffffff\""
    ),
    color!("color.success", "--kit-color-success", "\"#16a34a\""),
    color!(
        "color.success-foreground",
        "--kit-color-success-foreground",
        "\"#ffffff\""
    ),
    color!("color.warning", "--kit-color-warning", "\"#d97706\""),
    color!(
        "color.warning-foreground",
        "--kit-color-warning-foreground",
        "\"#111827\""
    ),
    color!("color.danger", "--kit-color-danger", "\"#dc2626\""),
    color!(
        "color.danger-hover",
        "--kit-color-danger-hover",
        "\"#b91c1c\""
    ),
    color!(
        "color.danger-foreground",
        "--kit-color-danger-foreground",
        "\"#ffffff\""
    ),
    color!("color.link", "--kit-color-link", "\"#111827\""),
    color!("color.link-hover", "--kit-color-link-hover", "\"#111827\""),
    color!("color.focus-ring", "--kit-focus-ring", "\"#2563eb\""),
    token!(
        "radius.sm",
        "dimension",
        "--kit-radius-sm",
        "theme",
        r#"{"value":0.25,"unit":"rem"}"#
    ),
    token!(
        "radius.md",
        "dimension",
        "--kit-radius-md",
        "theme",
        r#"{"value":0.375,"unit":"rem"}"#
    ),
    token!(
        "radius.lg",
        "dimension",
        "--kit-radius-lg",
        "theme",
        r#"{"value":0.5,"unit":"rem"}"#
    ),
    token!(
        "radius.full",
        "dimension",
        "--kit-radius-full",
        "theme",
        r#"{"value":999,"unit":"px"}"#
    ),
    token!(
        "border.width",
        "dimension",
        "--kit-border-width",
        "theme",
        r#"{"value":1,"unit":"px"}"#
    ),
    token!(
        "shadow.sm",
        "shadow",
        "--kit-shadow-sm",
        "theme",
        r##"{"color":"#0f172a14","offsetX":{"value":0,"unit":"px"},"offsetY":{"value":1,"unit":"px"},"blur":{"value":2,"unit":"px"},"spread":{"value":0,"unit":"px"}}"##
    ),
    token!(
        "shadow.md",
        "shadow",
        "--kit-shadow-md",
        "theme",
        r##"{"color":"#0f172a24","offsetX":{"value":0,"unit":"px"},"offsetY":{"value":12,"unit":"px"},"blur":{"value":28,"unit":"px"},"spread":{"value":0,"unit":"px"}}"##
    ),
    token!(
        "shadow.lg",
        "shadow",
        "--kit-shadow-lg",
        "theme",
        r##"{"color":"#0f172a2e","offsetX":{"value":0,"unit":"px"},"offsetY":{"value":20,"unit":"px"},"blur":{"value":40,"unit":"px"},"spread":{"value":0,"unit":"px"}}"##
    ),
    token!(
        "motion.duration-fast",
        "duration",
        "--kit-duration-fast",
        "motion",
        r#"{"value":120,"unit":"ms"}"#
    ),
    token!(
        "motion.duration-normal",
        "duration",
        "--kit-duration-normal",
        "motion",
        r#"{"value":140,"unit":"ms"}"#
    ),
    token!(
        "motion.easing-standard",
        "cubicBezier",
        "--kit-easing-standard",
        "motion",
        "[0.2,0,0,1]"
    ),
    token!(
        "state.disabled-opacity",
        "number",
        "--kit-disabled-opacity",
        "contrast",
        "0.55"
    ),
];

pub fn token_contract_json() -> Result<String, String> {
    let tokens = TOKENS
        .iter()
        .enumerate()
        .map(|(index, token)| {
            let default: Value = serde_json::from_str(token.default_json).map_err(|error| {
                format!("invalid token ABI default for {}: {error}", token.path)
            })?;
            Ok(json!({
                "path": token.path,
                "type": token.token_type,
                "cssCustomProperty": token.property,
                "domain": token.domain,
                "required": true,
                "order": index + 1,
                "themeOverride": true,
                "default": default
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let contrast_checks = [
        ("text-canvas", "color.text", "color.canvas", "text", 4.5),
        (
            "muted-canvas",
            "color.text-muted",
            "color.canvas",
            "text",
            4.5,
        ),
        (
            "primary-action",
            "color.primary-foreground",
            "color.primary",
            "text",
            4.5,
        ),
        (
            "secondary-action",
            "color.secondary-foreground",
            "color.secondary",
            "text",
            4.5,
        ),
        (
            "accent-action",
            "color.accent-foreground",
            "color.accent",
            "text",
            4.5,
        ),
        (
            "danger-action",
            "color.danger-foreground",
            "color.danger",
            "text",
            4.5,
        ),
        (
            "focus-canvas",
            "color.focus-ring",
            "color.canvas",
            "focus-indicator",
            3.0,
        ),
    ]
    .into_iter()
    .map(|(id, foreground, background, kind, minimum)| {
        json!({
            "id": id,
            "foreground": foreground,
            "background": background,
            "kind": kind,
            "minimum": minimum
        })
    })
    .collect::<Vec<_>>();
    let mut contract = json!({
        "$schema": TOKEN_CONTRACT_SCHEMA_URL,
        "schemaVersion": "1.0.0",
        "contractId": CONTRACT_ID,
        "abiVersion": CONTRACT_ABI_VERSION,
        "revision": CONTRACT_REVISION,
        "dtcgVersion": "2025.10",
        "dtcgProfile": "format+color+resolver:2025.10",
        "canonicalDigest": "",
        "tokens": tokens,
        "contrastChecks": contrast_checks,
        "extensions": {}
    });
    let mut semantic = contract.clone();
    semantic
        .as_object_mut()
        .expect("contract is an object")
        .remove("canonicalDigest");
    let canonical = serde_json_canonicalizer::to_vec(&semantic)
        .map_err(|error| format!("cannot canonicalize token ABI: {error}"))?;
    contract["canonicalDigest"] = Value::String(format!("sha256:{:x}", Sha256::digest(canonical)));
    serde_json::to_string_pretty(&contract)
        .map(|json| format!("{json}\n"))
        .map_err(|error| format!("cannot serialize token ABI: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{TOKENS, token_contract_json};

    #[test]
    fn token_contract_is_explicit_complete_and_stable() {
        let first = token_contract_json().unwrap();
        assert_eq!(first, token_contract_json().unwrap());
        let value: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(value["tokens"].as_array().unwrap().len(), TOKENS.len());
        assert_eq!(TOKENS.len(), 44);
        assert!(
            value["canonicalDigest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }
}
