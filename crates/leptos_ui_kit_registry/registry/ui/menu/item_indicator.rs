#![allow(dead_code)]

use leptos::prelude::*;
use web_ui_primitives::leptos::attrs::menu_item_indicator_attrs;

use super::root::{MenuContext, attr_bool, attr_string, class_with_base};

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
        let model = attrs_context.model_snapshot(|model| {
            if model.len() <= index {
                model.set_len(index + 1);
            }
        });
        menu_item_indicator_attrs(&model, index)
    });
    view! {
        <span
            class=class_with_base("kit-menu-item-indicator", &class)
            hidden=move || attr_bool(&attrs.get(), "hidden")
            data-state=move || attr_string(&attrs.get(), "data-state").unwrap_or_else(|| "unchecked".to_owned())
        >
            {children()}
        </span>
    }
}
