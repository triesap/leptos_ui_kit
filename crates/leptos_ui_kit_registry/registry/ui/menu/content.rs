use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::core::{PlacementAlign, PlacementSide};
use web_ui_primitives::leptos::{
    DismissibleFocusOutsideEvent, DismissiblePointerDownOutsideEvent, DismissibleReason,
    MenuLayerOptions, MenuPlacementBinding, MenuPlacementOptions, PlacementSink,
    use_menu_layer_with_node_ref, use_menu_placement_with_node_refs,
};

use super::root::{MenuContext, class_with_base};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum MenuContentSide {
    Bottom,
    Top,
    Right,
    Left,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum MenuContentAlign {
    Start,
    Center,
    End,
}

impl MenuContentSide {
    fn as_placement(self) -> PlacementSide {
        match self {
            Self::Bottom => PlacementSide::Bottom,
            Self::Top => PlacementSide::Top,
            Self::Right => PlacementSide::Right,
            Self::Left => PlacementSide::Left,
        }
    }
}

impl MenuContentAlign {
    fn as_placement(self) -> PlacementAlign {
        match self {
            Self::Start => PlacementAlign::Start,
            Self::Center => PlacementAlign::Center,
            Self::End => PlacementAlign::End,
        }
    }
}

#[component]
pub fn MenuContent(
    #[prop(optional, default = MenuContentSide::Bottom)] side: MenuContentSide,
    #[prop(optional, default = MenuContentAlign::Start)] align: MenuContentAlign,
    #[prop(optional, default = 4.0)] spacing: f64,
    #[prop(optional, default = 8.0)] viewport_padding: f64,
    #[prop(optional)] placement_sink: PlacementSink,
    #[prop(optional, into)] class: String,
    children: ChildrenFn,
) -> impl IntoView {
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
    let pointer_trigger_ref = context.trigger_ref;
    let focus_trigger_ref = context.trigger_ref;
    let mut options =
        MenuLayerOptions::new(Signal::derive(move || open_context.model.get().open()));
    options.on_pointer_down_outside = Some(Callback::new(
        move |event: DismissiblePointerDownOutsideEvent| {
            if target_is_trigger(event.event().target(), pointer_trigger_ref) {
                event.prevent_default();
            }
        },
    ));
    options.on_focus_outside = Some(Callback::new(move |event: DismissibleFocusOutsideEvent| {
        if target_is_trigger(event.event().target(), focus_trigger_ref) {
            event.prevent_default();
        }
    }));
    options.on_dismiss = Some(Callback::new(move |_reason: DismissibleReason| {
        dismiss_context.set_open(false);
    }));
    let layer = use_menu_layer_with_node_ref(node_ref, options);
    let render_layer = layer.clone();
    let is_rendered = Signal::derive(move || render_layer.is_rendered());
    let data_layer = layer.clone();
    let data_state = Signal::derive(move || data_layer.data_state());
    let placement_context = context.clone();
    let placement = use_menu_placement_with_node_refs(
        context.trigger_ref,
        node_ref,
        MenuPlacementOptions::new(
            Signal::derive(move || placement_context.model.get().open()),
            side.as_placement(),
            align.as_placement(),
        )
        .spacing(spacing)
        .viewport_padding(viewport_padding)
        .sink(placement_sink),
    );
    let transition_end = layer.transition_end_handler();
    let transition_cancel = layer.transition_cancel_handler();
    let animation_end = layer.animation_end_handler();
    let animation_cancel = layer.animation_cancel_handler();

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
                placement.clone(),
                transition_end.clone(),
                transition_cancel.clone(),
                animation_end.clone(),
                animation_cancel.clone(),
                children,
            )
            .into_any()
        }}
    }
}

#[cfg(target_arch = "wasm32")]
fn target_is_trigger(
    target: Option<leptos::web_sys::EventTarget>,
    trigger_ref: NodeRef<html::Button>,
) -> bool {
    use leptos::wasm_bindgen::JsCast;

    let Some(target) = target.and_then(|target| target.dyn_into::<leptos::web_sys::Node>().ok())
    else {
        return false;
    };
    trigger_ref
        .get_untracked()
        .and_then(|trigger| trigger.dyn_into::<leptos::web_sys::Node>().ok())
        .is_some_and(|trigger| trigger.contains(Some(&target)))
}

#[cfg(not(target_arch = "wasm32"))]
fn target_is_trigger(
    _target: Option<leptos::web_sys::EventTarget>,
    _trigger_ref: NodeRef<html::Button>,
) -> bool {
    false
}

fn menu_surface(
    node_ref: NodeRef<html::Div>,
    content_id: Signal<String>,
    trigger_id: Signal<String>,
    content_class: Signal<String>,
    data_state: Signal<&'static str>,
    placement: MenuPlacementBinding,
    transition_end: Callback<leptos::ev::TransitionEvent>,
    transition_cancel: Callback<leptos::ev::TransitionEvent>,
    animation_end: Callback<leptos::ev::AnimationEvent>,
    animation_cancel: Callback<leptos::ev::AnimationEvent>,
    children: StoredValue<ChildrenFn>,
) -> impl IntoView {
    let style_placement = placement.clone();
    let strict_id_placement = placement.clone();
    let side_placement = placement.clone();
    let align_placement = placement.clone();

    view! {
        <div
            node_ref=node_ref
            id=move || content_id.get()
            class=move || content_class.get()
            role="menu"
            tabindex="-1"
            style=move || {
                style_placement
                    .strict_id()
                    .is_none()
                    .then(|| style_placement.style())
            }
            data-web-ui-placement-id=move || {
                strict_id_placement
                    .strict_id()
                    .map(|id| id.as_str().to_owned())
            }
            data-state=move || data_state.get()
            data-side=move || side_placement.data_side()
            data-align=move || align_placement.data_align()
            aria-labelledby=move || trigger_id.get()
            on:transitionend=move |event| transition_end.run(event)
            on:transitioncancel=move |event| transition_cancel.run(event)
            on:animationend=move |event| animation_end.run(event)
            on:animationcancel=move |event| animation_cancel.run(event)
        >
            {children.with_value(|children| children())}
        </div>
    }
}
