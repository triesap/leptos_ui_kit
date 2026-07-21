use leptos::prelude::*;

use super::super::identity::use_kit_id;

#[derive(Clone)]
pub(crate) struct DialogContext {
    pub(crate) open: RwSignal<bool>,
    pub(crate) modal: bool,
    pub(crate) content_id: String,
    pub(crate) title_id: String,
    pub(crate) description_id: String,
    pub(crate) description_present: RwSignal<bool>,
}

impl DialogContext {
    pub(crate) fn set_open(&self, open: bool) {
        self.open.set(open);
    }
}

#[component]
pub fn DialogRoot(
    #[prop(optional, default = false)] default_open: bool,
    #[prop(optional)] open: Option<RwSignal<bool>>,
    #[prop(optional, default = true)] modal: bool,
    #[prop(optional, into)] class: String,
    #[prop(optional, into)] id: Option<String>,
    children: Children,
) -> impl IntoView {
    let open = open.unwrap_or_else(|| RwSignal::new(default_open));
    let base_id = id.unwrap_or_else(|| use_kit_id("kit-dialog"));
    provide_context(DialogContext {
        open,
        modal,
        content_id: format!("{base_id}-content"),
        title_id: format!("{base_id}-title"),
        description_id: format!("{base_id}-description"),
        description_present: RwSignal::new(false),
    });

    view! {
        <div class=class_with_base("kit-dialog", &class)>
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
