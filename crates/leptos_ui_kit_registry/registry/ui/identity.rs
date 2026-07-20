use std::collections::BTreeMap;

use leptos::prelude::*;

#[derive(Clone, Copy)]
struct KitIdScope {
    next_by_prefix: RwSignal<BTreeMap<&'static str, usize>>,
}

impl KitIdScope {
    fn new() -> Self {
        Self {
            next_by_prefix: RwSignal::new(BTreeMap::new()),
        }
    }

    fn next(self, prefix: &'static str) -> String {
        let mut ordinal = 0;
        self.next_by_prefix.update(|next_by_prefix| {
            let next = next_by_prefix.entry(prefix).or_insert(1);
            ordinal = *next;
            *next += 1;
        });
        format!("{prefix}-{ordinal}")
    }
}

#[component]
pub fn KitIdProvider(children: Children) -> impl IntoView {
    provide_context(KitIdScope::new());
    children()
}

pub(crate) fn use_kit_id(prefix: &'static str) -> String {
    let scope = use_context::<KitIdScope>().unwrap_or_else(|| {
        let scope = KitIdScope::new();
        provide_context(scope);
        scope
    });
    scope.next(prefix)
}
