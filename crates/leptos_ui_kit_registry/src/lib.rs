#![forbid(unsafe_code)]

//! Registry layer for configuration and item resolution.

mod config;
mod detect;

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
