use leptos::prelude::*;
use leptos_router::components::A;

#[component]
pub fn RouterLink(
    #[prop(into)] href: String,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let class = class_with_base("kit-anchor", &class);

    view! {
        <A attr:class=class href=href>
            {children()}
        </A>
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
