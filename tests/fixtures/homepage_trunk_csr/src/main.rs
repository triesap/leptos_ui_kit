use leptos::prelude::*;

mod components;

use components::ui::{
    Anchor, AnchorTarget, Button, ButtonSize, ButtonType, ButtonVariant, CollapsibleContent,
    CollapsibleRoot,
    CollapsibleTrigger, DialogClose, DialogContent, DialogDescription, DialogRoot, DialogTitle,
    DialogTrigger, FieldLabel, FieldMessage, FieldRequired, FieldRoot, MenuContent,
    MenuContentAlign, MenuContentSide, MenuRadioItem, MenuRoot, MenuTrigger, NativeSelect,
    RouterLink, SelectIcon, Status, StatusPoliteness, StatusRole, TabsList, TabsPanel, TabsRoot,
    TabsTrigger, TextArea, TextInput, TextInputType,
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
    let (locale_index, set_locale_index) = signal(Some(0));

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
            <MenuRoot checked_index=locale_index>
                <MenuTrigger>"Locale"</MenuTrigger>
                <MenuContent side=MenuContentSide::Bottom align=MenuContentAlign::Start>
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
