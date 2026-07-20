use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: &str = "0.9.0-alpha";
pub const KIT_SCHEMA_URL: &str =
    "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/kit.schema.json";
pub const LEPTOS_VERSION: &str = "0.9.0-alpha";
pub const LEPTOS_ROUTER_VERSION: &str = "0.9.0-alpha";
pub const TOOL_PACKAGE: &str = "leptos_ui_kit_cli";
pub const TOOL_BINARY: &str = "leptos_ui_kit";
pub const TOOL_GIT_URL: &str = "https://github.com/triesap/leptos_ui_kit";
pub const DEFAULT_UI_DIR: &str = "src/components/ui";
pub const DEFAULT_KIT_DIR: &str = "src/components/ui/_kit";
pub const DEFAULT_KIT_CONFIG_PATH: &str = "src/components/ui/_kit/kit.json";
pub const DEFAULT_CSS_PATH: &str = "styles/kit.css";

#[derive(Debug)]
pub enum ConfigError {
    Parse(serde_json::Error),
    Serialize(serde_json::Error),
    InvalidValue {
        field: &'static str,
        expected: &'static str,
        actual: String,
    },
    PathMustBeRelative {
        field: &'static str,
        value: String,
    },
    PathTraversal {
        field: &'static str,
        value: String,
    },
    UnsafePathSegment {
        field: &'static str,
        value: String,
    },
    PathOverlap {
        field: &'static str,
        value: String,
    },
    MissingToolProvenance {
        package: &'static str,
        binary: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "failed to parse kit.json: {error}"),
            Self::Serialize(error) => write!(f, "failed to serialize kit.json: {error}"),
            Self::InvalidValue {
                field,
                expected,
                actual,
            } => write!(
                f,
                "invalid kit.json value for {field}: expected {expected}, got {actual}"
            ),
            Self::PathMustBeRelative { field, value } => {
                write!(f, "kit.json path {field} must be relative: {value}")
            }
            Self::PathTraversal { field, value } => {
                write!(
                    f,
                    "kit.json path {field} must not traverse parent segments: {value}"
                )
            }
            Self::UnsafePathSegment { field, value } => {
                write!(
                    f,
                    "kit.json path {field} contains an unsafe segment: {value}"
                )
            }
            Self::PathOverlap { field, value } => {
                write!(
                    f,
                    "kit.json path {field} overlaps a reserved target: {value}"
                )
            }
            Self::MissingToolProvenance { package, binary } => {
                write!(
                    f,
                    "missing compiled tool provenance for {package}/{binary}; cannot write kit.json without a proven Git revision, so rebuild from canonical package metadata or set LEPTOS_UI_KIT_GIT_REV while building"
                )
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KitConfig {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: String,
    pub tool: ToolConfig,
    pub project: ProjectConfig,
    pub leptos: LeptosConfig,
    pub install: InstallConfig,
    pub styles: StylesConfig,
    pub registry: RegistryConfig,
    pub items: Vec<DesiredItemConfig>,
}

impl KitConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        expect_string("$schema", KIT_SCHEMA_URL, &self.schema)?;
        expect_string("schemaVersion", SCHEMA_VERSION, &self.schema_version)?;
        self.tool.validate()?;
        self.project.validate()?;
        self.leptos.validate()?;
        validate_render_mode_contract(self.project.kind, self.leptos.render_mode)?;
        self.install.validate()?;
        self.styles.validate()?;
        validate_distinct_file_targets(self)?;
        self.registry.validate()?;
        validate_desired_items(&self.items)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolConfig {
    pub package: String,
    pub binary: String,
    pub source: ToolSourceConfig,
}

impl ToolConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        expect_string("tool.package", TOOL_PACKAGE, &self.package)?;
        expect_string("tool.binary", TOOL_BINARY, &self.binary)?;
        self.source.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ToolSourceConfig {
    Git { url: String, rev: String },
}

impl ToolSourceConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        match self {
            Self::Git { url, rev } => {
                expect_string("tool.source.url", TOOL_GIT_URL, url)?;
                validate_git_rev("tool.source.rev", rev)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectConfig {
    pub kind: ProjectKind,
    pub crate_root: String,
    pub src_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_html: Option<String>,
}

impl ProjectConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        expect_path("project.crateRoot", ".", &self.crate_root)?;
        expect_path("project.srcDir", "src", &self.src_dir)?;
        match (self.kind, self.index_html.as_deref()) {
            (ProjectKind::SingleCrateTrunkCsr, Some(index_html)) => {
                expect_path("project.indexHtml", "index.html", index_html)
            }
            (ProjectKind::SingleCrateTrunkCsr, None) => Err(ConfigError::InvalidValue {
                field: "project.indexHtml",
                expected: "index.html for a single-crate-trunk-csr project",
                actual: "missing".to_owned(),
            }),
            (
                ProjectKind::SingleCrateNativeSsr
                | ProjectKind::SingleCrateBrowserHydration
                | ProjectKind::SharedLibraryCrate,
                None,
            ) => Ok(()),
            (
                ProjectKind::SingleCrateNativeSsr
                | ProjectKind::SingleCrateBrowserHydration
                | ProjectKind::SharedLibraryCrate,
                Some(index_html),
            ) => Err(ConfigError::InvalidValue {
                field: "project.indexHtml",
                expected: "absent unless project.kind is single-crate-trunk-csr",
                actual: index_html.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectKind {
    SingleCrateTrunkCsr,
    SingleCrateNativeSsr,
    SingleCrateBrowserHydration,
    SharedLibraryCrate,
}

impl ProjectKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleCrateTrunkCsr => "single-crate-trunk-csr",
            Self::SingleCrateNativeSsr => "single-crate-native-ssr",
            Self::SingleCrateBrowserHydration => "single-crate-browser-hydration",
            Self::SharedLibraryCrate => "shared-library-crate",
        }
    }

    pub const fn render_mode_contract(self) -> RenderModeContract {
        match self {
            Self::SingleCrateTrunkCsr => RenderModeContract::Selected(RenderMode::Csr),
            Self::SingleCrateNativeSsr => RenderModeContract::Selected(RenderMode::Ssr),
            Self::SingleCrateBrowserHydration => RenderModeContract::Selected(RenderMode::Hydrate),
            Self::SharedLibraryCrate => RenderModeContract::Neutral,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeptosConfig {
    pub version: String,
    pub router_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_mode: Option<RenderMode>,
}

impl LeptosConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        expect_string("leptos.version", LEPTOS_VERSION, &self.version)?;
        expect_string(
            "leptos.routerVersion",
            LEPTOS_ROUTER_VERSION,
            &self.router_version,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RenderMode {
    Csr,
    Hydrate,
    Ssr,
}

impl RenderMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Csr => "csr",
            Self::Hydrate => "hydrate",
            Self::Ssr => "ssr",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "mode", rename_all = "kebab-case")]
pub enum RenderModeContract {
    Neutral,
    Selected(RenderMode),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallConfig {
    pub ui_dir: String,
    pub ui_mod: String,
    pub components_mod: String,
}

impl InstallConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_safe_rust_module_dir("install.uiDir", &self.ui_dir, 3)?;
        validate_safe_relative_path("install.uiMod", &self.ui_mod)?;
        let expected_ui_mod = format!("{}/mod.rs", self.ui_dir);
        if self.ui_mod != expected_ui_mod {
            return Err(ConfigError::InvalidValue {
                field: "install.uiMod",
                expected: "install.uiDir followed by /mod.rs",
                actual: self.ui_mod.clone(),
            });
        }

        validate_safe_relative_path("install.componentsMod", &self.components_mod)?;
        let Some(components_root) = self.components_mod.strip_suffix("/mod.rs") else {
            return Err(ConfigError::InvalidValue {
                field: "install.componentsMod",
                expected: "a components-root path under src/ followed by /mod.rs",
                actual: self.components_mod.clone(),
            });
        };
        validate_safe_rust_module_dir("install.componentsMod", components_root, 2)?;

        let Some((ui_parent, _)) = self.ui_dir.rsplit_once('/') else {
            return Err(ConfigError::InvalidValue {
                field: "install.uiDir",
                expected: "a direct child of the configured components root",
                actual: self.ui_dir.clone(),
            });
        };
        if ui_parent != components_root {
            return Err(ConfigError::InvalidValue {
                field: "install.uiDir",
                expected: "a direct child of the configured components root",
                actual: self.ui_dir.clone(),
            });
        }

        let folded_ui_dir = self.ui_dir.to_ascii_lowercase();
        let folded_kit_dir = DEFAULT_KIT_DIR.to_ascii_lowercase();
        if folded_ui_dir == folded_kit_dir
            || folded_ui_dir
                .strip_prefix(&folded_kit_dir)
                .is_some_and(|suffix| suffix.starts_with('/'))
        {
            return Err(ConfigError::PathOverlap {
                field: "install.uiDir",
                value: self.ui_dir.clone(),
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StylesConfig {
    pub mode: StylesMode,
    pub css: String,
}

impl StylesConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_safe_relative_css_file("styles.css", &self.css)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StylesMode {
    PureCss,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryConfig {
    pub source: RegistrySource,
}

impl RegistryConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        match self.source {
            RegistrySource::Builtin => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistrySource {
    Builtin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesiredItemConfig {
    pub name: DesiredItemName,
    pub source: RegistrySource,
}

impl DesiredItemConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        match (self.name, self.source) {
            (DesiredItemName::Anchor, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Button, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Collapsible, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Dialog, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Field, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Menu, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::RouterLink, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Spinner, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Status, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Tabs, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Tokens, RegistrySource::Builtin) => Ok(()),
        }
    }

    pub fn item_name(&self) -> &'static str {
        self.name.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesiredItemName {
    Anchor,
    Button,
    Collapsible,
    Dialog,
    Field,
    Menu,
    RouterLink,
    Spinner,
    Status,
    Tabs,
    Tokens,
}

impl DesiredItemName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anchor => "anchor",
            Self::Button => "button",
            Self::Collapsible => "collapsible",
            Self::Dialog => "dialog",
            Self::Field => "field",
            Self::Menu => "menu",
            Self::RouterLink => "router-link",
            Self::Spinner => "spinner",
            Self::Status => "status",
            Self::Tabs => "tabs",
            Self::Tokens => "tokens",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkspaceMode {
    SingleCrate,
    SinglePackageWorkspaceRoot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRoots {
    pub ui_dir: PathBuf,
    pub ui_mod: PathBuf,
    pub components_mod: PathBuf,
    pub css_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedProjectConfig {
    pub schema_version: String,
    pub project_kind: ProjectKind,
    pub render_mode: Option<RenderMode>,
    pub desired_items: Vec<DesiredItemConfig>,
    pub workspace_mode: WorkspaceMode,
    pub project_root: PathBuf,
    pub crate_root: PathBuf,
    pub source_root: PathBuf,
    pub index_html: Option<PathBuf>,
    pub install_roots: InstallRoots,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizeOptions {
    pub project_root: PathBuf,
}

pub fn canonical_tool_config() -> Result<ToolConfig, ConfigError> {
    canonical_tool_config_from_revision(option_env!("LEPTOS_UI_KIT_GIT_REV"))
}

fn canonical_tool_config_from_revision(rev: Option<&str>) -> Result<ToolConfig, ConfigError> {
    let Some(rev) = rev else {
        return Err(ConfigError::MissingToolProvenance {
            package: TOOL_PACKAGE,
            binary: TOOL_BINARY,
        });
    };

    let tool = ToolConfig {
        package: TOOL_PACKAGE.to_owned(),
        binary: TOOL_BINARY.to_owned(),
        source: ToolSourceConfig::Git {
            url: TOOL_GIT_URL.to_owned(),
            rev: rev.to_ascii_lowercase(),
        },
    };
    tool.validate()?;
    Ok(tool)
}

pub fn canonical_kit_config() -> Result<KitConfig, ConfigError> {
    canonical_kit_config_from_tool(canonical_tool_config()?)
}

fn canonical_kit_config_from_tool(tool: ToolConfig) -> Result<KitConfig, ConfigError> {
    tool.validate()?;
    let config = KitConfig {
        schema: KIT_SCHEMA_URL.to_owned(),
        schema_version: SCHEMA_VERSION.to_owned(),
        tool,
        project: ProjectConfig {
            kind: ProjectKind::SingleCrateTrunkCsr,
            crate_root: ".".to_owned(),
            src_dir: "src".to_owned(),
            index_html: Some("index.html".to_owned()),
        },
        leptos: LeptosConfig {
            version: LEPTOS_VERSION.to_owned(),
            router_version: LEPTOS_ROUTER_VERSION.to_owned(),
            render_mode: Some(RenderMode::Csr),
        },
        install: InstallConfig {
            ui_dir: DEFAULT_UI_DIR.to_owned(),
            ui_mod: "src/components/ui/mod.rs".to_owned(),
            components_mod: "src/components/mod.rs".to_owned(),
        },
        styles: StylesConfig {
            mode: StylesMode::PureCss,
            css: DEFAULT_CSS_PATH.to_owned(),
        },
        registry: RegistryConfig {
            source: RegistrySource::Builtin,
        },
        items: Vec::new(),
    };
    config.validate()?;
    Ok(config)
}

fn validate_render_mode_contract(
    project_kind: ProjectKind,
    render_mode: Option<RenderMode>,
) -> Result<(), ConfigError> {
    let expected = project_kind.render_mode_contract();
    let valid = matches!(
        (expected, render_mode),
        (RenderModeContract::Neutral, None)
            | (
                RenderModeContract::Selected(RenderMode::Csr),
                Some(RenderMode::Csr)
            )
            | (
                RenderModeContract::Selected(RenderMode::Hydrate),
                Some(RenderMode::Hydrate)
            )
            | (
                RenderModeContract::Selected(RenderMode::Ssr),
                Some(RenderMode::Ssr)
            )
    );
    if valid {
        return Ok(());
    }

    Err(ConfigError::InvalidValue {
        field: "leptos.renderMode",
        expected: match expected {
            RenderModeContract::Neutral => "absent for a shared-library-crate project",
            RenderModeContract::Selected(RenderMode::Csr) => {
                "csr for a single-crate-trunk-csr project"
            }
            RenderModeContract::Selected(RenderMode::Hydrate) => {
                "hydrate for a single-crate-browser-hydration project"
            }
            RenderModeContract::Selected(RenderMode::Ssr) => {
                "ssr for a single-crate-native-ssr project"
            }
        },
        actual: render_mode
            .map(RenderMode::as_str)
            .unwrap_or("missing")
            .to_owned(),
    })
}

/// Replaces a configuration's tool source with this binary's proven Git
/// provenance immediately before the configuration is persisted.
///
/// Local builds without approved compiled provenance remain usable for
/// read-only commands, but cannot claim an invented or stale tool revision in
/// `kit.json`.
pub fn kit_config_for_write(config: KitConfig) -> Result<KitConfig, ConfigError> {
    kit_config_for_write_with_tool(config, canonical_tool_config())
}

fn kit_config_for_write_with_tool(
    mut config: KitConfig,
    tool: Result<ToolConfig, ConfigError>,
) -> Result<KitConfig, ConfigError> {
    config.validate()?;
    config.tool = tool?;
    config.validate()?;
    Ok(config)
}

pub fn kit_config_with_desired_item(
    mut config: KitConfig,
    item: DesiredItemConfig,
) -> Result<KitConfig, ConfigError> {
    if !config
        .items
        .iter()
        .any(|existing| existing.name == item.name)
    {
        config.items.push(item);
    }
    config.validate()?;
    Ok(config)
}

pub fn desired_builtin_anchor_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Anchor,
        source: RegistrySource::Builtin,
    }
}

pub fn desired_builtin_button_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Button,
        source: RegistrySource::Builtin,
    }
}

pub fn desired_builtin_collapsible_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Collapsible,
        source: RegistrySource::Builtin,
    }
}

pub fn desired_builtin_dialog_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Dialog,
        source: RegistrySource::Builtin,
    }
}

pub fn desired_builtin_field_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Field,
        source: RegistrySource::Builtin,
    }
}

pub fn desired_builtin_menu_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Menu,
        source: RegistrySource::Builtin,
    }
}

pub fn desired_builtin_router_link_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::RouterLink,
        source: RegistrySource::Builtin,
    }
}

pub fn desired_builtin_spinner_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Spinner,
        source: RegistrySource::Builtin,
    }
}

