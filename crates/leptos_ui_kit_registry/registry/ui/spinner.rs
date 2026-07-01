use leptos::prelude::*;

#[component]
pub fn Spinner(
    #[prop(optional, into, default = "Loading".to_owned())] label: String,
    #[prop(optional, into)] class: String,
) -> impl IntoView {
    view! {
        <span class=class_with_base("kit-spinner", &class) role="status">
            <span class="kit-spinner-mark" aria-hidden="true"></span>
            <span class="kit-spinner-label">{label}</span>
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
