#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]
#![doc = "Safe, capability-relative platform operations used by `leptos_ui_kit_codegen`."]
//!
//! Raw operating-system handles enter this crate only through
//! [`adopt_root_directory`]. Every filesystem operation after adoption requires an
//! opaque capability carrying its verified identity, kind, access profile, and
//! direct-child binding where applicable. Filesystem mutations consume their
//! capabilities and return phase-aware failures which distinguish a syscall
//! known not to have mutated the namespace from a successful mutation whose
//! postcondition could not be verified. No capability exposes its underlying
//! operating-system handle.

use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    io,
};

#[cfg(windows)]
mod windows;

/// The stable identity of one filesystem object within a Windows volume.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileIdentity {
    /// The complete volume serial number returned by `FILE_ID_INFO`.
    pub volume_serial_number: u64,
    /// The complete 128-bit file identifier returned by `FILE_ID_INFO`.
    pub file_id: [u8; 16],
}

impl FileIdentity {
    /// Constructs an identity without narrowing either component.
    #[must_use]
    pub const fn new(volume_serial_number: u64, file_id: [u8; 16]) -> Self {
        Self {
            volume_serial_number,
            file_id,
        }
    }
}

/// The kind of object an exact-handle operation is authorized to affect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectKind {
    /// A regular file.
    RegularFile,
    /// A directory.
    Directory,
}

/// The rights guaranteed by an opaque capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityAccess {
    /// Metadata and content inspection with share-delete enabled.
    Inspect,
    /// Inspection plus write, append, delete, synchronization, and
    /// share-delete rights required by transaction mutations and durability.
    Mutation,
}

/// A stable observation captured from one exact object handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectInfo {
    /// The object's complete filesystem identity.
    pub identity: FileIdentity,
    /// Whether the object is a regular file or directory.
    pub kind: ObjectKind,
    /// The logical byte length reported by the filesystem.
    pub byte_len: u64,
    /// The number of hard links reported by the filesystem.
    pub link_count: u64,
    /// Whether the readonly file attribute is set.
    pub readonly: bool,
}

#[derive(Debug)]
struct ChildBinding {
    parent_identity: FileIdentity,
    name: OsString,
}

struct CapabilityInner {
    file: File,
    info: ObjectInfo,
    access: CapabilityAccess,
    binding: Option<ChildBinding>,
}

impl fmt::Debug for CapabilityInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Keep the handle observably retained on non-Windows builds without
        // exposing its value through the public debug representation.
        let _opaque_handle = &self.file;
        formatter
            .debug_struct("CapabilityInner")
            .field("info", &self.info)
            .field("access", &self.access)
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

/// A verified exact-object capability.
///
/// Values are constructed only by this crate. A child capability records the
/// exact parent identity and direct name from which it was opened.
#[derive(Debug)]
pub struct ObjectCapability(CapabilityInner);

impl ObjectCapability {
    /// Returns the last stable observation certified by this capability.
    #[must_use]
    pub const fn info(&self) -> ObjectInfo {
        self.0.info
    }

    /// Returns the complete certified identity.
    #[must_use]
    pub const fn identity(&self) -> FileIdentity {
        self.0.info.identity
    }

    /// Returns the certified object kind.
    #[must_use]
    pub const fn kind(&self) -> ObjectKind {
        self.0.info.kind
    }

    /// Returns the rights profile guaranteed by this capability.
    #[must_use]
    pub const fn access(&self) -> CapabilityAccess {
        self.0.access
    }

    /// Returns the certified parent identity for a direct-child capability.
    #[must_use]
    pub fn parent_identity(&self) -> Option<FileIdentity> {
        self.0
            .binding
            .as_ref()
            .map(|binding| binding.parent_identity)
    }

    /// Returns the certified direct child name, when present.
    #[must_use]
    pub fn name(&self) -> Option<&OsStr> {
        self.0
            .binding
            .as_ref()
            .map(|binding| binding.name.as_os_str())
    }

    /// Converts a verified directory object into a parent-directory
    /// capability without reopening it.
    pub fn try_into_directory(self) -> Result<DirectoryCapability, Self> {
        if self.kind() == ObjectKind::Directory {
            Ok(DirectoryCapability(self.0))
        } else {
            Err(self)
        }
    }

    #[cfg(windows)]
    fn binding_matches(&self, parent: &DirectoryCapability, name: &OsStr) -> bool {
        self.0.binding.as_ref().is_some_and(|binding| {
            binding.parent_identity == parent.identity() && binding.name == name
        })
    }

