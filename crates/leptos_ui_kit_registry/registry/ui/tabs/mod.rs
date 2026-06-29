mod list;
mod panel;
mod root;
mod trigger;

pub use list::TabsList;
pub use panel::TabsPanel;
pub use root::TabsRoot;
pub use trigger::TabsTrigger;
pub use web_ui_primitives::core::{
    Direction as TabsDirection, Orientation as TabsOrientation, TabsActivation, TabsLoop,
};
