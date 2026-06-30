use std::sync::atomic::{AtomicUsize, Ordering};

use leptos::prelude::*;
use web_ui_primitives::core::CollapsibleModel;

static NEXT_COLLAPSIBLE_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone)]
pub(crate) struct CollapsibleContext {
    pub(crate) model: RwSignal<CollapsibleModel>,
    pub(crate) disabled: Signal<bool>,
    pub(crate) content_id: String,
}

#[component]
pub fn CollapsibleRoot(
    #[prop(optional, default = false)] default_open: bool,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] class: String,
    #[prop(optional, into)] content_id: Option<String>,
    children: Children,
) -> impl IntoView {
    let model = RwSignal::new(CollapsibleModel::new(default_open));
    let content_id = content_id.unwrap_or_else(next_content_id);
    provide_context(CollapsibleContext {
        model,
        disabled,
        content_id,
    });

    view! {
        <div class=class_with_base("kit-collapsible", &class)>
            {children()}
        </div>
    }
}

pub(crate) fn class_with_base(base: &str, class: &str) -> String {
    if class.is_empty() {
        base.to_owned()
    } else {
        format!("{base} {class}")
    }
}

fn next_content_id() -> String {
    let id = NEXT_COLLAPSIBLE_ID.fetch_add(1, Ordering::Relaxed);
    format!("kit-collapsible-content-{id}")
}
