#![forbid(unsafe_code)]

//! Registry layer for configuration and item resolution.

mod builtin_registry;
mod config;
mod detect;
mod embedded_assets;
mod item;
mod registry_health;
mod theme_contract;

pub use config::{
    ConfigError, DEFAULT_CSS_PATH, DEFAULT_KIT_CONFIG_PATH, DEFAULT_KIT_DIR, DEFAULT_UI_DIR,
    DesiredItemConfig, DesiredItemName, InstallConfig, InstallRoots, KIT_SCHEMA_URL, KitConfig,
    LEPTOS_ROUTER_VERSION, LEPTOS_VERSION, LeptosConfig, NormalizeOptions, NormalizedProjectConfig,
    ProjectConfig, ProjectKind, RegistryConfig, RegistrySource, RenderMode, SCHEMA_VERSION,
    StylesConfig, StylesMode, TOOL_BINARY, TOOL_GIT_URL, TOOL_PACKAGE, ToolConfig,
    ToolSourceConfig, WorkspaceMode, canonical_kit_config, canonical_kit_json,
    canonical_tool_config, desired_builtin_anchor_item, desired_builtin_button_item,
    desired_builtin_collapsible_item, desired_builtin_dialog_item, desired_builtin_field_item,
    desired_builtin_menu_item, desired_builtin_router_link_item, desired_builtin_spinner_item,
    desired_builtin_status_item, desired_builtin_tabs_item, desired_builtin_tokens_item,
    kit_config_for_write, kit_config_to_json, kit_config_with_desired_item,
    normalize_project_with_workspace_mode, normalize_single_crate_project, parse_kit_json_str,
};
pub use detect::{
    DependencyDeclarationKind, DependencyIncompatibility, DependencyPlan, DependencyRequirement,
    DependencyStatus, DetectedDependencySource, DetectedProject, DetectionError, InfoOutput,
    build_info_output, dependency_requirement_for_cargo_plan, detect_cargo_plan_requirements,
    detect_single_crate_project,
};
#[allow(
    deprecated,
    reason = "preserve the deprecated public compatibility API"
)]
pub use item::registry_item_content_hash;
pub use item::{
    BuiltInAssetError, BuiltInAssetKind, CargoPlanEntry, CargoPlanSource, CargoPlanSourceKind,
    REGISTRY_ITEM_SCHEMA_URL, REGISTRY_SCHEMA_URL, RegistryAccessibility,
    RegistryAccessibilityBehavior, RegistryError, RegistryFileTarget, RegistryFileTargetKind,
    RegistryItem, RegistryItemFile, RegistryItemKind, RegistryItemStyle, RegistryLeptos,
    RegistryRoot, RegistryRootItem, RegistrySourceKind, RegistryStyleTarget,
    RegistryStyleTargetKind, ResolvedRegistryItem, ResolvedRegistryTargets,
    ResolvedStyleBlockTarget, ResolvedUiTarget, WEB_UI_PRIMITIVES_VERSION,
    load_built_in_registry_item, load_built_in_registry_root, load_registry_item,
    normalize_cargo_plan, parse_registry_item_str, parse_registry_root_str,
    read_built_in_registry_source, resolve_built_in_registry_items, resolve_registry_targets,
    validate_registry_graph, validate_registry_item_name, validate_registry_manifest_identity,
};
pub use registry_health::{
    RegistryHealthError, RegistryHealthFileKind, validate_built_in_registry_health,
};
pub use theme_contract::{
    THEME_CONTRACT_NAME, THEME_CONTRACT_SCHEMA_URL, THEME_CONTRACT_VERSION, ThemeContract,
    ThemeContractError, ThemeToken, ThemeTokenCategory, load_built_in_theme_contract,
    parse_theme_contract_str,
};
