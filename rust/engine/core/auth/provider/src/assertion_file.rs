// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pinned-root assertion reads. No path canonicalization on token refresh.

use super::{ASSERTION_LIMIT, HeadlessError, credential_error};
use async_trait::async_trait;
use azure_core::http::ClientMethodOptions;
use azure_identity::ClientAssertion;
use std::{
    ffi::OsString,
    fmt,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

/// A renewable assertion source bound to a securely opened parent directory.
///
/// Construction opens directory handles, never the assertion or a token
/// endpoint. Refresh reads a bounded regular file off the async worker.
/// Unix projected-token relative symlinks may rotate within the pinned root.
/// Parent symlinks are refused except the validated `/var/run` → `/run` system
/// alias. Windows reparse points, device paths and network roots are refused.
#[derive(Clone)]
pub struct AssertionFile {
    path: PathBuf,
    name: OsString,
    root: Arc<platform::Root>,
}

impl fmt::Debug for AssertionFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssertionFile").finish_non_exhaustive()
    }
}

impl AssertionFile {
    /// Pins the selected parent and rereads the file on access-token renewal.
    ///
    /// # Errors
    /// Rejects unsafe paths, redirected parents and an unavailable directory.
    pub fn new(path: PathBuf) -> Result<Self, HeadlessError> {
        let text = path.to_str().ok_or(HeadlessError::Configuration)?;
        if !path.is_absolute()
            || text.starts_with("\\\\")
            || text.starts_with("//")
            || text.len() > 32 * 1024
            || path.components().count() > 256
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(HeadlessError::Configuration);
        }
        let name = path
            .file_name()
            .ok_or(HeadlessError::Configuration)?
            .to_owned();
        let parent = path.parent().ok_or(HeadlessError::Configuration)?;
        let root = platform::Root::new(parent).map_err(|_| HeadlessError::Configuration)?;
        Ok(Self {
            path,
            name,
            root: Arc::new(root),
        })
    }

    /// Returns the non-secret original input binding, not a resolved token path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<String, HeadlessError> {
        let result = (|| {
            let file = self.root.open(&self.name)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.len() > ASSERTION_LIMIT {
                return Err(std::io::Error::other("invalid assertion file"));
            }
            let mut bytes = Vec::new();
            file.take(ASSERTION_LIMIT + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > ASSERTION_LIMIT {
                return Err(std::io::Error::other("invalid assertion file"));
            }
            let text = String::from_utf8(bytes)
                .map_err(|_| std::io::Error::other("invalid assertion file"))?;
            let text = text.trim_end_matches(['\r', '\n']);
            if text.is_empty() || text.bytes().any(|byte| !byte.is_ascii_graphic()) {
                return Err(std::io::Error::other("invalid assertion file"));
            }
            Ok(text.to_owned())
        })();
        result.map_err(|_: std::io::Error| HeadlessError::AssertionFile)
    }
}

#[async_trait]
impl ClientAssertion for AssertionFile {
    async fn secret(&self, _: Option<ClientMethodOptions<'_>>) -> azure_core::Result<String> {
        let source = self.clone();
        tokio::task::spawn_blocking(move || source.read())
            .await
            .map_err(|_| credential_error(HeadlessError::AssertionFile))?
            .map_err(credential_error)
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use rustix::fs::{FileType, Mode, OFlags, fstat, open, openat, readlinkat_raw};
    use std::{collections::VecDeque, ffi::OsStr, io, os::unix::ffi::OsStringExt};

    pub(super) struct Root {
        directory: File,
    }

    fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
    }

    fn invalid() -> io::Error {
        io::Error::other("invalid assertion path")
    }

    fn system_run_alias(prefix: &Path, target: &Path) -> bool {
        prefix == Path::new("/var") && matches!(target.to_str(), Some("/run" | "../run"))
    }

