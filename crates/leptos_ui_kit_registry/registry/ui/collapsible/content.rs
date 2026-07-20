use leptos::prelude::*;
use web_ui_primitives::leptos::attrs::collapsible_content_attrs;

use super::root::{CollapsibleContext, attr_bool, attr_string, class_with_base};

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
    view! {
        <div
            id=move || attr_string(&attrs.get(), "id")
            class=class_with_base("kit-collapsible-content", &class)
            data-state=move || attr_string(&attrs.get(), "data-state")
            hidden=move || attr_bool(&attrs.get(), "hidden")
        >
            {children()}
        </div>
    }
}
