use leptos::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Ghost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonSize {
    Sm,
    Md,
    Lg,
}

#[component]
pub fn Button(
    #[prop(optional, default = ButtonVariant::Primary)] variant: ButtonVariant,
    #[prop(optional, default = ButtonSize::Md)] size: ButtonSize,
    #[prop(optional)] disabled: bool,
    children: Children,
) -> impl IntoView {
    let class = match (variant, size) {
        (ButtonVariant::Primary, ButtonSize::Sm) => {
            "luk-button luk-button--primary luk-button--sm"
        }
        (ButtonVariant::Primary, ButtonSize::Md) => {
            "luk-button luk-button--primary luk-button--md"
        }
        (ButtonVariant::Primary, ButtonSize::Lg) => {
            "luk-button luk-button--primary luk-button--lg"
        }
        (ButtonVariant::Secondary, ButtonSize::Sm) => {
            "luk-button luk-button--secondary luk-button--sm"
        }
        (ButtonVariant::Secondary, ButtonSize::Md) => {
            "luk-button luk-button--secondary luk-button--md"
        }
        (ButtonVariant::Secondary, ButtonSize::Lg) => {
            "luk-button luk-button--secondary luk-button--lg"
        }
        (ButtonVariant::Ghost, ButtonSize::Sm) => {
            "luk-button luk-button--ghost luk-button--sm"
        }
        (ButtonVariant::Ghost, ButtonSize::Md) => {
            "luk-button luk-button--ghost luk-button--md"
        }
        (ButtonVariant::Ghost, ButtonSize::Lg) => {
            "luk-button luk-button--ghost luk-button--lg"
        }
    };

    view! {
        <button class=class disabled=disabled>{children()}</button>
    }
}
