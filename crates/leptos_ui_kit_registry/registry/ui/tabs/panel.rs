use leptos::prelude::*;
use web_ui_primitives::leptos::attrs::tabs_panel_attrs;

use super::root::{TabsContext, attr_bool, attr_string, class_with_base};

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
    view! {
        <div
            id=move || attr_string(&attrs.get(), "id")
            class=class_with_base("kit-tabs-panel", &class)
            role=move || attr_string(&attrs.get(), "role")
            tabindex=move || attr_string(&attrs.get(), "tabindex")
            hidden=move || attr_bool(&attrs.get(), "hidden")
            aria-labelledby=move || attr_string(&attrs.get(), "aria-labelledby")
        >
            {children()}
        </div>
    }
}
