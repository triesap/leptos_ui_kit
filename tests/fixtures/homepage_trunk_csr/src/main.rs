use leptos::prelude::*;

mod components;

use components::ui::{
    Button, ButtonSize, ButtonType, ButtonVariant, CollapsibleContent, CollapsibleRoot,
    CollapsibleTrigger,
};

fn main() {
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let (sending, _) = signal(false);

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
            <CollapsibleRoot>
                <CollapsibleTrigger>"Details"</CollapsibleTrigger>
                <CollapsibleContent>
                    <p>"Primitive-backed content"</p>
                </CollapsibleContent>
            </CollapsibleRoot>
        </main>
    }
}
