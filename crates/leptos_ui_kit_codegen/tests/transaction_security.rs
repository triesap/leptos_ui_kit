#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use leptos_ui_kit_codegen::{
    CodegenError, DEFAULT_KIT_WRITE_LOCK_PATH, PathPreimage, WriteLock, apply_add, apply_init,
    apply_sync, plan_add, plan_init, plan_sync, write_file_atomic,
};
use tempfile::tempdir;

const KIT_COORDINATION_DIR: &str = "src/components/ui/_kit";
const KIT_GITIGNORE_PATH: &str = "src/components/ui/_kit/.gitignore";
const ADVISORY_LOCK_MARKER: &[u8] = b"leptos-ui-kit advisory lock v1\n";
const LEGACY_SENTINEL_MARKER: &[u8] = b"locked\n";
const KIT_GITIGNORE: &[u8] = b"/.write.lock\n/.transactions/\n";

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
    assert!(!directory.path().join(KIT_GITIGNORE_PATH).exists());
    assert!(!directory.path().join(DEFAULT_KIT_WRITE_LOCK_PATH).exists());
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

#[cfg(unix)]
#[test]
fn dry_run_does_not_repair_a_restrictive_coordination_directory_mode() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().expect("tempdir");
    setup_project(directory.path());
    apply_init(directory.path()).expect("initialize coordination state");
    fs::set_permissions(
        directory.path().join(KIT_COORDINATION_DIR),
        fs::Permissions::from_mode(0o500),
    )
    .expect("make coordination directory owner-read-only");
    let before = tree_snapshot(directory.path());

    let plan = plan_init(directory.path()).expect("plan through restrictive coordination mode");

    assert!(plan.is_empty());
    assert_eq!(tree_snapshot(directory.path()), before);
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
fn apply_and_no_change_commands_preserve_the_exact_coordination_residual() {
    let directory = tempdir().expect("tempdir");
    setup_project(directory.path());

    apply_init(directory.path()).expect("apply init");
    assert_exact_coordination_residual(directory.path());
    #[cfg(any(unix, windows))]
    let initial_identity = coordination_lock_identity(directory.path());
    #[cfg(unix)]
    let initial_modes = coordination_modes(directory.path());

    let second_init = apply_init(directory.path()).expect("no-change init");
    let add = apply_add(directory.path(), "button").expect("apply add");
    let second_add = apply_add(directory.path(), "button").expect("no-change add");
    let sync = apply_sync(directory.path()).expect("no-change sync");

    assert!(second_init.is_empty());
    assert!(!add.is_empty());
    assert!(second_add.is_empty());
    assert!(sync.is_empty());
    assert_exact_coordination_residual(directory.path());
    #[cfg(any(unix, windows))]
    assert_eq!(
        coordination_lock_identity(directory.path()),
        initial_identity
    );
    #[cfg(unix)]
    assert_eq!(coordination_modes(directory.path()), initial_modes);
}

#[test]
fn legacy_and_invalid_lock_markers_fail_closed_without_project_writes() {
    for (case, marker, expected) in [
        ("legacy sentinel", LEGACY_SENTINEL_MARKER, "legacy"),
        ("empty marker", b"".as_slice(), "invalid"),
        (
            "partial advisory marker",
            b"leptos-ui-kit advisory".as_slice(),
            "invalid",
        ),
        (
            "unknown marker",
            b"application-owned\n".as_slice(),
            "invalid",
        ),
    ] {
        let directory = tempdir().expect("tempdir");
        setup_project(directory.path());
        seed_coordination(directory.path(), marker);
        let before = tree_snapshot(directory.path());
        #[cfg(any(unix, windows))]
        let identity_before =
            coordination_file_identity(&directory.path().join(DEFAULT_KIT_WRITE_LOCK_PATH));

        let error = apply_init(directory.path()).expect_err(case);

        match expected {
            "legacy" => match &error {
                CodegenError::LegacyWriteLock { path } => {
                    assert_eq!(path, DEFAULT_KIT_WRITE_LOCK_PATH, "{case}");
                }
                other => panic!("{case}: expected legacy lock error, got {other}"),
            },
            "invalid" => match &error {
                CodegenError::InvalidCoordinationState { path, reason } => {
                    assert_eq!(path, DEFAULT_KIT_WRITE_LOCK_PATH, "{case}");
                    assert!(!reason.is_empty(), "{case}: invalid-state reason");
                }
                other => panic!("{case}: expected invalid coordination error, got {other}"),
            },
            _ => unreachable!("known expected error class"),
        }
        assert!(
            error.to_string().contains("manually"),
            "{case}: coordination recovery guidance"
        );
        assert_eq!(tree_snapshot(directory.path()), before, "{case}");
        assert_eq!(
            fs::read(directory.path().join(DEFAULT_KIT_WRITE_LOCK_PATH))
                .expect("read unchanged coordination marker"),
            marker,
            "{case}"
        );
        #[cfg(any(unix, windows))]
        assert_eq!(
            coordination_file_identity(&directory.path().join(DEFAULT_KIT_WRITE_LOCK_PATH)),
            identity_before,
            "{case}: rejected lock inode must not be replaced"
        );
    }
}

