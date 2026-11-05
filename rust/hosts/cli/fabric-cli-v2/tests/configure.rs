// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline preference effects in isolated project-owned directories.

#![deny(unnameable_test_items)]

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fabric_cli_v2::{
    Control, EffectKind, EffectState, Status,
    configure::{PREFERENCES_FILE, read as read_preferences, set},
};
use serde_json::{Value, json};

struct Running;

impl Control for Running {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn deadline_elapsed(&self) -> bool {
        false
    }
}

struct Stopped(bool);

impl Control for Stopped {
    fn is_cancelled(&self) -> bool {
        self.0
    }
    fn deadline_elapsed(&self) -> bool {
        !self.0
    }
}

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = PathBuf::from(
            std::env::var_os("FAB_CLI_V2_TEST_ROOT")
                .expect("the validator must inject FAB_CLI_V2_TEST_ROOT"),
        );
        assert!(
            parent.is_absolute(),
            "the injected test root must be absolute"
        );
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!(
            "configure-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn store(&self) -> PathBuf {
        self.0.join(PREFERENCES_FILE)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn read(root: &Root) -> Value {
    serde_json::from_slice(&fs::read(root.store()).unwrap()).unwrap()
}

fn assert_no_pending(root: &Root) {
    for entry in fs::read_dir(&root.0).unwrap() {
        assert!(
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".pending")
        );
    }
}

#[test]
fn readonly_missing_roots_and_stores_never_initialize_anything() {
    let root = Root::new();
    let missing = root.0.join("missing").join("nested");
    assert!(read_preferences(&missing).unwrap().is_empty());
    assert!(!root.0.join("missing").exists());
    assert!(read_preferences(&root.0).unwrap().is_empty());
    assert!(fs::read_dir(&root.0).unwrap().next().is_none());
}

#[test]
fn readonly_snapshot_preserves_data_without_defaults_migration_or_lock_access() {
    let root = Root::new();
    let original = json!({
        "output_format": "json",
        "default_open_experience": "powerbi",
        "output_item_sort_criteria": "bytype",
        "show_hidden": "true",
        "folder_listing_enabled": "false",
        "debug_enabled": "false",
        "cache_enabled": "true",
        "fab_debug_enabled": "legacy value is not a new default",
        "identity": {"must": "remain nonauthoritative"},
        "mode": "legacy",
        "unrelated": ["é中", null, 2],
    });
    let bytes = serde_json::to_vec(&original).unwrap();
    fs::write(root.store(), &bytes).unwrap();
    fs::write(root.0.join("config.json"), b"v1 remains unread").unwrap();
    // A non-file lock path proves reads neither open nor validate that path.
    fs::create_dir(root.0.join(".preferences-v2.lock")).unwrap();
    assert_eq!(Value::Object(read_preferences(&root.0).unwrap()), original);
    assert_eq!(fs::read(root.store()).unwrap(), bytes);
    assert_eq!(
        fs::read(root.0.join("config.json")).unwrap(),
        b"v1 remains unread"
    );
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 3);
    assert_no_pending(&root);
}

#[test]
fn readonly_rejects_malformed_nonobject_oversized_and_invalid_recognized_values() {
    let root = Root::new();
    let mut cases = vec![
        b"{broken".to_vec(),
        b"[]".to_vec(),
        b"null".to_vec(),
        vec![0xff],
        vec![b' '; 64 * 1024 + 1],
    ];
    for (key, bad_values) in [
        ("output_format", vec![json!("JSON"), json!(""), json!(null)]),
        (
            "default_open_experience",
            vec![json!("fabric-developer"), json!(4)],
        ),
        ("output_item_sort_criteria", vec![json!("name"), json!([])]),
        ("show_hidden", vec![json!(true), json!("TRUE")]),
        ("folder_listing_enabled", vec![json!("yes")]),
        ("debug_enabled", vec![json!({})]),
        ("cache_enabled", vec![json!(" true")]),
    ] {
        for value in bad_values {
            let mut object = serde_json::Map::new();
            object.insert(key.into(), value);
            cases.push(serde_json::to_vec(&object).unwrap());
        }
    }
    for bytes in cases {
        fs::write(root.store(), &bytes).unwrap();
        let error = read_preferences(&root.0).unwrap_err();
        assert!(matches!(error.code, "InvalidConfig" | "BudgetExceeded"));
        assert!(error.effects.is_empty());
        assert_eq!(fs::read(root.store()).unwrap(), bytes);
        assert!(!root.0.join(".preferences-v2.lock").exists());
        assert_no_pending(&root);
    }
}

