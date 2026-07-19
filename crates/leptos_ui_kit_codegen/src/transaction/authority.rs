use std::path::Path;

use cap_std::fs::Dir;

use crate::path_safety::PlanningContext;
use crate::{CodegenError, PreservedFileMode};

use super::fs::{ExactObjectIdentity, FsOps};
use super::journal::ExactDirectoryStateV2;
use super::lock::WriteLock;

/// Re-establishes transaction authority immediately before a filesystem
/// mutation. Callers must use the returned directory capability directly and
/// must not cache it across a later mutation boundary.
pub(super) struct TransactionAuthority<'a> {
    context: &'a PlanningContext,
    lock: &'a WriteLock,
}

impl<'a> TransactionAuthority<'a> {
    pub(super) const fn new(context: &'a PlanningContext, lock: &'a WriteLock) -> Self {
        Self { context, lock }
    }

    pub(super) fn validate_lock(&self) -> Result<(), CodegenError> {
        self.lock.validate_context(self.context)
    }

    pub(super) fn rebind_parent_for_mutation(
        &self,
        fs: &dyn FsOps,
        logical_parent: &str,
        expected_parent: &ExactDirectoryStateV2,
        mutation_path: &Path,
    ) -> Result<Dir, CodegenError> {
        fs.before_mutation_rebind(mutation_path).map_err(|source| {
            CodegenError::FilesystemOperation {
                operation: "rebind mutation authority",
                logical_path: if logical_parent.is_empty() {
                    ".".to_owned()
                } else {
                    logical_parent.to_owned()
                },
                path: mutation_path.to_path_buf(),
                source,
            }
        })?;
        self.validate_lock()?;

        let identity = expected_parent.identity();
        let mode = expected_parent.mode();
        let directory = self.context.reopen_exact_directory(
            logical_parent,
            ExactObjectIdentity::from_parts(identity.namespace_bytes(), identity.object_bytes()),
            PreservedFileMode {
                readonly: mode.readonly(),
                posix_mode: mode.posix_mode(),
            },
        )?;

        self.validate_lock()?;
        Ok(directory)
    }
}
