use leptos::prelude::*;
#[cfg(target_arch = "wasm32")]
use web_ui_primitives::leptos::PortalMount;

mod components;

use components::ui::{
    Anchor, AnchorTarget, Button, ButtonSize, ButtonType, ButtonVariant, CollapsibleContent,
    CollapsibleRoot,
    CollapsibleTrigger, DialogClose, DialogContent, DialogDescription, DialogRoot, DialogTitle,
    DialogTrigger, FieldLabel, FieldMessage, FieldRequired, FieldRoot, MenuContent,
    MenuContentAlign, MenuContentSide, MenuRadioItem, MenuRoot, MenuTrigger, NativeSelect,
    RouterLink, SelectField, SelectIcon, Spinner, SpinnerMode, Status, StatusPoliteness,
    StatusRole, TabsList, TabsPanel, TabsRoot, TabsTrigger, TextArea, TextAreaField, TextField,
    TextInput, TextInputType,
};

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
