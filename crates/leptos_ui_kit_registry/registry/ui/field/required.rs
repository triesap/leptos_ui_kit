use leptos::prelude::*;

use super::root::class_with_base;

#[component]
pub fn FieldRequired(#[prop(optional, into)] class: String) -> impl IntoView {
    view! {
        <span class=class_with_base("kit-field-required", &class) aria-hidden="true">
            "*"
        </span>
    }
}
