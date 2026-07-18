#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[cfg(windows)]
use leptos_ui_kit_codegen::DEFAULT_KIT_WRITE_LOCK_PATH;
use leptos_ui_kit_codegen::{
    CodegenError, PathPreimage, apply_add, apply_init, plan_add, plan_init, plan_sync,
    write_file_atomic,
};
use tempfile::tempdir;

const INIT_OBSERVATIONS: [&str; 6] = [
    "index.html",
    "src/components/mod.rs",
    "src/components/ui/_kit/kit.json",
    "src/components/ui/_kit/kit.lock.json",
    "src/components/ui/mod.rs",
    "styles/kit.css",
];

#[test]
fn init_snapshot_is_exact_sorted_and_dry_run_is_write_free() {
    let directory = tempdir().expect("tempdir");
    setup_project(directory.path());
    let before = tree_snapshot(directory.path());

    let plan = plan_init(directory.path()).expect("plan init");

    assert_eq!(tree_snapshot(directory.path()), before);
    assert_eq!(observation_paths(&plan.snapshot), expected_init_paths());
    assert_eq!(plan.snapshot.len(), INIT_OBSERVATIONS.len());
    assert!(matches!(
        plan.snapshot.preimage("index.html"),
        Some(PathPreimage::RegularFile { .. })
    ));
    for file in &plan.files {
        assert!(
            plan.snapshot.preimage(&file.path).is_some(),
            "planned target lacks a preimage: {}",
            file.path
        );
    }

    let value = serde_json::to_value(&plan).expect("serialize plan");
    assert_eq!(
        value
            .as_object()
            .expect("plan object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["changes", "files", "projectRoot"]
    );
    assert!(value.get("snapshot").is_none());
}

#[test]
fn unchanged_init_and_sync_inputs_still_have_exact_preimages() {
    let directory = tempdir().expect("tempdir");
    setup_project(directory.path());
    apply_init(directory.path()).expect("apply init");

    let init = plan_init(directory.path()).expect("plan unchanged init");
    let sync = plan_sync(directory.path()).expect("plan unchanged sync");

    assert!(init.files.is_empty());
    assert_eq!(observation_paths(&init.snapshot), expected_init_paths());
    assert_eq!(observation_paths(&sync.snapshot), expected_init_paths());
    for path in INIT_OBSERVATIONS {
        assert!(matches!(
            sync.snapshot.preimage(path),
            Some(PathPreimage::RegularFile { .. })
        ));
    }
}

#[test]
fn add_snapshot_covers_dependency_sources_and_every_planned_target() {
    let directory = tempdir().expect("tempdir");
    setup_project(directory.path());
    apply_init(directory.path()).expect("apply init");

    let plan = plan_add(directory.path(), "button").expect("plan add button");
    let paths = observation_paths(&plan.snapshot);

    assert_strictly_sorted(&paths);
    for path in expected_init_paths() {
        assert!(paths.contains(&path), "missing common observation {path}");
    }
    assert!(paths.contains(&"src/components/ui/button.rs"));
    assert!(paths.contains(&"src/components/ui/spinner.rs"));
    for file in &plan.files {
        assert!(
            plan.snapshot.preimage(&file.path).is_some(),
            "planned target lacks a preimage: {}",
            file.path
        );
    }

    let nested = plan_add(directory.path(), "dialog").expect("plan add dialog");
    assert!(
        nested
            .snapshot
            .preimage("src/components/ui/dialog/root.rs")
            .is_some()
    );
    for file in &nested.files {
        assert!(
            nested.snapshot.preimage(&file.path).is_some(),
            "nested planned target lacks a preimage: {}",
            file.path
        );
    }
}

#[test]
fn sync_snapshot_covers_installed_items_nested_targets_and_dependency_closure() {
    let directory = tempdir().expect("tempdir");
    setup_project(directory.path());
    apply_init(directory.path()).expect("apply init");
    apply_add(directory.path(), "router-link").expect("install router-link closure");
    apply_add(directory.path(), "dialog").expect("install nested dialog item");
    let before = tree_snapshot(directory.path());

    let plan = plan_sync(directory.path()).expect("plan installed desired state");
    let paths = observation_paths(&plan.snapshot);

    assert_eq!(tree_snapshot(directory.path()), before);
    assert!(plan.files.is_empty());
    assert_strictly_sorted(&paths);
    for item_id in [
        "builtin:anchor",
        "builtin:dialog",
        "builtin:router-link",
        "builtin:tokens",
    ] {
        assert!(
            plan.lock.items.contains_key(item_id),
            "resolved install lock is missing {item_id}"
        );
    }
    assert!(paths.contains(&"src/components/ui/anchor.rs"));
    assert!(paths.contains(&"src/components/ui/router_link.rs"));
    assert!(paths.contains(&"src/components/ui/dialog/root.rs"));
    assert!(paths.contains(&"src/components/ui/dialog/description.rs"));

    let mut expected = INIT_OBSERVATIONS
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for item in plan.lock.items.values() {
        for file in &item.files {
            expected.push(file.path.clone());
            assert!(matches!(
                plan.snapshot.preimage(&file.path),
                Some(PathPreimage::RegularFile { .. })
            ));
        }
    }
    expected.sort();
    expected.dedup();
    assert_eq!(
        paths,
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
}

#[test]
fn unexpected_target_types_are_rejected_before_content_reads() {
    #[cfg(unix)]
    let directory = tempfile::Builder::new()
        .prefix("luk")
        .tempdir_in("/tmp")
        .expect("short tempdir");
    #[cfg(not(unix))]
    let directory = tempdir().expect("tempdir");
    let root = directory.path();
    fs::create_dir_all(root.join("src")).expect("create source directory");
    fs::create_dir(root.join("index.html")).expect("create directory target");

    let error = plan_init(root).expect_err("directory target must fail");
    assert!(matches!(error, CodegenError::UnsafePath { .. }));

    #[cfg(unix)]
    {
        fs::remove_dir(root.join("index.html")).expect("remove directory target");
        let _listener = std::os::unix::net::UnixListener::bind(root.join("index.html"))
            .expect("create socket target");
        let error = plan_init(root).expect_err("socket target must fail before read");
        assert!(matches!(error, CodegenError::UnsafePath { .. }));
    }
}

#[cfg(unix)]
#[test]
fn parent_and_final_symlinks_are_rejected_without_touching_referents() {
    let directory = tempdir().expect("tempdir");
    let outside = tempdir().expect("outside tempdir");
    setup_project(directory.path());
    fs::create_dir_all(directory.path().join("src/components")).expect("create component parent");
    fs::write(outside.path().join("canary"), b"outside\n").expect("write canary");
    std::os::unix::fs::symlink(outside.path(), directory.path().join("src/components/ui"))
        .expect("create parent symlink");

    let error = plan_init(directory.path()).expect_err("parent symlink must fail");
    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(
        fs::read(outside.path().join("canary")).expect("read canary"),
        b"outside\n"
    );

    fs::remove_file(directory.path().join("src/components/ui")).expect("remove parent symlink");
    let outside_index = outside.path().join("outside-index.html");
    fs::write(&outside_index, b"outside html\n").expect("write outside index");
    fs::remove_file(directory.path().join("index.html")).expect("remove project index");
    std::os::unix::fs::symlink(&outside_index, directory.path().join("index.html"))
        .expect("create final symlink");

    let error = plan_init(directory.path()).expect_err("final symlink must fail");
    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(
        fs::read(outside_index).expect("read outside index"),
        b"outside html\n"
    );
}

#[cfg(unix)]
#[test]
fn predictable_temporary_symlink_cannot_capture_a_write() {
    let directory = tempdir().expect("tempdir");
    let outside = tempdir().expect("outside tempdir");
    fs::create_dir_all(directory.path().join("styles")).expect("create styles");
    let referent = outside.path().join("referent");
    fs::write(&referent, b"outside\n").expect("write referent");
    std::os::unix::fs::symlink(
        &referent,
        directory.path().join("styles/kit.leptos-ui-kit.tmp"),
    )
    .expect("create temp symlink");

    let error = write_file_atomic(directory.path(), "styles/kit.css", b"replacement\n")
        .expect_err("temporary symlink must fail");

    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(fs::read(referent).expect("read referent"), b"outside\n");
    assert!(!directory.path().join("styles/kit.css").exists());
}

#[cfg(windows)]
#[test]
fn windows_intermediate_directory_junction_is_rejected_without_touching_referent() {
    let directory = tempdir().expect("tempdir");
    let outside = tempdir().expect("outside tempdir");
    setup_project(directory.path());
    fs::create_dir_all(directory.path().join("src/components")).expect("create component parent");
    fs::write(outside.path().join("canary"), b"outside\n").expect("write canary");
    create_windows_junction(&directory.path().join("src/components/ui"), outside.path());

    let error = plan_init(directory.path()).expect_err("directory junction must fail");

    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(
        fs::read(outside.path().join("canary")).expect("read canary"),
        b"outside\n"
    );
}

#[cfg(windows)]
#[test]
fn windows_final_file_reparse_is_rejected_without_touching_referent() {
    let directory = tempdir().expect("tempdir");
    let outside = tempdir().expect("outside tempdir");
    setup_project(directory.path());
    let referent = outside.path().join("outside-index.html");
    fs::write(&referent, b"outside html\n").expect("write outside index");
    fs::remove_file(directory.path().join("index.html")).expect("remove project index");
    std::os::windows::fs::symlink_file(&referent, directory.path().join("index.html"))
        .expect("create final-file reparse point");

    let error = plan_init(directory.path()).expect_err("final-file reparse point must fail");

    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(
        fs::read(referent).expect("read outside index"),
        b"outside html\n"
    );
}

#[cfg(windows)]
#[test]
fn windows_sentinel_and_temporary_reparse_points_cannot_capture_writes() {
    let sentinel_project = tempdir().expect("sentinel project");
    let outside = tempdir().expect("outside tempdir");
    setup_project(sentinel_project.path());
    let sentinel_path = sentinel_project.path().join(DEFAULT_KIT_WRITE_LOCK_PATH);
    fs::create_dir_all(sentinel_path.parent().expect("sentinel parent"))
        .expect("create sentinel parent");
    let sentinel_referent = outside.path().join("sentinel-referent");
    fs::write(&sentinel_referent, b"outside sentinel\n").expect("write sentinel referent");
    std::os::windows::fs::symlink_file(&sentinel_referent, &sentinel_path)
        .expect("create sentinel reparse point");

    let error = apply_init(sentinel_project.path()).expect_err("sentinel reparse point must fail");
    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(
        fs::read(&sentinel_referent).expect("read sentinel referent"),
        b"outside sentinel\n"
    );

    let stage_project = tempdir().expect("stage project");
    fs::create_dir_all(stage_project.path().join("styles")).expect("create styles");
    let stage_referent = outside.path().join("stage-referent");
    fs::write(&stage_referent, b"outside stage\n").expect("write stage referent");
    std::os::windows::fs::symlink_file(
        &stage_referent,
        stage_project.path().join("styles/kit.leptos-ui-kit.tmp"),
    )
    .expect("create stage reparse point");

    let error = write_file_atomic(stage_project.path(), "styles/kit.css", b"replacement\n")
        .expect_err("stage reparse point must fail");
    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(
        fs::read(stage_referent).expect("read stage referent"),
        b"outside stage\n"
    );
    assert!(!stage_project.path().join("styles/kit.css").exists());
}

#[cfg(unix)]
#[test]
fn root_aliases_are_bound_and_existing_posix_modes_survive_replacement() {
    use std::os::unix::fs::PermissionsExt;

    let parent = tempdir().expect("tempdir");
    let real_root = parent.path().join("real");
    let alias = parent.path().join("alias");
    fs::create_dir(&real_root).expect("create real root");
    setup_project(&real_root);
    fs::set_permissions(
        real_root.join("index.html"),
        fs::Permissions::from_mode(0o751),
    )
    .expect("set index mode");
    std::os::unix::fs::symlink(&real_root, &alias).expect("create project alias");

    let plan = apply_init(&alias).expect("apply through stable alias");

    assert_eq!(plan.project_root, alias);
    assert_eq!(
        fs::metadata(real_root.join("index.html"))
            .expect("index metadata")
            .permissions()
            .mode()
            & 0o7777,
        0o751
    );
}

fn setup_project(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("create source directory");
    fs::write(
        root.join("index.html"),
        "<html><head></head><body></body></html>\n",
    )
    .expect("write index");
}

fn expected_init_paths() -> Vec<&'static str> {
    INIT_OBSERVATIONS.to_vec()
}

fn observation_paths(snapshot: &leptos_ui_kit_codegen::PlanSnapshot) -> Vec<&str> {
    snapshot.observations().map(|(path, _)| path).collect()
}

fn assert_strictly_sorted(paths: &[&str]) {
    assert!(
        paths.windows(2).all(|pair| pair[0] < pair[1]),
        "observation iterator must be strictly sorted: {paths:?}"
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
    kind: TreeEntryKind,
    readonly: bool,
    posix_mode: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TreeEntryKind {
    Directory,
    RegularFile(Vec<u8>),
    Symlink(PathBuf),
    Special(&'static str),
    #[cfg(windows)]
    ReparsePoint(Option<PathBuf>),
}

fn tree_snapshot(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn visit(root: &Path, directory: &Path, output: &mut BTreeMap<PathBuf, TreeEntry>) {
        for entry in fs::read_dir(directory).expect("read project tree") {
            let entry = entry.expect("project entry");
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("relative path")
                .to_path_buf();
            let metadata = fs::symlink_metadata(&path).expect("entry metadata");
            let file_type = metadata.file_type();
            let kind = if is_reparse_point(&metadata) {
                reparse_entry_kind(&path)
            } else if file_type.is_symlink() {
                TreeEntryKind::Symlink(fs::read_link(&path).expect("read project symlink"))
            } else if metadata.is_dir() {
                TreeEntryKind::Directory
            } else if metadata.is_file() {
                TreeEntryKind::RegularFile(fs::read(&path).expect("read project file"))
            } else {
                TreeEntryKind::Special(special_file_type(&file_type))
            };
            let should_recurse = matches!(kind, TreeEntryKind::Directory);
            output.insert(
                relative,
                TreeEntry {
                    kind,
                    readonly: metadata.permissions().readonly(),
                    posix_mode: posix_mode(&metadata),
                },
            );
            if should_recurse {
                visit(root, &path, output);
            }
        }
    }

    let mut output = BTreeMap::new();
    visit(root, root, &mut output);
    output
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_attributes() & 0x0000_0400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn reparse_entry_kind(path: &Path) -> TreeEntryKind {
    TreeEntryKind::ReparsePoint(fs::read_link(path).ok())
}

#[cfg(not(windows))]
fn reparse_entry_kind(_path: &Path) -> TreeEntryKind {
    unreachable!("non-Windows metadata does not report reparse points")
}

#[cfg(unix)]
fn special_file_type(file_type: &fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt;

    if file_type.is_socket() {
        "socket"
    } else if file_type.is_fifo() {
        "fifo"
    } else if file_type.is_block_device() {
        "block_device"
    } else if file_type.is_char_device() {
        "char_device"
    } else {
        "other"
    }
}

#[cfg(not(unix))]
fn special_file_type(_file_type: &fs::FileType) -> &'static str {
    "other"
}

#[cfg(unix)]
fn posix_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;

    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn posix_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(windows)]
fn create_windows_junction(junction: &Path, target: &Path) {
    let output = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(junction)
        .arg(target)
        .output()
        .expect("invoke mklink for directory junction");
    assert!(
        output.status.success(),
        "create directory junction: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
