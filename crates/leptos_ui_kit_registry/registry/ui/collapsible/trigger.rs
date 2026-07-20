use leptos::prelude::*;
use web_ui_primitives::leptos::attrs::collapsible_trigger_attrs;

use super::root::{CollapsibleContext, attr_bool, attr_string, class_with_base, data_attr};

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
    let on_click = move |_| {
        context.model.update(|model| {
            model.set_disabled(context.disabled.get());
            model.toggle();
        });
    };

    view! {
        <button
            class=class_with_base("kit-collapsible-trigger", &class)
            type="button"
            data-state=move || attr_string(&attrs.get(), "data-state")
            aria-expanded=move || attr_string(&attrs.get(), "aria-expanded")
            aria-controls=move || attr_string(&attrs.get(), "aria-controls")
            disabled=move || attr_bool(&attrs.get(), "disabled")
            data-disabled=move || data_attr(attr_bool(&attrs.get(), "data-disabled"))
            on:click=on_click
        >
            {children()}
        </button>
    }
}
