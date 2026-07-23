use leptos::prelude::*;

#[component]
pub fn Checkbox(
    #[prop(optional, into, default = false.into())] checked: Signal<bool>,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] name: Option<String>,
    #[prop(optional, default = Callback::new(|_| {}))] on_change: Callback<bool>,
    #[prop(optional, into)] class: String,
) -> impl IntoView {
    view! {
        <span class="kit-checkbox-root">
            <input
                class=class_with_base("kit-checkbox", &class)
                type="checkbox"
                name=name
                prop:checked=move || checked.get()
                disabled=move || disabled.get()
                on:change=move |event| on_change.run(event_target_checked(&event))
            />
            <svg
                class="kit-checkbox-indicator"
                viewBox="0 0 16 16"
                aria-hidden="true"
                focusable="false"
            >
                <path d="M3.25 8.25 6.5 11.5 12.75 4.75"/>
            </svg>
        </span>
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
