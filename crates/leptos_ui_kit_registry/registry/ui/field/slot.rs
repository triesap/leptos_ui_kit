use std::sync::Arc;

use leptos::prelude::*;

#[derive(Clone)]
pub struct FieldSlot {
    present: bool,
    render: Arc<dyn Fn() -> AnyView + Send + Sync>,
}

impl FieldSlot {
    pub fn new<F, V>(render: F) -> Self
    where
        F: Fn() -> V + Send + Sync + 'static,
        V: IntoView + 'static,
    {
        Self {
            present: true,
            render: Arc::new(move || render().into_any()),
        }
    }

    pub fn empty() -> Self {
        Self {
            present: false,
            render: Arc::new(|| ().into_any()),
        }
    }

    pub fn is_present(&self) -> bool {
        self.present
    }

    pub fn render(&self) -> AnyView {
        (self.render)()
    }
}

impl Default for FieldSlot {
    fn default() -> Self {
        Self::empty()
    }
}

impl<F, V> From<F> for FieldSlot
where
    F: Fn() -> V + Send + Sync + 'static,
    V: IntoView + 'static,
{
    fn from(render: F) -> Self {
        Self::new(render)
    }
}
