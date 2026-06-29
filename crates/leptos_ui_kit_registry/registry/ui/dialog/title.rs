use leptos::prelude::*;

use super::root::{DialogContext, class_with_base};

#[component]
pub fn DialogTitle(#[prop(optional, into)] class: String, children: Children) -> impl IntoView {
    let context =
        use_context::<DialogContext>().expect("DialogTitle must be used inside DialogRoot");

    view! {
        <h2 id=context.title_id class=class_with_base("luk-dialog-title", &class)>
            {children()}
        </h2>
    }
}