pub fn desired_builtin_status_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Status,
        source: RegistrySource::Builtin,
    }
}

pub fn desired_builtin_tabs_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Tabs,
        source: RegistrySource::Builtin,
    }
}

pub fn desired_builtin_tokens_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Tokens,
        source: RegistrySource::Builtin,
    }
}

fn validate_desired_items(items: &[DesiredItemConfig]) -> Result<(), ConfigError> {
    let mut names = BTreeSet::new();
    for item in items {
        item.validate()?;
        if !names.insert(item.name) {
            return Err(ConfigError::InvalidValue {
                field: "items",
                expected: "unique desired item names",
                actual: format!("{:?}", item.name),
            });
        }
    }
    Ok(())
}

fn validate_git_rev(field: &'static str, rev: &str) -> Result<(), ConfigError> {
    if rev.len() == 40
        && rev
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(ConfigError::InvalidValue {
            field,
            expected: "40-character git commit hash",
            actual: rev.to_owned(),
        })
    }
}

pub fn canonical_kit_json() -> Result<String, ConfigError> {
    kit_config_to_json(&canonical_kit_config()?)
}

pub fn kit_config_to_json(config: &KitConfig) -> Result<String, ConfigError> {
    config.validate()?;
    let mut output = serde_json::to_string_pretty(config).map_err(ConfigError::Serialize)?;
    output.push('\n');
    Ok(output)
}

