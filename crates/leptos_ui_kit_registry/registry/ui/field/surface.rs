use leptos::prelude::*;

use super::root::{FieldContext, class_with_base, data_state};

#[component]
pub fn FieldSurface(#[prop(optional, into)] class: String, children: Children) -> impl IntoView {
    let context =
        use_context::<FieldContext>().expect("FieldSurface must be used inside FieldRoot");
    let invalid = context.invalid;
    let disabled = context.disabled;

    view! {
        <div
            class=class_with_base("kit-field-surface", &class)
            data-invalid=move || data_state(invalid.get())
            data-disabled=move || data_state(disabled.get())
        >
            {children()}
        </div>
    }
}
