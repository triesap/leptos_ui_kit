use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::leptos::{
    DismissibleReason, MenuLayerOptions, use_menu_layer_with_node_ref,
};

use super::root::{MenuContext, class_with_base};

#[component]
pub fn MenuContent(#[prop(optional, into)] class: String, children: ChildrenFn) -> impl IntoView {
    let context = use_context::<MenuContext>().expect("MenuContent must be used inside MenuRoot");
    let children = StoredValue::new(children);
    let content_id_value = context.content_id();
    let content_id = Signal::derive(move || content_id_value.clone());
    let trigger_id_value = context.trigger_id();
    let trigger_id = Signal::derive(move || trigger_id_value.clone());
    let content_class_value = class_with_base("kit-menu-content", &class);
    let content_class = Signal::derive(move || content_class_value.clone());
    let node_ref = NodeRef::<html::Div>::new();

    let open_context = context.clone();
    let dismiss_context = context.clone();
    let mut options =
        MenuLayerOptions::new(Signal::derive(move || open_context.model.get().open()));
    options.on_dismiss = Some(Callback::new(move |_reason: DismissibleReason| {
        dismiss_context.set_open(false);
    }));
    let layer = use_menu_layer_with_node_ref(node_ref, options);
    let render_layer = layer.clone();
    let is_rendered = Signal::derive(move || render_layer.is_rendered());
    let data_layer = layer.clone();
    let data_state = Signal::derive(move || data_layer.data_state());
    let transition_end = layer.transition_end_handler();
    let animation_end = layer.animation_end_handler();

    view! {
        {move || {
            if !is_rendered.get() {
                return ().into_any();
            }

            menu_surface(
                node_ref,
                content_id,
                trigger_id,
                content_class,
                data_state,
                transition_end.clone(),
                animation_end.clone(),
                children,
            )
            .into_any()
        }}
    }
}

fn menu_surface(
    node_ref: NodeRef<html::Div>,
    content_id: Signal<String>,
    trigger_id: Signal<String>,
    content_class: Signal<String>,
    data_state: Signal<&'static str>,
    transition_end: Callback<leptos::ev::TransitionEvent>,
    animation_end: Callback<leptos::ev::AnimationEvent>,
    children: StoredValue<ChildrenFn>,
) -> impl IntoView {
    view! {
        <div
            node_ref=node_ref
            id=move || content_id.get()
            class=move || content_class.get()
            role="menu"
            tabindex="-1"
            data-state=move || data_state.get()
            aria-labelledby=move || trigger_id.get()
            on:transitionend=move |event| transition_end.run(event)
            on:animationend=move |event| animation_end.run(event)
        >
            {children.with_value(|children| children())}
        </div>
    }
}
