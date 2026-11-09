// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    Control, Error, Target,
    install::{hash_file, reject_links, sibling},
    model::validate_leaf,
    protocol::decode_digest,
};

pub(crate) const RECORD_CAP: usize = 16 * 1024;
pub(crate) const FILE_CAP: usize = 256 * 1024 * 1024;
pub(crate) const BACKUP_CAP: usize = 2;

/// Independent current-installation identity, not fields read from a release.
pub struct ReceiptBinding<'a> {
    /// Pinned publisher repository.
    pub repository: &'a str,
    /// Exact current executable semantic version.
    pub version: &'a str,
    /// Compiled native target.
    pub target: Target,
    /// Exact compiled feature variant.
    pub variant: &'a str,
}

/// Fixed installed helper name; the payload never selects a program to execute.
pub fn helper_name() -> &'static str {
    if cfg!(windows) {
        "fab-rs-update-helper.exe"
    } else {
        "fab-rs-update-helper"
    }
}

/// One installer-owned, versioned and digest-verified retained image.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RollbackImage {
    /// Absolute exact image path within the exclusively owned installation root.
    pub path: PathBuf,
    /// Canonical previous semantic version.
    pub version: String,
    /// Lowercase SHA-256 of the previous image.
    pub sha256: String,
    /// Fresh operation identity; prevents collisions across repeated versions.
    pub nonce: String,
}

/// Protected standalone installer record; not a publisher signature.
///
/// The trusted installer creates revision 2 and protects its directory. The
/// helper may advance version/digest and append a verified prior image, but
/// cannot promote package-manager custody or change helper/publisher identity.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CustodyReceipt {
    /// Exactly 2; revision 1 does not authorize helper execution.
    pub schema: u32,
    /// Exactly `fab-rs`.
    pub product: String,
    /// Exact pinned publisher repository.
    pub repository: String,
    /// Exactly `standalone`; package-manager records are refused.
    pub custody: String,
    /// Absolute installation path.
    pub executable: PathBuf,
    /// Current installed canonical semantic version.
    pub version: String,
    /// Exact native target triple.
    pub target: String,
    /// Exact installed feature variant.
    pub variant: String,
    /// Exact current executable digest.
    pub sha256: String,
    /// Fixed sibling helper installed by the trusted installer.
    pub helper: PathBuf,
    /// Exact installed helper digest, never derived from downloaded metadata.
    pub helper_sha256: String,
    /// Installer-pinned Windows runtime root for Winsock provider expansion.
    ///
    /// Not release metadata or an ambient child-environment fallback. Non-Windows
    /// installations use null. Windows launch fails closed when absent.
    /// Local DOS-drive and verbatim-drive spellings are accepted; validation
    /// canonicalizes only the child environment value, never this receipt pin.
    #[serde(default)]
    pub windows_system_root: Option<PathBuf>,
    /// At most two prior images; further upgrades require installer retention maintenance.
    pub backups: Vec<RollbackImage>,
}

impl CustodyReceipt {
    /// Load and validate exact installation, helper, backup and receipt bytes.
    ///
    /// This does not discover custody from writability or PATH. `repository`,
    /// version, target and variant are caller/build pins, not release inputs.
    pub fn load(
        path: &Path,
        executable: &Path,
        binding: ReceiptBinding<'_>,
        control: &dyn Control,
    ) -> Result<Self, Error> {
        let receipt = Self::read_owned(
            path,
            executable,
            binding.repository,
            binding.target,
            binding.variant,
            control,
        )?;
        let started = control.snapshot().monotonic_ms;
        let deadline = started.checked_add(30_000).ok_or(Error::Deadline)?;
        let checkpoint = || crate::model::check_control(control, started, deadline).map(|_| ());
        checkpoint()?;
        reject_links(executable)?;
        let executable = fs::canonicalize(executable).map_err(|_| Error::Custody)?;
        if receipt.version != binding.version
            || hash_file(&executable, FILE_CAP, &checkpoint)? != decode_digest(&receipt.sha256)?
        {
            return Err(Error::Custody);
        }
        Ok(receipt)
    }

