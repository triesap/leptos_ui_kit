use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use toml::Value as TomlValue;

use crate::{
    ComponentsConfig, ConfigError, LEPTOS_ROUTER_VERSION, LEPTOS_VERSION, NormalizeOptions,
    NormalizedProjectConfig, RenderMode, WorkspaceMode, normalize_single_crate_project,
    parse_components_json_str,
};

#[derive(Debug)]
pub enum DetectionError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    CargoTomlParse(toml::de::Error),
    MissingCargoManifest(PathBuf),
    MissingIndexHtml(PathBuf),
    MissingSourceRoot(PathBuf),
    MissingStylesheet(PathBuf),
    MissingTrunkCssLink {
        index_html: PathBuf,
        css_href: String,
    },
    UnsupportedProject(String),
    Config(ConfigError),
}

impl fmt::Display for DetectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "failed to read {}: {source}", path.display()),
            Self::CargoTomlParse(error) => write!(f, "failed to parse Cargo.toml: {error}"),
            Self::MissingCargoManifest(path) => {
                write!(f, "missing Cargo.toml at {}", path.display())
            }
            Self::MissingIndexHtml(path) => write!(f, "missing index.html at {}", path.display()),
            Self::MissingSourceRoot(path) => {
                write!(f, "missing source root at {}", path.display())
            }
            Self::MissingStylesheet(path) => {
                write!(f, "missing stylesheet at {}", path.display())
            }
            Self::MissingTrunkCssLink {
                index_html,
                css_href,
            } => write!(
                f,
                "missing Trunk CSS link for {css_href} in {}",
                index_html.display()
            ),
            Self::UnsupportedProject(reason) => write!(f, "unsupported project: {reason}"),
            Self::Config(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DetectionError {}

impl From<ConfigError> for DetectionError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedProject {
    pub project_root: PathBuf,
    pub cargo_manifest_path: PathBuf,
    pub workspace_mode: WorkspaceMode,
    pub source_root: PathBuf,
    pub index_html_path: PathBuf,
    pub css_file_path: PathBuf,
    pub render_mode: Option<RenderMode>,
    pub dependency_plan: DependencyPlan,
    pub components_config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyPlan {
    pub leptos: DependencyRequirement,
    pub leptos_router: DependencyRequirement,
}

impl DependencyPlan {
    fn from_manifest(manifest: &TomlValue) -> Self {
        let leptos = dependency_requirement(manifest, "leptos", LEPTOS_VERSION, true);
        let leptos_router =
            dependency_requirement(manifest, "leptos_router", LEPTOS_ROUTER_VERSION, false);

        Self {
            leptos,
            leptos_router,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyRequirement {
    pub crate_name: String,
    pub required_version: String,
    pub found_version: Option<String>,
    pub features: Vec<String>,
    pub status: DependencyStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyStatus {
    Satisfied,
    Missing,
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoOutput {
    pub detected: DetectedProject,
    pub components_config: Option<ComponentsConfig>,
    pub normalized_config: Option<NormalizedProjectConfig>,
}

pub fn detect_single_crate_project(project_root: &Path) -> Result<DetectedProject, DetectionError> {
    let cargo_manifest_path = project_root.join("Cargo.toml");
    if !cargo_manifest_path.is_file() {
        return Err(DetectionError::MissingCargoManifest(cargo_manifest_path));
    }

    let cargo_toml = read_to_string(&cargo_manifest_path)?;
    let manifest: TomlValue =
        toml::from_str(&cargo_toml).map_err(DetectionError::CargoTomlParse)?;

    let package = manifest
        .get("package")
        .and_then(TomlValue::as_table)
        .ok_or_else(|| DetectionError::UnsupportedProject("missing [package] table".to_owned()))?;

    if package.get("name").and_then(TomlValue::as_str).is_none() {
        return Err(DetectionError::UnsupportedProject(
            "missing package.name".to_owned(),
        ));
    }

    let workspace_mode = detect_workspace_mode(&manifest)?;

    let source_root = project_root.join("src");
    if !source_root.is_dir() {
        return Err(DetectionError::MissingSourceRoot(source_root));
    }

    let index_html_path = project_root.join("index.html");
    if !index_html_path.is_file() {
        return Err(DetectionError::MissingIndexHtml(index_html_path));
    }

    let css_file_path = project_root.join("styles/app.css");
    if !css_file_path.is_file() {
        return Err(DetectionError::MissingStylesheet(css_file_path));
    }

    let html = read_to_string(&index_html_path)?;
    if !contains_trunk_css_link(&html, "styles/app.css") {
        return Err(DetectionError::MissingTrunkCssLink {
            index_html: index_html_path,
            css_href: "styles/app.css".to_owned(),
        });
    }

    let dependency_plan = DependencyPlan::from_manifest(&manifest);
    let render_mode = detect_render_mode(&dependency_plan);
    let components_config_path = project_root.join("components.json");
    let components_config_path = components_config_path
        .is_file()
        .then_some(components_config_path);

    Ok(DetectedProject {
        project_root: project_root.to_path_buf(),
        cargo_manifest_path,
        workspace_mode,
        source_root,
        index_html_path,
        css_file_path,
        render_mode,
        dependency_plan,
        components_config_path,
    })
}

fn detect_workspace_mode(manifest: &TomlValue) -> Result<WorkspaceMode, DetectionError> {
    let Some(workspace) = manifest.get("workspace").and_then(TomlValue::as_table) else {
        return Ok(WorkspaceMode::SingleCrate);
    };

    let members = workspace
        .get("members")
        .and_then(TomlValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(TomlValue::as_str)
        .collect::<Vec<_>>();

    if members.iter().any(|member| *member != ".") {
        return Err(DetectionError::UnsupportedProject(
            "multi-member workspace roots are not supported by leptos_ui_kit 0.9.0-alpha"
                .to_owned(),
        ));
    }

    Ok(WorkspaceMode::SinglePackageWorkspaceRoot)
}

pub fn build_info_output(project_root: &Path) -> Result<InfoOutput, DetectionError> {
    let detected = detect_single_crate_project(project_root)?;

    let components_config = match detected.components_config_path.as_ref() {
        Some(path) => Some(parse_components_json_str(&read_to_string(path)?)?),
        None => None,
    };

    let normalized_config = match components_config.as_ref() {
        Some(config) => Some(normalize_single_crate_project(
            config,
            &NormalizeOptions {
                project_root: detected.project_root.clone(),
            },
        )?),
        None => None,
    };

    Ok(InfoOutput {
        detected,
        components_config,
        normalized_config,
    })
}

fn detect_render_mode(dependency_plan: &DependencyPlan) -> Option<RenderMode> {
    if dependency_plan
        .leptos
        .features
        .iter()
        .any(|feature| feature == "csr")
    {
        return Some(RenderMode::Csr);
    }

    None
}

fn dependency_requirement(
    manifest: &TomlValue,
    crate_name: &str,
    required_version: &str,
    require_csr_feature: bool,
) -> DependencyRequirement {
    let dependency = manifest
        .get("dependencies")
        .and_then(TomlValue::as_table)
        .and_then(|dependencies| dependencies.get(crate_name));

    let found_version = dependency
        .and_then(dependency_version)
        .map(ToOwned::to_owned);
    let features = dependency.and_then(dependency_features).unwrap_or_default();

    let status = match found_version.as_deref() {
        None => DependencyStatus::Missing,
        Some(version)
            if version == required_version
                && (!require_csr_feature || features.iter().any(|feature| feature == "csr"))
                && !features
                    .iter()
                    .any(|feature| matches!(feature.as_str(), "hydrate" | "ssr" | "islands")) =>
        {
            DependencyStatus::Satisfied
        }
        Some(_) => DependencyStatus::Incompatible,
    };

    DependencyRequirement {
        crate_name: crate_name.to_owned(),
        required_version: required_version.to_owned(),
        found_version,
        features,
        status,
    }
}

fn dependency_version(value: &TomlValue) -> Option<&str> {
    match value {
        TomlValue::String(version) => Some(version),
        TomlValue::Table(table) => table.get("version").and_then(TomlValue::as_str),
        _ => None,
    }
}

fn dependency_features(value: &TomlValue) -> Option<Vec<String>> {
    match value {
        TomlValue::Table(table) => Some(
            table
                .get("features")
                .and_then(TomlValue::as_array)
                .into_iter()
                .flatten()
                .filter_map(TomlValue::as_str)
                .map(ToOwned::to_owned)
                .collect(),
        ),
        _ => None,
    }
}

fn contains_trunk_css_link(html: &str, href: &str) -> bool {
    html.lines().any(|line| {
        line.contains("data-trunk")
            && line.contains("rel=\"css\"")
            && line.contains(&format!("href=\"{href}\""))
    })
}

fn read_to_string(path: &Path) -> Result<String, DetectionError> {
    fs::read_to_string(path).map_err(|source| DetectionError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::canonical_components_json;
    use tempfile::tempdir;

    fn write_homepage_fixture(root: &Path, features: &str) {
        fs::write(
            root.join("Cargo.toml"),
            format!(
                r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = {{ version = "0.9.0-alpha", features = [{features}] }}
leptos_router = "0.9.0-alpha"
"#
            ),
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::create_dir(root.join("styles")).expect("create styles");
        fs::write(root.join("styles/app.css"), ":root {}\n").expect("write css");
        fs::write(
            root.join("index.html"),
            r#"<!DOCTYPE html>
<html>
  <head>
    <link data-trunk rel="css" href="styles/app.css" />
  </head>
  <body></body>
</html>
"#,
        )
        .expect("write html");
    }

    #[test]
    fn detects_homepage_trunk_csr_project_shape() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"csr\"");

        let detected = detect_single_crate_project(root).expect("detect project");

        assert_eq!(detected.workspace_mode, WorkspaceMode::SingleCrate);
        assert_eq!(detected.source_root, root.join("src"));
        assert_eq!(detected.index_html_path, root.join("index.html"));
        assert_eq!(detected.css_file_path, root.join("styles/app.css"));
        assert_eq!(detected.render_mode, Some(RenderMode::Csr));
        assert_eq!(
            detected.dependency_plan.leptos.status,
            DependencyStatus::Satisfied
        );
        assert_eq!(
            detected.dependency_plan.leptos_router.status,
            DependencyStatus::Satisfied
        );
        assert_eq!(detected.components_config_path, None);
    }

    #[test]
    fn detects_single_package_workspace_root_project_shape() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"csr\"");
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[workspace]
resolver = "2"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
leptos_router = "0.9.0-alpha"
"#,
        )
        .expect("write cargo");

        let detected = detect_single_crate_project(root).expect("detect project");

        assert_eq!(
            detected.workspace_mode,
            WorkspaceMode::SinglePackageWorkspaceRoot
        );
        assert_eq!(detected.render_mode, Some(RenderMode::Csr));
        assert_eq!(
            detected.dependency_plan.leptos.status,
            DependencyStatus::Satisfied
        );
    }

    #[test]
    fn rejects_missing_trunk_css_link() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"csr\"");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write html");

        let error = detect_single_crate_project(root).expect_err("css link should fail");

        assert!(matches!(error, DetectionError::MissingTrunkCssLink { .. }));
    }

    #[test]
    fn dependency_plan_reports_missing_dependencies() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"csr\"");
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
"#,
        )
        .expect("write cargo");

        let detected = detect_single_crate_project(root).expect("detect project");

        assert_eq!(
            detected.dependency_plan.leptos.status,
            DependencyStatus::Missing
        );
        assert_eq!(
            detected.dependency_plan.leptos_router.status,
            DependencyStatus::Missing
        );
        assert_eq!(detected.render_mode, None);
    }

    #[test]
    fn dependency_plan_reports_incompatible_versions_and_features() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"hydrate\"");

        let detected = detect_single_crate_project(root).expect("detect project");

        assert_eq!(
            detected.dependency_plan.leptos.status,
            DependencyStatus::Incompatible
        );
        assert_eq!(detected.render_mode, None);
    }

    #[test]
    fn rejects_multi_member_workspace_roots() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"csr\"");
        fs::write(
            root.join("Cargo.toml"),
            r#"[workspace]
members = ["app"]

[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
leptos_router = "0.9.0-alpha"
"#,
        )
        .expect("write cargo");

        let error =
            detect_single_crate_project(root).expect_err("multi-member workspace should fail");

        assert!(matches!(error, DetectionError::UnsupportedProject(_)));
    }

    #[test]
    fn info_output_normalizes_components_config_when_present() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"csr\"");
        fs::write(
            root.join("components.json"),
            canonical_components_json().expect("canonical config"),
        )
        .expect("write components.json");

        let info = build_info_output(root).expect("build info output");

        assert!(info.components_config.is_some());
        let normalized = info.normalized_config.expect("normalized config");
        assert_eq!(normalized.render_mode, RenderMode::Csr);
        assert_eq!(
            normalized.install_roots.css_file,
            root.join("styles/app.css")
        );
    }
}
