use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::leptos::{attrs::collapsible_content_attrs, use_dom_bindings};

use super::root::{CollapsibleContext, class_with_base};

#[component]
pub fn CollapsibleContent(
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context = use_context::<CollapsibleContext>()
        .expect("CollapsibleContent must be used inside CollapsibleRoot");
    let attrs = Signal::derive(move || {
        let mut model = context.model.get();
        model.set_disabled(context.disabled.get());
        collapsible_content_attrs(&model, Some(context.content_id.as_str()))
    });
    let bindings = use_dom_bindings::<html::Div>(attrs, Vec::new());

    view! {
        <div
            node_ref=bindings.node_ref()
            class=class_with_base("luk-collapsible-content", &class)
        >
            {children()}
        </div>
    }
}
