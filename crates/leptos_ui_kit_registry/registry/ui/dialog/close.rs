use leptos::prelude::*;

use super::root::{DialogContext, class_with_base};

#[component]
pub fn DialogClose(
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context =
        use_context::<DialogContext>().expect("DialogClose must be used inside DialogRoot");
    let click_context = context.clone();
    let on_click = move |_| {
        if !disabled.get_untracked() {
            click_context.set_open(false);
        }
    };

    view! {
        <button
            class=class_with_base("kit-dialog-close", &class)
            type="button"
            disabled=move || disabled.get()
            on:click=on_click
        >
            {children()}
        </button>
    }
}
