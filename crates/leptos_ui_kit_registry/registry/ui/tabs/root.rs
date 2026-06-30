use std::sync::atomic::{AtomicUsize, Ordering};

use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::core::{TabsActivation, TabsLoop, TabsModel};

use super::{TabsDirection, TabsOrientation};

static NEXT_TABS_ID: AtomicUsize = AtomicUsize::new(1);

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
    let base_id = id.unwrap_or_else(next_tabs_id);
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

fn next_tabs_id() -> String {
    let id = NEXT_TABS_ID.fetch_add(1, Ordering::Relaxed);
    format!("kit-tabs-{id}")
}
