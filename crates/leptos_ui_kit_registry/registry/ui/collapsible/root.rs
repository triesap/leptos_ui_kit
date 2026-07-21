use leptos::prelude::*;
use web_ui_primitives::core::CollapsibleModel;
use web_ui_primitives::leptos::{DomAttribute, DomAttributeValue};

use super::super::identity::use_kit_id;

#[derive(Clone)]
pub(crate) struct CollapsibleContext {
    pub(crate) model: RwSignal<CollapsibleModel>,
    pub(crate) disabled: Signal<bool>,
    pub(crate) content_id: String,
}

#[component]
pub fn CollapsibleRoot(
    #[prop(optional, default = false)] default_open: bool,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] class: String,
    #[prop(optional, into)] content_id: Option<String>,
    children: Children,
) -> impl IntoView {
    let model = RwSignal::new(CollapsibleModel::new(default_open));
    let content_id = content_id.unwrap_or_else(|| use_kit_id("kit-collapsible-content"));
    provide_context(CollapsibleContext {
        model,
        disabled,
        content_id,
    });

    view! {
        <div class=class_with_base("kit-collapsible", &class)>
            {children()}
        </div>
    }
}

pub(crate) fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}

pub(crate) fn attr_string(attrs: &[DomAttribute], name: &str) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if attr.name() != name {
            return None;
        }
        match attr.value() {
            DomAttributeValue::String(value) => Some(value.clone()),
            DomAttributeValue::Bool(true) => Some(String::new()),
            DomAttributeValue::Bool(false) => None,
        }
    })
}

pub(crate) fn attr_bool(attrs: &[DomAttribute], name: &str) -> bool {
    attrs
        .iter()
        .any(|attr| attr.name() == name && matches!(attr.value(), DomAttributeValue::Bool(true)))
}

pub(crate) fn data_attr(active: bool) -> Option<&'static str> {
    active.then_some("")
}
