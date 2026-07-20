#![forbid(unsafe_code)]

use web_ui_primitives::{
    core::{Direction, MenuLoop, MenuModel},
    leptos::{
        DomAttribute, DomAttributeValue,
        attrs::{
            MenuItemAttrs, MenuItemKind, MenuTriggerAttrs, menu_item_attrs,
            menu_item_indicator_attrs, menu_trigger_attrs,
        },
    },
};

#[test]
fn menu_attrs_expose_checked_indicator_state() {
    let mut model = MenuModel::with_loop(2, MenuLoop::Wrap);
    model.set_checked(Some(0));

    let active_attrs = menu_item_indicator_attrs(&model, 0);
    let inactive_attrs = menu_item_indicator_attrs(&model, 1);

    assert_eq!(bool_attr(&active_attrs, "hidden"), Some(false));
    assert_eq!(string_attr(&active_attrs, "data-state"), Some("checked"));
    assert_eq!(bool_attr(&inactive_attrs, "hidden"), Some(true));
    assert_eq!(
        string_attr(&inactive_attrs, "data-state"),
        Some("unchecked")
    );
}

#[test]
fn menu_attrs_expose_trigger_and_item_open_state() {
    let mut model = MenuModel::with_loop(2, MenuLoop::Wrap);

    let closed_attrs = menu_trigger_attrs(
        &model,
        MenuTriggerAttrs::new().controls_id("locale-menu-content"),
    );
    assert_eq!(string_attr(&closed_attrs, "aria-expanded"), Some("false"));
    assert_eq!(string_attr(&closed_attrs, "data-state"), Some("closed"));
    assert_eq!(
        string_attr(&closed_attrs, "aria-controls"),
        Some("locale-menu-content")
    );

    model.set_open(true);
    model.focus_index(Some(1));
    model.set_checked(Some(1));

    let open_attrs = menu_trigger_attrs(
        &model,
        MenuTriggerAttrs::new().controls_id("locale-menu-content"),
    );
    let focused_item_attrs =
        menu_item_attrs(&model, 1, MenuItemAttrs::new().kind(MenuItemKind::Radio));

    assert_eq!(string_attr(&open_attrs, "aria-expanded"), Some("true"));
    assert_eq!(string_attr(&open_attrs, "data-state"), Some("open"));
    assert_eq!(
        string_attr(&focused_item_attrs, "role"),
        Some("menuitemradio")
    );
    assert_eq!(string_attr(&focused_item_attrs, "tabindex"), Some("0"));
    assert_eq!(
        string_attr(&focused_item_attrs, "aria-checked"),
        Some("true")
    );
    assert_eq!(
        bool_attr(&focused_item_attrs, "data-highlighted"),
        Some(true)
    );
}

#[test]
fn menu_model_keyboard_contract_closes_and_selects() {
    let mut model = MenuModel::with_loop(3, MenuLoop::Wrap);
    model.set_disabled(1, true);
    model.set_open(true);

    assert_eq!(model.focus_by_key("ArrowDown", Direction::Ltr), Some(2));
    assert_eq!(model.activate_index(2), Some(2));
    assert!(!model.open());
    assert_eq!(model.focused(), None);

    model.set_open(true);
    assert!(model.close_by_key("Escape"));
    assert!(!model.open());
}

fn string_attr<'a>(attrs: &'a [DomAttribute], name: &str) -> Option<&'a str> {
    attrs.iter().find_map(|attr| {
        if attr.name() != name {
            return None;
        }
        match attr.value() {
            DomAttributeValue::String(value) => Some(value.as_str()),
            DomAttributeValue::Bool(_) => None,
        }
    })
}

fn bool_attr(attrs: &[DomAttribute], name: &str) -> Option<bool> {
    attrs.iter().find_map(|attr| {
        if attr.name() != name {
            return None;
        }
        match attr.value() {
            DomAttributeValue::String(_) => None,
            DomAttributeValue::Bool(value) => Some(*value),
        }
    })
}
