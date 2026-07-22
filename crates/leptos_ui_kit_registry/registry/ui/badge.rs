use leptos::prelude::*;

#[component]
pub fn Badge(#[prop(optional, into)] class: String, children: Children) -> impl IntoView {
    view! {
        <span class=class_with_base("kit-badge", &class)>{children()}</span>
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
