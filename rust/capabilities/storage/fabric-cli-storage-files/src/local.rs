// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_mutations::CommandFuture;
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    num::NonZeroUsize,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::runtime::Handle;

/// Result of the native file/directory predicates (symlinks are followed).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A missing path.
    Missing,
    /// A device, socket or another non-file/non-directory entry.
    Other,
}

/// Local failure category; native diagnostics remain explicit data.
#[derive(Debug)]
pub enum LocalErrorKind {
    /// Native open, metadata, read, write or flush failure.
    Io(io::Error),
    /// The file exceeded the caller's finite byte bound.
    Limit,
    /// Path resolution escaped the explicitly selected local boundary.
    OutsideBoundary,
    /// The caller dropped/cancelled an in-flight operation.
    Cancelled,
    /// The caller-owned blocking worker was unavailable.
    WorkerUnavailable,
    /// The injected adapter cannot retain the source handle through a transfer.
    SourceLeaseUnavailable,
}

/// Local I/O failure with known partial-write evidence.
#[derive(Debug)]
pub struct LocalError {
    /// Actual failure, not an empty-file fallback.
    pub kind: LocalErrorKind,
    /// A destination was opened with truncation/create semantics.
    pub destination_opened: bool,
    /// Bytes whose native write calls succeeded before failure.
    pub bytes_written: usize,
}

impl LocalError {
    /// Construct an injected/native failure before a destination was opened.
    pub fn new(kind: LocalErrorKind) -> Self {
        Self {
            kind,
            destination_opened: false,
            bytes_written: 0,
        }
    }

    pub(crate) fn diagnostic(&self) -> String {
        match &self.kind {
            LocalErrorKind::Io(error) => error.to_string(),
            LocalErrorKind::Limit => "Local file reached its byte bound".into(),
            LocalErrorKind::OutsideBoundary => "Local path is outside the selected boundary".into(),
            LocalErrorKind::Cancelled => "Local I/O was cancelled".into(),
            LocalErrorKind::WorkerUnavailable => "Local I/O worker is unavailable".into(),
            LocalErrorKind::SourceLeaseUnavailable => {
                "Local adapter cannot retain an open source file".into()
            }
        }
    }
}

impl fmt::Display for LocalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "local filesystem operation failed; inspect typed path and partial-write evidence",
        )
    }
}
impl std::error::Error for LocalError {}

/// Bounded binary content whose source handle stays open until this value drops.
///
/// Python's local upload keeps its `with open(..., "rb")` alive through remote
/// create/append/flush, including failures. Consumers must retain this value
/// through the complete remote operation rather than extracting bytes early.
pub struct LocalReadFile {
    content: Vec<u8>,
    _lease: Box<dyn Send>,
}

impl LocalReadFile {
    /// Retain exact bytes and an injected caller-owned handle/disposal guard.
    pub fn new(content: Vec<u8>, lease: impl Send + 'static) -> Self {
        Self {
            content,
            _lease: Box::new(lease),
        }
    }

    /// Borrow the unchanged bytes without releasing the source handle.
    pub fn content(&self) -> &[u8] {
        &self.content
    }
}

impl fmt::Debug for LocalReadFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalReadFile")
            .field("byte_count", &self.content.len())
            .finish_non_exhaustive()
    }
}

/// Caller-injected local filesystem; methods must release handles when finished.
///
/// Implementations must bound reads before allocation and preserve binary bytes.
/// Dropping a write is not rollback: it may already have truncated/written a file.
pub trait LocalFileSystem: Send + Sync {
    /// Inspect a path without creating it or enumerating a directory.
    fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>>;

    /// Read a regular file, rejecting a payload larger than `limit`.
    fn read<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>>;

    /// Read exact bounded bytes while retaining a live source-file disposal guard.
    ///
    /// The default explicitly rejects this capability. It must not silently
    /// substitute `read`, whose source handle may already have been closed.
    fn read_for_copy<'a>(
        &'a self,
        _path: &'a Path,
        _limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<LocalReadFile, LocalError>> {
        Box::pin(async { Err(LocalError::new(LocalErrorKind::SourceLeaseUnavailable)) })
    }

    /// Open with binary create/truncate semantics; do not create parent directories.
    fn write<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>>;
}

/// Real OS adapter using an explicit existing Tokio runtime's blocking pool.
///
/// All paths resolve beneath a caller-selected, existing directory. This is an
/// explicit safety bound, not a hostile-process sandbox: concurrent symlink or
/// hard-link substitution cannot be made atomic with portable safe `std::fs`.
/// No runtime, thread pool, current-directory change or directory tree is created.
#[derive(Clone, Debug)]
pub struct OsFileSystem {
    root: PathBuf,
    runtime: Handle,
}

