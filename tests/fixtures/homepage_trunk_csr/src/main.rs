use leptos::prelude::*;

mod components;

use components::ui::{
    Button, ButtonSize, ButtonType, ButtonVariant, CollapsibleContent, CollapsibleRoot,
    CollapsibleTrigger, DialogClose, DialogContent, DialogDescription, DialogRoot, DialogTitle,
    DialogTrigger, FieldLabel, FieldMessage, FieldRequired, FieldRoot, MenuContent, MenuItem,
    MenuItemIndicator, MenuItemKind, MenuRoot, MenuTrigger, NativeSelect, SelectIcon, TabsList,
    TabsPanel, TabsRoot, TabsTrigger, TextArea, TextInput, TextInputType, Spinner, Status,
    StatusPoliteness, StatusRole,
};

fn main() {
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let (sending, _) = signal(false);
    let (count, set_count) = signal(0);
    let (name, set_name) = signal(String::new());
    let (method, set_method) = signal("email".to_owned());
    let (message, set_message) = signal(String::new());

    view! {
        <main>
            <form>
                <Button
                    variant=ButtonVariant::Primary
                    size=ButtonSize::Lg
                    button_type=ButtonType::Submit
                    disabled=move || sending.get()
                >
                    {move || {
                        if sending.get() {
                            view! { <Spinner label="Sending" /> }.into_any()
                        } else {
                            view! { <span>"Send message"</span> }.into_any()
                        }
                    }}
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
            <MenuRoot>
                <MenuTrigger>"Locale"</MenuTrigger>
                <MenuContent>
                    <MenuItem index=0 kind=MenuItemKind::Radio label="English">
                        <span>"English"</span>
                        <MenuItemIndicator index=0>
                            <span>"*"</span>
                        </MenuItemIndicator>
                    </MenuItem>
                    <MenuItem index=1 kind=MenuItemKind::Radio label="Spanish">
                        <span>"Spanish"</span>
                        <MenuItemIndicator index=1>
                            <span>"*"</span>
                        </MenuItemIndicator>
                    </MenuItem>
                </MenuContent>
            </MenuRoot>
        </main>
    }
}
