use leptos::ev::{FocusEvent, KeyboardEvent};
use leptos::html;
use leptos::prelude::*;
use web_ui_primitives::leptos::attrs::{TabsTriggerAttrs, tabs_trigger_attrs};

use super::root::{TabsContext, attr_bool, attr_string, class_with_base, data_attr};

#[component]
pub fn TabsTrigger(
    index: usize,
    #[prop(optional, default = false)] disabled: bool,
    #[prop(optional, into)] class: String,
    children: Children,
) -> impl IntoView {
    let context = use_context::<TabsContext>().expect("TabsTrigger must be used inside TabsRoot");
    context.set_disabled(index, disabled);
    let attrs_context = context.clone();
    let attrs = Signal::derive(move || {
        let trigger_id = attrs_context.trigger_id(index);
        let panel_id = attrs_context.panel_id(index);
        let mut model = attrs_context.model.get();
        if model.len() <= index {
            model.set_len(index + 1);
        }
        model.set_disabled(index, disabled);
        tabs_trigger_attrs(
            &model,
            index,
            TabsTriggerAttrs::new()
                .trigger_id(trigger_id.as_str())
                .controls_id(panel_id.as_str()),
        )
    });
    let node_ref = NodeRef::<html::Button>::new();
    context.register_trigger(index, node_ref);
    let cleanup_context = context.clone();
    on_cleanup(move || {
        cleanup_context.unregister_trigger(index);
    });

    let click_context = context.clone();
    let on_click = move |_| {
        if disabled {
            return;
        }
        click_context.model.update(|model| {
            model.focus_index(Some(index));
            model.select(Some(index));
        });
    };

    let focus_context = context.clone();
    let on_focus = move |_event: FocusEvent| {
        if disabled {
            return;
        }
        focus_context.model.update(|model| {
            model.focus_index(Some(index));
        });
    };

    let key_context = context.clone();
    let on_keydown = move |event: KeyboardEvent| {
        if disabled {
            return;
        }

        let key = event.key();
        if key == "Enter" || key == " " {
            event.prevent_default();
            key_context.model.update(|model| {
                model.activate_focused();
            });
            return;
        }

        let mut focused = None;
        key_context.model.update(|model| {
            focused = model.focus_by_key(&key, key_context.orientation, key_context.direction);
        });

        if let Some(index) = focused {
            event.prevent_default();
            key_context.focus_trigger(index);
        }
    };

    view! {
        <button
            node_ref=node_ref
            class=class_with_base("kit-tabs-trigger", &class)
            type="button"
            id=move || attr_string(&attrs.get(), "id")
            role=move || attr_string(&attrs.get(), "role")
            tabindex=move || attr_string(&attrs.get(), "tabindex")
            disabled=move || attr_bool(&attrs.get(), "disabled")
            aria-selected=move || attr_string(&attrs.get(), "aria-selected")
            aria-controls=move || attr_string(&attrs.get(), "aria-controls")
            aria-disabled=move || attr_string(&attrs.get(), "aria-disabled")
            data-state=move || attr_string(&attrs.get(), "data-state")
            data-disabled=move || data_attr(attr_bool(&attrs.get(), "data-disabled"))
            on:click=on_click
            on:focus=on_focus
            on:keydown=on_keydown
        >
            {children()}
        </button>
    }
}
