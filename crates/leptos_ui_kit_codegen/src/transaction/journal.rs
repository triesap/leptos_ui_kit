use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

pub(super) const JOURNAL_VERSION: u32 = 2;
pub(super) const TRANSACTION_ID_HEX_LEN: usize = 32;
pub(super) const SEQUENCE_DECIMAL_WIDTH: usize = 20;
pub(super) const ORDINAL_DECIMAL_WIDTH: usize = 8;

const MAX_ORDINAL: u32 = 99_999_999;
const SHA256_HEX_LEN: usize = 64;
const SHA256_PREFIX: &str = "sha256:";
const TRANSACTION_PREFIX: &str = "transaction-v2-";
const FINALIZATION_PREFIX: &str = "finalization-v2-";
const JOURNAL_SUFFIX: &str = ".json";
const PARTIAL_SUFFIX: &str = ".json.partial";
const STAGE_PREFIX: &str = ".leptos-ui-kit-stage-v2-";
const BACKUP_PREFIX: &str = ".leptos-ui-kit-backup-v2-";
const PARTIAL_MAGIC: &str = "leptos-ui-kit-journal-partial-v2";
const BOOTSTRAP_MAGIC: &str = "leptos-ui-kit-workspace-bootstrap-v2";
const BOOTSTRAP_PREFIX: &str = "bootstrap-v2-";
const BOOTSTRAP_INTENT_MAGIC: &str = "leptos-ui-kit-workspace-bootstrap-intent-v2";
const BOOTSTRAP_INTENT_PREFIX: &str = "bootstrap-intent-v2-";
const DIRECTORY_CANDIDATE_PREFIX: &str = ".leptos-ui-kit-directory-v2-";
const WORKSPACE_PARENT_LOGICAL_PATH: &str = "src/components/ui/_kit";
const CANONICAL_ROOT_HASH_DOMAIN: &[u8] = b"leptos-ui-kit:canonical-root:v2\0";
const WORKSPACE_OWNER_DOMAIN: &[u8] = b"leptos-ui-kit:workspace-owner:v2\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JournalModelError {
    reason: String,
}

