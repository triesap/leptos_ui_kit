use std::path::Path;
#[cfg(feature = "test-support")]
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use leptos_ui_kit_registry::{canonical_kit_json, kit_config_for_write};

use crate::path_safety::PlanningContext;
use crate::planning::{plan_add_with_context, plan_init_with_context, plan_sync_with_context};
use crate::transaction::{
    JournalOperationV2, WriteLock, apply_planned_files_locked, recover_pending_locked,
};
#[cfg(feature = "test-support")]
use crate::transaction::{
    SystemEntropy, SystemFs, TransactionRuntime, TransitionKey, TransitionObserver,
    apply_exact_transaction, recover_pending_locked_with_runtime,
};
use crate::{AddPlan, CodegenError, InitPlan, SyncPlan};

pub fn apply_init(project_root: &Path) -> Result<InitPlan, CodegenError> {
    let recovery_context = PlanningContext::open(project_root)?;
    let lock = WriteLock::acquire_with_context(&recovery_context)?;
    recover_pending_locked(&recovery_context, &lock)?;
    let context = PlanningContext::open(project_root)?;
    lock.validate_context(&context)?;
    let plan = plan_init_with_context(&context, project_root, canonical_kit_json)?;
    apply_planned_files_locked(
        &context,
        &lock,
        &plan.files,
        &plan.changes,
        &plan.snapshot,
        JournalOperationV2::Init,
    )?;

    Ok(plan)
}

/// Test-support entrypoint that blocks at one explicitly selected semantic
/// transition so an observer process can terminate this process without a
/// journal-polling race.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn apply_init_with_transition_barrier(
    project_root: &Path,
    ready_path: &Path,
    release_path: &Path,
    selector: &str,
    occurrence: usize,
) -> Result<InitPlan, CodegenError> {
    assert!(occurrence > 0, "transition occurrence is one-based");
    let observer = Arc::new(ProcessTransitionBarrier {
        ready_path: ready_path.to_path_buf(),
        release_path: release_path.to_path_buf(),
        selector: selector.to_owned(),
        occurrence,
        counts: Mutex::new(BTreeMap::new()),
    });
    let runtime = TransactionRuntime::new(Arc::new(SystemFs), Arc::new(SystemEntropy), observer);
    let recovery_context = PlanningContext::open(project_root)?;
    let lock = WriteLock::acquire_with_context(&recovery_context)?;
    recover_pending_locked_with_runtime(&recovery_context, &lock, &runtime)?;
    let context = PlanningContext::open(project_root)?;
    lock.validate_context(&context)?;
    let plan = plan_init_with_context(&context, project_root, canonical_kit_json)?;
    apply_exact_transaction(
        &context,
        &lock,
        &plan.files,
        &plan.changes,
        &plan.snapshot,
        runtime,
        JournalOperationV2::Init,
    )?;
    Ok(plan)
}

#[cfg(feature = "test-support")]
#[derive(Debug)]
struct ProcessTransitionBarrier {
    ready_path: PathBuf,
    release_path: PathBuf,
    selector: String,
    occurrence: usize,
    counts: Mutex<BTreeMap<String, usize>>,
}

#[cfg(feature = "test-support")]
impl TransitionObserver for ProcessTransitionBarrier {
    fn observe(&self, key: TransitionKey) {
        let debug = format!("{key:?}");
        let family = debug
            .split_once(" {")
            .map_or(debug.as_str(), |(family, _)| family);
        let window = if debug.contains("window: Before") {
            "Before"
        } else if debug.contains("window: After") {
            "After"
        } else {
            panic!("semantic transition lacks an explicit window: {debug}");
        };
        let selector = format!("{family}:{window}");
        if selector != self.selector {
            return;
        }
        let observed = {
            let mut counts = self.counts.lock().expect("transition barrier count lock");
            let count = counts.entry(selector).or_default();
            *count += 1;
            *count
        };
        if observed != self.occurrence {
            return;
        }

        let mut ready = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&self.ready_path)
            .unwrap_or_else(|error| {
                panic!(
                    "create transition barrier {}: {error}",
                    self.ready_path.display()
                )
            });
        ready.write_all(debug.as_bytes()).unwrap_or_else(|error| {
            panic!(
                "write transition barrier {}: {error}",
                self.ready_path.display()
            )
        });
        ready.sync_all().unwrap_or_else(|error| {
            panic!(
                "sync transition barrier {}: {error}",
                self.ready_path.display()
            )
        });

        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.release_path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for transition release {}",
                self.release_path.display()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

pub fn apply_add(project_root: &Path, item_name: &str) -> Result<AddPlan, CodegenError> {
    let recovery_context = PlanningContext::open(project_root)?;
    let lock = WriteLock::acquire_with_context(&recovery_context)?;
    recover_pending_locked(&recovery_context, &lock)?;
    let context = PlanningContext::open(project_root)?;
    lock.validate_context(&context)?;
    let plan = plan_add_with_context(&context, project_root, item_name, kit_config_for_write)?;
    apply_planned_files_locked(
        &context,
        &lock,
        &plan.files,
        &plan.changes,
        &plan.snapshot,
        JournalOperationV2::Add,
    )?;

    Ok(plan)
}

pub fn apply_sync(project_root: &Path) -> Result<SyncPlan, CodegenError> {
    let recovery_context = PlanningContext::open(project_root)?;
    let lock = WriteLock::acquire_with_context(&recovery_context)?;
    recover_pending_locked(&recovery_context, &lock)?;
    let context = PlanningContext::open(project_root)?;
    lock.validate_context(&context)?;
    let plan = plan_sync_with_context(&context, project_root, kit_config_for_write)?;
    apply_planned_files_locked(
        &context,
        &lock,
        &plan.files,
        &plan.changes,
        &plan.snapshot,
        JournalOperationV2::Sync,
    )?;

    Ok(plan)
}
