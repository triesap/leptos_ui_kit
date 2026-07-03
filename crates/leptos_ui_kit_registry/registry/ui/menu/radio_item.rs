use leptos::prelude::*;

use super::{MenuItem, MenuItemIndicator, MenuItemKind};

#[component]
pub fn MenuRadioItem(
    index: usize,
    #[prop(into)] label: Signal<String>,
    #[prop(optional, into, default = false.into())] disabled: Signal<bool>,
    #[prop(optional, default = Callback::new(|_| {}))] on_select: Callback<usize>,
    #[prop(optional, into)] class: String,
    #[prop(optional, into)] label_class: String,
    #[prop(optional, into)] indicator_class: String,
    children: Children,
) -> impl IntoView {
    let label_for_item = label;
    let label_for_text = label;

    view! {
        <MenuItem
            index=index
            kind=MenuItemKind::Radio
            disabled=disabled
            label=label_for_item
            on_select=on_select
            class=class
        >
            <span class=super::root::class_with_base("kit-menu-radio-item-label", &label_class)>
                {move || label_for_text.get()}
            </span>
            <MenuItemIndicator index=index class=indicator_class>
                {children()}
            </MenuItemIndicator>
        </MenuItem>
    }
}
