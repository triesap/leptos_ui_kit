use leptos::ev::PointerEvent;
use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::leptos::{
    attrs::{MenuTriggerAttrs, menu_trigger_attrs},
    use_dom_bindings,
};

use super::root::{MenuContext, class_with_base};

#[component]
pub fn MenuTrigger(
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context = use_context::<MenuContext>().expect("MenuTrigger must be used inside MenuRoot");
    let controls_id = context.content_id();
    let trigger_id = context.trigger_id();
    let attrs_context = context.clone();
    let attrs_controls_id = controls_id.clone();
    let attrs = Signal::derive(move || {
        let model = attrs_context.model.get();
        menu_trigger_attrs(
            &model,
            MenuTriggerAttrs::new().controls_id(attrs_controls_id.as_str()),
        )
    });
    let bindings = use_dom_bindings::<html::Button>(attrs, Vec::new());
    let node_ref = bindings.node_ref();
    let suppress_click = RwSignal::new(false);

    let pointer_context = context.clone();
    let on_pointerdown = move |event: PointerEvent| {
        if disabled.get_untracked() || !pointer_context.model.get_untracked().open() {
            return;
        }
        event.prevent_default();
        suppress_click.set(true);
        pointer_context.set_open(false);
    };

    let click_context = context.clone();
    let on_click = move |_| {
        if disabled.get_untracked() {
            return;
        }
        if suppress_click.get_untracked() {
            suppress_click.set(false);
            return;
        }
        click_context.toggle_open();
    };

    view! {
        <button
            id=trigger_id
            node_ref=node_ref
            class=class_with_base("kit-menu-trigger", &class)
            disabled=move || disabled.get()
            on:pointerdown=on_pointerdown
            on:click=on_click
        >
            {children()}
        </button>
    }
}
