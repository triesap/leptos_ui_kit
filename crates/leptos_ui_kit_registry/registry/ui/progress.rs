use leptos::prelude::*;

#[component]
pub fn Progress(
    value: f64,
    #[prop(optional, default = 100.0)] max: f64,
    #[prop(optional, into)] class: String,
) -> impl IntoView {
    view! {
        <progress class=class_with_base("kit-progress", &class) value=value max=max>
            {format!("{value} / {max}")}
        </progress>
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
