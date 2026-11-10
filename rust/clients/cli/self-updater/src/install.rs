// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use ring::digest;

use crate::{
    Control, DownloadedUpdate, Error, InstallContext, InstallCustody, Installer,
    model::check_control,
};

/// Caller-owned non-running standalone installation, never a package-manager path.
///
/// The caller must exclusively control the directory and ensure all writers obey
/// the same lock. This adapter does not defend against a malicious same-user
/// writer or elevated process racing filesystem names. No ambient PATH search,
/// permissions escalation, helper process, or downloaded executable is used.
#[derive(Clone, Debug)]
pub struct FileInstall {
    pub(crate) path: PathBuf,
    pub(crate) expected_current_sha256: [u8; 32],
    pub(crate) nonce: [u8; 32],
}

impl FileInstall {
    /// Pin the absolute existing file, its current digest, and a fresh lock nonce.
    ///
    /// Generate the nonce in the host, independently for each install attempt.
    /// Construction is pure; filesystem ownership is checked only when installing.
    pub fn new(
        path: PathBuf,
        expected_current_sha256: [u8; 32],
        nonce: [u8; 32],
    ) -> Result<Self, Error> {
        if !path.is_absolute()
            || path.file_name().is_none()
            || path.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            || nonce == [0; 32]
        {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self {
            path,
            expected_current_sha256,
            nonce,
        })
    }

    /// Deterministic retained rollback location. Existing files are never removed.
    pub fn rollback_path(&self) -> PathBuf {
        sibling(&self.path, ".update-rollback")
    }

    /// Cooperative exclusive lock location.
    pub fn lock_path(&self) -> PathBuf {
        sibling(&self.path, ".update-lock")
    }

    /// Same-directory staging location, never a global temporary directory.
    pub fn stage_path(&self) -> PathBuf {
        sibling(&self.path, ".update-stage")
    }
}

/// Evidence for an actual atomic namespace replacement, not an update download.
///
/// A retained hard link protects the previous bytes. Power-loss durability of the
/// directory entry is platform/filesystem dependent and is not certified here.
#[derive(Debug)]
pub struct FileInstallReceipt {
    /// The actual replaced absolute path.
    pub installed_path: PathBuf,
    /// Retained previous file. The updater never automatically deletes it.
    pub rollback_path: PathBuf,
    /// SHA-256 of installed bytes.
    pub installed_sha256: [u8; 32],
    /// SHA-256 of retained previous bytes.
    pub previous_sha256: [u8; 32],
    /// A committed install whose exact owned lock could not be removed.
    ///
    /// This is cleanup debt, not a claim that replacement was rolled back.
    pub cleanup_pending: Option<PathBuf>,
}

/// Real non-running-file adapter using same-directory staging and atomic rename.
///
/// Running executables are rejected on every platform, including Unix. Windows
/// locked-executable helpers need a separate approved adapter and acceptance
/// evidence; this adapter does not pretend a rename workaround provides one.
pub struct FileInstaller {
    installation: FileInstall,
}

impl FileInstaller {
    /// Construct without reading files or changing process-global state.
    pub fn new(installation: FileInstall) -> Self {
        Self { installation }
    }
}

impl Installer for FileInstaller {
    type Receipt = FileInstallReceipt;