impl OsFileSystem {
    /// Canonicalize the selected boundary on the caller's blocking pool.
    ///
    /// # Errors
    /// The boundary must exist and be a directory; worker/native errors are explicit.
    pub async fn new(root: PathBuf, runtime: Handle) -> Result<Self, LocalError> {
        let canonical = runtime
            .spawn_blocking(move || {
                let root = fs::canonicalize(root).map_err(native)?;
                if !root.is_dir() {
                    return Err(native(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        "Local boundary is not a directory",
                    )));
                }
                Ok(root)
            })
            .await
            .map_err(|_| LocalError::new(LocalErrorKind::WorkerUnavailable))??;
        Ok(Self {
            root: canonical,
            runtime,
        })
    }

    /// Canonical boundary selected by the caller; never a discovered global path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Path, &AtomicBool) -> Result<T, LocalError> + Send + 'static,
    ) -> CommandFuture<'_, Result<T, LocalError>> {
        let root = self.root.clone();
        let runtime = self.runtime.clone();
        Box::pin(async move {
            let stop = Stop(Arc::new(AtomicBool::new(false)));
            let flag = stop.0.clone();
            let result = runtime
                .spawn_blocking(move || {
                    check_stop(&flag)?;
                    operation(&root, &flag)
                })
                .await
                .map_err(|_| LocalError::new(LocalErrorKind::WorkerUnavailable))?;
            drop(stop);
            result
        })
    }
}

impl LocalFileSystem for OsFileSystem {
    fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        let path = path.to_owned();
        self.run(move |root, _| {
            let path = contained(root, &path)?;
            match fs::metadata(path) {
                Ok(value) if value.is_dir() => Ok(LocalKind::Directory),
                Ok(value) if value.is_file() => Ok(LocalKind::File),
                Ok(_) => Ok(LocalKind::Other),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(LocalKind::Missing),
                Err(error) => Err(native(error)),
            }
        })
    }

    fn read<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        Box::pin(async move { Ok(self.read_for_copy(path, limit).await?.content) })
    }

    fn read_for_copy<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<LocalReadFile, LocalError>> {
        let path = path.to_owned();
        self.run(move |root, stop| {
            let mut file = File::open(contained(root, &path)?).map_err(native)?;
            let metadata = file.metadata().map_err(native)?;
            if !metadata.is_file() {
                return Err(native(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Local source is not a regular file",
                )));
            }
            if metadata.len() > limit.get() as u64 {
                return Err(LocalError::new(LocalErrorKind::Limit));
            }
            let mut result = Vec::new();
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                check_stop(stop)?;
                let size = file.read(&mut buffer).map_err(native)?;
                if size == 0 {
                    return Ok(LocalReadFile::new(result, file));
                }
                if size > limit.get().saturating_sub(result.len()) {
                    return Err(LocalError::new(LocalErrorKind::Limit));
                }
                result.extend_from_slice(&buffer[..size]);
            }
        })
    }

    fn write<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        let path = path.to_owned();
        self.run(move |root, stop| {
            let path = contained(root, &path)?;
            check_stop(stop)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .map_err(native)?;
            let mut written = 0;
            let operation = (|| {
                while written < content.len() {
                    check_stop(stop)?;
                    let end = content.len().min(written.saturating_add(16 * 1024));
                    let size = file.write(&content[written..end]).map_err(native)?;
                    if size == 0 {
                        return Err(native(io::Error::from(io::ErrorKind::WriteZero)));
                    }
                    written += size;
                }
                file.flush().map_err(native)?;
                Ok(written)
            })();
            operation.map_err(|mut error: LocalError| {
                error.destination_opened = true;
                error.bytes_written = written;
                error
            })
        })
    }
}

struct Stop(Arc<AtomicBool>);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn native(error: io::Error) -> LocalError {
    LocalError::new(LocalErrorKind::Io(error))
}

fn check_stop(stop: &AtomicBool) -> Result<(), LocalError> {
    if stop.load(Ordering::SeqCst) {
        Err(LocalError::new(LocalErrorKind::Cancelled))
    } else {
        Ok(())
    }
}

fn contained(root: &Path, path: &Path) -> Result<PathBuf, LocalError> {
    if path.as_os_str().is_empty() {
        return Err(native(io::Error::from(io::ErrorKind::NotFound)));
    }
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    // Resolve the nearest existing ancestor, including dangling/outgoing symlinks.
    let mut ancestor = path.as_path();
    let mut tail = Vec::new();
    let canonical = loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => break fs::canonicalize(ancestor).map_err(native)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| LocalError::new(LocalErrorKind::OutsideBoundary))?;
                tail.push(name.to_owned());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| LocalError::new(LocalErrorKind::OutsideBoundary))?;
            }
            Err(error) => return Err(native(error)),
        }
    };
    if !canonical.starts_with(root) {
        return Err(LocalError::new(LocalErrorKind::OutsideBoundary));
    }
    if tail.is_empty() {
        // Keep the caller's spelling for native open/metadata semantics.
        return Ok(path);
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(LocalError::new(LocalErrorKind::OutsideBoundary));
    }
    Ok(path)
}

pub(crate) fn child_path(directory: &Path, name: &str) -> Result<PathBuf, LocalError> {
    let mut components = Path::new(name).components();
    if name.is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
        || (cfg!(windows) && name.contains(':'))
    {
        return Err(LocalError::new(LocalErrorKind::OutsideBoundary));
    }
    Ok(directory.join(name))
}
