use leptos::prelude::*;

use super::root::{FieldContext, class_with_base};

#[component]
pub fn FieldRequired(#[prop(optional, into)] class: String) -> impl IntoView {
    let context =
        use_context::<FieldContext>().expect("FieldRequired must be used inside FieldRoot");
    let required = context.required_signal();
    let class = class_with_base("kit-field-required", &class);

    view! {
        {move || {
            if !required.get() {
                return ().into_any();
            }

            view! {
                <span class=class.clone() aria-hidden="true">
                    "*"
                </span>
            }
            .into_any()
        }}
    }
}
