// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

mod support;

use fabric_cli_storage_files::{LocalErrorKind, LocalFileSystem, LocalKind, SelectedFileSystem};
use std::{fs, num::NonZeroUsize, path::Path};
use support::Fixture;

#[tokio::test]
async fn selected_path_is_deferred_and_sibling_paths_never_gain_boundary_authority() {
    let fixture = Fixture::new();
    let missing_root = fixture.path().join("not-created");
    let selected = missing_root.join("target");
    let files = SelectedFileSystem::defer(
        missing_root.clone(),
        selected.clone(),
        tokio::runtime::Handle::current(),
    );
    assert_eq!(files.selected(), selected);
    drop(files.write(&selected, b"unpolled".to_vec()));
    assert!(!missing_root.exists());
    let error = files.write(&selected, b"polled".to_vec()).await.unwrap_err();
    assert!(matches!(error.kind, LocalErrorKind::Io(_)));
    assert!(!missing_root.exists());

    let selected = fixture.path().join("selected");
    let neighbor = fixture.path().join("neighbor");
    fs::write(&neighbor, b"keep").unwrap();
    let files = SelectedFileSystem::defer(
        fixture.path().into(),
        selected.clone(),
        tokio::runtime::Handle::current(),
    );
    for error in [
        files.kind(&neighbor).await.unwrap_err(),
        files
            .read(&neighbor, NonZeroUsize::new(8).unwrap())
            .await
            .unwrap_err(),
        files.write(&neighbor, b"replace".to_vec()).await.unwrap_err(),
        files
            .write(&selected.join("nested").join("target"), b"replace".to_vec())
            .await
            .unwrap_err(),
    ] {
        assert!(matches!(error.kind, LocalErrorKind::OutsideBoundary));
        assert!(!error.destination_opened);
    }
    assert_eq!(fs::read(neighbor).unwrap(), b"keep");
    assert!(!selected.exists());
}

#[tokio::test]
async fn explicit_relative_alias_keeps_spelling_without_discovering_cwd_or_allowing_empty_input() {
    let fixture = Fixture::new();
    let file = fixture.path().join("selected");
    fs::write(&file, b"binary").unwrap();
    let files = SelectedFileSystem::defer(
        fixture.path().into(),
        file.clone(),
        tokio::runtime::Handle::current(),
    )
    .with_input("alias".into());
    assert_eq!(
        files
            .read(Path::new("alias"), NonZeroUsize::new(6).unwrap())
            .await
            .unwrap(),
        b"binary"
    );
    assert!(files.kind(&file).await.is_err());
    assert!(files.kind(Path::new("")).await.is_err());
    let files = files.with_input("".into());
    assert!(
        files
            .write(Path::new(""), b"replace".to_vec())
            .await
            .is_err()
    );
    assert_eq!(fs::read(file).unwrap(), b"binary");
}

#[tokio::test]
async fn selected_directory_allows_one_native_basename_but_never_traversal_or_parent_creation() {
    let fixture = Fixture::new();
    let selected = fixture.path().join("directory");
    fs::create_dir(&selected).unwrap();
    let files = SelectedFileSystem::defer(
        fixture.path().into(),
        selected.clone(),
        tokio::runtime::Handle::current(),
    )
    .with_input("chosen".into());
    assert_eq!(
        files.kind(Path::new("chosen")).await.unwrap(),
        LocalKind::Directory
    );
    let child = Path::new("chosen").join("payload");
    assert_eq!(files.write(&child, b"\0\xff".to_vec()).await.unwrap(), 2);
    assert_eq!(fs::read(selected.join("payload")).unwrap(), b"\0\xff");
    for path in [
        Path::new("chosen").join("..").join("escape"),
        Path::new("chosen").join("nested").join("payload"),
    ] {
        assert!(
            files
                .write(&path, b"must-not-write".to_vec())
                .await
                .is_err()
        );
    }
    assert!(!fixture.path().join("escape").exists());
    assert!(!selected.join("nested").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn selected_paths_retain_existing_symlink_containment_policy() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = Fixture::new();
    fs::write(fixture.path().join("data"), b"inside").unwrap();
    fs::write(outside.path().join("data"), b"outside").unwrap();
    for (name, destination, accepted) in [
        ("inside", fixture.path().join("data"), true),
        ("outside", outside.path().join("data"), false),
    ] {
        let selected = fixture.path().join(name);
        symlink(destination, &selected).unwrap();
        let files = SelectedFileSystem::defer(
            fixture.path().into(),
            selected.clone(),
            tokio::runtime::Handle::current(),
        );
        let result = files.read(&selected, NonZeroUsize::new(8).unwrap()).await;
        if accepted {
            assert_eq!(result.unwrap(), b"inside");
        } else {
            assert!(matches!(
                result.unwrap_err().kind,
                LocalErrorKind::OutsideBoundary
            ));
        }
    }
}
