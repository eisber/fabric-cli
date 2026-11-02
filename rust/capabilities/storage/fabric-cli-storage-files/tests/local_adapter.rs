// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real local failure/path evidence over the injected OS adapter.

mod support;

use fabric_cli_storage_files::{LocalErrorKind, LocalFileSystem, LocalKind, OsFileSystem};
use std::{fs, io, num::NonZeroUsize, path::Path};
use support::Fixture;

fn limit(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

#[tokio::test]
async fn binary_reads_match_exact_bounds_on_both_sides_of_native_chunk_size() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let path = fixture.path().join("source.bin");
    for length in [
        0,
        1,
        16 * 1024 - 1,
        16 * 1024,
        16 * 1024 + 1,
        4 * 1024 * 1024,
    ] {
        let bytes: Vec<_> = (0..length).map(|index| (index % 256) as u8).collect();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            local.read(&path, limit(length.max(1))).await.unwrap(),
            bytes
        );
        if length > 1 {
            let failure = local.read(&path, limit(length - 1)).await.unwrap_err();
            assert!(matches!(failure.kind, LocalErrorKind::Limit));
            assert!(!failure.destination_opened);
            assert_eq!(failure.bytes_written, 0);
        }
    }
}

#[tokio::test]
async fn native_missing_source_and_directory_reads_fail_without_creating_any_path() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let missing = fixture.path().join("missing");
    let failure = local.read(&missing, limit(8)).await.unwrap_err();
    assert!(
        matches!(failure.kind, LocalErrorKind::Io(ref error) if error.kind() == io::ErrorKind::NotFound)
    );
    assert!(!missing.exists());
    assert_eq!(
        local.kind(fixture.path()).await.unwrap(),
        LocalKind::Directory
    );
    let failure = local.read(fixture.path(), limit(8)).await.unwrap_err();
    assert!(matches!(failure.kind, LocalErrorKind::Io(_)));
    assert!(!failure.destination_opened);
}

#[tokio::test]
async fn real_binary_writes_truncate_only_selected_file_and_do_not_create_parents() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    fs::write(fixture.path().join("keep"), b"unrelated").unwrap();
    let target = fixture.path().join("target");
    fs::write(&target, b"old-longer-data").unwrap();
    for bytes in [vec![0, 255, 13, 10], vec![]] {
        assert_eq!(
            local.write(&target, bytes.clone()).await.unwrap(),
            bytes.len()
        );
        assert_eq!(fs::read(&target).unwrap(), bytes);
    }
    let missing = fixture.path().join("missing").join("target");
    let failure = local.write(&missing, b"new".to_vec()).await.unwrap_err();
    assert!(
        matches!(failure.kind, LocalErrorKind::Io(ref error) if error.kind() == io::ErrorKind::NotFound)
    );
    assert!(!failure.destination_opened);
    assert_eq!(failure.bytes_written, 0);
    assert!(!fixture.path().join("missing").exists());
    assert_eq!(fs::read(fixture.path().join("keep")).unwrap(), b"unrelated");
}

#[tokio::test]
async fn relative_paths_use_the_explicit_root_without_changing_process_cwd() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let before = std::env::current_dir().unwrap();
    fs::write(fixture.path().join("source"), b"binary").unwrap();
    assert_eq!(
        local.read(Path::new("source"), limit(6)).await.unwrap(),
        b"binary"
    );
    assert_eq!(
        local
            .write(Path::new("target"), b"new".to_vec())
            .await
            .unwrap(),
        3
    );
    assert_eq!(fs::read(fixture.path().join("target")).unwrap(), b"new");
    assert_eq!(std::env::current_dir().unwrap(), before);
}

#[tokio::test]
async fn empty_native_paths_are_not_reinterpreted_as_the_selected_root_directory() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let empty = Path::new("");
    for error in [
        local.kind(empty).await.unwrap_err(),
        local.read(empty, limit(8)).await.unwrap_err(),
        local
            .write(empty, b"must not write".to_vec())
            .await
            .unwrap_err(),
    ] {
        assert!(
            matches!(error.kind, LocalErrorKind::Io(ref cause) if cause.kind() == io::ErrorKind::NotFound)
        );
        assert!(!error.destination_opened);
        assert_eq!(error.bytes_written, 0);
    }
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn invalid_boundary_is_explicit_and_does_not_create_the_selected_directory() {
    let fixture = Fixture::new();
    let file = fixture.path().join("file");
    fs::write(&file, b"keep").unwrap();
    for root in [file.clone(), fixture.path().join("missing")] {
        let error = OsFileSystem::new(root, tokio::runtime::Handle::current())
            .await
            .unwrap_err();
        assert!(matches!(error.kind, LocalErrorKind::Io(_)));
    }
    assert_eq!(fs::read(file).unwrap(), b"keep");
    assert!(!fixture.path().join("missing").exists());
}

#[tokio::test]
async fn dropping_an_unpolled_local_write_never_starts_a_blocking_worker() {
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let target = fixture.path().join("target");
    fs::write(&target, b"keep").unwrap();
    drop(local.write(&target, b"replace".to_vec()));
    assert_eq!(fs::read(target).unwrap(), b"keep");
}

#[cfg(windows)]
#[tokio::test]
async fn native_exclusive_file_handle_surfaces_read_and_write_failure_without_truncation() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let target = fixture.path().join("locked");
    fs::write(&target, b"keep").unwrap();
    let guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&target)
        .unwrap();
    let read_error = local.read(&target, limit(8)).await.unwrap_err();
    let write_error = local.write(&target, b"replace".to_vec()).await.unwrap_err();
    for error in [read_error, write_error] {
        assert!(
            matches!(error.kind, LocalErrorKind::Io(ref native) if native.raw_os_error() == Some(32))
        );
        assert!(!error.destination_opened);
        assert_eq!(error.bytes_written, 0);
    }
    drop(guard);
    assert_eq!(fs::read(target).unwrap(), b"keep");
}

#[cfg(windows)]
#[tokio::test]
async fn copy_read_retains_native_source_handle_until_content_guard_drops() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let local = fixture.adapter().await;
    let source = fixture.path().join("source");
    fs::write(&source, b"exact bytes").unwrap();
    let content = local.read_for_copy(&source, limit(11)).await.unwrap();
    assert_eq!(content.content(), b"exact bytes");
    assert!(
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&source)
            .is_err()
    );
    drop(content);
    assert!(
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&source)
            .is_ok()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_sources_follow_native_metadata_but_cannot_escape_the_selected_root() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let local = fixture.adapter().await;
    fs::write(fixture.path().join("source"), b"inside").unwrap();
    fs::write(outside.path().join("source"), b"outside").unwrap();
    symlink(
        fixture.path().join("source"),
        fixture.path().join("inside-link"),
    )
    .unwrap();
    symlink(
        outside.path().join("source"),
        fixture.path().join("outside-link"),
    )
    .unwrap();
    assert_eq!(
        local.kind(Path::new("inside-link")).await.unwrap(),
        LocalKind::File
    );
    assert_eq!(
        local
            .read(Path::new("inside-link"), limit(6))
            .await
            .unwrap(),
        b"inside"
    );
    assert!(matches!(
        local
            .read(Path::new("outside-link"), limit(8))
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::OutsideBoundary
    ));
}
