#![forbid(unsafe_code)]

//! Registry layer for configuration and item resolution.

mod config;
mod detect;
mod item;

pub use config::{
    COMPONENTS_SCHEMA_URL, ComponentsConfig, ConfigError, DesiredItemConfig, DesiredItemName,
    InstallConfig, InstallRoots, LEPTOS_ROUTER_VERSION, LEPTOS_VERSION, LeptosConfig,
    NormalizeOptions, NormalizedProjectConfig, ProjectConfig, ProjectKind, RegistryConfig,
    RegistrySource, RenderMode, SCHEMA_VERSION, StateConfig, StylesConfig, StylesMode, TOOL_BINARY,
    TOOL_GIT_URL, TOOL_PACKAGE, ToolConfig, ToolSourceConfig, WorkspaceMode,
    canonical_components_config, canonical_components_json, canonical_tool_config,
    components_config_to_json, components_config_with_desired_item, desired_builtin_button_item,
    desired_builtin_collapsible_item, desired_builtin_tabs_item, normalize_single_crate_project,
    parse_components_json_str,
};
pub use detect::{
    DependencyPlan, DependencyRequirement, DependencyStatus, DetectedDependencySource,
    DetectedProject, DetectionError, InfoOutput, build_info_output,
    dependency_requirement_for_cargo_plan, detect_cargo_plan_requirements,
    detect_single_crate_project,
};
pub use item::{
    CargoPlanEntry, CargoPlanSource, CargoPlanSourceKind, REGISTRY_ITEM_SCHEMA_URL,
    REGISTRY_SCHEMA_URL, RegistryAccessibility, RegistryAccessibilityBehavior, RegistryError,
    RegistryFileTarget, RegistryFileTargetKind, RegistryItem, RegistryItemFile, RegistryItemKind,
    RegistryItemStyle, RegistryLeptos, RegistryRoot, RegistryRootItem, RegistrySourceKind,
    RegistryStyleTarget, RegistryStyleTargetKind, ResolvedRegistryItem, ResolvedRegistryTargets,
    ResolvedStyleBlockTarget, ResolvedUiTarget, WEB_UI_PRIMITIVES_GIT_URL,
    load_built_in_registry_item, load_built_in_registry_root, load_registry_item,
    parse_registry_item_str, parse_registry_root_str, read_built_in_registry_source,
    registry_item_content_hash, resolve_built_in_registry_items, resolve_registry_targets,
    validate_registry_graph,
};