    #[cfg(windows)]
    fn into_unverified(self) -> UnverifiedObjectCapability {
        UnverifiedObjectCapability {
            file: self.0.file,
            expected_kind: self.0.info.kind,
            access: self.0.access,
            binding: self.0.binding,
            last_observation: Some(self.0.info),
        }
    }
}

/// A verified directory capability suitable for capability-relative child
/// operations.
#[derive(Debug)]
pub struct DirectoryCapability(CapabilityInner);

impl DirectoryCapability {
    /// Returns the last stable observation certified by this capability.
    #[must_use]
    pub const fn info(&self) -> ObjectInfo {
        self.0.info
    }

    /// Returns the complete certified identity.
    #[must_use]
    pub const fn identity(&self) -> FileIdentity {
        self.0.info.identity
    }

    /// Returns the rights profile guaranteed by this capability.
    #[must_use]
    pub const fn access(&self) -> CapabilityAccess {
        self.0.access
    }

    /// Returns the certified parent identity if this directory was opened as a
    /// direct child.
    #[must_use]
    pub fn parent_identity(&self) -> Option<FileIdentity> {
        self.0
            .binding
            .as_ref()
            .map(|binding| binding.parent_identity)
    }

    /// Returns the certified direct child name, when present.
    #[must_use]
    pub fn name(&self) -> Option<&OsStr> {
        self.0
            .binding
            .as_ref()
            .map(|binding| binding.name.as_os_str())
    }

    /// Converts this directory into an exact-object capability for movement or
    /// deletion.
    #[must_use]
    pub fn into_object(self) -> ObjectCapability {
        ObjectCapability(self.0)
    }
}

/// An exact handle retained after the namespace may have mutated but the
/// operation's postcondition was not proven.
///
/// This type deliberately cannot be passed to another mutation or expose its
/// retained handle. Recovery may inspect its certified metadata, keep the
/// exact handle alive while reconciling the journal, and then drop it before
/// rebinding through a verified parent capability.
pub struct UnverifiedObjectCapability {
    file: File,
    expected_kind: ObjectKind,
    access: CapabilityAccess,
    binding: Option<ChildBinding>,
    last_observation: Option<ObjectInfo>,
}

impl fmt::Debug for UnverifiedObjectCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The exact handle intentionally remains live but opaque to callers.
        let _opaque_handle = &self.file;
        formatter
            .debug_struct("UnverifiedObjectCapability")
            .field("expected_kind", &self.expected_kind)
            .field("access", &self.access)
            .field("binding", &self.binding)
            .field("last_observation", &self.last_observation)
            .finish_non_exhaustive()
    }
}

impl UnverifiedObjectCapability {
    /// Returns the kind the operation was authorized to affect.
    #[must_use]
    pub const fn expected_kind(&self) -> ObjectKind {
        self.expected_kind
    }

    /// Returns the access profile with which the retained handle was opened.
    #[must_use]
    pub const fn access(&self) -> CapabilityAccess {
        self.access
    }

    /// Returns the last stable pre- or post-mutation observation, if one was
    /// captured.
    #[must_use]
    pub const fn last_observation(&self) -> Option<ObjectInfo> {
        self.last_observation
    }

    /// Returns the pre-operation parent identity, when known.
    #[must_use]
    pub fn parent_identity(&self) -> Option<FileIdentity> {
        self.binding.as_ref().map(|binding| binding.parent_identity)
    }

    /// Returns the pre-operation direct child name, when known.
    #[must_use]
    pub fn name(&self) -> Option<&OsStr> {
        self.binding
            .as_ref()
            .map(|binding| binding.name.as_os_str())
    }
}

/// The phase certified by a mutation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationPhase {
    /// The namespace-changing system call did not succeed.
    NotMutated,
    /// The system call succeeded, but an exact postcondition could not be
    /// proven.
    MutatedUnverified,
}

/// A phase-aware filesystem mutation failure which retains all owned
/// capabilities.
#[derive(Debug)]
pub enum MutationFailure<Verified, Unverified> {
    /// The namespace-changing system call did not succeed.
    NotMutated {
        /// The operation-specific error.
        error: io::Error,
        /// Verified capabilities returned to the caller unchanged.
        capabilities: Box<Verified>,
    },
    /// The system call succeeded, but postcondition verification failed.
    MutatedUnverified {
        /// The postcondition or observation error.
        error: io::Error,
        /// Exact retained handles whose namespace bindings are no longer
        /// trusted.
        capabilities: Box<Unverified>,
    },
}

