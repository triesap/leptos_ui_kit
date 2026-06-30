use leptos::prelude::*;

use super::root::{DialogContext, class_with_base};

#[component]
pub fn DialogDescription(
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context =
        use_context::<DialogContext>().expect("DialogDescription must be used inside DialogRoot");
    context.description_present.set(true);
    let cleanup_context = context.clone();
    on_cleanup(move || {
        cleanup_context.description_present.set(false);
    });

    view! {
        <p id=context.description_id class=class_with_base("kit-dialog-description", &class)>
            {children()}
        </p>
    }
}