#[test]
fn non_exact_gitignore_is_never_overwritten_or_augmented() {
    for content in [
        b"*.tmp\n".as_slice(),
        b"/.write.lock\n".as_slice(),
        b"/.write.lock\n/.transactions/\nkit.json\n".as_slice(),
    ] {
        let directory = tempdir().expect("tempdir");
        setup_project(directory.path());
        fs::create_dir_all(directory.path().join(KIT_COORDINATION_DIR))
            .expect("create coordination directory");
        set_fixture_coordination_directory_mode(directory.path());
        fs::write(directory.path().join(KIT_GITIGNORE_PATH), content)
            .expect("seed non-exact gitignore");
        let before = tree_snapshot(directory.path());
        #[cfg(any(unix, windows))]
        let identity_before =
            coordination_file_identity(&directory.path().join(KIT_GITIGNORE_PATH));

        let error = apply_init(directory.path()).expect_err("non-exact gitignore must fail");

        match error {
            CodegenError::InvalidCoordinationState { path, reason } => {
                assert_eq!(path, KIT_GITIGNORE_PATH);
                assert!(!reason.is_empty(), "invalid-state reason");
            }
            other => panic!("expected invalid coordination error, got {other}"),
        }
        let mut after = tree_snapshot(directory.path());
        let lock_entry = after.remove(Path::new(DEFAULT_KIT_WRITE_LOCK_PATH));
        assert_eq!(after, before);
        if let Some(lock_entry) = lock_entry {
            assert_eq!(
                lock_entry.kind,
                TreeEntryKind::RegularFile(ADVISORY_LOCK_MARKER.to_vec())
            );
        }
        assert_eq!(
            fs::read(directory.path().join(KIT_GITIGNORE_PATH)).expect("read gitignore"),
            content
        );
        #[cfg(any(unix, windows))]
        assert_eq!(
            coordination_file_identity(&directory.path().join(KIT_GITIGNORE_PATH)),
            identity_before,
            "rejected gitignore inode must not be replaced"
        );
        assert_absent_or_exact_persistent_lock(directory.path());
    }
}

#[cfg(unix)]
#[test]
fn permissive_existing_coordination_file_modes_fail_closed() {
    use std::os::unix::fs::PermissionsExt;

    for (logical_path, expected_path) in [
        (DEFAULT_KIT_WRITE_LOCK_PATH, DEFAULT_KIT_WRITE_LOCK_PATH),
        (KIT_GITIGNORE_PATH, KIT_GITIGNORE_PATH),
    ] {
        let directory = tempdir().expect("tempdir");
        setup_project(directory.path());
        apply_init(directory.path()).expect("initialize exact coordination");
        fs::set_permissions(
            directory.path().join(logical_path),
            fs::Permissions::from_mode(0o666),
        )
        .expect("make coordination file permissive");
        let before = tree_snapshot(directory.path());

        let error = apply_init(directory.path()).expect_err("permissive mode must fail closed");

        assert!(matches!(
            error,
            CodegenError::InvalidCoordinationState { path, .. }
                if path == expected_path
        ));
        assert_eq!(tree_snapshot(directory.path()), before);
    }
}

