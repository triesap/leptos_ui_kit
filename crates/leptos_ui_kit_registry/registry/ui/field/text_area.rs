use leptos::prelude::*;

use super::root::{
    class_with_base, data_state, field_context, resolved_bool_signal, resolved_control_id,
    resolved_described_by,
};

#[component]
pub fn TextArea(
    #[prop(optional, into)] id: Option<String>,
    #[prop(optional, into)] name: Option<String>,
    #[prop(optional, into)] value: Option<Signal<String>>,
    #[prop(optional, into)] required: Option<Signal<bool>>,
    #[prop(optional, into)] disabled: Option<Signal<bool>>,
    #[prop(optional, into)] invalid: Option<Signal<bool>>,
    #[prop(optional, into)] described_by: Option<String>,
    #[prop(optional, default = 4)] rows: u32,
    #[prop(optional)] on_input: Option<Callback<String>>,
    #[prop(optional, into)] class: String,
) -> impl IntoView {
    let context = field_context();
    let control_id = resolved_control_id(id, &context, "kit-textarea");
    let described_by = resolved_described_by(described_by, &context);
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
        <textarea
            id=control_id
            class=class_with_base("kit-field-control kit-text-area", &class)
            name=name
            rows=rows
            required=move || required.get()
            disabled=move || disabled.get()
            aria-describedby=move || described_by.get()
            aria-invalid=move || data_state(invalid.get())
            data-invalid=move || data_state(invalid.get())
            data-disabled=move || data_state(disabled.get())
            prop:value=move || value.as_ref().map(|value| value.get())
            on:input=move |event| {
                if let Some(on_input) = on_input.as_ref() {
                    on_input.run(event_target_value(&event));
                }
            }
        />
    }
}
