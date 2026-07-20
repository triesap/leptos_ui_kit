use leptos::prelude::*;
use web_ui_primitives::leptos::attrs::tabs_list_attrs;

use super::root::{TabsContext, attr_string, class_with_base};

#[component]
pub fn TabsList(#[prop(optional, into)] class: String, children: Children) -> impl IntoView {
    let context = use_context::<TabsContext>().expect("TabsList must be used inside TabsRoot");
    let attrs = Signal::derive(move || tabs_list_attrs(context.orientation));
    view! {
        <div
            class=class_with_base("kit-tabs-list", &class)
            role=move || attr_string(&attrs.get(), "role")
            aria-orientation=move || attr_string(&attrs.get(), "aria-orientation")
        >
            {children()}
        </div>
    }
}
