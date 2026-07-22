use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

pub const COMPONENT_CUSTOMIZATION_SCHEMA_URL: &str = "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/component-customization.schema.json";
pub const COMPONENT_CUSTOMIZATION_SCHEMA_VERSION: &str = "0.9.0-alpha";
pub const COMPONENT_CUSTOMIZATION_CONTRACT_ID: &str = "leptos-ui-kit-component-customization";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentCustomizationContract {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: String,
    pub contract_id: String,
    pub revision: u32,
    pub properties: Vec<ComponentCustomizationProperty>,
}

impl ComponentCustomizationContract {
    pub fn validate(&self) -> Result<(), ComponentCustomizationError> {
        expect("$schema", COMPONENT_CUSTOMIZATION_SCHEMA_URL, &self.schema)?;
        expect(
            "schemaVersion",
            COMPONENT_CUSTOMIZATION_SCHEMA_VERSION,
            &self.schema_version,
        )?;
        expect(
            "contractId",
            COMPONENT_CUSTOMIZATION_CONTRACT_ID,
            &self.contract_id,
        )?;
        if self.revision == 0 {
            return Err(ComponentCustomizationError::InvalidValue {
                field: "revision",
                reason: "must be at least 1".to_owned(),
            });
        }
        if self.properties.is_empty() {
            return Err(ComponentCustomizationError::InvalidValue {
                field: "properties",
                reason: "must contain at least one property".to_owned(),
            });
        }

        let mut by_name = BTreeMap::new();
        for property in &self.properties {
            property.validate()?;
            if by_name.insert(property.name.as_str(), property).is_some() {
                return Err(ComponentCustomizationError::DuplicateProperty(
                    property.name.clone(),
                ));
            }
        }
        for property in &self.properties {
            let mut visiting = BTreeSet::new();
            validate_acyclic(property.name.as_str(), &by_name, &mut visiting)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentCustomizationProperty {
    pub name: String,
    pub scope: ComponentCustomizationScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub css_property: CssProperty,
    pub value_kind: CssValueKind,
    pub role: RadiusRole,
    pub fallback: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub natural_fallback: Option<String>,
    pub inherits: bool,
    pub geometry_critical: bool,
    pub stability: CustomizationStability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<String>,
}

impl ComponentCustomizationProperty {
    fn validate(&self) -> Result<(), ComponentCustomizationError> {
        validate_custom_property_name(&self.name)?;
        match (&self.scope, &self.owner) {
            (ComponentCustomizationScope::Semantic, None) => {}
            (ComponentCustomizationScope::Component, Some(owner)) => {
                validate_owner(owner)?;
            }
            (ComponentCustomizationScope::Semantic, Some(_)) => {
                return Err(invalid(
                    &self.name,
                    "semantic properties must not name an owner",
                ));
            }
            (ComponentCustomizationScope::Component, None) => {
                return Err(invalid(
                    &self.name,
                    "component properties must name an owner",
                ));
            }
        }
        if !self.inherits {
            return Err(invalid(
                &self.name,
                "radius customization properties must inherit",
            ));
        }
        if self.geometry_critical != (self.role == RadiusRole::GeometryCritical) {
            return Err(invalid(
                &self.name,
                "geometryCritical must agree with the geometry-critical role",
            ));
        }
        if self.fallback.is_empty() {
            return Err(invalid(&self.name, "fallback must not be empty"));
        }
        let mut fallbacks = BTreeSet::new();
        for fallback in &self.fallback {
            validate_custom_property_name(fallback)?;
            if fallback == &self.name {
                return Err(invalid(&self.name, "fallback must not reference itself"));
            }
            if !fallbacks.insert(fallback) {
                return Err(invalid(&self.name, "fallback entries must be unique"));
            }
        }
        if self
            .natural_fallback
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(invalid(&self.name, "naturalFallback must not be blank"));
        }
        match self.stability {
            CustomizationStability::Deprecated => {
                if self.deprecated_since.as_deref().is_none_or(str::is_empty)
                    || self.replacement.as_deref().is_none_or(str::is_empty)
                {
                    return Err(invalid(
                        &self.name,
                        "deprecated properties require deprecatedSince and replacement",
                    ));
                }
            }
            CustomizationStability::Experimental | CustomizationStability::Stable => {
                if self.deprecated_since.is_some() || self.replacement.is_some() {
                    return Err(invalid(
                        &self.name,
                        "active properties must not carry deprecation metadata",
                    ));
                }
            }
        }
        if let Some(replacement) = &self.replacement {
            validate_custom_property_name(replacement)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentCustomizationScope {
    Semantic,
    Component,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CssProperty {
    BorderRadius,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CssValueKind {
    BorderRadius,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RadiusRole {
    Default,
    Control,
    Surface,
    Overlay,
    Indicator,
    GeometryCritical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CustomizationStability {
    Experimental,
    Stable,
    Deprecated,
}

#[derive(Debug)]
pub enum ComponentCustomizationError {
    Parse(serde_json::Error),
    BuiltInAsset(crate::RegistryError),
    InvalidValue { field: &'static str, reason: String },
    InvalidProperty { name: String, reason: String },
    DuplicateProperty(String),
    FallbackCycle(String),
}

impl fmt::Display for ComponentCustomizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => {
                write!(formatter, "invalid component customization JSON: {error}")
            }
            Self::BuiltInAsset(error) => write!(
                formatter,
                "cannot load component customization contract: {error}"
            ),
            Self::InvalidValue { field, reason } => write!(formatter, "invalid {field}: {reason}"),
            Self::InvalidProperty { name, reason } => {
                write!(
                    formatter,
                    "invalid component customization property {name}: {reason}"
                )
            }
            Self::DuplicateProperty(name) => write!(formatter, "duplicate property {name}"),
            Self::FallbackCycle(name) => write!(formatter, "fallback cycle through {name}"),
        }
    }
}

impl std::error::Error for ComponentCustomizationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::BuiltInAsset(error) => Some(error),
            _ => None,
        }
    }
}

pub fn parse_component_customization_contract_str(
    input: &str,
) -> Result<ComponentCustomizationContract, ComponentCustomizationError> {
    let contract = serde_json::from_str::<ComponentCustomizationContract>(input)
        .map_err(ComponentCustomizationError::Parse)?;
    contract.validate()?;
    Ok(contract)
}

pub fn load_built_in_component_customization_contract()
-> Result<ComponentCustomizationContract, ComponentCustomizationError> {
    let input = crate::read_built_in_asset("registry/contracts/component-customization-v1.json")
        .map_err(ComponentCustomizationError::BuiltInAsset)?;
    parse_component_customization_contract_str(&input)
}

fn validate_acyclic<'a>(
    name: &'a str,
    properties: &BTreeMap<&'a str, &'a ComponentCustomizationProperty>,
    visiting: &mut BTreeSet<&'a str>,
) -> Result<(), ComponentCustomizationError> {
    if !visiting.insert(name) {
        return Err(ComponentCustomizationError::FallbackCycle(name.to_owned()));
    }
    if let Some(property) = properties.get(name) {
        for fallback in &property.fallback {
            if properties.contains_key(fallback.as_str()) {
                validate_acyclic(fallback, properties, visiting)?;
            }
        }
    }
    visiting.remove(name);
    Ok(())
}

fn validate_custom_property_name(name: &str) -> Result<(), ComponentCustomizationError> {
    let suffix = name.strip_prefix("--kit-").ok_or_else(|| {
        ComponentCustomizationError::InvalidProperty {
            name: name.to_owned(),
            reason: "must start with --kit-".to_owned(),
        }
    })?;
    if suffix.is_empty()
        || suffix.starts_with('-')
        || suffix.ends_with('-')
        || suffix
            .bytes()
            .any(|byte| !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
    {
        return Err(ComponentCustomizationError::InvalidProperty {
            name: name.to_owned(),
            reason: "must use lowercase kebab-case".to_owned(),
        });
    }
    Ok(())
}

fn validate_owner(owner: &str) -> Result<(), ComponentCustomizationError> {
    if owner.is_empty()
        || !owner.as_bytes()[0].is_ascii_lowercase()
        || owner.starts_with('-')
        || owner.ends_with('-')
        || owner
            .bytes()
            .any(|byte| !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
    {
        return Err(ComponentCustomizationError::InvalidProperty {
            name: owner.to_owned(),
            reason: "owner must use lowercase kebab-case".to_owned(),
        });
    }
    Ok(())
}

fn expect(
    field: &'static str,
    expected: &str,
    actual: &str,
) -> Result<(), ComponentCustomizationError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ComponentCustomizationError::InvalidValue {
            field,
            reason: format!("expected {expected:?}, got {actual:?}"),
        })
    }
}

fn invalid(name: &str, reason: &str) -> ComponentCustomizationError {
    ComponentCustomizationError::InvalidProperty {
        name: name.to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_declared_fallback_cycles() {
        let input = format!(
            r#"{{
              "$schema":"{COMPONENT_CUSTOMIZATION_SCHEMA_URL}",
              "schemaVersion":"{COMPONENT_CUSTOMIZATION_SCHEMA_VERSION}",
              "contractId":"{COMPONENT_CUSTOMIZATION_CONTRACT_ID}",
              "revision":1,
              "properties":[
                {{"name":"--kit-a","scope":"semantic","cssProperty":"border-radius","valueKind":"border-radius","role":"default","fallback":["--kit-b"],"inherits":true,"geometryCritical":false,"stability":"stable"}},
                {{"name":"--kit-b","scope":"semantic","cssProperty":"border-radius","valueKind":"border-radius","role":"default","fallback":["--kit-a"],"inherits":true,"geometryCritical":false,"stability":"stable"}}
              ]
            }}"#
        );
        assert!(matches!(
            parse_component_customization_contract_str(&input),
            Err(ComponentCustomizationError::FallbackCycle(_))
        ));
    }
}
