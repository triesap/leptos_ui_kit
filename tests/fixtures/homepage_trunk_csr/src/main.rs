use leptos::prelude::*;
#[cfg(target_arch = "wasm32")]
use web_ui_primitives::leptos::PortalMount;

mod components;

use components::ui::{
    Anchor, AnchorTarget, Button, ButtonSize, ButtonType, ButtonVariant, CollapsibleContent,
    CollapsibleRoot, CollapsibleTrigger, DialogClose, DialogContent, DialogContentRole,
    DialogDescription, DialogRoot, DialogTitle, DialogTrigger, FieldLabel, FieldMessage,
    FieldRequired, FieldRoot, FieldSlot, FieldSurface, MenuContent, MenuContentAlign,
    MenuContentSide, MenuDirection, MenuItem, MenuItemIndicator, MenuItemKind, MenuLoop,
    MenuRadioItem, MenuRoot, MenuTrigger, NativeSelect, RouterLink, SelectField, SelectIcon,
    Spinner, SpinnerMode, Status, StatusPoliteness, StatusRole, TabsActivation, TabsDirection,
    TabsList, TabsLoop, TabsOrientation, TabsPanel, TabsRoot, TabsTrigger, TextArea, TextAreaField,
    TextField, TextInput, TextInputType,
};

#[allow(unused_imports)]
mod historical_generated_module_paths {
    use super::components::ui::anchor::{Anchor, AnchorTarget};
    use super::components::ui::button::{Button, ButtonSize, ButtonType, ButtonVariant};
    use super::components::ui::collapsible::{
        CollapsibleContent, CollapsibleRoot, CollapsibleTrigger,
    };
    use super::components::ui::dialog::{
        DialogClose, DialogContent, DialogContentRole, DialogDescription, DialogRoot, DialogTitle,
        DialogTrigger,
    };
    use super::components::ui::field::{
        FieldLabel, FieldMessage, FieldRequired, FieldRoot, FieldSlot, FieldSurface, NativeSelect,
        SelectField, SelectIcon, TextArea, TextAreaField, TextField, TextInput, TextInputType,
    };
    use super::components::ui::menu::{
        MenuContent, MenuContentAlign, MenuContentSide, MenuDirection, MenuItem, MenuItemIndicator,
        MenuItemKind, MenuLoop, MenuRadioItem, MenuRoot, MenuTrigger,
    };
    use super::components::ui::router_link::RouterLink;
    use super::components::ui::spinner::{Spinner, SpinnerMode};
    use super::components::ui::status::{Status, StatusPoliteness, StatusRole};
    use super::components::ui::tabs::{
        TabsActivation, TabsDirection, TabsList, TabsLoop, TabsOrientation, TabsPanel, TabsRoot,
        TabsTrigger,
    };
}

