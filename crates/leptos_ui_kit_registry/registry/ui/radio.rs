use leptos::prelude::*;

#[component]
pub fn Radio(
    #[prop(into)] value: String,
    #[prop(optional, into)] name: Option<String>,
    #[prop(optional, into, default = false.into())] checked: Signal<bool>,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, default = Callback::new(|_| {}))] on_change: Callback<String>,
    #[prop(optional, into)] class: String,
) -> impl IntoView {
    let emitted_value = value.clone();
    view! {
        <input
            class=class_with_base("kit-radio", &class)
            type="radio"
            name=name
            value=value
            prop:checked=move || checked.get()
            disabled=move || disabled.get()
            on:change=move |event| {
                if event_target_checked(&event) { on_change.run(emitted_value.clone()); }
            }
        />
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
