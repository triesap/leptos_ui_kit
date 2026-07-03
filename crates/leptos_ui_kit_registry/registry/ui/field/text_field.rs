use leptos::prelude::*;

use super::{
    FieldLabel, FieldMessage, FieldRequired, FieldRoot, FieldSlot, FieldSurface, TextInput,
    TextInputType,
};

#[component]
pub fn TextField(
    #[prop(into)] id: String,
    #[prop(into)] label: Signal<String>,
    #[prop(optional, default = TextInputType::Text)] input_type: TextInputType,
    #[prop(into)] name: String,
    #[prop(into)] value: Signal<String>,
    #[prop(optional, into)] autocomplete: String,
    #[prop(optional, into, default = false.into())] required: Signal<bool>,
    #[prop(optional, into, default = false.into())] invalid: Signal<bool>,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] message: Option<Signal<Option<String>>>,
    on_input: Callback<String>,
    #[prop(optional, into)] class: String,
    #[prop(optional, into)] surface_class: String,
    #[prop(optional, into)] label_row_class: String,
    #[prop(optional, into)] label_class: String,
    #[prop(optional, into)] required_class: String,
    #[prop(optional, into)] input_class: String,
    #[prop(optional, into)] message_class: String,
    #[prop(optional, into, default = FieldSlot::empty())] label_action: FieldSlot,
) -> impl IntoView {
    let required_class_for_marker = required_class.clone();
    let message_class_for_message = message_class.clone();
    let label_action_for_render = label_action.clone();

    view! {
        <FieldRoot id=id class=class required=required invalid=invalid disabled=disabled>
            <FieldSurface class=surface_class>
                <span class=super::root::class_with_base("kit-field-label-row", &label_row_class)>
                    <FieldLabel class=label_class>
                        {move || label.get()}
                        {move || {
                            required.get().then(|| view! {
                                <FieldRequired class=required_class_for_marker.clone() />
                            })
                        }}
                    </FieldLabel>
                    {move || label_action_for_render.render()}
                </span>
                <TextInput
                    input_type=input_type
                    name=name
                    value=value
                    autocomplete=autocomplete
                    class=input_class
                    on_input=on_input
                />
            </FieldSurface>
            {move || {
                message.as_ref().and_then(|message| message.get()).map(|message| view! {
                    <FieldMessage class=message_class_for_message.clone()>{message}</FieldMessage>
                })
            }}
        </FieldRoot>
    }
}