#[cfg(unix)]
#[test]
fn existing_coordination_directory_is_tightened_through_its_open_handle() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().expect("tempdir");
    setup_project(directory.path());
    apply_init(directory.path()).expect("initialize exact coordination");
    fs::set_permissions(
        directory.path().join(KIT_COORDINATION_DIR),
        fs::Permissions::from_mode(0o755),
    )
    .expect("make coordination directory permissive");

    let plan = apply_init(directory.path()).expect("tighten existing coordination directory");

    assert!(plan.is_empty());
    assert_exact_coordination_residual(directory.path());
}

#[test]
fn transaction_bootstrap_inventory_fails_closed() {
    for case in ["unexpected-name", "invalid-candidate"] {
        let directory = tempdir().expect("tempdir");
        setup_project(directory.path());
        apply_init(directory.path()).expect("initialize coordination");
        let transactions = directory
            .path()
            .join(KIT_COORDINATION_DIR)
            .join(".transactions");
        fs::create_dir(&transactions).expect("create transaction bootstrap directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&transactions, fs::Permissions::from_mode(0o700))
                .expect("set transaction bootstrap directory mode");
        }
        match case {
            "unexpected-name" => {
                fs::write(transactions.join("application-owned"), b"preserve\n")
                    .expect("write unexpected transaction entry");
            }
            "invalid-candidate" => {
                let candidate =
                    transactions.join("lock-bootstrap-00000000000000000000000000000000");
                fs::write(&candidate, b"application-owned\n").expect("write invalid candidate");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;

                    fs::set_permissions(candidate, fs::Permissions::from_mode(0o600))
                        .expect("set invalid candidate mode");
                }
            }
            _ => unreachable!("known transaction fixture"),
        }
        let before = tree_snapshot(directory.path());

        let error = WriteLock::acquire(directory.path()).expect_err(case);

        assert!(matches!(
            error,
            CodegenError::InvalidCoordinationState { .. }
        ));
        assert_eq!(tree_snapshot(directory.path()), before, "{case}");
    }
}