impl<Verified, Unverified> MutationFailure<Verified, Unverified> {
    /// Returns the certified failure phase.
    #[must_use]
    pub const fn phase(&self) -> MutationPhase {
        match self {
            Self::NotMutated { .. } => MutationPhase::NotMutated,
            Self::MutatedUnverified { .. } => MutationPhase::MutatedUnverified,
        }
    }

    /// Returns the underlying operation or verification error.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        match self {
            Self::NotMutated { error, .. } | Self::MutatedUnverified { error, .. } => error,
        }
    }

    /// Consumes the failure and returns its error together with the typed
    /// capabilities retained for the certified phase.
    #[must_use = "the phase-specific retained capabilities must be handled"]
    pub fn into_parts(self) -> (io::Error, Result<Verified, Unverified>) {
        match self {
            Self::NotMutated {
                error,
                capabilities,
            } => (error, Ok(*capabilities)),
            Self::MutatedUnverified {
                error,
                capabilities,
            } => (error, Err(*capabilities)),
        }
    }
}

/// Verified source and target handles returned when replacement did not
/// mutate the namespace.
#[derive(Debug)]
pub struct ReplacementCapabilities {
    source: ObjectCapability,
    target: ObjectCapability,
}

impl ReplacementCapabilities {
    /// Consumes the payload and returns the source and target capabilities.
    #[must_use]
    pub fn into_parts(self) -> (ObjectCapability, ObjectCapability) {
        (self.source, self.target)
    }
}

/// Retained source and target handles returned when replacement mutated the
/// namespace but its postcondition was not proven.
#[derive(Debug)]
pub struct UnverifiedReplacementCapabilities {
    source: UnverifiedObjectCapability,
    target: UnverifiedObjectCapability,
}

impl UnverifiedReplacementCapabilities {
    /// Consumes the payload and returns the retained source and target handles.
    #[must_use]
    pub fn into_parts(self) -> (UnverifiedObjectCapability, UnverifiedObjectCapability) {
        (self.source, self.target)
    }
}

/// An adoption failure which retains an exact operating-system handle.
pub struct AdoptionError {
    _handle: File,
    error: io::Error,
}

impl fmt::Debug for AdoptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdoptionError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl AdoptionError {
    /// Returns the adoption error.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        &self.error
    }

    /// Consumes the error, closes the retained handle, and returns the cause.
    #[must_use]
    pub fn into_error(self) -> io::Error {
        self.error
    }
}

impl fmt::Display for AdoptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "directory capability adoption failed: {}",
            self.error
        )
    }
}

impl Error for AdoptionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

/// Filesystem capabilities established for a local transaction volume.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeCapabilities {
    /// The exact identity of the probed directory handle.
    pub root_identity: FileIdentity,
    /// The filesystem name reported by the operating system.
    pub file_system_name: String,
    /// The maximum direct-component length reported by the filesystem.
    pub maximum_component_length: u32,
    /// Whether the volume reports hard-link support.
    pub supports_hard_links: bool,
    /// Whether the volume reports reparse-point support.
    pub supports_reparse_points: bool,
}

/// Consumes the transaction root directory handle and upgrades it to a
/// mutation-grade, share-delete capability for the same exact directory.
///
/// This is the crate's sole raw-handle ingress. Child authorities can be
/// derived only through capability-relative operations, and no API returns an
/// underlying file handle.
pub fn adopt_root_directory(file: File) -> Result<DirectoryCapability, AdoptionError> {
    #[cfg(windows)]
    {
        windows::adopt_root_directory(file)
    }
    #[cfg(not(windows))]
    {
        Err(AdoptionError {
            _handle: file,
            error: unsupported("directory capability adoption"),
        })
    }
}

/// Consumes an already-open exact object handle and upgrades it to an opaque
/// capability with the requested access profile.
///
/// The object kind and full identity are verified before and after the exact
/// reopen, and the original ingress handle is closed before mutation rights
/// are requested. This is intended for bridging capability-safe filesystem
/// libraries whose object handles are already pinned but whose portable
/// metadata narrows Windows file identifiers.
pub fn adopt_object(
    file: File,
    expected_kind: ObjectKind,
    access: CapabilityAccess,
) -> Result<ObjectCapability, AdoptionError> {
    #[cfg(windows)]
    {
        windows::adopt_object(file, expected_kind, access)
    }
    #[cfg(not(windows))]
    {
        let _ = (expected_kind, access);
        Err(AdoptionError {
            _handle: file,
            error: unsupported("object capability adoption"),
        })
    }
}

