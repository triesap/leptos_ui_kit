use std::{fs, io, path::Path};

use crate::CodegenError;

/// Reports legacy journal-v1 evidence without ever interpreting or mutating it.
///
/// Journal v1 did not bind every mutation to exact object identities. Recovering
/// one of those journals would therefore reintroduce the unsafe protocol that
/// journal v2 replaced. The only supported behavior is to preserve the evidence
/// and require explicit operator remediation.
pub(super) fn check_pending_recovery_v1(project_root: &Path) -> Result<(), CodegenError> {
    let directory = project_root.join("src/components/ui/_kit/.transactions");
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(CodegenError::Io {
                path: directory,
                source,
            });
        }
    };
    if !metadata.file_type().is_dir() {
        return Err(CodegenError::RecoveryRequired {
            journal_path: directory,
            reason: "legacy journal-v1 namespace is not a no-follow directory; no filesystem mutation was attempted"
                .to_owned(),
        });
    }

    let mut entries = fs::read_dir(&directory).map_err(|source| CodegenError::Io {
        path: directory.clone(),
        source,
    })?;
    let Some(entry) = entries.next() else {
        // An empty legacy directory carries no transaction authority. Leaving it
        // in place is intentional: this diagnostic path is strictly read-only.
        return Ok(());
    };
    let entry = entry.map_err(|source| CodegenError::Io {
        path: directory.clone(),
        source,
    })?;
    let evidence = entry.path();
    Err(CodegenError::RecoveryRequired {
        journal_path: evidence,
        reason: "legacy journal-v1 evidence cannot be recovered safely by the exact journal-v2 engine; preserve the namespace for explicit operator remediation"
            .to_owned(),
    })
}

#[cfg(test)]
pub(crate) use test_seam::{apply_planned_files_with, apply_planned_files_with_snapshot};

#[cfg(test)]
mod test_seam {
    use std::{path::Path, sync::Arc};

    use crate::path_safety::{PlanningContext, capture_plan_snapshot};
    use crate::{ChangeKind, ChangeRecord, CodegenError, PlanSnapshot, PlannedFile};

    use super::super::{
        engine::apply_exact_transaction,
        fs::FsOps,
        journal::JournalOperationV2,
        lock::WriteLock,
        recovery::recover_pending_locked,
        runtime::{NoopTransitionObserver, SystemEntropy, TransactionRuntime},
    };

    pub(crate) fn apply_planned_files_with(
        project_root: &Path,
        files: &[PlannedFile],
        changes: &[ChangeRecord],
        fs: Arc<dyn FsOps>,
    ) -> Result<(), CodegenError> {
        let snapshot = capture_plan_snapshot(project_root, files.iter().map(|file| &file.path))?;
        apply_planned_files_with_snapshot(project_root, files, changes, &snapshot, fs)
    }

    pub(crate) fn apply_planned_files_with_snapshot(
        project_root: &Path,
        files: &[PlannedFile],
        changes: &[ChangeRecord],
        snapshot: &PlanSnapshot,
        fs: Arc<dyn FsOps>,
    ) -> Result<(), CodegenError> {
        let context = PlanningContext::open(project_root)?;
        let lock = WriteLock::acquire_with_context_and_fs(&context, Arc::clone(&fs))?;
        lock.validate_context(&context)?;
        recover_pending_locked(&context, &lock)?;
        let runtime = TransactionRuntime::new(
            fs,
            Arc::new(SystemEntropy),
            Arc::new(NoopTransitionObserver),
        );
        apply_exact_transaction(
            &context,
            &lock,
            files,
            changes,
            snapshot,
            runtime,
            operation_for_test(files, changes),
        )
    }

    fn operation_for_test(files: &[PlannedFile], changes: &[ChangeRecord]) -> JournalOperationV2 {
        if files.is_empty()
            || changes
                .iter()
                .any(|change| change.kind == ChangeKind::WriteLockFile)
        {
            JournalOperationV2::Sync
        } else {
            JournalOperationV2::AtomicWrite
        }
    }
}