fn main() {
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let (sending, _) = signal(false);
    let (count, set_count) = signal(0);
    let (name, set_name) = signal(String::new());
    let (email, set_email) = signal(String::new());
    let (method, set_method) = signal("email".to_owned());
    let (message, set_message) = signal(String::new());
    let (body, set_body) = signal(String::new());
    let (locale_index, set_locale_index) = signal(Some(0));
    let selected_method = Signal::derive(move || match method.get().as_str() {
        "email" => "Email",
        "nostr" => "Nostr",
        _ => "Unknown",
    }.to_owned());

    view! {
        <main>
            <form>
                <Button
                    variant=ButtonVariant::Primary
                    size=ButtonSize::Lg
                    button_type=ButtonType::Submit
                    disabled=move || sending.get()
                    loading=move || sending.get()
                    loading_label="Sending"
                >
                    <span>"Send message"</span>
                </Button>
            </form>
            <Button
                variant=ButtonVariant::Ghost
                size=ButtonSize::Sm
                button_type=ButtonType::Button
                on_click=Callback::new(move |_| set_count.update(|count| *count += 1))
            >
                {move || format!("Clicked {}", count.get())}
            </Button>
            <Anchor href="https://example.com" target=AnchorTarget::Blank>
                "External link"
            </Anchor>
            <RouterLink href="/contact">
                "Contact"
            </RouterLink>
            <TextField
                id="contact-email"
                label="Email"
                input_type=TextInputType::Email
                name="email"
                value=email
                required=true
                on_input=Callback::new(move |value| set_email.set(value))
                label_action=|| view! {
                    <button type="button">"Use browser signer"</button>
                }
            />
            <SelectField
                id="contact-kind"
                label="Contact"
                name="contact_kind"
                value=method
                selected_label=selected_method
                on_change=Callback::new(move |value| set_method.set(value))
                label_action=|| view! {
                    <span>"Browser signer available"</span>
                }
                icon=|| view! {
                    <span>"v"</span>
                }
            >
                <option value="email">"Email"</option>
                <option value="nostr">"Nostr"</option>
            </SelectField>
            <TextAreaField
                id="contact-body"
                label="Body"
                name="body"
                value=body
                rows=4
                required=true
                on_input=Callback::new(move |value| set_body.set(value))
                label_action=|| view! {
                    <span>"Required"</span>
                }
            />
            <FieldRoot id="contact-name" required=true invalid=false disabled=false>
                <FieldLabel>
                    "Name"
                    <FieldRequired />
                </FieldLabel>
                <TextInput
                    input_type=TextInputType::Text
                    name="name"
                    value=name
                    on_input=Callback::new(move |value| set_name.set(value))
                />
                <FieldMessage>"Use your public name."</FieldMessage>
            </FieldRoot>
            <FieldRoot id="contact-method">
                <FieldLabel>"Contact"</FieldLabel>
                <NativeSelect
                    name="contact_method"
                    value=method
                    on_change=Callback::new(move |value| set_method.set(value))
                >
                    <option value="email">"Email"</option>
                    <option value="nostr">"Nostr"</option>
                </NativeSelect>
                <SelectIcon>"v"</SelectIcon>
            </FieldRoot>
            <FieldRoot id="contact-message" required=true>
                <FieldLabel>
                    "Message"
                    <FieldRequired />
                </FieldLabel>
                <TextArea
                    name="message"
                    value=message
                    rows=4
                    on_input=Callback::new(move |value| set_message.set(value))
                />
            </FieldRoot>
            <Status role=StatusRole::Status politeness=StatusPoliteness::Polite>
                "Message sent"
            </Status>
            <Spinner label="Loading" />
            <Spinner mode=SpinnerMode::Decorative />
            <CollapsibleRoot>
                <CollapsibleTrigger>"Details"</CollapsibleTrigger>
                <CollapsibleContent>
                    <p>"Primitive-backed content"</p>
                </CollapsibleContent>
            </CollapsibleRoot>
            <TabsRoot>
                <TabsList>
                    <TabsTrigger index=0>"First"</TabsTrigger>
                    <TabsTrigger index=1>"Second"</TabsTrigger>
                </TabsList>
                <TabsPanel index=0>
                    <p>"First panel"</p>
                </TabsPanel>
                <TabsPanel index=1>
                    <p>"Second panel"</p>
                </TabsPanel>
            </TabsRoot>
            <DialogRoot>
                <DialogTrigger>"Open dialog"</DialogTrigger>
                <DialogContent>
                    <DialogTitle>"Dialog title"</DialogTitle>
                    <DialogDescription>"Dialog description"</DialogDescription>
                    <DialogClose>"Close"</DialogClose>
                </DialogContent>
            </DialogRoot>
            <DarkThemeDialog />
            <MenuRoot checked_index=locale_index>
                <MenuTrigger>"Locale"</MenuTrigger>
                <MenuContent side=MenuContentSide::Bottom align=MenuContentAlign::End>
                    <MenuRadioItem
                        index=0
                        label="English"
                        on_select=Callback::new(move |_| set_locale_index.set(Some(0)))
                    >
                        <span>"*"</span>
                    </MenuRadioItem>
                    <MenuRadioItem
                        index=1
                        label="Spanish"
                        on_select=Callback::new(move |_| set_locale_index.set(Some(1)))
                    >
                        <span>"*"</span>
                    </MenuRadioItem>
                </MenuContent>
            </MenuRoot>
        </main>
    }
}