pub fn parse_kit_json_str(input: &str) -> Result<KitConfig, ConfigError> {
    let config: KitConfig = serde_json::from_str(input)?;
    config.validate()?;
    Ok(config)
}

pub fn normalize_project(
    config: &KitConfig,
    options: &NormalizeOptions,
) -> Result<NormalizedProjectConfig, ConfigError> {
    normalize_project_with_workspace_mode(config, options, WorkspaceMode::SingleCrate)
}

pub fn normalize_project_with_workspace_mode(
    config: &KitConfig,
    options: &NormalizeOptions,
    workspace_mode: WorkspaceMode,
) -> Result<NormalizedProjectConfig, ConfigError> {
    config.validate()?;

    let project_root = options.project_root.clone();
    let crate_root = join_checked(
        &project_root,
        &config.project.crate_root,
        "project.crateRoot",
    )?;
    let source_root = join_checked(&project_root, &config.project.src_dir, "project.srcDir")?;
    let index_html = config
        .project
        .index_html
        .as_deref()
        .map(|path| join_checked(&project_root, path, "project.indexHtml"))
        .transpose()?;

    Ok(NormalizedProjectConfig {
        schema_version: config.schema_version.clone(),
        project_kind: config.project.kind,
        render_mode: config.leptos.render_mode,
        desired_items: config.items.clone(),
        workspace_mode,
        project_root: project_root.clone(),
        crate_root,
        source_root,
        index_html,
        install_roots: InstallRoots {
            ui_dir: join_checked(&project_root, &config.install.ui_dir, "install.uiDir")?,
            ui_mod: join_checked(&project_root, &config.install.ui_mod, "install.uiMod")?,
            components_mod: join_checked(
                &project_root,
                &config.install.components_mod,
                "install.componentsMod",
            )?,
            css_file: join_checked(&project_root, &config.styles.css, "styles.css")?,
        },
    })
}

