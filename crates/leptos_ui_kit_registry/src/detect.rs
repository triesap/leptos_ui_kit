use std::{
    collections::BTreeSet,
    fmt, fs,
    path::{Path, PathBuf},
};

use semver::{Op as SemverOp, Version, VersionReq};
use serde::{Deserialize, Serialize};
use toml::{Table as TomlTable, Value as TomlValue};

use crate::{
    CargoPlanEntry, CargoPlanSource, CargoPlanSourceKind, ConfigError, DEFAULT_CSS_PATH,
    DEFAULT_KIT_CONFIG_PATH, KitConfig, LEPTOS_VERSION, NormalizeOptions, NormalizedProjectConfig,
    ProjectKind, RegistryError, RenderMode, WorkspaceMode, normalize_cargo_plan,
    normalize_project_with_workspace_mode, parse_kit_json_str,
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
    UnsupportedProject(String),
    Config(ConfigError),
    Registry(RegistryError),
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
            Self::UnsupportedProject(reason) => write!(f, "unsupported project: {reason}"),
            Self::Config(error) => write!(f, "{error}"),
            Self::Registry(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DetectionError {}

impl From<ConfigError> for DetectionError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

impl From<RegistryError> for DetectionError {
    fn from(value: RegistryError) -> Self {
        Self::Registry(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedProject {
    pub project_root: PathBuf,
    pub cargo_manifest_path: PathBuf,
    pub project_kind: ProjectKind,
    pub workspace_mode: WorkspaceMode,
    pub source_root: PathBuf,
    pub index_html_path: Option<PathBuf>,
    pub css_file_path: PathBuf,
    pub render_mode: Option<RenderMode>,
    pub dependency_plan: DependencyPlan,
    pub kit_config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyPlan {
    pub leptos: DependencyRequirement,
}

impl DependencyPlan {
    fn from_manifest(manifest: &TomlValue) -> Self {
        let leptos = dependency_requirement_for_cargo_plan(
            manifest,
            &CargoPlanEntry {
                crate_name: "leptos".to_owned(),
                source: CargoPlanSource::version(LEPTOS_VERSION),
                features: vec!["csr".to_owned()],
                required: true,
            },
        );
        Self { leptos }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyRequirement {
    pub crate_name: String,
    pub required: bool,
    pub required_source: CargoPlanSource,
    pub required_features: Vec<String>,
    pub found_source: Option<DetectedDependencySource>,
    pub features: Vec<String>,
    pub status: DependencyStatus,
    pub incompatibility: Option<DependencyIncompatibility>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedDependencySource {
    pub kind: CargoPlanSourceKind,
    pub version: Option<String>,
    pub url: Option<String>,
    pub rev: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyStatus {
    Satisfied,
    Missing,
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DependencyIncompatibility {
    InvalidDeclaration,
    Renamed {
        dependency_key: String,
    },
    NonNormal {
        declaration: DependencyDeclarationKind,
    },
    Optional,
    MissingWorkspaceDeclaration,
    InvalidWorkspaceInheritance,
    UnsupportedPathSource,
    UnsupportedRegistrySource,
    UnsupportedGitSource,
    UnsupportedSource,
    InvalidVersionRequirement,
    UnprovenVersionRequirement,
    SourceMismatch,
    MissingFeatures {
        features: Vec<String>,
    },
    ConflictingFeatures {
        features: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyDeclarationKind {
    Development,
    Build,
    Target,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoOutput {
    pub detected: DetectedProject,
    pub kit_config: Option<KitConfig>,
    pub normalized_config: Option<NormalizedProjectConfig>,
}

pub fn detect_project(project_root: &Path) -> Result<DetectedProject, DetectionError> {
    detect_project_snapshot_with(project_root, |path| fs::read_to_string(path))
        .map(|snapshot| snapshot.detected)
}

pub fn detect_single_crate_project(project_root: &Path) -> Result<DetectedProject, DetectionError> {
    detect_project(project_root)
}

struct ProjectSnapshot {
    detected: DetectedProject,
    kit_config: Option<KitConfig>,
}

fn detect_project_snapshot_with(
    project_root: &Path,
    mut read: impl FnMut(&Path) -> Result<String, std::io::Error>,
) -> Result<ProjectSnapshot, DetectionError> {
    let cargo_manifest_path = project_root.join("Cargo.toml");
    if !cargo_manifest_path.is_file() {
        return Err(DetectionError::MissingCargoManifest(cargo_manifest_path));
    }

    let cargo_toml = read_path_with(&cargo_manifest_path, &mut read)?;
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

    let kit_config_path = project_root.join(DEFAULT_KIT_CONFIG_PATH);
    let kit_config_path = kit_config_path.is_file().then_some(kit_config_path);
    let kit_config = match kit_config_path.as_ref() {
        Some(path) => Some(parse_kit_json_str(&read_path_with(path, &mut read)?)?),
        None => None,
    };
    let project_kind = kit_config
        .as_ref()
        .map(|config| config.project.kind)
        .unwrap_or(ProjectKind::SingleCrateTrunkCsr);
    let index_html_path = match kit_config
        .as_ref()
        .and_then(|config| config.project.index_html.as_deref())
        .or_else(|| (project_kind == ProjectKind::SingleCrateTrunkCsr).then_some("index.html"))
    {
        Some(relative) => {
            let path = project_root.join(relative);
            if !path.is_file() {
                return Err(DetectionError::MissingIndexHtml(path));
            }
            Some(path)
        }
        None => None,
    };
    let css_file_path = project_root.join(
        kit_config
            .as_ref()
            .map(|config| config.styles.css.as_str())
            .unwrap_or(DEFAULT_CSS_PATH),
    );

    let dependency_plan = DependencyPlan::from_manifest(&manifest);
    let render_mode = detect_render_mode(&dependency_plan);

    Ok(ProjectSnapshot {
        detected: DetectedProject {
            project_root: project_root.to_path_buf(),
            cargo_manifest_path,
            project_kind,
            workspace_mode,
            source_root,
            index_html_path,
            css_file_path,
            render_mode,
            dependency_plan,
            kit_config_path,
        },
        kit_config,
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
    build_info_output_with(project_root, |path| fs::read_to_string(path))
}

fn build_info_output_with(
    project_root: &Path,
    read: impl FnMut(&Path) -> Result<String, std::io::Error>,
) -> Result<InfoOutput, DetectionError> {
    let snapshot = detect_project_snapshot_with(project_root, read)?;
    let normalized_config = match snapshot.kit_config.as_ref() {
        Some(config) => Some(normalize_project_with_workspace_mode(
            config,
            &NormalizeOptions {
                project_root: snapshot.detected.project_root.clone(),
            },
            snapshot.detected.workspace_mode,
        )?),
        None => None,
    };

    Ok(InfoOutput {
        detected: snapshot.detected,
        kit_config: snapshot.kit_config,
        normalized_config,
    })
}

pub fn detect_cargo_plan_requirements(
    project_root: &Path,
    cargo_plan: &[CargoPlanEntry],
) -> Result<Vec<DependencyRequirement>, DetectionError> {
    let cargo_manifest_path = project_root.join("Cargo.toml");
    if !cargo_manifest_path.is_file() {
        return Err(DetectionError::MissingCargoManifest(cargo_manifest_path));
    }

    let cargo_toml = read_to_string(&cargo_manifest_path)?;
    let manifest: TomlValue =
        toml::from_str(&cargo_toml).map_err(DetectionError::CargoTomlParse)?;

    Ok(normalize_cargo_plan(cargo_plan)?
        .iter()
        .map(|entry| dependency_requirement_for_cargo_plan(&manifest, entry))
        .collect())
}

fn detect_render_mode(dependency_plan: &DependencyPlan) -> Option<RenderMode> {
    if dependency_plan.leptos.status == DependencyStatus::Satisfied {
        return Some(RenderMode::Csr);
    }

    None
}

pub fn dependency_requirement_for_cargo_plan(
    manifest: &TomlValue,
    entry: &CargoPlanEntry,
) -> DependencyRequirement {
    let effective = effective_normal_dependency(manifest, &entry.crate_name);
    let found_source = effective.as_ref().and_then(|found| found.source.clone());
    let features = effective
        .as_ref()
        .map(|found| found.features.clone())
        .unwrap_or_default();
    let incompatibility = effective
        .as_ref()
        .and_then(|found| found.incompatibility.clone())
        .or_else(|| {
            found_source.as_ref().and_then(|source| {
                if source_matches_requirement(source, &entry.source) {
                    None
                } else if source.kind == CargoPlanSourceKind::Version
                    && entry.source.kind == CargoPlanSourceKind::Version
                {
                    Some(DependencyIncompatibility::UnprovenVersionRequirement)
                } else {
                    Some(DependencyIncompatibility::SourceMismatch)
                }
            })
        })
        .or_else(|| {
            let missing = entry
                .features
                .iter()
                .filter(|required| !features.iter().any(|found| found == *required))
                .cloned()
                .collect::<Vec<_>>();
            (!missing.is_empty())
                .then_some(DependencyIncompatibility::MissingFeatures { features: missing })
        })
        .or_else(|| {
            let conflicting = conflicting_features(&entry.crate_name, &features);
            (!conflicting.is_empty()).then_some(DependencyIncompatibility::ConflictingFeatures {
                features: conflicting,
            })
        });
    let status = match effective {
        None => DependencyStatus::Missing,
        Some(_) if incompatibility.is_none() => DependencyStatus::Satisfied,
        Some(_) => DependencyStatus::Incompatible,
    };

    DependencyRequirement {
        crate_name: entry.crate_name.clone(),
        required: entry.required,
        required_source: entry.source.clone(),
        required_features: entry.features.clone(),
        found_source,
        features,
        status,
        incompatibility,
    }
}

#[derive(Debug)]
struct EffectiveDependency {
    source: Option<DetectedDependencySource>,
    features: Vec<String>,
    incompatibility: Option<DependencyIncompatibility>,
}

fn effective_normal_dependency(
    manifest: &TomlValue,
    crate_name: &str,
) -> Option<EffectiveDependency> {
    let normal = manifest.get("dependencies").and_then(TomlValue::as_table);
    if let Some(value) = normal.and_then(|dependencies| dependencies.get(crate_name)) {
        let resolved = resolve_normal_dependency(manifest, crate_name, value);
        if resolved.incompatibility.is_some() {
            return Some(resolved);
        }
        if let Some(dependency_key) = renamed_dependency_key(normal, crate_name) {
            return Some(EffectiveDependency {
                incompatibility: Some(DependencyIncompatibility::Renamed { dependency_key }),
                ..resolved
            });
        }
        if manifest
            .get("target")
            .and_then(TomlValue::as_table)
            .is_some_and(|targets| target_has_dependency(targets, crate_name))
        {
            return Some(EffectiveDependency {
                incompatibility: Some(DependencyIncompatibility::NonNormal {
                    declaration: DependencyDeclarationKind::Target,
                }),
                ..resolved
            });
        }
        return Some(resolved);
    }

    if let Some(dependency_key) = renamed_dependency_key(normal, crate_name) {
        return Some(incompatible_dependency(
            DependencyIncompatibility::Renamed { dependency_key },
        ));
    }

    for (table_name, declaration) in [
        ("dev-dependencies", DependencyDeclarationKind::Development),
        ("build-dependencies", DependencyDeclarationKind::Build),
    ] {
        let table = manifest.get(table_name).and_then(TomlValue::as_table);
        if table.is_some_and(|dependencies| dependencies.contains_key(crate_name))
            || renamed_dependency_key(table, crate_name).is_some()
        {
            return Some(incompatible_dependency(
                DependencyIncompatibility::NonNormal { declaration },
            ));
        }
    }

    if manifest
        .get("target")
        .and_then(TomlValue::as_table)
        .is_some_and(|targets| target_has_dependency(targets, crate_name))
    {
        return Some(incompatible_dependency(
            DependencyIncompatibility::NonNormal {
                declaration: DependencyDeclarationKind::Target,
            },
        ));
    }

    None
}

fn resolve_normal_dependency(
    manifest: &TomlValue,
    crate_name: &str,
    member_value: &TomlValue,
) -> EffectiveDependency {
    let Ok(mut features) = dependency_features(member_value) else {
        return incompatible_dependency(DependencyIncompatibility::InvalidDeclaration);
    };
    let Ok(mut optional) = dependency_optional(member_value) else {
        return incompatible_dependency(DependencyIncompatibility::InvalidDeclaration);
    };
    if !dependency_scalar_fields_are_valid(member_value) {
        return incompatible_dependency(DependencyIncompatibility::InvalidDeclaration);
    }
    let source_value = if member_value
        .as_table()
        .and_then(|table| table.get("workspace"))
        .is_some()
    {
        let Some(member) = member_value.as_table() else {
            return incompatible_dependency(DependencyIncompatibility::InvalidWorkspaceInheritance);
        };
        if member.get("workspace").and_then(TomlValue::as_bool) != Some(true)
            || member.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "workspace" | "features" | "optional" | "default-features"
                )
            })
        {
            return EffectiveDependency {
                source: dependency_source(member_value),
                features: sorted_features(features),
                incompatibility: Some(DependencyIncompatibility::InvalidWorkspaceInheritance),
            };
        }

        let Some(workspace_value) = manifest
            .get("workspace")
            .and_then(TomlValue::as_table)
            .and_then(|workspace| workspace.get("dependencies"))
            .and_then(TomlValue::as_table)
            .and_then(|dependencies| dependencies.get(crate_name))
        else {
            return EffectiveDependency {
                source: None,
                features: sorted_features(features),
                incompatibility: Some(DependencyIncompatibility::MissingWorkspaceDeclaration),
            };
        };
        let Ok(workspace_features) = dependency_features(workspace_value) else {
            return incompatible_dependency(DependencyIncompatibility::InvalidDeclaration);
        };
        let Ok(workspace_optional) = dependency_optional(workspace_value) else {
            return incompatible_dependency(DependencyIncompatibility::InvalidDeclaration);
        };
        if !dependency_scalar_fields_are_valid(workspace_value) {
            return incompatible_dependency(DependencyIncompatibility::InvalidDeclaration);
        }
        features.extend(workspace_features);
        optional |= workspace_optional;
        workspace_value
    } else {
        member_value
    };

    if dependency_package(source_value).is_some_and(|package| package != crate_name) {
        return EffectiveDependency {
            source: dependency_source(source_value),
            features: sorted_features(features),
            incompatibility: Some(DependencyIncompatibility::Renamed {
                dependency_key: crate_name.to_owned(),
            }),
        };
    }

    let features = sorted_features(features);
    if optional {
        return EffectiveDependency {
            source: dependency_source(source_value),
            features,
            incompatibility: Some(DependencyIncompatibility::Optional),
        };
    }

    let (source, incompatibility) = classify_dependency_source(source_value);
    EffectiveDependency {
        source,
        features,
        incompatibility,
    }
}

fn incompatible_dependency(incompatibility: DependencyIncompatibility) -> EffectiveDependency {
    EffectiveDependency {
        source: None,
        features: Vec::new(),
        incompatibility: Some(incompatibility),
    }
}

fn classify_dependency_source(
    value: &TomlValue,
) -> (
    Option<DetectedDependencySource>,
    Option<DependencyIncompatibility>,
) {
    let Some(table) = value.as_table() else {
        let source = dependency_source(value);
        let incompatibility = match source.as_ref() {
            Some(DetectedDependencySource {
                kind: CargoPlanSourceKind::Version,
                version: Some(version),
                ..
            }) => match VersionReq::parse(version) {
                Ok(_) => None,
                Err(_) => Some(DependencyIncompatibility::InvalidVersionRequirement),
            },
            Some(_) | None => Some(DependencyIncompatibility::UnsupportedSource),
        };
        return (source, incompatibility);
    };

    if table.contains_key("path") {
        return (
            dependency_source(value),
            Some(DependencyIncompatibility::UnsupportedPathSource),
        );
    }
    if table.contains_key("registry") {
        return (
            dependency_source(value),
            Some(DependencyIncompatibility::UnsupportedRegistrySource),
        );
    }
    if table.contains_key("git")
        && (table.contains_key("branch")
            || table.contains_key("tag")
            || !table.contains_key("rev")
            || table.contains_key("version"))
    {
        return (
            dependency_source(value),
            Some(DependencyIncompatibility::UnsupportedGitSource),
        );
    }
    if table
        .keys()
        .any(|key| !is_supported_dependency_key(key.as_str()))
        || (!table.contains_key("git")
            && ["rev", "branch", "tag"]
                .iter()
                .any(|key| table.contains_key(*key)))
    {
        return (
            dependency_source(value),
            Some(DependencyIncompatibility::UnsupportedSource),
        );
    }

    let source = dependency_source(value);
    let incompatibility = match source.as_ref() {
        Some(DetectedDependencySource {
            kind: CargoPlanSourceKind::Version,
            version: Some(version),
            ..
        }) => match VersionReq::parse(version) {
            Ok(_) => None,
            Err(_) => Some(DependencyIncompatibility::InvalidVersionRequirement),
        },
        Some(DetectedDependencySource {
            kind: CargoPlanSourceKind::Git,
            rev: Some(_),
            ..
        }) => None,
        Some(DetectedDependencySource {
            kind: CargoPlanSourceKind::Git,
            rev: None,
            ..
        }) => Some(DependencyIncompatibility::UnsupportedGitSource),
        Some(_) | None => Some(DependencyIncompatibility::UnsupportedSource),
    };
    (source, incompatibility)
}

fn dependency_source(value: &TomlValue) -> Option<DetectedDependencySource> {
    match value {
        TomlValue::String(version) => Some(DetectedDependencySource {
            kind: CargoPlanSourceKind::Version,
            version: Some(version.to_owned()),
            url: None,
            rev: None,
        }),
        TomlValue::Table(table) => {
            if let Some(url) = table.get("git").and_then(TomlValue::as_str) {
                return Some(DetectedDependencySource {
                    kind: CargoPlanSourceKind::Git,
                    version: None,
                    url: Some(url.to_owned()),
                    rev: table
                        .get("rev")
                        .and_then(TomlValue::as_str)
                        .map(ToOwned::to_owned),
                });
            }

            table
                .get("version")
                .and_then(TomlValue::as_str)
                .map(|version| DetectedDependencySource {
                    kind: CargoPlanSourceKind::Version,
                    version: Some(version.to_owned()),
                    url: None,
                    rev: None,
                })
        }
        _ => None,
    }
}

fn source_matches_requirement(
    found: &DetectedDependencySource,
    required: &CargoPlanSource,
) -> bool {
    if found.kind == CargoPlanSourceKind::Version && required.kind == CargoPlanSourceKind::Version {
        return matches!(
            (found.version.as_deref(), required.version.as_deref()),
            (Some(found), Some(anchor)) if version_requirement_proves_compatibility(found, anchor)
        );
    }

    let found = CargoPlanSource {
        kind: found.kind,
        version: found.version.clone(),
        url: found.url.clone(),
        rev: found.rev.clone(),
    }
    .normalized();
    let required = required.normalized();
    matches!((found, required), (Ok(found), Ok(required)) if found == required)
}

fn version_requirement_proves_compatibility(requirement: &str, anchor: &str) -> bool {
    let Ok(requirement) = VersionReq::parse(requirement) else {
        return false;
    };
    let Ok(anchor) = Version::parse(anchor) else {
        return false;
    };
    if !requirement.matches(&anchor) || requirement.comparators.len() != 1 {
        return false;
    }

    let comparator = &requirement.comparators[0];
    matches!(
        comparator.op,
        SemverOp::Caret | SemverOp::Exact | SemverOp::Tilde
    ) && comparator.major == anchor.major
        && comparator.minor == Some(anchor.minor)
        && comparator.patch == Some(anchor.patch)
        && comparator.pre == anchor.pre
}

fn conflicting_features(crate_name: &str, features: &[String]) -> Vec<String> {
    if crate_name != "leptos" {
        return Vec::new();
    }
    features
        .iter()
        .filter(|feature| matches!(feature.as_str(), "hydrate" | "ssr" | "islands"))
        .cloned()
        .collect()
}

fn dependency_features(value: &TomlValue) -> Result<Vec<String>, ()> {
    match value {
        TomlValue::String(_) => Ok(Vec::new()),
        TomlValue::Table(table) => match table.get("features") {
            None => Ok(Vec::new()),
            Some(TomlValue::Array(features))
                if features.iter().all(|feature| feature.as_str().is_some()) =>
            {
                Ok(features
                    .iter()
                    .filter_map(TomlValue::as_str)
                    .map(ToOwned::to_owned)
                    .collect())
            }
            Some(_) => Err(()),
        },
        _ => Err(()),
    }
}

fn dependency_optional(value: &TomlValue) -> Result<bool, ()> {
    match value {
        TomlValue::String(_) => Ok(false),
        TomlValue::Table(table) => match table.get("optional") {
            None => Ok(false),
            Some(TomlValue::Boolean(optional)) => Ok(*optional),
            Some(_) => Err(()),
        },
        _ => Err(()),
    }
}

fn dependency_scalar_fields_are_valid(value: &TomlValue) -> bool {
    let Some(table) = value.as_table() else {
        return value.as_str().is_some();
    };
    table.iter().all(|(key, value)| match key.as_str() {
        "features" => value
            .as_array()
            .is_some_and(|values| values.iter().all(|value| value.as_str().is_some())),
        "optional" | "default-features" | "workspace" => value.as_bool().is_some(),
        "version" | "registry" | "git" | "rev" | "branch" | "tag" | "path" | "package" => {
            value.as_str().is_some()
        }
        _ => true,
    })
}

fn is_supported_dependency_key(key: &str) -> bool {
    matches!(
        key,
        "version" | "git" | "rev" | "features" | "optional" | "default-features" | "package"
    )
}

fn dependency_package(value: &TomlValue) -> Option<&str> {
    value
        .as_table()
        .and_then(|table| table.get("package"))
        .and_then(TomlValue::as_str)
}

fn renamed_dependency_key(table: Option<&TomlTable>, crate_name: &str) -> Option<String> {
    table?.iter().find_map(|(key, value)| {
        (key != crate_name && dependency_package(value) == Some(crate_name)).then(|| key.to_owned())
    })
}

fn target_has_dependency(targets: &TomlTable, crate_name: &str) -> bool {
    targets.values().any(|target| {
        let Some(target) = target.as_table() else {
            return false;
        };
        ["dependencies", "dev-dependencies", "build-dependencies"]
            .iter()
            .filter_map(|key| target.get(*key).and_then(TomlValue::as_table))
            .any(|dependencies| {
                dependencies.contains_key(crate_name)
                    || renamed_dependency_key(Some(dependencies), crate_name).is_some()
            })
    })
}

fn sorted_features(features: Vec<String>) -> Vec<String> {
    features
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn read_to_string(path: &Path) -> Result<String, DetectionError> {
    fs::read_to_string(path).map_err(|source| DetectionError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_path_with(
    path: &Path,
    read: &mut impl FnMut(&Path) -> Result<String, std::io::Error>,
) -> Result<String, DetectionError> {
    read(path).map_err(|source| DetectionError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::canonical_kit_json;
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
        fs::write(root.join("styles/kit.css"), ":root {}\n").expect("write css");
        fs::write(
            root.join("index.html"),
            r#"<!DOCTYPE html>
<html>
  <head>
    <link data-trunk rel="css" href="styles/kit.css" />
  </head>
  <body></body>
</html>
"#,
        )
        .expect("write html");
    }

    fn write_kit_config(root: &Path, config: impl AsRef<[u8]>) {
        let path = root.join(DEFAULT_KIT_CONFIG_PATH);
        fs::create_dir_all(path.parent().expect("kit config parent")).expect("create kit dir");
        fs::write(path, config).expect("write kit.json");
    }

    fn leptos_entry() -> CargoPlanEntry {
        CargoPlanEntry {
            crate_name: "leptos".to_owned(),
            source: CargoPlanSource::version(LEPTOS_VERSION),
            features: vec!["csr".to_owned()],
            required: true,
        }
    }

    fn requirement_for(manifest: &str) -> DependencyRequirement {
        let manifest = toml::from_str(manifest).expect("parse manifest");
        dependency_requirement_for_cargo_plan(&manifest, &leptos_entry())
    }

    #[test]
    fn detects_homepage_trunk_csr_project_shape() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"csr\"");

        let detected = detect_single_crate_project(root).expect("detect project");

        assert_eq!(detected.workspace_mode, WorkspaceMode::SingleCrate);
        assert_eq!(detected.source_root, root.join("src"));
        assert_eq!(detected.index_html_path, Some(root.join("index.html")));
        assert_eq!(detected.css_file_path, root.join("styles/kit.css"));
        assert_eq!(detected.render_mode, Some(RenderMode::Csr));
        assert_eq!(
            detected.dependency_plan.leptos.status,
            DependencyStatus::Satisfied
        );
        assert_eq!(detected.kit_config_path, None);
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
    fn detects_project_before_generated_stylesheet_link_exists() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"csr\"");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write html");

        let detected = detect_single_crate_project(root).expect("detect project");

        assert_eq!(detected.css_file_path, root.join(DEFAULT_CSS_PATH));
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
    fn dependency_requirement_reports_satisfied_version_dependency() {
        let manifest: TomlValue = toml::from_str(
            r#"[dependencies]
web_ui_primitives = { version = "0.1.0", features = ["leptos"] }
"#,
        )
        .expect("parse manifest");
        let entry = CargoPlanEntry {
            crate_name: "web_ui_primitives".to_owned(),
            source: CargoPlanSource::version("0.1.0"),
            features: vec!["leptos".to_owned()],
            required: true,
        };

        let requirement = dependency_requirement_for_cargo_plan(&manifest, &entry);

        assert_eq!(requirement.status, DependencyStatus::Satisfied);
    }

    #[test]
    fn dependency_requirement_uses_canonical_git_source_comparison() {
        let manifest: TomlValue = toml::from_str(
            r#"
[dependencies]
leptos = { git = "SSH://git@EXAMPLE.COM:22/Org/Repo/", rev = "ABCDEF0123456789ABCDEF0123456789ABCDEF01", features = ["csr"] }
"#,
        )
        .expect("parse manifest");
        let entry = CargoPlanEntry {
            crate_name: "leptos".to_owned(),
            source: CargoPlanSource::git(
                "ssh://git@example.com/Org/Repo",
                "abcdef0123456789abcdef0123456789abcdef01",
            ),
            features: vec!["csr".to_owned()],
            required: true,
        };

        let requirement = dependency_requirement_for_cargo_plan(&manifest, &entry);

        assert_eq!(requirement.status, DependencyStatus::Satisfied);
    }

    #[test]
    fn dependency_requirement_reports_missing_required_version_dependency() {
        let manifest: TomlValue = toml::from_str("[dependencies]\n").expect("parse manifest");
        let entry = CargoPlanEntry {
            crate_name: "web_ui_primitives".to_owned(),
            source: CargoPlanSource::version("0.1.0"),
            features: vec!["leptos".to_owned()],
            required: true,
        };

        let requirement = dependency_requirement_for_cargo_plan(&manifest, &entry);

        assert_eq!(requirement.status, DependencyStatus::Missing);
    }

    #[test]
    fn version_requirements_are_accepted_only_when_compatibility_is_proven() {
        for requirement in [
            "0.9.0-alpha",
            "^0.9.0-alpha",
            "=0.9.0-alpha",
            "~0.9.0-alpha",
        ] {
            let detected = requirement_for(&format!(
                r#"[dependencies]
leptos = {{ version = "{requirement}", features = ["csr"] }}
"#
            ));
            assert_eq!(
                detected.status,
                DependencyStatus::Satisfied,
                "{requirement}"
            );
            assert_eq!(detected.incompatibility, None, "{requirement}");
        }

        for requirement in [
            "0.9.*",
            ">=0.9.0-alpha",
            ">=0.9.0-alpha, <1.0.0",
            "0.8.0",
            "0.9.0-beta",
            "*",
        ] {
            let detected = requirement_for(&format!(
                r#"[dependencies]
leptos = {{ version = "{requirement}", features = ["csr"] }}
"#
            ));
            assert_eq!(
                detected.status,
                DependencyStatus::Incompatible,
                "{requirement}"
            );
            assert_eq!(
                detected.incompatibility,
                Some(DependencyIncompatibility::UnprovenVersionRequirement),
                "{requirement}"
            );
        }
    }

    #[test]
    fn same_manifest_workspace_dependency_merges_member_features() {
        let detected = requirement_for(
            r#"[workspace]

[workspace.dependencies]
leptos = { version = "0.9.0-alpha", features = ["tracing"] }

[dependencies]
leptos = { workspace = true, features = ["csr", "tracing"] }
"#,
        );

        assert_eq!(detected.status, DependencyStatus::Satisfied);
        assert_eq!(detected.features, ["csr", "tracing"]);
    }

    #[test]
    fn workspace_inheritance_fails_closed_for_missing_or_overridden_sources() {
        let missing = requirement_for(
            r#"[workspace]
[dependencies]
leptos = { workspace = true, features = ["csr"] }
"#,
        );
        assert_eq!(
            missing.incompatibility,
            Some(DependencyIncompatibility::MissingWorkspaceDeclaration)
        );

        let override_source = requirement_for(
            r#"[workspace]
[workspace.dependencies]
leptos = "0.9.0-alpha"
[dependencies]
leptos = { workspace = true, version = "0.9.0-alpha", features = ["csr"] }
"#,
        );
        assert_eq!(
            override_source.incompatibility,
            Some(DependencyIncompatibility::InvalidWorkspaceInheritance)
        );
    }

    #[test]
    fn optional_renamed_and_non_normal_dependencies_cannot_satisfy_codegen() {
        let cases = [
            (
                r#"[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"], optional = true }
"#,
                DependencyIncompatibility::Optional,
            ),
            (
                r#"[dependencies]
web = { package = "leptos", version = "0.9.0-alpha", features = ["csr"] }
"#,
                DependencyIncompatibility::Renamed {
                    dependency_key: "web".to_owned(),
                },
            ),
            (
                r#"[dev-dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
"#,
                DependencyIncompatibility::NonNormal {
                    declaration: DependencyDeclarationKind::Development,
                },
            ),
            (
                r#"[build-dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
"#,
                DependencyIncompatibility::NonNormal {
                    declaration: DependencyDeclarationKind::Build,
                },
            ),
            (
                r#"[target.'cfg(target_arch = "wasm32")'.dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
"#,
                DependencyIncompatibility::NonNormal {
                    declaration: DependencyDeclarationKind::Target,
                },
            ),
        ];

        for (manifest, incompatibility) in cases {
            let detected = requirement_for(manifest);
            assert_eq!(detected.status, DependencyStatus::Incompatible);
            assert_eq!(detected.incompatibility, Some(incompatibility));
        }
    }

    #[test]
    fn unsupported_path_and_inexact_git_sources_are_precise() {
        let path = requirement_for(
            r#"[dependencies]
leptos = { path = "../leptos", version = "0.9.0-alpha", features = ["csr"] }
"#,
        );
        assert_eq!(
            path.incompatibility,
            Some(DependencyIncompatibility::UnsupportedPathSource)
        );

        let git_without_rev = requirement_for(
            r#"[dependencies]
leptos = { git = "https://example.com/leptos", branch = "main", features = ["csr"] }
"#,
        );
        assert_eq!(
            git_without_rev.incompatibility,
            Some(DependencyIncompatibility::UnsupportedGitSource)
        );
    }

    #[test]
    fn malformed_dependency_fields_fail_closed() {
        for manifest in [
            r#"[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"], optional = "false" }
"#,
            r#"[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr", 42] }
"#,
            r#"[dependencies]
leptos = { version = "0.9.0-alpha", features = "csr" }
"#,
            r#"[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"], default-features = "false" }
"#,
        ] {
            let detected = requirement_for(manifest);
            assert_eq!(detected.status, DependencyStatus::Incompatible);
            assert_eq!(
                detected.incompatibility,
                Some(DependencyIncompatibility::InvalidDeclaration),
                "{manifest}"
            );
        }
    }

    #[test]
    fn custom_registries_and_mixed_git_selectors_fail_closed() {
        let registry = requirement_for(
            r#"[dependencies]
leptos = { version = "0.9.0-alpha", registry = "private", features = ["csr"] }
"#,
        );
        assert_eq!(
            registry.incompatibility,
            Some(DependencyIncompatibility::UnsupportedRegistrySource)
        );

        for selector in [
            r#"branch = "main""#,
            r#"tag = "v0.9.0""#,
            r#"branch = "main", tag = "v0.9.0""#,
        ] {
            let detected = requirement_for(&format!(
                r#"[dependencies]
leptos = {{ git = "https://example.com/leptos", rev = "abcdef0123456789abcdef0123456789abcdef01", {selector}, features = ["csr"] }}
"#
            ));
            assert_eq!(
                detected.incompatibility,
                Some(DependencyIncompatibility::UnsupportedGitSource),
                "{selector}"
            );
        }
    }

    #[test]
    fn conflicting_normal_dependency_declarations_fail_closed() {
        let target = requirement_for(
            r#"[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }

[target.'cfg(target_arch = "wasm32")'.dependencies]
leptos = { version = "0.9.0-alpha", features = ["ssr"] }
"#,
        );
        assert_eq!(
            target.incompatibility,
            Some(DependencyIncompatibility::NonNormal {
                declaration: DependencyDeclarationKind::Target,
            })
        );

        let renamed = requirement_for(
            r#"[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
web = { package = "leptos", version = "0.9.0-alpha" }
"#,
        );
        assert_eq!(
            renamed.incompatibility,
            Some(DependencyIncompatibility::Renamed {
                dependency_key: "web".to_owned(),
            })
        );
    }

    #[test]
    fn missing_and_conflicting_csr_features_are_distinct() {
        let missing = requirement_for(
            r#"[dependencies]
leptos = "0.9.0-alpha"
"#,
        );
        assert_eq!(
            missing.incompatibility,
            Some(DependencyIncompatibility::MissingFeatures {
                features: vec!["csr".to_owned()],
            })
        );

        let conflicting = requirement_for(
            r#"[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr", "hydrate", "ssr", "islands"] }
"#,
        );
        assert_eq!(
            conflicting.incompatibility,
            Some(DependencyIncompatibility::ConflictingFeatures {
                features: vec!["hydrate".to_owned(), "islands".to_owned(), "ssr".to_owned(),],
            })
        );
    }

    #[test]
    fn dependency_detection_does_not_mutate_consumer_manifest_or_lock() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("Cargo.toml");
        let lock_path = dir.path().join("Cargo.lock");
        let manifest = r#"[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
"#;
        let lock = "# intentionally opaque consumer lock\n";
        fs::write(&manifest_path, manifest).expect("write manifest");
        fs::write(&lock_path, lock).expect("write lock");

        let requirements =
            detect_cargo_plan_requirements(dir.path(), &[leptos_entry()]).expect("detect");

        assert_eq!(requirements[0].status, DependencyStatus::Satisfied);
        assert_eq!(fs::read_to_string(manifest_path).unwrap(), manifest);
        assert_eq!(fs::read_to_string(lock_path).unwrap(), lock);
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
    fn info_output_normalizes_kit_config_when_present() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"csr\"");
        write_kit_config(root, canonical_kit_json().expect("canonical config"));

        let info = build_info_output(root).expect("build info output");

        assert!(info.kit_config.is_some());
        let normalized = info.normalized_config.expect("normalized config");
        assert_eq!(normalized.render_mode, RenderMode::Csr);
        assert_eq!(
            normalized.install_roots.css_file,
            root.join("styles/kit.css")
        );
    }

    #[test]
    fn info_output_uses_configured_css_path_when_present() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        write_homepage_fixture(root, "\"csr\"");
        let config = canonical_kit_json().expect("canonical config").replace(
            "\"css\": \"styles/kit.css\"",
            "\"css\": \"styles/custom.css\"",
        );
        write_kit_config(root, config);

        let info = build_info_output(root).expect("build info output");

        assert_eq!(info.detected.css_file_path, root.join("styles/custom.css"));
        assert_eq!(
            info.normalized_config
                .expect("normalized config")
                .install_roots
                .css_file,
            root.join("styles/custom.css")
        );
    }

    #[test]
    fn info_uses_one_config_snapshot_and_preserves_detected_workspace_mode() {
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
"#,
        )
        .expect("write manifest");
        write_kit_config(root, canonical_kit_json().expect("canonical config"));
        let kit_path = root.join(DEFAULT_KIT_CONFIG_PATH);
        let mut kit_reads = 0;

        let info = build_info_output_with(root, |path| {
            if path == kit_path {
                kit_reads += 1;
            }
            fs::read_to_string(path)
        })
        .expect("build info");

        assert_eq!(kit_reads, 1);
        assert_eq!(
            info.detected.workspace_mode,
            WorkspaceMode::SinglePackageWorkspaceRoot
        );
        assert_eq!(
            info.normalized_config.unwrap().workspace_mode,
            WorkspaceMode::SinglePackageWorkspaceRoot
        );
    }
}
