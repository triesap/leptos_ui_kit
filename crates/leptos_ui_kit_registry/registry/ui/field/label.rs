use leptos::prelude::*;

use super::root::{FieldContext, class_with_base, data_state};

#[component]
pub fn FieldLabel(#[prop(optional, into)] class: String, children: Children) -> impl IntoView {
    let context = use_context::<FieldContext>().expect("FieldLabel must be used inside FieldRoot");
    let control_id = context.control_id();
    let disabled = context.disabled;

    view! {
        <label
            class=class_with_base("kit-field-label", &class)
            for=control_id
            data-disabled=move || data_state(disabled.get())
        >
            {children()}
        </label>
    }
}
