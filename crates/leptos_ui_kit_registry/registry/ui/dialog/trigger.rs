use leptos::prelude::*;

use super::root::{DialogContext, class_with_base};

#[component]
pub fn DialogTrigger(
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context =
        use_context::<DialogContext>().expect("DialogTrigger must be used inside DialogRoot");
    let state_context = context.clone();
    let expanded_context = context.clone();
    let click_context = context.clone();
    let controls_id = context.content_id.clone();
    let on_click = move |_| {
        if !disabled.get_untracked() {
            click_context.set_open(true);
        }
    };

    view! {
        <button
            class=class_with_base("kit-dialog-trigger", &class)
            type="button"
            disabled=move || disabled.get()
            data-state=move || dialog_state(state_context.open.get())
            aria-haspopup="dialog"
            aria-expanded=move || if expanded_context.open.get() { "true" } else { "false" }
            aria-controls=controls_id
            on:click=on_click
        >
            {children()}
        </button>
    }
}

pub(crate) fn dialog_state(open: bool) -> &'static str {
    if open { "open" } else { "closed" }
}
