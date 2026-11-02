// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! External consumers of pure copy policy, without SDK clients or credential I/O.

use fabric_cli_context::ItemType;
use fabric_cli_mutations::{
    OneLakeResourceKind, OneLakeWriteTarget, file_destination, validate_copy_destination,
};
use fabric_sdk_onelake::OneLakePath;

fn target(path: &str, kind: OneLakeResourceKind) -> OneLakeWriteTarget {
    OneLakeWriteTarget {
        path: OneLakePath::new("workspace", "item", path).unwrap(),
        item_type: ItemType::parse("Lakehouse").unwrap(),
        kind,
        display_path: format!("/Team.Workspace/Lake.Lakehouse/{path}"),
    }
}

#[test]
fn public_policy_is_pure_and_resolution_preserves_the_original_target() {
    let destination = target("Files/nested", OneLakeResourceKind::Shortcut);
    validate_copy_destination(&destination).unwrap();
    let file = file_destination(&destination, "data.csv").unwrap();
    assert_eq!(file.path.relative(), "Files/nested/data.csv");
    assert_eq!(
        file.display_path,
        "/Team.Workspace/Lake.Lakehouse/Files/nested/data.csv"
    );
    assert_eq!(file.kind, OneLakeResourceKind::File);
    assert_eq!(destination.kind, OneLakeResourceKind::Shortcut);
    assert_eq!(destination.path.relative(), "Files/nested");
}

#[test]
fn writable_validation_and_file_resolution_are_separate_source_ordered_steps() {
    let table = target("Files/table", OneLakeResourceKind::Table);
    validate_copy_destination(&table).unwrap();
    assert_eq!(
        file_destination(&table, "source.txt")
            .unwrap_err()
            .output()
            .error_code
            .as_deref(),
        Some("InvalidPath")
    );
    let invalid_root = target("Tables/target", OneLakeResourceKind::File);
    assert_eq!(
        validate_copy_destination(&invalid_root)
            .unwrap_err()
            .output()
            .error_code
            .as_deref(),
        Some("NotSupported")
    );
    // The filename helper is deliberately not a second writable-policy check.
    assert_eq!(
        file_destination(&invalid_root, "ignored")
            .unwrap()
            .path
            .relative(),
        "Tables/target"
    );
}

#[test]
fn bounded_source_name_rejection_precedes_any_read_and_does_not_apply_to_direct_file_targets() {
    for name in ["", "path/child"] {
        assert!(
            file_destination(
                &target("Files/folder", OneLakeResourceKind::Directory),
                name
            )
            .is_err()
        );
        let direct =
            file_destination(&target("Files/file", OneLakeResourceKind::File), name).unwrap();
        assert_eq!(direct.path.relative(), "Files/file");
    }
}
