// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ExportRequest, ExportSink, checked_destination};
use std::io;

/// Native no-follow, atomic no-clobber file delivery with private staging.
#[derive(Default)]
pub struct FileSink {
    destination: Option<Destination>,
    output: Option<std::path::PathBuf>,
}

impl ExportSink for FileSink {
    fn preflight(&mut self, request: &ExportRequest) -> io::Result<()> {
        self.destination = Some(Destination::prepare(&checked_destination(
            &request.output,
        )?)?);
        self.output = Some(request.output.clone());
        Ok(())
    }

    fn publish(&mut self, request: &ExportRequest, bytes: &[u8]) -> io::Result<()> {
        if self.output.as_deref() != Some(request.output.as_path()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Export destination changed",
            ));
        }
        self.destination
            .as_mut()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "Export was not preflighted")
            })?
            .publish(bytes)
    }
}

#[cfg(unix)]
struct Destination {
    parent: std::fs::File,
    directory: std::fs::File,
    staging: std::fs::File,
    staging_name: std::ffi::OsString,
    destination_name: std::ffi::OsString,
    output: std::path::PathBuf,
    attempted: bool,
}

#[cfg(unix)]
impl Destination {
    fn prepare(path: &std::path::Path) -> io::Result<Self> {
        use rustix::fs::{Mode, OFlags, mkdirat, openat};
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let parent = Self::open_parent(path)?;
        // Tempfile owns random naming/retries; all actual creation and cleanup
        // remain relative to the no-follow directory handle, not a mutable path.
        let temporary = tempfile::Builder::new()
            .prefix(".fab-tabular-")
            .disable_cleanup(true)
            .make_in(path.parent().expect("checked parent"), |candidate| {
                let name = candidate.file_name().expect("generated staging name");
                mkdirat(&parent, name, Mode::from_bits_truncate(0o700))?;
                match openat(&parent, name, flags, Mode::empty()) {
                    Ok(directory) => Ok(std::fs::File::from(directory)),
                    Err(error) => {
                        let _ = rustix::fs::unlinkat(&parent, name, rustix::fs::AtFlags::REMOVEDIR);
                        Err(io::Error::from(error))
                    }
                }
            })?;
        let name = temporary
            .path()
            .file_name()
            .expect("generated staging name")
            .to_owned();
        let (directory, temporary_path) = temporary.into_parts();
        drop(temporary_path);
        let staging = match openat(
            &directory,
            "data",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        ) {
            Ok(staging) => std::fs::File::from(staging),
            Err(error) => {
                let _ = rustix::fs::unlinkat(&parent, &name, rustix::fs::AtFlags::REMOVEDIR);
                return Err(error.into());
            }
        };
        let destination = Self {
            parent,
            directory,
            staging,
            staging_name: name,
            destination_name: path.file_name().expect("checked destination").into(),
            output: path.into(),
            attempted: false,
        };
        destination.check_parent()?;
        Ok(destination)
    }

    fn publish(&mut self, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        if std::mem::replace(&mut self.attempted, true) {
            return Err(io::Error::other("Export delivery was already attempted"));
        }
        self.check_parent()?;
        self.staging.write_all(bytes)?;
        self.staging.sync_all()?;
        rustix::fs::linkat(
            &self.directory,
            "data",
            &self.parent,
            &self.destination_name,
            rustix::fs::AtFlags::empty(),
        )?;
        self.check_parent()?;
        Ok(())
    }

    fn open_parent(path: &std::path::Path) -> io::Result<std::fs::File> {
        use rustix::fs::{Mode, OFlags, open, openat};
        use std::path::Component;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut parent: std::fs::File = open("/", flags, Mode::empty())?.into();
        for component in path.parent().expect("checked parent").components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(name) => {
                    parent = openat(&parent, name, flags, Mode::empty())?.into()
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Invalid export directory",
                    ));
                }
            }
        }
        Ok(parent)
    }

    fn check_parent(&self) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let selected = self.parent.metadata()?;
        let current = Self::open_parent(&self.output)?.metadata()?;
        if (selected.dev(), selected.ino()) != (current.dev(), current.ino()) {
            return Err(io::Error::other(
                "Export directory changed during execution",
            ));
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for Destination {
    fn drop(&mut self) {
        let _ = rustix::fs::unlinkat(&self.directory, "data", rustix::fs::AtFlags::empty());
        let _ = rustix::fs::unlinkat(
            &self.parent,
            &self.staging_name,
            rustix::fs::AtFlags::REMOVEDIR,
        );
    }
}

#[cfg(windows)]
struct Destination {
    staging: Option<tempfile::NamedTempFile>,
    output: std::path::PathBuf,
    _directories: Vec<std::fs::File>,
}

#[cfg(windows)]
impl Destination {
    fn prepare(path: &std::path::Path) -> io::Result<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        let mut directories = Vec::new();
        let ancestors: Vec<_> = path.parent().expect("checked parent").ancestors().collect();
        for ancestor in ancestors.into_iter().rev() {
            // Hold every directory without FILE_SHARE_DELETE so no ancestor can
            // be replaced between no-follow validation and atomic publication.
            let handle = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0x1 | 0x2)
                .custom_flags(0x0200_0000 | 0x0020_0000)
                .open(ancestor)?;
            let metadata = handle.metadata()?;
            if !metadata.is_dir() || super::linked(&metadata) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Export directory contains a reparse point",
                ));
            }
            directories.push(handle);
        }
        checked_destination(path)?;
        let staging = tempfile::Builder::new()
            .prefix(".fab-tabular-")
            .tempfile_in(path.parent().expect("checked parent"))?;
        Ok(Self {
            staging: Some(staging),
            output: path.into(),
            _directories: directories,
        })
    }

    fn publish(&mut self, bytes: &[u8]) -> io::Result<()> {
        use std::io::{Read, Write};
        use std::os::windows::fs::OpenOptionsExt;
        let mut staging = self
            .staging
            .take()
            .ok_or_else(|| io::Error::other("Export already published"))?;
        staging.write_all(bytes)?;
        staging.as_file().sync_all()?;
        let path = staging.into_temp_path();
        let mut source = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1)
            .custom_flags(0x0020_0000)
            .open(&path)?;
        let metadata = source.metadata()?;
        if !metadata.is_file() || super::linked(&metadata) {
            return Err(io::Error::other("Export staging file was replaced"));
        }
        let mut buffer = [0_u8; 8192];
        for expected in bytes.chunks(buffer.len()) {
            source.read_exact(&mut buffer[..expected.len()])?;
            if &buffer[..expected.len()] != expected {
                return Err(io::Error::other("Export staging content changed"));
            }
        }
        if source.read(&mut buffer[..1])? != 0 {
            return Err(io::Error::other("Export staging content changed"));
        }
        // The source handle denies both writes and deletion while the hard link
        // is created. Publication cannot follow a substituted staging link.
        std::fs::hard_link(&path, &self.output)?;
        drop(source);
        drop(path);
        Ok(())
    }
}
