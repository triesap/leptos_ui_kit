use std::{
    fmt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: &str = "0.9.0-alpha";
pub const COMPONENTS_SCHEMA_URL: &str =
    "https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/components.schema.json";
pub const LEPTOS_VERSION: &str = "0.9.0-alpha";
pub const LEPTOS_ROUTER_VERSION: &str = "0.9.0-alpha";

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
    pub project: ProjectConfig,
    pub leptos: LeptosConfig,
    pub install: InstallConfig,
    pub styles: StylesConfig,
    pub registry: RegistryConfig,
    pub state: StateConfig,
}

impl ComponentsConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        expect_string("$schema", COMPONENTS_SCHEMA_URL, &self.schema)?;
        expect_string("schemaVersion", SCHEMA_VERSION, &self.schema_version)?;
        self.project.validate()?;
        self.leptos.validate()?;
        self.install.validate()?;
        self.styles.validate()?;
        self.registry.validate()?;
        self.state.validate()?;
        Ok(())
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
    pub class_prefix: String,
    pub css_variable_prefix: String,
}

impl StylesConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        expect_path("styles.css", "styles/app.css", &self.css)?;
        expect_string("styles.classPrefix", "luk", &self.class_prefix)?;
        expect_string("styles.cssVariablePrefix", "luk", &self.css_variable_prefix)
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
pub struct StateConfig {
    pub dir: String,
}

impl StateConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        expect_path("state.dir", ".leptos-ui", &self.dir)
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
    pub state_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedProjectConfig {
    pub schema_version: String,
    pub render_mode: RenderMode,
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

pub fn canonical_components_config() -> ComponentsConfig {
    ComponentsConfig {
        schema: COMPONENTS_SCHEMA_URL.to_owned(),
        schema_version: SCHEMA_VERSION.to_owned(),
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
            css: "styles/app.css".to_owned(),
            class_prefix: "luk".to_owned(),
            css_variable_prefix: "luk".to_owned(),
        },
        registry: RegistryConfig {
            source: RegistrySource::Builtin,
        },
        state: StateConfig {
            dir: ".leptos-ui".to_owned(),
        },
    }
}

pub fn canonical_components_json() -> Result<String, ConfigError> {
    let mut output = serde_json::to_string_pretty(&canonical_components_config())
        .map_err(ConfigError::Serialize)?;
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
            state_dir: join_checked(&project_root, &config.state.dir, "state.dir")?,
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
        assert_eq!(config.leptos.version, LEPTOS_VERSION);
        assert_eq!(config.leptos.router_version, LEPTOS_ROUTER_VERSION);
        assert_eq!(config.leptos.render_mode, RenderMode::Csr);
        assert_eq!(config.styles.mode, StylesMode::PureCss);
        assert_eq!(config.registry.source, RegistrySource::Builtin);
    }

    #[test]
    fn canonical_json_is_deterministic() {
        let first = canonical_components_json().expect("serialize first");
        let second = canonical_components_json().expect("serialize second");

        assert_eq!(first, second);
        assert!(first.ends_with('\n'));
        assert!(first.contains("\"schemaVersion\": \"0.9.0-alpha\""));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let input = valid_config_json().replace(
            "\"state\": {",
            "\"tailwind\": { \"css\": \"input.css\" },\n  \"state\": {",
        );

        let error = parse_components_json_str(&input).expect_err("unknown field should fail");

        assert!(matches!(error, ConfigError::Parse(_)));
    }

    #[test]
    fn rejects_forbidden_legacy_fields() {
        for field in ["tailwind", "aliases", "rsc", "tsx"] {
            let input = valid_config_json().replace(
                "\"state\": {",
                &format!("\"{field}\": true,\n  \"state\": {{"),
            );

            let error = parse_components_json_str(&input).expect_err("legacy field should fail");

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
    fn rejects_non_homepage_paths() {
        let input =
            valid_config_json().replace("\"css\": \"styles/app.css\"", "\"css\": \"src/app.css\"");

        let error = parse_components_json_str(&input).expect_err("css path should fail");

        assert!(matches!(error, ConfigError::InvalidValue { field, .. } if field == "styles.css"));
    }

    #[test]
    fn rejects_parent_traversal_in_paths() {
        let input =
            valid_config_json().replace("\"css\": \"styles/app.css\"", "\"css\": \"../app.css\"");

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
            PathBuf::from("/workspace/demo/styles/app.css")
        );
    }
}
