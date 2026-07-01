use std::sync::atomic::{AtomicUsize, Ordering};

use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::core::{Direction, MenuLoop, MenuModel};
use web_ui_primitives::leptos::{DomAttribute, DomAttributeValue};

static NEXT_MENU_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone)]
pub(crate) struct MenuContext {
    pub(crate) model: RwSignal<MenuModel>,
    pub(crate) checked_index: Option<Signal<Option<usize>>>,
    pub(crate) direction: Direction,
    base_id: String,
    item_refs: RwSignal<Vec<Option<NodeRef<html::Button>>>>,
    item_labels: RwSignal<Vec<String>>,
}

impl MenuContext {
    pub(crate) fn ensure_len(&self, index: usize) {
        self.model.update(|model| {
            if model.len() <= index {
                model.set_len(index + 1);
            }
        });
        self.item_refs.update(|refs| {
            if refs.len() <= index {
                refs.resize_with(index + 1, || None);
            }
        });
        self.item_labels.update(|labels| {
            if labels.len() <= index {
                labels.resize(index + 1, String::new());
            }
        });
    }

    pub(crate) fn register_item(
        &self,
        index: usize,
        node_ref: NodeRef<html::Button>,
        label: String,
    ) {
        self.ensure_len(index);
        self.item_refs.update(|refs| {
            refs[index] = Some(node_ref);
        });
        self.item_labels.update(|labels| {
            labels[index] = label;
        });
    }

    pub(crate) fn set_disabled(&self, index: usize, disabled: bool) {
        self.ensure_len(index);
        self.model.update(|model| {
            model.set_disabled(index, disabled);
        });
    }

    pub(crate) fn set_open(&self, open: bool) {
        self.model.update(|model| {
            self.apply_controlled_checked_untracked(model);
            model.set_open(open);
        });
    }

    pub(crate) fn toggle_open(&self) {
        self.model.update(|model| {
            self.apply_controlled_checked_untracked(model);
            model.toggle();
        });
    }

    pub(crate) fn checked_is_controlled(&self) -> bool {
        self.checked_index.is_some()
    }

    pub(crate) fn model_snapshot(&self, update: impl FnOnce(&mut MenuModel)) -> MenuModel {
        let mut model = self.model.get();
        update(&mut model);
        if let Some(checked_index) = self.checked_index {
            model.set_checked(checked_index.get());
        }
        model
    }

    fn apply_controlled_checked_untracked(&self, model: &mut MenuModel) {
        if let Some(checked_index) = self.checked_index {
            model.set_checked(checked_index.get_untracked());
        }
    }

    pub(crate) fn focus_item(&self, index: usize) {
        let node_ref = self
            .item_refs
            .with(|refs| refs.get(index).cloned().flatten());
        if let Some(node_ref) = node_ref {
            if let Some(element) = node_ref.get() {
                let _ = element.focus();
            }
        }
    }

    pub(crate) fn item_labels(&self) -> Vec<String> {
        self.item_labels.get_untracked()
    }

    pub(crate) fn trigger_id(&self) -> String {
        format!("{}-trigger", self.base_id)
    }

    pub(crate) fn content_id(&self) -> String {
        format!("{}-content", self.base_id)
    }
}

#[component]
pub fn MenuRoot(
    #[prop(optional, default = false)] default_open: bool,
    #[prop(optional, into)] checked_index: Option<Signal<Option<usize>>>,
    #[prop(optional, default = MenuLoop::Wrap)] loop_policy: MenuLoop,
    #[prop(optional, default = Direction::Ltr)] direction: Direction,
    #[prop(optional, into)] class: String,
    #[prop(optional, into)] id: Option<String>,
    children: Children,
) -> impl IntoView {
    let mut model = MenuModel::with_loop(0, loop_policy);
    model.set_open(default_open);
    let base_id = id.unwrap_or_else(next_menu_id);
    provide_context(MenuContext {
        model: RwSignal::new(model),
        checked_index,
        direction,
        base_id,
        item_refs: RwSignal::new(Vec::new()),
        item_labels: RwSignal::new(Vec::new()),
    });

    view! {
        <div class=class_with_base("kit-menu", &class)>
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

pub(crate) fn attr_string(attrs: &[DomAttribute], name: &str) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if attr.name() != name {
            return None;
        }
        match attr.value() {
            DomAttributeValue::String(value) => Some(value.clone()),
            DomAttributeValue::Bool(true) => Some(String::new()),
            DomAttributeValue::Bool(false) => None,
        }
    })
}

pub(crate) fn attr_bool(attrs: &[DomAttribute], name: &str) -> bool {
    attrs
        .iter()
        .any(|attr| attr.name() == name && matches!(attr.value(), DomAttributeValue::Bool(true)))
}

pub(crate) fn data_attr(active: bool) -> Option<&'static str> {
    active.then_some("")
}

fn next_menu_id() -> String {
    let id = NEXT_MENU_ID.fetch_add(1, Ordering::Relaxed);
    format!("kit-menu-{id}")
}
