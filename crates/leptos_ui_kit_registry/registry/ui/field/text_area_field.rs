use leptos::prelude::*;

use super::{FieldLabel, FieldMessage, FieldRequired, FieldRoot, FieldSurface, TextArea};

#[component]
pub fn TextAreaField(
    #[prop(into)] id: String,
    #[prop(into)] label: Signal<String>,
    #[prop(into)] name: String,
    #[prop(into)] value: Signal<String>,
    #[prop(optional, into, default = false.into())] required: Signal<bool>,
    #[prop(optional, into, default = false.into())] invalid: Signal<bool>,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] message: Option<Signal<Option<String>>>,
    #[prop(optional, default = 4)] rows: u32,
    on_input: Callback<String>,
    #[prop(optional, into)] class: String,
    #[prop(optional, into)] surface_class: String,
    #[prop(optional, into)] label_row_class: String,
    #[prop(optional, into)] label_class: String,
    #[prop(optional, into)] required_class: String,
    #[prop(optional, into)] text_area_class: String,
    #[prop(optional, into)] message_class: String,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    let required_class_for_marker = required_class.clone();
    let message_class_for_message = message_class.clone();

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
                    {children.map(|children| children())}
                </span>
                <TextArea
                    name=name
                    value=value
                    rows=rows
                    class=text_area_class
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
