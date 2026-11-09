// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Blocking, exact-path output, separate from the truncating copy adapter.

use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

/// Why an explicit output path cannot name an exact local file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactOutputPathError {
    /// The caller did not resolve the path to an absolute native path.
    NotAbsolute,
    /// A parent/current-directory component still needs caller resolution.
    Unresolved,
    /// A network share or URI is not a local-file output destination.
    Remote,
    /// A Windows device namespace or reserved device name was selected.
    Device,
    /// No exact file basename exists, or native filename syntax is invalid.
    InvalidFileName,
}

/// Failure category for exact local output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactOutputErrorKind {
    /// The destination is not an explicit resolved local-file path.
    InvalidPath(ExactOutputPathError),
    /// Any entry already occupies the destination, including dangling symlinks.
    OutputExists,
    /// The caller-selected file's parent does not exist.
    MissingParent,
    /// An ancestor that must be a directory is not a directory.
    ParentNotDirectory,
    /// The provided bytes exceed the caller's bound; no file was created.
    ByteLimit {
        /// Maximum bytes selected at preflight.
        limit: usize,
        /// Actual bytes offered to commit.
        actual: usize,
    },
    /// An explicit native inspection, create, write or flush failure.
    Io,
}

/// The boundary where exact output stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactOutputStage {
    /// Read-only destination and parent validation.
    Preflight,
    /// Byte-bound validation, before attempting file creation.
    Limit,
    /// Exclusive creation of the final filename.
    Create,
    /// Writing exact bytes to the created final file.
    Write,
    /// Flushing the created file after all write calls succeeded.
    Flush,
}

/// Exact output failure, retaining known side effects rather than claiming rollback.
#[derive(Debug)]
pub struct ExactOutputError {
    /// Exact caller-supplied path, not included in the display diagnostic.
    pub path: PathBuf,
    /// Typed failure classification.
    pub kind: ExactOutputErrorKind,
    /// Boundary that failed.
    pub stage: ExactOutputStage,
    /// This operation successfully created the final path.
    pub path_created: bool,
    /// Bytes accepted by successful write calls before failure.
    pub bytes_written: usize,
    /// Original native error, when a filesystem or write call failed.
    pub io_error: Option<io::Error>,
}

impl ExactOutputError {
    fn new(path: &Path, kind: ExactOutputErrorKind, stage: ExactOutputStage) -> Self {
        Self {
            path: path.to_owned(),
            kind,
            stage,
            path_created: false,
            bytes_written: 0,
            io_error: None,
        }
    }

    fn native(path: &Path, stage: ExactOutputStage, cause: io::Error) -> Self {
        let kind = match (stage, cause.kind()) {
            (ExactOutputStage::Create, io::ErrorKind::AlreadyExists) => {
                ExactOutputErrorKind::OutputExists
            }
            (ExactOutputStage::Preflight | ExactOutputStage::Create, io::ErrorKind::NotFound) => {
                ExactOutputErrorKind::MissingParent
            }
            (
                ExactOutputStage::Preflight | ExactOutputStage::Create,
                io::ErrorKind::NotADirectory,
            ) => ExactOutputErrorKind::ParentNotDirectory,
            _ => ExactOutputErrorKind::Io,
        };
        Self {
            io_error: Some(cause),
            ..Self::new(path, kind, stage)
        }
    }
}

impl fmt::Display for ExactOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("exact local output failed; inspect kind, stage and partial-write evidence")
    }
}

impl std::error::Error for ExactOutputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.io_error
            .as_ref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}

/// Successful exact local output, not a durable-storage or atomic-commit guarantee.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactOutputReceipt {
    /// Original filename, without canonicalization, extensions or timestamps.
    pub path: PathBuf,
    /// Exact number of bytes accepted before successful flush.
    pub bytes_written: usize,
}

