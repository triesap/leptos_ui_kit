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
    DependencyPlan, DependencyRequirement, DependencyStatus, DetectedProject, DetectionError,
    InfoOutput, build_info_output, detect_single_crate_project,
};
pub use item::{
    CargoPlanEntry, REGISTRY_ITEM_SCHEMA_URL, REGISTRY_SCHEMA_URL, RegistryError,
    RegistryFileTarget, RegistryFileTargetKind, RegistryItem, RegistryItemFile, RegistryItemKind,
    RegistryItemStyle, RegistryLeptos, RegistryRoot, RegistryRootItem, RegistrySourceKind,
    RegistryStyleTarget, RegistryStyleTargetKind, ResolvedRegistryItem, ResolvedRegistryTargets,
    ResolvedStyleBlockTarget, ResolvedUiTarget, load_built_in_registry_item,
    load_built_in_registry_root, load_registry_item, parse_registry_item_str,
    parse_registry_root_str, registry_item_content_hash, resolve_built_in_registry_items,
    resolve_registry_targets, validate_registry_graph,
};
