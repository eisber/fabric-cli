// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline exact-file/no-clobber contracts using private disposable directories.

#![deny(unnameable_test_items)]

use fabric_cli_storage_files::{
    ExactOutputError, ExactOutputErrorKind, ExactOutputFile, ExactOutputPathError, ExactOutputStage,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::current_dir().unwrap();
        loop {
            let root = base.join(format!(
                ".exact-output-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Self { root },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create private test directory: {error}"),
            }
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove only this test's private directory");
    }
}

fn untouched(error: &ExactOutputError, path: &Path) {
    assert_eq!(error.path.as_os_str(), path.as_os_str());
    assert!(!error.path_created);
    assert_eq!(error.bytes_written, 0);
}

#[test]
fn exact_binary_bytes_and_original_filename_survive_without_added_entries() {
    let fixture = Fixture::new();
    let path = fixture.path("chosen.Name.without-rewriting");
    let bytes: Vec<u8> = (0_u8..=255).cycle().take(32 * 1024 + 1).collect();
    let output = ExactOutputFile::preflight(&path, bytes.len()).unwrap();
    assert_eq!(output.path().as_os_str(), path.as_os_str());
    assert_eq!(output.max_bytes(), bytes.len());
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);

    let receipt = output.commit(&bytes).unwrap();
    assert_eq!(receipt.path.as_os_str(), path.as_os_str());
    assert_eq!(receipt.bytes_written, bytes.len());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let entries: Vec<_> = fs::read_dir(&fixture.root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, [path.file_name().unwrap().to_os_string()]);
}

#[test]
fn empty_bytes_create_an_empty_file_even_with_a_zero_byte_cap() {
    let fixture = Fixture::new();
    let path = fixture.path("empty");
    let receipt = ExactOutputFile::preflight(&path, 0)
        .unwrap()
        .commit(&[])
        .unwrap();
    assert_eq!(receipt.path, path);
    assert_eq!(receipt.bytes_written, 0);
    assert!(fs::metadata(&path).unwrap().is_file());
    assert!(fs::read(&path).unwrap().is_empty());
}

#[test]
fn existing_file_is_rejected_without_changing_content() {
    let fixture = Fixture::new();
    let path = fixture.path("existing");
    fs::write(&path, b"preserve original").unwrap();
    let error = ExactOutputFile::preflight(&path, 64).unwrap_err();
    assert_eq!(error.kind, ExactOutputErrorKind::OutputExists);
    assert_eq!(error.stage, ExactOutputStage::Preflight);
    untouched(&error, &path);
    assert_eq!(fs::read(&path).unwrap(), b"preserve original");
}

#[test]
fn file_created_by_a_competitor_after_preflight_is_not_clobbered() {
    let fixture = Fixture::new();
    let path = fixture.path("raced");
    let output = ExactOutputFile::preflight(&path, 64).unwrap();
    fs::write(&path, b"competitor wins").unwrap();

    let error = output.commit(b"must not replace").unwrap_err();
    assert_eq!(error.kind, ExactOutputErrorKind::OutputExists);
    assert_eq!(error.stage, ExactOutputStage::Create);
    assert!(error.io_error.is_some());
    untouched(&error, &path);
    assert_eq!(fs::read(&path).unwrap(), b"competitor wins");
}

#[test]
fn existing_directory_is_not_used_as_a_destination_directory() {
    let fixture = Fixture::new();
    let path = fixture.path("directory");
    fs::create_dir(&path).unwrap();
    let error = ExactOutputFile::preflight(&path, 32).unwrap_err();
    assert_eq!(error.kind, ExactOutputErrorKind::OutputExists);
    untouched(&error, &path);
    assert!(fs::metadata(&path).unwrap().is_dir());
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
}

