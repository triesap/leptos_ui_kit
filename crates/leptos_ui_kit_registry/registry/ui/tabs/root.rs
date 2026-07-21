use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::core::{TabsActivation, TabsLoop, TabsModel};
use web_ui_primitives::leptos::{DomAttribute, DomAttributeValue};

use super::super::identity::use_kit_id;
use super::{TabsDirection, TabsOrientation};

#[derive(Clone)]
pub(crate) struct TabsContext {
    pub(crate) model: RwSignal<TabsModel>,
    pub(crate) orientation: TabsOrientation,
    pub(crate) direction: TabsDirection,
    base_id: String,
    trigger_refs: RwSignal<Vec<Option<NodeRef<html::Button>>>>,
}

impl TabsContext {
    pub(crate) fn ensure_len(&self, index: usize) {
        self.model.update(|model| {
            if model.len() <= index {
                model.set_len(index + 1);
            }
        });
    }

    pub(crate) fn register_trigger(&self, index: usize, node_ref: NodeRef<html::Button>) {
        self.trigger_refs.update(|refs| {
            if refs.len() <= index {
                refs.resize_with(index + 1, || None);
            }
            refs[index] = Some(node_ref);
        });
    }

    pub(crate) fn unregister_trigger(&self, index: usize) {
        let mut retained_len = 0;
        self.trigger_refs.update(|refs| {
            if let Some(trigger_ref) = refs.get_mut(index) {
                *trigger_ref = None;
            }
            retained_len = refs
                .iter()
                .rposition(Option::is_some)
                .map_or(0, |last| last + 1);
            refs.truncate(retained_len);
        });
        self.model.update(|model| {
            if index < retained_len {
                model.set_disabled(index, true);
            }
            model.set_len(retained_len);
        });
    }

    pub(crate) fn set_disabled(&self, index: usize, disabled: bool) {
        self.ensure_len(index);
        self.model.update(|model| {
            model.set_disabled(index, disabled);
        });
    }

    pub(crate) fn focus_trigger(&self, index: usize) {
        let node_ref = self
            .trigger_refs
            .with(|refs| refs.get(index).cloned().flatten());
        if let Some(node_ref) = node_ref {
            if let Some(element) = node_ref.get() {
                let _ = element.focus();
            }
        }
    }

    pub(crate) fn trigger_id(&self, index: usize) -> String {
        format!("{}-trigger-{index}", self.base_id)
    }

    pub(crate) fn panel_id(&self, index: usize) -> String {
        format!("{}-panel-{index}", self.base_id)
    }
}

#[component]
pub fn TabsRoot(
    #[prop(optional, default = TabsActivation::Automatic)] activation: TabsActivation,
    #[prop(optional, default = TabsLoop::Wrap)] loop_policy: TabsLoop,
    #[prop(optional, default = TabsOrientation::Horizontal)] orientation: TabsOrientation,
    #[prop(optional, default = TabsDirection::Ltr)] direction: TabsDirection,
    #[prop(optional, into)] class: String,
    #[prop(optional, into)] id: Option<String>,
    children: Children,
) -> impl IntoView {
    let model = RwSignal::new(TabsModel::with_activation_and_loop(
        0,
        activation,
        loop_policy,
    ));
    let base_id = id.unwrap_or_else(|| use_kit_id("kit-tabs"));
    provide_context(TabsContext {
        model,
        orientation,
        direction,
        base_id,
        trigger_refs: RwSignal::new(Vec::new()),
    });

    view! {
        <div class=class_with_base("kit-tabs", &class)>
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
