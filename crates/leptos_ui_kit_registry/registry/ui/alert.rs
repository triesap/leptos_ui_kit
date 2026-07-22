use leptos::prelude::*;

#[component]
pub fn Alert(#[prop(optional, into)] class: String, children: Children) -> impl IntoView {
    view! {
        <div class=class_with_base("kit-alert", &class) role="alert">{children()}</div>
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
