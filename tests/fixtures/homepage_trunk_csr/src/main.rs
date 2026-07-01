use leptos::prelude::*;

mod components;

use components::ui::{
    Button, ButtonSize, ButtonType, ButtonVariant, CollapsibleContent, CollapsibleRoot,
    CollapsibleTrigger, DialogClose, DialogContent, DialogDescription, DialogRoot, DialogTitle,
    DialogTrigger, MenuContent, MenuItem, MenuItemIndicator, MenuItemKind, MenuRoot, MenuTrigger,
    TabsList, TabsPanel, TabsRoot, TabsTrigger,
};

fn main() {
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let (sending, _) = signal(false);
    let (count, set_count) = signal(0);

    view! {
        <main>
            <form>
                <Button
                    variant=ButtonVariant::Primary
                    size=ButtonSize::Lg
                    button_type=ButtonType::Submit
                    disabled=move || sending.get()
                >
                    {move || if sending.get() { "Sending" } else { "Send message" }}
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
