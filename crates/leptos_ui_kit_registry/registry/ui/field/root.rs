use std::sync::atomic::{AtomicUsize, Ordering};

use leptos::prelude::*;

static NEXT_FIELD_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone)]
pub(crate) struct FieldContext {
    control_id: String,
    message_id: String,
    pub(crate) required: Signal<bool>,
    pub(crate) invalid: Signal<bool>,
    pub(crate) disabled: Signal<bool>,
}

impl FieldContext {
    pub(crate) fn control_id(&self) -> String {
        self.control_id.clone()
    }

    pub(crate) fn message_id(&self) -> String {
        self.message_id.clone()
    }

    pub(crate) fn required_signal(&self) -> Signal<bool> {
        self.required
    }

    pub(crate) fn invalid_signal(&self) -> Signal<bool> {
        self.invalid
    }

    pub(crate) fn disabled_signal(&self) -> Signal<bool> {
        self.disabled
    }
}

#[component]
pub fn FieldRoot(
    #[prop(optional, into)] id: Option<String>,
    #[prop(optional, into, default = false.into())] required: Signal<bool>,
    #[prop(optional, into, default = false.into())] invalid: Signal<bool>,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let base_id = id.unwrap_or_else(|| next_id("kit-field"));
    provide_context(FieldContext {
        control_id: format!("{base_id}-control"),
        message_id: format!("{base_id}-message"),
        required,
        invalid,
        disabled,
    });

    view! {
        <div
            class=class_with_base("kit-field", &class)
            data-required=move || data_state(required.get())
            data-invalid=move || data_state(invalid.get())
            data-disabled=move || data_state(disabled.get())
        >
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

pub(crate) fn data_state(active: bool) -> Option<&'static str> {
    active.then_some("true")
}

pub(crate) fn field_context() -> Option<FieldContext> {
    use_context::<FieldContext>()
}

pub(crate) fn resolved_control_id(
    id: Option<String>,
    context: &Option<FieldContext>,
    prefix: &'static str,
) -> String {
    id.or_else(|| context.as_ref().map(FieldContext::control_id))
        .unwrap_or_else(|| next_id(prefix))
}

pub(crate) fn resolved_message_id(
    described_by: Option<String>,
    context: &Option<FieldContext>,
) -> Option<String> {
    described_by.or_else(|| context.as_ref().map(FieldContext::message_id))
}

pub(crate) fn resolved_bool_signal(
    signal: Option<Signal<bool>>,
    context_signal: Option<Signal<bool>>,
) -> Signal<bool> {
    signal.or(context_signal).unwrap_or_else(|| false.into())
}

fn next_id(prefix: &'static str) -> String {
    let id = NEXT_FIELD_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{id}")
}
