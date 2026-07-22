use leptos::prelude::*;

#[component]
pub fn Switch(
    #[prop(optional, into, default = false.into())] checked: Signal<bool>,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, default = Callback::new(|_| {}))] on_change: Callback<bool>,
    #[prop(optional, into)] class: String,
) -> impl IntoView {
    view! {
        <button
            class=class_with_base("kit-switch", &class)
            type="button"
            role="switch"
            aria-checked=move || checked.get().to_string()
            data-state=move || if checked.get() { "checked" } else { "unchecked" }
            disabled=move || disabled.get()
            on:click=move |_| {
                if !disabled.get_untracked() { on_change.run(!checked.get_untracked()); }
            }
        ><span class="kit-switch-thumb" aria-hidden="true" /></button>
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
