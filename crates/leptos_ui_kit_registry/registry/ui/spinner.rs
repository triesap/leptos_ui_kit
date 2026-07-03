use leptos::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum SpinnerMode {
    Status,
    Decorative,
}

impl SpinnerMode {
    fn role(self) -> Option<&'static str> {
        match self {
            Self::Status => Some("status"),
            Self::Decorative => None,
        }
    }

    fn aria_hidden(self) -> Option<&'static str> {
        match self {
            Self::Status => None,
            Self::Decorative => Some("true"),
        }
    }
}

#[component]
pub fn Spinner(
    #[prop(optional, default = SpinnerMode::Status)] mode: SpinnerMode,
    #[prop(optional, into, default = "Loading".to_owned())] label: String,
    #[prop(optional, into)] class: String,
) -> impl IntoView {
    view! {
        <span
            class=class_with_base("kit-spinner", &class)
            role=mode.role()
            aria-hidden=mode.aria_hidden()
        >
            <span class="kit-spinner-mark" aria-hidden="true"></span>
            {move || {
                if mode == SpinnerMode::Status {
                    view! { <span class="kit-spinner-label">{label.clone()}</span> }.into_any()
                } else {
                    ().into_any()
                }
            }}
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