    /// Validate protected custody/helper pins without assuming binary commit state.
    ///
    /// Only exact transaction reconciliation may use this weaker preflight. A
    /// new update must call `load` before network work or preparing any bytes.
    pub fn read_owned(
        path: &Path,
        executable: &Path,
        repository: &str,
        target: Target,
        variant: &str,
        control: &dyn Control,
    ) -> Result<Self, Error> {
        let started = control.snapshot().monotonic_ms;
        let deadline = started.checked_add(30_000).ok_or(Error::Deadline)?;
        let checkpoint = || crate::model::check_control(control, started, deadline).map(|_| ());
        absolute(executable)?;
        reject_links(executable.parent().ok_or(Error::Custody)?)?;
        let executable = executable
            .parent()
            .and_then(|parent| {
                fs::canonicalize(parent)
                    .ok()
                    .map(|p| p.join(executable.file_name().unwrap_or_default()))
            })
            .ok_or(Error::Custody)?;
        let expected = executable.with_file_name(".fab-rs-install.json");
        absolute(path)?;
        reject_links(path)?;
        if fs::canonicalize(path).map_err(|_| Error::Custody)?
            != fs::canonicalize(expected).map_err(|_| Error::Custody)?
        {
            return Err(Error::Custody);
        }
        let bytes = read_record(path, RECORD_CAP)?;
        let receipt: Self = serde_json::from_slice(&bytes).map_err(|_| Error::Custody)?;
        receipt.validate_shape(&executable, repository, target, variant)?;
        receipt.verify_components(&checkpoint)?;
        Ok(receipt)
    }

    pub(crate) fn validate_shape(
        &self,
        executable: &Path,
        repository: &str,
        target: Target,
        variant: &str,
    ) -> Result<(), Error> {
        absolute(&self.executable)?;
        absolute(&self.helper)?;
        let version = semver::Version::parse(&self.version).map_err(|_| Error::Custody)?;
        if self.schema != 2
            || self.product != "fab-rs"
            || self.repository != repository
            || self.custody != "standalone"
            || self.executable != executable
            || self.target != target.as_str()
            || self.variant != variant
            || self.version != version.to_string()
            || self.helper != executable.with_file_name(helper_name())
            || self.helper == self.executable
            || self.backups.len() > BACKUP_CAP
        {
            return Err(Error::Custody);
        }
        decode_digest(&self.sha256)?;
        decode_digest(&self.helper_sha256)?;
        if let Some(root) = &self.windows_system_root {
            Self::validate_system_root(root)?;
        }
        for (index, backup) in self.backups.iter().enumerate() {
            validate_leaf(&backup.version).map_err(|_| Error::Custody)?;
            decode_digest(&backup.sha256)?;
            let nonce = decode_digest(&backup.nonce)?;
            if nonce == [0; 32]
                || self.backups[..index]
                    .iter()
                    .any(|other| other.path == backup.path)
                || backup.path
                    != rollback_path(executable, &backup.version, &backup.sha256, &backup.nonce)
            {
                return Err(Error::Custody);
            }
        }
        Ok(())
    }

    pub(crate) fn validate_system_root(root: &Path) -> Result<PathBuf, Error> {
        Self::local_drive_path(root)?;
        reject_links(root)?;
        let canonical = fs::canonicalize(root).map_err(|_| Error::NativeBoundary)?;
        Self::local_drive_path(&canonical)?;
        reject_links(&canonical)?;
        if !fs::metadata(&canonical)
            .map_err(|_| Error::NativeBoundary)?
            .is_dir()
        {
            return Err(Error::NativeBoundary);
        }
        let provider = canonical.join("System32").join("mswsock.dll");
        reject_links(&provider)?;
        if !fs::metadata(provider)
            .map_err(|_| Error::NativeBoundary)?
            .is_file()
        {
            return Err(Error::NativeBoundary);
        }
        Ok(canonical)
    }

    fn local_drive_path(path: &Path) -> Result<(), Error> {
        absolute(path)?;
        #[cfg(windows)]
        {
            use std::path::Prefix;
            match path.components().next() {
                Some(Component::Prefix(prefix))
                    if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
                {
                    Ok(())
                }
                _ => Err(Error::NativeBoundary),
            }
        }
        #[cfg(not(windows))]
        {
            Err(Error::NativeBoundary)
        }
    }

    pub(crate) fn verify_components(
        &self,
        checkpoint: &dyn Fn() -> Result<(), Error>,
    ) -> Result<(), Error> {
        for (path, expected) in std::iter::once((&self.helper, &self.helper_sha256))
            .chain(self.backups.iter().map(|b| (&b.path, &b.sha256)))
        {
            reject_links(path)?;
            let metadata = fs::symlink_metadata(path).map_err(|_| Error::Custody)?;
            if !metadata.is_file()
                || hash_file(path, FILE_CAP, checkpoint)? != decode_digest(expected)?
            {
                return Err(Error::Custody);
            }
        }
        Ok(())
    }
}

pub(crate) fn absolute(path: &Path) -> Result<(), Error> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        Err(Error::Custody)
    } else {
        Ok(())
    }
}

