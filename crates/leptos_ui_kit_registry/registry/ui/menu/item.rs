use leptos::ev::{FocusEvent, KeyboardEvent};
use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::leptos::{
    attrs::{MenuItemAttrs, MenuItemKind as AttrsMenuItemKind, menu_item_attrs},
    use_dom_bindings,
};

use super::root::{MenuContext, class_with_base};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuItemKind {
    Item,
    Radio,
}

impl MenuItemKind {
    fn as_attrs_kind(self) -> AttrsMenuItemKind {
        match self {
            Self::Item => AttrsMenuItemKind::Item,
            Self::Radio => AttrsMenuItemKind::Radio,
        }
    }
}

#[component]
pub fn MenuItem(
    index: usize,
    #[prop(optional, default = MenuItemKind::Item)] kind: MenuItemKind,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, into)] label: Option<String>,
    #[prop(optional)] on_select: Option<Callback<usize>>,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context = use_context::<MenuContext>().expect("MenuItem must be used inside MenuRoot");
    context.set_disabled(index, disabled.get_untracked());
    let attrs_context = context.clone();
    let attrs = Signal::derive(move || {
        let mut model = attrs_context.model.get();
        if model.len() <= index {
            model.set_len(index + 1);
        }
        model.set_disabled(index, disabled.get());
        menu_item_attrs(
            &model,
            index,
            MenuItemAttrs::new().kind(kind.as_attrs_kind()),
        )
    });
    let bindings = use_dom_bindings::<html::Button>(attrs, Vec::new());
    let node_ref = bindings.node_ref();
    context.register_item(index, node_ref, label.unwrap_or_default());

    let click_context = context.clone();
    let click_on_select = on_select.clone();
    let on_click = move |_| {
        if disabled.get_untracked() {
            return;
        }
        if activate_item(&click_context, index, kind) {
            if let Some(on_select) = click_on_select.as_ref() {
                on_select.run(index);
            }
        }
    };

    let focus_context = context.clone();
    let on_focus = move |_event: FocusEvent| {
        if disabled.get_untracked() {
            return;
        }
        focus_context.model.update(|model| {
            model.focus_index(Some(index));
        });
    };

    let key_context = context.clone();
    let key_on_select = on_select;
    let on_keydown = move |event: KeyboardEvent| {
        if disabled.get_untracked() {
            return;
        }

        let key = event.key();
        if key == "Enter" || key == " " {
            event.prevent_default();
            if activate_item(&key_context, index, kind) {
                if let Some(on_select) = key_on_select.as_ref() {
                    on_select.run(index);
                }
            }
            return;
        }

        let mut focused = None;
        key_context.model.update(|model| {
            focused = model.focus_by_key(&key, key_context.direction);
        });
        if let Some(index) = focused {
            event.prevent_default();
            key_context.focus_item(index);
            return;
        }

        let labels = key_context.item_labels();
        let mut typed = None;
        key_context.model.update(|model| {
            typed = model
                .typeahead_by_key(&key, event_time_ms(&event), &labels, |label| label.as_str());
        });
        if let Some(index) = typed {
            event.prevent_default();
            key_context.focus_item(index);
        }
    };

    view! {
        <button
            node_ref=node_ref
            class=class_with_base("kit-menu-item", &class)
            on:click=on_click
            on:focus=on_focus
            on:keydown=on_keydown
        >
            {children()}
        </button>
    }
}

fn activate_item(context: &MenuContext, index: usize, kind: MenuItemKind) -> bool {
    let mut activated = false;
    context.model.update(|model| {
        if kind == MenuItemKind::Radio {
            model.set_checked(Some(index));
        }
        activated = model.activate_index(index).is_some();
    });
    activated
}

fn event_time_ms(event: &KeyboardEvent) -> u64 {
    event.time_stamp().max(0.0).round() as u64
}