// Compile-only coverage for the complete generated Rust API that shipped before
// the theme-token refactor. The workflow test installs these sources and checks
// this fixture for both the host and wasm32 targets.
#[allow(dead_code)]
#[component]
fn HistoricalApiCompatibility() -> impl IntoView {
    historical_enum_variants();

    let (string_value, _) = signal("value".to_owned());
    let optional_message = Signal::derive(|| Some("Message".to_owned()));
    let checked_index = Signal::derive(|| Some(0));
    let dialog_open = RwSignal::new(false);

    let slot = FieldSlot::new(|| view! { <span>"Slot"</span> });
    let _ = slot.is_present();
    let _ = slot.render();
    let _ = FieldSlot::empty();
    let _: FieldSlot = FieldSlot::default();
    let _: FieldSlot = (|| view! { <span>"Converted slot"</span> }).into();

    view! {
        <section class="historical-api-compatibility">
            <Anchor
                href="/historical-anchor"
                target=AnchorTarget::SameTab
                rel="author"
                class="historical-anchor"
            >
                "Anchor"
            </Anchor>
            <Button
                variant=ButtonVariant::Secondary
                size=ButtonSize::Md
                button_type=ButtonType::Reset
                disabled=false
                loading=false
                loading_label="Loading historical button"
                on_click=Callback::new(|_: leptos::ev::MouseEvent| {})
                class="historical-button"
            >
                "Button"
            </Button>
            <RouterLink href="/historical-router-link" class="historical-router-link">
                "Router link"
            </RouterLink>
            <Spinner
                mode=SpinnerMode::Status
                label="Loading historical fixture"
                class="historical-spinner"
            />
            <Status
                role=StatusRole::Alert
                politeness=StatusPoliteness::Assertive
                atomic=false
                class="historical-status"
            >
                "Status"
            </Status>

            <CollapsibleRoot
                default_open=true
                disabled=false
                class="historical-collapsible"
                content_id="historical-collapsible-content"
            >
                <CollapsibleTrigger class="historical-collapsible-trigger">
                    "Trigger"
                </CollapsibleTrigger>
                <CollapsibleContent class="historical-collapsible-content">
                    "Content"
                </CollapsibleContent>
            </CollapsibleRoot>

            <DialogRoot
                default_open=false
                open=dialog_open
                modal=true
                class="historical-dialog"
                id="historical-dialog"
            >
                <DialogTrigger disabled=false class="historical-dialog-trigger">
                    "Trigger"
                </DialogTrigger>
                <DialogContent
                    role=DialogContentRole::AlertDialog
                    label="Historical dialog"
                    class="historical-dialog-content"
                >
                    <DialogTitle class="historical-dialog-title">"Title"</DialogTitle>
                    <DialogDescription class="historical-dialog-description">
                        "Description"
                    </DialogDescription>
                    <DialogClose disabled=false class="historical-dialog-close">
                        "Close"
                    </DialogClose>
                </DialogContent>
            </DialogRoot>

            <FieldRoot
                id="historical-field"
                required=true
                invalid=false
                disabled=false
                class="historical-field"
            >
                <FieldLabel class="historical-field-label">
                    "Label"
                    <FieldRequired class="historical-field-required" />
                </FieldLabel>
                <FieldSurface class="historical-field-surface">
                    <TextInput
                        input_type=TextInputType::Text
                        id="historical-text-input"
                        name="historical_text_input"
                        value=string_value
                        autocomplete="name"
                        required=true
                        disabled=false
                        invalid=false
                        described_by="historical-text-input-help"
                        on_input=Callback::new(|_: String| {})
                        class="historical-text-input"
                    />
                </FieldSurface>
                <FieldMessage class="historical-field-message">"Message"</FieldMessage>
            </FieldRoot>
            <TextArea
                id="historical-text-area"
                name="historical_text_area"
                value=string_value
                required=true
                disabled=false
                invalid=false
                described_by="historical-text-area-help"
                rows=6
                on_input=Callback::new(|_: String| {})
                class="historical-text-area"
            />
            <NativeSelect
                id="historical-native-select"
                name="historical_native_select"
                value=string_value
                required=true
                disabled=false
                invalid=false
                described_by="historical-native-select-help"
                on_change=Callback::new(|_: String| {})
                class="historical-native-select"
            >
                <option value="value">"Value"</option>
            </NativeSelect>
            <SelectIcon class="historical-select-icon">"Icon"</SelectIcon>
            <TextField
                id="historical-text-field"
                label="Text field"
                input_type=TextInputType::Search
                name="historical_text_field"
                value=string_value
                autocomplete="off"
                required=true
                invalid=false
                disabled=false
                message=optional_message
                on_input=Callback::new(|_: String| {})
                class="historical-text-field"
                surface_class="historical-text-field-surface"
                label_row_class="historical-text-field-label-row"
                label_class="historical-text-field-label"
                required_class="historical-text-field-required"
                input_class="historical-text-field-input"
                message_class="historical-text-field-message"
                label_action=|| view! { <span>"Action"</span> }
            />
            <TextAreaField
                id="historical-text-area-field"
                label="Text area field"
                name="historical_text_area_field"
                value=string_value
                required=true
                invalid=false
                disabled=false
                message=optional_message
                rows=7
                on_input=Callback::new(|_: String| {})
                class="historical-text-area-field"
                surface_class="historical-text-area-field-surface"
                label_row_class="historical-text-area-field-label-row"
                label_class="historical-text-area-field-label"
                required_class="historical-text-area-field-required"
                text_area_class="historical-text-area-field-input"
                message_class="historical-text-area-field-message"
                label_action=|| view! { <span>"Action"</span> }
            />
            <SelectField
                id="historical-select-field"
                label="Select field"
                name="historical_select_field"
                value=string_value
                selected_label=string_value
                required=true
                invalid=false
                disabled=false
                message=optional_message
                on_change=Callback::new(|_: String| {})
                class="historical-select-field"
                surface_class="historical-select-field-surface"
                label_row_class="historical-select-field-label-row"
                label_class="historical-select-field-label"
                required_class="historical-select-field-required"
                select_class="historical-select-field-select"
                value_row_class="historical-select-field-value-row"
                value_class="historical-select-field-value"
                icon_class="historical-select-field-icon"
                message_class="historical-select-field-message"
                label_action=|| view! { <span>"Action"</span> }
                icon=|| view! { <span>"Icon"</span> }
            >
                <option value="value">"Value"</option>
            </SelectField>

            <MenuRoot
                default_open=false
                checked_index=checked_index
                loop_policy=MenuLoop::Clamp
                direction=MenuDirection::Rtl
                class="historical-menu"
                id="historical-menu"
            >
                <MenuTrigger disabled=false class="historical-menu-trigger">
                    "Trigger"
                </MenuTrigger>
                <MenuContent
                    side=MenuContentSide::Top
                    align=MenuContentAlign::Center
                    spacing=6.0
                    viewport_padding=10.0
                    class="historical-menu-content"
                >
                    <MenuItem
                        index=0
                        kind=MenuItemKind::Item
                        disabled=false
                        label="Item"
                        on_select=Callback::new(|_: usize| {})
                        class="historical-menu-item"
                    >
                        "Item"
                        <MenuItemIndicator index=0 class="historical-menu-item-indicator">
                            "Indicator"
                        </MenuItemIndicator>
                    </MenuItem>
                    <MenuRadioItem
                        index=1
                        label="Radio item"
                        disabled=false
                        on_select=Callback::new(|_: usize| {})
                        class="historical-menu-radio-item"
                        label_class="historical-menu-radio-label"
                        indicator_class="historical-menu-radio-indicator"
                    >
                        "Indicator"
                    </MenuRadioItem>
                </MenuContent>
            </MenuRoot>

            <TabsRoot
                activation=TabsActivation::Manual
                loop_policy=TabsLoop::Clamp
                orientation=TabsOrientation::Vertical
                direction=TabsDirection::Rtl
                class="historical-tabs"
                id="historical-tabs"
            >
                <TabsList class="historical-tabs-list">
                    <TabsTrigger index=0 disabled=false class="historical-tabs-trigger">
                        "Trigger"
                    </TabsTrigger>
                </TabsList>
                <TabsPanel index=0 class="historical-tabs-panel">"Panel"</TabsPanel>
            </TabsRoot>
        </section>
    }
}

