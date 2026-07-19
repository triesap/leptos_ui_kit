#![cfg_attr(not(windows), forbid(unsafe_code))]
#![cfg_attr(windows, deny(unsafe_code))]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]
#![doc = "Safe, capability-relative platform operations used by `leptos_ui_kit_codegen`."]
//!
//! Raw operating-system handles enter this crate only through
//! [`adopt_root_directory`], and an adopted root cannot authorize child access
//! until [`probe_volume`] consumes it. Every subsequent filesystem operation
//! requires an opaque capability carrying its volume qualification, verified
//! identity, kind, access profile, and direct-child binding where applicable.
//! Filesystem mutations distinguish not-mutated, mutated-but-unverified,
//! namespace-verified, and parent-durable outcomes. No capability exposes its
//! underlying operating-system handle.

use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    io,
    sync::Arc,
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

/// Exact source and target observations authorized for one replacement.
///
/// Callers must construct this policy from their journal-certified
/// preconditions. The platform boundary compares both observations with the
/// opaque capabilities and with fresh exact-handle observations immediately
/// before invoking the replacement ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplacementPolicy {
    source_before: ObjectInfo,
    target_before: ObjectInfo,
}

/// The exact source observation authorized for one no-replace move.
///
/// Regular-file movement is intentionally limited to a source with exactly one
/// hard link. Directories must have a nonzero link count, and every metadata
/// field must remain equal to `source_before` immediately before the rename.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MovePolicy {
    source_before: ObjectInfo,
}

/// The exact regular-file state authorized for one hard-link alias creation.
///
/// The source must remain bound to `source_parent`, the destination must be
/// absent, and the source link count must increase by exactly one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HardLinkPolicy {
    source_before: ObjectInfo,
}

/// The exact two-link regular-file state authorized for alias retirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HardLinkRetirementPolicy {
    linked_before: ObjectInfo,
}

impl HardLinkRetirementPolicy {
    /// Declares the complete exact two-link state authorized for retirement.
    #[must_use]
    pub const fn new(linked_before: ObjectInfo) -> Self {
        Self { linked_before }
    }

    /// Returns the complete authorized pre-retirement observation.
    #[must_use]
    pub const fn linked_before(self) -> ObjectInfo {
        self.linked_before
    }
}

impl HardLinkPolicy {
    /// Declares the complete exact source state authorized for alias creation.
    #[must_use]
    pub const fn new(source_before: ObjectInfo) -> Self {
        Self { source_before }
    }

    /// Returns the complete authorized source observation.
    #[must_use]
    pub const fn source_before(self) -> ObjectInfo {
        self.source_before
    }
}

impl MovePolicy {
    /// Declares the complete exact source state authorized for movement.
    #[must_use]
    pub const fn new(source_before: ObjectInfo) -> Self {
        Self { source_before }
    }

    /// Returns the complete authorized source observation.
    #[must_use]
    pub const fn source_before(self) -> ObjectInfo {
        self.source_before
    }
}

impl ReplacementPolicy {
    /// Declares the exact source and target states authorized for replacement.
    #[must_use]
    pub const fn new(source_before: ObjectInfo, target_before: ObjectInfo) -> Self {
        Self {
            source_before,
            target_before,
        }
    }

    /// Returns the exact authorized source observation.
    #[must_use]
    pub const fn source_before(self) -> ObjectInfo {
        self.source_before
    }

    /// Returns the exact authorized target observation.
    #[must_use]
    pub const fn target_before(self) -> ObjectInfo {
        self.target_before
    }
}

#[derive(Debug)]
struct ChildBinding {
    parent_identity: FileIdentity,
    name: OsString,
}

#[cfg(windows)]
#[derive(Debug)]
enum NamespacePostcondition {
    Present {
        parent_identity: FileIdentity,
        name: OsString,
        expected: ObjectInfo,
    },
    Absent {
        parent_identity: FileIdentity,
        name: OsString,
    },
}

