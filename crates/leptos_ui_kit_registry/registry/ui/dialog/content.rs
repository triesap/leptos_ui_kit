use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::leptos::{
    DialogLayerOptions, DismissibleReason, Portal, use_dialog_layer_with_node_ref,
};

use super::root::{DialogContext, class_with_base};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DialogContentRole {
    Dialog,
    AlertDialog,
}

impl DialogContentRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dialog => "dialog",
            Self::AlertDialog => "alertdialog",
        }
    }
}

#[component]
pub fn DialogContent(
    #[prop(optional, default = DialogContentRole::Dialog)] role: DialogContentRole,
    #[prop(optional, into)] label: Option<String>,
    #[prop(optional, into)] class: String,
    children: ChildrenFn,
) -> impl IntoView {
    let context =
        use_context::<DialogContext>().expect("DialogContent must be used inside DialogRoot");
    let children = StoredValue::new(children);
    let content_id_value = context.content_id.clone();
    let content_id = Signal::derive(move || content_id_value.clone());
    let content_class_value = class_with_base("luk-dialog-content", &class);
    let content_class = Signal::derive(move || content_class_value.clone());
    let modal = context.modal;
    let label_value = label.clone();
    let labelled_by_value = if label_value.is_some() {
        None
    } else {
        Some(context.title_id.clone())
    };
    let label = Signal::derive(move || label_value.clone());
    let labelled_by = Signal::derive(move || labelled_by_value.clone());
    let description_context = context.clone();
    let described_by = Signal::derive(move || {
        description_context
            .description_present
            .get()
            .then(|| description_context.description_id.clone())
    });
    let node_ref = NodeRef::<html::Div>::new();

    let open_context = context.clone();
    let dismiss_context = context.clone();
    let mut options = DialogLayerOptions::new(Signal::derive(move || open_context.open.get()));
    options.modal = modal;
    options.on_dismiss = Some(Callback::new(move |_reason: DismissibleReason| {
        dismiss_context.set_open(false);
    }));
    let layer = use_dialog_layer_with_node_ref(node_ref, options);
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

            view! {
                <Portal>
                    {move || {
                        dialog_surface(
                            node_ref,
                            content_id,
                            content_class,
                            role,
                            modal,
                            label,
                            labelled_by,
                            described_by,
                            data_state,
                            transition_end.clone(),
                            animation_end.clone(),
                            children,
                        )
                    }}
                </Portal>
            }
            .into_any()
        }}
    }
}

fn dialog_surface(
    node_ref: NodeRef<html::Div>,
    content_id: Signal<String>,
    content_class: Signal<String>,
    role: DialogContentRole,
    modal: bool,
    label: Signal<Option<String>>,
    labelled_by: Signal<Option<String>>,
    described_by: Signal<Option<String>>,
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
            role=role.as_str()
            tabindex="-1"
            data-state=move || data_state.get()
            aria-modal=if modal { "true" } else { "false" }
            aria-label=move || label.get()
            aria-labelledby=move || labelled_by.get()
            aria-describedby=move || described_by.get()
            on:transitionend=move |event| transition_end.run(event)
            on:animationend=move |event| animation_end.run(event)
        >
            {children.with_value(|children| children())}
        </div>
    }
}