#[allow(dead_code)]
fn historical_enum_variants() {
    let _: [AnchorTarget; 4] = [
        AnchorTarget::SameTab,
        AnchorTarget::Blank,
        AnchorTarget::Parent,
        AnchorTarget::Top,
    ];
    let _: [ButtonVariant; 3] = [
        ButtonVariant::Primary,
        ButtonVariant::Secondary,
        ButtonVariant::Ghost,
    ];
    let _: [ButtonSize; 3] = [ButtonSize::Sm, ButtonSize::Md, ButtonSize::Lg];
    let _: [ButtonType; 3] = [ButtonType::Button, ButtonType::Submit, ButtonType::Reset];
    let _: [SpinnerMode; 2] = [SpinnerMode::Status, SpinnerMode::Decorative];
    let _: [StatusRole; 2] = [StatusRole::Status, StatusRole::Alert];
    let _: [StatusPoliteness; 2] = [StatusPoliteness::Polite, StatusPoliteness::Assertive];
    let _: [DialogContentRole; 2] = [
        DialogContentRole::Dialog,
        DialogContentRole::AlertDialog,
    ];
    let _: [TextInputType; 6] = [
        TextInputType::Text,
        TextInputType::Email,
        TextInputType::Password,
        TextInputType::Search,
        TextInputType::Tel,
        TextInputType::Url,
    ];
    let _: [MenuContentSide; 4] = [
        MenuContentSide::Bottom,
        MenuContentSide::Top,
        MenuContentSide::Right,
        MenuContentSide::Left,
    ];
    let _: [MenuContentAlign; 3] = [
        MenuContentAlign::Start,
        MenuContentAlign::Center,
        MenuContentAlign::End,
    ];
    let _: [MenuItemKind; 2] = [MenuItemKind::Item, MenuItemKind::Radio];
    let _: [MenuDirection; 2] = [MenuDirection::Ltr, MenuDirection::Rtl];
    let _: [MenuLoop; 2] = [MenuLoop::Wrap, MenuLoop::Clamp];
    let _: [TabsActivation; 2] = [TabsActivation::Automatic, TabsActivation::Manual];
    let _: [TabsLoop; 2] = [TabsLoop::Wrap, TabsLoop::Clamp];
    let _: [TabsOrientation; 2] = [TabsOrientation::Horizontal, TabsOrientation::Vertical];
    let _: [TabsDirection; 2] = [TabsDirection::Ltr, TabsDirection::Rtl];
}