#[test]
fn readonly_rejects_relative_traversing_and_nonregular_paths_without_writes() {
    let root = Root::new();
    assert_eq!(
        read_preferences(&PathBuf::from("relative"))
            .unwrap_err()
            .code,
        "InvalidInput"
    );
    assert_eq!(
        read_preferences(&root.0.join("..").join("escape"))
            .unwrap_err()
            .code,
        "InvalidInput"
    );
    let file = root.0.join("file");
    fs::write(&file, b"sentinel").unwrap();
    assert_eq!(read_preferences(&file).unwrap_err().code, "UnsafePath");
    fs::create_dir(root.store()).unwrap();
    assert_eq!(read_preferences(&root.0).unwrap_err().code, "UnsafePath");
    assert_eq!(fs::read(&file).unwrap(), b"sentinel");
    assert!(!root.0.join(".preferences-v2.lock").exists());
}

#[test]
fn readonly_atomic_snapshots_coexist_with_replacing_writer_without_writer_locks() {
    let root = Root::new();
    set(&root.0, "/.config/output_format", "text", &Running).unwrap();
    let path = root.0.clone();
    let writer = std::thread::spawn(move || {
        for index in 0..24 {
            set(
                &path,
                "/.config/output_format",
                if index % 2 == 0 { "json" } else { "text" },
                &Running,
            )
            .unwrap();
        }
    });
    let snapshots: Vec<_> = (0..64).map(|_| read_preferences(&root.0)).collect();
    writer.join().unwrap();
    for snapshot in snapshots {
        let snapshot = snapshot.unwrap();
        assert_eq!(snapshot.len(), 1);
        assert!(matches!(
            snapshot["output_format"].as_str(),
            Some("text" | "json")
        ));
    }
    assert_no_pending(&root);
}

#[cfg(unix)]
#[test]
fn readonly_rejects_linked_roots_and_stores_without_touching_other_files() {
    use std::os::unix::fs::symlink;
    let root = Root::new();
    let directory = root.0.join("other");
    fs::create_dir(&directory).unwrap();
    let link = root.0.join("link");
    symlink(&directory, &link).unwrap();
    assert_eq!(read_preferences(&link).unwrap_err().code, "UnsafePath");
    let original = directory.join("original");
    fs::write(&original, b"{}").unwrap();
    symlink(&original, root.store()).unwrap();
    assert_eq!(read_preferences(&root.0).unwrap_err().code, "UnsafePath");
    fs::remove_file(root.store()).unwrap();
    fs::hard_link(&original, root.store()).unwrap();
    assert_eq!(read_preferences(&root.0).unwrap_err().code, "UnsafePath");
    assert_eq!(fs::read(original).unwrap(), b"{}");
    assert!(!root.0.join(".preferences-v2.lock").exists());
}

#[test]
fn all_declared_values_persist_without_v1_defaults_or_other_effects() {
    let root = Root::new();
    fs::write(root.0.join("config.json"), b"v1 sentinel").unwrap();
    let pairs = [
        ("output_format", &["text", "json"][..]),
        ("default_open_experience", &["fabric", "powerbi"][..]),
        ("output_item_sort_criteria", &["byname", "bytype"][..]),
        ("show_hidden", &["true", "false"][..]),
        ("folder_listing_enabled", &["true", "false"][..]),
        ("debug_enabled", &["true", "false"][..]),
        ("cache_enabled", &["true", "false"][..]),
    ];
    for (key, values) in pairs {
        for value in values {
            let result = set(&root.0, &format!("/.config/{key}"), value, &Running).unwrap();
            assert_eq!(result.status, Status::Succeeded);
            assert_eq!(result.effects.len(), 1);
            assert_eq!(result.effects[0].kind, EffectKind::ConfigurationWrite);
            assert_eq!(result.effects[0].state, EffectState::Completed);
            assert_eq!(read(&root)[key], *value);
        }
    }
    assert_eq!(read(&root).as_object().unwrap().len(), pairs.len());
    assert_eq!(
        fs::read(root.0.join("config.json")).unwrap(),
        b"v1 sentinel"
    );
    assert_no_pending(&root);
}

#[test]
fn preserves_unrelated_nested_unicode_and_legacy_members_without_migration() {
    let root = Root::new();
    let original = json!({
        "unrelated": {"unicode": "é中", "array": [null, 1, true]},
        "fab_debug_enabled": "false",
        "mode": "legacy",
        "identity": {"must": "remain unchanged"},
    });
    fs::write(root.store(), serde_json::to_vec(&original).unwrap()).unwrap();
    set(&root.0, "/.config/output_format", "json", &Running).unwrap();
    let actual = read(&root);
    for (key, value) in original.as_object().unwrap() {
        assert_eq!(&actual[key], value);
    }
    assert_eq!(actual["output_format"], "json");
    assert_eq!(actual.as_object().unwrap().len(), 5);
    set(&root.0, "/.config/output_format", "text", &Running).unwrap();
    assert_eq!(read(&root)["output_format"], "text");
}

