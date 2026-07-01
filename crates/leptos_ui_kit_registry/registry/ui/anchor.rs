use leptos::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum AnchorTarget {
    SameTab,
    Blank,
    Parent,
    Top,
}

impl AnchorTarget {
    fn as_attr(self) -> Option<&'static str> {
        match self {
            Self::SameTab => None,
            Self::Blank => Some("_blank"),
            Self::Parent => Some("_parent"),
            Self::Top => Some("_top"),
        }
    }

    fn default_rel(self) -> Option<&'static str> {
        match self {
            Self::Blank => Some("noopener noreferrer"),
            Self::SameTab | Self::Parent | Self::Top => None,
        }
    }
}

#[component]
pub fn Anchor(
    #[prop(into)] href: String,
    #[prop(optional, default = AnchorTarget::SameTab)] target: AnchorTarget,
    #[prop(optional, into)] rel: Option<String>,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let target_attr = target.as_attr();
    let rel_attr = rel.or_else(|| target.default_rel().map(str::to_owned));

    view! {
        <a
            class=class_with_base("kit-anchor", &class)
            href=href
            target=target_attr
            rel=rel_attr
        >
            {children()}
        </a>
    }
}

fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}
