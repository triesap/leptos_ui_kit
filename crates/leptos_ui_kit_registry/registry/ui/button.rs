use leptos::prelude::*;

use super::{Spinner, SpinnerMode};

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
            Self::Primary => "kit-button--primary",
            Self::Secondary => "kit-button--secondary",
            Self::Ghost => "kit-button--ghost",
        }
    }
}

impl ButtonSize {
    fn class(self) -> &'static str {
        match self {
            Self::Sm => "kit-button--sm",
            Self::Md => "kit-button--md",
            Self::Lg => "kit-button--lg",
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
    #[prop(optional, into, default = false.into())] loading: Signal<bool>,
    #[prop(optional, into, default = "Loading".to_owned())] loading_label: String,
    #[prop(optional)] on_click: Option<Callback<leptos::ev::MouseEvent>>,
    #[prop(optional, into)] class: String,
    children: ChildrenFn,
) -> impl IntoView {
    let base_class = format!("kit-button {} {}", variant.class(), size.class(),);
    let class = if class.is_empty() {
        base_class
    } else {
        format!("{base_class} {class}")
    };
    let disabled_state = Signal::derive(move || disabled.get() || loading.get());
    let children = StoredValue::new(children);

    view! {
        <button
            class=class
            type=button_type.as_str()
            disabled=move || disabled_state.get()
            aria-busy=move || loading.get().then_some("true")
            on:click=move |event| {
                if disabled_state.get_untracked() {
                    return;
                }

                if let Some(on_click) = on_click.as_ref() {
                    on_click.run(event);
                }
            }
        >
            {move || {
                if loading.get() {
                    view! {
                        <Spinner mode=SpinnerMode::Decorative class="kit-button-spinner" />
                        <span class="kit-button-loading-label">{loading_label.clone()}</span>
                    }
                        .into_any()
                } else {
                    children.with_value(|children| children()).into_any()
                }
            }}
        </button>
    }
}