/// Read-only preflight followed by bounded, no-clobber creation of one local file.
///
/// Both methods are **blocking**. Call them from the host's blocking context,
/// never directly on an async worker. No runtime, worker pool, network client,
/// credentials or process working-directory discovery is created here.
///
/// Preflight does not reserve a name or test write permission by creating a
/// file. Perform it before remote reads; keep remote reads independently bounded
/// by the same limit, then pass the final bytes to [`Self::commit`].
///
/// The caller selects an absolute native file path and owns its stable, trusted
/// parent namespace. This is not a hostile-process sandbox: portable safe
/// filesystem calls cannot pin ancestors against concurrent replacement or
/// detect whether a native drive/mount is backed by remote storage. Network-share
/// paths and URIs are rejected; there is no remote-output adapter.
#[derive(Debug)]
pub struct ExactOutputFile {
    path: PathBuf,
    max_bytes: usize,
}

impl ExactOutputFile {
    /// Inspect an explicit resolved absolute file path without modifying it.
    ///
    /// The filename is retained exactly. No parent directories, timestamp
    /// directories, temporary files or alternate extensions are created.
    /// A zero-byte bound permits only an empty output file.
    ///
    /// # Errors
    /// Existing entries (files, directories, symlinks and other native entries)
    /// fail with [`ExactOutputErrorKind::OutputExists`]. Missing/non-directory
    /// parents, invalid paths and native inspection errors remain explicit.
    pub fn preflight(path: &Path, max_bytes: usize) -> Result<Self, ExactOutputError> {
        validate_path(path).map_err(|reason| {
            ExactOutputError::new(
                path,
                ExactOutputErrorKind::InvalidPath(reason),
                ExactOutputStage::Preflight,
            )
        })?;
        match fs::symlink_metadata(path) {
            Ok(_) => {
                return Err(ExactOutputError::new(
                    path,
                    ExactOutputErrorKind::OutputExists,
                    ExactOutputStage::Preflight,
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ExactOutputError::native(
                    path,
                    ExactOutputStage::Preflight,
                    error,
                ));
            }
        }
        let spelling = path.as_os_str().as_encoded_bytes();
        if path.file_name().is_none()
            || spelling.last() == Some(&b'/')
            || (cfg!(windows) && spelling.last() == Some(&b'\\'))
        {
            return Err(ExactOutputError::new(
                path,
                ExactOutputErrorKind::InvalidPath(ExactOutputPathError::InvalidFileName),
                ExactOutputStage::Preflight,
            ));
        }
        let parent = path.parent().ok_or_else(|| {
            ExactOutputError::new(
                path,
                ExactOutputErrorKind::InvalidPath(ExactOutputPathError::InvalidFileName),
                ExactOutputStage::Preflight,
            )
        })?;
        let metadata = fs::metadata(parent)
            .map_err(|error| ExactOutputError::native(path, ExactOutputStage::Preflight, error))?;
        if !metadata.is_dir() {
            return Err(ExactOutputError::new(
                path,
                ExactOutputErrorKind::ParentNotDirectory,
                ExactOutputStage::Preflight,
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            max_bytes,
        })
    }

    /// Borrow the exact preflighted destination spelling.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Maximum bytes accepted by [`Self::commit`], including zero.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Create the final filename exclusively and write unchanged binary bytes.
    ///
    /// Consumes the preflight; it does not read remote data or encode output.
    /// Empty bytes create an empty file. `create_new` enforces no-clobber even
    /// when a competing process creates an entry after preflight.
    ///
    /// # Errors
    /// An oversized payload fails before creation. Once creation succeeds,
    /// write/flush failure can leave an empty or partial final file. Errors
    /// retain creation, write count, stage and original I/O evidence. No cleanup,
    /// rollback, retry of the transfer, atomic full-file commit, `sync_all`
    /// durability or error-free close is promised.
    pub fn commit(self, bytes: &[u8]) -> Result<ExactOutputReceipt, ExactOutputError> {
        if bytes.len() > self.max_bytes {
            return Err(ExactOutputError::new(
                &self.path,
                ExactOutputErrorKind::ByteLimit {
                    limit: self.max_bytes,
                    actual: bytes.len(),
                },
                ExactOutputStage::Limit,
            ));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
            .map_err(|error| {
                ExactOutputError::native(&self.path, ExactOutputStage::Create, error)
            })?;
        let bytes_written = write_bytes(&mut file, &self.path, bytes)?;
        Ok(ExactOutputReceipt {
            path: self.path,
            bytes_written,
        })
    }
}

fn write_bytes(
    writer: &mut impl Write,
    path: &Path,
    bytes: &[u8],
) -> Result<usize, ExactOutputError> {
    let mut written = 0;
    let failure = |stage, cause, written| ExactOutputError {
        path_created: true,
        bytes_written: written,
        ..ExactOutputError::native(path, stage, cause)
    };
    while written < bytes.len() {
        match writer.write(&bytes[written..]) {
            Ok(0) => {
                return Err(failure(
                    ExactOutputStage::Write,
                    io::Error::from(io::ErrorKind::WriteZero),
                    written,
                ));
            }
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(failure(ExactOutputStage::Write, error, written)),
        }
    }
    writer
        .flush()
        .map_err(|error| failure(ExactOutputStage::Flush, error, written))?;
    Ok(written)
}

fn validate_path(path: &Path) -> Result<(), ExactOutputPathError> {
    let spelling = path.as_os_str().as_encoded_bytes();
    if spelling.starts_with(b"//") || spelling.windows(3).any(|part| part == b"://") {
        return Err(ExactOutputPathError::Remote);
    }
    #[cfg(windows)]
    validate_windows_path(path)?;
    if !path.is_absolute() {
        return Err(ExactOutputPathError::NotAbsolute);
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || spelling
            .split(|&byte| byte == b'/' || (cfg!(windows) && byte == b'\\'))
            .any(|part| part == b"." || part == b"..")
    {
        return Err(ExactOutputPathError::Unresolved);
    }
    if spelling.contains(&0) {
        return Err(ExactOutputPathError::InvalidFileName);
    }
    Ok(())
}

#[cfg(windows)]
fn validate_windows_path(path: &Path) -> Result<(), ExactOutputPathError> {
    use std::path::Prefix;

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::UNC(..) | Prefix::VerbatimUNC(..) => {
                    return Err(ExactOutputPathError::Remote);
                }
                Prefix::DeviceNS(_) | Prefix::Verbatim(_) => {
                    return Err(ExactOutputPathError::Device);
                }
                Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {}
            },
            Component::Normal(name) => {
                let name = name.to_str().ok_or(ExactOutputPathError::InvalidFileName)?;
                let stem = name
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(' ')
                    .to_ascii_uppercase();
                if matches!(
                    stem.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                ) || ["COM", "LPT"].into_iter().any(|prefix| {
                    stem.strip_prefix(prefix).is_some_and(|suffix| {
                        matches!(
                            suffix,
                            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                        )
                    })
                }) {
                    return Err(ExactOutputPathError::Device);
                }
                if name.ends_with(['.', ' '])
                    || name.chars().any(|character| {
                        character <= '\u{1f}'
                            || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
                    })
                {
                    return Err(ExactOutputPathError::InvalidFileName);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;

    struct FaultWriter {
        remaining: usize,
        fail_flush: bool,
    }

    impl Write for FaultWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::from(io::ErrorKind::StorageFull));
            }
            let count = self.remaining.min(bytes.len());
            self.remaining -= count;
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                Err(io::Error::from(io::ErrorKind::Other))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn partial_write_error_retains_created_path_and_confirmed_bytes() {
        let mut writer = FaultWriter {
            remaining: 2,
            fail_flush: false,
        };
        let error = write_bytes(&mut writer, Path::new("evidence-only"), b"abcd").unwrap_err();
        assert_eq!(error.stage, ExactOutputStage::Write);
        assert_eq!(error.kind, ExactOutputErrorKind::Io);
        assert!(error.path_created);
        assert_eq!(error.bytes_written, 2);
        assert_eq!(error.io_error.unwrap().kind(), io::ErrorKind::StorageFull);
    }

    #[test]
    fn flush_error_retains_all_written_bytes_including_empty_output() {
        for bytes in [b"abcd".as_slice(), b"".as_slice()] {
            let mut writer = FaultWriter {
                remaining: bytes.len(),
                fail_flush: true,
            };
            let error = write_bytes(&mut writer, Path::new("evidence-only"), bytes).unwrap_err();
            assert_eq!(error.stage, ExactOutputStage::Flush);
            assert!(error.path_created);
            assert_eq!(error.bytes_written, bytes.len());
            assert!(error.io_error.is_some());
        }
    }
}
