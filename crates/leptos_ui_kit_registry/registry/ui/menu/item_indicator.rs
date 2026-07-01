#![allow(dead_code)]

use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::leptos::{attrs::menu_item_indicator_attrs, use_dom_bindings};

use super::root::{MenuContext, class_with_base};

#[component]
pub fn MenuItemIndicator(
    index: usize,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context =
        use_context::<MenuContext>().expect("MenuItemIndicator must be used inside MenuRoot");
    context.ensure_len(index);
    let attrs_context = context.clone();
    let attrs = Signal::derive(move || {
        let model = attrs_context.model.get();
        menu_item_indicator_attrs(&model, index)
    });
    let bindings = use_dom_bindings::<html::Span>(attrs, Vec::new());

    view! {
        <span node_ref=bindings.node_ref() class=class_with_base("kit-menu-item-indicator", &class)>
            {children()}
        </span>
    }
}
