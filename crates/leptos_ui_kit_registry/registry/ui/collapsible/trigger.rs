use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::leptos::{attrs::collapsible_trigger_attrs, use_dom_bindings};

use super::root::{CollapsibleContext, class_with_base};

#[component]
pub fn CollapsibleTrigger(
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context = use_context::<CollapsibleContext>()
        .expect("CollapsibleTrigger must be used inside CollapsibleRoot");
    let attrs = Signal::derive(move || {
        let mut model = context.model.get();
        model.set_disabled(context.disabled.get());
        collapsible_trigger_attrs(&model, Some(context.content_id.as_str()))
    });
    let bindings = use_dom_bindings::<html::Button>(attrs, Vec::new());
    let on_click = move |_| {
        context.model.update(|model| {
            model.set_disabled(context.disabled.get());
            model.toggle();
        });
    };

    view! {
        <button
            node_ref=bindings.node_ref()
            class=class_with_base("luk-collapsible-trigger", &class)
            type="button"
            on:click=on_click
        >
            {children()}
        </button>
    }
}
