use leptos::prelude::*;

use super::root::{
    class_with_base, data_state, field_context, resolved_bool_signal, resolved_control_id,
    resolved_message_id,
};

#[component]
pub fn NativeSelect(
    #[prop(optional, into)] id: Option<String>,
    #[prop(optional, into)] name: Option<String>,
    #[prop(optional, into)] value: Option<Signal<String>>,
    #[prop(optional, into)] required: Option<Signal<bool>>,
    #[prop(optional, into)] disabled: Option<Signal<bool>>,
    #[prop(optional, into)] invalid: Option<Signal<bool>>,
    #[prop(optional, into)] described_by: Option<String>,
    #[prop(optional)] on_change: Option<Callback<String>>,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context = field_context();
    let control_id = resolved_control_id(id, &context, "kit-select");
    let message_id = resolved_message_id(described_by, &context);
    let required = resolved_bool_signal(
        required,
        context.as_ref().map(|context| context.required_signal()),
    );
    let disabled = resolved_bool_signal(
        disabled,
        context.as_ref().map(|context| context.disabled_signal()),
    );
    let invalid = resolved_bool_signal(
        invalid,
        context.as_ref().map(|context| context.invalid_signal()),
    );

    view! {
        <select
            id=control_id
            class=class_with_base("kit-field-control kit-native-select", &class)
            name=name
            required=move || required.get()
            disabled=move || disabled.get()
            aria-describedby=move || message_id.clone()
            aria-invalid=move || data_state(invalid.get())
            data-invalid=move || data_state(invalid.get())
            data-disabled=move || data_state(disabled.get())
            prop:value=move || value.as_ref().map(|value| value.get())
            on:change=move |event| {
                if let Some(on_change) = on_change.as_ref() {
                    on_change.run(event_target_value(&event));
                }
            }
        >
            {children()}
        </select>
    }
}

#[component]
pub fn SelectIcon(#[prop(optional, into)] class: String, children: Children) -> impl IntoView {
    view! {
        <span class=class_with_base("kit-select-icon", &class) aria-hidden="true">
            {children()}
        </span>
    }
}