pub fn normalize_single_crate_project(
    config: &KitConfig,
    options: &NormalizeOptions,
) -> Result<NormalizedProjectConfig, ConfigError> {
    normalize_project(config, options)
}

fn expect_string(
    field: &'static str,
    expected: &'static str,
    actual: &str,
) -> Result<(), ConfigError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ConfigError::InvalidValue {
            field,
            expected,
            actual: actual.to_owned(),
        })
    }
}

fn expect_path(
    field: &'static str,
    expected: &'static str,
    actual: &str,
) -> Result<(), ConfigError> {
    validate_relative_path(field, actual)?;
    expect_string(field, expected, actual)
}

fn validate_relative_path(field: &'static str, value: &str) -> Result<(), ConfigError> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(ConfigError::PathMustBeRelative {
            field,
            value: value.to_owned(),
        });
    }

    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ConfigError::PathTraversal {
            field,
            value: value.to_owned(),
        });
    }

    Ok(())
}

fn validate_safe_relative_css_file(field: &'static str, value: &str) -> Result<(), ConfigError> {
    validate_safe_relative_path(field, value)?;
    if !value.starts_with("styles/") || !value.ends_with(".css") || value == "styles/.css" {
        return Err(ConfigError::InvalidValue {
            field,
            expected: "safe relative .css file under styles/",
            actual: value.to_owned(),
        });
    }
    if value
        .split('/')
        .skip(1)
        .any(|segment| segment.starts_with('.'))
    {
        return Err(ConfigError::UnsafePathSegment {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn validate_safe_relative_path(field: &'static str, value: &str) -> Result<(), ConfigError> {
    validate_relative_path(field, value)?;
    if value.is_empty() || value.contains('\\') {
        return Err(ConfigError::UnsafePathSegment {
            field,
            value: value.to_owned(),
        });
    }

    for segment in value.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(ConfigError::UnsafePathSegment {
                field,
                value: value.to_owned(),
            });
        }
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return Err(ConfigError::UnsafePathSegment {
                field,
                value: value.to_owned(),
            });
        }
    }

    Ok(())
}

fn validate_safe_rust_module_dir(
    field: &'static str,
    value: &str,
    minimum_segments: usize,
) -> Result<(), ConfigError> {
    validate_safe_relative_path(field, value)?;
    let segments = value.split('/').collect::<Vec<_>>();
    if segments.len() < minimum_segments || segments.first() != Some(&"src") {
        return Err(ConfigError::InvalidValue {
            field,
            expected: "a safe Rust module directory under src/",
            actual: value.to_owned(),
        });
    }
    for segment in segments.iter().skip(1) {
        if !is_rust_module_identifier(segment) || *segment == "_kit" {
            return Err(ConfigError::UnsafePathSegment {
                field,
                value: value.to_owned(),
            });
        }
    }
    Ok(())
}

fn is_rust_module_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return false;
    }
    !crate::item::is_rust_2024_keyword(value)
}

