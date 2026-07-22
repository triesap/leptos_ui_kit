use leptos::prelude::*;

#[component]
pub fn Avatar(
    #[prop(into)] src: String,
    #[prop(into)] alt: String,
    #[prop(optional, into)] class: String,
) -> impl IntoView {
    view! {
        <img class=class_with_base("kit-avatar", &class) src=src alt=alt />
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
