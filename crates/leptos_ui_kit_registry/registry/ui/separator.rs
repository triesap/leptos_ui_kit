use leptos::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum SeparatorOrientation {
    Horizontal,
    Vertical,
}

impl SeparatorOrientation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

#[component]
pub fn Separator(
    #[prop(optional, default = SeparatorOrientation::Horizontal)] orientation: SeparatorOrientation,
    #[prop(optional, into)] class: String,
) -> impl IntoView {
    view! {
        <div
            class=class_with_base("kit-separator", &class)
            role="separator"
            aria-orientation=orientation.as_str()
            data-orientation=orientation.as_str()
        />
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
