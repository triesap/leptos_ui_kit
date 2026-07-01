use leptos::prelude::*;

use super::root::{FieldContext, class_with_base, data_state};

#[component]
pub fn FieldMessage(#[prop(optional, into)] class: String, children: Children) -> impl IntoView {
    let context =
        use_context::<FieldContext>().expect("FieldMessage must be used inside FieldRoot");
    let message_id = context.message_id();
    let invalid = context.invalid;
    let disabled = context.disabled;

    view! {
        <p
            id=message_id
            class=class_with_base("kit-field-message", &class)
            data-invalid=move || data_state(invalid.get())
            data-disabled=move || data_state(disabled.get())
        >
            {children()}
        </p>
    }
}
