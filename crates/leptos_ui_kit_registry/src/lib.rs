#![forbid(unsafe_code)]

//! Registry layer for configuration and item resolution.

mod config;
mod detect;
mod item;

pub use config::{
    COMPONENTS_SCHEMA_URL, ComponentsConfig, ConfigError, InstallConfig, InstallRoots,
    LEPTOS_ROUTER_VERSION, LEPTOS_VERSION, LeptosConfig, NormalizeOptions, NormalizedProjectConfig,
    ProjectConfig, ProjectKind, RegistryConfig, RegistrySource, RenderMode, SCHEMA_VERSION,
    StateConfig, StylesConfig, StylesMode, WorkspaceMode, canonical_components_config,
    canonical_components_json, normalize_single_crate_project, parse_components_json_str,
};
pub use detect::{
    DetectedProject, DetectionError, InfoOutput, build_info_output, detect_single_crate_project,
};
pub use item::{
    RegistryError, RegistryItem, RegistryItemFile, RegistryItemType, RegistrySourceKind,
    ResolvedRegistryItem, load_built_in_registry_item, load_local_registry_item,
    load_registry_item, parse_registry_item_str,
};
