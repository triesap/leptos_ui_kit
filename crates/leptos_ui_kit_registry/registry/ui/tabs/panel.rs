use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::leptos::{attrs::tabs_panel_attrs, use_dom_bindings};

use super::root::{TabsContext, class_with_base};

#[component]
pub fn TabsPanel(
    index: usize,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context = use_context::<TabsContext>().expect("TabsPanel must be used inside TabsRoot");
    context.ensure_len(index);
    let attrs_context = context.clone();
    let attrs = Signal::derive(move || {
        let panel_id = attrs_context.panel_id(index);
        let trigger_id = attrs_context.trigger_id(index);
        let mut model = attrs_context.model.get();
        if model.len() <= index {
            model.set_len(index + 1);
        }
        tabs_panel_attrs(
            &model,
            index,
            Some(panel_id.as_str()),
            Some(trigger_id.as_str()),
        )
    });
    let bindings = use_dom_bindings::<html::Div>(attrs, Vec::new());

    view! {
        <div
            node_ref=bindings.node_ref()
            class=class_with_base("kit-tabs-panel", &class)
        >
            {children()}
        </div>
    }
}
