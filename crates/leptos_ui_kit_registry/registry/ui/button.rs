use leptos::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Ghost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum ButtonSize {
    Sm,
    Md,
    Lg,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum ButtonType {
    Button,
    Submit,
    Reset,
}

impl ButtonVariant {
    fn class(self) -> &'static str {
        match self {
            Self::Primary => "luk-button--primary",
            Self::Secondary => "luk-button--secondary",
            Self::Ghost => "luk-button--ghost",
        }
    }
}

impl ButtonSize {
    fn class(self) -> &'static str {
        match self {
            Self::Sm => "luk-button--sm",
            Self::Md => "luk-button--md",
            Self::Lg => "luk-button--lg",
        }
    }
}

impl ButtonType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Button => "button",
            Self::Submit => "submit",
            Self::Reset => "reset",
        }
    }
}

#[component]
pub fn Button(
    #[prop(optional, default = ButtonVariant::Primary)] variant: ButtonVariant,
    #[prop(optional, default = ButtonSize::Md)] size: ButtonSize,
    #[prop(optional, default = ButtonType::Button)] button_type: ButtonType,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let base_class = format!("luk-button {} {}", variant.class(), size.class(),);
    let class = if class.is_empty() {
        base_class
    } else {
        format!("{base_class} {class}")
    };

    view! {
        <button
            class=class
            type=button_type.as_str()
            disabled=move || disabled.get()
        >
            {children()}
        </button>
    }
}
