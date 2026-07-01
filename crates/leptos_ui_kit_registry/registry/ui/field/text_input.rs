use leptos::prelude::*;

use super::root::{
    class_with_base, data_state, field_context, resolved_bool_signal, resolved_control_id,
    resolved_message_id,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum TextInputType {
    Text,
    Email,
    Password,
    Search,
    Tel,
    Url,
}

impl TextInputType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Email => "email",
            Self::Password => "password",
            Self::Search => "search",
            Self::Tel => "tel",
            Self::Url => "url",
        }
    }
}

#[component]
pub fn TextInput(
    #[prop(optional, default = TextInputType::Text)] input_type: TextInputType,
    #[prop(optional, into)] id: Option<String>,
    #[prop(optional, into)] name: Option<String>,
    #[prop(optional, into)] value: Option<Signal<String>>,
    #[prop(optional, into)] autocomplete: Option<String>,
    #[prop(optional, default = false)] required: bool,
    #[prop(optional, into)] disabled: Option<Signal<bool>>,
    #[prop(optional, into)] invalid: Option<Signal<bool>>,
    #[prop(optional, into)] described_by: Option<String>,
    #[prop(optional)] on_input: Option<Callback<String>>,
    #[prop(optional, into)] class: String,
) -> impl IntoView {
    let context = field_context();
    let control_id = resolved_control_id(id, &context, "kit-input");
    let message_id = resolved_message_id(described_by, &context);
    let disabled = resolved_bool_signal(
        disabled,
        context.as_ref().map(|context| context.disabled_signal()),
    );
    let invalid = resolved_bool_signal(
        invalid,
        context.as_ref().map(|context| context.invalid_signal()),
    );

    view! {
        <input
            id=control_id
            class=class_with_base("kit-field-control kit-text-input", &class)
            type=input_type.as_str()
            name=name
            autocomplete=autocomplete
            required=required
            disabled=move || disabled.get()
            aria-describedby=move || message_id.clone()
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