#[cfg(target_arch = "wasm32")]
#[component]
fn DarkThemeDialog() -> impl IntoView {
    let portal_mount = explicit_dialog_portal_mount().expect("dark portal root should exist");

    view! {
        <section class="preview-pane" data-ui-theme="dark">
            <DialogRoot>
                <DialogTrigger>"Open dark dialog"</DialogTrigger>
                <DialogContent portal_mount=portal_mount>
                    <DialogTitle>"Dark dialog title"</DialogTitle>
                    <DialogDescription>"Mounted in the dark theme scope."</DialogDescription>
                    <DialogClose>"Close"</DialogClose>
                </DialogContent>
            </DialogRoot>
        </section>
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[component]
fn DarkThemeDialog() -> impl IntoView {
    view! {
        <section class="preview-pane" data-ui-theme="dark">
            <DialogRoot>
                <DialogTrigger>"Open dark dialog"</DialogTrigger>
                <DialogContent>
                    <DialogTitle>"Dark dialog title"</DialogTitle>
                    <DialogDescription>"Mounted in the dark theme scope."</DialogDescription>
                    <DialogClose>"Close"</DialogClose>
                </DialogContent>
            </DialogRoot>
        </section>
    }
}

#[cfg(target_arch = "wasm32")]
fn explicit_dialog_portal_mount() -> Option<PortalMount> {
    leptos::web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("dark-theme-portal-root"))
}