    fn install(
        &mut self,
        update: &DownloadedUpdate,
        context: InstallContext,
        control: &dyn Control,
    ) -> Result<Self::Receipt, Error> {
        if context.custody != InstallCustody::Standalone {
            return Err(Error::Custody);
        }
        update.require_major_policy(context.allow_major)?;
        if context.budget_ms == 0 || context.budget_ms > 120_000 {
            return Err(Error::InvalidLimits);
        }
        let start = control.snapshot().monotonic_ms;
        let deadline = start
            .checked_add(context.budget_ms)
            .ok_or(Error::Deadline)?;
        let checkpoint = || check_control(control, start, deadline).map(|_| ());
        checkpoint()?;
        reject_links(&self.installation.path)?;
        let actual = fs::canonicalize(&self.installation.path).map_err(|_| Error::Io)?;
        let running = std::env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(|_| Error::Io)?;
        if actual == running {
            return Err(Error::RunningExecutable);
        }
        let metadata = fs::symlink_metadata(&self.installation.path).map_err(|_| Error::Io)?;
        if !metadata.is_file() || metadata.permissions().readonly() {
            return Err(Error::Custody);
        }
        let lock_path = self.installation.lock_path();
        let mut lock = OwnedFile::create(lock_path, Error::Lock)?;
        lock.file
            .as_mut()
            .ok_or(Error::Lock)?
            .write_all(&self.installation.nonce)
            .map_err(|_| Error::Io)?;
        lock.file
            .as_mut()
            .ok_or(Error::Lock)?
            .sync_all()
            .map_err(|_| Error::Io)?;
        lock.cleanup_nonce = Some(self.installation.nonce);
        // Close before Windows cleanup; the path remains protected by create_new
        // and the nonce check under the caller's exclusive-directory contract.
        lock.file.take();
        let check_lock = || {
            checkpoint()?;
            reject_links(&lock.path).map_err(|_| Error::Lock)?;
            let mut file = File::open(&lock.path).map_err(|_| Error::Lock)?;
            let mut bytes = [0; 33];
            let count = file.read(&mut bytes).map_err(|_| Error::Lock)?;
            if count != 32 || bytes[..32] != self.installation.nonce {
                Err(Error::Lock)
            } else {
                Ok(())
            }
        };
        check_lock()?;
        let expected = self.installation.expected_current_sha256;
        let current_cap = 256 * 1024 * 1024;
        if hash_file(&self.installation.path, current_cap, &checkpoint)? != expected {
            return Err(Error::InstallConflict);
        }
        let mut header = Vec::new();
        File::open(&self.installation.path)
            .map_err(|_| Error::Io)?
            .take(64 * 1024)
            .read_to_end(&mut header)
            .map_err(|_| Error::Io)?;
        crate::protocol::validate_binary(
            &header,
            crate::Target::parse(&update.manifest().manifest().target)?,
        )?;
        checkpoint()?;
        let rollback = self.installation.rollback_path();
        if fs::symlink_metadata(&rollback).is_ok() {
            return Err(Error::InstallConflict);
        }
        let mut stage = OwnedFile::create(self.installation.stage_path(), Error::InstallConflict)?;
        let file = stage.file.as_mut().ok_or(Error::Io)?;
        for chunk in update.bytes().chunks(64 * 1024) {
            check_lock()?;
            file.write_all(chunk).map_err(|_| Error::Io)?;
        }
        file.set_permissions(metadata.permissions())
            .map_err(|_| Error::Io)?;
        file.sync_all().map_err(|_| Error::Io)?;
        stage.file.take();
        if hash_file(&stage.path, update.bytes().len(), &checkpoint)? != update.manifest().digest {
            return Err(Error::Integrity);
        }
        check_lock()?;
        if hash_file(&self.installation.path, current_cap, &checkpoint)? != expected {
            return Err(Error::InstallConflict);
        }
        fs::hard_link(&self.installation.path, &rollback).map_err(|_| Error::Io)?;
        if hash_file(&rollback, current_cap, &checkpoint)? != expected {
            return Err(Error::InstallConflict);
        }
        check_lock()?;
        let final_snapshot = check_control(control, start, deadline)?;
        if final_snapshot.unix_seconds >= update.manifest().manifest().expires_at {
            return Err(Error::Expired);
        }
        let staged = fs::symlink_metadata(&stage.path).map_err(|_| Error::Install)?;
        if !staged.is_file()
            || staged.file_type().is_symlink()
            || staged.len() != update.bytes().len() as u64
        {
            return Err(Error::Install);
        }
        fs::rename(&stage.path, &self.installation.path).map_err(|_| Error::Install)?;
        stage.keep = true;
        lock.keep = true;
        let cleanup_pending = fs::remove_file(&lock.path).err().map(|_| lock.path.clone());
        // A post-commit failure/signal cannot truthfully claim "not installed".
        Ok(FileInstallReceipt {
            installed_path: self.installation.path.clone(),
            rollback_path: rollback,
            installed_sha256: update.manifest().digest,
            previous_sha256: expected,
            cleanup_pending,
        })
    }
}

pub(crate) fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    if let Some(filename) = path.file_name() {
        name.push(filename);
    }
    name.push(suffix);
    path.with_file_name(name)
}

pub(crate) fn reject_links(path: &Path) -> Result<(), Error> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(|_| Error::Io)?;
        if metadata.file_type().is_symlink() {
            return Err(Error::Custody);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(Error::Custody);
            }
        }
    }
    Ok(())
}

pub(crate) fn hash_file(
    path: &Path,
    cap: usize,
    checkpoint: &dyn Fn() -> Result<(), Error>,
) -> Result<[u8; 32], Error> {
    let mut file = File::open(path).map_err(|_| Error::Io)?;
    let mut digest = digest::Context::new(&digest::SHA256);
    let mut buffer = [0; 64 * 1024];
    let mut total = 0_usize;
    loop {
        checkpoint()?;
        let count = file.read(&mut buffer).map_err(|_| Error::Io)?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count).ok_or(Error::Limit)?;
        if total > cap {
            return Err(Error::Limit);
        }
        digest.update(&buffer[..count]);
    }
    let mut result = [0; 32];
    result.copy_from_slice(digest.finish().as_ref());
    Ok(result)
}

pub(crate) struct OwnedFile {
    pub(crate) path: PathBuf,
    pub(crate) file: Option<File>,
    pub(crate) keep: bool,
    pub(crate) cleanup_nonce: Option<[u8; 32]>,
}

impl OwnedFile {
    pub(crate) fn create(path: PathBuf, conflict: Error) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    conflict
                } else {
                    Error::Io
                }
            })?;
        Ok(Self {
            path,
            file: Some(file),
            keep: false,
            cleanup_nonce: None,
        })
    }
}

impl Drop for OwnedFile {
    fn drop(&mut self) {
        self.file.take();
        if !self.keep {
            if let Some(nonce) = self.cleanup_nonce {
                let mut bytes = [0; 33];
                let own_lock = File::open(&self.path)
                    .and_then(|mut file| file.read(&mut bytes))
                    .is_ok_and(|count| count == 32 && bytes[..32] == nonce);
                if !own_lock {
                    return;
                }
            }
            let _ = fs::remove_file(&self.path);
        }
    }
}