#[test]
fn rejects_unknown_remote_sensitive_and_malformed_inputs_before_filesystem_access() {
    let root = Root::new();
    let absent = root.0.join("must-not-exist");
    for target in [
        "/.config/",
        "/.config/output_format/child",
        "/.config/OUTPUT_FORMAT",
        "/.config/../output_format",
        "/.config/context_persistence_enabled",
        "/.config/encryption_fallback_enabled",
        "/.config/job_cancel_ontimeout",
        "/.config/default_capacity",
        "/.config/default_az_subscription_id",
        "/.config/default_az_resource_group",
        "/.config/default_az_admin",
        "/.config/workspace_private_links_enabled",
        "/.auth/account",
        "/.profiles/default",
        "/.capacities/id/settings/spark",
        "/workspace.Workspace/settings/key",
        "config/output_format",
    ] {
        assert!(set(&absent, target, "true", &Running).is_err(), "{target}");
        assert!(!absent.exists());
    }
    for (key, values) in [
        (
            "output_format",
            &["", "JSON", " json", "json ", "'json'", "json\n"][..],
        ),
        ("show_hidden", &["TRUE", "1", "yes", "false\0"][..]),
        (
            "default_open_experience",
            &["browser", "fabric-developer"][..],
        ),
        ("output_item_sort_criteria", &["name", "byName"][..]),
    ] {
        for value in values {
            assert!(set(&absent, &format!("/.config/{key}"), value, &Running).is_err());
            assert!(!absent.exists());
        }
    }
    assert!(
        set(
            &PathBuf::from("relative-root"),
            "/.config/show_hidden",
            "true",
            &Running
        )
        .is_err()
    );
    assert!(
        set(
            &root.0.join("..").join("escape"),
            "/.config/show_hidden",
            "true",
            &Running
        )
        .is_err()
    );
}

#[test]
fn cancelled_or_expired_calls_create_nothing() {
    let root = Root::new();
    let absent = root.0.join("absent");
    for (cancelled, code) in [(true, "Cancelled"), (false, "Timeout")] {
        let error = set(
            &absent,
            "/.config/cache_enabled",
            "false",
            &Stopped(cancelled),
        )
        .unwrap_err();
        assert_eq!(error.code, code);
        assert!(error.effects.is_empty());
        assert!(!absent.exists());
    }
}

#[test]
fn malformed_nonobject_and_oversized_stores_are_not_replaced() {
    let root = Root::new();
    for bytes in [
        b"{broken".to_vec(),
        b"[]".to_vec(),
        b"null".to_vec(),
        vec![0xff],
        vec![b' '; 64 * 1024 + 1],
    ] {
        fs::write(root.store(), &bytes).unwrap();
        let error = set(&root.0, "/.config/show_hidden", "true", &Running).unwrap_err();
        assert!(matches!(error.code, "InvalidConfig" | "BudgetExceeded"));
        assert_eq!(fs::read(root.store()).unwrap(), bytes);
        assert_no_pending(&root);
    }
}

#[test]
fn valid_store_whose_encoded_replacement_is_oversized_is_preserved() {
    let root = Root::new();
    let bytes = serde_json::to_vec(&json!({"unrelated": "x".repeat(64 * 1024 - 20)})).unwrap();
    assert!(bytes.len() <= 64 * 1024);
    fs::write(root.store(), &bytes).unwrap();
    assert_eq!(
        set(&root.0, "/.config/show_hidden", "true", &Running)
            .unwrap_err()
            .code,
        "BudgetExceeded",
    );
    assert_eq!(fs::read(root.store()).unwrap(), bytes);
}