/// Refreshes an object capability and requires its certified identity and kind
/// to remain unchanged.
pub fn refresh_object(object: &ObjectCapability) -> io::Result<ObjectInfo> {
    #[cfg(windows)]
    {
        windows::refresh_verified(&object.0)
    }
    #[cfg(not(windows))]
    {
        let _ = object;
        Err(unsupported("exact object inspection"))
    }
}

/// Refreshes a directory capability and requires its certified identity to
/// remain unchanged.
pub fn refresh_directory(directory: &DirectoryCapability) -> io::Result<ObjectInfo> {
    #[cfg(windows)]
    {
        windows::refresh_verified(&directory.0)
    }
    #[cfg(not(windows))]
    {
        let _ = directory;
        Err(unsupported("exact directory inspection"))
    }
}

/// Opens one direct child without following a reparse point and returns an
/// opaque capability with the requested guaranteed rights.
pub fn open_child_nofollow(
    parent: &DirectoryCapability,
    name: &OsStr,
    expected_kind: ObjectKind,
    access: CapabilityAccess,
) -> io::Result<ObjectCapability> {
    validate_direct_name(name)?;
    #[cfg(windows)]
    {
        windows::open_child_nofollow(parent, name, expected_kind, access)
    }
    #[cfg(not(windows))]
    {
        let _ = (parent, expected_kind, access);
        Err(unsupported("capability-relative no-follow open"))
    }
}

/// Atomically creates one direct child directory and returns a mutation-grade
/// capability to it.
pub fn create_directory_nofollow(
    parent: &DirectoryCapability,
    name: &OsStr,
) -> Result<DirectoryCapability, MutationFailure<(), UnverifiedObjectCapability>> {
    if let Err(error) = validate_direct_name(name) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(()),
        });
    }
    #[cfg(windows)]
    {
        windows::create_directory_nofollow(parent, name)
    }
    #[cfg(not(windows))]
    {
        let _ = parent;
        Err(MutationFailure::NotMutated {
            error: unsupported("capability-relative directory creation"),
            capabilities: Box::new(()),
        })
    }
}

/// Moves an exact file or directory capability to a direct child of another
/// parent without replacing an existing destination.
pub fn move_noreplace(
    source: ObjectCapability,
    target_parent: &DirectoryCapability,
    target_name: &OsStr,
) -> Result<ObjectCapability, MutationFailure<ObjectCapability, UnverifiedObjectCapability>> {
    if let Err(error) = validate_direct_name(target_name) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(source),
        });
    }
    #[cfg(windows)]
    {
        windows::move_noreplace(source, target_parent, target_name)
    }
    #[cfg(not(windows))]
    {
        let _ = target_parent;
        Err(MutationFailure::NotMutated {
            error: unsupported("capability-relative no-replace move"),
            capabilities: Box::new(source),
        })
    }
}

/// Replaces a verified direct child with an exact source capability.
///
/// Windows does not provide a compare-by-identity rename primitive. The API
/// therefore holds and revalidates the target handle and preserves the
/// transaction protocol's exclusion of active same-user swapping between the
/// final verification and the system call.
pub fn replace_exact(
    source: ObjectCapability,
    target_parent: &DirectoryCapability,
    target_name: &OsStr,
    target: ObjectCapability,
) -> Result<
    ObjectCapability,
    MutationFailure<ReplacementCapabilities, UnverifiedReplacementCapabilities>,
> {
    if let Err(error) = validate_direct_name(target_name) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(ReplacementCapabilities { source, target }),
        });
    }
    #[cfg(windows)]
    {
        windows::replace_exact(source, target_parent, target_name, target)
    }
    #[cfg(not(windows))]
    {
        let _ = target_parent;
        Err(MutationFailure::NotMutated {
            error: unsupported("capability-relative exact replacement"),
            capabilities: Box::new(ReplacementCapabilities { source, target }),
        })
    }
}

/// Deletes the bound direct child represented by an exact capability using
/// modern POSIX disposition semantics.
///
/// There is deliberately no legacy fallback and readonly attributes are never
/// cleared.
pub fn delete_exact(
    parent: &DirectoryCapability,
    object: ObjectCapability,
) -> Result<(), MutationFailure<ObjectCapability, UnverifiedObjectCapability>> {
    #[cfg(windows)]
    {
        windows::delete_exact(parent, object)
    }
    #[cfg(not(windows))]
    {
        let _ = parent;
        Err(MutationFailure::NotMutated {
            error: unsupported("exact handle-based deletion"),
            capabilities: Box::new(object),
        })
    }
}