    fn link(directory: &File, name: &OsStr) -> io::Result<PathBuf> {
        let mut bytes = [0; 4096];
        let count = readlinkat_raw(directory, name, &mut bytes[..])?;
        if count == bytes.len() || count == 0 {
            return Err(invalid());
        }
        Ok(PathBuf::from(OsString::from_vec(bytes[..count].to_vec())))
    }

    impl Root {
        pub(super) fn new(parent: &Path) -> io::Result<Self> {
            let filesystem_root = File::from(open("/", directory_flags(), Mode::empty())?);
            let mut directory = filesystem_root.try_clone()?;
            let mut prefix = PathBuf::from("/");
            for component in parent.components() {
                let Component::Normal(name) = component else {
                    continue;
                };
                let next = openat(&directory, name, directory_flags(), Mode::empty());
                directory = match next {
                    Ok(handle) => File::from(handle),
                    Err(_) if prefix == Path::new("/var") && name == OsStr::new("run") => {
                        let target = link(&directory, name)?;
                        if !system_run_alias(&prefix, &target) {
                            return Err(invalid());
                        }
                        File::from(openat(
                            &filesystem_root,
                            "run",
                            directory_flags(),
                            Mode::empty(),
                        )?)
                    }
                    Err(error) => return Err(error.into()),
                };
                prefix.push(name);
            }
            Ok(Self { directory })
        }

        pub(super) fn open(&self, name: &std::ffi::OsStr) -> io::Result<File> {
            let mut pending = VecDeque::from([name.to_owned()]);
            let mut directories = vec![self.directory.try_clone()?];
            let mut links = 0;
            while let Some(name) = pending.pop_front() {
                if name == OsStr::new(".") {
                    continue;
                }
                if name == OsStr::new("..") {
                    if directories.len() == 1 {
                        return Err(invalid());
                    }
                    directories.pop();
                    continue;
                }
                let directory = directories.last().ok_or_else(invalid)?;
                let final_component = pending.is_empty();
                let flags = OFlags::RDONLY
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC
                    | if final_component {
                        OFlags::empty()
                    } else {
                        OFlags::DIRECTORY
                    };
                match openat(directory, &name, flags, Mode::empty()) {
                    Ok(handle) => {
                        let kind = FileType::from_raw_mode(fstat(&handle)?.st_mode);
                        if final_component {
                            if kind != FileType::RegularFile {
                                return Err(invalid());
                            }
                            return Ok(File::from(handle));
                        }
                        if kind != FileType::Directory {
                            return Err(invalid());
                        }
                        if directories.len() >= 256 {
                            return Err(invalid());
                        }
                        directories.push(File::from(handle));
                    }
                    Err(_) => {
                        links += 1;
                        if links > 40 {
                            return Err(invalid());
                        }
                        let target = link(directory, &name)?;
                        if target.is_absolute() {
                            return Err(invalid());
                        }
                        let mut parts = Vec::new();
                        for part in target.components() {
                            match part {
                                Component::Normal(name) => parts.push(name.to_owned()),
                                Component::ParentDir => parts.push(OsString::from("..")),
                                Component::CurDir => {}
                                _ => return Err(invalid()),
                            }
                        }
                        if parts.len() + pending.len() > 256 {
                            return Err(invalid());
                        }
                        for part in parts.into_iter().rev() {
                            pending.push_front(part);
                        }
                    }
                }
            }
            Err(invalid())
        }
    }

    #[cfg(test)]
    mod tests {
        #![deny(unnameable_test_items)]
        use super::*;

        #[test]
        fn standard_run_alias_is_bounded_and_not_a_general_parent_symlink_rule() {
            assert!(system_run_alias(Path::new("/var"), Path::new("/run")));
            assert!(system_run_alias(Path::new("/var"), Path::new("../run")));
            assert!(!system_run_alias(Path::new("/selected"), Path::new("/run")));
            assert!(!system_run_alias(Path::new("/var"), Path::new("/outside")));
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        ffi::OsStr,
        fs::OpenOptions,
        io,
        os::windows::fs::{MetadataExt, OpenOptionsExt},
        path::Prefix,
    };

    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const REPARSE_POINT: u32 = 0x400;
    const SHARE_READ: u32 = 1;