#[test]
fn lock_contention_fails_without_writes_and_os_release_allows_retry() {
    let root = Root::new();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.0.join(".preferences-v2.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let error = set(&root.0, "/.config/debug_enabled", "true", &Running).unwrap_err();
    assert_eq!(error.code, "ConfigurationBusy");
    assert!(!root.store().exists());
    drop(lock);
    set(&root.0, "/.config/debug_enabled", "true", &Running).unwrap();
    assert_eq!(read(&root)["debug_enabled"], "true");
    assert_no_pending(&root);
}

#[test]
fn nondirectory_roots_and_nonregular_store_or_lock_paths_fail_explicitly() {
    let root = Root::new();
    let file = root.0.join("file");
    fs::write(&file, b"sentinel").unwrap();
    assert_eq!(
        set(&file, "/.config/cache_enabled", "true", &Running)
            .unwrap_err()
            .code,
        "UnsafePath"
    );
    fs::create_dir(root.store()).unwrap();
    assert_eq!(
        set(&root.0, "/.config/cache_enabled", "true", &Running)
            .unwrap_err()
            .code,
        "UnsafePath"
    );
    fs::remove_dir(root.store()).unwrap();
    fs::remove_file(root.0.join(".preferences-v2.lock")).unwrap();
    fs::create_dir(root.0.join(".preferences-v2.lock")).unwrap();
    assert_eq!(
        set(&root.0, "/.config/cache_enabled", "true", &Running)
            .unwrap_err()
            .code,
        "UnsafePath"
    );
    assert_eq!(fs::read(&file).unwrap(), b"sentinel");
}

struct CancelAt {
    root: PathBuf,
    committed: bool,
}

impl Control for CancelAt {
    fn is_cancelled(&self) -> bool {
        if self.committed {
            self.root.join(PREFERENCES_FILE).exists()
        } else {
            fs::read_dir(&self.root).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".pending")
            })
        }
    }
    fn deadline_elapsed(&self) -> bool {
        false
    }
}

#[test]
fn cancellation_before_commit_cleans_staging_and_after_commit_retains_evidence() {
    let root = Root::new();
    let error = set(
        &root.0,
        "/.config/cache_enabled",
        "false",
        &CancelAt {
            root: root.0.clone(),
            committed: false,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "Cancelled");
    assert!(error.effects.is_empty());
    assert!(!root.store().exists());
    assert_no_pending(&root);
    let error = set(
        &root.0,
        "/.config/cache_enabled",
        "false",
        &CancelAt {
            root: root.0.clone(),
            committed: true,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "Cancelled");
    assert_eq!(error.effects.len(), 1);
    assert_eq!(error.effects[0].kind, EffectKind::ConfigurationWrite);
    assert_eq!(error.effects[0].state, EffectState::Completed);
    assert_eq!(read(&root)["cache_enabled"], "false");
    assert_no_pending(&root);
}

#[cfg(unix)]
#[test]
fn linked_root_store_and_lock_are_rejected_without_touching_the_target() {
    use std::os::unix::fs::symlink;
    let root = Root::new();
    let outside = root.0.join("other");
    fs::create_dir(&outside).unwrap();
    let link = root.0.join("link");
    symlink(&outside, &link).unwrap();
    assert_eq!(
        set(&link, "/.config/debug_enabled", "true", &Running)
            .unwrap_err()
            .code,
        "UnsafePath"
    );
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
    let sentinel = outside.join("sentinel");
    fs::write(&sentinel, b"untouched").unwrap();
    for linked in [root.store(), root.0.join(".preferences-v2.lock")] {
        let lock_path = root.0.join(".preferences-v2.lock");
        if lock_path.exists() {
            fs::remove_file(lock_path).unwrap();
        }
        symlink(&sentinel, &linked).unwrap();
        assert_eq!(
            set(&root.0, "/.config/debug_enabled", "true", &Running)
                .unwrap_err()
                .code,
            "UnsafePath"
        );
        fs::remove_file(linked).unwrap();
    }
    assert_eq!(fs::read(sentinel).unwrap(), b"untouched");
}

#[cfg(unix)]
#[test]
fn new_files_are_private_and_hardlinked_preferences_are_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let root = Root::new();
    let new_root = root.0.join("private");
    set(&new_root, "/.config/show_hidden", "true", &Running).unwrap();
    assert_eq!(
        fs::metadata(&new_root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(new_root.join(PREFERENCES_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::write(root.store(), b"{}").unwrap();
    fs::hard_link(root.store(), root.0.join("alias")).unwrap();
    assert_eq!(
        set(&root.0, "/.config/show_hidden", "true", &Running)
            .unwrap_err()
            .code,
        "UnsafePath"
    );
    assert_eq!(fs::read(root.store()).unwrap(), b"{}");
}

#[cfg(windows)]
#[test]
fn blocked_atomic_replacement_retains_uncertain_effect_and_cleans_staging() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = Root::new();
    fs::write(root.store(), b"{}").unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(root.store())
        .unwrap();
    let error = set(&root.0, "/.config/show_hidden", "true", &Running).unwrap_err();
    assert_eq!(error.code, "LocalIOError");
    assert_eq!(error.effects[0].kind, EffectKind::ConfigurationWrite);
    assert_eq!(error.effects[0].state, EffectState::Unknown);
    assert_eq!(fs::read(root.store()).unwrap(), b"{}");
    assert_no_pending(&root);
    drop(held);
    set(&root.0, "/.config/show_hidden", "true", &Running).unwrap();
}