pub(crate) fn rollback_path(
    executable: &Path,
    version: &str,
    digest: &str,
    nonce: &str,
) -> PathBuf {
    sibling(executable, &format!(".rollback-{version}-{digest}-{nonce}"))
}

pub(crate) fn read_record(path: &Path, cap: usize) -> Result<Vec<u8>, Error> {
    reject_links(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| Error::Io)?;
    if !metadata.is_file() || metadata.len() > cap as u64 {
        return Err(Error::Limit);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(Error::Custody);
        }
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| Error::Io)?
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Io)?;
    if bytes.len() > cap {
        return Err(Error::Limit);
    }
    Ok(bytes)
}

pub(crate) fn create_record(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > 64 * 1024 {
        return Err(Error::Limit);
    }
    reject_links(path.parent().ok_or(Error::Custody)?)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| Error::InstallConflict)?;
    file.write_all(bytes).map_err(|_| Error::Io)?;
    file.sync_all().map_err(|_| Error::Io)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(all(test, windows))]
mod tests {
    use std::{
        os::windows::fs::{MetadataExt, symlink_dir},
        path::Prefix,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn runtime_spellings() -> (PathBuf, PathBuf) {
        let canonical =
            fs::canonicalize(std::env::var_os("SystemRoot").expect("Windows runtime root"))
                .expect("canonical local Windows runtime root");
        let mut components = canonical.components();
        let drive = match components.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
                _ => panic!("test runtime must use a local drive"),
            },
            _ => panic!("test runtime must use a drive prefix"),
        };
        assert_eq!(components.next(), Some(Component::RootDir));
        let mut plain = PathBuf::from(format!("{}:\\", char::from(drive)));
        for component in components {
            let Component::Normal(value) = component else {
                panic!("canonical runtime has an unexpected component");
            };
            plain.push(value);
        }
        (plain, canonical)
    }

    fn owned_directory(label: &str) -> PathBuf {
        let root = std::env::var_os("FAB_UPDATE_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        assert!(root.is_absolute());
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = root.join(format!(
            "runtime-{label}-{:x}-{unique:x}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }

    #[test]
    fn system_root_accepts_plain_and_verbatim_local_drive_pins_without_rewriting_them() {
        let (plain, canonical) = runtime_spellings();
        for pin in [&plain, &canonical] {
            let original = pin.to_path_buf();
            assert_eq!(
                CustodyReceipt::validate_system_root(pin).unwrap(),
                canonical
            );
            assert_eq!(pin, &original);
        }
    }

    #[test]
    fn system_root_rejects_nonlocal_namespaces_relative_and_parent_components() {
        for root in [
            r"\\server\share\Windows",
            r"\\?\UNC\server\share\Windows",
            r"\\.\C:\Windows",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\Windows",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\Windows",
            r"\Windows",
            r"C:Windows",
            r"Windows",
            r"C:\Windows\..\Windows",
            r"\\?\C:\Windows\..\Windows",
        ] {
            assert!(
                CustodyReceipt::validate_system_root(Path::new(root)).is_err(),
                "{root}"
            );
        }
    }

    #[test]
    fn system_root_rejects_a_local_directory_without_the_provider() {
        let root = owned_directory("no-provider");
        assert!(CustodyReceipt::validate_system_root(&root).is_err());
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn system_root_rejects_real_reparse_roots_before_canonicalization() {
        let (_, canonical) = runtime_spellings();
        let root = owned_directory("reparse");
        let link = root.join("runtime-link");
        let created = symlink_dir(&canonical, &link).is_ok();
        let reparse = if created {
            link.clone()
        } else {
            // Unprivileged Windows hosts can still exercise an existing OS
            // junction. Require an actual readable reparse point, never skip.
            let volume = canonical.ancestors().last().unwrap();
            [
                volume.join("Users").join("All Users"),
                volume.join("ProgramData").join("Application Data"),
            ]
            .into_iter()
            .find(|path| {
                fs::symlink_metadata(path)
                    .is_ok_and(|metadata| metadata.file_attributes() & 0x400 != 0)
            })
            .expect("reparse acceptance requires symlink privilege or a readable OS junction")
        };
        assert_ne!(
            fs::symlink_metadata(&reparse).unwrap().file_attributes() & 0x400,
            0
        );
        assert_eq!(
            CustodyReceipt::validate_system_root(&reparse),
            Err(Error::Custody)
        );
        if created {
            fs::remove_dir(link).unwrap();
        }
        fs::remove_dir(root).unwrap();
    }
}