#[cfg(unix)]
#[test]
fn unix_transaction_bootstrap_mode_and_symlinks_fail_closed() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().expect("tempdir");
    setup_project(directory.path());
    apply_init(directory.path()).expect("initialize coordination");
    let transactions = directory
        .path()
        .join(KIT_COORDINATION_DIR)
        .join(".transactions");
    fs::create_dir(&transactions).expect("create transaction bootstrap directory");
    fs::set_permissions(&transactions, fs::Permissions::from_mode(0o755))
        .expect("make transaction bootstrap directory permissive");
    let before = tree_snapshot(directory.path());

    let error = WriteLock::acquire(directory.path())
        .expect_err("permissive transaction directory must fail closed");

    assert!(matches!(
        error,
        CodegenError::InvalidCoordinationState { .. }
    ));
    assert_eq!(tree_snapshot(directory.path()), before);

    let directory = tempdir().expect("tempdir");
    setup_project(directory.path());
    apply_init(directory.path()).expect("initialize coordination");
    let transactions = directory
        .path()
        .join(KIT_COORDINATION_DIR)
        .join(".transactions");
    let outside = tempdir().expect("outside tempdir");
    fs::write(outside.path().join("sentinel"), b"outside\n").expect("write outside sentinel");
    std::os::unix::fs::symlink(outside.path(), &transactions)
        .expect("symlink transaction bootstrap directory");
    let before = tree_snapshot(directory.path());

    let error = WriteLock::acquire(directory.path()).expect_err("transaction symlink");

    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(tree_snapshot(directory.path()), before);
    assert_eq!(
        fs::read(outside.path().join("sentinel")).expect("read outside sentinel"),
        b"outside\n"
    );

    let directory = tempdir().expect("tempdir");
    setup_project(directory.path());
    apply_init(directory.path()).expect("initialize coordination");
    let transactions = directory
        .path()
        .join(KIT_COORDINATION_DIR)
        .join(".transactions");
    fs::create_dir(&transactions).expect("create transaction bootstrap directory");
    fs::set_permissions(&transactions, fs::Permissions::from_mode(0o700))
        .expect("set transaction bootstrap directory mode");
    let outside = tempdir().expect("candidate referent tempdir");
    let referent = outside.path().join("candidate-referent");
    fs::write(&referent, b"outside candidate\n").expect("write candidate referent");
    let candidate = transactions.join("lock-bootstrap-00000000000000000000000000000000");
    std::os::unix::fs::symlink(&referent, &candidate).expect("symlink transaction candidate");
    let before = tree_snapshot(directory.path());

    let error = WriteLock::acquire(directory.path()).expect_err("candidate symlink");

    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(tree_snapshot(directory.path()), before);
    assert_eq!(
        fs::read(referent).expect("read candidate referent"),
        b"outside candidate\n"
    );
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
fn unix_coordination_chain_symlinks_and_special_files_fail_closed() {
    for chain_path in [
        "src",
        "src/components",
        "src/components/ui",
        KIT_COORDINATION_DIR,
        "src/components/ui/_kit/.transactions",
    ] {
        let directory = tempdir().expect("tempdir");
        let outside = tempdir().expect("outside tempdir");
        setup_project(directory.path());
        if chain_path == "src" {
            fs::remove_dir(directory.path().join(chain_path)).expect("remove source directory");
        }
        fs::create_dir_all(
            directory
                .path()
                .join(chain_path)
                .parent()
                .expect("chain parent"),
        )
        .expect("create chain parent");
        let canary = outside.path().join("canary");
        fs::write(&canary, b"outside\n").expect("write canary");
        std::os::unix::fs::symlink(outside.path(), directory.path().join(chain_path))
            .expect("create coordination-chain symlink");
        let before = tree_snapshot(directory.path());

        let error = apply_init(directory.path()).expect_err("chain symlink must fail");

        assert!(matches!(error, CodegenError::UnsafePath { .. }));
        let mut expected = before;
        if chain_path == "src/components/ui/_kit/.transactions" {
            // Hardening the verified coordination directory is part of the
            // permitted pre-lock directory bootstrap; the hostile child and
            // every other entry must remain exact.
            expected
                .get_mut(Path::new(KIT_COORDINATION_DIR))
                .expect("coordination directory snapshot")
                .posix_mode = Some(0o700);
        }
        assert_eq!(tree_snapshot(directory.path()), expected);
        assert_eq!(fs::read(&canary).expect("read canary"), b"outside\n");
        assert!(!outside.path().join(".write.lock").exists());
        assert!(!outside.path().join(".gitignore").exists());
    }

    for chain_path in [
        "src",
        "src/components",
        "src/components/ui",
        KIT_COORDINATION_DIR,
    ] {
        let directory = tempfile::Builder::new()
            .prefix("luk")
            .tempdir_in("/tmp")
            .expect("short tempdir");
        setup_project(directory.path());
        if chain_path == "src" {
            fs::remove_dir(directory.path().join(chain_path)).expect("remove source directory");
        }
        fs::create_dir_all(
            directory
                .path()
                .join(chain_path)
                .parent()
                .expect("chain parent"),
        )
        .expect("create coordination parent");
        let _listener = std::os::unix::net::UnixListener::bind(directory.path().join(chain_path))
            .expect("create special coordination entry");
        let before = tree_snapshot(directory.path());

        let error = apply_init(directory.path()).expect_err("special coordination entry must fail");

        assert!(matches!(error, CodegenError::UnsafePath { .. }));
        assert_eq!(tree_snapshot(directory.path()), before);
    }
}

