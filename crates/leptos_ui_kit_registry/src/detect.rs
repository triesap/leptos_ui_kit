use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use toml::Value as TomlValue;

use crate::{
    ComponentsConfig, ConfigError, NormalizeOptions, NormalizedProjectConfig, RenderMode,
    TailwindVersion, WorkspaceMode, normalize_single_crate_project, parse_components_json_str,
};

#[derive(Debug)]
pub enum DetectionError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    CargoTomlParse(toml::de::Error),
    MissingCargoManifest(PathBuf),
    MissingSourceRoot(PathBuf),
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
            Self::MissingSourceRoot(path) => {
                write!(f, "missing source root at {}", path.display())
            }
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
pub struct DetectedTailwind {
    pub version: Option<TailwindVersion>,
    pub css_entry: Option<PathBuf>,
    pub trunk_config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedProject {
    pub project_root: PathBuf,
    pub cargo_manifest_path: PathBuf,
    pub workspace_mode: WorkspaceMode,
    pub source_root: PathBuf,
    pub render_mode: Option<RenderMode>,
    pub tailwind: DetectedTailwind,
    pub components_config_path: Option<PathBuf>,
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

    let source_root = project_root.join("src");
    if !source_root.is_dir() {
        return Err(DetectionError::MissingSourceRoot(source_root));
    }

    let render_mode = detect_render_mode(&manifest);
    let components_config_path = project_root.join("components.json");
    let components_config_path = components_config_path
        .is_file()
        .then_some(components_config_path);

    let trunk_config_path = project_root.join("Trunk.toml");
    let trunk_config = if trunk_config_path.is_file() {
        Some(trunk_config_path.clone())
    } else {
        None
    };

    let tailwind_css_entry = detect_tailwind_css_entry(project_root)?;
    let tailwind_version = detect_tailwind_version(
        project_root,
        trunk_config.as_deref(),
        tailwind_css_entry.as_deref(),
    )?;

    Ok(DetectedProject {
        project_root: project_root.to_path_buf(),
        cargo_manifest_path,
        workspace_mode: WorkspaceMode::SingleCrate,
        source_root,
        render_mode,
        tailwind: DetectedTailwind {
            version: tailwind_version,
            css_entry: tailwind_css_entry,
            trunk_config_path: trunk_config,
        },
        components_config_path,
    })
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
                source_root: detected.source_root.clone(),
                detected_render_mode: detected.render_mode,
                tailwind_version: detected.tailwind.version.unwrap_or(TailwindVersion::V4),
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

fn detect_render_mode(manifest: &TomlValue) -> Option<RenderMode> {
    let features = manifest
        .get("dependencies")
        .and_then(TomlValue::as_table)
        .and_then(|dependencies| dependencies.get("leptos"))
        .and_then(leptos_dependency_features)
        .unwrap_or_default();

    if features.iter().any(|feature| feature == "islands") {
        return Some(RenderMode::Islands);
    }

    if features.iter().any(|feature| feature == "hydrate") {
        return Some(RenderMode::Hydrate);
    }

    if features.iter().any(|feature| feature == "csr") {
        return Some(RenderMode::Csr);
    }

    None
}

fn leptos_dependency_features(value: &TomlValue) -> Option<Vec<String>> {
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

fn detect_tailwind_css_entry(project_root: &Path) -> Result<Option<PathBuf>, DetectionError> {
    let index_path = project_root.join("index.html");
    if !index_path.is_file() {
        return Ok(None);
    }

    let html = read_to_string(&index_path)?;
    let Some(link_tag) = html
        .lines()
        .find(|line| line.contains("rel=\"tailwind-css\""))
    else {
        return Ok(None);
    };

    let Some(href_start) = link_tag.find("href=\"") else {
        return Ok(None);
    };
    let href_value = &link_tag[href_start + 6..];
    let Some(href_end) = href_value.find('"') else {
        return Ok(None);
    };

    let href = &href_value[..href_end];
    let path = Path::new(href);
    if path.is_absolute() {
        return Ok(Some(path.to_path_buf()));
    }

    Ok(Some(project_root.join(path)))
}

fn detect_tailwind_version(
    project_root: &Path,
    trunk_config_path: Option<&Path>,
    css_entry: Option<&Path>,
) -> Result<Option<TailwindVersion>, DetectionError> {
    if let Some(path) = trunk_config_path {
        let trunk_toml = read_to_string(path)?;
        let manifest: TomlValue =
            toml::from_str(&trunk_toml).map_err(DetectionError::CargoTomlParse)?;

        if let Some(version) = manifest
            .get("tools")
            .and_then(TomlValue::as_table)
            .and_then(|tools| tools.get("tailwindcss"))
            .and_then(TomlValue::as_str)
        {
            if version.starts_with('4') {
                return Ok(Some(TailwindVersion::V4));
            }
        }
    }

    if let Some(path) = css_entry {
        let css_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        };

        if css_path.is_file() {
            let css = read_to_string(&css_path)?;
            if css.contains("@import \"tailwindcss\";") {
                return Ok(Some(TailwindVersion::V4));
            }
        }
    }

    Ok(None)
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

    use tempfile::tempdir;

    #[test]
    fn detects_leptos_web_project_shape() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.8.17", features = ["csr"] }
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("Trunk.toml"),
            "[tools]\ntailwindcss = \"4.1.13\"\n",
        )
        .expect("write trunk");
        fs::write(
            root.join("index.html"),
            r#"<!DOCTYPE html>
<html>
  <head>
    <link data-trunk rel="tailwind-css" href="input.css" />
  </head>
  <body></body>
</html>
"#,
        )
        .expect("write html");
        fs::write(root.join("input.css"), "@import \"tailwindcss\";\n").expect("write css");

        let detected = detect_single_crate_project(root).expect("detect project");

        assert_eq!(detected.workspace_mode, WorkspaceMode::SingleCrate);
        assert_eq!(detected.source_root, root.join("src"));
        assert_eq!(detected.render_mode, Some(RenderMode::Csr));
        assert_eq!(detected.tailwind.css_entry, Some(root.join("input.css")));
        assert_eq!(detected.tailwind.version, Some(TailwindVersion::V4));
        assert_eq!(detected.components_config_path, None);
    }

    #[test]
    fn info_output_normalizes_components_config_when_present() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.8.17", features = ["hydrate"] }
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("components.json"),
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
        .expect("write components.json");

        let info = build_info_output(root).expect("build info output");

        assert!(info.components_config.is_some());
        let normalized = info.normalized_config.expect("normalized config");
        assert_eq!(normalized.render_mode, RenderMode::Hydrate);
        assert_eq!(
            normalized.install_roots.css_file,
            root.join("src/styles/app.css")
        );
    }
}
