use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum ConfigError {
    Parse(serde_json::Error),
    InvalidStyle(String),
    InvalidRenderMode(String),
    InvalidRegistryName(String),
    InvalidRegistryTemplate(String),
    MissingRenderMode,
    PathMustBeRelative(String),
    PathTraversal(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "failed to parse components.json: {error}"),
            Self::InvalidStyle(value) => write!(f, "invalid style value: {value}"),
            Self::InvalidRenderMode(value) => write!(f, "invalid render mode: {value}"),
            Self::InvalidRegistryName(value) => write!(f, "invalid registry name: {value}"),
            Self::InvalidRegistryTemplate(value) => {
                write!(f, "registry template must include {{name}}: {value}")
            }
            Self::MissingRenderMode => {
                write!(f, "render mode is required when detection is absent")
            }
            Self::PathMustBeRelative(value) => write!(f, "path must be relative: {value}"),
            Self::PathTraversal(value) => {
                write!(f, "path must not traverse parent segments: {value}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<serde_json::Error> for ConfigError {
    fn from(value: serde_json::Error) -> Self {
        Self::Parse(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RegistryConfigItem {
    Template(String),
    Remote {
        url: String,
        #[serde(default)]
        params: BTreeMap<String, String>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

impl RegistryConfigItem {
    fn validate(&self) -> Result<(), ConfigError> {
        match self {
            Self::Template(template) => validate_registry_template(template),
            Self::Remote { url, .. } => validate_registry_template(url),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentsConfigTailwind {
    #[serde(default)]
    pub config: String,
    pub css: String,
    pub base_color: String,
    #[serde(default = "default_true")]
    pub css_variables: bool,
    #[serde(default)]
    pub prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentsConfigLeptos {
    pub render_mode: Option<String>,
    pub portal_selector: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentsConfigAliases {
    pub components: String,
    pub utils: String,
    pub ui: Option<String>,
    pub lib: Option<String>,
    pub hooks: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MenuColor {
    Default,
    Inverted,
    DefaultTranslucent,
    InvertedTranslucent,
}

impl Default for MenuColor {
    fn default() -> Self {
        Self::Default
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MenuAccent {
    Subtle,
    Bold,
}

impl Default for MenuAccent {
    fn default() -> Self {
        Self::Subtle
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentsConfig {
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    pub style: String,
    #[serde(default)]
    pub rsc: bool,
    #[serde(default = "default_true")]
    pub tsx: bool,
    pub tailwind: ComponentsConfigTailwind,
    #[serde(default)]
    pub icon_library: Option<String>,
    #[serde(default)]
    pub rtl: bool,
    #[serde(default)]
    pub menu_color: MenuColor,
    #[serde(default)]
    pub menu_accent: MenuAccent,
    pub aliases: ComponentsConfigAliases,
    #[serde(default)]
    pub registries: BTreeMap<String, RegistryConfigItem>,
    #[serde(default)]
    pub leptos: ComponentsConfigLeptos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RenderMode {
    Csr,
    Hydrate,
    Islands,
}

impl RenderMode {
    fn parse(value: &str) -> Result<Self, ConfigError> {
        match value {
            "csr" => Ok(Self::Csr),
            "hydrate" => Ok(Self::Hydrate),
            "islands" => Ok(Self::Islands),
            _ => Err(ConfigError::InvalidRenderMode(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StyleBase {
    Base,
    Radix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StyleFamily {
    Vega,
    Nova,
    Maia,
    Lyra,
    Mira,
    Luma,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LegacyStyleAlias {
    Default,
    NewYork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolvedStyleTarget {
    BaseStyle {
        base: StyleBase,
        family: StyleFamily,
    },
    LegacyNewYorkV4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TailwindVersion {
    V4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkspaceMode {
    SingleCrate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasRoots {
    pub components: PathBuf,
    pub utils: PathBuf,
    pub ui: PathBuf,
    pub lib: PathBuf,
    pub hooks: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRoots {
    pub components: PathBuf,
    pub ui: PathBuf,
    pub lib: PathBuf,
    pub hooks: PathBuf,
    pub styles: PathBuf,
    pub css_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedProjectConfig {
    pub base: StyleBase,
    pub style_family: StyleFamily,
    pub legacy_alias: Option<LegacyStyleAlias>,
    pub resolved_style_target: ResolvedStyleTarget,
    pub tailwind_version: TailwindVersion,
    pub render_mode: RenderMode,
    pub icon_library: String,
    pub base_color: String,
    pub menu_color: MenuColor,
    pub menu_accent: MenuAccent,
    pub rtl: bool,
    pub alias_roots: AliasRoots,
    pub registry_map: BTreeMap<String, RegistryConfigItem>,
    pub workspace_mode: WorkspaceMode,
    pub project_root: PathBuf,
    pub install_roots: InstallRoots,
    pub portal_selector: String,
    pub rsc: bool,
    pub tsx: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizeOptions {
    pub project_root: PathBuf,
    pub source_root: PathBuf,
    pub detected_render_mode: Option<RenderMode>,
    pub tailwind_version: TailwindVersion,
}

pub fn parse_components_json_str(input: &str) -> Result<ComponentsConfig, ConfigError> {
    let config: ComponentsConfig = serde_json::from_str(input)?;

    for (name, registry) in &config.registries {
        if !name.starts_with('@') {
            return Err(ConfigError::InvalidRegistryName(name.clone()));
        }

        registry.validate()?;
    }

    Ok(config)
}

pub fn normalize_single_crate_project(
    config: &ComponentsConfig,
    options: &NormalizeOptions,
) -> Result<NormalizedProjectConfig, ConfigError> {
    let normalized_style = NormalizedStyle::parse(&config.style)?;
    let render_mode = match config.leptos.render_mode.as_deref() {
        Some(mode) => RenderMode::parse(mode)?,
        None => options
            .detected_render_mode
            .ok_or(ConfigError::MissingRenderMode)?,
    };

    let css_file = resolve_project_path(
        &options.project_root,
        &options.source_root,
        &config.tailwind.css,
    )?;
    let components = resolve_alias_root(
        &options.project_root,
        &options.source_root,
        &config.aliases.components,
    )?;
    let utils = resolve_alias_root(
        &options.project_root,
        &options.source_root,
        &config.aliases.utils,
    )?;
    let ui = match config.aliases.ui.as_deref() {
        Some(value) => resolve_alias_root(&options.project_root, &options.source_root, value)?,
        None => components.join("ui"),
    };
    let lib = match config.aliases.lib.as_deref() {
        Some(value) => resolve_alias_root(&options.project_root, &options.source_root, value)?,
        None => utils
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| options.source_root.join("lib")),
    };
    let hooks = match config.aliases.hooks.as_deref() {
        Some(value) => resolve_alias_root(&options.project_root, &options.source_root, value)?,
        None => options.source_root.join("hooks"),
    };

    Ok(NormalizedProjectConfig {
        base: normalized_style.base,
        style_family: normalized_style.family,
        legacy_alias: normalized_style.legacy_alias,
        resolved_style_target: normalized_style.resolved_target,
        tailwind_version: options.tailwind_version,
        render_mode,
        icon_library: config
            .icon_library
            .clone()
            .unwrap_or_else(|| "lucide".to_owned()),
        base_color: config.tailwind.base_color.clone(),
        menu_color: config.menu_color.clone(),
        menu_accent: config.menu_accent.clone(),
        rtl: config.rtl,
        alias_roots: AliasRoots {
            components: components.clone(),
            utils: utils.clone(),
            ui: ui.clone(),
            lib: lib.clone(),
            hooks: hooks.clone(),
        },
        registry_map: config.registries.clone(),
        workspace_mode: WorkspaceMode::SingleCrate,
        project_root: options.project_root.clone(),
        install_roots: InstallRoots {
            components,
            ui,
            lib,
            hooks,
            styles: css_file
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| options.project_root.clone()),
            css_file,
        },
        portal_selector: config
            .leptos
            .portal_selector
            .clone()
            .unwrap_or_else(|| "body".to_owned()),
        rsc: config.rsc,
        tsx: config.tsx,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NormalizedStyle {
    base: StyleBase,
    family: StyleFamily,
    legacy_alias: Option<LegacyStyleAlias>,
    resolved_target: ResolvedStyleTarget,
}

impl NormalizedStyle {
    fn parse(value: &str) -> Result<Self, ConfigError> {
        match value {
            "default" => Ok(Self {
                base: StyleBase::Base,
                family: StyleFamily::Nova,
                legacy_alias: Some(LegacyStyleAlias::Default),
                resolved_target: ResolvedStyleTarget::BaseStyle {
                    base: StyleBase::Base,
                    family: StyleFamily::Nova,
                },
            }),
            "new-york" => Ok(Self {
                base: StyleBase::Radix,
                family: StyleFamily::Nova,
                legacy_alias: Some(LegacyStyleAlias::NewYork),
                resolved_target: ResolvedStyleTarget::LegacyNewYorkV4,
            }),
            _ => {
                let (base, family) = value
                    .split_once('-')
                    .ok_or_else(|| ConfigError::InvalidStyle(value.to_owned()))?;

                let base = match base {
                    "base" => StyleBase::Base,
                    "radix" => StyleBase::Radix,
                    _ => return Err(ConfigError::InvalidStyle(value.to_owned())),
                };

                let family = match family {
                    "vega" => StyleFamily::Vega,
                    "nova" => StyleFamily::Nova,
                    "maia" => StyleFamily::Maia,
                    "lyra" => StyleFamily::Lyra,
                    "mira" => StyleFamily::Mira,
                    "luma" => StyleFamily::Luma,
                    _ => return Err(ConfigError::InvalidStyle(value.to_owned())),
                };

                Ok(Self {
                    base,
                    family,
                    legacy_alias: None,
                    resolved_target: ResolvedStyleTarget::BaseStyle { base, family },
                })
            }
        }
    }
}

fn default_true() -> bool {
    true
}

fn validate_registry_template(value: &str) -> Result<(), ConfigError> {
    if value.contains("{name}") {
        return Ok(());
    }

    Err(ConfigError::InvalidRegistryTemplate(value.to_owned()))
}

fn resolve_alias_root(
    project_root: &Path,
    source_root: &Path,
    value: &str,
) -> Result<PathBuf, ConfigError> {
    if let Some(stripped) = value.strip_prefix("@/") {
        return join_checked(source_root, stripped, value);
    }

    join_checked(project_root, value, value)
}

fn resolve_project_path(
    project_root: &Path,
    source_root: &Path,
    value: &str,
) -> Result<PathBuf, ConfigError> {
    if let Some(stripped) = value.strip_prefix("@/") {
        return join_checked(source_root, stripped, value);
    }

    join_checked(project_root, value, value)
}

fn join_checked(base: &Path, relative: &str, original: &str) -> Result<PathBuf, ConfigError> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(ConfigError::PathMustBeRelative(original.to_owned()));
    }

    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ConfigError::PathTraversal(original.to_owned()));
    }

    Ok(base.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize_options() -> NormalizeOptions {
        NormalizeOptions {
            project_root: PathBuf::from("/workspace/demo"),
            source_root: PathBuf::from("/workspace/demo/src"),
            detected_render_mode: Some(RenderMode::Hydrate),
            tailwind_version: TailwindVersion::V4,
        }
    }

    #[test]
    fn parses_upstream_components_json_shape() {
        let config = parse_components_json_str(
            r#"{
              "$schema": "https://ui.shadcn.com/schema.json",
              "style": "new-york",
              "rsc": true,
              "tsx": true,
              "tailwind": {
                "config": "",
                "css": "app/globals.css",
                "baseColor": "neutral",
                "cssVariables": true,
                "prefix": ""
              },
              "aliases": {
                "components": "@/components",
                "utils": "@/lib/utils",
                "ui": "@/registry/new-york-v4/ui",
                "lib": "@/lib",
                "hooks": "@/hooks"
              },
              "iconLibrary": "lucide"
            }"#,
        )
        .expect("components.json should parse");

        assert_eq!(config.style, "new-york");
        assert_eq!(config.tailwind.base_color, "neutral");
        assert_eq!(config.aliases.components, "@/components");
        assert_eq!(config.icon_library.as_deref(), Some("lucide"));
    }

    #[test]
    fn parses_leptos_extension_block() {
        let config = parse_components_json_str(
            r##"{
              "style": "base-nova",
              "tailwind": {
                "css": "src/styles/app.css",
                "baseColor": "stone"
              },
              "aliases": {
                "components": "src/components",
                "utils": "src/lib/utils"
              },
              "leptos": {
                "renderMode": "csr",
                "portalSelector": "#portal-root"
              }
            }"##,
        )
        .expect("components.json should parse");

        assert_eq!(config.leptos.render_mode.as_deref(), Some("csr"));
        assert_eq!(
            config.leptos.portal_selector.as_deref(),
            Some("#portal-root")
        );
    }

    #[test]
    fn rejects_invalid_registry_names_and_templates() {
        let error = parse_components_json_str(
            r#"{
              "style": "base-nova",
              "tailwind": {
                "css": "src/styles/app.css",
                "baseColor": "stone"
              },
              "aliases": {
                "components": "src/components",
                "utils": "src/lib/utils"
              },
              "registries": {
                "bad": "https://example.com/item.json"
              }
            }"#,
        )
        .expect_err("registry config should be rejected");

        assert!(matches!(error, ConfigError::InvalidRegistryName(_)));
    }

    #[test]
    fn normalizes_new_york_single_crate_project() {
        let config = parse_components_json_str(
            r#"{
              "style": "new-york",
              "tailwind": {
                "css": "src/styles/app.css",
                "baseColor": "neutral"
              },
              "aliases": {
                "components": "src/components",
                "utils": "src/lib/utils"
              }
            }"#,
        )
        .expect("components.json should parse");

        let normalized =
            normalize_single_crate_project(&config, &normalize_options()).expect("normalize");

        assert_eq!(normalized.base, StyleBase::Radix);
        assert_eq!(normalized.style_family, StyleFamily::Nova);
        assert_eq!(normalized.legacy_alias, Some(LegacyStyleAlias::NewYork));
        assert_eq!(
            normalized.resolved_style_target,
            ResolvedStyleTarget::LegacyNewYorkV4
        );
        assert_eq!(normalized.render_mode, RenderMode::Hydrate);
        assert_eq!(
            normalized.alias_roots.components,
            PathBuf::from("/workspace/demo/src/components")
        );
        assert_eq!(
            normalized.alias_roots.ui,
            PathBuf::from("/workspace/demo/src/components/ui")
        );
        assert_eq!(
            normalized.alias_roots.lib,
            PathBuf::from("/workspace/demo/src/lib")
        );
        assert_eq!(
            normalized.install_roots.css_file,
            PathBuf::from("/workspace/demo/src/styles/app.css")
        );
        assert_eq!(normalized.portal_selector, "body");
        assert_eq!(normalized.icon_library, "lucide");
    }

    #[test]
    fn explicit_leptos_render_mode_overrides_detected_mode() {
        let config = parse_components_json_str(
            r#"{
              "style": "base-vega",
              "tailwind": {
                "css": "@/styles/app.css",
                "baseColor": "slate"
              },
              "aliases": {
                "components": "@/components",
                "utils": "@/lib/utils",
                "hooks": "@/hooks"
              },
              "leptos": {
                "renderMode": "islands"
              }
            }"#,
        )
        .expect("components.json should parse");

        let normalized =
            normalize_single_crate_project(&config, &normalize_options()).expect("normalize");

        assert_eq!(normalized.base, StyleBase::Base);
        assert_eq!(normalized.style_family, StyleFamily::Vega);
        assert_eq!(normalized.legacy_alias, None);
        assert_eq!(normalized.render_mode, RenderMode::Islands);
        assert_eq!(
            normalized.alias_roots.components,
            PathBuf::from("/workspace/demo/src/components")
        );
        assert_eq!(
            normalized.alias_roots.hooks,
            PathBuf::from("/workspace/demo/src/hooks")
        );
        assert_eq!(
            normalized.install_roots.styles,
            PathBuf::from("/workspace/demo/src/styles")
        );
    }

    #[test]
    fn missing_render_mode_fails_without_detection() {
        let config = parse_components_json_str(
            r#"{
              "style": "base-maia",
              "tailwind": {
                "css": "src/styles/app.css",
                "baseColor": "zinc"
              },
              "aliases": {
                "components": "src/components",
                "utils": "src/lib/utils"
              }
            }"#,
        )
        .expect("components.json should parse");

        let error = normalize_single_crate_project(
            &config,
            &NormalizeOptions {
                detected_render_mode: None,
                ..normalize_options()
            },
        )
        .expect_err("normalize should fail");

        assert!(matches!(error, ConfigError::MissingRenderMode));
    }

    #[test]
    fn rejects_parent_traversal_in_project_paths() {
        let config = parse_components_json_str(
            r#"{
              "style": "base-lyra",
              "tailwind": {
                "css": "../styles/app.css",
                "baseColor": "gray"
              },
              "aliases": {
                "components": "src/components",
                "utils": "src/lib/utils"
              },
              "leptos": {
                "renderMode": "csr"
              }
            }"#,
        )
        .expect("components.json should parse");

        let error =
            normalize_single_crate_project(&config, &normalize_options()).expect_err("should fail");

        assert!(matches!(error, ConfigError::PathTraversal(_)));
    }
}
