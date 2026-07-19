#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

use cap_std::{ambient_authority, fs::Dir};
use leptos_ui_kit_codegen::{
    CodegenError, DEFAULT_KIT_WRITE_LOCK_PATH, WriteLock, apply_add, apply_init, apply_sync,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const WORKER_ROLE_ENV: &str = "LEPTOS_UI_KIT_TRANSACTION_PROCESS_ROLE";
const WORKER_PROJECT_ENV: &str = "LEPTOS_UI_KIT_TRANSACTION_PROCESS_PROJECT";
const WORKER_CONTROL_ENV: &str = "LEPTOS_UI_KIT_TRANSACTION_PROCESS_CONTROL";
const WORKER_ID_ENV: &str = "LEPTOS_UI_KIT_TRANSACTION_PROCESS_ID";
const BARRIER_TIMEOUT: Duration = Duration::from_secs(20);
const BARRIER_POLL_INTERVAL: Duration = Duration::from_millis(10);
const ADVISORY_LOCK_MARKER: &[u8] = b"leptos-ui-kit advisory lock v1\n";
const KIT_GITIGNORE: &[u8] = b"/.write.lock\n/.transactions/\n";
const INDEX_HTML: &[u8] = b"<html><head></head><body></body></html>\n";

#[test]
fn held_lock_wins_before_invalid_config_is_parsed() {
    let sandbox = tempdir().expect("process-test sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    setup_project(&project);
    fs::create_dir_all(project.join("src/components/ui/_kit"))
        .expect("create installer state directory");
    let config_path = project.join("src/components/ui/_kit/kit.json");
    let invalid_config = b"{\"tailwind\":true}\n";
    fs::write(&config_path, invalid_config).expect("write invalid config");
    fs::create_dir(&control).expect("create process control directory");

    let mut holder = spawn_worker("hold", &project, &control, "holder");
    wait_for_path(&barrier_path(&control, "ready", "holder"));
    let before_contender = project_tree(&project);

    let error = apply_init(&project).expect_err("held lock must reject the writer");
    match error {
        CodegenError::WriteLockContended { path } => {
            assert_eq!(path, DEFAULT_KIT_WRITE_LOCK_PATH);
        }
        other => panic!("expected write-lock contention before config parsing, got {other}"),
    }
    assert_eq!(project_tree(&project), before_contender);

    signal(&barrier_path(&control, "release", "holder"));
    holder.wait_success();
}

#[test]
fn killed_holder_releases_lock_without_replacing_the_persistent_inode() {
    let sandbox = tempdir().expect("process-test sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    setup_project(&project);
    fs::create_dir(&control).expect("create process control directory");

    let lock_path = project.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    let initial = WriteLock::acquire(&project).expect("bootstrap persistent lock");
    drop(initial);
    let bytes_before = fs::read(&lock_path).expect("read persistent lock before holder");
    let identity_before = file_identity(&lock_path);

    let mut holder = spawn_worker("hold", &project, &control, "killed-holder");
    wait_for_path(&barrier_path(&control, "ready", "killed-holder"));

    holder.kill_and_wait();

    assert_eq!(
        fs::read(&lock_path).expect("read persistent lock after kill"),
        bytes_before
    );
    assert_eq!(file_identity(&lock_path), identity_before);
    assert_exact_first_use_coordination(&project);

    let lock = WriteLock::acquire(&project).expect("reacquire after abrupt process exit");
    drop(lock);

    assert_eq!(
        fs::read(&lock_path).expect("read persistent lock after reacquisition"),
        bytes_before
    );
    assert_eq!(file_identity(&lock_path), identity_before);
}

#[test]
fn concurrent_first_use_has_one_holder_and_one_fast_contender() {
    let sandbox = tempdir().expect("process-test sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    setup_project(&project);
    fs::create_dir(&control).expect("create process control directory");

    let mut first = spawn_worker("race", &project, &control, "first");
    let mut second = spawn_worker("race", &project, &control, "second");
    wait_for_path(&barrier_path(&control, "ready", "first"));
    wait_for_path(&barrier_path(&control, "ready", "second"));

    signal(&control.join("start"));
    wait_for_worker_outcomes(&control, &mut first, &mut second);

    let acquired = ["first", "second"]
        .into_iter()
        .filter(|id| barrier_path(&control, "acquired", id).exists())
        .collect::<Vec<_>>();
    let contended = ["first", "second"]
        .into_iter()
        .filter(|id| barrier_path(&control, "contended", id).exists())
        .collect::<Vec<_>>();
    assert_eq!(acquired.len(), 1, "first-use holders: {acquired:?}");
    assert_eq!(contended.len(), 1, "first-use contenders: {contended:?}");

    signal(&control.join("release"));
    first.wait_success();
    second.wait_success();

    let lock_path = project.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    let bytes_after_race = fs::read(&lock_path).expect("read persistent lock after first-use race");
    let identity_after_race = file_identity(&lock_path);
    assert_exact_first_use_coordination(&project);

    let lock = WriteLock::acquire(&project).expect("reuse raced persistent lock inode");
    drop(lock);
    assert_eq!(
        fs::read(&lock_path).expect("read persistent lock after reuse"),
        bytes_after_race
    );
    assert_eq!(file_identity(&lock_path), identity_after_race);
}

#[test]
fn legacy_sentinel_is_rejected_without_changing_its_bytes_or_identity() {
    let sandbox = tempdir().expect("process-test sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    setup_project(&project);
    fs::create_dir(&control).expect("create process control directory");
    let lock_path = project.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    fs::create_dir_all(lock_path.parent().expect("legacy lock parent"))
        .expect("create legacy lock parent");
    fs::write(&lock_path, b"locked\n").expect("write legacy sentinel");
    let identity_before = file_identity(&lock_path);

    let mut worker = spawn_worker("legacy", &project, &control, "legacy");
    wait_for_worker_signal(&barrier_path(&control, "validated", "legacy"), &mut worker);
    worker.wait_success();

    assert_eq!(
        fs::read(&lock_path).expect("read rejected legacy sentinel"),
        b"locked\n"
    );
    assert_eq!(file_identity(&lock_path), identity_before);
}

#[test]
fn unknown_coordination_lock_is_rejected_without_replacement() {
    let sandbox = tempdir().expect("process-test sandbox");
    let project = sandbox.path().join("project");
    let control = sandbox.path().join("control");
    setup_project(&project);
    fs::create_dir(&control).expect("create process control directory");
    let lock_path = project.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    fs::create_dir_all(lock_path.parent().expect("coordination lock parent"))
        .expect("create coordination lock parent");
    let unknown_bytes = b"unknown lock format\n";
    fs::write(&lock_path, unknown_bytes).expect("write unknown coordination lock");
    let identity_before = file_identity(&lock_path);

    let mut worker = spawn_worker("invalid", &project, &control, "invalid");
    wait_for_worker_signal(&barrier_path(&control, "validated", "invalid"), &mut worker);
    worker.wait_success();

    assert_eq!(
        fs::read(&lock_path).expect("read rejected coordination lock"),
        unknown_bytes
    );
    assert_eq!(file_identity(&lock_path), identity_before);
}

#[test]
fn killed_transaction_recovers_to_the_state_selected_by_its_durable_v2_phase() {
    for attempt in 1..=12 {
        let sandbox = tempdir().expect("process-test sandbox");
        let project = sandbox.path().join("project");
        let control = sandbox.path().join("control");
        setup_project(&project);
        fs::create_dir(&control).expect("create process control directory");
        let lock = WriteLock::acquire(&project).expect("bootstrap coordination");
        drop(lock);
        let before = project_tree(&project);

        let desired_sandbox = tempdir().expect("desired-state sandbox");
        let desired_project = desired_sandbox.path().join("project");
        setup_project(&desired_project);
        apply_init(&desired_project).expect("materialize exact desired state");
        let desired = project_tree(&desired_project);

        let mut worker = spawn_worker("apply-init", &project, &control, "crash-writer");
        let deadline = Instant::now() + BARRIER_TIMEOUT;
        let mut saw_published_record = false;
        while Instant::now() < deadline {
            if latest_durable_record(&project).is_some() {
                saw_published_record = true;
                worker.kill_and_wait();
                break;
            }
            if worker.try_status().is_some() {
                break;
            }
            thread::yield_now();
        }
        if !saw_published_record {
            worker.wait_success();
            continue;
        }

        let Some(killed_record) = latest_durable_record(&project) else {
            continue;
        };
        let (recovery_role, expected) = match killed_record.disposition {
            DurableDisposition::RollbackClass => ("recover-precommit", &before),
            DurableDisposition::FinishOnly => ("recover-postcommit", &desired),
        };

        let mut recovery = spawn_worker(recovery_role, &project, &control, "recovery");
        recovery.wait_success();
        assert_eq!(
            project_tree(&project),
            *expected,
            "fresh-process recovery disagreed with durable record sequence {} ({:?}) on attempt {attempt}",
            killed_record.sequence,
            killed_record.disposition,
        );
        assert!(
            transaction_workspace_paths(&project).is_empty(),
            "recovery must remove every journal-v2 transaction workspace"
        );
        assert!(
            !has_transaction_artifacts(&project),
            "recovery must remove every journal-v2 top-level authority"
        );

        let mut second = spawn_worker(recovery_role, &project, &control, "recovery-again");
        second.wait_success();
        assert_eq!(project_tree(&project), *expected);
        return;
    }
    panic!("could not kill a worker while a published journal-v2 record remained authoritative");
}

#[test]
fn transaction_process_worker() {
    let Some(role) = env::var_os(WORKER_ROLE_ENV) else {
        return;
    };
    let project = worker_path(WORKER_PROJECT_ENV);
    let control = worker_path(WORKER_CONTROL_ENV);
    let id = env::var(WORKER_ID_ENV).expect("worker id");

    match role.to_str().expect("UTF-8 worker role") {
        "hold" => worker_hold(&project, &control, &id),
        "race" => worker_race(&project, &control, &id),
        "legacy" => worker_legacy(&project, &control, &id),
        "invalid" => worker_invalid(&project, &control, &id),
        "apply-init" => {
            apply_init(&project).expect("worker applies init");
        }
        "recover-precommit" => {
            apply_add(&project, "__recovery_probe_missing_item__")
                .expect_err("the recovery probe must fail after completing recovery");
        }
        "recover-postcommit" => {
            apply_sync(&project).expect("finish-only recovery preserves the installed project");
        }
        other => panic!("unknown transaction-process worker role {other}"),
    }
}

fn worker_hold(project: &Path, control: &Path, id: &str) {
    let _lock = WriteLock::acquire(project).expect("worker acquires persistent lock");
    signal(&barrier_path(control, "ready", id));
    wait_for_path(&barrier_path(control, "release", id));
}

fn worker_race(project: &Path, control: &Path, id: &str) {
    signal(&barrier_path(control, "ready", id));
    wait_for_path(&control.join("start"));

    match WriteLock::acquire(project) {
        Ok(_lock) => {
            signal(&barrier_path(control, "acquired", id));
            wait_for_path(&control.join("release"));
        }
        Err(CodegenError::WriteLockContended { path }) => {
            assert_eq!(path, DEFAULT_KIT_WRITE_LOCK_PATH);
            signal(&barrier_path(control, "contended", id));
        }
        Err(other) => panic!("first-use worker expected contention, got {other}"),
    }
}

fn worker_legacy(project: &Path, control: &Path, id: &str) {
    match WriteLock::acquire(project) {
        Err(CodegenError::LegacyWriteLock { path }) => {
            assert_eq!(path, DEFAULT_KIT_WRITE_LOCK_PATH);
        }
        Err(other) => panic!("expected legacy write-lock diagnostic, got {other}"),
        Ok(_) => panic!("legacy sentinel was accepted as an advisory lock"),
    }
    signal(&barrier_path(control, "validated", id));
}

fn worker_invalid(project: &Path, control: &Path, id: &str) {
    match WriteLock::acquire(project) {
        Err(CodegenError::InvalidCoordinationState { path, reason }) => {
            assert_eq!(path, DEFAULT_KIT_WRITE_LOCK_PATH);
            assert!(!reason.is_empty(), "invalid coordination reason");
        }
        Err(other) => panic!("expected invalid coordination diagnostic, got {other}"),
        Ok(_) => panic!("unknown coordination lock was accepted"),
    }
    signal(&barrier_path(control, "validated", id));
}

fn spawn_worker(role: &str, project: &Path, control: &Path, id: &str) -> ChildGuard {
    let child = Command::new(env::current_exe().expect("current process-test executable"))
        .args([
            "--exact",
            "transaction_process_worker",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(WORKER_ROLE_ENV, role)
        .env(WORKER_PROJECT_ENV, project)
        .env(WORKER_CONTROL_ENV, control)
        .env(WORKER_ID_ENV, id)
        .spawn()
        .expect("spawn transaction-process worker");
    ChildGuard { child: Some(child) }
}

struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn wait_success(&mut self) {
        let mut child = self.child.take().expect("live child process");
        let status = child.wait().expect("wait for transaction-process worker");
        assert!(
            status.success(),
            "transaction-process worker failed: {status}"
        );
    }

    fn kill_and_wait(&mut self) {
        let mut child = self.child.take().expect("live child process");
        child.kill().expect("kill transaction-process worker");
        let _ = child
            .wait()
            .expect("reap killed transaction-process worker");
    }

    fn try_status(&mut self) -> Option<ExitStatus> {
        self.child
            .as_mut()
            .expect("live child process")
            .try_wait()
            .expect("poll transaction-process worker")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn worker_path(name: &str) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("missing worker path environment variable {name}"))
}

fn barrier_path(control: &Path, kind: &str, id: &str) -> PathBuf {
    control.join(format!("{kind}-{id}"))
}

fn signal(path: &Path) {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|error| panic!("create barrier {}: {error}", path.display()));
    file.write_all(b"ready\n")
        .unwrap_or_else(|error| panic!("write barrier {}: {error}", path.display()));
    file.flush()
        .unwrap_or_else(|error| panic!("flush barrier {}: {error}", path.display()));
}

fn wait_for_path(path: &Path) {
    wait_until(&format!("barrier {}", path.display()), || path.exists());
}

fn wait_for_worker_signal(path: &Path, worker: &mut ChildGuard) {
    wait_until(&format!("worker signal {}", path.display()), || {
        if path.exists() {
            return true;
        }
        if let Some(status) = worker.try_status() {
            panic!(
                "transaction-process worker exited before {}: {status}",
                path.display()
            );
        }
        false
    });
}

fn wait_for_worker_outcomes(control: &Path, first: &mut ChildGuard, second: &mut ChildGuard) {
    wait_until("both first-use worker outcomes", || {
        let mut complete = true;
        for (id, worker) in [("first", &mut *first), ("second", &mut *second)] {
            let outcome_count = ["acquired", "contended"]
                .into_iter()
                .filter(|kind| barrier_path(control, kind, id).exists())
                .count();
            assert!(outcome_count <= 1, "worker {id} published two outcomes");
            if outcome_count == 0 {
                complete = false;
                if let Some(status) = worker.try_status() {
                    panic!("first-use worker {id} exited before an outcome: {status}");
                }
            }
        }
        complete
    });
}

fn wait_until(label: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + BARRIER_TIMEOUT;
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::sleep(BARRIER_POLL_INTERVAL);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

fn file_identity(path: &Path) -> FileIdentity {
    let parent_path = path.parent().expect("file identity parent");
    let name = path.file_name().expect("file identity name");
    let parent = Dir::open_ambient_dir(parent_path, ambient_authority())
        .unwrap_or_else(|error| panic!("open identity parent {}: {error}", parent_path.display()));
    let metadata = parent
        .symlink_metadata(name)
        .unwrap_or_else(|error| panic!("inspect identity {}: {error}", path.display()));
    FileIdentity {
        device: cap_fs_ext::MetadataExt::dev(&metadata),
        inode: cap_fs_ext::MetadataExt::ino(&metadata),
    }
}

fn setup_project(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("create project source directory");
    fs::write(root.join("index.html"), INDEX_HTML).expect("write project index");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableDisposition {
    RollbackClass,
    FinishOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DurableRecord {
    sequence: u64,
    disposition: DurableDisposition,
}

fn latest_durable_record(project: &Path) -> Option<DurableRecord> {
    transaction_workspace_paths(project)
        .into_iter()
        .flat_map(|workspace| {
            let workspace_id = workspace
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("transaction-v2-"))
                .filter(|id| is_lower_hex(id, 32))?
                .to_owned();
            let entries = fs::read_dir(workspace).ok()?;
            Some(entries.filter_map(move |entry| {
                durable_record_from_entry(entry.ok()?.path(), &workspace_id)
            }))
        })
        .flatten()
        .max_by_key(|record| record.sequence)
}

fn durable_record_from_entry(path: PathBuf, workspace_id: &str) -> Option<DurableRecord> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".json")?;
    let tail = stem.strip_prefix("transaction-v2-")?;
    let (transaction_id, sequence_text) = tail.rsplit_once('-')?;
    if transaction_id != workspace_id
        || !is_lower_hex(transaction_id, 32)
        || sequence_text.len() != 20
        || !sequence_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let sequence = sequence_text.parse::<u64>().ok()?;
    let bytes = fs::read(&path).ok()?;
    let newline = bytes.iter().position(|byte| *byte == b'\n')?;
    let header: serde_json::Value = serde_json::from_slice(&bytes[..newline]).ok()?;
    let payload = &bytes[newline + 1..];
    let payload_len = u64::try_from(payload.len()).ok()?;
    let payload_hash = format!("sha256:{:x}", Sha256::digest(payload));
    if header.get("magic")?.as_str()? != "leptos-ui-kit-journal-partial-v2"
        || header.get("version")?.as_u64()? != 2
        || header.get("transactionId")?.as_str()? != transaction_id
        || header.get("sequence")?.as_u64()? != sequence
        || header.get("payloadLen")?.as_u64()? != payload_len
        || header.get("payloadHash")?.as_str()? != payload_hash
    {
        return None;
    }
    let snapshot: serde_json::Value = serde_json::from_slice(payload).ok()?;
    if snapshot.get("version")?.as_u64()? != 2
        || snapshot.get("transactionId")?.as_str()? != transaction_id
        || snapshot.get("sequence")?.as_u64()? != sequence
    {
        return None;
    }
    let disposition = match snapshot.get("phase")?.get("kind")?.as_str()? {
        "commitComplete" => DurableDisposition::FinishOnly,
        "preparing" | "prepared" | "replacing" | "rollingBack" | "rollbackComplete" => {
            DurableDisposition::RollbackClass
        }
        _ => return None,
    };
    Some(DurableRecord {
        sequence,
        disposition,
    })
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn transaction_workspace_paths(project: &Path) -> Vec<PathBuf> {
    let kit = project.join("src/components/ui/_kit/.transactions");
    let Ok(entries) = fs::read_dir(kit) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("transaction-v2-"))
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .map(|entry| entry.path())
        .collect()
}

fn has_transaction_artifacts(project: &Path) -> bool {
    let kit = project.join("src/components/ui/_kit/.transactions");
    fs::read_dir(kit)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .any(|name| {
            name.starts_with("transaction-v2-")
                || name.starts_with("bootstrap-intent-v2-")
                || name.starts_with("finalization-v2-")
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TreeEntry {
    Directory,
    File(Vec<u8>),
}

fn project_tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn visit(root: &Path, directory: &Path, entries: &mut BTreeMap<PathBuf, TreeEntry>) {
        for entry in fs::read_dir(directory).expect("read project tree") {
            let entry = entry.expect("project tree entry");
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("project tree metadata");
            let relative = path
                .strip_prefix(root)
                .expect("project-relative entry")
                .to_path_buf();
            if metadata.is_dir() {
                assert_eq!(entries.insert(relative, TreeEntry::Directory), None);
                visit(root, &path, entries);
            } else {
                assert!(metadata.is_file(), "unexpected project entry {path:?}");
                assert_eq!(
                    entries.insert(
                        relative,
                        TreeEntry::File(fs::read(&path).expect("read project tree file")),
                    ),
                    None
                );
            }
        }
    }

    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn assert_exact_first_use_coordination(root: &Path) {
    assert_eq!(
        project_tree(root),
        BTreeMap::from([
            (
                PathBuf::from("index.html"),
                TreeEntry::File(INDEX_HTML.to_vec())
            ),
            (PathBuf::from("src"), TreeEntry::Directory),
            (PathBuf::from("src/components"), TreeEntry::Directory),
            (PathBuf::from("src/components/ui"), TreeEntry::Directory),
            (
                PathBuf::from("src/components/ui/_kit"),
                TreeEntry::Directory,
            ),
            (
                PathBuf::from("src/components/ui/_kit/.gitignore"),
                TreeEntry::File(KIT_GITIGNORE.to_vec()),
            ),
            (
                PathBuf::from(DEFAULT_KIT_WRITE_LOCK_PATH),
                TreeEntry::File(ADVISORY_LOCK_MARKER.to_vec()),
            ),
            (
                PathBuf::from("src/components/ui/_kit/.transactions"),
                TreeEntry::Directory,
            ),
        ]),
        "first-use operation left a non-exact coordination residual"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for (path, expected_mode) in [
            ("src/components/ui/_kit", 0o700),
            ("src/components/ui/_kit/.gitignore", 0o644),
            (DEFAULT_KIT_WRITE_LOCK_PATH, 0o600),
        ] {
            assert_eq!(
                fs::metadata(root.join(path))
                    .unwrap_or_else(|error| panic!("metadata {path}: {error}"))
                    .permissions()
                    .mode()
                    & 0o7777,
                expected_mode,
                "unexpected coordination mode for {path}"
            );
        }
    }
}