impl JournalModelError {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    pub(super) fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for JournalModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl Error for JournalModelError {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct TransactionId(String);

impl TransactionId {
    pub(super) fn parse(value: &str) -> Result<Self, JournalModelError> {
        if value.len() != TRANSACTION_ID_HEX_LEN
            || !value
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(JournalModelError::new(format!(
                "transaction identifier must be exactly {TRANSACTION_ID_HEX_LEN} lowercase hexadecimal characters"
            )));
        }
        Ok(Self(value.to_owned()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for TransactionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TransactionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Sha256Digest(String);

impl Sha256Digest {
    pub(super) fn parse(value: &str) -> Result<Self, JournalModelError> {
        let Some(hex) = value.strip_prefix(SHA256_PREFIX) else {
            return Err(JournalModelError::new(
                "SHA-256 digest must use sha256:<lowercase-hex>",
            ));
        };
        if hex.len() != SHA256_HEX_LEN
            || !hex
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(JournalModelError::new(format!(
                "SHA-256 digest must contain exactly {SHA256_HEX_LEN} lowercase hexadecimal characters"
            )));
        }
        Ok(Self(value.to_owned()))
    }

    fn from_digest(bytes: &[u8]) -> Self {
        let mut value = String::with_capacity(SHA256_PREFIX.len() + SHA256_HEX_LEN);
        value.push_str(SHA256_PREFIX);
        for byte in bytes {
            use fmt::Write as _;
            write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Self(value)
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

fn hash_with_domain(domain: &[u8], parts: &[&[u8]]) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    Sha256Digest::from_digest(&hasher.finalize())
}

pub(super) fn canonical_root_hash(canonical_native_bytes: &[u8]) -> Sha256Digest {
    hash_with_domain(CANONICAL_ROOT_HASH_DOMAIN, &[canonical_native_bytes])
}

fn content_hash(bytes: &[u8]) -> Sha256Digest {
    let digest = Sha256::digest(bytes);
    Sha256Digest::from_digest(&digest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub(super) struct ArtifactOrdinal(u32);

impl ArtifactOrdinal {
    pub(super) fn new(value: u32) -> Result<Self, JournalModelError> {
        if value > MAX_ORDINAL {
            return Err(JournalModelError::new(format!(
                "artifact ordinal exceeds the fixed {ORDINAL_DECIMAL_WIDTH}-digit namespace"
            )));
        }
        Ok(Self(value))
    }

    pub(super) fn get(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ArtifactOrdinal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u32::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ObjectIdentityV2 {
    namespace: [u8; 16],
    object: [u8; 16],
}

impl ObjectIdentityV2 {
    pub(super) const fn new(device: u64, inode: u64) -> Self {
        Self::from_u128(device as u128, inode as u128)
    }

    pub(super) const fn from_u128(namespace: u128, object: u128) -> Self {
        Self {
            namespace: namespace.to_le_bytes(),
            object: object.to_le_bytes(),
        }
    }

    pub(super) const fn namespace(self) -> u128 {
        u128::from_le_bytes(self.namespace)
    }

    pub(super) const fn object(self) -> u128 {
        u128::from_le_bytes(self.object)
    }

    pub(super) const fn device(self) -> u64 {
        self.namespace() as u64
    }

    pub(super) const fn inode(self) -> u64 {
        self.object() as u64
    }

    fn hash_parts(&self) -> (&[u8], &[u8]) {
        (&self.namespace, &self.object)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct FileStateV2 {
    content_hash: Sha256Digest,
    byte_len: u64,
    readonly: bool,
    posix_mode: Option<u32>,
}

impl FileStateV2 {
    pub(super) fn new(
        content_hash: Sha256Digest,
        byte_len: u64,
        readonly: bool,
        posix_mode: Option<u32>,
    ) -> Result<Self, JournalModelError> {
        validate_posix_mode(posix_mode)?;
        Ok(Self {
            content_hash,
            byte_len,
            readonly,
            posix_mode,
        })
    }

    pub(super) fn content_hash(&self) -> &Sha256Digest {
        &self.content_hash
    }

    pub(super) const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub(super) const fn readonly(&self) -> bool {
        self.readonly
    }

    pub(super) const fn posix_mode(&self) -> Option<u32> {
        self.posix_mode
    }

    fn validate(&self) -> Result<(), JournalModelError> {
        Sha256Digest::parse(self.content_hash.as_str())?;
        validate_posix_mode(self.posix_mode)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum FileModePolicyV2 {
    PreservePreimage,
    NormalCreateResolveOnStage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct PlannedFileStateV2 {
    content_hash: Sha256Digest,
    byte_len: u64,
    mode_policy: FileModePolicyV2,
}

impl PlannedFileStateV2 {
    pub(super) fn new(
        content_hash: Sha256Digest,
        byte_len: u64,
        mode_policy: FileModePolicyV2,
    ) -> Result<Self, JournalModelError> {
        Sha256Digest::parse(content_hash.as_str())?;
        Ok(Self {
            content_hash,
            byte_len,
            mode_policy,
        })
    }

    pub(super) fn content_hash(&self) -> &Sha256Digest {
        &self.content_hash
    }

    pub(super) const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub(super) const fn mode_policy(&self) -> FileModePolicyV2 {
        self.mode_policy
    }

    fn validate(&self) -> Result<(), JournalModelError> {
        Sha256Digest::parse(self.content_hash.as_str()).map(|_| ())
    }

    fn matches_resolved(&self, state: &FileStateV2, preimage: &PreimageV2) -> bool {
        if state.content_hash != self.content_hash
            || state.byte_len != self.byte_len
            || state.readonly
        {
            return false;
        }
        match self.mode_policy {
            FileModePolicyV2::NormalCreateResolveOnStage => {
                matches!(preimage, PreimageV2::Absent)
            }
            FileModePolicyV2::PreservePreimage => match preimage {
                PreimageV2::Regular { exact } => state.posix_mode == exact.state.posix_mode,
                PreimageV2::Absent => false,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct DirectoryModeV2 {
    readonly: bool,
    posix_mode: Option<u32>,
}

impl DirectoryModeV2 {
    pub(super) fn new(readonly: bool, posix_mode: Option<u32>) -> Result<Self, JournalModelError> {
        validate_posix_mode(posix_mode)?;
        Ok(Self {
            readonly,
            posix_mode,
        })
    }

    pub(super) const fn readonly(self) -> bool {
        self.readonly
    }

    pub(super) const fn posix_mode(self) -> Option<u32> {
        self.posix_mode
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ExactFileStateV2 {
    identity: ObjectIdentityV2,
    state: FileStateV2,
    link_count: u64,
}

impl ExactFileStateV2 {
    pub(super) fn new(
        identity: ObjectIdentityV2,
        state: FileStateV2,
        link_count: u64,
    ) -> Result<Self, JournalModelError> {
        let exact = Self {
            identity,
            state,
            link_count,
        };
        exact.validate()?;
        Ok(exact)
    }

    pub(super) const fn identity(&self) -> ObjectIdentityV2 {
        self.identity
    }

    pub(super) fn state(&self) -> &FileStateV2 {
        &self.state
    }

    pub(super) const fn link_count(&self) -> u64 {
        self.link_count
    }

    fn with_link_count(&self, link_count: u64) -> Result<Self, JournalModelError> {
        Self::new(self.identity, self.state.clone(), link_count)
    }

    fn validate(&self) -> Result<(), JournalModelError> {
        self.state.validate()?;
        if self.link_count == 0 {
            return Err(JournalModelError::new(
                "an exact regular-file state must have a positive link count",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ExactDirectoryStateV2 {
    identity: ObjectIdentityV2,
    mode: DirectoryModeV2,
    link_count: u64,
}

impl ExactDirectoryStateV2 {
    pub(super) fn new(
        identity: ObjectIdentityV2,
        mode: DirectoryModeV2,
        link_count: u64,
    ) -> Result<Self, JournalModelError> {
        if link_count == 0 {
            return Err(JournalModelError::new(
                "an exact directory state must have a positive link count",
            ));
        }
        Ok(Self {
            identity,
            mode,
            link_count,
        })
    }

    pub(super) const fn identity(&self) -> ObjectIdentityV2 {
        self.identity
    }

    pub(super) const fn mode(&self) -> DirectoryModeV2 {
        self.mode
    }

    pub(super) const fn link_count(&self) -> u64 {
        self.link_count
    }

    fn validate(&self) -> Result<(), JournalModelError> {
        validate_posix_mode(self.mode.posix_mode)?;
        if self.link_count == 0 {
            return Err(JournalModelError::new(
                "an exact directory state must have a positive link count",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    content = "exact",
    rename_all = "camelCase"
)]
pub(super) enum PresenceV2<T> {
    Missing,
    Present(T),
}

impl<T> PresenceV2<T> {
    pub(super) const fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    pub(super) const fn as_present(&self) -> Option<&T> {
        match self {
            Self::Missing => None,
            Self::Present(value) => Some(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct WorkspaceBindingV2 {
    name: String,
    exact: ExactDirectoryStateV2,
    owner_tag: Sha256Digest,
}

impl WorkspaceBindingV2 {
    pub(super) fn new(
        transaction_id: &TransactionId,
        canonical_root_hash: &Sha256Digest,
        workspace_parent: &ExactDirectoryStateV2,
        exact: ExactDirectoryStateV2,
    ) -> Result<Self, JournalModelError> {
        let name = transaction_directory_name(transaction_id);
        require_private_directory_mode(&exact, 0o700, "transaction workspace")?;
        let owner_tag = workspace_owner_tag(
            transaction_id,
            canonical_root_hash,
            workspace_parent,
            &name,
            exact.identity,
        );
        Ok(Self {
            name,
            exact,
            owner_tag,
        })
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn exact(&self) -> &ExactDirectoryStateV2 {
        &self.exact
    }

    pub(super) fn owner_tag(&self) -> &Sha256Digest {
        &self.owner_tag
    }

    fn validate(
        &self,
        transaction_id: &TransactionId,
        canonical_root_hash: &Sha256Digest,
        workspace_parent: &ExactDirectoryStateV2,
    ) -> Result<(), JournalModelError> {
        if self.name != transaction_directory_name(transaction_id) {
            return Err(JournalModelError::new(
                "workspace name is not bound to its transaction identifier",
            ));
        }
        require_private_directory_mode(&self.exact, 0o700, "transaction workspace")?;
        let expected = workspace_owner_tag(
            transaction_id,
            canonical_root_hash,
            workspace_parent,
            &self.name,
            self.exact.identity,
        );
        if self.owner_tag != expected {
            return Err(JournalModelError::new(
                "workspace ownership tag does not match its exact project binding",
            ));
        }
        Ok(())
    }
}

fn workspace_owner_tag(
    transaction_id: &TransactionId,
    canonical_root_hash: &Sha256Digest,
    workspace_parent: &ExactDirectoryStateV2,
    workspace_name: &str,
    identity: ObjectIdentityV2,
) -> Sha256Digest {
    let (parent_namespace, parent_object) = workspace_parent.identity.hash_parts();
    let (workspace_namespace, workspace_object) = identity.hash_parts();
    hash_with_domain(
        WORKSPACE_OWNER_DOMAIN,
        &[
            transaction_id.as_str().as_bytes(),
            canonical_root_hash.as_str().as_bytes(),
            WORKSPACE_PARENT_LOGICAL_PATH.as_bytes(),
            parent_namespace,
            parent_object,
            workspace_name.as_bytes(),
            workspace_namespace,
            workspace_object,
        ],
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ProjectBindingV2 {
    canonical_root_hash: Sha256Digest,
    root_preimage: ExactDirectoryStateV2,
    root_current: ExactDirectoryStateV2,
    write_lock: ExactFileStateV2,
    workspace_parent_preimage: ExactDirectoryStateV2,
    workspace_parent_after_workspace: ExactDirectoryStateV2,
    workspace_parent_current: ExactDirectoryStateV2,
    workspace: WorkspaceBindingV2,
}

impl ProjectBindingV2 {
    pub(super) fn new(
        transaction_id: &TransactionId,
        canonical_root_hash: Sha256Digest,
        root: ExactDirectoryStateV2,
        write_lock: ExactFileStateV2,
        workspace_parent_preimage: ExactDirectoryStateV2,
        workspace_parent_after_workspace: ExactDirectoryStateV2,
        workspace: ExactDirectoryStateV2,
    ) -> Result<Self, JournalModelError> {
        validate_parent_creation_transition(
            &workspace_parent_preimage,
            &workspace_parent_after_workspace,
        )?;
        let binding = Self {
            workspace: WorkspaceBindingV2::new(
                transaction_id,
                &canonical_root_hash,
                &workspace_parent_after_workspace,
                workspace,
            )?,
            canonical_root_hash,
            root_preimage: root.clone(),
            root_current: root,
            write_lock,
            workspace_parent_preimage,
            workspace_parent_current: workspace_parent_after_workspace.clone(),
            workspace_parent_after_workspace,
        };
        binding.validate(transaction_id)?;
        Ok(binding)
    }

    pub(super) fn canonical_root_hash(&self) -> &Sha256Digest {
        &self.canonical_root_hash
    }

    pub(super) fn root_preimage(&self) -> &ExactDirectoryStateV2 {
        &self.root_preimage
    }

    pub(super) fn root_current(&self) -> &ExactDirectoryStateV2 {
        &self.root_current
    }

    pub(super) fn write_lock(&self) -> &ExactFileStateV2 {
        &self.write_lock
    }

    pub(super) fn workspace_parent_preimage(&self) -> &ExactDirectoryStateV2 {
        &self.workspace_parent_preimage
    }

    pub(super) fn workspace_parent_after_workspace(&self) -> &ExactDirectoryStateV2 {
        &self.workspace_parent_after_workspace
    }

    pub(super) fn workspace_parent_current(&self) -> &ExactDirectoryStateV2 {
        &self.workspace_parent_current
    }

    pub(super) fn workspace(&self) -> &WorkspaceBindingV2 {
        &self.workspace
    }

    fn validate(&self, transaction_id: &TransactionId) -> Result<(), JournalModelError> {
        Sha256Digest::parse(self.canonical_root_hash.as_str())?;
        self.root_preimage.validate()?;
        self.root_current.validate()?;
        if self.root_preimage.identity != self.root_current.identity
            || self.root_preimage.mode != self.root_current.mode
        {
            return Err(JournalModelError::new(
                "project-root identity and mode cannot change within a transaction",
            ));
        }
        self.write_lock.validate()?;
        require_private_file_mode(&self.write_lock, 0o600, "write lock")?;
        if self.write_lock.link_count != 1 {
            return Err(JournalModelError::new(
                "persistent write lock must be independently linked with no unowned alias",
            ));
        }
        self.workspace_parent_preimage.validate()?;
        self.workspace_parent_after_workspace.validate()?;
        self.workspace_parent_current.validate()?;
        validate_parent_creation_transition(
            &self.workspace_parent_preimage,
            &self.workspace_parent_after_workspace,
        )?;
        if self.workspace_parent_after_workspace.identity != self.workspace_parent_current.identity
            || self.workspace_parent_after_workspace.mode != self.workspace_parent_current.mode
            || self.workspace_parent_current.link_count
                < self.workspace_parent_after_workspace.link_count
        {
            return Err(JournalModelError::new(
                "workspace-parent current state cannot predate or substitute the exact post-workspace parent",
            ));
        }
        self.workspace.validate(
            transaction_id,
            &self.canonical_root_hash,
            &self.workspace_parent_after_workspace,
        )?;
        let identities = [
            self.root_current.identity,
            self.write_lock.identity,
            self.workspace_parent_current.identity,
            self.workspace.exact.identity,
        ];
        if identities.iter().copied().collect::<BTreeSet<_>>().len() != identities.len() {
            return Err(JournalModelError::new(
                "project root, write lock, workspace parent, and transaction workspace must have distinct identities",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum JournalOperationV2 {
    Init,
    Add,
    Sync,
    AtomicWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum EntryActionV2 {
    Create,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum EntryRoleV2 {
    Ordinary,
    InstallLock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "camelCase")]
pub(super) enum PreimageV2 {
    Absent,
    Regular { exact: ExactFileStateV2 },
}

impl PreimageV2 {
    pub(super) const fn regular(exact: ExactFileStateV2) -> Self {
        Self::Regular { exact }
    }

    fn presence(&self) -> PresenceV2<ExactFileStateV2> {
        match self {
            Self::Absent => PresenceV2::Missing,
            Self::Regular { exact } => PresenceV2::Present(exact.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ArtifactV2 {
    name: String,
    prepared: Option<ExactFileStateV2>,
    current: PresenceV2<ExactFileStateV2>,
}

impl ArtifactV2 {
    fn missing(name: String) -> Self {
        Self {
            name,
            prepared: None,
            current: PresenceV2::Missing,
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn current(&self) -> &PresenceV2<ExactFileStateV2> {
        &self.current
    }

    pub(super) fn prepared(&self) -> Option<&ExactFileStateV2> {
        self.prepared.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct JournalEntryV2 {
    ordinal: ArtifactOrdinal,
    logical_path: String,
    action: EntryActionV2,
    role: EntryRoleV2,
    preimage: PreimageV2,
    planned: PlannedFileStateV2,
    current_target: PresenceV2<ExactFileStateV2>,
    stage: ArtifactV2,
    backup: Option<ArtifactV2>,
}

impl JournalEntryV2 {
    pub(super) fn new(
        transaction_id: &TransactionId,
        ordinal: ArtifactOrdinal,
        logical_path: impl Into<String>,
        action: EntryActionV2,
        role: EntryRoleV2,
        preimage: PreimageV2,
        planned: PlannedFileStateV2,
    ) -> Result<Self, JournalModelError> {
        let current_target = preimage.presence();
        let entry = Self {
            ordinal,
            logical_path: logical_path.into(),
            action,
            role,
            preimage,
            planned,
            current_target,
            stage: ArtifactV2::missing(stage_name(transaction_id, ordinal)),
            backup: (action == EntryActionV2::Replace)
                .then(|| ArtifactV2::missing(backup_name(transaction_id, ordinal))),
        };
        entry.validate_static(transaction_id)?;
        Ok(entry)
    }

    pub(super) const fn ordinal(&self) -> ArtifactOrdinal {
        self.ordinal
    }

    pub(super) fn logical_path(&self) -> &str {
        &self.logical_path
    }

    pub(super) const fn action(&self) -> EntryActionV2 {
        self.action
    }

    pub(super) const fn role(&self) -> EntryRoleV2 {
        self.role
    }

    pub(super) fn preimage(&self) -> &PreimageV2 {
        &self.preimage
    }

    pub(super) fn planned(&self) -> &PlannedFileStateV2 {
        &self.planned
    }

    pub(super) fn current_target(&self) -> &PresenceV2<ExactFileStateV2> {
        &self.current_target
    }

    pub(super) fn stage(&self) -> &ArtifactV2 {
        &self.stage
    }

    pub(super) fn backup(&self) -> Option<&ArtifactV2> {
        self.backup.as_ref()
    }

    fn resolved_planned_state(&self) -> Option<&FileStateV2> {
        self.stage.prepared.as_ref().map(|exact| &exact.state)
    }

    fn validate_static(&self, transaction_id: &TransactionId) -> Result<(), JournalModelError> {
        validate_logical_path(&self.logical_path)?;
        self.planned.validate()?;
        if self.stage.name != stage_name(transaction_id, self.ordinal) {
            return Err(JournalModelError::new(format!(
                "entry {} has a non-deterministic stage name",
                self.logical_path
            )));
        }
        match (&self.action, &self.preimage, &self.backup) {
            (EntryActionV2::Create, PreimageV2::Absent, None) => {}
            (EntryActionV2::Replace, PreimageV2::Regular { exact }, Some(backup)) => {
                exact.validate()?;
                if exact.link_count != 1 || exact.state.readonly {
                    return Err(JournalModelError::new(format!(
                        "replace preimage {} must be writable and independently linked",
                        self.logical_path
                    )));
                }
                if self.planned.mode_policy != FileModePolicyV2::PreservePreimage {
                    return Err(JournalModelError::new(format!(
                        "replacement {} must use preserve-preimage mode policy",
                        self.logical_path
                    )));
                }
                if backup.name != backup_name(transaction_id, self.ordinal) {
                    return Err(JournalModelError::new(format!(
                        "entry {} has a non-deterministic backup name",
                        self.logical_path
                    )));
                }
            }
            _ => {
                return Err(JournalModelError::new(format!(
                    "entry {} has inconsistent action, preimage, and backup fields",
                    self.logical_path
                )));
            }
        }
        if self.action == EntryActionV2::Create
            && self.planned.mode_policy != FileModePolicyV2::NormalCreateResolveOnStage
        {
            return Err(JournalModelError::new(format!(
                "created file {} must resolve normal-create mode from its exact stage",
                self.logical_path
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum DirectoryDispositionV2 {
    Existing,
    Create,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ManagedChildKindV2 {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ManagedChildV2 {
    name: String,
    kind: ManagedChildKindV2,
}

impl ManagedChildV2 {
    fn new(name: impl Into<String>, kind: ManagedChildKindV2) -> Self {
        Self {
            name: name.into(),
            kind,
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) const fn kind(&self) -> ManagedChildKindV2 {
        self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct JournalDirectoryV2 {
    ordinal: ArtifactOrdinal,
    logical_path: String,
    disposition: DirectoryDispositionV2,
    planned_mode: DirectoryModeV2,
    preimage: PresenceV2<ExactDirectoryStateV2>,
    candidate_name: Option<String>,
    created_exact: Option<ExactDirectoryStateV2>,
    candidate_current: PresenceV2<ExactDirectoryStateV2>,
    current: PresenceV2<ExactDirectoryStateV2>,
    managed_children: Vec<ManagedChildV2>,
}

impl JournalDirectoryV2 {
    pub(super) fn existing(
        ordinal: ArtifactOrdinal,
        logical_path: impl Into<String>,
        exact: ExactDirectoryStateV2,
    ) -> Result<Self, JournalModelError> {
        exact.validate()?;
        let directory = Self {
            ordinal,
            logical_path: logical_path.into(),
            disposition: DirectoryDispositionV2::Existing,
            planned_mode: exact.mode,
            preimage: PresenceV2::Present(exact.clone()),
            candidate_name: None,
            created_exact: None,
            candidate_current: PresenceV2::Missing,
            current: PresenceV2::Present(exact),
            managed_children: Vec::new(),
        };
        validate_logical_path(&directory.logical_path)?;
        Ok(directory)
    }

    pub(super) fn create(
        transaction_id: &TransactionId,
        ordinal: ArtifactOrdinal,
        logical_path: impl Into<String>,
        planned_mode: DirectoryModeV2,
    ) -> Result<Self, JournalModelError> {
        if planned_mode.readonly {
            return Err(JournalModelError::new(
                "a transaction-created directory must be writable",
            ));
        }
        let directory = Self {
            ordinal,
            logical_path: logical_path.into(),
            disposition: DirectoryDispositionV2::Create,
            planned_mode,
            preimage: PresenceV2::Missing,
            candidate_name: Some(directory_candidate_name(transaction_id, ordinal)),
            created_exact: None,
            candidate_current: PresenceV2::Missing,
            current: PresenceV2::Missing,
            managed_children: Vec::new(),
        };
        validate_logical_path(&directory.logical_path)?;
        Ok(directory)
    }

    pub(super) const fn ordinal(&self) -> ArtifactOrdinal {
        self.ordinal
    }

    pub(super) fn logical_path(&self) -> &str {
        &self.logical_path
    }

    pub(super) const fn disposition(&self) -> DirectoryDispositionV2 {
        self.disposition
    }

    pub(super) const fn planned_mode(&self) -> DirectoryModeV2 {
        self.planned_mode
    }

    pub(super) fn preimage(&self) -> &PresenceV2<ExactDirectoryStateV2> {
        &self.preimage
    }

    pub(super) fn current(&self) -> &PresenceV2<ExactDirectoryStateV2> {
        &self.current
    }

    pub(super) fn candidate_name(&self) -> Option<&str> {
        self.candidate_name.as_deref()
    }

    pub(super) fn candidate_current(&self) -> &PresenceV2<ExactDirectoryStateV2> {
        &self.candidate_current
    }

    pub(super) fn created_exact(&self) -> Option<&ExactDirectoryStateV2> {
        self.created_exact.as_ref()
    }

    pub(super) fn managed_children(&self) -> &[ManagedChildV2] {
        &self.managed_children
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "camelCase")]
pub(super) enum DirectoryParentV2 {
    ProjectRoot,
    Cohort { ordinal: ArtifactOrdinal },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PreparationObservationV2 {
    DirectoryCandidate {
        exact: ExactDirectoryStateV2,
        parent_after: ExactDirectoryStateV2,
    },
    Stage {
        exact: ExactFileStateV2,
    },
    Backup {
        exact: ExactFileStateV2,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct DirectoryPublishIntentV2 {
    ordinal: ArtifactOrdinal,
    candidate_name: String,
    expected_candidate: ExactDirectoryStateV2,
    parent: DirectoryParentV2,
    parent_before: ExactDirectoryStateV2,
}

impl DirectoryPublishIntentV2 {
    pub(super) fn new(
        ordinal: ArtifactOrdinal,
        candidate_name: impl Into<String>,
        expected_candidate: ExactDirectoryStateV2,
        parent: DirectoryParentV2,
        parent_before: ExactDirectoryStateV2,
    ) -> Self {
        Self {
            ordinal,
            candidate_name: candidate_name.into(),
            expected_candidate,
            parent,
            parent_before,
        }
    }

    pub(super) const fn ordinal(&self) -> ArtifactOrdinal {
        self.ordinal
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DirectoryPublicationObservationV2 {
    target: ExactDirectoryStateV2,
    candidate: PresenceV2<ExactDirectoryStateV2>,
    parent_after: ExactDirectoryStateV2,
}

impl DirectoryPublicationObservationV2 {
    pub(super) const fn new(
        target: ExactDirectoryStateV2,
        candidate: PresenceV2<ExactDirectoryStateV2>,
        parent_after: ExactDirectoryStateV2,
    ) -> Self {
        Self {
            target,
            candidate,
            parent_after,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectoryPublicationWorldV2 {
    CandidatePrivate,
    CandidateReady,
    Published,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReplacementObservationV2 {
    target: ExactFileStateV2,
    stage: PresenceV2<ExactFileStateV2>,
}

impl ReplacementObservationV2 {
    pub(super) const fn new(target: ExactFileStateV2, stage: PresenceV2<ExactFileStateV2>) -> Self {
        Self { target, stage }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "camelCase")]
pub(super) enum CleanupTargetV2 {
    Stage { ordinal: ArtifactOrdinal },
    Backup { ordinal: ArtifactOrdinal },
    CreatedDirectory { ordinal: ArtifactOrdinal },
    DirectoryCandidate { ordinal: ArtifactOrdinal },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct CleanupPlansV2 {
    commit: Vec<CleanupTargetV2>,
    rollback: Vec<CleanupTargetV2>,
}

impl CleanupPlansV2 {
    pub(super) fn commit(&self) -> &[CleanupTargetV2] {
        &self.commit
    }

    pub(super) fn rollback(&self) -> &[CleanupTargetV2] {
        &self.rollback
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "camelCase")]
pub(super) enum RollbackIntentV2 {
    RemoveCreatedTarget {
        ordinal: ArtifactOrdinal,
        expected_target: ExactFileStateV2,
    },
    RestoreBackup {
        ordinal: ArtifactOrdinal,
        expected_target: ExactFileStateV2,
        expected_backup: ExactFileStateV2,
    },
}

impl RollbackIntentV2 {
    pub(super) const fn ordinal(&self) -> ArtifactOrdinal {
        match self {
            Self::RemoveCreatedTarget { ordinal, .. } | Self::RestoreBackup { ordinal, .. } => {
                *ordinal
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "camelCase")]
pub(super) enum CleanupIntentV2 {
    RemoveFile {
        target: CleanupTargetV2,
        expected: ExactFileStateV2,
    },
    RemoveDirectory {
        target: CleanupTargetV2,
        expected: ExactDirectoryStateV2,
        parent: DirectoryParentV2,
        parent_before: ExactDirectoryStateV2,
    },
}

impl CleanupIntentV2 {
    pub(super) const fn target(&self) -> CleanupTargetV2 {
        match self {
            Self::RemoveFile { target, .. } | Self::RemoveDirectory { target, .. } => *target,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "camelCase")]
pub(super) enum JournalPhaseV2 {
    Preparing {
        completed: u32,
        pending_directory: Option<DirectoryPublishIntentV2>,
    },
    Prepared,
    Replacing {
        committed: u32,
    },
    RollingBack {
        next: u32,
        pending: Option<RollbackIntentV2>,
    },
    RollbackComplete {
        cleanup_completed: u32,
        pending: Option<CleanupIntentV2>,
    },
    CommitComplete {
        cleanup_completed: u32,
        pending: Option<CleanupIntentV2>,
    },
}

impl JournalPhaseV2 {
    pub(super) const fn desired_state_is_irreversible(&self) -> bool {
        matches!(self, Self::CommitComplete { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct RecordBindingV2 {
    sequence: u64,
    name: String,
    exact: ExactFileStateV2,
}

impl RecordBindingV2 {
    pub(super) fn new(
        sequence: u64,
        name: impl Into<String>,
        exact: ExactFileStateV2,
    ) -> Result<Self, JournalModelError> {
        exact.validate()?;
        Ok(Self {
            sequence,
            name: name.into(),
            exact,
        })
    }

    pub(super) const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn exact(&self) -> &ExactFileStateV2 {
        &self.exact
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct WorkspaceBootstrapIntentEnvelopeV2 {
    magic: String,
    version: u32,
    transaction_id: TransactionId,
    canonical_root_hash: Sha256Digest,
    workspace_parent_preimage: ExactDirectoryStateV2,
    workspace_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WorkspaceBootstrapIntentEnvelopeWireV2 {
    magic: String,
    version: u32,
    transaction_id: TransactionId,
    canonical_root_hash: Sha256Digest,
    workspace_parent_preimage: ExactDirectoryStateV2,
    workspace_name: String,
}

impl<'de> Deserialize<'de> for WorkspaceBootstrapIntentEnvelopeV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WorkspaceBootstrapIntentEnvelopeWireV2::deserialize(deserializer)?;
        let envelope = Self {
            magic: wire.magic,
            version: wire.version,
            transaction_id: wire.transaction_id,
            canonical_root_hash: wire.canonical_root_hash,
            workspace_parent_preimage: wire.workspace_parent_preimage,
            workspace_name: wire.workspace_name,
        };
        envelope.validate().map_err(D::Error::custom)?;
        Ok(envelope)
    }
}

impl WorkspaceBootstrapIntentEnvelopeV2 {
    pub(super) fn new(
        transaction_id: TransactionId,
        canonical_root_hash: Sha256Digest,
        workspace_parent_preimage: ExactDirectoryStateV2,
    ) -> Result<Self, JournalModelError> {
        let envelope = Self {
            magic: BOOTSTRAP_INTENT_MAGIC.to_owned(),
            version: JOURNAL_VERSION,
            workspace_name: transaction_directory_name(&transaction_id),
            transaction_id,
            canonical_root_hash,
            workspace_parent_preimage,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub(super) fn to_json_bytes(&self) -> Result<Vec<u8>, JournalModelError> {
        self.validate()?;
        let mut bytes = serde_json::to_vec(self).map_err(|error| {
            JournalModelError::new(format!(
                "could not serialize workspace-bootstrap intent: {error}"
            ))
        })?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn validate(&self) -> Result<(), JournalModelError> {
        if self.magic != BOOTSTRAP_INTENT_MAGIC
            || self.version != JOURNAL_VERSION
            || self.workspace_name != transaction_directory_name(&self.transaction_id)
        {
            return Err(JournalModelError::new(
                "workspace-bootstrap intent has invalid magic/version/name",
            ));
        }
        Sha256Digest::parse(self.canonical_root_hash.as_str())?;
        self.workspace_parent_preimage.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct WorkspaceBootstrapIntentBindingV2 {
    name: String,
    exact: ExactFileStateV2,
    envelope: WorkspaceBootstrapIntentEnvelopeV2,
}

impl WorkspaceBootstrapIntentBindingV2 {
    pub(super) fn new(
        envelope: WorkspaceBootstrapIntentEnvelopeV2,
        exact: ExactFileStateV2,
    ) -> Result<Self, JournalModelError> {
        let binding = Self {
            name: bootstrap_intent_name(&envelope.transaction_id),
            exact,
            envelope,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub(super) fn exact(&self) -> &ExactFileStateV2 {
        &self.exact
    }

    fn validate(&self) -> Result<(), JournalModelError> {
        self.envelope.validate()?;
        if self.name != bootstrap_intent_name(&self.envelope.transaction_id) {
            return Err(JournalModelError::new(
                "workspace-bootstrap intent binding has a non-canonical name",
            ));
        }
        require_private_file_mode(&self.exact, 0o600, "workspace-bootstrap intent")?;
        let bytes = self.envelope.to_json_bytes()?;
        if self.exact.link_count != 1
            || self.exact.state.content_hash != content_hash(&bytes)
            || self.exact.state.byte_len != bytes.len() as u64
        {
            return Err(JournalModelError::new(
                "workspace-bootstrap intent is not an independent exact canonical envelope",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct WorkspaceBootstrapEnvelopeV2 {
    magic: String,
    version: u32,
    transaction_id: TransactionId,
    canonical_root_hash: Sha256Digest,
    owner_tag: Sha256Digest,
    workspace_parent_preimage: ExactDirectoryStateV2,
    workspace_parent_after_workspace: ExactDirectoryStateV2,
    workspace_name: String,
    workspace_exact: ExactDirectoryStateV2,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WorkspaceBootstrapEnvelopeWireV2 {
    magic: String,
    version: u32,
    transaction_id: TransactionId,
    canonical_root_hash: Sha256Digest,
    owner_tag: Sha256Digest,
    workspace_parent_preimage: ExactDirectoryStateV2,
    workspace_parent_after_workspace: ExactDirectoryStateV2,
    workspace_name: String,
    workspace_exact: ExactDirectoryStateV2,
}

impl<'de> Deserialize<'de> for WorkspaceBootstrapEnvelopeV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WorkspaceBootstrapEnvelopeWireV2::deserialize(deserializer)?;
        let envelope = Self {
            magic: wire.magic,
            version: wire.version,
            transaction_id: wire.transaction_id,
            canonical_root_hash: wire.canonical_root_hash,
            owner_tag: wire.owner_tag,
            workspace_parent_preimage: wire.workspace_parent_preimage,
            workspace_parent_after_workspace: wire.workspace_parent_after_workspace,
            workspace_name: wire.workspace_name,
            workspace_exact: wire.workspace_exact,
        };
        envelope.validate().map_err(D::Error::custom)?;
        Ok(envelope)
    }
}

impl WorkspaceBootstrapEnvelopeV2 {
    fn for_project(transaction_id: &TransactionId, project: &ProjectBindingV2) -> Self {
        Self {
            magic: BOOTSTRAP_MAGIC.to_owned(),
            version: JOURNAL_VERSION,
            transaction_id: transaction_id.clone(),
            canonical_root_hash: project.canonical_root_hash.clone(),
            owner_tag: project.workspace.owner_tag.clone(),
            workspace_parent_preimage: project.workspace_parent_preimage.clone(),
            workspace_parent_after_workspace: project.workspace_parent_after_workspace.clone(),
            workspace_name: project.workspace.name.clone(),
            workspace_exact: project.workspace.exact.clone(),
        }
    }

    pub(super) fn to_json_bytes(&self) -> Result<Vec<u8>, JournalModelError> {
        self.validate()?;
        let mut bytes = serde_json::to_vec(self).map_err(|error| {
            JournalModelError::new(format!("could not serialize bootstrap envelope: {error}"))
        })?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn validate(&self) -> Result<(), JournalModelError> {
        if self.magic != BOOTSTRAP_MAGIC || self.version != JOURNAL_VERSION {
            return Err(JournalModelError::new(
                "workspace bootstrap has unsupported magic or version",
            ));
        }
        validate_parent_creation_transition(
            &self.workspace_parent_preimage,
            &self.workspace_parent_after_workspace,
        )?;
        require_private_directory_mode(&self.workspace_exact, 0o700, "bootstrap workspace")?;
        if self.workspace_name != transaction_directory_name(&self.transaction_id)
            || self.owner_tag
                != workspace_owner_tag(
                    &self.transaction_id,
                    &self.canonical_root_hash,
                    &self.workspace_parent_after_workspace,
                    &self.workspace_name,
                    self.workspace_exact.identity,
                )
        {
            return Err(JournalModelError::new(
                "workspace bootstrap is not bound to its exact parent/workspace ownership tuple",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct WorkspaceBootstrapBindingV2 {
    intent: WorkspaceBootstrapIntentBindingV2,
    name: String,
    exact: ExactFileStateV2,
    envelope: WorkspaceBootstrapEnvelopeV2,
}

impl WorkspaceBootstrapBindingV2 {
    pub(super) fn new(
        transaction_id: &TransactionId,
        project: &ProjectBindingV2,
        intent: WorkspaceBootstrapIntentBindingV2,
        exact: ExactFileStateV2,
    ) -> Result<Self, JournalModelError> {
        let binding = Self {
            intent,
            name: bootstrap_owner_name(transaction_id),
            exact,
            envelope: WorkspaceBootstrapEnvelopeV2::for_project(transaction_id, project),
        };
        binding.validate_for(transaction_id, project)?;
        Ok(binding)
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn intent(&self) -> &WorkspaceBootstrapIntentBindingV2 {
        &self.intent
    }

    pub(super) fn exact(&self) -> &ExactFileStateV2 {
        &self.exact
    }

    pub(super) fn envelope(&self) -> &WorkspaceBootstrapEnvelopeV2 {
        &self.envelope
    }

    fn validate_for(
        &self,
        transaction_id: &TransactionId,
        project: &ProjectBindingV2,
    ) -> Result<(), JournalModelError> {
        self.intent.validate()?;
        if self.intent.envelope.transaction_id != *transaction_id
            || self.intent.envelope.canonical_root_hash != project.canonical_root_hash
            || self.intent.envelope.workspace_parent_preimage != project.workspace_parent_preimage
            || self.name != bootstrap_owner_name(transaction_id)
            || self.envelope != WorkspaceBootstrapEnvelopeV2::for_project(transaction_id, project)
        {
            return Err(JournalModelError::new(
                "bootstrap binding is not the canonical exact owner envelope",
            ));
        }
        self.validate_exact_file()
    }

    fn validate_exact_file(&self) -> Result<(), JournalModelError> {
        require_private_file_mode(&self.exact, 0o600, "bootstrap owner envelope")?;
        if self.exact.link_count != 1 {
            return Err(JournalModelError::new(
                "bootstrap owner envelope must be independently linked",
            ));
        }
        let bytes = self.envelope.to_json_bytes()?;
        if self.exact.state.content_hash != content_hash(&bytes)
            || self.exact.state.byte_len != bytes.len() as u64
        {
            return Err(JournalModelError::new(
                "bootstrap exact file state does not match canonical envelope bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct JournalSnapshotV2 {
    version: u32,
    transaction_id: TransactionId,
    sequence: u64,
    operation: JournalOperationV2,
    project: ProjectBindingV2,
    bootstrap: WorkspaceBootstrapBindingV2,
    previous_record: Option<RecordBindingV2>,
    phase: JournalPhaseV2,
    entries: Vec<JournalEntryV2>,
    directories: Vec<JournalDirectoryV2>,
    cleanup_plans: CleanupPlansV2,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct JournalSnapshotWireV2 {
    version: u32,
    transaction_id: TransactionId,
    sequence: u64,
    operation: JournalOperationV2,
    project: ProjectBindingV2,
    bootstrap: WorkspaceBootstrapBindingV2,
    previous_record: Option<RecordBindingV2>,
    phase: JournalPhaseV2,
    entries: Vec<JournalEntryV2>,
    directories: Vec<JournalDirectoryV2>,
    cleanup_plans: CleanupPlansV2,
}

impl<'de> Deserialize<'de> for JournalSnapshotV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = JournalSnapshotWireV2::deserialize(deserializer)?;
        let snapshot = Self {
            version: wire.version,
            transaction_id: wire.transaction_id,
            sequence: wire.sequence,
            operation: wire.operation,
            project: wire.project,
            bootstrap: wire.bootstrap,
            previous_record: wire.previous_record,
            phase: wire.phase,
            entries: wire.entries,
            directories: wire.directories,
            cleanup_plans: wire.cleanup_plans,
        };
        snapshot.validate().map_err(D::Error::custom)?;
        Ok(snapshot)
    }
}

impl JournalSnapshotV2 {
    pub(super) fn new(
        transaction_id: TransactionId,
        operation: JournalOperationV2,
        project: ProjectBindingV2,
        bootstrap: WorkspaceBootstrapBindingV2,
        entries: Vec<JournalEntryV2>,
        mut directories: Vec<JournalDirectoryV2>,
    ) -> Result<Self, JournalModelError> {
        populate_managed_children(&entries, &mut directories)?;
        let cleanup_plans = derive_cleanup_plans(&entries, &directories);
        let snapshot = Self {
            version: JOURNAL_VERSION,
            transaction_id,
            sequence: 0,
            operation,
            project,
            bootstrap,
            previous_record: None,
            phase: JournalPhaseV2::Preparing {
                completed: 0,
                pending_directory: None,
            },
            entries,
            directories,
            cleanup_plans,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub(super) fn from_json_slice(bytes: &[u8]) -> Result<Self, JournalModelError> {
        serde_json::from_slice(bytes)
            .map_err(|error| JournalModelError::new(format!("invalid journal JSON: {error}")))
    }

    pub(super) fn from_record_envelope_slice(bytes: &[u8]) -> Result<Self, JournalModelError> {
        let (header, payload) = PartialEnvelopeHeaderV2::parse_prefix(bytes)?;
        let snapshot = Self::from_json_slice(payload)?;
        header.validate_binding(
            &snapshot.transaction_id,
            &snapshot.project,
            snapshot.sequence,
        )?;
        header.validate_payload_prefix(payload, payload)?;
        Ok(snapshot)
    }

    pub(super) fn to_json_bytes(&self) -> Result<Vec<u8>, JournalModelError> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|error| {
            JournalModelError::new(format!("could not serialize journal snapshot: {error}"))
        })?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub(super) fn record_envelope_bytes(&self) -> Result<Vec<u8>, JournalModelError> {
        let payload = self.to_json_bytes()?;
        let header = PartialEnvelopeHeaderV2::for_payload(
            self.transaction_id.clone(),
            &self.project,
            self.sequence,
            &payload,
        )?;
        let mut bytes = header.to_prefix_bytes()?;
        bytes.extend_from_slice(&payload);
        Ok(bytes)
    }

    pub(super) const fn version(&self) -> u32 {
        self.version
    }

    pub(super) fn transaction_id(&self) -> &TransactionId {
        &self.transaction_id
    }

    pub(super) const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) const fn operation(&self) -> JournalOperationV2 {
        self.operation
    }

    pub(super) fn project(&self) -> &ProjectBindingV2 {
        &self.project
    }

    pub(super) fn bootstrap(&self) -> &WorkspaceBootstrapBindingV2 {
        &self.bootstrap
    }

    pub(super) fn previous_record(&self) -> Option<&RecordBindingV2> {
        self.previous_record.as_ref()
    }

    pub(super) fn phase(&self) -> &JournalPhaseV2 {
        &self.phase
    }

    pub(super) fn entries(&self) -> &[JournalEntryV2] {
        &self.entries
    }

    pub(super) fn directories(&self) -> &[JournalDirectoryV2] {
        &self.directories
    }

    pub(super) fn cleanup_plans(&self) -> &CleanupPlansV2 {
        &self.cleanup_plans
    }

    pub(super) fn record_name(&self) -> String {
        journal_record_name(&self.transaction_id, self.sequence)
    }

    pub(super) fn partial_name(&self) -> String {
        journal_partial_name(&self.transaction_id, self.sequence)
    }

    pub(super) fn expected_record_binding(
        &self,
        identity: ObjectIdentityV2,
    ) -> Result<RecordBindingV2, JournalModelError> {
        let bytes = self.record_envelope_bytes()?;
        let state = FileStateV2::new(
            content_hash(&bytes),
            bytes.len() as u64,
            false,
            private_posix_mode(0o600),
        )?;
        RecordBindingV2::new(
            self.sequence,
            self.record_name(),
            ExactFileStateV2::new(identity, state, 1)?,
        )
    }

    pub(super) fn validate_record_binding(
        &self,
        binding: &RecordBindingV2,
    ) -> Result<(), JournalModelError> {
        let expected = self.expected_record_binding(binding.exact.identity)?;
        if binding != &expected {
            return Err(JournalModelError::new(
                "record binding does not match the snapshot's canonical bytes, name, and sequence",
            ));
        }
        Ok(())
    }

    pub(super) fn preparation_step_count(&self) -> usize {
        self.directories
            .iter()
            .filter(|directory| directory.disposition == DirectoryDispositionV2::Create)
            .count()
            * 2
            + self.entries.len()
            + self
                .entries
                .iter()
                .filter(|entry| entry.backup.is_some())
                .count()
    }

    pub(super) fn adopt_next_preparation(
        &self,
        current_record: RecordBindingV2,
        observation: PreparationObservationV2,
    ) -> Result<Self, JournalModelError> {
        let JournalPhaseV2::Preparing {
            completed,
            pending_directory: None,
        } = &self.phase
        else {
            return Err(JournalModelError::new(
                "preparation observation requires an unarmed Preparing phase",
            ));
        };
        let index = usize::try_from(*completed)
            .map_err(|_| JournalModelError::new("preparation counter does not fit usize"))?;
        if index >= self.preparation_step_count() {
            return Err(JournalModelError::new(
                "all preparation observations are already recorded",
            ));
        }
        let mut next = self.next_base(current_record)?;
        next.apply_preparation(index, observation)?;
        next.phase = JournalPhaseV2::Preparing {
            completed: completed
                .checked_add(1)
                .ok_or_else(|| JournalModelError::new("preparation counter overflow"))?,
            pending_directory: None,
        };
        self.finish_named_successor(next)
    }

    pub(super) fn arm_directory_publication(
        &self,
        current_record: RecordBindingV2,
        intent: DirectoryPublishIntentV2,
    ) -> Result<Self, JournalModelError> {
        let JournalPhaseV2::Preparing {
            completed,
            pending_directory: None,
        } = &self.phase
        else {
            return Err(JournalModelError::new(
                "directory publication requires an unarmed Preparing phase",
            ));
        };
        let index = usize::try_from(*completed)
            .map_err(|_| JournalModelError::new("preparation counter does not fit usize"))?;
        let PreparationSlot::DirectoryPublish(directory_index) = self.preparation_slot(index)?
        else {
            return Err(JournalModelError::new(
                "directory publication intent does not match the next preparation slot",
            ));
        };
        if self.directories[directory_index].ordinal != intent.ordinal {
            return Err(JournalModelError::new(
                "directory publication intent has the wrong deterministic ordinal",
            ));
        }
        self.validate_directory_publish_intent(&intent)?;
        let mut next = self.next_base(current_record)?;
        next.phase = JournalPhaseV2::Preparing {
            completed: *completed,
            pending_directory: Some(intent),
        };
        self.finish_named_successor(next)
    }

    pub(super) fn complete_directory_publication(
        &self,
        current_record: RecordBindingV2,
        observation: DirectoryPublicationObservationV2,
    ) -> Result<Self, JournalModelError> {
        let JournalPhaseV2::Preparing {
            completed,
            pending_directory: Some(intent),
        } = &self.phase
        else {
            return Err(JournalModelError::new(
                "directory publication completion requires a durable pending intent",
            ));
        };
        let mut next = self.next_base(current_record)?;
        next.apply_directory_publication(intent, observation)?;
        next.phase = JournalPhaseV2::Preparing {
            completed: completed
                .checked_add(1)
                .ok_or_else(|| JournalModelError::new("preparation counter overflow"))?,
            pending_directory: None,
        };
        self.finish_named_successor(next)
    }

    pub(super) fn mark_prepared(
        &self,
        current_record: RecordBindingV2,
    ) -> Result<Self, JournalModelError> {
        let JournalPhaseV2::Preparing {
            completed,
            pending_directory: None,
        } = &self.phase
        else {
            return Err(JournalModelError::new("only Preparing can become Prepared"));
        };
        if usize::try_from(*completed).ok() != Some(self.preparation_step_count()) {
            return Err(JournalModelError::new(
                "all preparation steps must be captured before Prepared",
            ));
        }
        let mut next = self.next_base(current_record)?;
        next.phase = JournalPhaseV2::Prepared;
        self.finish_named_successor(next)
    }

    pub(super) fn record_replacement_completion(
        &self,
        current_record: RecordBindingV2,
        observation: ReplacementObservationV2,
    ) -> Result<Self, JournalModelError> {
        let committed = match self.phase {
            JournalPhaseV2::Prepared => 0,
            JournalPhaseV2::Replacing { committed } => committed,
            _ => {
                return Err(JournalModelError::new(
                    "a replacement can only complete from Prepared or Replacing",
                ));
            }
        };
        let index = usize::try_from(committed)
            .map_err(|_| JournalModelError::new("replacement counter does not fit usize"))?;
        if index >= self.entries.len() {
            return Err(JournalModelError::new(
                "all cohort entries are already replaced",
            ));
        }
        let mut next = self.next_base(current_record)?;
        next.apply_replacement(index, observation)?;
        next.phase = JournalPhaseV2::Replacing {
            committed: committed
                .checked_add(1)
                .ok_or_else(|| JournalModelError::new("replacement counter overflow"))?,
        };
        self.finish_named_successor(next)
    }

    pub(super) fn begin_rollback(
        &self,
        current_record: RecordBindingV2,
    ) -> Result<Self, JournalModelError> {
        if !matches!(
            &self.phase,
            JournalPhaseV2::Preparing {
                pending_directory: None,
                ..
            } | JournalPhaseV2::Prepared
                | JournalPhaseV2::Replacing { .. }
        ) {
            return Err(JournalModelError::new(
                "rollback can only begin before CommitComplete",
            ));
        }
        let mut next = self.next_base(current_record)?;
        next.phase = JournalPhaseV2::RollingBack {
            next: u32::try_from(self.entries.len())
                .map_err(|_| JournalModelError::new("entry cohort exceeds u32"))?,
            pending: None,
        };
        self.finish_named_successor(next)
    }

    pub(super) fn arm_rollback(
        &self,
        current_record: RecordBindingV2,
        intent: RollbackIntentV2,
    ) -> Result<Self, JournalModelError> {
        let JournalPhaseV2::RollingBack {
            next: cursor,
            pending: None,
        } = &self.phase
        else {
            return Err(JournalModelError::new(
                "rollback mutation requires an unarmed RollingBack cursor",
            ));
        };
        if *cursor == 0 || intent.ordinal().get() != *cursor - 1 {
            return Err(JournalModelError::new(
                "rollback intent must target the next reverse-order entry",
            ));
        }
        self.validate_rollback_intent(&intent)?;
        let mut next = self.next_base(current_record)?;
        next.phase = JournalPhaseV2::RollingBack {
            next: *cursor,
            pending: Some(intent),
        };
        self.finish_named_successor(next)
    }

    pub(super) fn complete_rollback(
        &self,
        current_record: RecordBindingV2,
    ) -> Result<Self, JournalModelError> {
        let JournalPhaseV2::RollingBack {
            next: cursor,
            pending: Some(intent),
        } = &self.phase
        else {
            return Err(JournalModelError::new(
                "rollback completion requires a durable pending intent",
            ));
        };
        let mut next = self.next_base(current_record)?;
        next.apply_rollback_completion(intent)?;
        next.phase = JournalPhaseV2::RollingBack {
            next: cursor - 1,
            pending: None,
        };
        self.finish_named_successor(next)
    }

    pub(super) fn advance_rollback_noop(
        &self,
        current_record: RecordBindingV2,
    ) -> Result<Self, JournalModelError> {
        let JournalPhaseV2::RollingBack {
            next: cursor,
            pending: None,
        } = self.phase
        else {
            return Err(JournalModelError::new(
                "rollback no-op requires an unarmed RollingBack cursor",
            ));
        };
        if cursor == 0 {
            return Err(JournalModelError::new(
                "rollback cursor is already complete",
            ));
        }
        self.validate_rollback_noop(usize::try_from(cursor - 1).expect("u32 fits usize"))?;
        let mut next = self.next_base(current_record)?;
        next.phase = JournalPhaseV2::RollingBack {
            next: cursor - 1,
            pending: None,
        };
        self.finish_named_successor(next)
    }

    pub(super) fn finish_rollback_targets(
        &self,
        current_record: RecordBindingV2,
    ) -> Result<Self, JournalModelError> {
        if self.phase
            != (JournalPhaseV2::RollingBack {
                next: 0,
                pending: None,
            })
        {
            return Err(JournalModelError::new(
                "rollback targets must reach zero with no pending intent",
            ));
        }
        let mut next = self.next_base(current_record)?;
        next.phase = JournalPhaseV2::RollbackComplete {
            cleanup_completed: 0,
            pending: None,
        };
        self.finish_named_successor(next)
    }

    pub(super) fn enter_commit_complete(
        &self,
        current_record: RecordBindingV2,
    ) -> Result<Self, JournalModelError> {
        let entries = u32::try_from(self.entries.len())
            .map_err(|_| JournalModelError::new("entry cohort exceeds u32"))?;
        if self.phase != (JournalPhaseV2::Replacing { committed: entries }) {
            return Err(JournalModelError::new(
                "CommitComplete requires every replacement to be durable",
            ));
        }
        let mut next = self.next_base(current_record)?;
        next.phase = JournalPhaseV2::CommitComplete {
            cleanup_completed: 0,
            pending: None,
        };
        self.finish_named_successor(next)
    }

    pub(super) fn arm_cleanup(
        &self,
        current_record: RecordBindingV2,
        intent: CleanupIntentV2,
    ) -> Result<Self, JournalModelError> {
        let (completed, plan) = self.cleanup_cursor()?;
        if self.cleanup_pending().is_some() {
            return Err(JournalModelError::new(
                "cleanup already has a durable pending intent",
            ));
        }
        let target = *plan.get(completed).ok_or_else(|| {
            JournalModelError::new("all deterministic cleanup slots are already complete")
        })?;
        if intent.target() != target {
            return Err(JournalModelError::new(
                "cleanup intent does not match the deterministic cleanup cursor",
            ));
        }
        self.validate_cleanup_intent(&intent)?;
        let mut next = self.next_base(current_record)?;
        next.phase = match self.phase {
            JournalPhaseV2::RollbackComplete { .. } => JournalPhaseV2::RollbackComplete {
                cleanup_completed: completed as u32,
                pending: Some(intent),
            },
            JournalPhaseV2::CommitComplete { .. } => JournalPhaseV2::CommitComplete {
                cleanup_completed: completed as u32,
                pending: Some(intent),
            },
            _ => unreachable!("cleanup_cursor rejects other phases"),
        };
        self.finish_named_successor(next)
    }

    pub(super) fn complete_cleanup(
        &self,
        current_record: RecordBindingV2,
        parent_after: Option<ExactDirectoryStateV2>,
    ) -> Result<Self, JournalModelError> {
        let (completed, _) = self.cleanup_cursor()?;
        let intent = self
            .cleanup_pending()
            .cloned()
            .ok_or_else(|| JournalModelError::new("cleanup has no pending intent"))?;
        let mut next = self.next_base(current_record)?;
        next.apply_cleanup_completion(&intent, parent_after)?;
        let after = u32::try_from(completed + 1)
            .map_err(|_| JournalModelError::new("cleanup counter exceeds u32"))?;
        next.phase = match self.phase {
            JournalPhaseV2::RollbackComplete { .. } => JournalPhaseV2::RollbackComplete {
                cleanup_completed: after,
                pending: None,
            },
            JournalPhaseV2::CommitComplete { .. } => JournalPhaseV2::CommitComplete {
                cleanup_completed: after,
                pending: None,
            },
            _ => unreachable!("cleanup_cursor rejects other phases"),
        };
        self.finish_named_successor(next)
    }

    pub(super) fn advance_cleanup_noop(
        &self,
        current_record: RecordBindingV2,
    ) -> Result<Self, JournalModelError> {
        let (completed, plan) = self.cleanup_cursor()?;
        if self.cleanup_pending().is_some() {
            return Err(JournalModelError::new(
                "cleanup no-op cannot bypass a pending intent",
            ));
        }
        let target = *plan.get(completed).ok_or_else(|| {
            JournalModelError::new("all deterministic cleanup slots are already complete")
        })?;
        if !self.cleanup_target_missing(target)? {
            return Err(JournalModelError::new(
                "a present cleanup target requires a durable pending mutation intent",
            ));
        }
        let mut next = self.next_base(current_record)?;
        let after = u32::try_from(completed + 1)
            .map_err(|_| JournalModelError::new("cleanup counter exceeds u32"))?;
        next.phase = match self.phase {
            JournalPhaseV2::RollbackComplete { .. } => JournalPhaseV2::RollbackComplete {
                cleanup_completed: after,
                pending: None,
            },
            JournalPhaseV2::CommitComplete { .. } => JournalPhaseV2::CommitComplete {
                cleanup_completed: after,
                pending: None,
            },
            _ => unreachable!("cleanup_cursor rejects other phases"),
        };
        self.finish_named_successor(next)
    }

    pub(super) fn cleanup_is_complete(&self) -> bool {
        self.cleanup_cursor().is_ok_and(|(completed, plan)| {
            completed == plan.len() && self.cleanup_pending().is_none()
        })
    }

    pub(super) fn ready_for_finalization(&self) -> bool {
        matches!(
            self.phase,
            JournalPhaseV2::RollbackComplete { .. } | JournalPhaseV2::CommitComplete { .. }
        ) && self.cleanup_is_complete()
    }

    fn next_base(&self, current_record: RecordBindingV2) -> Result<Self, JournalModelError> {
        self.validate()?;
        self.validate_record_binding(&current_record)?;
        let mut next = self.clone();
        next.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| JournalModelError::new("journal sequence overflow"))?;
        next.previous_record = Some(current_record);
        Ok(next)
    }

    fn finish_named_successor(&self, next: Self) -> Result<Self, JournalModelError> {
        next.validate()?;
        self.validate_successor(&next)?;
        Ok(next)
    }

    fn preparation_slot(&self, mut index: usize) -> Result<PreparationSlot, JournalModelError> {
        for (directory_index, directory) in self.directories.iter().enumerate() {
            if directory.disposition == DirectoryDispositionV2::Create {
                if index == 0 {
                    return Ok(PreparationSlot::DirectoryCandidate(directory_index));
                }
                index -= 1;
                if index == 0 {
                    return Ok(PreparationSlot::DirectoryPublish(directory_index));
                }
                index -= 1;
            }
        }
        for entry_index in 0..self.entries.len() {
            if index == 0 {
                return Ok(PreparationSlot::Stage(entry_index));
            }
            index -= 1;
        }
        for (entry_index, entry) in self.entries.iter().enumerate() {
            if entry.backup.is_some() {
                if index == 0 {
                    return Ok(PreparationSlot::Backup(entry_index));
                }
                index -= 1;
            }
        }
        Err(JournalModelError::new(
            "preparation slot exceeds the deterministic cohort",
        ))
    }

    fn apply_preparation(
        &mut self,
        index: usize,
        observation: PreparationObservationV2,
    ) -> Result<(), JournalModelError> {
        match (self.preparation_slot(index)?, observation) {
            (
                PreparationSlot::DirectoryCandidate(directory_index),
                PreparationObservationV2::DirectoryCandidate {
                    exact,
                    parent_after,
                },
            ) => {
                exact.validate()?;
                let directory = &self.directories[directory_index];
                if !directory.current.is_missing()
                    || !directory.candidate_current.is_missing()
                    || directory.created_exact.is_some()
                {
                    return Err(JournalModelError::new(
                        "directory candidate observation does not match its missing exact preimage",
                    ));
                }
                require_private_directory_mode(&exact, 0o700, "directory candidate")?;
                let parent = self.directory_parent(directory_index)?;
                self.validate_and_set_parent_after_creation(parent, parent_after)?;
                self.directories[directory_index].created_exact = Some(exact.clone());
                self.directories[directory_index].candidate_current = PresenceV2::Present(exact);
            }
            (PreparationSlot::Stage(entry_index), PreparationObservationV2::Stage { exact }) => {
                exact.validate()?;
                let entry = &mut self.entries[entry_index];
                if !entry.stage.current.is_missing()
                    || entry.stage.prepared.is_some()
                    || !entry
                        .planned
                        .matches_resolved(&exact.state, &entry.preimage)
                    || exact.link_count != 1
                {
                    return Err(JournalModelError::new(
                        "stage observation must be an independent exact planned file",
                    ));
                }
                entry.stage.prepared = Some(exact.clone());
                entry.stage.current = PresenceV2::Present(exact);
            }
            (PreparationSlot::Backup(entry_index), PreparationObservationV2::Backup { exact }) => {
                exact.validate()?;
                let entry = &mut self.entries[entry_index];
                let PreimageV2::Regular { exact: preimage } = &entry.preimage else {
                    return Err(JournalModelError::new(
                        "only replace entries have backup preparation slots",
                    ));
                };
                let backup = entry.backup.as_mut().expect("replace entry has backup");
                if !backup.current.is_missing()
                    || backup.prepared.is_some()
                    || exact.state != preimage.state
                    || exact.link_count != 1
                    || exact.identity == preimage.identity
                {
                    return Err(JournalModelError::new(
                        "backup observation must be an independent exact preimage copy",
                    ));
                }
                backup.prepared = Some(exact.clone());
                backup.current = PresenceV2::Present(exact);
            }
            _ => {
                return Err(JournalModelError::new(
                    "preparation observation kind does not match the deterministic next slot",
                ));
            }
        }
        Ok(())
    }

    fn validate_directory_publish_intent(
        &self,
        intent: &DirectoryPublishIntentV2,
    ) -> Result<(), JournalModelError> {
        let index = index_of(intent.ordinal, self.directories.len())?;
        let directory = &self.directories[index];
        let expected = directory
            .candidate_current
            .as_present()
            .ok_or_else(|| JournalModelError::new("directory candidate is not present"))?;
        if directory.disposition != DirectoryDispositionV2::Create
            || !directory.current.is_missing()
            || directory.candidate_name.as_deref() != Some(intent.candidate_name.as_str())
            || expected != &intent.expected_candidate
            || directory.created_exact.as_ref() != Some(expected)
            || self.directory_parent(index)? != intent.parent
            || self.parent_current(intent.parent)? != &intent.parent_before
        {
            return Err(JournalModelError::new(
                "directory publication intent is not bound to the exact private candidate, missing target, and parent",
            ));
        }
        require_private_directory_mode(expected, 0o700, "directory candidate")
    }

    pub(super) fn validate_directory_publication_world(
        &self,
        intent: &DirectoryPublishIntentV2,
        candidate: &PresenceV2<ExactDirectoryStateV2>,
        target: &PresenceV2<ExactDirectoryStateV2>,
        parent: &ExactDirectoryStateV2,
    ) -> Result<DirectoryPublicationWorldV2, JournalModelError> {
        self.validate_directory_publish_intent(intent)?;
        if parent != &intent.parent_before {
            return Err(JournalModelError::new(
                "directory publication world substituted or mutated its exact parent",
            ));
        }
        let directory = &self.directories[index_of(intent.ordinal, self.directories.len())?];
        match (candidate, target) {
            (PresenceV2::Present(exact), PresenceV2::Missing)
                if exact == &intent.expected_candidate =>
            {
                Ok(DirectoryPublicationWorldV2::CandidatePrivate)
            }
            (PresenceV2::Present(exact), PresenceV2::Missing)
                if exact.identity == intent.expected_candidate.identity
                    && exact.link_count == intent.expected_candidate.link_count
                    && exact.mode == directory.planned_mode =>
            {
                Ok(DirectoryPublicationWorldV2::CandidateReady)
            }
            (PresenceV2::Missing, PresenceV2::Present(exact))
                if exact.identity == intent.expected_candidate.identity
                    && exact.link_count == intent.expected_candidate.link_count
                    && exact.mode == directory.planned_mode =>
            {
                Ok(DirectoryPublicationWorldV2::Published)
            }
            _ => Err(JournalModelError::new(
                "directory publication world is neither candidate-before, mode-ready candidate, nor exact target-after",
            )),
        }
    }

    fn apply_directory_publication(
        &mut self,
        intent: &DirectoryPublishIntentV2,
        observation: DirectoryPublicationObservationV2,
    ) -> Result<(), JournalModelError> {
        let world = self.validate_directory_publication_world(
            intent,
            &observation.candidate,
            &PresenceV2::Present(observation.target.clone()),
            &observation.parent_after,
        )?;
        if world != DirectoryPublicationWorldV2::Published {
            return Err(JournalModelError::new(
                "directory publication completion requires the exact published after-world",
            ));
        }
        let index = index_of(intent.ordinal, self.directories.len())?;
        self.directories[index].candidate_current = PresenceV2::Missing;
        self.directories[index].current = PresenceV2::Present(observation.target);
        Ok(())
    }

    fn apply_replacement(
        &mut self,
        index: usize,
        observation: ReplacementObservationV2,
    ) -> Result<(), JournalModelError> {
        observation.target.validate()?;
        if let PresenceV2::Present(stage) = &observation.stage {
            stage.validate()?;
        }
        let entry = &mut self.entries[index];
        let stage_before = entry.stage.current.as_present().ok_or_else(|| {
            JournalModelError::new("replacement requires its exact prepared stage")
        })?;
        match entry.action {
            EntryActionV2::Create => {
                if !entry.current_target.is_missing()
                    || observation.target.identity != stage_before.identity
                    || observation.target.state != stage_before.state
                    || observation.target.link_count != 2
                    || observation.stage != PresenceV2::Present(observation.target.clone())
                {
                    return Err(JournalModelError::new(
                        "create publication must leave target and stage as the same exact two-link planned file",
                    ));
                }
            }
            EntryActionV2::Replace => {
                let PreimageV2::Regular { exact: preimage } = &entry.preimage else {
                    unreachable!("validated replace preimage")
                };
                if entry.current_target != PresenceV2::Present(preimage.clone())
                    || observation.target.identity != stage_before.identity
                    || observation.target.state != stage_before.state
                    || observation.target.link_count != 1
                    || !observation.stage.is_missing()
                    || entry
                        .backup
                        .as_ref()
                        .and_then(|backup| backup.current.as_present())
                        .is_none()
                {
                    return Err(JournalModelError::new(
                        "replace publication must move the exact stage over its exact preimage while retaining an independent backup",
                    ));
                }
            }
        }
        entry.current_target = PresenceV2::Present(observation.target);
        entry.stage.current = observation.stage;
        Ok(())
    }

    fn validate_rollback_intent(&self, intent: &RollbackIntentV2) -> Result<(), JournalModelError> {
        let index = usize::try_from(intent.ordinal().get()).expect("u32 fits usize");
        let entry = self
            .entries
            .get(index)
            .ok_or_else(|| JournalModelError::new("rollback intent ordinal exceeds cohort"))?;
        match (intent, entry.action) {
            (
                RollbackIntentV2::RemoveCreatedTarget {
                    expected_target, ..
                },
                EntryActionV2::Create,
            ) => {
                if entry.current_target != PresenceV2::Present(expected_target.clone())
                    || entry.resolved_planned_state() != Some(&expected_target.state)
                    || expected_target.link_count != 2
                    || entry.stage.current != PresenceV2::Present(expected_target.clone())
                {
                    return Err(JournalModelError::new(
                        "created-target rollback intent does not bind the exact target/stage alias",
                    ));
                }
            }
            (
                RollbackIntentV2::RestoreBackup {
                    expected_target,
                    expected_backup,
                    ..
                },
                EntryActionV2::Replace,
            ) => {
                let backup = entry.backup.as_ref().expect("replace backup");
                if entry.current_target != PresenceV2::Present(expected_target.clone())
                    || entry.resolved_planned_state() != Some(&expected_target.state)
                    || expected_target.link_count != 1
                    || backup.current != PresenceV2::Present(expected_backup.clone())
                    || expected_backup.link_count != 1
                    || !entry.stage.current.is_missing()
                {
                    return Err(JournalModelError::new(
                        "restore intent does not bind the exact planned target and independent backup",
                    ));
                }
                let PreimageV2::Regular { exact: preimage } = &entry.preimage else {
                    unreachable!("validated replace preimage")
                };
                if expected_backup.state != preimage.state
                    || expected_backup.identity == expected_target.identity
                {
                    return Err(JournalModelError::new(
                        "restore backup must be an independent exact preimage copy",
                    ));
                }
            }
            _ => {
                return Err(JournalModelError::new(
                    "rollback intent action does not match its entry",
                ));
            }
        }
        Ok(())
    }

    fn apply_rollback_completion(
        &mut self,
        intent: &RollbackIntentV2,
    ) -> Result<(), JournalModelError> {
        self.validate_rollback_intent(intent)?;
        let index = usize::try_from(intent.ordinal().get()).expect("u32 fits usize");
        let entry = &mut self.entries[index];
        match intent {
            RollbackIntentV2::RemoveCreatedTarget {
                expected_target, ..
            } => {
                entry.current_target = PresenceV2::Missing;
                entry.stage.current = PresenceV2::Present(expected_target.with_link_count(1)?);
            }
            RollbackIntentV2::RestoreBackup {
                expected_backup, ..
            } => {
                entry.current_target = PresenceV2::Present(expected_backup.clone());
                entry.backup.as_mut().expect("replace backup").current = PresenceV2::Missing;
            }
        }
        Ok(())
    }

    fn validate_rollback_noop(&self, index: usize) -> Result<(), JournalModelError> {
        let entry = &self.entries[index];
        match (&entry.action, &entry.preimage, &entry.current_target) {
            (EntryActionV2::Create, PreimageV2::Absent, PresenceV2::Missing) => Ok(()),
            (
                EntryActionV2::Replace,
                PreimageV2::Regular { exact },
                PresenceV2::Present(current),
            ) if current == exact => Ok(()),
            _ => Err(JournalModelError::new(
                "rollback cursor can advance without mutation only for an exact preimage",
            )),
        }
    }

    fn cleanup_cursor(&self) -> Result<(usize, &[CleanupTargetV2]), JournalModelError> {
        match &self.phase {
            JournalPhaseV2::RollbackComplete {
                cleanup_completed, ..
            } => Ok((
                usize::try_from(*cleanup_completed)
                    .map_err(|_| JournalModelError::new("cleanup counter does not fit usize"))?,
                &self.cleanup_plans.rollback,
            )),
            JournalPhaseV2::CommitComplete {
                cleanup_completed, ..
            } => Ok((
                usize::try_from(*cleanup_completed)
                    .map_err(|_| JournalModelError::new("cleanup counter does not fit usize"))?,
                &self.cleanup_plans.commit,
            )),
            _ => Err(JournalModelError::new(
                "cleanup is only available after rollback or commit completion",
            )),
        }
    }

    fn cleanup_pending(&self) -> Option<&CleanupIntentV2> {
        match &self.phase {
            JournalPhaseV2::RollbackComplete { pending, .. }
            | JournalPhaseV2::CommitComplete { pending, .. } => pending.as_ref(),
            _ => None,
        }
    }

    fn cleanup_target_missing(&self, target: CleanupTargetV2) -> Result<bool, JournalModelError> {
        Ok(match target {
            CleanupTargetV2::Stage { ordinal } => self.entries
                [index_of(ordinal, self.entries.len())?]
            .stage
            .current
            .is_missing(),
            CleanupTargetV2::Backup { ordinal } => self.entries
                [index_of(ordinal, self.entries.len())?]
            .backup
            .as_ref()
            .ok_or_else(|| JournalModelError::new("cleanup plan names a missing backup slot"))?
            .current
            .is_missing(),
            CleanupTargetV2::CreatedDirectory { ordinal } => self.directories
                [index_of(ordinal, self.directories.len())?]
            .current
            .is_missing(),
            CleanupTargetV2::DirectoryCandidate { ordinal } => self.directories
                [index_of(ordinal, self.directories.len())?]
            .candidate_current
            .is_missing(),
        })
    }

    fn validate_cleanup_intent(&self, intent: &CleanupIntentV2) -> Result<(), JournalModelError> {
        match intent {
            CleanupIntentV2::RemoveFile { target, expected } => {
                expected.validate()?;
                let current = match target {
                    CleanupTargetV2::Stage { ordinal } => {
                        &self.entries[index_of(*ordinal, self.entries.len())?]
                            .stage
                            .current
                    }
                    CleanupTargetV2::Backup { ordinal } => {
                        &self.entries[index_of(*ordinal, self.entries.len())?]
                            .backup
                            .as_ref()
                            .ok_or_else(|| {
                                JournalModelError::new("cleanup intent names a missing backup slot")
                            })?
                            .current
                    }
                    CleanupTargetV2::CreatedDirectory { .. }
                    | CleanupTargetV2::DirectoryCandidate { .. } => {
                        return Err(JournalModelError::new(
                            "a created directory requires a directory cleanup intent",
                        ));
                    }
                };
                if current != &PresenceV2::Present(expected.clone()) {
                    return Err(JournalModelError::new(
                        "cleanup file intent does not bind the exact current artifact",
                    ));
                }
            }
            CleanupIntentV2::RemoveDirectory {
                target,
                expected,
                parent,
                parent_before,
            } => {
                let (ordinal, candidate) = match target {
                    CleanupTargetV2::CreatedDirectory { ordinal } => (*ordinal, false),
                    CleanupTargetV2::DirectoryCandidate { ordinal } => (*ordinal, true),
                    _ => {
                        return Err(JournalModelError::new(
                            "directory cleanup intent must target a logical created directory or its candidate",
                        ));
                    }
                };
                let directory_index = index_of(ordinal, self.directories.len())?;
                let directory = &self.directories[directory_index];
                let current = if candidate {
                    &directory.candidate_current
                } else {
                    &directory.current
                };
                if directory.disposition != DirectoryDispositionV2::Create
                    || current != &PresenceV2::Present(expected.clone())
                    || self.directory_parent(directory_index)? != *parent
                    || self.parent_current(*parent)? != parent_before
                    || (!candidate && !self.managed_children_are_missing(directory_index)?)
                {
                    return Err(JournalModelError::new(
                        "created-directory cleanup intent lacks an exact empty owned directory and exact parent",
                    ));
                }
            }
        }
        Ok(())
    }

    fn apply_cleanup_completion(
        &mut self,
        intent: &CleanupIntentV2,
        parent_after: Option<ExactDirectoryStateV2>,
    ) -> Result<(), JournalModelError> {
        self.validate_cleanup_intent(intent)?;
        match intent {
            CleanupIntentV2::RemoveFile { target, expected } => {
                if parent_after.is_some() {
                    return Err(JournalModelError::new(
                        "file cleanup must not supply a directory parent transition",
                    ));
                }
                match target {
                    CleanupTargetV2::Stage { ordinal } => {
                        let index = index_of(*ordinal, self.entries.len())?;
                        let entry = &mut self.entries[index];
                        entry.stage.current = PresenceV2::Missing;
                        if entry.action == EntryActionV2::Create {
                            if let PresenceV2::Present(target) = &entry.current_target {
                                if target.identity == expected.identity && target.link_count == 2 {
                                    entry.current_target =
                                        PresenceV2::Present(target.with_link_count(1)?);
                                }
                            }
                        }
                    }
                    CleanupTargetV2::Backup { ordinal } => {
                        let index = index_of(*ordinal, self.entries.len())?;
                        self.entries[index]
                            .backup
                            .as_mut()
                            .expect("validated backup cleanup")
                            .current = PresenceV2::Missing;
                    }
                    CleanupTargetV2::CreatedDirectory { .. }
                    | CleanupTargetV2::DirectoryCandidate { .. } => {
                        unreachable!("validated intent")
                    }
                }
            }
            CleanupIntentV2::RemoveDirectory {
                target,
                parent,
                parent_before,
                ..
            } => {
                let Some(parent_after) = parent_after else {
                    return Err(JournalModelError::new(
                        "directory cleanup requires its exact parent-after observation",
                    ));
                };
                validate_parent_removal_transition(parent_before, &parent_after)?;
                let (ordinal, candidate) = match target {
                    CleanupTargetV2::CreatedDirectory { ordinal } => (*ordinal, false),
                    CleanupTargetV2::DirectoryCandidate { ordinal } => (*ordinal, true),
                    _ => unreachable!("validated directory intent"),
                };
                let index = index_of(ordinal, self.directories.len())?;
                let directory = &mut self.directories[index];
                if candidate {
                    directory.candidate_current = PresenceV2::Missing;
                } else {
                    directory.current = PresenceV2::Missing;
                }
                self.set_parent_current(*parent, parent_after)?;
            }
        }
        Ok(())
    }

    fn directory_parent(
        &self,
        directory_index: usize,
    ) -> Result<DirectoryParentV2, JournalModelError> {
        let path = &self.directories[directory_index].logical_path;
        let Some((parent, _)) = path.rsplit_once('/') else {
            return Ok(DirectoryParentV2::ProjectRoot);
        };
        let parent_index = self
            .directories
            .iter()
            .position(|directory| directory.logical_path == parent)
            .ok_or_else(|| JournalModelError::new("directory parent is absent from the cohort"))?;
        Ok(DirectoryParentV2::Cohort {
            ordinal: self.directories[parent_index].ordinal,
        })
    }

    fn parent_current(
        &self,
        parent: DirectoryParentV2,
    ) -> Result<&ExactDirectoryStateV2, JournalModelError> {
        match parent {
            DirectoryParentV2::ProjectRoot => Ok(&self.project.root_current),
            DirectoryParentV2::Cohort { ordinal } => self.directories
                [index_of(ordinal, self.directories.len())?]
            .current
            .as_present()
            .ok_or_else(|| JournalModelError::new("controlled parent directory is missing")),
        }
    }

    fn set_parent_current(
        &mut self,
        parent: DirectoryParentV2,
        exact: ExactDirectoryStateV2,
    ) -> Result<(), JournalModelError> {
        match parent {
            DirectoryParentV2::ProjectRoot => self.project.root_current = exact,
            DirectoryParentV2::Cohort { ordinal } => {
                let index = index_of(ordinal, self.directories.len())?;
                if self.directories[index].logical_path == WORKSPACE_PARENT_LOGICAL_PATH {
                    self.project.workspace_parent_current = exact.clone();
                }
                self.directories[index].current = PresenceV2::Present(exact);
            }
        }
        Ok(())
    }

    fn validate_and_set_parent_after_creation(
        &mut self,
        parent: DirectoryParentV2,
        after: ExactDirectoryStateV2,
    ) -> Result<(), JournalModelError> {
        let before = self.parent_current(parent)?.clone();
        validate_parent_creation_transition(&before, &after)?;
        self.set_parent_current(parent, after)
    }

    fn managed_children_are_missing(
        &self,
        directory_index: usize,
    ) -> Result<bool, JournalModelError> {
        let directory_path = &self.directories[directory_index].logical_path;
        for directory in &self.directories {
            if immediate_parent(&directory.logical_path) == Some(directory_path.as_str())
                && !directory.current.is_missing()
            {
                return Ok(false);
            }
        }
        for entry in &self.entries {
            if immediate_parent(&entry.logical_path) == Some(directory_path.as_str())
                && (!entry.current_target.is_missing()
                    || !entry.stage.current.is_missing()
                    || entry
                        .backup
                        .as_ref()
                        .is_some_and(|backup| !backup.current.is_missing()))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn validate(&self) -> Result<(), JournalModelError> {
        if self.version != JOURNAL_VERSION {
            return Err(JournalModelError::new(format!(
                "unsupported journal version {}",
                self.version
            )));
        }
        TransactionId::parse(self.transaction_id.as_str())?;
        self.project.validate(&self.transaction_id)?;
        self.bootstrap
            .validate_for(&self.transaction_id, &self.project)?;
        if self.entries.is_empty() {
            return Err(JournalModelError::new(
                "transaction cohort must contain at least one entry",
            ));
        }
        match (self.sequence, &self.previous_record) {
            (0, None) => {}
            (0, Some(_)) => {
                return Err(JournalModelError::new(
                    "sequence zero cannot bind a previous record",
                ));
            }
            (_, None) => {
                return Err(JournalModelError::new(
                    "every nonzero sequence must bind its previous record",
                ));
            }
            (sequence, Some(previous)) => {
                if previous.sequence.checked_add(1) != Some(sequence)
                    || previous.name != journal_record_name(&self.transaction_id, previous.sequence)
                {
                    return Err(JournalModelError::new(
                        "previous-record binding is not the canonical immediate predecessor",
                    ));
                }
                require_private_file_mode(&previous.exact, 0o600, "journal record")?;
                if previous.exact.link_count != 1 {
                    return Err(JournalModelError::new(
                        "journal records must be independently linked",
                    ));
                }
            }
        }
        self.validate_entries()?;
        self.validate_directories()?;
        if self.cleanup_plans != derive_cleanup_plans(&self.entries, &self.directories) {
            return Err(JournalModelError::new(
                "cleanup plans are not the immutable deterministic cohort plans",
            ));
        }
        self.validate_phase_matrix()?;
        self.validate_live_identity_independence()?;
        Ok(())
    }

    pub(super) fn validate_successor(&self, next: &Self) -> Result<(), JournalModelError> {
        self.validate()?;
        next.validate()?;
        if self.sequence.checked_add(1) != Some(next.sequence) {
            return Err(JournalModelError::new(
                "successor sequence must increase by exactly one",
            ));
        }
        let previous = next.previous_record.as_ref().ok_or_else(|| {
            JournalModelError::new("successor is missing its exact previous-record binding")
        })?;
        self.validate_record_binding(previous)?;
        self.validate_static_successor(next)?;
        self.validate_dynamic_successor(next)
    }

    fn validate_entries(&self) -> Result<(), JournalModelError> {
        let mut folded_paths = BTreeSet::new();
        let mut last_ordinary: Option<&str> = None;
        let mut install_lock_seen = false;
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.ordinal != ordinal_from_index(index)? {
                return Err(JournalModelError::new(
                    "entry ordinals must be contiguous and deterministic",
                ));
            }
            entry.validate_static(&self.transaction_id)?;
            if !folded_paths.insert(entry.logical_path.to_ascii_lowercase()) {
                return Err(JournalModelError::new(
                    "entry paths collide under ASCII case folding",
                ));
            }
            match entry.role {
                EntryRoleV2::Ordinary => {
                    if install_lock_seen
                        || last_ordinary.is_some_and(|last| last >= entry.logical_path.as_str())
                    {
                        return Err(JournalModelError::new(
                            "ordinary entries must be strictly sorted before the install lock",
                        ));
                    }
                    last_ordinary = Some(&entry.logical_path);
                }
                EntryRoleV2::InstallLock => {
                    if install_lock_seen || index + 1 != self.entries.len() {
                        return Err(JournalModelError::new(
                            "at most one install-lock entry is allowed and it must be last",
                        ));
                    }
                    install_lock_seen = true;
                }
            }
            if let PresenceV2::Present(target) = &entry.current_target {
                target.validate()?;
                match entry.action {
                    EntryActionV2::Create => {
                        if entry.resolved_planned_state() != Some(&target.state)
                            || !matches!(target.link_count, 1 | 2)
                        {
                            return Err(JournalModelError::new(format!(
                                "created target {} is not an exact planned state",
                                entry.logical_path
                            )));
                        }
                    }
                    EntryActionV2::Replace => {
                        let PreimageV2::Regular { exact: preimage } = &entry.preimage else {
                            unreachable!("validated replace entry")
                        };
                        if target.link_count != 1
                            || (entry.resolved_planned_state() != Some(&target.state)
                                && target.state != preimage.state)
                        {
                            return Err(JournalModelError::new(format!(
                                "replace target {} is neither an exact planned nor preimage state",
                                entry.logical_path
                            )));
                        }
                    }
                }
            }
            if let PresenceV2::Present(stage) = &entry.stage.current {
                stage.validate()?;
                if entry.stage.prepared.as_ref() != Some(stage)
                    && !(entry.action == EntryActionV2::Create
                        && entry.stage.prepared.as_ref().is_some_and(|prepared| {
                            prepared.identity == stage.identity
                                && prepared.state == stage.state
                                && prepared.link_count == 1
                                && stage.link_count == 2
                        }))
                {
                    return Err(JournalModelError::new(format!(
                        "stage for {} changed from its prepared exact identity/state",
                        entry.logical_path
                    )));
                }
                if !entry
                    .planned
                    .matches_resolved(&stage.state, &entry.preimage)
                    || !matches!(stage.link_count, 1 | 2)
                {
                    return Err(JournalModelError::new(format!(
                        "stage for {} is not an exact planned state",
                        entry.logical_path
                    )));
                }
            }
            if let Some(prepared) = &entry.stage.prepared {
                prepared.validate()?;
                if !entry
                    .planned
                    .matches_resolved(&prepared.state, &entry.preimage)
                    || prepared.link_count != 1
                {
                    return Err(JournalModelError::new(
                        "prepared stage binding must remain an independent planned file",
                    ));
                }
            }
            if let Some(backup) = &entry.backup {
                if let Some(prepared) = &backup.prepared {
                    prepared.validate()?;
                    let PreimageV2::Regular { exact: preimage } = &entry.preimage else {
                        unreachable!("backup belongs to replace entry")
                    };
                    if prepared.state != preimage.state
                        || prepared.link_count != 1
                        || prepared.identity == preimage.identity
                    {
                        return Err(JournalModelError::new(
                            "prepared backup binding must remain an independent preimage copy",
                        ));
                    }
                }
                if let PresenceV2::Present(current) = &backup.current {
                    current.validate()?;
                    let PreimageV2::Regular { exact: preimage } = &entry.preimage else {
                        unreachable!("backup belongs to replace entry")
                    };
                    if backup.prepared.as_ref() != Some(current)
                        || current.state != preimage.state
                        || current.link_count != 1
                        || current.identity == preimage.identity
                    {
                        return Err(JournalModelError::new(format!(
                            "backup for {} is not an independent exact preimage copy",
                            entry.logical_path
                        )));
                    }
                }
            }
        }
        match self.operation {
            JournalOperationV2::Init | JournalOperationV2::Add | JournalOperationV2::Sync
                if !install_lock_seen =>
            {
                return Err(JournalModelError::new(
                    "Init/Add/Sync cohorts require exactly one selected install-lock entry, last",
                ));
            }
            JournalOperationV2::AtomicWrite if self.entries.len() != 1 => {
                return Err(JournalModelError::new(
                    "AtomicWrite requires exactly one explicitly-role-marked logical target",
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_directories(&self) -> Result<(), JournalModelError> {
        let mut expected_paths = BTreeSet::new();
        for entry in &self.entries {
            for parent in logical_parents(&entry.logical_path) {
                expected_paths.insert(parent);
            }
        }
        let mut expected_paths: Vec<_> = expected_paths.into_iter().collect();
        expected_paths.sort_by(|left, right| {
            path_depth(left)
                .cmp(&path_depth(right))
                .then_with(|| left.cmp(right))
        });
        if expected_paths.len() != self.directories.len() {
            return Err(JournalModelError::new(
                "directory cohort must exactly cover every entry parent",
            ));
        }
        let mut created_ancestors = Vec::new();
        for (index, (directory, expected_path)) in
            self.directories.iter().zip(expected_paths).enumerate()
        {
            if directory.ordinal != ordinal_from_index(index)?
                || directory.logical_path != expected_path
            {
                return Err(JournalModelError::new(
                    "directories must use contiguous parent-first deterministic ordering",
                ));
            }
            validate_logical_path(&directory.logical_path)?;
            match (
                directory.disposition,
                &directory.preimage,
                &directory.current,
            ) {
                (
                    DirectoryDispositionV2::Existing,
                    PresenceV2::Present(preimage),
                    PresenceV2::Present(current),
                ) => {
                    preimage.validate()?;
                    current.validate()?;
                    if directory.created_exact.is_some()
                        || directory.candidate_name.is_some()
                        || !directory.candidate_current.is_missing()
                        || preimage.identity != current.identity
                        || preimage.mode != current.mode
                        || directory.planned_mode != preimage.mode
                        || created_ancestors.iter().any(|ancestor: &String| {
                            is_strict_logical_ancestor(ancestor, &directory.logical_path)
                        })
                    {
                        return Err(JournalModelError::new(format!(
                            "existing directory {} changed identity/mode or descends from a created directory",
                            directory.logical_path
                        )));
                    }
                }
                (DirectoryDispositionV2::Create, PresenceV2::Missing, current) => {
                    validate_posix_mode(directory.planned_mode.posix_mode)?;
                    if directory.planned_mode.readonly
                        || directory.candidate_name.as_deref()
                            != Some(
                                directory_candidate_name(&self.transaction_id, directory.ordinal)
                                    .as_str(),
                            )
                    {
                        return Err(JournalModelError::new(
                            "created directories must be writable and use their transaction-bound candidate name",
                        ));
                    }
                    if let Some(created) = &directory.created_exact {
                        require_private_directory_mode(created, 0o700, "directory candidate")?;
                    } else if !directory.candidate_current.is_missing() || !current.is_missing() {
                        return Err(JournalModelError::new(
                            "directory candidate/target cannot exist without exact prepared ownership evidence",
                        ));
                    }
                    if let PresenceV2::Present(candidate) = &directory.candidate_current {
                        candidate.validate()?;
                        if directory.created_exact.as_ref() != Some(candidate)
                            || !current.is_missing()
                        {
                            return Err(JournalModelError::new(format!(
                                "directory candidate {} changed identity/state or coexists with its target",
                                directory.logical_path
                            )));
                        }
                    }
                    if let PresenceV2::Present(current) = current {
                        current.validate()?;
                        if !directory.created_exact.as_ref().is_some_and(|created| {
                            created.identity == current.identity
                                && created.link_count == current.link_count
                                && current.mode == directory.planned_mode
                        }) || !directory.candidate_current.is_missing()
                        {
                            return Err(JournalModelError::new(format!(
                                "created directory {} is not the no-clobber publication of its exact candidate",
                                directory.logical_path
                            )));
                        }
                    }
                    created_ancestors.push(directory.logical_path.clone());
                }
                _ => {
                    return Err(JournalModelError::new(format!(
                        "directory {} has an invalid exact preimage/current shape",
                        directory.logical_path
                    )));
                }
            }
            let expected_children = derive_managed_children_for(
                &directory.logical_path,
                &self.entries,
                &self.directories,
            );
            if directory.managed_children != expected_children {
                return Err(JournalModelError::new(format!(
                    "directory {} has a non-deterministic managed-child manifest",
                    directory.logical_path
                )));
            }
        }
        let workspace_parent = self
            .directories
            .iter()
            .find(|directory| directory.logical_path == WORKSPACE_PARENT_LOGICAL_PATH)
            .ok_or_else(|| {
                JournalModelError::new(
                    "directory cohort must bind the transaction workspace parent _kit",
                )
            })?;
        if workspace_parent.disposition != DirectoryDispositionV2::Existing
            || workspace_parent.preimage
                != PresenceV2::Present(self.project.workspace_parent_after_workspace.clone())
            || workspace_parent.current
                != PresenceV2::Present(self.project.workspace_parent_current.clone())
        {
            return Err(JournalModelError::new(
                "_kit cohort state is not synchronized with the exact workspace-parent binding",
            ));
        }
        Ok(())
    }

    fn validate_phase_matrix(&self) -> Result<(), JournalModelError> {
        let entry_count = u32::try_from(self.entries.len())
            .map_err(|_| JournalModelError::new("entry cohort exceeds u32"))?;
        match &self.phase {
            JournalPhaseV2::Preparing {
                completed,
                pending_directory,
            } => {
                if usize::try_from(*completed)
                    .ok()
                    .is_none_or(|value| value > self.preparation_step_count())
                    || self.preparation_prefix_len()? != *completed as usize
                    || !self.targets_match_preimages()
                {
                    return Err(JournalModelError::new(
                        "Preparing does not match its exact preparation prefix and preimages",
                    ));
                }
                if let Some(intent) = pending_directory {
                    let PreparationSlot::DirectoryPublish(index) =
                        self.preparation_slot(*completed as usize)?
                    else {
                        return Err(JournalModelError::new(
                            "pending directory intent is not at a publication slot",
                        ));
                    };
                    if self.directories[index].ordinal != intent.ordinal {
                        return Err(JournalModelError::new(
                            "pending directory intent is not at the deterministic ordinal",
                        ));
                    }
                    self.validate_directory_publish_intent(intent)?;
                }
            }
            JournalPhaseV2::Prepared => {
                if self.preparation_prefix_len()? != self.preparation_step_count()
                    || !self.targets_match_preimages()
                {
                    return Err(JournalModelError::new(
                        "Prepared requires every exact preparation and unchanged target",
                    ));
                }
            }
            JournalPhaseV2::Replacing { committed } => {
                if *committed == 0
                    || *committed > entry_count
                    || !self.all_created_directories_present()
                    || !self.entries_match_replacement_prefix(*committed as usize)
                {
                    return Err(JournalModelError::new(
                        "Replacing does not match its exact committed prefix",
                    ));
                }
            }
            JournalPhaseV2::RollingBack { next, pending } => {
                if *next > entry_count {
                    return Err(JournalModelError::new(
                        "rollback cursor exceeds the entry cohort",
                    ));
                }
                if let Some(intent) = pending {
                    if *next == 0 || intent.ordinal().get() != *next - 1 {
                        return Err(JournalModelError::new(
                            "pending rollback intent is not bound to the reverse cursor",
                        ));
                    }
                    self.validate_rollback_intent(intent)?;
                }
                for entry in self.entries.iter().skip(*next as usize) {
                    if !entry_is_rolled_back(entry) {
                        return Err(JournalModelError::new(
                            "entries after the rollback cursor must be exact rollback states",
                        ));
                    }
                }
                for entry in self.entries.iter().take(*next as usize) {
                    if !entry_is_preimage_or_planned(entry) {
                        return Err(JournalModelError::new(
                            "entries before the rollback cursor contain a third state",
                        ));
                    }
                }
            }
            JournalPhaseV2::RollbackComplete {
                cleanup_completed,
                pending,
            } => {
                if !self.entries.iter().all(entry_is_rolled_back) {
                    return Err(JournalModelError::new(
                        "RollbackComplete requires every exact target preimage",
                    ));
                }
                self.validate_cleanup_phase(
                    *cleanup_completed,
                    pending.as_ref(),
                    &self.cleanup_plans.rollback,
                )?;
            }
            JournalPhaseV2::CommitComplete {
                cleanup_completed,
                pending,
            } => {
                if !self.entries.iter().all(entry_is_desired) {
                    return Err(JournalModelError::new(
                        "CommitComplete requires every exact desired target",
                    ));
                }
                self.validate_cleanup_phase(
                    *cleanup_completed,
                    pending.as_ref(),
                    &self.cleanup_plans.commit,
                )?;
            }
        }
        Ok(())
    }

    fn validate_cleanup_phase(
        &self,
        completed: u32,
        pending: Option<&CleanupIntentV2>,
        plan: &[CleanupTargetV2],
    ) -> Result<(), JournalModelError> {
        let completed = usize::try_from(completed)
            .map_err(|_| JournalModelError::new("cleanup counter does not fit usize"))?;
        if completed > plan.len() || (completed == plan.len() && pending.is_some()) {
            return Err(JournalModelError::new(
                "cleanup cursor or pending intent exceeds the immutable plan",
            ));
        }
        if let Some(intent) = pending {
            if Some(&intent.target()) != plan.get(completed) {
                return Err(JournalModelError::new(
                    "pending cleanup intent is not bound to the current plan slot",
                ));
            }
            self.validate_cleanup_intent(intent)?;
        }
        Ok(())
    }

    fn validate_live_identity_independence(&self) -> Result<(), JournalModelError> {
        let mut identities = BTreeSet::new();
        for identity in [
            self.project.root_current.identity,
            self.project.write_lock.identity,
            self.project.workspace_parent_current.identity,
            self.project.workspace.exact.identity,
            self.bootstrap.intent.exact.identity,
            self.bootstrap.exact.identity,
        ] {
            if !identities.insert(identity) {
                return Err(JournalModelError::new(
                    "project binding contains aliased live identities",
                ));
            }
        }
        if let Some(previous) = &self.previous_record {
            if !identities.insert(previous.exact.identity) {
                return Err(JournalModelError::new(
                    "previous journal record aliases a protected live object",
                ));
            }
        }
        for directory in &self.directories {
            if let PresenceV2::Present(current) = &directory.current {
                if directory.logical_path == WORKSPACE_PARENT_LOGICAL_PATH
                    && current == &self.project.workspace_parent_current
                {
                    continue;
                }
                if !identities.insert(current.identity) {
                    return Err(JournalModelError::new(
                        "live directories must have distinct exact identities",
                    ));
                }
            }
            if let PresenceV2::Present(candidate) = &directory.candidate_current {
                if !identities.insert(candidate.identity) {
                    return Err(JournalModelError::new(
                        "live directory candidate aliases an unrelated protected object",
                    ));
                }
            }
        }
        for entry in &self.entries {
            let target = entry.current_target.as_present();
            let stage = entry.stage.current.as_present();
            if let Some(target) = target {
                if !identities.insert(target.identity) {
                    return Err(JournalModelError::new(
                        "live target aliases an unrelated protected object",
                    ));
                }
            }
            if let Some(stage) = stage {
                let allowed_create_alias = entry.action == EntryActionV2::Create
                    && target.is_some_and(|target| target == stage)
                    && stage.link_count == 2;
                if !allowed_create_alias && !identities.insert(stage.identity) {
                    return Err(JournalModelError::new(
                        "live stage aliases an unrelated protected object",
                    ));
                }
            }
            if let Some(backup) = entry
                .backup
                .as_ref()
                .and_then(|backup| backup.current.as_present())
            {
                if !identities.insert(backup.identity) {
                    return Err(JournalModelError::new(
                        "live backup is not an independent exact file",
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_static_successor(&self, next: &Self) -> Result<(), JournalModelError> {
        if self.version != next.version
            || self.transaction_id != next.transaction_id
            || self.operation != next.operation
            || self.project.canonical_root_hash != next.project.canonical_root_hash
            || self.project.root_preimage != next.project.root_preimage
            || self.project.write_lock != next.project.write_lock
            || self.project.workspace_parent_preimage != next.project.workspace_parent_preimage
            || self.project.workspace_parent_after_workspace
                != next.project.workspace_parent_after_workspace
            || self.project.workspace != next.project.workspace
            || self.bootstrap != next.bootstrap
            || self.cleanup_plans != next.cleanup_plans
            || self.entries.len() != next.entries.len()
            || self.directories.len() != next.directories.len()
        {
            return Err(JournalModelError::new(
                "successor changed immutable transaction metadata",
            ));
        }
        for (old, new) in self.entries.iter().zip(&next.entries) {
            if old.ordinal != new.ordinal
                || old.logical_path != new.logical_path
                || old.action != new.action
                || old.role != new.role
                || old.preimage != new.preimage
                || old.planned != new.planned
                || old.stage.name != new.stage.name
                || old.backup.as_ref().map(|backup| &backup.name)
                    != new.backup.as_ref().map(|backup| &backup.name)
            {
                return Err(JournalModelError::new(
                    "successor changed an immutable entry plan",
                ));
            }
        }
        for (old, new) in self.directories.iter().zip(&next.directories) {
            if old.ordinal != new.ordinal
                || old.logical_path != new.logical_path
                || old.disposition != new.disposition
                || old.planned_mode != new.planned_mode
                || old.preimage != new.preimage
                || old.candidate_name != new.candidate_name
                || old.managed_children != new.managed_children
            {
                return Err(JournalModelError::new(
                    "successor changed an immutable directory plan",
                ));
            }
        }
        Ok(())
    }

    fn validate_dynamic_successor(&self, next: &Self) -> Result<(), JournalModelError> {
        let same_runtime = || self.runtime_equals(next);
        match (&self.phase, &next.phase) {
            (
                JournalPhaseV2::Preparing {
                    completed,
                    pending_directory: None,
                },
                JournalPhaseV2::Preparing {
                    completed: next_completed,
                    pending_directory: None,
                },
            ) if next_completed == &completed.saturating_add(1) => {
                let index = usize::try_from(*completed).map_err(|_| {
                    JournalModelError::new("preparation counter does not fit usize")
                })?;
                let mut expected = self.clone_runtime_only();
                let observation = self.preparation_observation_from_successor(next, index)?;
                expected.apply_preparation(index, observation)?;
                if !expected.runtime_equals(next) {
                    return Err(JournalModelError::new(
                        "preparation successor changed state outside the next exact slot",
                    ));
                }
                Ok(())
            }
            (
                JournalPhaseV2::Preparing {
                    completed,
                    pending_directory: None,
                },
                JournalPhaseV2::Preparing {
                    completed: next_completed,
                    pending_directory: Some(intent),
                },
            ) if completed == next_completed && same_runtime() => {
                self.validate_directory_publish_intent(intent)
            }
            (
                JournalPhaseV2::Preparing {
                    completed,
                    pending_directory: Some(intent),
                },
                JournalPhaseV2::Preparing {
                    completed: next_completed,
                    pending_directory: None,
                },
            ) if completed.checked_add(1) == Some(*next_completed) => {
                let index = index_of(intent.ordinal, next.directories.len())?;
                let target = next.directories[index]
                    .current
                    .as_present()
                    .cloned()
                    .ok_or_else(|| {
                        JournalModelError::new(
                            "directory publication successor is missing its exact target",
                        )
                    })?;
                let parent_after = next.parent_current(intent.parent)?.clone();
                let mut expected = self.clone_runtime_only();
                expected.apply_directory_publication(
                    intent,
                    DirectoryPublicationObservationV2::new(
                        target,
                        next.directories[index].candidate_current.clone(),
                        parent_after,
                    ),
                )?;
                if expected.runtime_equals(next) {
                    Ok(())
                } else {
                    Err(JournalModelError::new(
                        "directory publication successor changed state outside its exact candidate/target pair",
                    ))
                }
            }
            (
                JournalPhaseV2::Preparing {
                    completed,
                    pending_directory: None,
                },
                JournalPhaseV2::Prepared,
            ) if usize::try_from(*completed).ok() == Some(self.preparation_step_count())
                && same_runtime() =>
            {
                Ok(())
            }
            (JournalPhaseV2::Prepared, JournalPhaseV2::Replacing { committed: 1 })
            | (
                JournalPhaseV2::Replacing { committed: 0 },
                JournalPhaseV2::Replacing { committed: 1 },
            ) => self.validate_replacement_successor(next, 0),
            (
                JournalPhaseV2::Replacing { committed },
                JournalPhaseV2::Replacing {
                    committed: next_committed,
                },
            ) if *next_committed == committed.saturating_add(1) => {
                self.validate_replacement_successor(next, *committed as usize)
            }
            (
                JournalPhaseV2::Preparing {
                    pending_directory: None,
                    ..
                }
                | JournalPhaseV2::Prepared
                | JournalPhaseV2::Replacing { .. },
                JournalPhaseV2::RollingBack {
                    next: rollback_next,
                    pending: None,
                },
            ) if *rollback_next == self.entries.len() as u32 && same_runtime() => Ok(()),
            (
                JournalPhaseV2::RollingBack {
                    next: cursor,
                    pending: None,
                },
                JournalPhaseV2::RollingBack {
                    next: next_cursor,
                    pending: Some(intent),
                },
            ) if cursor == next_cursor && same_runtime() => self.validate_rollback_intent(intent),
            (
                JournalPhaseV2::RollingBack {
                    next: cursor,
                    pending: Some(intent),
                },
                JournalPhaseV2::RollingBack {
                    next: next_cursor,
                    pending: None,
                },
            ) if cursor.checked_sub(1) == Some(*next_cursor) => {
                let mut expected = self.clone_runtime_only();
                expected.apply_rollback_completion(intent)?;
                if expected.runtime_equals(next) {
                    Ok(())
                } else {
                    Err(JournalModelError::new(
                        "rollback completion does not match its durable intent's unique after-state",
                    ))
                }
            }
            (
                JournalPhaseV2::RollingBack {
                    next: cursor,
                    pending: None,
                },
                JournalPhaseV2::RollingBack {
                    next: next_cursor,
                    pending: None,
                },
            ) if cursor.checked_sub(1) == Some(*next_cursor) && same_runtime() => {
                self.validate_rollback_noop(*next_cursor as usize)
            }
            (
                JournalPhaseV2::RollingBack {
                    next: 0,
                    pending: None,
                },
                JournalPhaseV2::RollbackComplete {
                    cleanup_completed: 0,
                    pending: None,
                },
            ) if same_runtime() => Ok(()),
            (
                JournalPhaseV2::Replacing { committed },
                JournalPhaseV2::CommitComplete {
                    cleanup_completed: 0,
                    pending: None,
                },
            ) if *committed == self.entries.len() as u32 && same_runtime() => Ok(()),
            (
                JournalPhaseV2::RollbackComplete {
                    cleanup_completed,
                    pending: None,
                },
                JournalPhaseV2::RollbackComplete {
                    cleanup_completed: next_completed,
                    pending: Some(intent),
                },
            )
            | (
                JournalPhaseV2::CommitComplete {
                    cleanup_completed,
                    pending: None,
                },
                JournalPhaseV2::CommitComplete {
                    cleanup_completed: next_completed,
                    pending: Some(intent),
                },
            ) if cleanup_completed == next_completed && same_runtime() => {
                self.validate_cleanup_intent(intent)
            }
            (
                JournalPhaseV2::RollbackComplete {
                    cleanup_completed,
                    pending: Some(intent),
                },
                JournalPhaseV2::RollbackComplete {
                    cleanup_completed: next_completed,
                    pending: None,
                },
            )
            | (
                JournalPhaseV2::CommitComplete {
                    cleanup_completed,
                    pending: Some(intent),
                },
                JournalPhaseV2::CommitComplete {
                    cleanup_completed: next_completed,
                    pending: None,
                },
            ) if cleanup_completed.checked_add(1) == Some(*next_completed) => {
                self.validate_cleanup_completion_successor(next, intent)
            }
            (
                JournalPhaseV2::RollbackComplete {
                    cleanup_completed,
                    pending: None,
                },
                JournalPhaseV2::RollbackComplete {
                    cleanup_completed: next_completed,
                    pending: None,
                },
            )
            | (
                JournalPhaseV2::CommitComplete {
                    cleanup_completed,
                    pending: None,
                },
                JournalPhaseV2::CommitComplete {
                    cleanup_completed: next_completed,
                    pending: None,
                },
            ) if cleanup_completed.checked_add(1) == Some(*next_completed) && same_runtime() => {
                let (_, plan) = self.cleanup_cursor()?;
                let target = *plan
                    .get(*cleanup_completed as usize)
                    .ok_or_else(|| JournalModelError::new("cleanup cursor exceeds plan"))?;
                if self.cleanup_target_missing(target)? {
                    Ok(())
                } else {
                    Err(JournalModelError::new(
                        "cleanup cannot advance a present target without a pending intent",
                    ))
                }
            }
            _ => Err(JournalModelError::new(format!(
                "closed journal transition rejects {:?} -> {:?}",
                self.phase, next.phase
            ))),
        }
    }

    fn validate_replacement_successor(
        &self,
        next: &Self,
        index: usize,
    ) -> Result<(), JournalModelError> {
        let next_entry = next
            .entries
            .get(index)
            .ok_or_else(|| JournalModelError::new("replacement successor exceeds cohort"))?;
        let target = next_entry
            .current_target
            .as_present()
            .cloned()
            .ok_or_else(|| JournalModelError::new("replacement successor is missing its target"))?;
        let mut expected = self.clone_runtime_only();
        expected.apply_replacement(
            index,
            ReplacementObservationV2::new(target, next_entry.stage.current.clone()),
        )?;
        if expected.runtime_equals(next) {
            Ok(())
        } else {
            Err(JournalModelError::new(
                "replacement successor changed state outside its exact target/stage pair",
            ))
        }
    }

    fn validate_cleanup_completion_successor(
        &self,
        next: &Self,
        intent: &CleanupIntentV2,
    ) -> Result<(), JournalModelError> {
        let parent_after = match intent {
            CleanupIntentV2::RemoveFile { .. } => None,
            CleanupIntentV2::RemoveDirectory { parent, .. } => {
                Some(next.parent_current(*parent)?.clone())
            }
        };
        let mut expected = self.clone_runtime_only();
        expected.apply_cleanup_completion(intent, parent_after)?;
        if expected.runtime_equals(next) {
            Ok(())
        } else {
            Err(JournalModelError::new(
                "cleanup completion does not match its durable intent's unique after-state",
            ))
        }
    }

    fn preparation_observation_from_successor(
        &self,
        next: &Self,
        index: usize,
    ) -> Result<PreparationObservationV2, JournalModelError> {
        match self.preparation_slot(index)? {
            PreparationSlot::DirectoryCandidate(directory_index) => {
                let exact = next.directories[directory_index]
                    .candidate_current
                    .as_present()
                    .cloned()
                    .ok_or_else(|| {
                        JournalModelError::new("preparation successor is missing its directory")
                    })?;
                let parent = self.directory_parent(directory_index)?;
                Ok(PreparationObservationV2::DirectoryCandidate {
                    exact,
                    parent_after: next.parent_current(parent)?.clone(),
                })
            }
            PreparationSlot::DirectoryPublish(_) => Err(JournalModelError::new(
                "directory publication requires its named armed transition",
            )),
            PreparationSlot::Stage(entry_index) => Ok(PreparationObservationV2::Stage {
                exact: next.entries[entry_index]
                    .stage
                    .current
                    .as_present()
                    .cloned()
                    .ok_or_else(|| {
                        JournalModelError::new("preparation successor is missing its stage")
                    })?,
            }),
            PreparationSlot::Backup(entry_index) => Ok(PreparationObservationV2::Backup {
                exact: next.entries[entry_index]
                    .backup
                    .as_ref()
                    .and_then(|backup| backup.current.as_present())
                    .cloned()
                    .ok_or_else(|| {
                        JournalModelError::new("preparation successor is missing its backup")
                    })?,
            }),
        }
    }

    fn runtime_equals(&self, other: &Self) -> bool {
        self.project.root_current == other.project.root_current
            && self.project.workspace_parent_current == other.project.workspace_parent_current
            && self.entries == other.entries
            && self.directories == other.directories
    }

    fn clone_runtime_only(&self) -> Self {
        self.clone()
    }

    fn preparation_prefix_len(&self) -> Result<usize, JournalModelError> {
        let mut completed = 0;
        let mut missing_seen = false;
        for index in 0..self.preparation_step_count() {
            let present = match self.preparation_slot(index)? {
                PreparationSlot::DirectoryCandidate(directory) => {
                    self.directories[directory].created_exact.is_some()
                }
                PreparationSlot::DirectoryPublish(directory) => {
                    self.directories[directory].current.as_present().is_some()
                }
                PreparationSlot::Stage(entry) => self.entries[entry].stage.prepared.is_some(),
                PreparationSlot::Backup(entry) => self.entries[entry]
                    .backup
                    .as_ref()
                    .expect("backup slot")
                    .prepared
                    .is_some(),
            };
            if present && missing_seen {
                return Err(JournalModelError::new(
                    "preparation observations do not form one deterministic prefix",
                ));
            }
            if present {
                completed += 1;
            } else {
                missing_seen = true;
            }
        }
        Ok(completed)
    }

    fn targets_match_preimages(&self) -> bool {
        self.entries
            .iter()
            .all(|entry| entry.current_target == entry.preimage.presence())
    }

    fn all_created_directories_present(&self) -> bool {
        self.directories.iter().all(|directory| {
            directory.disposition == DirectoryDispositionV2::Existing
                || !directory.current.is_missing()
        })
    }

    fn entries_match_replacement_prefix(&self, committed: usize) -> bool {
        self.entries.iter().enumerate().all(|(index, entry)| {
            if index < committed {
                entry_is_desired(entry)
            } else {
                entry.current_target == entry.preimage.presence()
                    && entry.stage.current.as_present().is_some()
                    && entry
                        .backup
                        .as_ref()
                        .is_none_or(|backup| backup.current.as_present().is_some())
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreparationSlot {
    DirectoryCandidate(usize),
    DirectoryPublish(usize),
    Stage(usize),
    Backup(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JournalFileKindV2 {
    Published,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JournalFileNameV2 {
    transaction_id: TransactionId,
    sequence: u64,
    kind: JournalFileKindV2,
}

impl JournalFileNameV2 {
    pub(super) fn transaction_id(&self) -> &TransactionId {
        &self.transaction_id
    }

    pub(super) const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) const fn kind(&self) -> JournalFileKindV2 {
        self.kind
    }
}

pub(super) fn transaction_directory_name(transaction_id: &TransactionId) -> String {
    format!("{TRANSACTION_PREFIX}{}", transaction_id.as_str())
}

pub(super) fn parse_transaction_directory_name(
    name: &str,
) -> Result<TransactionId, JournalModelError> {
    let value = name.strip_prefix(TRANSACTION_PREFIX).ok_or_else(|| {
        JournalModelError::new("transaction directory does not use the v2 namespace")
    })?;
    let transaction_id = TransactionId::parse(value)?;
    if transaction_directory_name(&transaction_id) != name {
        return Err(JournalModelError::new(
            "transaction directory name is not canonical",
        ));
    }
    Ok(transaction_id)
}

pub(super) fn finalization_lease_name(transaction_id: &TransactionId) -> String {
    format!(
        "{FINALIZATION_PREFIX}{}{JOURNAL_SUFFIX}",
        transaction_id.as_str()
    )
}

pub(super) fn bootstrap_owner_name(transaction_id: &TransactionId) -> String {
    format!(
        "{BOOTSTRAP_PREFIX}{}{PARTIAL_SUFFIX}",
        transaction_id.as_str()
    )
}

pub(super) fn bootstrap_intent_name(transaction_id: &TransactionId) -> String {
    format!(
        "{BOOTSTRAP_INTENT_PREFIX}{}{PARTIAL_SUFFIX}",
        transaction_id.as_str()
    )
}

pub(super) fn parse_bootstrap_owner_name(name: &str) -> Result<TransactionId, JournalModelError> {
    let value = name
        .strip_prefix(BOOTSTRAP_PREFIX)
        .and_then(|value| value.strip_suffix(PARTIAL_SUFFIX))
        .ok_or_else(|| JournalModelError::new("invalid v2 bootstrap-owner name"))?;
    let transaction_id = TransactionId::parse(value)?;
    if bootstrap_owner_name(&transaction_id) != name {
        return Err(JournalModelError::new(
            "bootstrap-owner name is not canonical",
        ));
    }
    Ok(transaction_id)
}

pub(super) fn parse_finalization_lease_name(
    name: &str,
) -> Result<TransactionId, JournalModelError> {
    let value = name
        .strip_prefix(FINALIZATION_PREFIX)
        .and_then(|value| value.strip_suffix(JOURNAL_SUFFIX))
        .ok_or_else(|| JournalModelError::new("invalid v2 finalization-lease name"))?;
    let transaction_id = TransactionId::parse(value)?;
    if finalization_lease_name(&transaction_id) != name {
        return Err(JournalModelError::new(
            "finalization-lease name is not canonical",
        ));
    }
    Ok(transaction_id)
}

pub(super) fn stage_name(transaction_id: &TransactionId, ordinal: ArtifactOrdinal) -> String {
    format!(
        "{STAGE_PREFIX}{}-{ordinal:0width$}",
        transaction_id.as_str(),
        ordinal = ordinal.get(),
        width = ORDINAL_DECIMAL_WIDTH
    )
}

pub(super) fn backup_name(transaction_id: &TransactionId, ordinal: ArtifactOrdinal) -> String {
    format!(
        "{BACKUP_PREFIX}{}-{ordinal:0width$}",
        transaction_id.as_str(),
        ordinal = ordinal.get(),
        width = ORDINAL_DECIMAL_WIDTH
    )
}

pub(super) fn directory_candidate_name(
    transaction_id: &TransactionId,
    ordinal: ArtifactOrdinal,
) -> String {
    format!(
        "{DIRECTORY_CANDIDATE_PREFIX}{}-{ordinal:0width$}",
        transaction_id.as_str(),
        ordinal = ordinal.get(),
        width = ORDINAL_DECIMAL_WIDTH
    )
}

pub(super) fn journal_record_name(transaction_id: &TransactionId, sequence: u64) -> String {
    format!(
        "{TRANSACTION_PREFIX}{}-{sequence:0width$}{JOURNAL_SUFFIX}",
        transaction_id.as_str(),
        width = SEQUENCE_DECIMAL_WIDTH
    )
}

pub(super) fn journal_partial_name(transaction_id: &TransactionId, sequence: u64) -> String {
    format!(
        "{TRANSACTION_PREFIX}{}-{sequence:0width$}{PARTIAL_SUFFIX}",
        transaction_id.as_str(),
        width = SEQUENCE_DECIMAL_WIDTH
    )
}

pub(super) fn parse_journal_file_name(name: &str) -> Result<JournalFileNameV2, JournalModelError> {
    let (stem, kind) = if let Some(stem) = name.strip_suffix(PARTIAL_SUFFIX) {
        (stem, JournalFileKindV2::Partial)
    } else if let Some(stem) = name.strip_suffix(JOURNAL_SUFFIX) {
        (stem, JournalFileKindV2::Published)
    } else {
        return Err(JournalModelError::new(
            "journal filename must end in .json or .json.partial",
        ));
    };
    let value = stem
        .strip_prefix(TRANSACTION_PREFIX)
        .ok_or_else(|| JournalModelError::new("journal filename does not use the v2 namespace"))?;
    let expected_len = TRANSACTION_ID_HEX_LEN + 1 + SEQUENCE_DECIMAL_WIDTH;
    if value.len() != expected_len || value.as_bytes()[TRANSACTION_ID_HEX_LEN] != b'-' {
        return Err(JournalModelError::new(
            "journal filename does not have fixed-width identity and sequence fields",
        ));
    }
    let transaction_id = TransactionId::parse(&value[..TRANSACTION_ID_HEX_LEN])?;
    let sequence_text = &value[TRANSACTION_ID_HEX_LEN + 1..];
    if !sequence_text.as_bytes().iter().all(u8::is_ascii_digit) {
        return Err(JournalModelError::new(
            "journal sequence must be exactly 20 decimal digits",
        ));
    }
    let sequence = sequence_text
        .parse::<u64>()
        .map_err(|_| JournalModelError::new("journal sequence exceeds u64"))?;
    let canonical = match kind {
        JournalFileKindV2::Published => journal_record_name(&transaction_id, sequence),
        JournalFileKindV2::Partial => journal_partial_name(&transaction_id, sequence),
    };
    if canonical != name {
        return Err(JournalModelError::new("journal filename is not canonical"));
    }
    Ok(JournalFileNameV2 {
        transaction_id,
        sequence,
        kind,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct PartialEnvelopeHeaderV2 {
    magic: String,
    version: u32,
    owner_tag: Sha256Digest,
    transaction_id: TransactionId,
    canonical_root_hash: Sha256Digest,
    workspace_parent_identity: ObjectIdentityV2,
    workspace_identity: ObjectIdentityV2,
    sequence: u64,
    payload_hash: Sha256Digest,
    payload_len: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PartialEnvelopeHeaderWireV2 {
    magic: String,
    version: u32,
    owner_tag: Sha256Digest,
    transaction_id: TransactionId,
    canonical_root_hash: Sha256Digest,
    workspace_parent_identity: ObjectIdentityV2,
    workspace_identity: ObjectIdentityV2,
    sequence: u64,
    payload_hash: Sha256Digest,
    payload_len: u64,
}

impl<'de> Deserialize<'de> for PartialEnvelopeHeaderV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PartialEnvelopeHeaderWireV2::deserialize(deserializer)?;
        let header = Self {
            magic: wire.magic,
            version: wire.version,
            owner_tag: wire.owner_tag,
            transaction_id: wire.transaction_id,
            canonical_root_hash: wire.canonical_root_hash,
            workspace_parent_identity: wire.workspace_parent_identity,
            workspace_identity: wire.workspace_identity,
            sequence: wire.sequence,
            payload_hash: wire.payload_hash,
            payload_len: wire.payload_len,
        };
        header.validate().map_err(D::Error::custom)?;
        Ok(header)
    }
}

impl PartialEnvelopeHeaderV2 {
    pub(super) fn for_payload(
        transaction_id: TransactionId,
        project: &ProjectBindingV2,
        sequence: u64,
        payload: &[u8],
    ) -> Result<Self, JournalModelError> {
        let header = Self {
            magic: PARTIAL_MAGIC.to_owned(),
            version: JOURNAL_VERSION,
            owner_tag: project.workspace.owner_tag.clone(),
            transaction_id,
            canonical_root_hash: project.canonical_root_hash.clone(),
            workspace_parent_identity: project.workspace_parent_after_workspace.identity,
            workspace_identity: project.workspace.exact.identity,
            sequence,
            payload_hash: content_hash(payload),
            payload_len: u64::try_from(payload.len())
                .map_err(|_| JournalModelError::new("partial payload exceeds u64"))?,
        };
        header.validate()?;
        Ok(header)
    }

    pub(super) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) fn owner_tag(&self) -> &Sha256Digest {
        &self.owner_tag
    }

    pub(super) fn to_prefix_bytes(&self) -> Result<Vec<u8>, JournalModelError> {
        self.validate()?;
        let mut bytes = serde_json::to_vec(self).map_err(|error| {
            JournalModelError::new(format!("could not serialize partial header: {error}"))
        })?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub(super) fn parse_prefix(bytes: &[u8]) -> Result<(Self, &[u8]), JournalModelError> {
        let newline = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| JournalModelError::new("partial envelope has no complete header"))?;
        let header: Self = serde_json::from_slice(&bytes[..newline]).map_err(|error| {
            JournalModelError::new(format!("invalid partial-envelope header: {error}"))
        })?;
        if bytes[..=newline] != header.to_prefix_bytes()? {
            return Err(JournalModelError::new(
                "partial-envelope header bytes are not canonical",
            ));
        }
        Ok((header, &bytes[newline + 1..]))
    }

    pub(super) fn validate_payload_prefix(
        &self,
        bytes: &[u8],
        expected_payload: &[u8],
    ) -> Result<(), JournalModelError> {
        self.validate()?;
        if self.payload_len != expected_payload.len() as u64
            || self.payload_hash != content_hash(expected_payload)
            || bytes.len() > expected_payload.len()
            || bytes != &expected_payload[..bytes.len()]
        {
            return Err(JournalModelError::new(
                "partial payload is not an exact prefix of its bound journal record",
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), JournalModelError> {
        if self.magic != PARTIAL_MAGIC || self.version != JOURNAL_VERSION {
            return Err(JournalModelError::new(
                "partial envelope has unsupported magic or version",
            ));
        }
        TransactionId::parse(self.transaction_id.as_str())?;
        Sha256Digest::parse(self.owner_tag.as_str())?;
        Sha256Digest::parse(self.canonical_root_hash.as_str())?;
        Sha256Digest::parse(self.payload_hash.as_str())?;
        Ok(())
    }

    fn validate_binding(
        &self,
        transaction_id: &TransactionId,
        project: &ProjectBindingV2,
        sequence: u64,
    ) -> Result<(), JournalModelError> {
        if &self.transaction_id != transaction_id
            || self.canonical_root_hash != project.canonical_root_hash
            || self.owner_tag != project.workspace.owner_tag
            || self.workspace_parent_identity != project.workspace_parent_after_workspace.identity
            || self.workspace_identity != project.workspace.exact.identity
            || self.sequence != sequence
        {
            return Err(JournalModelError::new(
                "partial envelope is not owned by the exact transaction workspace and sequence",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct PartialRecordBindingV2 {
    sequence: u64,
    name: String,
    exact: ExactFileStateV2,
    header: PartialEnvelopeHeaderV2,
}

impl PartialRecordBindingV2 {
    pub(super) fn new(
        candidate: &JournalSnapshotV2,
        exact: ExactFileStateV2,
        header: PartialEnvelopeHeaderV2,
        envelope_bytes: &[u8],
    ) -> Result<Self, JournalModelError> {
        let sequence = candidate.sequence;
        let binding = Self {
            sequence,
            name: journal_partial_name(&candidate.transaction_id, sequence),
            exact,
            header,
        };
        binding.validate_candidate(candidate, envelope_bytes)?;
        Ok(binding)
    }

    pub(super) const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn exact(&self) -> &ExactFileStateV2 {
        &self.exact
    }

    pub(super) fn completed_record_binding(
        &self,
        candidate: &JournalSnapshotV2,
    ) -> Result<RecordBindingV2, JournalModelError> {
        if self.sequence != candidate.sequence
            || self.name != journal_partial_name(&candidate.transaction_id, candidate.sequence)
        {
            return Err(JournalModelError::new(
                "completed partial is not bound to the candidate record",
            ));
        }
        self.header.validate_binding(
            &candidate.transaction_id,
            &candidate.project,
            candidate.sequence,
        )?;
        let expected = candidate.expected_record_binding(self.exact.identity)?;
        if self.exact.state != expected.exact.state || self.exact.link_count != 1 {
            return Err(JournalModelError::new(
                "partial cannot publish until its exact bytes equal the canonical complete record envelope",
            ));
        }
        Ok(expected)
    }

    fn validate_candidate(
        &self,
        candidate: &JournalSnapshotV2,
        envelope_bytes: &[u8],
    ) -> Result<(), JournalModelError> {
        let expected_payload = candidate.to_json_bytes()?;
        let (header, payload_prefix) = PartialEnvelopeHeaderV2::parse_prefix(envelope_bytes)?;
        if header != self.header
            || self.sequence != candidate.sequence
            || self.name != journal_partial_name(&candidate.transaction_id, candidate.sequence)
            || self.exact.state.content_hash != content_hash(envelope_bytes)
            || self.exact.state.byte_len != envelope_bytes.len() as u64
        {
            return Err(JournalModelError::new(
                "partial binding does not match its exact envelope bytes and candidate sequence",
            ));
        }
        self.validate_exact_file()?;
        self.header.validate_binding(
            &candidate.transaction_id,
            &candidate.project,
            candidate.sequence,
        )?;
        self.header
            .validate_payload_prefix(payload_prefix, &expected_payload)
    }

    fn validate_next_after(&self, snapshot: &JournalSnapshotV2) -> Result<(), JournalModelError> {
        let expected_sequence = snapshot
            .sequence
            .checked_add(1)
            .ok_or_else(|| JournalModelError::new("partial sequence overflow"))?;
        if self.sequence != expected_sequence
            || self.name != journal_partial_name(&snapshot.transaction_id, expected_sequence)
        {
            return Err(JournalModelError::new(
                "partial record is not the canonical immediate successor candidate",
            ));
        }
        self.validate_exact_file()?;
        self.header.validate_binding(
            &snapshot.transaction_id,
            &snapshot.project,
            expected_sequence,
        )
    }

    fn validate_exact_file(&self) -> Result<(), JournalModelError> {
        require_private_file_mode(&self.exact, 0o600, "partial journal record")?;
        if self.exact.link_count != 1 {
            return Err(JournalModelError::new(
                "partial journal record must be independently linked",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum FinalizationOutcomeV2 {
    Commit,
    Rollback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum FinalizationStateV2 {
    WorkspacePresent,
    WorkspaceRemoved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct FinalizationLeaseV2 {
    version: u32,
    generation: u64,
    transaction_id: TransactionId,
    canonical_root_hash: Sha256Digest,
    owner_tag: Sha256Digest,
    outcome: FinalizationOutcomeV2,
    terminal_sequence: u64,
    workspace_parent_before: ExactDirectoryStateV2,
    workspace_parent_current: ExactDirectoryStateV2,
    workspace_name: String,
    workspace: PresenceV2<ExactDirectoryStateV2>,
    bootstrap: WorkspaceBootstrapBindingV2,
    bootstrap_intent_current: PresenceV2<ExactFileStateV2>,
    bootstrap_current: PresenceV2<ExactFileStateV2>,
    records: Vec<RecordBindingV2>,
    partial: Option<PartialRecordBindingV2>,
    state: FinalizationStateV2,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FinalizationLeaseWireV2 {
    version: u32,
    generation: u64,
    transaction_id: TransactionId,
    canonical_root_hash: Sha256Digest,
    owner_tag: Sha256Digest,
    outcome: FinalizationOutcomeV2,
    terminal_sequence: u64,
    workspace_parent_before: ExactDirectoryStateV2,
    workspace_parent_current: ExactDirectoryStateV2,
    workspace_name: String,
    workspace: PresenceV2<ExactDirectoryStateV2>,
    bootstrap: WorkspaceBootstrapBindingV2,
    bootstrap_intent_current: PresenceV2<ExactFileStateV2>,
    bootstrap_current: PresenceV2<ExactFileStateV2>,
    records: Vec<RecordBindingV2>,
    partial: Option<PartialRecordBindingV2>,
    state: FinalizationStateV2,
}

impl<'de> Deserialize<'de> for FinalizationLeaseV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FinalizationLeaseWireV2::deserialize(deserializer)?;
        let lease = Self {
            version: wire.version,
            generation: wire.generation,
            transaction_id: wire.transaction_id,
            canonical_root_hash: wire.canonical_root_hash,
            owner_tag: wire.owner_tag,
            outcome: wire.outcome,
            terminal_sequence: wire.terminal_sequence,
            workspace_parent_before: wire.workspace_parent_before,
            workspace_parent_current: wire.workspace_parent_current,
            workspace_name: wire.workspace_name,
            workspace: wire.workspace,
            bootstrap: wire.bootstrap,
            bootstrap_intent_current: wire.bootstrap_intent_current,
            bootstrap_current: wire.bootstrap_current,
            records: wire.records,
            partial: wire.partial,
            state: wire.state,
        };
        lease.validate().map_err(D::Error::custom)?;
        Ok(lease)
    }
}

impl FinalizationLeaseV2 {
    pub(super) fn arm(
        terminal: &JournalSnapshotV2,
        records: Vec<RecordBindingV2>,
        partial: Option<PartialRecordBindingV2>,
    ) -> Result<Self, JournalModelError> {
        if !terminal.ready_for_finalization() {
            return Err(JournalModelError::new(
                "finalization lease requires a terminal snapshot with completed cleanup",
            ));
        }
        let outcome = match &terminal.phase {
            JournalPhaseV2::RollbackComplete { .. } => FinalizationOutcomeV2::Rollback,
            JournalPhaseV2::CommitComplete { .. } => FinalizationOutcomeV2::Commit,
            _ => unreachable!("ready_for_finalization restricts phases"),
        };
        let lease = Self {
            version: JOURNAL_VERSION,
            generation: 0,
            transaction_id: terminal.transaction_id.clone(),
            canonical_root_hash: terminal.project.canonical_root_hash.clone(),
            owner_tag: terminal.project.workspace.owner_tag.clone(),
            outcome,
            terminal_sequence: terminal.sequence,
            workspace_parent_before: terminal.project.workspace_parent_current.clone(),
            workspace_parent_current: terminal.project.workspace_parent_current.clone(),
            workspace_name: terminal.project.workspace.name.clone(),
            workspace: PresenceV2::Present(terminal.project.workspace.exact.clone()),
            bootstrap: terminal.bootstrap.clone(),
            bootstrap_intent_current: PresenceV2::Present(terminal.bootstrap.intent.exact.clone()),
            bootstrap_current: PresenceV2::Present(terminal.bootstrap.exact.clone()),
            records,
            partial,
            state: FinalizationStateV2::WorkspacePresent,
        };
        lease.validate_against_terminal(terminal)?;
        Ok(lease)
    }

    pub(super) fn name(&self) -> String {
        finalization_lease_name(&self.transaction_id)
    }

    pub(super) fn state(&self) -> FinalizationStateV2 {
        self.state
    }

    pub(super) fn records(&self) -> &[RecordBindingV2] {
        &self.records
    }

    pub(super) fn partial(&self) -> Option<&PartialRecordBindingV2> {
        self.partial.as_ref()
    }

    pub(super) fn mark_workspace_removed(
        &self,
        workspace_parent_after: ExactDirectoryStateV2,
    ) -> Result<Self, JournalModelError> {
        if self.state != FinalizationStateV2::WorkspacePresent {
            return Err(JournalModelError::new(
                "finalization workspace is already durably removed",
            ));
        }
        let mut next = self.clone();
        next.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| JournalModelError::new("finalization generation overflow"))?;
        next.workspace = PresenceV2::Missing;
        validate_parent_removal_transition(&self.workspace_parent_before, &workspace_parent_after)?;
        next.workspace_parent_current = workspace_parent_after;
        next.bootstrap_intent_current = PresenceV2::Missing;
        next.bootstrap_current = PresenceV2::Missing;
        next.records.clear();
        next.partial = None;
        next.state = FinalizationStateV2::WorkspaceRemoved;
        self.validate_successor(&next)?;
        Ok(next)
    }

    pub(super) fn validate_successor(&self, next: &Self) -> Result<(), JournalModelError> {
        self.validate()?;
        next.validate()?;
        if self.state != FinalizationStateV2::WorkspacePresent
            || next.state != FinalizationStateV2::WorkspaceRemoved
            || self.generation.checked_add(1) != Some(next.generation)
            || self.version != next.version
            || self.transaction_id != next.transaction_id
            || self.canonical_root_hash != next.canonical_root_hash
            || self.owner_tag != next.owner_tag
            || self.outcome != next.outcome
            || self.terminal_sequence != next.terminal_sequence
            || self.workspace_parent_before != next.workspace_parent_before
            || self.workspace_name != next.workspace_name
            || self.bootstrap != next.bootstrap
            || !next.workspace.is_missing()
            || !next.bootstrap_intent_current.is_missing()
            || !next.bootstrap_current.is_missing()
            || !next.records.is_empty()
            || next.partial.is_some()
        {
            return Err(JournalModelError::new(
                "closed finalization transition only permits exact workspace-present to workspace-removed",
            ));
        }
        Ok(())
    }

    pub(super) fn to_json_bytes(&self) -> Result<Vec<u8>, JournalModelError> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|error| {
            JournalModelError::new(format!("could not serialize finalization lease: {error}"))
        })?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub(super) fn from_json_slice(bytes: &[u8]) -> Result<Self, JournalModelError> {
        serde_json::from_slice(bytes).map_err(|error| {
            JournalModelError::new(format!("invalid finalization lease JSON: {error}"))
        })
    }

    fn validate_against_terminal(
        &self,
        terminal: &JournalSnapshotV2,
    ) -> Result<(), JournalModelError> {
        self.validate()?;
        let terminal_record = self.records.last().ok_or_else(|| {
            JournalModelError::new("finalization inventory has no terminal record")
        })?;
        terminal.validate_record_binding(terminal_record)?;
        if self.transaction_id != terminal.transaction_id
            || self.canonical_root_hash != terminal.project.canonical_root_hash
            || self.owner_tag != terminal.project.workspace.owner_tag
            || self.workspace_name != terminal.project.workspace.name
            || self.workspace_parent_before != terminal.project.workspace_parent_current
            || self.workspace_parent_current != terminal.project.workspace_parent_current
            || self.bootstrap != terminal.bootstrap
            || self.bootstrap_intent_current
                != PresenceV2::Present(terminal.bootstrap.intent.exact.clone())
            || self.bootstrap_current != PresenceV2::Present(terminal.bootstrap.exact.clone())
            || self.workspace != PresenceV2::Present(terminal.project.workspace.exact.clone())
        {
            return Err(JournalModelError::new(
                "finalization lease is not bound to its terminal exact workspace",
            ));
        }
        if let Some(partial) = &self.partial {
            partial.validate_next_after(terminal)?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), JournalModelError> {
        if self.version != JOURNAL_VERSION {
            return Err(JournalModelError::new(
                "unsupported finalization-lease version",
            ));
        }
        TransactionId::parse(self.transaction_id.as_str())?;
        Sha256Digest::parse(self.canonical_root_hash.as_str())?;
        Sha256Digest::parse(self.owner_tag.as_str())?;
        if self.workspace_name != transaction_directory_name(&self.transaction_id) {
            return Err(JournalModelError::new(
                "finalization lease has a non-canonical workspace name",
            ));
        }
        self.bootstrap.intent.validate()?;
        self.bootstrap.envelope.validate()?;
        self.bootstrap.validate_exact_file()?;
        if self.bootstrap.intent.envelope.transaction_id != self.transaction_id
            || self.bootstrap.intent.envelope.canonical_root_hash != self.canonical_root_hash
            || self.bootstrap.intent.envelope.workspace_name != self.workspace_name
            || self.bootstrap.intent.envelope.workspace_parent_preimage
                != self.bootstrap.envelope.workspace_parent_preimage
            || self.bootstrap.name != bootstrap_owner_name(&self.transaction_id)
            || self.bootstrap.envelope.transaction_id != self.transaction_id
            || self.bootstrap.envelope.canonical_root_hash != self.canonical_root_hash
            || self.bootstrap.envelope.owner_tag != self.owner_tag
            || self.bootstrap.envelope.workspace_name != self.workspace_name
        {
            return Err(JournalModelError::new(
                "finalization lease bootstrap lineage does not match its parent/workspace owner",
            ));
        }
        match self.state {
            FinalizationStateV2::WorkspacePresent => {
                let workspace = self.workspace.as_present().ok_or_else(|| {
                    JournalModelError::new("workspace-present lease is missing its exact workspace")
                })?;
                require_private_directory_mode(workspace, 0o700, "finalization workspace")?;
                if self.workspace_parent_current != self.workspace_parent_before
                    || self.bootstrap_intent_current
                        != PresenceV2::Present(self.bootstrap.intent.exact.clone())
                    || self.bootstrap_current != PresenceV2::Present(self.bootstrap.exact.clone())
                {
                    return Err(JournalModelError::new(
                        "workspace-present lease must bind its exact parent and bootstrap file",
                    ));
                }
                if self.records.is_empty() {
                    return Err(JournalModelError::new(
                        "workspace-present lease requires an exact record inventory",
                    ));
                }
                let mut identities = BTreeSet::new();
                identities.insert(workspace.identity);
                if !identities.insert(self.bootstrap.intent.exact.identity) {
                    return Err(JournalModelError::new(
                        "finalization bootstrap intent aliases its workspace",
                    ));
                }
                if !identities.insert(self.bootstrap.exact.identity) {
                    return Err(JournalModelError::new(
                        "finalization bootstrap aliases its workspace",
                    ));
                }
                for (index, record) in self.records.iter().enumerate() {
                    if record.sequence != index as u64
                        || record.name != journal_record_name(&self.transaction_id, index as u64)
                        || record.exact.link_count != 1
                        || !identities.insert(record.exact.identity)
                    {
                        return Err(JournalModelError::new(
                            "finalization record inventory is not contiguous, canonical, exact, and independent",
                        ));
                    }
                    require_private_file_mode(&record.exact, 0o600, "finalization journal record")?;
                }
                if self.records.last().map(|record| record.sequence) != Some(self.terminal_sequence)
                {
                    return Err(JournalModelError::new(
                        "finalization inventory does not end at its terminal sequence",
                    ));
                }
                if let Some(partial) = &self.partial {
                    if partial.sequence != self.terminal_sequence.saturating_add(1)
                        || partial.name
                            != journal_partial_name(&self.transaction_id, partial.sequence)
                        || !identities.insert(partial.exact.identity)
                    {
                        return Err(JournalModelError::new(
                            "finalization partial is not the exact independent next candidate",
                        ));
                    }
                }
            }
            FinalizationStateV2::WorkspaceRemoved => {
                if !self.workspace.is_missing()
                    || !self.bootstrap_intent_current.is_missing()
                    || !self.bootstrap_current.is_missing()
                    || !self.records.is_empty()
                    || self.partial.is_some()
                {
                    return Err(JournalModelError::new(
                        "workspace-removed lease must have an empty exact inventory",
                    ));
                }
                validate_parent_removal_transition(
                    &self.workspace_parent_before,
                    &self.workspace_parent_current,
                )?;
            }
        }
        Ok(())
    }
}

fn validate_posix_mode(mode: Option<u32>) -> Result<(), JournalModelError> {
    #[cfg(unix)]
    {
        let mode = mode.ok_or_else(|| {
            JournalModelError::new("exact Unix state requires an explicit POSIX mode")
        })?;
        if mode > 0o7777 {
            return Err(JournalModelError::new(
                "POSIX mode contains bits outside the permission/special-bit mask",
            ));
        }
    }
    #[cfg(not(unix))]
    if mode.is_some() {
        return Err(JournalModelError::new(
            "non-Unix exact state must not contain a POSIX mode",
        ));
    }
    Ok(())
}

const fn private_posix_mode(mode: u32) -> Option<u32> {
    #[cfg(unix)]
    {
        Some(mode)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        None
    }
}

fn require_private_file_mode(
    exact: &ExactFileStateV2,
    expected_mode: u32,
    label: &str,
) -> Result<(), JournalModelError> {
    exact.validate()?;
    if exact.state.readonly || exact.state.posix_mode != private_posix_mode(expected_mode) {
        return Err(JournalModelError::new(format!(
            "{label} must have exact private mode {expected_mode:#o} and be writable"
        )));
    }
    Ok(())
}

fn require_private_directory_mode(
    exact: &ExactDirectoryStateV2,
    expected_mode: u32,
    label: &str,
) -> Result<(), JournalModelError> {
    exact.validate()?;
    if exact.mode.readonly || exact.mode.posix_mode != private_posix_mode(expected_mode) {
        return Err(JournalModelError::new(format!(
            "{label} must have exact private mode {expected_mode:#o} and be writable"
        )));
    }
    Ok(())
}

fn validate_parent_creation_transition(
    before: &ExactDirectoryStateV2,
    after: &ExactDirectoryStateV2,
) -> Result<(), JournalModelError> {
    before.validate()?;
    after.validate()?;
    if before.identity != after.identity
        || before.mode != after.mode
        || before.link_count.checked_add(1) != Some(after.link_count)
    {
        return Err(JournalModelError::new(
            "directory creation must preserve the exact parent identity/mode and add one link",
        ));
    }
    Ok(())
}

fn validate_parent_removal_transition(
    before: &ExactDirectoryStateV2,
    after: &ExactDirectoryStateV2,
) -> Result<(), JournalModelError> {
    validate_parent_creation_transition(after, before).map_err(|_| {
        JournalModelError::new(
            "directory removal must preserve the exact parent identity/mode and remove one link",
        )
    })
}

fn ordinal_from_index(index: usize) -> Result<ArtifactOrdinal, JournalModelError> {
    let value =
        u32::try_from(index).map_err(|_| JournalModelError::new("cohort index exceeds u32"))?;
    ArtifactOrdinal::new(value)
}

fn index_of(ordinal: ArtifactOrdinal, len: usize) -> Result<usize, JournalModelError> {
    let index = usize::try_from(ordinal.get()).expect("u32 always fits supported usize targets");
    if index >= len {
        return Err(JournalModelError::new(
            "artifact ordinal exceeds its deterministic cohort",
        ));
    }
    Ok(index)
}

fn validate_logical_path(path: &str) -> Result<(), JournalModelError> {
    if path.is_empty()
        || path.len() > 4096
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(JournalModelError::new(
            "logical path must be a bounded relative slash-separated UTF-8 path",
        ));
    }
    for component in path.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > 255
            || component.ends_with([' ', '.'])
            || component.contains(':')
            || is_windows_reserved_component(component)
        {
            return Err(JournalModelError::new(format!(
                "logical path contains an unsafe or non-portable component: {component:?}"
            )));
        }
    }
    Ok(())
}

fn is_windows_reserved_component(component: &str) -> bool {
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
}

fn logical_parents(path: &str) -> Vec<String> {
    let mut parents = Vec::new();
    let mut offset = 0;
    while let Some(relative) = path[offset..].find('/') {
        let end = offset + relative;
        parents.push(path[..end].to_owned());
        offset = end + 1;
    }
    parents
}

fn path_depth(path: &str) -> usize {
    path.bytes().filter(|byte| *byte == b'/').count() + 1
}

fn immediate_parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

fn leaf_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, leaf)| leaf)
}

fn is_strict_logical_ancestor(ancestor: &str, descendant: &str) -> bool {
    descendant
        .strip_prefix(ancestor)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

fn populate_managed_children(
    entries: &[JournalEntryV2],
    directories: &mut [JournalDirectoryV2],
) -> Result<(), JournalModelError> {
    let snapshot = directories.to_vec();
    for directory in directories {
        directory.managed_children =
            derive_managed_children_for(&directory.logical_path, entries, &snapshot);
    }
    Ok(())
}

fn derive_managed_children_for(
    directory_path: &str,
    entries: &[JournalEntryV2],
    directories: &[JournalDirectoryV2],
) -> Vec<ManagedChildV2> {
    let mut children = BTreeSet::new();
    for entry in entries {
        if immediate_parent(&entry.logical_path) == Some(directory_path) {
            children.insert(ManagedChildV2::new(
                leaf_name(&entry.logical_path),
                ManagedChildKindV2::File,
            ));
            children.insert(ManagedChildV2::new(
                entry.stage.name.clone(),
                ManagedChildKindV2::File,
            ));
            if let Some(backup) = &entry.backup {
                children.insert(ManagedChildV2::new(
                    backup.name.clone(),
                    ManagedChildKindV2::File,
                ));
            }
        }
    }
    for directory in directories {
        if immediate_parent(&directory.logical_path) == Some(directory_path) {
            children.insert(ManagedChildV2::new(
                leaf_name(&directory.logical_path),
                ManagedChildKindV2::Directory,
            ));
            if let Some(candidate_name) = &directory.candidate_name {
                children.insert(ManagedChildV2::new(
                    candidate_name.clone(),
                    ManagedChildKindV2::Directory,
                ));
            }
        }
    }
    children.into_iter().collect()
}

fn derive_cleanup_plans(
    entries: &[JournalEntryV2],
    directories: &[JournalDirectoryV2],
) -> CleanupPlansV2 {
    let backups = entries.iter().rev().filter_map(|entry| {
        entry.backup.as_ref().map(|_| CleanupTargetV2::Backup {
            ordinal: entry.ordinal,
        })
    });
    let stages = entries.iter().rev().map(|entry| CleanupTargetV2::Stage {
        ordinal: entry.ordinal,
    });
    let candidates = directories.iter().rev().filter_map(|directory| {
        (directory.disposition == DirectoryDispositionV2::Create).then_some(
            CleanupTargetV2::DirectoryCandidate {
                ordinal: directory.ordinal,
            },
        )
    });
    let created = directories.iter().rev().filter_map(|directory| {
        (directory.disposition == DirectoryDispositionV2::Create).then_some(
            CleanupTargetV2::CreatedDirectory {
                ordinal: directory.ordinal,
            },
        )
    });
    let commit = backups
        .clone()
        .chain(stages.clone())
        .chain(candidates.clone())
        .collect();
    let rollback = backups
        .chain(stages)
        .chain(candidates)
        .chain(created)
        .collect();
    CleanupPlansV2 { commit, rollback }
}

fn entry_is_desired(entry: &JournalEntryV2) -> bool {
    let Some(prepared) = &entry.stage.prepared else {
        return false;
    };
    let Some(target) = entry.current_target.as_present() else {
        return false;
    };
    if target.identity != prepared.identity || target.state != prepared.state {
        return false;
    }
    match entry.action {
        EntryActionV2::Create => match entry.stage.current.as_present() {
            Some(stage) => stage == target && target.link_count == 2,
            None => target.link_count == 1,
        },
        EntryActionV2::Replace => target.link_count == 1 && entry.stage.current.is_missing(),
    }
}

fn entry_is_rolled_back(entry: &JournalEntryV2) -> bool {
    match (&entry.action, &entry.preimage, &entry.current_target) {
        (EntryActionV2::Create, PreimageV2::Absent, PresenceV2::Missing) => true,
        (
            EntryActionV2::Replace,
            PreimageV2::Regular { exact: preimage },
            PresenceV2::Present(target),
        ) if target == preimage => true,
        (
            EntryActionV2::Replace,
            PreimageV2::Regular { exact: preimage },
            PresenceV2::Present(target),
        ) => entry.backup.as_ref().is_some_and(|backup| {
            backup.prepared.as_ref().is_some_and(|prepared| {
                backup.current.is_missing()
                    && target == prepared
                    && target.state == preimage.state
                    && target.link_count == 1
            })
        }),
        _ => false,
    }
}

fn entry_is_preimage_or_planned(entry: &JournalEntryV2) -> bool {
    entry_is_rolled_back(entry) || entry_is_desired(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TX_TEXT: &str = "0123456789abcdef0123456789abcdef";

    fn mode(value: u32) -> Option<u32> {
        private_posix_mode(value)
    }

    fn exact_directory(
        namespace: u128,
        object: u128,
        permissions: u32,
        links: u64,
    ) -> ExactDirectoryStateV2 {
        ExactDirectoryStateV2::new(
            ObjectIdentityV2::from_u128(namespace, object),
            DirectoryModeV2::new(false, mode(permissions)).unwrap(),
            links,
        )
        .unwrap()
    }

    fn exact_file(
        namespace: u128,
        object: u128,
        bytes: &[u8],
        permissions: u32,
        links: u64,
    ) -> ExactFileStateV2 {
        ExactFileStateV2::new(
            ObjectIdentityV2::from_u128(namespace, object),
            FileStateV2::new(
                content_hash(bytes),
                bytes.len() as u64,
                false,
                mode(permissions),
            )
            .unwrap(),
            links,
        )
        .unwrap()
    }

    fn build_snapshot(with_created_directory: bool) -> JournalSnapshotV2 {
        let transaction_id = TransactionId::parse(TX_TEXT).unwrap();
        let root_hash = canonical_root_hash(b"/canonical/project");
        let root = exact_directory(1, 1, 0o755, 20);
        let write_lock = exact_file(1, 2, b"lock-v2\n", 0o600, 1);
        let workspace_parent_preimage = exact_directory(1, 3, 0o755, 10);
        let workspace_parent_after = exact_directory(1, 3, 0o755, 11);
        let workspace = exact_directory(1, 4, 0o700, 2);
        let bootstrap_intent_envelope = WorkspaceBootstrapIntentEnvelopeV2::new(
            transaction_id.clone(),
            root_hash.clone(),
            workspace_parent_preimage.clone(),
        )
        .unwrap();
        let bootstrap_intent_bytes = bootstrap_intent_envelope.to_json_bytes().unwrap();
        let bootstrap_intent = WorkspaceBootstrapIntentBindingV2::new(
            bootstrap_intent_envelope,
            exact_file(1, 5, &bootstrap_intent_bytes, 0o600, 1),
        )
        .unwrap();
        let project = ProjectBindingV2::new(
            &transaction_id,
            root_hash,
            root,
            write_lock,
            workspace_parent_preimage,
            workspace_parent_after.clone(),
            workspace,
        )
        .unwrap();
        let bootstrap_envelope =
            WorkspaceBootstrapEnvelopeV2::for_project(&transaction_id, &project);
        let bootstrap_bytes = bootstrap_envelope.to_json_bytes().unwrap();
        let bootstrap = WorkspaceBootstrapBindingV2::new(
            &transaction_id,
            &project,
            bootstrap_intent,
            exact_file(1, 6, &bootstrap_bytes, 0o600, 1),
        )
        .unwrap();
        let logical_path = if with_created_directory {
            "src/components/ui/_kit/generated/theme.css"
        } else {
            "src/components/ui/_kit/theme.css"
        };
        let desired = b":root { --accent: blue; }\n";
        let entry = JournalEntryV2::new(
            &transaction_id,
            ArtifactOrdinal::new(0).unwrap(),
            logical_path,
            EntryActionV2::Create,
            EntryRoleV2::Ordinary,
            PreimageV2::Absent,
            PlannedFileStateV2::new(
                content_hash(desired),
                desired.len() as u64,
                FileModePolicyV2::NormalCreateResolveOnStage,
            )
            .unwrap(),
        )
        .unwrap();
        let paths = logical_parents(logical_path);
        assert!(
            paths
                .iter()
                .any(|path| path.as_str() == WORKSPACE_PARENT_LOGICAL_PATH),
            "test path cohort omitted _kit: {paths:?}"
        );
        let mut directories = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let ordinal = ordinal_from_index(index).unwrap();
            let directory = if path.as_str() == WORKSPACE_PARENT_LOGICAL_PATH {
                JournalDirectoryV2::existing(ordinal, path, workspace_parent_after.clone()).unwrap()
            } else if with_created_directory && path.ends_with("/generated") {
                JournalDirectoryV2::create(
                    &transaction_id,
                    ordinal,
                    path,
                    DirectoryModeV2::new(false, mode(0o755)).unwrap(),
                )
                .unwrap()
            } else {
                JournalDirectoryV2::existing(
                    ordinal,
                    path,
                    exact_directory(2, index as u128 + 1, 0o755, 8),
                )
                .unwrap()
            };
            directories.push(directory);
        }
        JournalSnapshotV2::new(
            transaction_id,
            JournalOperationV2::AtomicWrite,
            project,
            bootstrap,
            vec![entry],
            directories,
        )
        .unwrap()
    }

    fn record(snapshot: &JournalSnapshotV2, object: u128) -> RecordBindingV2 {
        snapshot
            .expected_record_binding(ObjectIdentityV2::from_u128(9, object))
            .unwrap()
    }

    #[test]
    fn names_and_wide_identities_are_canonical() {
        let transaction_id = TransactionId::parse(TX_TEXT).unwrap();
        assert_eq!(
            parse_transaction_directory_name(&transaction_directory_name(&transaction_id)).unwrap(),
            transaction_id
        );
        assert_eq!(
            parse_bootstrap_owner_name(&bootstrap_owner_name(&transaction_id)).unwrap(),
            transaction_id
        );
        let parsed =
            parse_journal_file_name(&journal_partial_name(&transaction_id, u64::MAX)).unwrap();
        assert_eq!(parsed.sequence(), u64::MAX);
        assert_eq!(parsed.kind(), JournalFileKindV2::Partial);
        assert!(parse_journal_file_name("transaction-v2-bad-1.json").is_err());

        let identity = ObjectIdentityV2::from_u128(u128::MAX - 1, u128::MAX);
        let bytes = serde_json::to_vec(&identity).unwrap();
        assert_eq!(
            serde_json::from_slice::<ObjectIdentityV2>(&bytes).unwrap(),
            identity
        );
    }

    #[test]
    fn record_and_partial_use_one_exact_canonical_envelope() {
        let snapshot = build_snapshot(false);
        let envelope = snapshot.record_envelope_bytes().unwrap();
        assert_eq!(
            JournalSnapshotV2::from_record_envelope_slice(&envelope).unwrap(),
            snapshot
        );
        let (header, payload) = PartialEnvelopeHeaderV2::parse_prefix(&envelope).unwrap();
        assert_eq!(payload, snapshot.to_json_bytes().unwrap());
        let partial = PartialRecordBindingV2::new(
            &snapshot,
            exact_file(8, 1, &envelope, 0o600, 1),
            header,
            &envelope,
        )
        .unwrap();
        assert_eq!(
            partial.completed_record_binding(&snapshot).unwrap(),
            snapshot
                .expected_record_binding(ObjectIdentityV2::from_u128(8, 1))
                .unwrap()
        );

        let (header, payload) = PartialEnvelopeHeaderV2::parse_prefix(&envelope).unwrap();
        let mut prefix = header.to_prefix_bytes().unwrap();
        prefix.extend_from_slice(&payload[..payload.len() / 2]);
        let partial = PartialRecordBindingV2::new(
            &snapshot,
            exact_file(8, 2, &prefix, 0o600, 1),
            header,
            &prefix,
        )
        .unwrap();
        assert!(partial.completed_record_binding(&snapshot).is_err());

        let mut noncanonical = envelope.clone();
        noncanonical.insert(0, b' ');
        assert!(JournalSnapshotV2::from_record_envelope_slice(&noncanonical).is_err());
    }

    #[test]
    fn exact_create_commit_cleanup_and_finalization_chain_is_closed() {
        let mut snapshot = build_snapshot(false);
        let mut records = Vec::new();
        let desired = b":root { --accent: blue; }\n";
        let staged = exact_file(7, 1, desired, 0o644, 1);

        let binding = record(&snapshot, 100);
        records.push(binding.clone());
        snapshot = snapshot
            .adopt_next_preparation(
                binding,
                PreparationObservationV2::Stage {
                    exact: staged.clone(),
                },
            )
            .unwrap();
        let binding = record(&snapshot, 101);
        records.push(binding.clone());
        snapshot = snapshot.mark_prepared(binding).unwrap();
        let binding = record(&snapshot, 102);
        records.push(binding.clone());
        let linked = staged.with_link_count(2).unwrap();
        snapshot = snapshot
            .record_replacement_completion(
                binding,
                ReplacementObservationV2::new(linked.clone(), PresenceV2::Present(linked.clone())),
            )
            .unwrap();
        let binding = record(&snapshot, 103);
        records.push(binding.clone());
        snapshot = snapshot.enter_commit_complete(binding).unwrap();
        assert!(snapshot.phase().desired_state_is_irreversible());
        let binding = record(&snapshot, 104);
        records.push(binding.clone());
        snapshot = snapshot
            .arm_cleanup(
                binding,
                CleanupIntentV2::RemoveFile {
                    target: CleanupTargetV2::Stage {
                        ordinal: ArtifactOrdinal::new(0).unwrap(),
                    },
                    expected: linked,
                },
            )
            .unwrap();
        let binding = record(&snapshot, 105);
        records.push(binding.clone());
        snapshot = snapshot.complete_cleanup(binding, None).unwrap();
        assert!(snapshot.ready_for_finalization());
        records.push(record(&snapshot, 106));

        let lease = FinalizationLeaseV2::arm(&snapshot, records, None).unwrap();
        let parent_after = exact_directory(1, 3, 0o755, 10);
        let removed = lease.mark_workspace_removed(parent_after).unwrap();
        lease.validate_successor(&removed).unwrap();
        assert_eq!(removed.state(), FinalizationStateV2::WorkspaceRemoved);
        assert_eq!(
            FinalizationLeaseV2::from_json_slice(&removed.to_json_bytes().unwrap()).unwrap(),
            removed
        );
    }

    #[test]
    fn directory_candidate_publish_world_and_rollback_cleanup_are_exact() {
        let mut snapshot = build_snapshot(true);
        let directory_ordinal = ArtifactOrdinal::new(4).unwrap();
        let candidate = exact_directory(7, 20, 0o700, 2);
        let parent_after_candidate = exact_directory(1, 3, 0o755, 12);

        snapshot = snapshot
            .adopt_next_preparation(
                record(&snapshot, 200),
                PreparationObservationV2::DirectoryCandidate {
                    exact: candidate.clone(),
                    parent_after: parent_after_candidate.clone(),
                },
            )
            .unwrap();
        let intent = DirectoryPublishIntentV2::new(
            directory_ordinal,
            directory_candidate_name(snapshot.transaction_id(), directory_ordinal),
            candidate.clone(),
            DirectoryParentV2::Cohort {
                ordinal: ArtifactOrdinal::new(3).unwrap(),
            },
            parent_after_candidate.clone(),
        );
        assert_eq!(
            snapshot
                .validate_directory_publication_world(
                    &intent,
                    &PresenceV2::Present(candidate.clone()),
                    &PresenceV2::Missing,
                    &parent_after_candidate,
                )
                .unwrap(),
            DirectoryPublicationWorldV2::CandidatePrivate
        );
        let ready = exact_directory(7, 20, 0o755, 2);
        assert_eq!(
            snapshot
                .validate_directory_publication_world(
                    &intent,
                    &PresenceV2::Present(ready.clone()),
                    &PresenceV2::Missing,
                    &parent_after_candidate,
                )
                .unwrap(),
            DirectoryPublicationWorldV2::CandidateReady
        );
        assert!(
            snapshot
                .validate_directory_publication_world(
                    &intent,
                    &PresenceV2::Missing,
                    &PresenceV2::Present(exact_directory(7, 21, 0o755, 2)),
                    &parent_after_candidate,
                )
                .is_err()
        );

        snapshot = snapshot
            .arm_directory_publication(record(&snapshot, 201), intent)
            .unwrap();
        snapshot = snapshot
            .complete_directory_publication(
                record(&snapshot, 202),
                DirectoryPublicationObservationV2::new(
                    ready.clone(),
                    PresenceV2::Missing,
                    parent_after_candidate.clone(),
                ),
            )
            .unwrap();
        let desired = b":root { --accent: blue; }\n";
        let staged = exact_file(7, 30, desired, 0o640, 1);
        snapshot = snapshot
            .adopt_next_preparation(
                record(&snapshot, 203),
                PreparationObservationV2::Stage {
                    exact: staged.clone(),
                },
            )
            .unwrap();
        snapshot = snapshot.mark_prepared(record(&snapshot, 204)).unwrap();
        snapshot = snapshot.begin_rollback(record(&snapshot, 205)).unwrap();
        snapshot = snapshot
            .advance_rollback_noop(record(&snapshot, 206))
            .unwrap();
        snapshot = snapshot
            .finish_rollback_targets(record(&snapshot, 207))
            .unwrap();
        snapshot = snapshot
            .arm_cleanup(
                record(&snapshot, 208),
                CleanupIntentV2::RemoveFile {
                    target: CleanupTargetV2::Stage {
                        ordinal: ArtifactOrdinal::new(0).unwrap(),
                    },
                    expected: staged,
                },
            )
            .unwrap();
        snapshot = snapshot
            .complete_cleanup(record(&snapshot, 209), None)
            .unwrap();
        snapshot = snapshot
            .advance_cleanup_noop(record(&snapshot, 210))
            .unwrap();
        snapshot = snapshot
            .arm_cleanup(
                record(&snapshot, 211),
                CleanupIntentV2::RemoveDirectory {
                    target: CleanupTargetV2::CreatedDirectory {
                        ordinal: directory_ordinal,
                    },
                    expected: ready,
                    parent: DirectoryParentV2::Cohort {
                        ordinal: ArtifactOrdinal::new(3).unwrap(),
                    },
                    parent_before: parent_after_candidate,
                },
            )
            .unwrap();
        snapshot = snapshot
            .complete_cleanup(
                record(&snapshot, 212),
                Some(exact_directory(1, 3, 0o755, 11)),
            )
            .unwrap();
        assert!(snapshot.ready_for_finalization());
    }

    #[test]
    fn mode_policy_roles_and_strict_deserialization_reject_footguns() {
        let transaction_id = TransactionId::parse(TX_TEXT).unwrap();
        let desired = b"desired";
        let invalid_replace = JournalEntryV2::new(
            &transaction_id,
            ArtifactOrdinal::new(0).unwrap(),
            "src/components/ui/_kit/theme.css",
            EntryActionV2::Replace,
            EntryRoleV2::Ordinary,
            PreimageV2::regular(exact_file(3, 1, b"old", 0o644, 1)),
            PlannedFileStateV2::new(
                content_hash(desired),
                desired.len() as u64,
                FileModePolicyV2::NormalCreateResolveOnStage,
            )
            .unwrap(),
        );
        assert!(invalid_replace.is_err());

        let snapshot = build_snapshot(false);
        let mut value: serde_json::Value =
            serde_json::from_slice(&snapshot.to_json_bytes().unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_owned(), serde_json::Value::Bool(true));
        assert!(JournalSnapshotV2::from_json_slice(&serde_json::to_vec(&value).unwrap()).is_err());

        #[cfg(unix)]
        assert!(FileStateV2::new(content_hash(b"x"), 1, false, None).is_err());
    }
}