#[test]
fn an_existing_directory_with_a_trailing_separator_is_still_output_exists() {
    let fixture = Fixture::new();
    let mut spelling = fixture.root.clone().into_os_string();
    spelling.push(std::path::MAIN_SEPARATOR_STR);
    let path = PathBuf::from(spelling);
    let error = ExactOutputFile::preflight(&path, 8).unwrap_err();
    assert_eq!(error.kind, ExactOutputErrorKind::OutputExists);
    untouched(&error, &path);
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[test]
fn missing_parent_is_reported_without_creating_any_directory() {
    let fixture = Fixture::new();
    let path = fixture.path("missing").join("file");
    let error = ExactOutputFile::preflight(&path, 16).unwrap_err();
    assert_eq!(error.kind, ExactOutputErrorKind::MissingParent);
    assert_eq!(error.stage, ExactOutputStage::Preflight);
    untouched(&error, &path);
    assert!(!fixture.path("missing").exists());
}

#[test]
fn a_file_cannot_be_used_as_an_output_parent() {
    let fixture = Fixture::new();
    let parent = fixture.path("parent-file");
    fs::write(&parent, b"keep parent").unwrap();
    let path = parent.join("file");
    let error = ExactOutputFile::preflight(&path, 16).unwrap_err();
    assert_eq!(error.kind, ExactOutputErrorKind::ParentNotDirectory);
    untouched(&error, &path);
    assert_eq!(fs::read(&parent).unwrap(), b"keep parent");
}

#[test]
fn removing_the_parent_after_preflight_reports_create_failure_without_rollback() {
    let fixture = Fixture::new();
    let parent = fixture.path("parent");
    fs::create_dir(&parent).unwrap();
    let path = parent.join("file");
    let output = ExactOutputFile::preflight(&path, 16).unwrap();
    fs::remove_dir(&parent).unwrap();
    let error = output.commit(b"bytes").unwrap_err();
    assert_eq!(error.kind, ExactOutputErrorKind::MissingParent);
    assert_eq!(error.stage, ExactOutputStage::Create);
    untouched(&error, &path);
    assert!(!parent.exists());
}

#[test]
fn exact_cap_is_allowed_and_one_byte_over_fails_before_creation() {
    let fixture = Fixture::new();
    for cap in [0, 1, 16 * 1024] {
        let exact = fixture.path(&format!("exact-{cap}"));
        let bytes = vec![255; cap];
        let receipt = ExactOutputFile::preflight(&exact, cap)
            .unwrap()
            .commit(&bytes)
            .unwrap();
        assert_eq!(receipt.bytes_written, cap);
        assert_eq!(fs::read(&exact).unwrap(), bytes);

        let oversized = fixture.path(&format!("oversized-{cap}"));
        let error = ExactOutputFile::preflight(&oversized, cap)
            .unwrap()
            .commit(&vec![0; cap + 1])
            .unwrap_err();
        assert_eq!(
            error.kind,
            ExactOutputErrorKind::ByteLimit {
                limit: cap,
                actual: cap + 1,
            }
        );
        assert_eq!(error.stage, ExactOutputStage::Limit);
        untouched(&error, &oversized);
        assert!(!oversized.exists());
    }
}

#[test]
fn relative_empty_and_service_paths_are_not_resolved_by_the_api() {
    for path in [
        Path::new(""),
        Path::new("relative.bin"),
        Path::new("../relative.bin"),
        Path::new("abfss://workspace@onelake.dfs.fabric.microsoft.com/file"),
        Path::new("https://example.invalid/output"),
    ] {
        let error = ExactOutputFile::preflight(path, 8).unwrap_err();
        assert!(matches!(error.kind, ExactOutputErrorKind::InvalidPath(_)));
        assert_eq!(error.stage, ExactOutputStage::Preflight);
        untouched(&error, path);
    }
}

#[test]
fn unresolved_parent_and_current_directory_components_are_rejected() {
    let fixture = Fixture::new();
    let mut current = fixture.root.clone().into_os_string();
    current.push(std::path::MAIN_SEPARATOR_STR);
    current.push(".");
    current.push(std::path::MAIN_SEPARATOR_STR);
    current.push("file");
    for path in [
        fixture.path("child").join("..").join("file"),
        PathBuf::from(current),
    ] {
        let error = ExactOutputFile::preflight(&path, 8).unwrap_err();
        assert_eq!(
            error.kind,
            ExactOutputErrorKind::InvalidPath(ExactOutputPathError::Unresolved)
        );
        untouched(&error, &path);
    }
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[test]
fn trailing_separator_is_not_silently_removed_from_a_file_destination() {
    let fixture = Fixture::new();
    let mut spelling = fixture.path("not-a-directory").into_os_string();
    spelling.push(std::path::MAIN_SEPARATOR_STR);
    let path = PathBuf::from(spelling);
    let error = ExactOutputFile::preflight(&path, 8).unwrap_err();
    assert_eq!(
        error.kind,
        ExactOutputErrorKind::InvalidPath(ExactOutputPathError::InvalidFileName)
    );
    untouched(&error, &path);
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[test]
fn embedded_nul_is_an_explicit_invalid_filename() {
    let fixture = Fixture::new();
    let path = fixture.path("invalid\0name");
    let error = ExactOutputFile::preflight(&path, 8).unwrap_err();
    assert_eq!(
        error.kind,
        ExactOutputErrorKind::InvalidPath(ExactOutputPathError::InvalidFileName)
    );
    untouched(&error, &path);
}

#[cfg(windows)]
#[test]
fn windows_drive_relative_root_relative_share_and_device_paths_are_rejected() {
    for (spelling, reason) in [
        (r"Q:relative", ExactOutputPathError::NotAbsolute),
        (r"\root-relative", ExactOutputPathError::NotAbsolute),
        (r"\\server\share\file", ExactOutputPathError::Remote),
        (r"\\?\UNC\server\share\file", ExactOutputPathError::Remote),
        (r"\\.\NUL", ExactOutputPathError::Device),
        (r"\\?\GLOBALROOT\Device\file", ExactOutputPathError::Device),
    ] {
        let path = Path::new(spelling);
        let error = ExactOutputFile::preflight(path, 8).unwrap_err();
        assert_eq!(error.kind, ExactOutputErrorKind::InvalidPath(reason));
        untouched(&error, path);
    }
}

#[cfg(windows)]
#[test]
fn windows_reserved_names_streams_and_lossy_filename_aliases_are_rejected() {
    let fixture = Fixture::new();
    for name in [
        "NUL", "NUL.txt", "con", "CONOUT$", "COM1.dat", "LPT9", "COM¹", "CON .txt",
    ] {
        let path = fixture.path(name);
        let error = ExactOutputFile::preflight(&path, 8).unwrap_err();
        assert_eq!(
            error.kind,
            ExactOutputErrorKind::InvalidPath(ExactOutputPathError::Device)
        );
        untouched(&error, &path);
    }
    for name in ["file:stream", "file.", "file ", "bad?name", "bad|name"] {
        let path = fixture.path(name);
        let error = ExactOutputFile::preflight(&path, 8).unwrap_err();
        assert_eq!(
            error.kind,
            ExactOutputErrorKind::InvalidPath(ExactOutputPathError::InvalidFileName)
        );
        untouched(&error, &path);
    }
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[cfg(any(unix, windows))]
fn file_symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_file(target, link);
    match result {
        Ok(()) => true,
        #[cfg(windows)]
        Err(error)
            if error.kind() == io::ErrorKind::PermissionDenied
                || error.raw_os_error() == Some(1314) =>
        {
            eprintln!("Windows symlink fixture unavailable without symlink privilege: {error}");
            false
        }
        Err(error) => panic!("cannot create test-owned symlink: {error}"),
    }
}

#[cfg(any(unix, windows))]
#[test]
fn existing_and_dangling_symlinks_are_rejected_without_following_them() {
    let fixture = Fixture::new();
    let target = fixture.path("target");
    fs::write(&target, b"keep target").unwrap();
    for (name, destination) in [
        ("existing-link", target.clone()),
        ("dangling-link", fixture.path("missing-target")),
    ] {
        let link = fixture.path(name);
        if !file_symlink(&destination, &link) {
            return;
        }
        let error = ExactOutputFile::preflight(&link, 8).unwrap_err();
        assert_eq!(error.kind, ExactOutputErrorKind::OutputExists);
        untouched(&error, &link);
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    }
    assert_eq!(fs::read(&target).unwrap(), b"keep target");
    assert!(!fixture.path("missing-target").exists());
}

#[cfg(any(unix, windows))]
#[test]
fn symlink_created_after_preflight_is_not_followed_or_replaced() {
    let fixture = Fixture::new();
    let target = fixture.path("target");
    fs::write(&target, b"keep target").unwrap();
    let path = fixture.path("raced-link");
    let output = ExactOutputFile::preflight(&path, 32).unwrap();
    if !file_symlink(&target, &path) {
        return;
    }
    let error = output.commit(b"must not replace").unwrap_err();
    assert_eq!(error.stage, ExactOutputStage::Create);
    untouched(&error, &path);
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
    assert_eq!(fs::read(&target).unwrap(), b"keep target");
}
