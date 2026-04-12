#![forbid(unsafe_code)]

//! Registry layer for configuration and item resolution.

mod config;
mod detect;
mod item;

pub use config::{
    AliasRoots, ComponentsConfig, ComponentsConfigAliases, ComponentsConfigLeptos,
    ComponentsConfigTailwind, ConfigError, InstallRoots, LegacyStyleAlias, MenuAccent, MenuColor,
    NormalizeOptions, NormalizedProjectConfig, RegistryConfigItem, RenderMode, ResolvedStyleTarget,
    StyleBase, StyleFamily, TailwindVersion, WorkspaceMode, normalize_single_crate_project,
    parse_components_json_str,
};
pub use detect::{
    DetectedProject, DetectedTailwind, DetectionError, InfoOutput, build_info_output,
    detect_single_crate_project,
};
pub use item::{
    RegistryError, RegistryItem, RegistryItemFile, RegistryItemType, RegistrySourceKind,
    ResolvedRegistryItem, load_built_in_registry_item, load_local_registry_item,
    load_registry_item, parse_registry_item_str,
};
