use std::{collections::BTreeSet, fmt};

use crate::KitConfig;

pub const RUNTIME_ABI_VERSION: u32 = 1;
pub const PRESENCE_ABI_VERSION: u32 = 2;
pub const LAYER_ABI_VERSION: u32 = 1;
pub const PORTAL_ABI_VERSION: u32 = 1;
pub const WEB_UI_PRIMITIVES_REQUIREMENT: &str = ">=0.2.0,<0.3.0";
pub const PORTAL_MOUNT_TYPE: &str = "web_ui_primitives::leptos::PortalMount";
pub const PORTAL_BODY_HOST: bool = true;
pub const KIT_LAYER_ORDER: [&str; 3] = [
    "leptos-ui-kit.tokens",
    "leptos-ui-kit.themes",
    "leptos-ui-kit.components",
];
pub const KIT_LAYER_ORDER_DECLARATION: &str =
    "@layer leptos-ui-kit.tokens, leptos-ui-kit.themes, leptos-ui-kit.components;\n";
pub const PRESENCE_COMPLETION_EVENTS: [&str; 4] = [
    "transitionend",
    "transitioncancel",
    "animationend",
    "animationcancel",
];
pub const RADROOTS_EE_WEB_COMPONENT_CLOSURE: [&str; 8] = [
    "tokens",
    "anchor",
    "spinner",
    "button",
    "field",
    "menu",
    "router-link",
    "status",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeAbi {
    pub version: u32,
    pub presence_version: u32,
    pub layer_version: u32,
    pub layer_order: &'static [&'static str],
    pub portal_version: u32,
    pub portal_mount_type: &'static str,
    pub portal_body_host: bool,
}

pub const fn runtime_abi() -> RuntimeAbi {
    RuntimeAbi {
        version: RUNTIME_ABI_VERSION,
        presence_version: PRESENCE_ABI_VERSION,
        layer_version: LAYER_ABI_VERSION,
        layer_order: &KIT_LAYER_ORDER,
        portal_version: PORTAL_ABI_VERSION,
        portal_mount_type: PORTAL_MOUNT_TYPE,
        portal_body_host: PORTAL_BODY_HOST,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumerClosureError {
    DuplicateItem(String),
    MissingItem(String),
    UnexpectedItem(String),
}

impl fmt::Display for ConsumerClosureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateItem(item) => {
                write!(formatter, "consumer closure contains duplicate item {item}")
            }
            Self::MissingItem(item) => {
                write!(
                    formatter,
                    "consumer closure is missing required item {item}"
                )
            }
            Self::UnexpectedItem(item) => {
                write!(
                    formatter,
                    "consumer closure contains unexpected item {item}"
                )
            }
        }
    }
}

impl std::error::Error for ConsumerClosureError {}

pub fn validate_radroots_ee_web_component_closure(
    config: &KitConfig,
) -> Result<(), ConsumerClosureError> {
    let mut actual = BTreeSet::new();
    for item in &config.items {
        let name = item.item_name();
        if !actual.insert(name) {
            return Err(ConsumerClosureError::DuplicateItem(name.to_owned()));
        }
    }

    let expected = RADROOTS_EE_WEB_COMPONENT_CLOSURE
        .into_iter()
        .collect::<BTreeSet<_>>();
    if let Some(missing) = expected.difference(&actual).next() {
        return Err(ConsumerClosureError::MissingItem((*missing).to_owned()));
    }
    if let Some(unexpected) = actual.difference(&expected).next() {
        return Err(ConsumerClosureError::UnexpectedItem(
            (*unexpected).to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        KIT_LAYER_ORDER, LAYER_ABI_VERSION, PORTAL_ABI_VERSION, PORTAL_BODY_HOST,
        PORTAL_MOUNT_TYPE, PRESENCE_ABI_VERSION, PRESENCE_COMPLETION_EVENTS,
        RADROOTS_EE_WEB_COMPONENT_CLOSURE, RUNTIME_ABI_VERSION, runtime_abi,
        validate_radroots_ee_web_component_closure,
    };
    use crate::{parse_kit_json_str, web_ui_primitives_version};

    #[test]
    fn runtime_abi_constants_match_the_qualified_primitives_surface() {
        assert_eq!(RUNTIME_ABI_VERSION, 1);
        assert_eq!(
            PRESENCE_ABI_VERSION,
            web_ui_primitives::leptos::PRESENCE_ABI_VERSION
        );
        assert_eq!(LAYER_ABI_VERSION, 1);
        assert_eq!(PORTAL_ABI_VERSION, 1);
        assert_eq!(
            KIT_LAYER_ORDER,
            [
                "leptos-ui-kit.tokens",
                "leptos-ui-kit.themes",
                "leptos-ui-kit.components",
            ]
        );
        assert_eq!(
            PRESENCE_COMPLETION_EVENTS,
            [
                "transitionend",
                "transitioncancel",
                "animationend",
                "animationcancel",
            ]
        );
        assert_eq!(PORTAL_MOUNT_TYPE, "web_ui_primitives::leptos::PortalMount");
        assert!(PORTAL_BODY_HOST);
        assert_eq!(runtime_abi().presence_version, PRESENCE_ABI_VERSION);
        assert_eq!(web_ui_primitives_version(), "0.2.0");
    }

    #[test]
    fn radroots_shared_fixture_has_the_exact_approved_closure() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/shared_library/src/components/ui/_kit/kit.json");
        let config = parse_kit_json_str(
            &std::fs::read_to_string(fixture).expect("read shared-library fixture"),
        )
        .expect("parse shared-library fixture");
        validate_radroots_ee_web_component_closure(&config).expect("approved closure");
        assert_eq!(
            RADROOTS_EE_WEB_COMPONENT_CLOSURE,
            [
                "tokens",
                "anchor",
                "spinner",
                "button",
                "field",
                "menu",
                "router-link",
                "status",
            ]
        );
    }

    #[test]
    fn closure_validation_fails_closed_for_missing_and_unexpected_items() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/shared_library/src/components/ui/_kit/kit.json");
        let mut config = parse_kit_json_str(
            &std::fs::read_to_string(fixture).expect("read shared-library fixture"),
        )
        .expect("parse shared-library fixture");

        config.items.pop();
        assert!(validate_radroots_ee_web_component_closure(&config).is_err());

        config.items.push(crate::desired_builtin_tabs_item());
        assert!(validate_radroots_ee_web_component_closure(&config).is_err());
    }
}
