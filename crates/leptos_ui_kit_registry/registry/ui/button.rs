use leptos::prelude::*;

#[component]
pub fn Button(children: Children) -> impl IntoView {
    view! {
        <button class="luk-button">{children()}</button>
    }
}
