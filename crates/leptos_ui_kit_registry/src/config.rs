use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: &str = "0.9.0-alpha";
pub const COMPONENTS_SCHEMA_URL: &str =
    "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/components.schema.json";
pub const LEPTOS_VERSION: &str = "0.9.0-alpha";
pub const LEPTOS_ROUTER_VERSION: &str = "0.9.0-alpha";
pub const TOOL_PACKAGE: &str = "leptos_ui_kit_cli";
pub const TOOL_BINARY: &str = "leptos_ui_kit";
pub const TOOL_GIT_URL: &str = "https://github.com/triesap/leptos_ui_kit";
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
            Self::Parse(error) => write!(f, "failed to parse components.json: {error}"),
            Self::Serialize(error) => write!(f, "failed to serialize components.json: {error}"),
            Self::InvalidValue {
                field,
                expected,
                actual,
            } => write!(
                f,
                "invalid components.json value for {field}: expected {expected}, got {actual}"
            ),
            Self::PathMustBeRelative { field, value } => {
                write!(f, "components.json path {field} must be relative: {value}")
            }
            Self::PathTraversal { field, value } => {
                write!(
                    f,
                    "components.json path {field} must not traverse parent segments: {value}"
                )
            }
            Self::UnsafePathSegment { field, value } => {
                write!(
                    f,
                    "components.json path {field} contains an unsafe segment: {value}"
                )
            }
            Self::PathOverlap { field, value } => {
                write!(
                    f,
                    "components.json path {field} overlaps a reserved target: {value}"
                )
            }
            Self::MissingToolProvenance { package, binary } => {
                write!(
                    f,
                    "missing tool provenance for {package}/{binary}; pass an explicit git rev"
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
pub struct ComponentsConfig {
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

impl ComponentsConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        expect_string("$schema", COMPONENTS_SCHEMA_URL, &self.schema)?;
        expect_string("schemaVersion", SCHEMA_VERSION, &self.schema_version)?;
        self.tool.validate()?;
        self.project.validate()?;
        self.leptos.validate()?;
        self.install.validate()?;
        self.styles.validate()?;
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
#[serde(tag = "kind", rename_all = "kebab-case")]
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
    pub index_html: String,
}

impl ProjectConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        expect_path("project.crateRoot", ".", &self.crate_root)?;
        expect_path("project.srcDir", "src", &self.src_dir)?;
        expect_path("project.indexHtml", "index.html", &self.index_html)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectKind {
    SingleCrateTrunkCsr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeptosConfig {
    pub version: String,
    pub router_version: String,
    pub render_mode: RenderMode,
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
        expect_path("install.uiDir", "src/components/ui", &self.ui_dir)?;
        expect_path("install.uiMod", "src/components/ui/mod.rs", &self.ui_mod)?;
        expect_path(
            "install.componentsMod",
            "src/components/mod.rs",
            &self.components_mod,
        )
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
            (DesiredItemName::Button, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Collapsible, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Dialog, RegistrySource::Builtin) => Ok(()),
            (DesiredItemName::Tabs, RegistrySource::Builtin) => Ok(()),
        }
    }

    pub fn item_name(&self) -> &'static str {
        self.name.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesiredItemName {
    Button,
    Collapsible,
    Dialog,
    Tabs,
}

impl DesiredItemName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Button => "button",
            Self::Collapsible => "collapsible",
            Self::Dialog => "dialog",
            Self::Tabs => "tabs",
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
    pub render_mode: RenderMode,
    pub desired_items: Vec<DesiredItemConfig>,
    pub workspace_mode: WorkspaceMode,
    pub project_root: PathBuf,
    pub crate_root: PathBuf,
    pub source_root: PathBuf,
    pub index_html: PathBuf,
    pub install_roots: InstallRoots,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizeOptions {
    pub project_root: PathBuf,
}

pub fn canonical_tool_config() -> Result<ToolConfig, ConfigError> {
    let Some(rev) = option_env!("LEPTOS_UI_KIT_GIT_REV") else {
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
            rev: rev.to_owned(),
        },
    };
    tool.validate()?;
    Ok(tool)
}

pub fn canonical_components_config() -> Result<ComponentsConfig, ConfigError> {
    let tool = canonical_tool_config()?;
    let config = ComponentsConfig {
        schema: COMPONENTS_SCHEMA_URL.to_owned(),
        schema_version: SCHEMA_VERSION.to_owned(),
        tool,
        project: ProjectConfig {
            kind: ProjectKind::SingleCrateTrunkCsr,
            crate_root: ".".to_owned(),
            src_dir: "src".to_owned(),
            index_html: "index.html".to_owned(),
        },
        leptos: LeptosConfig {
            version: LEPTOS_VERSION.to_owned(),
            router_version: LEPTOS_ROUTER_VERSION.to_owned(),
            render_mode: RenderMode::Csr,
        },
        install: InstallConfig {
            ui_dir: "src/components/ui".to_owned(),
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

pub fn components_config_with_desired_item(
    mut config: ComponentsConfig,
    item: DesiredItemConfig,
) -> Result<ComponentsConfig, ConfigError> {
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

pub fn desired_builtin_tabs_item() -> DesiredItemConfig {
    DesiredItemConfig {
        name: DesiredItemName::Tabs,
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
    if rev.len() == 40 && rev.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ConfigError::InvalidValue {
            field,
            expected: "40-character git commit hash",
            actual: rev.to_owned(),
        })
    }
}

pub fn canonical_components_json() -> Result<String, ConfigError> {
    components_config_to_json(&canonical_components_config()?)
}

pub fn components_config_to_json(config: &ComponentsConfig) -> Result<String, ConfigError> {
    config.validate()?;
    let mut output = serde_json::to_string_pretty(config).map_err(ConfigError::Serialize)?;
    output.push('\n');
    Ok(output)
}

pub fn parse_components_json_str(input: &str) -> Result<ComponentsConfig, ConfigError> {
    let config: ComponentsConfig = serde_json::from_str(input)?;
    config.validate()?;
    Ok(config)
}

pub fn normalize_single_crate_project(
    config: &ComponentsConfig,
    options: &NormalizeOptions,
) -> Result<NormalizedProjectConfig, ConfigError> {
    config.validate()?;

    let project_root = options.project_root.clone();
    let crate_root = join_checked(
        &project_root,
        &config.project.crate_root,
        "project.crateRoot",
    )?;
    let source_root = join_checked(&project_root, &config.project.src_dir, "project.srcDir")?;
    let index_html = join_checked(
        &project_root,
        &config.project.index_html,
        "project.indexHtml",
    )?;

    Ok(NormalizedProjectConfig {
        schema_version: config.schema_version.clone(),
        render_mode: config.leptos.render_mode,
        desired_items: config.items.clone(),
        workspace_mode: WorkspaceMode::SingleCrate,
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

    fn valid_config_json() -> String {
        canonical_components_json().expect("serialize config")
    }

    #[test]
    fn parses_canonical_components_json() {
        let config = parse_components_json_str(&valid_config_json()).expect("parse config");

        assert_eq!(config.schema_version, SCHEMA_VERSION);
        assert_eq!(config.tool.package, TOOL_PACKAGE);
        assert_eq!(config.tool.binary, TOOL_BINARY);
        assert!(matches!(config.tool.source, ToolSourceConfig::Git { .. }));
        assert_eq!(config.leptos.version, LEPTOS_VERSION);
        assert_eq!(config.leptos.router_version, LEPTOS_ROUTER_VERSION);
        assert_eq!(config.leptos.render_mode, RenderMode::Csr);
        assert_eq!(config.styles.mode, StylesMode::PureCss);
        assert_eq!(config.registry.source, RegistrySource::Builtin);
        assert!(config.items.is_empty());
    }

    #[test]
    fn canonical_json_is_deterministic() {
        let first = canonical_components_json().expect("serialize first");
        let second = canonical_components_json().expect("serialize second");

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
        let config = parse_components_json_str(&valid_config_json()).expect("parse config");
        let config = components_config_with_desired_item(config, desired_builtin_button_item())
            .expect("add item");

        assert_eq!(config.items.len(), 1);
        assert_eq!(config.items[0].name, DesiredItemName::Button);
        assert_eq!(config.items[0].source, RegistrySource::Builtin);
    }

    #[test]
    fn desired_item_add_is_idempotent() {
        let config = parse_components_json_str(&valid_config_json()).expect("parse config");
        let config = components_config_with_desired_item(config, desired_builtin_button_item())
            .expect("add item");
        let config = components_config_with_desired_item(config, desired_builtin_button_item())
            .expect("add item");

        assert_eq!(config.items.len(), 1);
    }

    #[test]
    fn rejects_invalid_tool_rev() {
        let input = valid_config_json().replace("\"rev\": \"", "\"rev\": \"not-a-rev");

        let error = parse_components_json_str(&input).expect_err("rev should fail");

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

        let error = parse_components_json_str(&input).expect_err("url should fail");

        assert!(
            matches!(error, ConfigError::InvalidValue { field, .. } if field == "tool.source.url")
        );
    }

    #[test]
    fn rejects_invalid_desired_item() {
        let input = valid_config_json().replace(
            "\"items\": []",
            r#""items": [{"name":"card","source":"builtin"}]"#,
        );

        let error = parse_components_json_str(&input).expect_err("item should fail");

        assert!(matches!(error, ConfigError::Parse(_)));
    }

    #[test]
    fn rejects_duplicate_desired_items() {
        let input = valid_config_json().replace(
            "\"items\": []",
            r#""items": [{"name":"button","source":"builtin"},{"name":"button","source":"builtin"}]"#,
        );

        let error = parse_components_json_str(&input).expect_err("duplicate should fail");

        assert!(matches!(error, ConfigError::InvalidValue { field, .. } if field == "items"));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let input = valid_config_json().replace(
            "\"items\": []",
            "\"items\": [],\n  \"tailwind\": { \"css\": \"input.css\" }",
        );

        let error = parse_components_json_str(&input).expect_err("unknown field should fail");

        assert!(matches!(error, ConfigError::Parse(_)));
    }

    #[test]
    fn rejects_forbidden_legacy_fields() {
        for field in ["tailwind", "aliases", "rsc", "tsx"] {
            let input = valid_config_json().replace(
                "\"items\": []",
                &format!("\"items\": [],\n  \"{field}\": true"),
            );

            let error = parse_components_json_str(&input).expect_err("legacy field should fail");

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

            let error = parse_components_json_str(&input).expect_err("prefix field should fail");

            assert!(matches!(error, ConfigError::Parse(_)), "{field}");
        }
    }

    #[test]
    fn rejects_non_csr_render_mode() {
        let input =
            valid_config_json().replace("\"renderMode\": \"csr\"", "\"renderMode\": \"hydrate\"");

        let error = parse_components_json_str(&input).expect_err("hydrate should fail");

        assert!(matches!(error, ConfigError::Parse(_)));
    }

    #[test]
    fn rejects_wrong_leptos_version() {
        let input =
            valid_config_json().replace("\"version\": \"0.9.0-alpha\"", "\"version\": \"0.8.17\"");

        let error = parse_components_json_str(&input).expect_err("version should fail");

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

        let config = parse_components_json_str(&input).expect("parse config");

        assert_eq!(config.styles.css, "styles/generated.css");
    }

    #[test]
    fn rejects_css_paths_outside_styles_dir() {
        let input =
            valid_config_json().replace("\"css\": \"styles/kit.css\"", "\"css\": \"src/app.css\"");

        let error = parse_components_json_str(&input).expect_err("css path should fail");

        assert!(matches!(error, ConfigError::InvalidValue { field, .. } if field == "styles.css"));
    }

    #[test]
    fn rejects_non_css_style_paths() {
        let input = valid_config_json()
            .replace("\"css\": \"styles/kit.css\"", "\"css\": \"styles/kit.txt\"");

        let error = parse_components_json_str(&input).expect_err("css path should fail");

        assert!(matches!(error, ConfigError::InvalidValue { field, .. } if field == "styles.css"));
    }

    #[test]
    fn rejects_hidden_style_css_paths() {
        let input = valid_config_json().replace(
            "\"css\": \"styles/kit.css\"",
            "\"css\": \"styles/.kit.css\"",
        );

        let error = parse_components_json_str(&input).expect_err("css path should fail");

        assert!(
            matches!(error, ConfigError::UnsafePathSegment { field, .. } if field == "styles.css")
        );
    }

    #[test]
    fn rejects_parent_traversal_in_paths() {
        let input =
            valid_config_json().replace("\"css\": \"styles/kit.css\"", "\"css\": \"../app.css\"");

        let error = parse_components_json_str(&input).expect_err("traversal should fail");

        assert!(matches!(error, ConfigError::PathTraversal { field, .. } if field == "styles.css"));
    }

    #[test]
    fn normalizes_canonical_install_roots() {
        let config = parse_components_json_str(&valid_config_json()).expect("parse config");
        let normalized = normalize_single_crate_project(
            &config,
            &NormalizeOptions {
                project_root: PathBuf::from("/workspace/demo"),
            },
        )
        .expect("normalize");

        assert_eq!(normalized.render_mode, RenderMode::Csr);
        assert_eq!(normalized.source_root, PathBuf::from("/workspace/demo/src"));
        assert_eq!(
            normalized.install_roots.ui_dir,
            PathBuf::from("/workspace/demo/src/components/ui")
        );
        assert_eq!(
            normalized.install_roots.css_file,
            PathBuf::from("/workspace/demo/styles/kit.css")
        );
    }
}
