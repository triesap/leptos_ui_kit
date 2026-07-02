use leptos::prelude::*;

use super::{
    FieldLabel, FieldMessage, FieldRequired, FieldRoot, FieldSurface, NativeSelect, SelectIcon,
};

#[component]
pub fn SelectField(
    #[prop(into)] id: String,
    #[prop(into)] label: Signal<String>,
    #[prop(into)] name: String,
    #[prop(into)] value: Signal<String>,
    #[prop(into)] selected_label: Signal<String>,
    #[prop(optional, into, default = false.into())] required: Signal<bool>,
    #[prop(optional, into, default = false.into())] invalid: Signal<bool>,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] message: Option<Signal<Option<String>>>,
    on_change: Callback<String>,
    #[prop(optional, into)] class: String,
    #[prop(optional, into)] surface_class: String,
    #[prop(optional, into)] label_row_class: String,
    #[prop(optional, into)] label_class: String,
    #[prop(optional, into)] required_class: String,
    #[prop(optional, into)] select_class: String,
    #[prop(optional, into)] value_row_class: String,
    #[prop(optional, into)] value_class: String,
    #[prop(optional, into)] icon_class: String,
    #[prop(optional, into)] message_class: String,
    #[prop(optional)] label_action: Option<Children>,
    #[prop(optional)] icon: Option<Children>,
    children: Children,
) -> impl IntoView {
    let required_class_for_marker = required_class.clone();
    let message_class_for_message = message_class.clone();
    let surface_class = super::root::class_with_base("kit-select-field-surface", &surface_class);
    let select_class = super::root::class_with_base("kit-select-field-native", &select_class);
    let value_row_class =
        super::root::class_with_base("kit-select-field-value-row", &value_row_class);
    let value_class = super::root::class_with_base("kit-select-field-value", &value_class);

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
                    {label_action.map(|label_action| label_action())}
                </span>
                <NativeSelect
                    name=name
                    value=value
                    class=select_class
                    on_change=on_change
                >
                    {children()}
                </NativeSelect>
                <span class=value_row_class>
                    <span class=value_class>{move || selected_label.get()}</span>
                </span>
                {icon.map(|icon| view! {
                    <SelectIcon class=icon_class>{icon()}</SelectIcon>
                })}
            </FieldSurface>
            {move || {
                message.as_ref().and_then(|message| message.get()).map(|message| view! {
                    <FieldMessage class=message_class_for_message.clone()>{message}</FieldMessage>
                })
            }}
        </FieldRoot>
    }
}