#[cfg(unix)]
#[test]
fn unix_coordination_file_symlinks_and_special_files_do_not_touch_referents() {
    for logical_path in [KIT_GITIGNORE_PATH, DEFAULT_KIT_WRITE_LOCK_PATH] {
        let directory = tempdir().expect("tempdir");
        let outside = tempdir().expect("outside tempdir");
        setup_project(directory.path());
        fs::create_dir_all(directory.path().join(KIT_COORDINATION_DIR))
            .expect("create coordination directory");
        set_fixture_coordination_directory_mode(directory.path());
        if logical_path == DEFAULT_KIT_WRITE_LOCK_PATH {
            fs::write(directory.path().join(KIT_GITIGNORE_PATH), KIT_GITIGNORE)
                .expect("seed exact gitignore");
        }
        let referent = outside.path().join("referent");
        fs::write(&referent, b"outside\n").expect("write referent");
        std::os::unix::fs::symlink(&referent, directory.path().join(logical_path))
            .expect("create coordination-file symlink");
        let before = tree_snapshot(directory.path());

        let error = apply_init(directory.path()).expect_err("coordination symlink must fail");

        assert!(matches!(error, CodegenError::UnsafePath { .. }));
        if logical_path == KIT_GITIGNORE_PATH {
            assert_tree_unchanged_except_optional_lock(directory.path(), &before);
        } else {
            assert_eq!(tree_snapshot(directory.path()), before);
        }
        assert_eq!(fs::read(&referent).expect("read referent"), b"outside\n");
        if logical_path == KIT_GITIGNORE_PATH {
            assert_absent_or_exact_persistent_lock(directory.path());
        }
    }

    for logical_path in [KIT_GITIGNORE_PATH, DEFAULT_KIT_WRITE_LOCK_PATH] {
        let directory = tempfile::Builder::new()
            .prefix("luk")
            .tempdir_in("/tmp")
            .expect("short tempdir");
        setup_project(directory.path());
        fs::create_dir_all(directory.path().join(KIT_COORDINATION_DIR))
            .expect("create coordination directory");
        set_fixture_coordination_directory_mode(directory.path());
        if logical_path == DEFAULT_KIT_WRITE_LOCK_PATH {
            fs::write(directory.path().join(KIT_GITIGNORE_PATH), KIT_GITIGNORE)
                .expect("seed exact gitignore");
        }
        let _listener = std::os::unix::net::UnixListener::bind(directory.path().join(logical_path))
            .expect("create special coordination entry");
        let before = tree_snapshot(directory.path());

        let error = apply_init(directory.path()).expect_err("special coordination file must fail");

        assert!(matches!(error, CodegenError::UnsafePath { .. }));
        if logical_path == KIT_GITIGNORE_PATH {
            assert_tree_unchanged_except_optional_lock(directory.path(), &before);
        } else {
            assert_eq!(tree_snapshot(directory.path()), before);
        }
        if logical_path == KIT_GITIGNORE_PATH {
            assert_absent_or_exact_persistent_lock(directory.path());
        }
    }
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
fn windows_coordination_chain_junctions_are_rejected_without_touching_referents() {
    for chain_path in [
        "src",
        "src/components",
        "src/components/ui",
        KIT_COORDINATION_DIR,
        "src/components/ui/_kit/.transactions",
    ] {
        let directory = tempdir().expect("tempdir");
        let outside = tempdir().expect("outside tempdir");
        setup_project(directory.path());
        if chain_path == "src" {
            fs::remove_dir(directory.path().join(chain_path)).expect("remove source directory");
        }
        fs::create_dir_all(
            directory
                .path()
                .join(chain_path)
                .parent()
                .expect("chain parent"),
        )
        .expect("create coordination parent");
        fs::write(outside.path().join("canary"), b"outside\n").expect("write canary");
        let outside_before = tree_snapshot(outside.path());
        create_windows_junction(&directory.path().join(chain_path), outside.path());
        let before = tree_snapshot(directory.path());

        let error = apply_init(directory.path()).expect_err("directory junction must fail");

        assert!(matches!(error, CodegenError::UnsafePath { .. }));
        assert_eq!(tree_snapshot(directory.path()), before);
        assert_eq!(tree_snapshot(outside.path()), outside_before);
        assert_eq!(
            fs::read(outside.path().join("canary")).expect("read canary"),
            b"outside\n"
        );
        assert!(!outside.path().join(".write.lock").exists());
        assert!(!outside.path().join(".gitignore").exists());
    }
}

#[cfg(windows)]
#[test]
fn windows_transaction_candidate_reparse_is_rejected_without_touching_referent() {
    let directory = tempdir().expect("tempdir");
    let outside = tempdir().expect("outside tempdir");
    setup_project(directory.path());
    apply_init(directory.path()).expect("initialize coordination");
    let transactions = directory
        .path()
        .join(KIT_COORDINATION_DIR)
        .join(".transactions");
    fs::create_dir(&transactions).expect("create transaction bootstrap directory");
    let referent = outside.path().join("candidate-referent");
    fs::write(&referent, b"outside candidate\n").expect("write candidate referent");
    let candidate = transactions.join("lock-bootstrap-00000000000000000000000000000000");
    std::os::windows::fs::symlink_file(&referent, &candidate)
        .expect("create candidate reparse point");
    let before = tree_snapshot(directory.path());
    let outside_before = tree_snapshot(outside.path());

    let error = WriteLock::acquire(directory.path()).expect_err("candidate reparse point");

    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(tree_snapshot(directory.path()), before);
    assert_eq!(tree_snapshot(outside.path()), outside_before);
    assert_eq!(
        fs::read(referent).expect("read candidate referent"),
        b"outside candidate\n"
    );
}

#[cfg(windows)]
#[test]
fn windows_coordination_gitignore_reparse_is_rejected_without_touching_referent() {
    let directory = tempdir().expect("tempdir");
    let outside = tempdir().expect("outside tempdir");
    setup_project(directory.path());
    fs::create_dir_all(directory.path().join(KIT_COORDINATION_DIR))
        .expect("create coordination directory");
    set_fixture_coordination_directory_mode(directory.path());
    let referent = outside.path().join("gitignore-referent");
    fs::write(&referent, b"outside gitignore\n").expect("write referent");
    std::os::windows::fs::symlink_file(&referent, directory.path().join(KIT_GITIGNORE_PATH))
        .expect("create gitignore reparse point");
    let before = tree_snapshot(directory.path());

    let error = apply_init(directory.path()).expect_err("gitignore reparse point must fail");

    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_tree_unchanged_except_optional_lock(directory.path(), &before);
    assert_eq!(
        fs::read(&referent).expect("read referent"),
        b"outside gitignore\n"
    );
    assert_absent_or_exact_persistent_lock(directory.path());
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
fn windows_coordination_lock_and_temporary_reparse_points_cannot_capture_writes() {
    let lock_project = tempdir().expect("lock project");
    let outside = tempdir().expect("outside tempdir");
    setup_project(lock_project.path());
    let lock_path = lock_project.path().join(DEFAULT_KIT_WRITE_LOCK_PATH);
    fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("create lock parent");
    let lock_referent = outside.path().join("lock-referent");
    fs::write(&lock_referent, b"outside lock\n").expect("write lock referent");
    std::os::windows::fs::symlink_file(&lock_referent, &lock_path)
        .expect("create lock reparse point");
    let lock_before = tree_snapshot(lock_project.path());

    let error = apply_init(lock_project.path()).expect_err("lock reparse point must fail");
    assert!(matches!(error, CodegenError::UnsafePath { .. }));
    assert_eq!(tree_snapshot(lock_project.path()), lock_before);
    assert_eq!(
        fs::read(&lock_referent).expect("read lock referent"),
        b"outside lock\n"
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
    assert_exact_coordination_residual(&real_root);
}

fn seed_coordination(root: &Path, marker: &[u8]) {
    fs::create_dir_all(root.join(KIT_COORDINATION_DIR)).expect("create coordination directory");
    fs::write(root.join(KIT_GITIGNORE_PATH), KIT_GITIGNORE).expect("seed exact gitignore");
    fs::write(root.join(DEFAULT_KIT_WRITE_LOCK_PATH), marker).expect("seed lock marker");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(
            root.join(KIT_COORDINATION_DIR),
            fs::Permissions::from_mode(0o700),
        )
        .expect("set coordination directory mode");
        fs::set_permissions(
            root.join(KIT_GITIGNORE_PATH),
            fs::Permissions::from_mode(0o644),
        )
        .expect("set gitignore mode");
        fs::set_permissions(
            root.join(DEFAULT_KIT_WRITE_LOCK_PATH),
            fs::Permissions::from_mode(0o600),
        )
        .expect("set lock mode");
    }
}

fn assert_exact_coordination_residual(root: &Path) {
    let coordination_dir = root.join(KIT_COORDINATION_DIR);
    let gitignore = root.join(KIT_GITIGNORE_PATH);
    let lock = root.join(DEFAULT_KIT_WRITE_LOCK_PATH);

    let directory_metadata =
        fs::symlink_metadata(&coordination_dir).expect("coordination directory metadata");
    assert!(directory_metadata.is_dir());
    assert!(!directory_metadata.file_type().is_symlink());
    assert!(!is_reparse_point(&directory_metadata));

    for (path, expected) in [(&gitignore, KIT_GITIGNORE), (&lock, ADVISORY_LOCK_MARKER)] {
        let metadata = fs::symlink_metadata(path).expect("coordination file metadata");
        assert!(
            metadata.is_file(),
            "{} must be a regular file",
            path.display()
        );
        assert!(!metadata.file_type().is_symlink());
        assert!(!is_reparse_point(&metadata));
        assert_eq!(fs::read(path).expect("read coordination file"), expected);
    }

    let mut names = fs::read_dir(&coordination_dir)
        .expect("read coordination directory")
        .map(|entry| {
            entry
                .expect("coordination entry")
                .file_name()
                .into_string()
                .expect("ASCII coordination name")
        })
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![".gitignore", ".write.lock", "kit.json", "kit.lock.json"]
    );
    assert!(!coordination_dir.join(".transactions").exists());

    #[cfg(unix)]
    {
        assert_eq!(
            coordination_modes(root),
            (0o700, 0o644, 0o600),
            "new coordination entries must use their frozen POSIX modes"
        );
    }
}

fn assert_exact_persistent_lock(root: &Path) {
    let lock = root.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    let metadata = fs::symlink_metadata(&lock).expect("persistent advisory lock metadata");
    assert!(metadata.is_file(), "advisory lock must be a regular file");
    assert!(!metadata.file_type().is_symlink());
    assert!(!is_reparse_point(&metadata));
    assert_eq!(
        fs::read(lock).expect("read persistent advisory lock"),
        ADVISORY_LOCK_MARKER
    );
}

fn assert_absent_or_exact_persistent_lock(root: &Path) {
    let lock = root.join(DEFAULT_KIT_WRITE_LOCK_PATH);
    if lock.try_exists().expect("inspect persistent advisory lock") {
        assert_exact_persistent_lock(root);
    }
}

fn assert_tree_unchanged_except_optional_lock(root: &Path, before: &BTreeMap<PathBuf, TreeEntry>) {
    assert!(!before.contains_key(Path::new(DEFAULT_KIT_WRITE_LOCK_PATH)));
    let mut after = tree_snapshot(root);
    let lock = after.remove(Path::new(DEFAULT_KIT_WRITE_LOCK_PATH));
    assert_eq!(&after, before);
    if let Some(lock) = lock {
        assert_eq!(
            lock.kind,
            TreeEntryKind::RegularFile(ADVISORY_LOCK_MARKER.to_vec())
        );
        #[cfg(unix)]
        assert_eq!(lock.posix_mode, Some(0o600));
    }
}

#[cfg(unix)]
fn set_fixture_coordination_directory_mode(root: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(
        root.join(KIT_COORDINATION_DIR),
        fs::Permissions::from_mode(0o700),
    )
    .expect("set fixture coordination directory mode");
}

#[cfg(not(unix))]
fn set_fixture_coordination_directory_mode(_root: &Path) {}

#[cfg(unix)]
fn coordination_modes(root: &Path) -> (u32, u32, u32) {
    use std::os::unix::fs::PermissionsExt;

    let mode = |path: &Path| {
        fs::metadata(path)
            .expect("coordination metadata")
            .permissions()
            .mode()
            & 0o7777
    };
    (
        mode(&root.join(KIT_COORDINATION_DIR)),
        mode(&root.join(KIT_GITIGNORE_PATH)),
        mode(&root.join(DEFAULT_KIT_WRITE_LOCK_PATH)),
    )
}

fn coordination_lock_identity(root: &Path) -> (u64, u64) {
    coordination_file_identity(&root.join(DEFAULT_KIT_WRITE_LOCK_PATH))
}

fn coordination_file_identity(path: &Path) -> (u64, u64) {
    let parent_path = path.parent().expect("coordination file parent");
    let name = path.file_name().expect("coordination file name");
    let parent = cap_std::fs::Dir::open_ambient_dir(parent_path, cap_std::ambient_authority())
        .unwrap_or_else(|error| {
            panic!(
                "open coordination parent {}: {error}",
                parent_path.display()
            )
        });
    let metadata = parent
        .symlink_metadata(Path::new(name))
        .unwrap_or_else(|error| panic!("inspect coordination file {}: {error}", path.display()));
    (
        cap_fs_ext::MetadataExt::dev(&metadata),
        cap_fs_ext::MetadataExt::ino(&metadata),
    )
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
