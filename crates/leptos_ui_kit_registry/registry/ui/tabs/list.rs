use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::leptos::{attrs::tabs_list_attrs, use_dom_bindings};

use super::root::{TabsContext, class_with_base};

#[component]
pub fn TabsList(#[prop(optional, into)] class: String, children: Children) -> impl IntoView {
    let context = use_context::<TabsContext>().expect("TabsList must be used inside TabsRoot");
    let attrs = Signal::derive(move || tabs_list_attrs(context.orientation));
    let bindings = use_dom_bindings::<html::Div>(attrs, Vec::new());

    view! {
        <div
            node_ref=bindings.node_ref()
            class=class_with_base("kit-tabs-list", &class)
        >
            {children()}
        </div>
    }
}
