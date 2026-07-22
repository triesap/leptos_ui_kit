use leptos::prelude::*;

#[component]
pub fn Skeleton(#[prop(optional, into)] class: String) -> impl IntoView {
    view! {
        <span class=class_with_base("kit-skeleton", &class) aria-hidden="true" />
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
