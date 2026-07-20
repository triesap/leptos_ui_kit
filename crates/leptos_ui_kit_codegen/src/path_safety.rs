use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
#[cfg(unix)]
use cap_std::fs::DirBuilder;
use cap_std::{
    ambient_authority,
    fs::{Dir, File, Metadata, OpenOptions},
};

use leptos_ui_kit_registry::DEFAULT_KIT_CONFIG_PATH;

use crate::transaction::DEFAULT_KIT_COORDINATION_IGNORE_PATH;
use crate::transaction::{
    ExactObjectIdentity, opened_directory_identity, opened_regular_file_identity,
};
use crate::{
    CodegenError, DEFAULT_KIT_LOCK_PATH, DEFAULT_KIT_WRITE_LOCK_PATH, THEME_CAPABILITY_PATH,
    TOKEN_CONTRACT_PATH, hash_content_bytes,
};

/// The permission state retained in an exact regular-file preimage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreservedFileMode {
    pub readonly: bool,
    pub posix_mode: Option<u32>,
}

/// The exact supported state observed for a mutable project path.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathPreimage {
    Absent,
    RegularFile {
        content_hash: String,
        mode: PreservedFileMode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectRootIdentity {
    requested_root: PathBuf,
    canonical_root: PathBuf,
    identity: ExactObjectIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryIdentity {
    identity: ExactObjectIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExactRegularFileObservation {
    identity: ExactObjectIdentity,
    byte_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotObservation {
    preimage: PathPreimage,
    regular_file: Option<ExactRegularFileObservation>,
}

/// A point-in-time, read-only record of every mutable project observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSnapshot {
    root: ProjectRootIdentity,
    observations: BTreeMap<String, SnapshotObservation>,
    directories: BTreeMap<String, DirectoryIdentity>,
}

impl PlanSnapshot {
    pub fn preimage(&self, logical_path: &str) -> Option<&PathPreimage> {
        self.observations
            .get(logical_path)
            .map(|observation| &observation.preimage)
    }

    pub fn observations(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, &PathPreimage)> + DoubleEndedIterator + '_ {
        self.observations
            .iter()
            .map(|(path, observation)| (path.as_str(), &observation.preimage))
    }

    pub fn len(&self) -> usize {
        self.observations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }

    pub(crate) fn directory_identity(&self, logical_path: &str) -> Option<ExactObjectIdentity> {
        if logical_path.is_empty() {
            return Some(self.root.identity);
        }
        self.directories
            .get(logical_path)
            .map(|identity| identity.identity)
    }

    pub(crate) fn revalidate_all(&self, context: &PlanningContext) -> Result<(), CodegenError> {
        self.validate_context_root(context)?;
        for (logical_path, expected) in &self.observations {
            let actual = context.inspect_uncached(logical_path).map_err(|error| {
                CodegenError::PreimageConflict {
                    path: logical_path.clone(),
                    reason: error.to_string(),
                }
            })?;
            if actual.preimage != expected.preimage {
                return Err(preimage_conflict(
                    logical_path,
                    &expected.preimage,
                    &actual.preimage,
                ));
            }
            if actual.regular_file != expected.regular_file {
                return Err(exact_regular_file_conflict(
                    logical_path,
                    expected.regular_file,
                    actual.regular_file,
                ));
            }
        }
        context.revalidate_directories(&self.directories, None)?;
        Ok(())
    }

    pub(crate) fn revalidate_path(
        &self,
        context: &PlanningContext,
        logical_path: &str,
    ) -> Result<(), CodegenError> {
        self.validate_context_root(context)?;
        let expected =
            self.observations
                .get(logical_path)
                .ok_or_else(|| CodegenError::PreimageConflict {
                    path: logical_path.to_owned(),
                    reason: "planned target has no recorded preimage".to_owned(),
                })?;
        let actual = context.inspect_uncached(logical_path).map_err(|error| {
            CodegenError::PreimageConflict {
                path: logical_path.to_owned(),
                reason: error.to_string(),
            }
        })?;
        if actual.preimage != expected.preimage {
            return Err(preimage_conflict(
                logical_path,
                &expected.preimage,
                &actual.preimage,
            ));
        }
        if actual.regular_file != expected.regular_file {
            return Err(exact_regular_file_conflict(
                logical_path,
                expected.regular_file,
                actual.regular_file,
            ));
        }
        context.revalidate_directories(&self.directories, Some(logical_path))?;
        Ok(())
    }

    fn validate_context_root(&self, context: &PlanningContext) -> Result<(), CodegenError> {
        context.revalidate_project_root_identity()?;
        let metadata = context
            .dir
            .dir_metadata()
            .map_err(|source| CodegenError::Io {
                path: context.root.canonical_root.clone(),
                source,
            })?;
        ensure_directory_metadata(".", &metadata)?;
        if opened_directory_identity(&context.dir).map_err(|source| CodegenError::Io {
            path: context.root.canonical_root.clone(),
            source,
        })? != self.root.identity
        {
            return Err(CodegenError::ProjectRootChanged {
                path: context.root.canonical_root.clone(),
                reason: "held project-root capability no longer has the planned identity"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct CachedObservation {
    preimage: PathPreimage,
    bytes: Option<Vec<u8>>,
    regular_file: Option<ExactRegularFileObservation>,
}

pub(crate) struct PlanningContext {
    root: ProjectRootIdentity,
    dir: Dir,
    observations: RefCell<BTreeMap<String, CachedObservation>>,
    directories: RefCell<BTreeMap<String, DirectoryIdentity>>,
}

impl PlanningContext {
    pub(crate) fn open(project_root: &Path) -> Result<Self, CodegenError> {
        let root = open_project_root(project_root).map_err(|source| CodegenError::Io {
            path: project_root.to_path_buf(),
            source,
        })?;
        let dir =
            Dir::open_ambient_dir(&root.canonical_root, ambient_authority()).map_err(|source| {
                CodegenError::Io {
                    path: root.canonical_root.clone(),
                    source,
                }
            })?;
        let metadata = dir.dir_metadata().map_err(|source| CodegenError::Io {
            path: root.canonical_root.clone(),
            source,
        })?;
        ensure_directory_metadata(".", &metadata)?;
        let actual_identity =
            opened_directory_identity(&dir).map_err(|source| CodegenError::Io {
                path: root.canonical_root.clone(),
                source,
            })?;
        if actual_identity != root.identity {
            return Err(CodegenError::ProjectRootChanged {
                path: project_root.to_path_buf(),
                reason: "project root changed while its capability was opened".to_owned(),
            });
        }

        Ok(Self {
            root,
            dir,
            observations: RefCell::new(BTreeMap::new()),
            directories: RefCell::new(BTreeMap::new()),
        })
    }

    pub(crate) fn project_root(&self) -> &Path {
        &self.root.canonical_root
    }

    pub(crate) fn project_identity(&self) -> (&Path, ExactObjectIdentity) {
        (&self.root.canonical_root, self.root.identity)
    }

    pub(crate) fn open_pinned_project_root(&self) -> Result<Dir, CodegenError> {
        let directory = Dir::open_ambient_dir(&self.root.canonical_root, ambient_authority())
            .map_err(|source| CodegenError::Io {
                path: self.root.canonical_root.clone(),
                source,
            })?;
        self.validate_project_root_directory(&directory)?;
        Ok(directory)
    }

    pub(crate) fn revalidate_project_root_identity(&self) -> Result<(), CodegenError> {
        let current_alias = open_project_root(&self.root.requested_root).map_err(|error| {
            CodegenError::ProjectRootChanged {
                path: self.root.requested_root.clone(),
                reason: error.to_string(),
            }
        })?;
        if !same_root(&current_alias, &self.root) {
            return Err(CodegenError::ProjectRootChanged {
                path: self.root.requested_root.clone(),
                reason: format!(
                    "expected {}, but the caller's project-root alias now identifies {}",
                    self.root.canonical_root.display(),
                    current_alias.canonical_root.display()
                ),
            });
        }
        let current = self.open_pinned_project_root()?;
        let held = self.dir.dir_metadata().map_err(|source| CodegenError::Io {
            path: self.root.canonical_root.clone(),
            source,
        })?;
        ensure_directory_metadata(".", &held)?;
        if opened_directory_identity(&self.dir).map_err(|source| CodegenError::Io {
            path: self.root.canonical_root.clone(),
            source,
        })? != self.root.identity
        {
            return Err(CodegenError::ProjectRootChanged {
                path: self.root.canonical_root.clone(),
                reason: "held project-root capability no longer has the opened identity".to_owned(),
            });
        }
        drop(current);
        Ok(())
    }

    fn validate_project_root_directory(&self, directory: &Dir) -> Result<(), CodegenError> {
        let metadata = directory
            .dir_metadata()
            .map_err(|source| CodegenError::Io {
                path: self.root.canonical_root.clone(),
                source,
            })?;
        ensure_directory_metadata(".", &metadata)?;
        if opened_directory_identity(directory).map_err(|source| CodegenError::Io {
            path: self.root.canonical_root.clone(),
            source,
        })? != self.root.identity
        {
            return Err(CodegenError::ProjectRootChanged {
                path: self.root.canonical_root.clone(),
                reason: "project root path no longer resolves to the opened identity".to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn read_optional_string(
        &self,
        logical_path: &str,
    ) -> Result<Option<String>, CodegenError> {
        let observation = self.observe(logical_path)?;
        observation
            .bytes
            .map(|bytes| {
                String::from_utf8(bytes).map_err(|source| CodegenError::Io {
                    path: self.root.canonical_root.join(logical_path),
                    source: io::Error::new(io::ErrorKind::InvalidData, source),
                })
            })
            .transpose()
    }

    pub(crate) fn read_string(&self, logical_path: &str) -> Result<String, CodegenError> {
        self.read_optional_string(logical_path)?
            .ok_or_else(|| CodegenError::Io {
                path: self.root.canonical_root.join(logical_path),
                source: io::Error::new(io::ErrorKind::NotFound, "project file is missing"),
            })
    }

    pub(crate) fn observe_path(&self, logical_path: &str) -> Result<PathPreimage, CodegenError> {
        Ok(self.observe(logical_path)?.preimage)
    }

    pub(crate) fn finish_snapshot(&self) -> PlanSnapshot {
        PlanSnapshot {
            root: self.root.clone(),
            observations: self
                .observations
                .borrow()
                .iter()
                .map(|(path, observation)| {
                    (
                        path.clone(),
                        SnapshotObservation {
                            preimage: observation.preimage.clone(),
                            regular_file: observation.regular_file,
                        },
                    )
                })
                .collect(),
            directories: self.directories.borrow().clone(),
        }
    }

    pub(crate) fn ensure_parent_with<F>(
        &self,
        logical_path: &str,
        mut create_event: F,
    ) -> Result<Vec<String>, CodegenError>
    where
        F: FnMut(&str, bool) -> Result<(), CodegenError>,
    {
        let mut created = Vec::new();
        self.walk_parent_with(logical_path, true, true, &mut create_event, &mut created)?;
        Ok(created)
    }

    pub(crate) fn open_parent(&self, logical_path: &str) -> Result<(Dir, String), CodegenError> {
        self.walk_parent(logical_path, false)?
            .ok_or_else(|| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    "controlled parent directory is missing",
                ),
            })
    }

    pub(crate) fn open_directory(&self, logical_path: &str) -> Result<Dir, CodegenError> {
        if logical_path.is_empty() {
            return Dir::reopen_dir(&self.dir).map_err(|source| CodegenError::Io {
                path: self.project_root().to_path_buf(),
                source,
            });
        }
        validate_controlled_relative_path(logical_path)?;
        let probe = format!("{logical_path}/directory-probe");
        self.walk_parent(&probe, false)?
            .map(|(directory, _)| directory)
            .ok_or_else(|| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source: io::Error::new(io::ErrorKind::NotFound, "directory is missing"),
            })
    }

    /// Freshly opens a logical directory and proves that it still has the
    /// exact identity and mode recorded by the transaction journal.
    pub(crate) fn reopen_exact_directory(
        &self,
        logical_path: &str,
        expected_identity: ExactObjectIdentity,
        expected_mode: PreservedFileMode,
    ) -> Result<Dir, CodegenError> {
        self.revalidate_project_root_identity()?;
        let directory = if logical_path.is_empty() {
            self.open_pinned_project_root()?
        } else {
            self.open_directory(logical_path)?
        };
        let metadata = directory
            .dir_metadata()
            .map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?;
        ensure_directory_metadata(logical_path, &metadata)?;

        let actual_identity =
            opened_directory_identity(&directory).map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?;
        if actual_identity != expected_identity {
            return Err(CodegenError::PreimageConflict {
                path: logical_path.to_owned(),
                reason: format!(
                    "controlled directory identity changed before mutation: expected {expected_identity:?}, found {actual_identity:?}"
                ),
            });
        }

        let actual_mode = preserved_mode(&metadata);
        if actual_mode != expected_mode {
            return Err(CodegenError::PreimageConflict {
                path: logical_path.to_owned(),
                reason: format!(
                    "controlled directory mode changed before mutation: expected {expected_mode:?}, found {actual_mode:?}"
                ),
            });
        }

        Ok(directory)
    }

    pub(crate) fn open_auxiliary_file(
        &self,
        logical_path: &str,
        write: bool,
    ) -> Result<File, CodegenError> {
        validate_controlled_relative_path(logical_path)?;
        let (parent, file_name) = self.open_parent(logical_path)?;
        let metadata = parent
            .symlink_metadata(&file_name)
            .map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?;
        ensure_regular_file_metadata(logical_path, &metadata)?;

        let mut options = OpenOptions::new();
        options.read(true).write(write);
        options.follow(FollowSymlinks::No);
        options.nonblock(true);
        let file = parent
            .open_with(&file_name, &options)
            .map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?;
        let opened = file.metadata().map_err(|source| CodegenError::Io {
            path: self.project_root().join(logical_path),
            source,
        })?;
        ensure_regular_file_metadata(logical_path, &opened)?;
        let opened_identity =
            opened_regular_file_identity(&file).map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?;
        let current =
            parent
                .open_with(&file_name, &options)
                .map_err(|source| CodegenError::Io {
                    path: self.project_root().join(logical_path),
                    source,
                })?;
        if opened_identity
            != opened_regular_file_identity(&current).map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?
        {
            return unsafe_path(
                logical_path,
                "controlled file changed while it was opened without following",
            );
        }
        Ok(file)
    }

    pub(crate) fn revalidate_auxiliary_file(
        &self,
        logical_path: &str,
        file: &File,
    ) -> Result<(), CodegenError> {
        let held = file.metadata().map_err(|source| CodegenError::Io {
            path: self.project_root().join(logical_path),
            source,
        })?;
        ensure_regular_file_metadata(logical_path, &held)?;
        let (parent, file_name) = self.open_parent(logical_path)?;
        let current = parent
            .symlink_metadata(&file_name)
            .map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?;
        ensure_regular_file_metadata(logical_path, &current)?;
        let held_identity =
            opened_regular_file_identity(file).map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?;
        let current_file = self.open_auxiliary_file(logical_path, false)?;
        if held_identity
            != opened_regular_file_identity(&current_file).map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?
        {
            return unsafe_path(
                logical_path,
                "controlled file identity changed after it was opened",
            );
        }
        Ok(())
    }

    pub(crate) fn revalidate_auxiliary_identity(
        &self,
        logical_path: &str,
        identity: ExactObjectIdentity,
    ) -> Result<(), CodegenError> {
        let (parent, file_name) = self.open_parent(logical_path)?;
        let current = parent
            .symlink_metadata(&file_name)
            .map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?;
        ensure_regular_file_metadata(logical_path, &current)?;
        let current_file = self.open_auxiliary_file(logical_path, false)?;
        if opened_regular_file_identity(&current_file).map_err(|source| CodegenError::Io {
            path: self.project_root().join(logical_path),
            source,
        })? != identity
        {
            return unsafe_path(
                logical_path,
                "controlled file identity changed after it was opened",
            );
        }
        Ok(())
    }

    pub(crate) fn ensure_same_directory(
        &self,
        logical_path: &str,
        staged: &Dir,
        current: &Dir,
    ) -> Result<(), CodegenError> {
        let staged_identity =
            opened_directory_identity(staged).map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?;
        let current_identity =
            opened_directory_identity(current).map_err(|source| CodegenError::Io {
                path: self.project_root().join(logical_path),
                source,
            })?;
        let staged = staged.dir_metadata().map_err(|source| CodegenError::Io {
            path: self.project_root().join(logical_path),
            source,
        })?;
        let current = current.dir_metadata().map_err(|source| CodegenError::Io {
            path: self.project_root().join(logical_path),
            source,
        })?;
        ensure_directory_metadata(logical_path, &staged)?;
        ensure_directory_metadata(logical_path, &current)?;
        if staged_identity != current_identity {
            return Err(CodegenError::PreimageConflict {
                path: logical_path.to_owned(),
                reason: "controlled parent changed between staging and commit".to_owned(),
            });
        }
        Ok(())
    }

    fn observe(&self, logical_path: &str) -> Result<CachedObservation, CodegenError> {
        if let Some(observation) = self.observations.borrow().get(logical_path) {
            return Ok(observation.clone());
        }
        let observation = self.inspect_uncached(logical_path)?;
        self.observations
            .borrow_mut()
            .insert(logical_path.to_owned(), observation.clone());
        Ok(observation)
    }

    fn inspect_uncached(&self, logical_path: &str) -> Result<CachedObservation, CodegenError> {
        validate_logical_write_path(logical_path)?;
        let Some((parent, file_name)) = self.walk_parent(logical_path, false)? else {
            return Ok(CachedObservation {
                preimage: PathPreimage::Absent,
                bytes: None,
                regular_file: None,
            });
        };

        let metadata = match parent.symlink_metadata(&file_name) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(CachedObservation {
                    preimage: PathPreimage::Absent,
                    bytes: None,
                    regular_file: None,
                });
            }
            Err(source) => {
                return Err(CodegenError::Io {
                    path: self.root.canonical_root.join(logical_path),
                    source,
                });
            }
        };
        ensure_regular_file_metadata(logical_path, &metadata)?;

        let mut options = OpenOptions::new();
        options.read(true);
        options.follow(FollowSymlinks::No);
        options.nonblock(true);
        let mut file =
            parent
                .open_with(&file_name, &options)
                .map_err(|source| CodegenError::Io {
                    path: self.root.canonical_root.join(logical_path),
                    source,
                })?;
        let opened_metadata = file.metadata().map_err(|source| CodegenError::Io {
            path: self.root.canonical_root.join(logical_path),
            source,
        })?;
        ensure_regular_file_metadata(logical_path, &opened_metadata)?;
        if metadata_identity(&metadata) != metadata_identity(&opened_metadata) {
            return unsafe_path(
                logical_path,
                "file changed while it was opened without following",
            );
        }
        let opened_identity =
            opened_regular_file_identity(&file).map_err(|source| CodegenError::Io {
                path: self.root.canonical_root.join(logical_path),
                source,
            })?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| CodegenError::Io {
                path: self.root.canonical_root.join(logical_path),
                source,
            })?;
        Ok(CachedObservation {
            preimage: PathPreimage::RegularFile {
                content_hash: hash_content_bytes(&bytes),
                mode: preserved_mode(&opened_metadata),
            },
            bytes: Some(bytes),
            regular_file: Some(ExactRegularFileObservation {
                identity: opened_identity,
                byte_len: opened_metadata.len(),
            }),
        })
    }

    fn validate_target_metadata(&self, logical_path: &str) -> Result<(), CodegenError> {
        validate_logical_write_path(logical_path)?;
        let Some((parent, file_name)) = self.walk_parent(logical_path, false)? else {
            return Ok(());
        };
        match parent.symlink_metadata(&file_name) {
            Ok(metadata) => ensure_regular_file_metadata(logical_path, &metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CodegenError::Io {
                path: self.root.canonical_root.join(logical_path),
                source,
            }),
        }
    }

    fn walk_parent(
        &self,
        logical_path: &str,
        create_missing: bool,
    ) -> Result<Option<(Dir, String)>, CodegenError> {
        let mut created = Vec::new();
        self.walk_parent_with(
            logical_path,
            create_missing,
            false,
            &mut |_, _| Ok(()),
            &mut created,
        )
    }

    fn walk_parent_with<F>(
        &self,
        logical_path: &str,
        create_missing: bool,
        recover_coordination_mode: bool,
        create_event: &mut F,
        created: &mut Vec<String>,
    ) -> Result<Option<(Dir, String)>, CodegenError>
    where
        F: FnMut(&str, bool) -> Result<(), CodegenError>,
    {
        validate_controlled_relative_path(logical_path)?;
        let components = Path::new(logical_path)
            .components()
            .map(|component| match component {
                Component::Normal(value) => value.to_owned(),
                _ => unreachable!("validated relative path"),
            })
            .collect::<Vec<_>>();
        let (file_name, parents) = components
            .split_last()
            .expect("validated path has at least one component");
        let mut current = Dir::reopen_dir(&self.dir).map_err(|source| CodegenError::Io {
            path: self.root.canonical_root.clone(),
            source,
        })?;
        let mut relative = PathBuf::new();

        for component in parents {
            relative.push(component);
            let mut metadata = match current.symlink_metadata(component) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound && create_missing => {
                    let relative_string = relative.to_string_lossy().into_owned();
                    let parent_path = relative.parent().unwrap_or_else(|| Path::new(""));
                    self.revalidate_held_directory_path(parent_path, &current)?;
                    create_event(&relative_string, false)?;
                    self.revalidate_held_directory_path(parent_path, &current)?;
                    match create_controlled_directory(&current, component, relative_string.as_str())
                    {
                        Ok(()) => {
                            created.push(relative_string.clone());
                            create_event(&relative_string, true)?;
                            self.revalidate_held_directory_path(parent_path, &current)?;
                        }
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(source) => {
                            return Err(CodegenError::Io {
                                path: self.root.canonical_root.join(&relative),
                                source,
                            });
                        }
                    }
                    current
                        .symlink_metadata(component)
                        .map_err(|source| CodegenError::Io {
                            path: self.root.canonical_root.join(&relative),
                            source,
                        })?
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(source) => {
                    return Err(CodegenError::Io {
                        path: self.root.canonical_root.join(&relative),
                        source,
                    });
                }
            };
            ensure_directory_metadata(relative.to_string_lossy().as_ref(), &metadata)?;
            if recover_coordination_mode {
                recover_restrictive_kit_directory_mode(
                    &current,
                    component,
                    relative.to_string_lossy().as_ref(),
                    &mut metadata,
                )?;
            }
            current = current.open_dir_nofollow(component).map_err(|source| {
                CodegenError::UnsafePath {
                    path: relative.to_string_lossy().into_owned(),
                    reason: format!("failed to open directory without following: {source}"),
                }
            })?;
            let opened = current.dir_metadata().map_err(|source| CodegenError::Io {
                path: self.root.canonical_root.join(&relative),
                source,
            })?;
            ensure_directory_metadata(relative.to_string_lossy().as_ref(), &opened)?;
            if metadata_identity(&metadata) != metadata_identity(&opened) {
                return unsafe_path(
                    relative.to_string_lossy().as_ref(),
                    "directory changed while it was opened without following",
                );
            }
            let identity =
                opened_directory_identity(&current).map_err(|source| CodegenError::Io {
                    path: self.root.canonical_root.join(&relative),
                    source,
                })?;
            self.record_directory_identity(
                relative.to_string_lossy().into_owned(),
                DirectoryIdentity { identity },
            )?;
        }

        Ok(Some((current, file_name.to_string_lossy().into_owned())))
    }

    fn revalidate_held_directory_path(
        &self,
        logical_path: &Path,
        held: &Dir,
    ) -> Result<(), CodegenError> {
        self.revalidate_project_root_identity()?;
        let logical_path = logical_path.to_string_lossy();
        let current = if logical_path.is_empty() {
            Dir::reopen_dir(&self.dir).map_err(|source| CodegenError::Io {
                path: self.project_root().to_path_buf(),
                source,
            })?
        } else {
            self.open_directory(logical_path.as_ref())?
        };
        self.ensure_same_directory(logical_path.as_ref(), held, &current)
    }

    fn record_directory_identity(
        &self,
        path: String,
        identity: DirectoryIdentity,
    ) -> Result<(), CodegenError> {
        use std::collections::btree_map::Entry;

        match self.directories.borrow_mut().entry(path.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(identity);
                Ok(())
            }
            Entry::Occupied(entry) if entry.get() == &identity => Ok(()),
            Entry::Occupied(_) => Err(CodegenError::PreimageConflict {
                path,
                reason: "controlled parent identity changed during operation".to_owned(),
            }),
        }
    }

    fn revalidate_directories(
        &self,
        expected: &BTreeMap<String, DirectoryIdentity>,
        logical_path: Option<&str>,
    ) -> Result<(), CodegenError> {
        for (directory, expected_identity) in expected {
            if logical_path.is_some_and(|path| !is_path_prefix(directory, path)) {
                continue;
            }
            let current =
                self.open_directory(directory)
                    .map_err(|error| CodegenError::PreimageConflict {
                        path: directory.clone(),
                        reason: format!("controlled parent changed after planning: {error}"),
                    })?;
            let metadata =
                current
                    .dir_metadata()
                    .map_err(|source| CodegenError::PreimageConflict {
                        path: directory.clone(),
                        reason: format!(
                            "failed to inspect controlled parent after planning: {source}"
                        ),
                    })?;
            ensure_directory_metadata(directory, &metadata).map_err(|error| {
                CodegenError::PreimageConflict {
                    path: directory.clone(),
                    reason: error.to_string(),
                }
            })?;
            if metadata_identity(&metadata) != expected_identity.identity {
                return Err(CodegenError::PreimageConflict {
                    path: directory.clone(),
                    reason: "controlled parent identity changed after planning".to_owned(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn recover_restrictive_kit_directory_mode(
    parent: &Dir,
    name: &OsStr,
    logical_path: &str,
    metadata: &mut Metadata,
) -> Result<(), CodegenError> {
    use cap_std::fs::{Permissions, PermissionsExt};

    if logical_path != "src/components/ui/_kit" {
        return Ok(());
    }
    let mode = metadata.permissions().mode() & 0o7777;
    if mode == 0o700 || mode & !0o700 != 0 {
        return Ok(());
    }
    let identity = metadata_identity(metadata);
    parent
        .set_symlink_permissions(name, Permissions::from_mode(0o700))
        .map_err(|source| CodegenError::Io {
            path: PathBuf::from(logical_path),
            source,
        })?;
    let current = parent
        .symlink_metadata(name)
        .map_err(|source| CodegenError::Io {
            path: PathBuf::from(logical_path),
            source,
        })?;
    ensure_directory_metadata(logical_path, &current)?;
    if metadata_identity(&current) != identity {
        return unsafe_path(
            logical_path,
            "coordination directory changed while recovering its private mode",
        );
    }
    *metadata = current;
    Ok(())
}

#[cfg(not(unix))]
fn recover_restrictive_kit_directory_mode(
    _parent: &Dir,
    _name: &OsStr,
    _logical_path: &str,
    _metadata: &mut Metadata,
) -> Result<(), CodegenError> {
    Ok(())
}

#[cfg(unix)]
fn create_controlled_directory(parent: &Dir, name: &OsStr, logical_path: &str) -> io::Result<()> {
    use cap_std::fs::DirBuilderExt;

    if logical_path == "src/components/ui/_kit" {
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        parent.create_dir_with(name, &builder)
    } else {
        parent.create_dir(name)
    }
}

#[cfg(not(unix))]
fn create_controlled_directory(parent: &Dir, name: &OsStr, _logical_path: &str) -> io::Result<()> {
    parent.create_dir(name)
}

pub fn validate_planned_write_paths(paths: &[String]) -> Result<(), CodegenError> {
    let mut seen = BTreeSet::new();
    for path in paths {
        validate_logical_write_path(path)?;
        let folded = path.to_ascii_lowercase();
        if !seen.insert(folded) {
            return Err(CodegenError::DuplicatePath(path.clone()));
        }
    }
    Ok(())
}

pub fn validate_project_write_path(
    project_root: &Path,
    logical_path: &str,
) -> Result<PathBuf, CodegenError> {
    let context = PlanningContext::open(project_root)?;
    context.validate_target_metadata(logical_path)?;
    Ok(project_root.join(logical_path))
}

pub fn validate_logical_write_path(path: &str) -> Result<(), CodegenError> {
    validate_relative_path(path)?;
    if is_allowed_write_path(path) {
        Ok(())
    } else {
        unsafe_path(path, "path is outside the MVP write allow-list")
    }
}

#[cfg(test)]
pub(crate) fn capture_plan_snapshot(
    project_root: &Path,
    logical_paths: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<PlanSnapshot, CodegenError> {
    let context = PlanningContext::open(project_root)?;
    for logical_path in logical_paths {
        context.observe_path(logical_path.as_ref())?;
    }
    Ok(context.finish_snapshot())
}

fn open_project_root(project_root: &Path) -> io::Result<ProjectRootIdentity> {
    let requested_root = if project_root.is_absolute() {
        project_root.to_path_buf()
    } else {
        std::env::current_dir()?.join(project_root)
    };
    let canonical_root = fs::canonicalize(&requested_root)?;
    let dir = Dir::open_ambient_dir(&canonical_root, ambient_authority())?;
    let metadata = dir.dir_metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "project root is not a directory",
        ));
    }
    let identity = opened_directory_identity(&dir)?;
    Ok(ProjectRootIdentity {
        requested_root,
        canonical_root,
        identity,
    })
}

fn same_root(actual: &ProjectRootIdentity, expected: &ProjectRootIdentity) -> bool {
    actual.canonical_root == expected.canonical_root && actual.identity == expected.identity
}

fn is_path_prefix(prefix: &str, path: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn validate_relative_path(path: &str) -> Result<(), CodegenError> {
    if path.is_empty() {
        return unsafe_path(path, "path is empty");
    }
    if path.starts_with('/')
        || path.starts_with("//")
        || path.starts_with("\\\\")
        || path.as_bytes().get(1) == Some(&b':')
    {
        return unsafe_path(path, "absolute paths and platform prefixes are rejected");
    }
    if !path.is_ascii() {
        return unsafe_path(path, "path must be ASCII");
    }
    if path.contains('\\') {
        return unsafe_path(path, "backslashes are rejected");
    }

    for component in path.split('/') {
        if component.is_empty() || component == "." {
            return unsafe_path(path, "empty or current-dir segments are rejected");
        }
        if component == ".." {
            return unsafe_path(path, "parent traversal is rejected");
        }
        if component.starts_with('.') {
            return unsafe_path(path, "hidden paths are rejected");
        }
        if !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return unsafe_path(path, "file name contains unsafe characters");
        }
    }
    Ok(())
}

fn validate_controlled_relative_path(path: &str) -> Result<(), CodegenError> {
    const TRANSACTION_NAMESPACE: &str = "src/components/ui/_kit/.transactions";
    if path == TRANSACTION_NAMESPACE {
        validate_relative_path("src/components/ui/_kit")?;
        return Ok(());
    }
    if let Some(child) = path.strip_prefix("src/components/ui/_kit/.transactions/") {
        validate_relative_path("src/components/ui/_kit")?;
        validate_relative_path(child)?;
        return Ok(());
    }
    if !matches!(
        path,
        DEFAULT_KIT_WRITE_LOCK_PATH | DEFAULT_KIT_COORDINATION_IGNORE_PATH
    ) {
        return validate_relative_path(path);
    }

    let Some((parent, name)) = path.rsplit_once('/') else {
        return unsafe_path(path, "internal sentinel path is malformed");
    };
    validate_relative_path(parent)?;
    if !matches!(name, ".write.lock" | ".gitignore") {
        return unsafe_path(path, "hidden paths are rejected");
    }
    Ok(())
}

fn ensure_directory_metadata(path: &str, metadata: &Metadata) -> Result<(), CodegenError> {
    ensure_not_indirection(path, metadata)?;
    if !metadata.is_dir() {
        return unsafe_path(path, "controlled parent is not a directory");
    }
    Ok(())
}

fn ensure_regular_file_metadata(path: &str, metadata: &Metadata) -> Result<(), CodegenError> {
    ensure_not_indirection(path, metadata)?;
    if !metadata.is_file() {
        return unsafe_path(path, "controlled target is not a regular file");
    }
    Ok(())
}

fn ensure_not_indirection(path: &str, metadata: &Metadata) -> Result<(), CodegenError> {
    if metadata.file_type().is_symlink() {
        return unsafe_path(path, "symbolic links are rejected");
    }
    #[cfg(windows)]
    if cap_fs_ext::OsMetadataExt::file_attributes(metadata) & 0x0000_0400 != 0 {
        return unsafe_path(path, "Windows reparse points are rejected");
    }
    Ok(())
}

fn metadata_identity(metadata: &Metadata) -> ExactObjectIdentity {
    ExactObjectIdentity::from_unix(MetadataExt::dev(metadata), MetadataExt::ino(metadata))
}

fn preserved_mode(metadata: &Metadata) -> PreservedFileMode {
    PreservedFileMode {
        readonly: metadata.permissions().readonly(),
        posix_mode: posix_mode(metadata),
    }
}

#[cfg(unix)]
fn posix_mode(metadata: &Metadata) -> Option<u32> {
    use cap_std::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn posix_mode(_metadata: &Metadata) -> Option<u32> {
    None
}

fn preimage_conflict(
    logical_path: &str,
    expected: &PathPreimage,
    actual: &PathPreimage,
) -> CodegenError {
    CodegenError::PreimageConflict {
        path: logical_path.to_owned(),
        reason: format!("expected {expected:?}, found {actual:?}"),
    }
}

fn exact_regular_file_conflict(
    logical_path: &str,
    expected: Option<ExactRegularFileObservation>,
    actual: Option<ExactRegularFileObservation>,
) -> CodegenError {
    let reason = match (expected, actual) {
        (Some(expected), Some(actual)) if expected.identity != actual.identity => {
            "regular-file identity changed after planning".to_owned()
        }
        (Some(expected), Some(actual)) if expected.byte_len != actual.byte_len => format!(
            "regular-file byte length changed after planning: expected {}, found {}",
            expected.byte_len, actual.byte_len
        ),
        (None, Some(_)) => "an absent path became a regular file after planning".to_owned(),
        (Some(_), None) => "a regular file became absent after planning".to_owned(),
        _ => "regular-file observation changed after planning".to_owned(),
    };
    CodegenError::PreimageConflict {
        path: logical_path.to_owned(),
        reason,
    }
}

fn unsafe_path<T>(path: &str, reason: &str) -> Result<T, CodegenError> {
    Err(CodegenError::UnsafePath {
        path: path.to_owned(),
        reason: reason.to_owned(),
    })
}

fn is_allowed_write_path(path: &str) -> bool {
    matches!(
        path,
        DEFAULT_KIT_CONFIG_PATH
            | DEFAULT_KIT_LOCK_PATH
            | TOKEN_CONTRACT_PATH
            | THEME_CAPABILITY_PATH
            | "index.html"
    ) || is_allowed_stylesheet_path(path)
        || is_allowed_component_rust_path(path)
}

fn is_allowed_stylesheet_path(path: &str) -> bool {
    path.starts_with("styles/") && path.ends_with(".css")
}

fn is_allowed_component_rust_path(path: &str) -> bool {
    let segments = path.split('/').collect::<Vec<_>>();
    if segments.len() < 3
        || segments.first() != Some(&"src")
        || segments.contains(&"_kit")
        || !segments
            .last()
            .is_some_and(|file_name| file_name.ends_with(".rs"))
    {
        return false;
    }
    segments.len() > 3 || segments.last() == Some(&"mod.rs")
}

#[cfg(test)]
mod tests {
    use super::{PlanningContext, metadata_identity, preserved_mode};

    #[test]
    fn reopen_exact_directory_returns_a_fresh_matching_capability() {
        let temporary = tempfile::tempdir().expect("temporary project root");
        std::fs::create_dir(temporary.path().join("styles")).expect("create styles directory");
        let context = PlanningContext::open(temporary.path()).expect("open planning context");
        let original = context
            .open_directory("styles")
            .expect("open original directory");
        let metadata = original.dir_metadata().expect("observe original directory");
        let expected_identity = metadata_identity(&metadata);
        let expected_mode = preserved_mode(&metadata);

        let rebound = context
            .reopen_exact_directory("styles", expected_identity, expected_mode)
            .expect("reopen exact directory");
        let rebound_metadata = rebound.dir_metadata().expect("observe rebound directory");

        assert_eq!(metadata_identity(&rebound_metadata), expected_identity);
        assert_eq!(preserved_mode(&rebound_metadata), expected_mode);
    }

    #[test]
    fn reopen_exact_directory_rejects_a_substituted_parent() {
        let temporary = tempfile::tempdir().expect("temporary project root");
        let styles = temporary.path().join("styles");
        let moved_styles = temporary.path().join("styles-original");
        std::fs::create_dir(&styles).expect("create styles directory");
        let context = PlanningContext::open(temporary.path()).expect("open planning context");
        let original = context
            .open_directory("styles")
            .expect("open original directory");
        let metadata = original.dir_metadata().expect("observe original directory");
        let expected_identity = metadata_identity(&metadata);
        let expected_mode = preserved_mode(&metadata);
        drop(original);

        std::fs::rename(&styles, &moved_styles).expect("detach original directory");
        std::fs::create_dir(&styles).expect("substitute directory");

        let error = context
            .reopen_exact_directory("styles", expected_identity, expected_mode)
            .expect_err("substituted directory must be rejected");
        assert!(matches!(
            error,
            crate::CodegenError::PreimageConflict { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn reopen_exact_directory_rejects_a_mode_change() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary project root");
        let styles = temporary.path().join("styles");
        std::fs::create_dir(&styles).expect("create styles directory");
        let context = PlanningContext::open(temporary.path()).expect("open planning context");
        let original = context
            .open_directory("styles")
            .expect("open original directory");
        let metadata = original.dir_metadata().expect("observe original directory");
        let expected_identity = metadata_identity(&metadata);
        let expected_mode = preserved_mode(&metadata);
        drop(original);

        let changed_mode = expected_mode.posix_mode.expect("Unix mode") ^ 0o100;
        std::fs::set_permissions(&styles, std::fs::Permissions::from_mode(changed_mode))
            .expect("change directory mode");

        let error = context
            .reopen_exact_directory("styles", expected_identity, expected_mode)
            .expect_err("mode change must be rejected");
        assert!(matches!(
            error,
            crate::CodegenError::PreimageConflict { .. }
        ));
    }
}