/// Flushes an exact mutation-grade directory capability and verifies that its
/// identity remained stable across the operation.
pub fn sync_directory(directory: &DirectoryCapability) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::sync_directory(directory)
    }
    #[cfg(not(windows))]
    {
        let _ = directory;
        Err(unsupported("exact directory durability"))
    }
}

/// Establishes static support for a local NTFS or ReFS transaction volume.
///
/// Transaction code must additionally exercise its journal-owned behavioral
/// probe before preparing an application cohort.
pub fn probe_volume(root: &DirectoryCapability) -> io::Result<VolumeCapabilities> {
    #[cfg(windows)]
    {
        windows::probe_volume(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        Err(unsupported("Windows transaction volume probing"))
    }
}

fn validate_direct_name(name: &OsStr) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::validate_direct_name(name)
    }
    #[cfg(not(windows))]
    {
        let rendered = name.to_str().ok_or_else(invalid_direct_name)?;
        validate_windows_name_text(rendered)
    }
}

fn validate_windows_name_text(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.chars().any(|character| {
            character <= '\u{1f}'
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
        || is_reserved_windows_device_name(name)
    {
        return Err(invalid_direct_name());
    }
    Ok(())
}

fn is_reserved_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let uppercase = stem.to_ascii_uppercase();
    matches!(
        uppercase.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) || matches_reserved_numbered_device(&uppercase, "COM")
        || matches_reserved_numbered_device(&uppercase, "LPT")
}

fn matches_reserved_numbered_device(name: &str, prefix: &str) -> bool {
    let Some(suffix) = name.strip_prefix(prefix) else {
        return false;
    };
    matches!(
        suffix,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
    )
}

fn invalid_direct_name() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "expected one portable Windows direct child name without reserved characters, device syntax, trailing dot/space, controls, or invalid Unicode",
    )
}

#[cfg(windows)]
fn access_required(operation: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{operation} requires a mutation-grade capability"),
    )
}

#[cfg(windows)]
fn alias_rejected(operation: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{operation} rejected source/target aliasing"),
    )
}

#[cfg(windows)]
fn binding_changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "the direct child binding no longer names the certified object",
    )
}

#[cfg(not(windows))]
fn unsupported(operation: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{operation} is only available on Windows"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_preserves_all_file_id_bits() {
        let identity = FileIdentity::new(
            0xfedc_ba98_7654_3210,
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ],
        );
        assert_eq!(identity.volume_serial_number, 0xfedc_ba98_7654_3210);
        assert_eq!(
            &identity.file_id[8..],
            &[0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]
        );
    }

    #[test]
    fn direct_name_validation_rejects_escape_device_and_stream_forms() {
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a:b",
            "a?b",
            "a*b",
            "a|b",
            "a<b",
            "a>b",
            "a\"b",
            "trailing.",
            "trailing ",
            "nul\0suffix",
            "control\u{1f}",
            "NUL",
            "nul.txt",
            "COM1",
            "lpt9.log",
            "COM¹",
        ] {
            assert!(validate_direct_name(OsStr::new(name)).is_err(), "{name:?}");
        }
        assert!(validate_direct_name(OsStr::new("theme-stage-01")).is_ok());
        assert!(validate_direct_name(OsStr::new("component.name")).is_ok());
    }

    #[test]
    fn mutation_phase_is_explicit() {
        let failure: MutationFailure<(), ()> = MutationFailure::NotMutated {
            error: io::Error::other("test"),
            capabilities: Box::new(()),
        };
        assert_eq!(failure.phase(), MutationPhase::NotMutated);
    }

    #[cfg(unix)]
    #[test]
    fn direct_name_validation_rejects_non_unicode_input() {
        use std::os::unix::ffi::OsStringExt as _;

        let name = OsString::from_vec(vec![0xff]);
        assert!(validate_direct_name(&name).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn adoption_fails_closed_and_retains_handle_off_windows() {
        let directory = File::open(".").expect("open current directory");
        let error = adopt_root_directory(directory).expect_err("non-Windows adoption must fail");
        assert_eq!(error.error().kind(), io::ErrorKind::Unsupported);
        let cause = error.into_error();
        assert_eq!(cause.kind(), io::ErrorKind::Unsupported);
    }
}