fn validate_distinct_file_targets(config: &KitConfig) -> Result<(), ConfigError> {
    const KIT_LOCK_PATH: &str = "src/components/ui/_kit/kit.lock.json";
    let targets = [
        ("install.uiMod", Some(config.install.ui_mod.as_str())),
        (
            "install.componentsMod",
            Some(config.install.components_mod.as_str()),
        ),
        ("styles.css", Some(config.styles.css.as_str())),
        ("project.indexHtml", config.project.index_html.as_deref()),
        ("kit.config", Some(DEFAULT_KIT_CONFIG_PATH)),
        ("kit.lock", Some(KIT_LOCK_PATH)),
    ];
    let mut seen = BTreeSet::new();
    for (field, value) in targets
        .into_iter()
        .filter_map(|(field, value)| value.map(|value| (field, value)))
    {
        if !seen.insert(value.to_ascii_lowercase()) {
            return Err(ConfigError::PathOverlap {
                field,
                value: value.to_owned(),
            });
        }
    }
    Ok(())
}

fn join_checked(
    project_root: &Path,
    relative: &str,
    field: &'static str,
) -> Result<PathBuf, ConfigError> {
    validate_relative_path(field, relative)?;
    Ok(project_root.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_REV_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const TEST_REV_B: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn tool_config(rev: &str) -> ToolConfig {
        canonical_tool_config_from_revision(Some(rev)).expect("known test provenance")
    }

    fn valid_config_json() -> String {
        let config =
            canonical_kit_config_from_tool(tool_config(TEST_REV_A)).expect("canonical test config");
        kit_config_to_json(&config).expect("serialize config")
    }

    #[test]
    fn canonical_tool_config_distinguishes_known_invalid_and_unavailable_revisions() {
        let known = canonical_tool_config_from_revision(Some(&TEST_REV_A.to_ascii_uppercase()))
            .expect("known provenance");
        let ToolSourceConfig::Git { rev, .. } = known.source;
        assert_eq!(rev, TEST_REV_A);

        assert!(matches!(
            canonical_tool_config_from_revision(None),
            Err(ConfigError::MissingToolProvenance {
                package: TOOL_PACKAGE,
                binary: TOOL_BINARY,
            })
        ));
        assert!(matches!(
            canonical_tool_config_from_revision(Some("short")),
            Err(ConfigError::InvalidValue {
                field: "tool.source.rev",
                ..
            })
        ));
    }

    #[test]
    fn config_write_boundary_stamps_known_provenance_and_rejects_unavailable_builds() {
        let config = parse_kit_json_str(&valid_config_json()).expect("parse test config");
        let stamped = kit_config_for_write_with_tool(config.clone(), Ok(tool_config(TEST_REV_B)))
            .expect("stamp write provenance");
        let ToolSourceConfig::Git { rev, .. } = stamped.tool.source;
        assert_eq!(rev, TEST_REV_B);

        let error = kit_config_for_write_with_tool(
            config,
            Err(ConfigError::MissingToolProvenance {
                package: TOOL_PACKAGE,
                binary: TOOL_BINARY,
            }),
        )
        .expect_err("unavailable provenance must disable config writes");
        assert!(matches!(
            error,
            ConfigError::MissingToolProvenance {
                package: TOOL_PACKAGE,
                binary: TOOL_BINARY,
            }
        ));
        assert!(error.to_string().contains("cannot write kit.json"));
    }

    #[test]
    fn parses_canonical_kit_json() {
        let config = parse_kit_json_str(&valid_config_json()).expect("parse config");

        assert_eq!(config.schema_version, SCHEMA_VERSION);
        assert_eq!(config.tool.package, TOOL_PACKAGE);
        assert_eq!(config.tool.binary, TOOL_BINARY);
        assert!(matches!(config.tool.source, ToolSourceConfig::Git { .. }));
        assert_eq!(config.leptos.version, LEPTOS_VERSION);
        assert_eq!(config.leptos.router_version, LEPTOS_ROUTER_VERSION);
        assert_eq!(config.leptos.render_mode, Some(RenderMode::Csr));
        assert_eq!(config.styles.mode, StylesMode::PureCss);
        assert_eq!(config.registry.source, RegistrySource::Builtin);
        assert!(config.items.is_empty());
    }

    #[test]
    fn canonical_json_is_deterministic() {
        let first = valid_config_json();
        let second = valid_config_json();

        assert_eq!(first, second);
        assert!(first.ends_with('\n'));
        assert!(first.contains("\"schemaVersion\": \"0.9.0-alpha\""));
        assert!(first.contains("\"package\": \"leptos_ui_kit_cli\""));
        assert!(first.contains("\"binary\": \"leptos_ui_kit\""));
        assert!(first.contains("\"items\": []"));
        assert!(!first.contains("classPrefix"));
        assert!(!first.contains("cssVariablePrefix"));
    }

    #[test]
    fn records_desired_builtin_button_item() {
        let config = parse_kit_json_str(&valid_config_json()).expect("parse config");
        let config =
            kit_config_with_desired_item(config, desired_builtin_button_item()).expect("add item");

        assert_eq!(config.items.len(), 1);
        assert_eq!(config.items[0].name, DesiredItemName::Button);
        assert_eq!(config.items[0].source, RegistrySource::Builtin);
    }

    #[test]
    fn records_desired_builtin_tokens_item() {
        let config = parse_kit_json_str(&valid_config_json()).expect("parse config");
        let config =
            kit_config_with_desired_item(config, desired_builtin_tokens_item()).expect("add item");

        assert_eq!(config.items.len(), 1);
        assert_eq!(config.items[0].name, DesiredItemName::Tokens);
        assert_eq!(config.items[0].source, RegistrySource::Builtin);
    }

    #[test]
    fn public_schema_matches_desired_item_vocabulary() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/0.9.0-alpha/kit.schema.json");
        let schema = serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(path).expect("read schema"),
        )
        .expect("parse schema");
        let names = schema["properties"]["items"]["items"]["properties"]["name"]["enum"]
            .as_array()
            .expect("desired item enum")
            .iter()
            .map(|value| value.as_str().expect("desired item name"))
            .collect::<BTreeSet<_>>();
        let expected = [
            DesiredItemName::Anchor,
            DesiredItemName::Button,
            DesiredItemName::Collapsible,
            DesiredItemName::Dialog,
            DesiredItemName::Field,
            DesiredItemName::Menu,
            DesiredItemName::RouterLink,
            DesiredItemName::Spinner,
            DesiredItemName::Status,
            DesiredItemName::Tabs,
            DesiredItemName::Tokens,
        ]
        .into_iter()
        .map(DesiredItemName::as_str)
        .collect::<BTreeSet<_>>();

        assert_eq!(names, expected);
    }

    #[test]
    fn desired_item_add_is_idempotent() {
        let config = parse_kit_json_str(&valid_config_json()).expect("parse config");
        let config =
            kit_config_with_desired_item(config, desired_builtin_button_item()).expect("add item");
        let config =
            kit_config_with_desired_item(config, desired_builtin_button_item()).expect("add item");

        assert_eq!(config.items.len(), 1);
    }

    #[test]
    fn rejects_invalid_tool_rev() {
        let input = valid_config_json().replace("\"rev\": \"", "\"rev\": \"not-a-rev");

        let error = parse_kit_json_str(&input).expect_err("rev should fail");

        assert!(
            matches!(error, ConfigError::InvalidValue { field, .. } if field == "tool.source.rev")
        );
    }

    #[test]
    fn rejects_invalid_tool_url() {
        let input = valid_config_json().replace(
            "\"url\": \"https://github.com/triesap/leptos_ui_kit\"",
            "\"url\": \"https://example.com/leptos_ui_kit\"",
        );

        let error = parse_kit_json_str(&input).expect_err("url should fail");

        assert!(
            matches!(error, ConfigError::InvalidValue { field, .. } if field == "tool.source.url")
        );
    }

    #[test]
    fn rejects_null_missing_cross_kind_and_unknown_tool_source_fields() {
        for source in [
            serde_json::json!({"kind": "git", "url": null, "rev": TEST_REV_A}),
            serde_json::json!({"kind": "git", "url": TOOL_GIT_URL, "rev": null}),
            serde_json::json!({"kind": "git", "url": TOOL_GIT_URL}),
            serde_json::json!({"kind": "git", "url": TOOL_GIT_URL, "rev": TEST_REV_A, "version": "1"}),
            serde_json::json!({"kind": "git", "url": TOOL_GIT_URL, "rev": TEST_REV_A, "branch": "main"}),
        ] {
            let mut config: serde_json::Value =
                serde_json::from_str(&valid_config_json()).expect("parse fixture");
            config["tool"]["source"] = source.clone();
            let input = serde_json::to_string(&config).expect("serialize fixture");
            assert!(parse_kit_json_str(&input).is_err(), "{source}");
        }
    }

    #[test]
    fn rejects_invalid_desired_item() {
        let input = valid_config_json().replace(
            "\"items\": []",
            r#""items": [{"name":"card","source":"builtin"}]"#,
        );

        let error = parse_kit_json_str(&input).expect_err("item should fail");

        assert!(matches!(error, ConfigError::Parse(_)));
    }

    #[test]
    fn rejects_duplicate_desired_items() {
        let input = valid_config_json().replace(
            "\"items\": []",
            r#""items": [{"name":"button","source":"builtin"},{"name":"button","source":"builtin"}]"#,
        );

        let error = parse_kit_json_str(&input).expect_err("duplicate should fail");

        assert!(matches!(error, ConfigError::InvalidValue { field, .. } if field == "items"));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let input = valid_config_json().replace(
            "\"items\": []",
            "\"items\": [],\n  \"tailwind\": { \"css\": \"input.css\" }",
        );

        let error = parse_kit_json_str(&input).expect_err("unknown field should fail");

        assert!(matches!(error, ConfigError::Parse(_)));
    }

    #[test]
    fn rejects_forbidden_legacy_fields() {
        for field in ["tailwind", "aliases", "rsc", "tsx"] {
            let input = valid_config_json().replace(
                "\"items\": []",
                &format!("\"items\": [],\n  \"{field}\": true"),
            );

            let error = parse_kit_json_str(&input).expect_err("legacy field should fail");

            assert!(matches!(error, ConfigError::Parse(_)), "{field}");
        }
    }

    #[test]
    fn rejects_stale_style_prefix_fields() {
        for field in ["classPrefix", "cssVariablePrefix"] {
            let input = valid_config_json().replace(
                "\"css\": \"styles/kit.css\"",
                &format!("\"css\": \"styles/kit.css\",\n    \"{field}\": \"luk\""),
            );

            let error = parse_kit_json_str(&input).expect_err("prefix field should fail");

            assert!(matches!(error, ConfigError::Parse(_)), "{field}");
        }
    }

    #[test]
    fn rejects_render_mode_that_does_not_match_project_kind() {
        let input =
            valid_config_json().replace("\"renderMode\": \"csr\"", "\"renderMode\": \"hydrate\"");

        let error = parse_kit_json_str(&input).expect_err("hydrate should fail");

        assert!(matches!(
            error,
            ConfigError::InvalidValue {
                field: "leptos.renderMode",
                ..
            }
        ));
    }

    #[test]
    fn rejects_wrong_leptos_version() {
        let input =
            valid_config_json().replace("\"version\": \"0.9.0-alpha\"", "\"version\": \"0.8.17\"");

        let error = parse_kit_json_str(&input).expect_err("version should fail");

        assert!(
            matches!(error, ConfigError::InvalidValue { field, .. } if field == "leptos.version")
        );
    }

    #[test]
    fn accepts_explicit_safe_style_css_path() {
        let input = valid_config_json().replace(
            "\"css\": \"styles/kit.css\"",
            "\"css\": \"styles/generated.css\"",
        );

        let config = parse_kit_json_str(&input).expect("parse config");

        assert_eq!(config.styles.css, "styles/generated.css");
    }

    #[test]
    fn rejects_css_paths_outside_styles_dir() {
        let input =
            valid_config_json().replace("\"css\": \"styles/kit.css\"", "\"css\": \"src/app.css\"");

        let error = parse_kit_json_str(&input).expect_err("css path should fail");

        assert!(matches!(error, ConfigError::InvalidValue { field, .. } if field == "styles.css"));
    }

    #[test]
    fn rejects_non_css_style_paths() {
        let input = valid_config_json()
            .replace("\"css\": \"styles/kit.css\"", "\"css\": \"styles/kit.txt\"");

        let error = parse_kit_json_str(&input).expect_err("css path should fail");

        assert!(matches!(error, ConfigError::InvalidValue { field, .. } if field == "styles.css"));
    }

    #[test]
    fn rejects_hidden_style_css_paths() {
        let input = valid_config_json().replace(
            "\"css\": \"styles/kit.css\"",
            "\"css\": \"styles/.kit.css\"",
        );

        let error = parse_kit_json_str(&input).expect_err("css path should fail");

        assert!(
            matches!(error, ConfigError::UnsafePathSegment { field, .. } if field == "styles.css")
        );
    }

    #[test]
    fn rejects_parent_traversal_in_paths() {
        let input =
            valid_config_json().replace("\"css\": \"styles/kit.css\"", "\"css\": \"../app.css\"");

        let error = parse_kit_json_str(&input).expect_err("traversal should fail");

        assert!(matches!(error, ConfigError::PathTraversal { field, .. } if field == "styles.css"));
    }

    #[test]
    fn normalizes_canonical_install_roots() {
        let config = parse_kit_json_str(&valid_config_json()).expect("parse config");
        let normalized = normalize_single_crate_project(
            &config,
            &NormalizeOptions {
                project_root: PathBuf::from("/workspace/demo"),
            },
        )
        .expect("normalize");

        assert_eq!(normalized.render_mode, Some(RenderMode::Csr));
        assert_eq!(normalized.source_root, PathBuf::from("/workspace/demo/src"));
        assert_eq!(
            normalized.install_roots.ui_dir,
            PathBuf::from("/workspace/demo/src/components/ui")
        );
        assert_eq!(
            normalized.install_roots.css_file,
            PathBuf::from("/workspace/demo/styles/kit.css")
        );
        assert_eq!(normalized.project_kind, ProjectKind::SingleCrateTrunkCsr);
        assert_eq!(
            normalized.index_html,
            Some(PathBuf::from("/workspace/demo/index.html"))
        );
    }

    #[test]
    fn shared_library_project_omits_index_html_and_normalizes_safely() {
        let mut config = parse_kit_json_str(&valid_config_json()).expect("parse config");
        config.project.kind = ProjectKind::SharedLibraryCrate;
        config.project.index_html = None;
        config.leptos.render_mode = None;

        let encoded = kit_config_to_json(&config).expect("serialize shared config");
        assert!(encoded.contains("\"kind\": \"shared-library-crate\""));
        assert!(!encoded.contains("\"indexHtml\""));

        let parsed = parse_kit_json_str(&encoded).expect("parse shared config");
        let normalized = normalize_project(
            &parsed,
            &NormalizeOptions {
                project_root: PathBuf::from("/workspace/shared_ui"),
            },
        )
        .expect("normalize shared project");
        assert_eq!(normalized.project_kind, ProjectKind::SharedLibraryCrate);
        assert_eq!(normalized.render_mode, None);
        assert_eq!(normalized.index_html, None);
        assert_eq!(
            normalized.install_roots.ui_dir,
            PathBuf::from("/workspace/shared_ui/src/components/ui")
        );
    }

    #[test]
    fn native_ssr_and_browser_hydration_projects_round_trip_exact_modes() {
        for (kind, mode, serialized_kind) in [
            (
                ProjectKind::SingleCrateNativeSsr,
                RenderMode::Ssr,
                "single-crate-native-ssr",
            ),
            (
                ProjectKind::SingleCrateBrowserHydration,
                RenderMode::Hydrate,
                "single-crate-browser-hydration",
            ),
        ] {
            let mut config = parse_kit_json_str(&valid_config_json()).expect("canonical config");
            config.project.kind = kind;
            config.project.index_html = None;
            config.leptos.render_mode = Some(mode);

            let encoded = kit_config_to_json(&config).expect("serialize delivery config");
            assert!(encoded.contains(&format!("\"kind\": \"{serialized_kind}\"")));
            assert!(encoded.contains(&format!("\"renderMode\": \"{}\"", mode.as_str())));
            assert!(!encoded.contains("\"indexHtml\""));

            let parsed = parse_kit_json_str(&encoded).expect("parse delivery config");
            assert_eq!(
                parsed.project.kind.render_mode_contract(),
                RenderModeContract::Selected(mode)
            );
            assert_eq!(parsed.leptos.render_mode, Some(mode));
        }
    }

    #[test]
    fn project_kind_and_index_html_contract_fails_closed() {
        let mut trunk = parse_kit_json_str(&valid_config_json()).expect("parse config");
        trunk.project.index_html = None;
        assert!(matches!(
            trunk.validate(),
            Err(ConfigError::InvalidValue {
                field: "project.indexHtml",
                ..
            })
        ));

        let mut shared = parse_kit_json_str(&valid_config_json()).expect("parse config");
        shared.project.kind = ProjectKind::SharedLibraryCrate;
        assert!(matches!(
            shared.validate(),
            Err(ConfigError::InvalidValue {
                field: "project.indexHtml",
                ..
            })
        ));
    }

    #[test]
    fn accepts_and_normalizes_a_coherent_custom_components_root() {
        let mut config = parse_kit_json_str(&valid_config_json()).expect("parse config");
        config.install = InstallConfig {
            ui_dir: "src/widgets/kit_ui".to_owned(),
            ui_mod: "src/widgets/kit_ui/mod.rs".to_owned(),
            components_mod: "src/widgets/mod.rs".to_owned(),
        };
        let serialized = kit_config_to_json(&config).expect("serialize custom root");
        let parsed = parse_kit_json_str(&serialized).expect("parse custom root");
        let normalized = normalize_single_crate_project(
            &parsed,
            &NormalizeOptions {
                project_root: PathBuf::from("/workspace/demo"),
            },
        )
        .expect("normalize custom root");

        assert_eq!(
            normalized.install_roots.components_mod,
            PathBuf::from("/workspace/demo/src/widgets/mod.rs")
        );
        assert_eq!(
            normalized.install_roots.ui_dir,
            PathBuf::from("/workspace/demo/src/widgets/kit_ui")
        );
        assert_eq!(
            normalized.install_roots.ui_mod,
            PathBuf::from("/workspace/demo/src/widgets/kit_ui/mod.rs")
        );
    }

    #[test]
    fn rejects_incoherent_or_unsafe_custom_components_roots() {
        let cases = [
            (
                InstallConfig {
                    ui_dir: "src/widgets/kit_ui".to_owned(),
                    ui_mod: "src/widgets/other/mod.rs".to_owned(),
                    components_mod: "src/widgets/mod.rs".to_owned(),
                },
                "install.uiMod",
            ),
            (
                InstallConfig {
                    ui_dir: "src/Widgets/kit_ui".to_owned(),
                    ui_mod: "src/widgets/kit_ui/mod.rs".to_owned(),
                    components_mod: "src/Widgets/mod.rs".to_owned(),
                },
                "install.uiMod",
            ),
            (
                InstallConfig {
                    ui_dir: "src/other/kit_ui".to_owned(),
                    ui_mod: "src/other/kit_ui/mod.rs".to_owned(),
                    components_mod: "src/widgets/mod.rs".to_owned(),
                },
                "install.uiDir",
            ),
            (
                InstallConfig {
                    ui_dir: "src/widgets/ui-kit".to_owned(),
                    ui_mod: "src/widgets/ui-kit/mod.rs".to_owned(),
                    components_mod: "src/widgets/mod.rs".to_owned(),
                },
                "install.uiDir",
            ),
            (
                InstallConfig {
                    ui_dir: "src/widgets/type".to_owned(),
                    ui_mod: "src/widgets/type/mod.rs".to_owned(),
                    components_mod: "src/widgets/mod.rs".to_owned(),
                },
                "install.uiDir",
            ),
            (
                InstallConfig {
                    ui_dir: "src/components/ui/_kit".to_owned(),
                    ui_mod: "src/components/ui/_kit/mod.rs".to_owned(),
                    components_mod: "src/components/ui/mod.rs".to_owned(),
                },
                "install.uiDir",
            ),
        ];

        for (install, expected_field) in cases {
            let mut config = parse_kit_json_str(&valid_config_json()).expect("parse config");
            config.install = install;
            let error = config.validate().expect_err("unsafe custom root must fail");
            assert!(
                matches!(
                    error,
                    ConfigError::InvalidValue { field, .. }
                        | ConfigError::UnsafePathSegment { field, .. }
                        | ConfigError::PathOverlap { field, .. }
                        if field == expected_field
                ),
                "unexpected error for {expected_field}: {error}"
            );
        }
    }
}