    pub(super) struct Root {
        directory: PathBuf,
        // Deny write/delete sharing on every parent for the provider's lifetime.
        _guards: Vec<File>,
    }

    fn invalid() -> io::Error {
        io::Error::other("invalid local assertion path")
    }

    fn valid_name(name: &OsStr) -> bool {
        let Some(name) = name.to_str() else {
            return false;
        };
        let stem = name
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches([' ', '.'])
            .to_ascii_uppercase();
        !name.is_empty()
            && !name.ends_with(['.', ' '])
            && !name.contains(':')
            && !name.chars().any(|character| {
                character.is_control() || matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
            })
            && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
            && !["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            })
    }

    fn local_disk(path: &Path) -> io::Result<PathBuf> {
        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return Err(invalid());
        };
        let letter = match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
            _ => return Err(invalid()),
        };
        let mut result = PathBuf::from(format!("{}:\\", char::from(letter)));
        for component in components {
            match component {
                Component::RootDir => {}
                Component::Normal(name) if valid_name(name) => result.push(name),
                _ => return Err(invalid()),
            }
        }
        Ok(result)
    }

    fn directory(path: &Path) -> io::Result<File> {
        let handle = OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ)
            .custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
            .open(path)?;
        let metadata = handle.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & REPARSE_POINT != 0 {
            return Err(invalid());
        }
        Ok(handle)
    }

    impl Root {
        pub(super) fn new(parent: &Path) -> io::Result<Self> {
            let selected = local_disk(parent)?;
            let drive = selected.ancestors().last().ok_or_else(invalid)?;
            // Classify only the drive root, never a caller-controlled parent or leaf.
            let resolved_drive = local_disk(&drive.canonicalize()?)?;
            let mut current = PathBuf::new();
            let mut guards = Vec::new();
            for part in resolved_drive.components() {
                current.push(part.as_os_str());
                if matches!(part, Component::RootDir | Component::Normal(_)) {
                    guards.push(directory(&current)?);
                }
            }
            for part in selected.components().skip(2) {
                let Component::Normal(name) = part else {
                    return Err(invalid());
                };
                current.push(name);
                guards.push(directory(&current)?);
            }
            Ok(Self {
                directory: current,
                _guards: guards,
            })
        }

        pub(super) fn open(&self, name: &OsStr) -> io::Result<File> {
            if !valid_name(name) {
                return Err(invalid());
            }
            let handle = OpenOptions::new()
                .read(true)
                .share_mode(SHARE_READ)
                .custom_flags(OPEN_REPARSE_POINT)
                .open(self.directory.join(name))?;
            let metadata = handle.metadata()?;
            if !metadata.is_file() || metadata.file_attributes() & REPARSE_POINT != 0 {
                return Err(invalid());
            }
            Ok(handle)
        }
    }

    #[cfg(test)]
    mod tests {
        #![deny(unnameable_test_items)]
        use super::*;

        #[test]
        fn network_and_device_targets_cannot_become_local_roots() {
            for path in [
                r"\\server\share\tokens",
                r"\\?\UNC\server\share\tokens",
                r"\\.\pipe\token",
                r"\\?\GLOBALROOT\Device\HarddiskVolume1\tokens",
            ] {
                assert!(local_disk(Path::new(path)).is_err());
            }
            for name in [
                "NUL",
                "con.txt",
                "COM1",
                "LPT¹",
                "CON .txt",
                "COM1 .dat",
                "token:stream",
                "token.",
            ] {
                assert!(!valid_name(OsStr::new(name)));
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;
    pub(super) struct Root;
    impl Root {
        pub(super) fn new(_: &Path) -> std::io::Result<Self> {
            Err(std::io::Error::other("unsupported assertion filesystem"))
        }
        pub(super) fn open(&self, _: &std::ffi::OsStr) -> std::io::Result<File> {
            Err(std::io::Error::other("unsupported assertion filesystem"))
        }
    }
}
