use std::{
    collections::BTreeSet,
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::SCHEMA_VERSION;

pub const THEME_CONTRACT_SCHEMA_URL: &str =
    "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/theme-contract.schema.json";
pub const THEME_CONTRACT_VERSION: &str = "1";
pub const THEME_CONTRACT_NAME: &str = "leptos-ui-kit-semantic";

#[derive(Debug)]
pub enum ThemeContractError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse(serde_json::Error),
    InvalidValue {
        field: &'static str,
        expected: String,
        actual: String,
    },
    DuplicateToken(String),
}

impl fmt::Display for ThemeContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    f,
                    "failed to read theme contract {}: {source}",
                    path.display()
                )
            }
            Self::Parse(error) => write!(f, "failed to parse theme contract: {error}"),
            Self::InvalidValue {
                field,
                expected,
                actual,
            } => write!(
                f,
                "invalid theme contract value for {field}: expected {expected}, got {actual}"
            ),
            Self::DuplicateToken(name) => write!(f, "duplicate theme contract token: {name}"),
        }
    }
}

impl std::error::Error for ThemeContractError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeContract {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: String,
    pub contract_version: String,
    pub name: String,
    pub tokens: Vec<ThemeToken>,
}

impl ThemeContract {
    pub fn validate(&self) -> Result<(), ThemeContractError> {
        expect_string("$schema", THEME_CONTRACT_SCHEMA_URL, &self.schema)?;
        expect_string("schemaVersion", SCHEMA_VERSION, &self.schema_version)?;
        expect_string(
            "contractVersion",
            THEME_CONTRACT_VERSION,
            &self.contract_version,
        )?;
        expect_string("name", THEME_CONTRACT_NAME, &self.name)?;

        if self.tokens.is_empty() {
            return Err(ThemeContractError::InvalidValue {
                field: "tokens",
                expected: "at least one token".to_owned(),
                actual: "empty array".to_owned(),
            });
        }

        let mut names = BTreeSet::new();
        for token in &self.tokens {
            token.validate()?;
            if !names.insert(token.name.clone()) {
                return Err(ThemeContractError::DuplicateToken(token.name.clone()));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeToken {
    pub name: String,
    pub category: ThemeTokenCategory,
    pub required: bool,
    #[serde(rename = "default")]
    pub default_value: String,
    pub description: String,
}

impl ThemeToken {
    fn validate(&self) -> Result<(), ThemeContractError> {
        validate_theme_token_name(&self.name)?;
        validate_non_empty("tokens[].default", &self.default_value)?;
        validate_non_empty("tokens[].description", &self.description)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeTokenCategory {
    Color,
    Shape,
    Elevation,
    Motion,
    State,
}

pub fn parse_theme_contract_str(input: &str) -> Result<ThemeContract, ThemeContractError> {
    let contract: ThemeContract = serde_json::from_str(input).map_err(ThemeContractError::Parse)?;
    contract.validate()?;
    Ok(contract)
}

pub fn load_built_in_theme_contract() -> Result<ThemeContract, ThemeContractError> {
    let path = built_in_theme_contract_path();
    let input = fs::read_to_string(&path).map_err(|source| ThemeContractError::Io {
        path: path.clone(),
        source,
    })?;
    parse_theme_contract_str(&input)
}

fn built_in_theme_contract_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("registry")
        .join("contracts")
        .join("theme-v1.json")
}

fn expect_string(
    field: &'static str,
    expected: &str,
    actual: &str,
) -> Result<(), ThemeContractError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ThemeContractError::InvalidValue {
            field,
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        })
    }
}

fn validate_theme_token_name(value: &str) -> Result<(), ThemeContractError> {
    let Some(suffix) = value.strip_prefix("--kit-") else {
        return Err(ThemeContractError::InvalidValue {
            field: "tokens[].name",
            expected: "a --kit- prefixed semantic token name".to_owned(),
            actual: value.to_owned(),
        });
    };

    if suffix
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase())
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        Ok(())
    } else {
        Err(ThemeContractError::InvalidValue {
            field: "tokens[].name",
            expected: "a --kit- prefixed lowercase kebab-case token name".to_owned(),
            actual: value.to_owned(),
        })
    }
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), ThemeContractError> {
    if value.is_empty() {
        Err(ThemeContractError::InvalidValue {
            field,
            expected: "a non-empty string".to_owned(),
            actual: value.to_owned(),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
        path::Path,
    };

    use serde_json::json;

    use super::{
        THEME_CONTRACT_NAME, THEME_CONTRACT_SCHEMA_URL, THEME_CONTRACT_VERSION, ThemeContract,
        ThemeContractError, ThemeToken, ThemeTokenCategory, load_built_in_theme_contract,
        parse_theme_contract_str,
    };
    use crate::{SCHEMA_VERSION, read_built_in_registry_source};

    fn valid_contract() -> ThemeContract {
        ThemeContract {
            schema: THEME_CONTRACT_SCHEMA_URL.to_owned(),
            schema_version: SCHEMA_VERSION.to_owned(),
            contract_version: THEME_CONTRACT_VERSION.to_owned(),
            name: THEME_CONTRACT_NAME.to_owned(),
            tokens: vec![ThemeToken {
                name: "--kit-color-surface".to_owned(),
                category: ThemeTokenCategory::Color,
                required: true,
                default_value: "#ffffff".to_owned(),
                description: "Default component surface.".to_owned(),
            }],
        }
    }

    #[test]
    fn loads_built_in_contract() {
        let contract = load_built_in_theme_contract().expect("load built-in contract");

        assert_eq!(contract.contract_version, THEME_CONTRACT_VERSION);
        assert_eq!(contract.name, THEME_CONTRACT_NAME);
        assert!(contract.tokens.len() > 40);
        assert!(
            contract
                .tokens
                .iter()
                .any(|token| token.name == "--kit-color-surface")
        );
    }

    #[test]
    fn rejects_unknown_contract_fields() {
        let error = parse_theme_contract_str(
            r#"{
              "$schema": "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/theme-contract.schema.json",
              "schemaVersion": "0.9.0-alpha",
              "contractVersion": "1",
              "name": "leptos-ui-kit-semantic",
              "tokens": [],
              "unexpected": true
            }"#,
        )
        .expect_err("unknown fields should fail");

        assert!(matches!(error, ThemeContractError::Parse(_)));
    }

    #[test]
    fn rejects_invalid_contract_token_values() {
        let mut contract = valid_contract();
        contract.tokens.push(contract.tokens[0].clone());
        assert!(matches!(
            contract.validate(),
            Err(ThemeContractError::DuplicateToken(_))
        ));

        let mut contract = valid_contract();
        contract.tokens[0].name = "--other-color".to_owned();
        assert!(matches!(
            contract.validate(),
            Err(ThemeContractError::InvalidValue {
                field: "tokens[].name",
                ..
            })
        ));

        let mut contract = valid_contract();
        contract.tokens[0].default_value.clear();
        assert!(matches!(
            contract.validate(),
            Err(ThemeContractError::InvalidValue {
                field: "tokens[].default",
                ..
            })
        ));

        let mut contract = valid_contract();
        contract.tokens[0].description.clear();
        assert!(matches!(
            contract.validate(),
            Err(ThemeContractError::InvalidValue {
                field: "tokens[].description",
                ..
            })
        ));
    }

    #[test]
    fn rejects_unsupported_contract_version_and_category() {
        let mut contract = valid_contract();
        contract.contract_version = "2".to_owned();
        assert!(matches!(
            contract.validate(),
            Err(ThemeContractError::InvalidValue {
                field: "contractVersion",
                ..
            })
        ));

        let invalid_category = json!({
            "$schema": THEME_CONTRACT_SCHEMA_URL,
            "schemaVersion": SCHEMA_VERSION,
            "contractVersion": THEME_CONTRACT_VERSION,
            "name": THEME_CONTRACT_NAME,
            "tokens": [{
                "name": "--kit-color-surface",
                "category": "palette",
                "required": true,
                "default": "#ffffff",
                "description": "Default component surface."
            }]
        });
        let error = parse_theme_contract_str(&invalid_category.to_string())
            .expect_err("unsupported category should fail");
        assert!(matches!(error, ThemeContractError::Parse(_)));
    }

    #[test]
    fn serializes_deterministically() {
        let contract = load_built_in_theme_contract().expect("load built-in contract");
        let first = serde_json::to_string(&contract).expect("serialize contract");
        let second = serde_json::to_string(&contract).expect("serialize contract");

        assert_eq!(first, second);
    }

    #[test]
    fn package_schema_declares_the_public_contract_identity() {
        let package_schema = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schema/0.9.0-alpha/theme-contract.schema.json");

        let package_value = serde_json::from_str::<serde_json::Value>(
            &fs::read_to_string(package_schema).expect("read package schema"),
        )
        .expect("parse package schema");

        assert_eq!(package_value["$id"], json!(THEME_CONTRACT_SCHEMA_URL));
        assert_eq!(
            package_value["$schema"],
            json!("https://json-schema.org/draft/2020-12/schema")
        );
    }

    #[test]
    fn tokens_css_defaults_match_the_contract() {
        let contract = load_built_in_theme_contract().expect("load built-in contract");
        let css = read_built_in_registry_source("styles/tokens.css").expect("read tokens CSS");
        let defaults = css_token_defaults(&css);
        let contract_defaults = contract
            .tokens
            .into_iter()
            .map(|token| (token.name, token.default_value))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(defaults, contract_defaults);
        assert_eq!(css.matches(":root").count(), 1);
        assert!(css.contains("color-scheme: light;"));
    }

    #[test]
    fn built_in_component_css_enforces_theme_token_boundaries() {
        const COMPONENT_STYLES: [&str; 9] = [
            "anchor",
            "button",
            "collapsible",
            "dialog",
            "field",
            "menu",
            "spinner",
            "status",
            "tabs",
        ];

        let contract = load_built_in_theme_contract().expect("load built-in contract");
        let canonical_tokens = contract
            .tokens
            .iter()
            .map(|token| token.name.as_str())
            .collect::<BTreeSet<_>>();
        let tokens_css =
            read_built_in_registry_source("styles/tokens.css").expect("read tokens CSS");
        let token_declarations = css_custom_property_declarations(&tokens_css);

        assert_eq!(tokens_css.matches(":root").count(), 1);
        for token in &canonical_tokens {
            assert_eq!(
                token_declarations
                    .iter()
                    .filter(|name| name == token)
                    .count(),
                1,
                "tokens.css must declare {token} exactly once"
            );
        }

        for name in COMPONENT_STYLES {
            let css = read_built_in_registry_source(&format!("styles/{name}.css"))
                .unwrap_or_else(|error| panic!("read {name} CSS: {error}"));
            let declared_canonical_tokens = css_custom_property_declarations(&css)
                .into_iter()
                .filter(|token| canonical_tokens.contains(token.as_str()))
                .collect::<Vec<_>>();

            assert!(!css.contains(":root"), "{name}.css must not define :root");
            assert!(
                declared_canonical_tokens.is_empty(),
                "{name}.css redeclares canonical tokens: {declared_canonical_tokens:?}"
            );
            assert!(
                !contains_disallowed_theme_color_literal(&css),
                "{name}.css contains a literal theme color"
            );
            assert_eq!(
                css.matches(&format!("/* leptos-ui-kit:start {name} */"))
                    .count(),
                1,
                "{name}.css must contain one managed start marker"
            );
            assert_eq!(
                css.matches(&format!("/* leptos-ui-kit:end {name} */"))
                    .count(),
                1,
                "{name}.css must contain one managed end marker"
            );
        }
    }

    fn css_token_defaults(input: &str) -> BTreeMap<String, String> {
        input
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let (name, value) = line.split_once(':')?;
                name.starts_with("--kit-").then(|| {
                    (
                        name.to_owned(),
                        value.trim().trim_end_matches(';').trim().to_owned(),
                    )
                })
            })
            .collect()
    }

    fn css_custom_property_declarations(input: &str) -> Vec<String> {
        input
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let (name, _) = line.split_once(':')?;
                name.starts_with("--kit-").then(|| name.to_owned())
            })
            .collect()
    }

    fn contains_disallowed_theme_color_literal(input: &str) -> bool {
        let without_comments = input
            .lines()
            .filter(|line| !line.trim_start().starts_with("/*"))
            .collect::<Vec<_>>()
            .join("\n")
            .to_ascii_lowercase();

        without_comments.contains('#')
            || [
                "rgb(", "rgba(", "hsl(", "hsla(", "oklch(", "oklab(", "lab(", "lch(",
            ]
            .iter()
            .any(|syntax| without_comments.contains(syntax))
    }
}
