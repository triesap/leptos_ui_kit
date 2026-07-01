use leptos::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum StatusRole {
    Status,
    Alert,
}

impl StatusRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Alert => "alert",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum StatusPoliteness {
    Polite,
    Assertive,
}

impl StatusPoliteness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Polite => "polite",
            Self::Assertive => "assertive",
        }
    }
}

#[component]
pub fn Status(
    #[prop(optional, default = StatusRole::Status)] role: StatusRole,
    #[prop(optional, default = StatusPoliteness::Polite)] politeness: StatusPoliteness,
    #[prop(optional, default = true)] atomic: bool,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    view! {
        <p
            class=class_with_base("kit-status", &class)
            role=role.as_str()
            aria-live=politeness.as_str()
            aria-atomic=if atomic { "true" } else { "false" }
        >
            {children()}
        </p>
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