#[cfg(windows)]
impl NamespacePostcondition {
    fn present(parent: &DirectoryCapability, name: &OsStr, expected: ObjectInfo) -> Self {
        Self::Present {
            parent_identity: parent.identity(),
            name: name.to_os_string(),
            expected,
        }
    }

    fn absent(parent: &DirectoryCapability, name: &OsStr) -> Self {
        Self::Absent {
            parent_identity: parent.identity(),
            name: name.to_os_string(),
        }
    }
}

struct CapabilityInner {
    file: File,
    info: ObjectInfo,
    access: CapabilityAccess,
    binding: Option<ChildBinding>,
    qualification: Arc<VolumeQualification>,
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
            .field("volume", &self.qualification.capabilities)
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

    /// Returns the immutable volume capabilities inherited from the qualified
    /// transaction root.
    #[must_use]
    pub fn volume_capabilities(&self) -> &VolumeCapabilities {
        &self.0.qualification.capabilities
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
    pub fn try_into_directory(self) -> Result<DirectoryCapability, Box<Self>> {
        if self.kind() == ObjectKind::Directory {
            Ok(DirectoryCapability(self.0))
        } else {
            Err(Box::new(self))
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
            qualification: self.0.qualification,
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

    /// Returns the immutable volume capabilities inherited from the qualified
    /// transaction root.
    #[must_use]
    pub fn volume_capabilities(&self) -> &VolumeCapabilities {
        &self.0.qualification.capabilities
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
#[must_use = "the exact recovery handle must be reconciled deliberately before it is dropped"]
pub struct UnverifiedObjectCapability {
    file: File,
    expected_kind: ObjectKind,
    access: CapabilityAccess,
    binding: Option<ChildBinding>,
    last_observation: Option<ObjectInfo>,
    qualification: Arc<VolumeQualification>,
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
            .field("volume", &self.qualification.capabilities)
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

    /// Returns the immutable volume capabilities retained with this exact
    /// recovery handle.
    #[must_use]
    pub fn volume_capabilities(&self) -> &VolumeCapabilities {
        &self.qualification.capabilities
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
    /// The namespace mutation and its exact postconditions were verified, but
    /// required parent-directory durability barriers remain pending.
    NamespaceVerified,
    /// Every required parent-directory durability barrier completed and was
    /// reverified.
    ParentsDurable,
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

/// A namespace mutation whose exact postconditions are verified but whose
/// required parent-directory durability barriers are still pending.
///
/// Values can be constructed only by this crate after a successful mutator.
/// Pass the value and the exact parent capabilities to
/// [`sync_mutation_parents`] to obtain a [`DurableMutation`].
#[derive(Debug)]
#[must_use = "the verified namespace mutation still requires its exact parent durability barriers"]
pub struct VerifiedMutation<T> {
    value: T,
    required_parent_identities: Vec<FileIdentity>,
    qualification: Arc<VolumeQualification>,
    #[cfg(windows)]
    postconditions: Vec<NamespacePostcondition>,
}

impl<T> VerifiedMutation<T> {
    /// Returns this outcome's explicit protocol phase.
    #[must_use]
    pub const fn phase(&self) -> MutationPhase {
        MutationPhase::NamespaceVerified
    }

    /// Returns the exact parent identities that must be flushed.
    #[must_use]
    pub fn required_parent_identities(&self) -> &[FileIdentity] {
        &self.required_parent_identities
    }

    /// Returns the immutable volume capabilities shared by every required
    /// parent and mutated object.
    #[must_use]
    pub fn volume_capabilities(&self) -> &VolumeCapabilities {
        &self.qualification.capabilities
    }

    #[cfg(windows)]
    fn new(
        value: T,
        qualification: Arc<VolumeQualification>,
        required_parent_identities: impl IntoIterator<Item = FileIdentity>,
        postconditions: impl IntoIterator<Item = NamespacePostcondition>,
    ) -> Self {
        let mut unique = Vec::new();
        for identity in required_parent_identities {
            if !unique.contains(&identity) {
                unique.push(identity);
            }
        }
        Self {
            value,
            required_parent_identities: unique,
            qualification,
            postconditions: postconditions.into_iter().collect(),
        }
    }
}

/// A verified namespace mutation whose required parent-directory durability
/// barriers all completed successfully.
#[derive(Debug)]
#[must_use = "the durable mutation result retains the verified operation value"]
pub struct DurableMutation<T> {
    value: T,
}

impl<T> DurableMutation<T> {
    /// Returns this outcome's explicit protocol phase.
    #[must_use]
    pub const fn phase(&self) -> MutationPhase {
        MutationPhase::ParentsDurable
    }

    /// Returns the durably published value.
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Consumes the outcome and returns the durably published value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.value
    }
}

/// A parent-directory durability failure which retains the verified namespace
/// outcome for an idempotent retry or journal-driven recovery.
#[derive(Debug)]
#[must_use = "the failure retains a verified namespace mutation whose durability obligation remains pending"]
pub struct DurabilityFailure<T> {
    error: io::Error,
    mutation: VerifiedMutation<T>,
}

impl<T> DurabilityFailure<T> {
    /// Returns the retained mutation phase; namespace postconditions are
    /// verified, but one or more parent durability barriers remain pending.
    #[must_use]
    pub const fn phase(&self) -> MutationPhase {
        MutationPhase::NamespaceVerified
    }

    /// Returns the durability or authority-validation error.
    #[must_use]
    pub const fn error(&self) -> &io::Error {
        &self.error
    }

    /// Consumes the failure and returns both its cause and retained verified
    /// namespace outcome.
    #[must_use = "the retained namespace outcome must be recovered or retried"]
    pub fn into_parts(self) -> (io::Error, VerifiedMutation<T>) {
        (self.error, self.mutation)
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
#[must_use = "both exact recovery handles must be reconciled deliberately before they are dropped"]
pub struct UnverifiedReplacementCapabilities {
    source: UnverifiedObjectCapability,
    target: UnverifiedObjectCapability,
}

/// Verified owner and alias handles returned when alias retirement did not
/// mutate the namespace.
#[derive(Debug)]
pub struct HardLinkAliasCapabilities {
    owner: ObjectCapability,
    alias: ObjectCapability,
}

impl HardLinkAliasCapabilities {
    /// Consumes the payload and returns the surviving owner and alias.
    #[must_use]
    pub fn into_parts(self) -> (ObjectCapability, ObjectCapability) {
        (self.owner, self.alias)
    }
}

/// Retained exact owner and alias handles returned when alias retirement
/// mutated but its postcondition could not be proven.
#[derive(Debug)]
#[must_use = "both exact recovery handles must be reconciled deliberately before they are dropped"]
pub struct UnverifiedHardLinkAliasCapabilities {
    owner: UnverifiedObjectCapability,
    alias: UnverifiedObjectCapability,
}

impl UnverifiedHardLinkAliasCapabilities {
    /// Consumes the payload and returns both retained recovery handles.
    pub fn into_parts(self) -> (UnverifiedObjectCapability, UnverifiedObjectCapability) {
        (self.owner, self.alias)
    }
}

impl UnverifiedReplacementCapabilities {
    /// Consumes the payload and returns the retained source and target handles.
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

struct AdoptedRootInner {
    file: File,
    info: ObjectInfo,
}

/// An exact mutation-grade root handle which has not yet passed volume
/// qualification.
///
/// This type cannot authorize child access or mutation. It must be consumed by
/// [`probe_volume`] first, which prevents an unsupported filesystem from
/// reaching transaction preparation through this API.
pub struct AdoptedRootDirectory(AdoptedRootInner);

impl fmt::Debug for AdoptedRootDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _opaque_handle = &self.0.file;
        formatter
            .debug_struct("AdoptedRootDirectory")
            .field("info", &self.0.info)
            .finish_non_exhaustive()
    }
}

impl AdoptedRootDirectory {
    /// Returns the exact root observation established during raw-handle
    /// adoption.
    #[must_use]
    pub const fn info(&self) -> ObjectInfo {
        self.0.info
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

#[derive(Debug)]
struct VolumeQualification {
    capabilities: VolumeCapabilities,
}

/// A transaction root whose local volume has passed all static platform
/// capability checks.
///
/// The qualification token is private and shared by every child capability
/// derived from this root. Mutators reject capabilities from different
/// qualification lineages even when volume serial numbers happen to match.
#[derive(Debug)]
pub struct QualifiedVolume {
    root: DirectoryCapability,
}

impl QualifiedVolume {
    /// Returns the immutable established volume capabilities.
    #[must_use]
    pub fn capabilities(&self) -> &VolumeCapabilities {
        self.root.volume_capabilities()
    }

    /// Returns the qualified root directory capability.
    #[must_use]
    pub const fn root(&self) -> &DirectoryCapability {
        &self.root
    }

    /// Consumes the qualification wrapper and returns its qualified root.
    #[must_use]
    pub fn into_root(self) -> DirectoryCapability {
        self.root
    }
}

/// Consumes the transaction root directory handle and reopens a mutation-grade,
/// share-delete handle for the same exact directory.
///
/// This is the crate's sole raw-handle ingress. The returned adopted root cannot
/// authorize child access until [`probe_volume`] consumes it. Qualified child
/// authorities can then be derived only through capability-relative operations,
/// and no API returns an underlying file handle.
pub fn adopt_root_directory(file: File) -> Result<AdoptedRootDirectory, AdoptionError> {
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

/// Atomically creates one direct child directory and returns its verified
/// capability with an explicit pending parent-durability obligation.
pub fn create_directory_nofollow(
    parent: &DirectoryCapability,
    name: &OsStr,
) -> Result<VerifiedMutation<DirectoryCapability>, MutationFailure<(), UnverifiedObjectCapability>>
{
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

/// Atomically creates and durably populates one direct-child regular file.
///
/// The child is opened with mutation-grade, share-delete, no-follow rights.
/// A successful result has exact identity, kind, length, link-count, and
/// readonly observations, but still requires its parent durability barrier
/// through [`sync_mutation_parents`].
pub fn create_regular_file_nofollow(
    parent: &DirectoryCapability,
    name: &OsStr,
    bytes: &[u8],
) -> Result<VerifiedMutation<ObjectCapability>, MutationFailure<(), UnverifiedObjectCapability>> {
    if let Err(error) = validate_direct_name(name) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(()),
        });
    }
    #[cfg(windows)]
    {
        windows::create_regular_file_nofollow(parent, name, bytes)
    }
    #[cfg(not(windows))]
    {
        let _ = (parent, bytes);
        Err(MutationFailure::NotMutated {
            error: unsupported("capability-relative regular-file creation"),
            capabilities: Box::new(()),
        })
    }
}

/// Creates a no-replace hard-link alias for one exact regular file.
///
/// Both the source's original direct-child binding and the new alias are
/// reverified against the complete post-link observation. The target parent
/// must then be flushed through [`sync_mutation_parents`].
pub fn create_hard_link_alias(
    source: ObjectCapability,
    source_parent: &DirectoryCapability,
    target_parent: &DirectoryCapability,
    target_name: &OsStr,
    policy: HardLinkPolicy,
) -> Result<
    VerifiedMutation<ObjectCapability>,
    MutationFailure<ObjectCapability, UnverifiedObjectCapability>,
> {
    if let Err(error) = validate_direct_name(target_name) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(source),
        });
    }
    #[cfg(windows)]
    {
        windows::create_hard_link_alias(source, source_parent, target_parent, target_name, policy)
    }
    #[cfg(not(windows))]
    {
        let _ = (source_parent, target_parent, policy);
        Err(MutationFailure::NotMutated {
            error: unsupported("capability-relative hard-link alias creation"),
            capabilities: Box::new(source),
        })
    }
}

/// Retires one exact hard-link alias while preserving and reverifying its
/// exact owner binding.
///
/// This is deliberately separate from [`delete_exact`], which rejects
/// multi-link regular files. The precondition requires two mutation-grade
/// capabilities for the same exact two-link object under distinct bindings.
pub fn retire_hard_link_alias(
    owner: ObjectCapability,
    owner_parent: &DirectoryCapability,
    alias: ObjectCapability,
    alias_parent: &DirectoryCapability,
    policy: HardLinkRetirementPolicy,
) -> Result<
    VerifiedMutation<ObjectCapability>,
    MutationFailure<HardLinkAliasCapabilities, UnverifiedHardLinkAliasCapabilities>,
> {
    #[cfg(windows)]
    {
        windows::retire_hard_link_alias(owner, owner_parent, alias, alias_parent, policy)
    }
    #[cfg(not(windows))]
    {
        let _ = (owner_parent, alias_parent, policy);
        Err(MutationFailure::NotMutated {
            error: unsupported("exact hard-link alias retirement"),
            capabilities: Box::new(HardLinkAliasCapabilities { owner, alias }),
        })
    }
}

/// Moves an exact bound file or directory capability to a direct child of
/// another parent without replacing an existing destination.
///
/// The source policy and binding are revalidated through `source_parent`. A
/// successful result must flush both exact parents through
/// [`sync_mutation_parents`] before its capability can be recovered.
pub fn move_noreplace(
    source: ObjectCapability,
    source_parent: &DirectoryCapability,
    target_parent: &DirectoryCapability,
    target_name: &OsStr,
    policy: MovePolicy,
) -> Result<
    VerifiedMutation<ObjectCapability>,
    MutationFailure<ObjectCapability, UnverifiedObjectCapability>,
> {
    if let Err(error) = validate_direct_name(target_name) {
        return Err(MutationFailure::NotMutated {
            error,
            capabilities: Box::new(source),
        });
    }
    #[cfg(windows)]
    {
        windows::move_noreplace(source, source_parent, target_parent, target_name, policy)
    }
    #[cfg(not(windows))]
    {
        let _ = (source_parent, target_parent, policy);
        Err(MutationFailure::NotMutated {
            error: unsupported("capability-relative no-replace move"),
            capabilities: Box::new(source),
        })
    }
}

/// Replaces a verified direct child with an exact source capability.
///
/// Windows does not provide a compare-by-identity rename primitive. The API
/// therefore holds the target handle and revalidates its identity, direct-child
/// binding, and exact policy immediately before the atomic rename. Callers must
/// hold the transaction lock across this operation; the Windows ABI does not
/// provide hostile same-user compare-and-swap semantics for the destination
/// name. A successful syscall is followed by a typed exact-handle/name
/// postcondition check. `source_parent` and `target_parent` may differ; both are
/// part of the returned durability obligation. Readonly targets are always
/// rejected.
pub fn replace_exact(
    source: ObjectCapability,
    source_parent: &DirectoryCapability,
    target_parent: &DirectoryCapability,
    target_name: &OsStr,
    target: ObjectCapability,
    policy: ReplacementPolicy,
) -> Result<
    VerifiedMutation<ObjectCapability>,
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
        windows::replace_exact(
            source,
            source_parent,
            target_parent,
            target_name,
            target,
            policy,
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (source_parent, target_parent, policy);
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
/// cleared. Regular files must have exactly one hard link. Directory callers
/// remain responsible for proving their declared empty/inventory policy before
/// requesting disposition.
pub fn delete_exact(
    parent: &DirectoryCapability,
    object: ObjectCapability,
) -> Result<VerifiedMutation<()>, MutationFailure<ObjectCapability, UnverifiedObjectCapability>> {
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

/// Flushes and reverifies every exact parent required by a verified namespace
/// mutation.
///
/// The complete parent set, shared volume qualification, and exact
/// presence/absence postconditions are validated before the first flush. The
/// postconditions are checked again after every required parent is flushed. A
/// failure retains the original [`VerifiedMutation`] so the same durability
/// obligation can be retried safely after rediscovery.
pub fn sync_mutation_parents<T>(
    mutation: VerifiedMutation<T>,
    parents: &[&DirectoryCapability],
) -> Result<DurableMutation<T>, DurabilityFailure<T>> {
    #[cfg(windows)]
    {
        windows::sync_mutation_parents(mutation, parents)
    }
    #[cfg(not(windows))]
    {
        let _ = parents;
        let _retained_value = &mutation.value;
        Err(DurabilityFailure {
            error: unsupported("mutation parent durability"),
            mutation,
        })
    }
}

/// Consumes an adopted root and establishes static support for a local NTFS or
/// ReFS transaction volume.
///
/// Transaction code must additionally exercise its journal-owned behavioral
/// probe before preparing an application cohort.
pub fn probe_volume(root: AdoptedRootDirectory) -> io::Result<QualifiedVolume> {
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

    #[test]
    fn replacement_policy_retains_both_exact_observations() {
        let source = ObjectInfo {
            identity: FileIdentity::new(1, [1; 16]),
            kind: ObjectKind::RegularFile,
            byte_len: 10,
            link_count: 1,
            readonly: false,
        };
        let target = ObjectInfo {
            identity: FileIdentity::new(1, [2; 16]),
            kind: ObjectKind::RegularFile,
            byte_len: 20,
            link_count: 2,
            readonly: false,
        };
        let policy = ReplacementPolicy::new(source, target);
        assert_eq!(policy.source_before(), source);
        assert_eq!(policy.target_before(), target);
    }

    #[test]
    fn move_policy_retains_the_complete_source_observation() {
        let source = ObjectInfo {
            identity: FileIdentity::new(7, [9; 16]),
            kind: ObjectKind::RegularFile,
            byte_len: 42,
            link_count: 1,
            readonly: false,
        };
        assert_eq!(MovePolicy::new(source).source_before(), source);
    }

    #[test]
    fn hard_link_policies_retain_the_complete_source_observation() {
        let source = ObjectInfo {
            identity: FileIdentity::new(7, [11; 16]),
            kind: ObjectKind::RegularFile,
            byte_len: 31,
            link_count: 2,
            readonly: false,
        };
        assert_eq!(HardLinkPolicy::new(source).source_before(), source);
        assert_eq!(
            HardLinkRetirementPolicy::new(source).linked_before(),
            source
        );
    }

    #[test]
    fn windows_ffi_source_has_one_narrow_unsafe_allowance() {
        let root_source = include_str!("lib.rs");
        let windows_source = include_str!("windows.rs");
        let manifest = include_str!("../Cargo.toml");
        let allowance = ["allow", "(unsafe_code)"].concat();
        let module_allowance = ["#![", allowance.as_str(), "]"].concat();
        assert!(!root_source.contains(&allowance));
        assert_eq!(windows_source.matches(&module_allowance).count(), 1);
        assert_eq!(windows_source.matches(&allowance).count(), 1);
        assert_eq!(manifest.matches("\"src/").count(), 2);
        assert!(manifest.contains("include = [\"src/lib.rs\", \"src/windows.rs\"]"));
    }

    #[test]
    fn windows_child_lookup_and_creation_are_explicitly_case_insensitive() {
        let windows_source = include_str!("windows.rs");
        assert_eq!(
            windows_source
                .matches("object_attributes(OBJ_CASE_INSENSITIVE)")
                .count(),
            1
        );
        assert_eq!(
            windows_source
                .matches("Attributes: OBJ_CASE_INSENSITIVE")
                .count(),
            1
        );
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
