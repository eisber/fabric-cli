// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_mutations::CommandFuture;
use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    num::NonZeroUsize,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
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
    /// The caller did not inject the additive tree capability.
    CapabilityUnavailable,
    /// A source changed after it was copied; deletion was refused.
    SourceChanged,
    /// A native filename would address a device, stream or normalized alias.
    InvalidName,
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
            LocalErrorKind::CapabilityUnavailable => "Local tree capability is unavailable".into(),
            LocalErrorKind::SourceChanged => {
                "Source changed after copying; deletion refused".into()
            }
            LocalErrorKind::InvalidName => {
                "Local file name cannot be represented safely on this platform".into()
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

/// One bounded source snapshot entry, relative to the explicitly selected root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEntry {
    /// Empty for the root itself; otherwise validated native relative components.
    pub relative: PathBuf,
    /// Only regular files and directories can enter a tree snapshot.
    pub kind: LocalKind,
    /// Original file size, used to detect source replacement before deletion.
    pub len: u64,
    /// Original modification time, when the platform provides it.
    pub modified: Option<SystemTime>,
}

/// Open version-pinned native source, without a file-sized allocation.
pub trait LocalFileReader: Send {
    /// Length captured when the source lease was opened.
    fn len(&self) -> u64;

    /// Whether the captured source is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read at most 4 MiB at an explicit offset, validating source metadata.
    fn read_at(
        &mut self,
        offset: u64,
        limit: NonZeroUsize,
    ) -> CommandFuture<'_, Result<Vec<u8>, LocalError>>;
}

/// One single-attempt, incremental native destination lease.
///
/// Dropping an in-flight write poisons the lease. No drop flushes, retries,
/// deletes, or rolls back a partially written destination.
pub trait LocalFileWriter: Send {
    /// Append the next bounded chunk at its exact expected position.
    fn append_at(
        &mut self,
        position: u64,
        content: Vec<u8>,
    ) -> CommandFuture<'_, Result<(), LocalError>>;

    /// Flush only after every declared byte was written, including empty files.
    fn finish(&mut self) -> CommandFuture<'_, Result<u64, LocalError>>;
}

/// Caller-injected local filesystem with explicit handle ownership.
///
/// Implementations must bound reads before allocation and preserve binary bytes.
/// Dropping a write is not rollback: it may already have truncated/written a file.
/// Returned incremental leases retain their handles until the caller drops them.
pub trait LocalFileSystem: Send + Sync {
    /// Open an incremental source lease matching a preflight snapshot.
    fn open_reader<'a>(
        &'a self,
        _path: &'a Path,
        _expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileReader>, LocalError>> {
        Box::pin(async { Err(LocalError::new(LocalErrorKind::CapabilityUnavailable)) })
    }

    /// Open a destination without truncating any existing file identity.
    ///
    /// An approved overwrite must replace the directory entry, never mutate an
    /// inode that could also be another source entry through a hard link.
    fn open_writer<'a>(
        &'a self,
        _path: &'a Path,
        _length: u64,
        _overwrite: bool,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileWriter>, LocalError>> {
        Box::pin(async { Err(LocalError::new(LocalErrorKind::CapabilityUnavailable)) })
    }

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

    /// Write a file, atomically refusing an unapproved existing destination.
    fn write_checked<'a>(
        &'a self,
        _path: &'a Path,
        _content: Vec<u8>,
        _overwrite: bool,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        Box::pin(async { Err(LocalError::new(LocalErrorKind::CapabilityUnavailable)) })
    }

    /// Snapshot a complete tree, including its root and empty directories.
    ///
    /// The entry bound is enforced while walking. Links and special files must
    /// fail explicitly, never escape the selected root or become empty files.
    fn snapshot_tree<'a>(
        &'a self,
        _path: &'a Path,
        _max_entries: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<LocalEntry>, LocalError>> {
        Box::pin(async { Err(LocalError::new(LocalErrorKind::CapabilityUnavailable)) })
    }

    /// Resolve a prospective path for same-path/overlap checks without creating it.
    fn canonical_path<'a>(
        &'a self,
        _path: &'a Path,
    ) -> CommandFuture<'a, Result<PathBuf, LocalError>> {
        Box::pin(async { Err(LocalError::new(LocalErrorKind::CapabilityUnavailable)) })
    }

    /// Create exactly one directory, refusing an already-existing path.
    fn create_directory<'a>(
        &'a self,
        _path: &'a Path,
    ) -> CommandFuture<'a, Result<(), LocalError>> {
        Box::pin(async { Err(LocalError::new(LocalErrorKind::CapabilityUnavailable)) })
    }

    /// Remove one copied entry only; directories must be empty.
    ///
    /// File identity metadata is checked immediately before unlinking. Callers
    /// must first finish every copy and revalidate the complete source snapshot.
    /// This operation is never a recursive remove-all or an automatic retry.
    fn remove_copied<'a>(
        &'a self,
        _path: &'a Path,
        _expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<(), LocalError>> {
        Box::pin(async { Err(LocalError::new(LocalErrorKind::CapabilityUnavailable)) })
    }
}

/// Real OS adapter using an explicit existing Tokio runtime's blocking pool.
///
/// All paths resolve beneath a caller-selected, existing directory. This is an
/// explicit safety bound, not a hostile-process sandbox: concurrent symlink or
/// hard-link substitution cannot be made atomic with portable safe `std::fs`.
/// Construction creates no runtime, thread pool, current-directory change or
/// directory tree. Explicitly polled tree methods perform only selected operations.
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

    pub(crate) async fn tree_root_kind(&self, path: &Path) -> Result<LocalKind, LocalError> {
        let path = path.to_owned();
        self.run(move |root, _| {
            let path = contained(root, &path)?;
            match fs::symlink_metadata(path) {
                Ok(metadata) => Ok(snapshot_entry(PathBuf::new(), &metadata)?.kind),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(LocalKind::Missing),
                Err(error) => Err(native(error)),
            }
        })
        .await
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
    fn open_reader<'a>(
        &'a self,
        path: &'a Path,
        expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileReader>, LocalError>> {
        let path = path.to_owned();
        let expected = expected.clone();
        let version = expected.clone();
        let files = self.clone();
        Box::pin(async move {
            let file = self
                .run(move |root, _| {
                    let path = contained(root, &path)?;
                    snapshot_entry(
                        PathBuf::new(),
                        &fs::symlink_metadata(&path).map_err(native)?,
                    )?;
                    #[cfg(windows)]
                    let file = {
                        use std::os::windows::fs::OpenOptionsExt;
                        OpenOptions::new()
                            .read(true)
                            .share_mode(5)
                            .open(path)
                            .map_err(native)?
                    };
                    #[cfg(not(windows))]
                    let file = fs::File::open(path).map_err(native)?;
                    verify_file(&file, &version)?;
                    Ok(file)
                })
                .await?;
            Ok(Box::new(NativeReader {
                files,
                file: Arc::new(Mutex::new(file)),
                expected,
            }) as Box<dyn LocalFileReader>)
        })
    }

    fn open_writer<'a>(
        &'a self,
        path: &'a Path,
        length: u64,
        overwrite: bool,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileWriter>, LocalError>> {
        let path = path.to_owned();
        let files = self.clone();
        Box::pin(async move {
            let file = self
                .run(move |root, stop| {
                    check_native_name(&path)?;
                    let path = contained(root, &path)?;
                    check_stop(stop)?;
                    replace_entry(&path, overwrite)?;
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)
                        .map_err(native)
                })
                .await?;
            Ok(Box::new(NativeWriter {
                files,
                file: Arc::new(Mutex::new(file)),
                length,
                position: 0,
                active: true,
            }) as Box<dyn LocalFileWriter>)
        })
    }

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
            let path = contained(root, &path)?;
            #[cfg(windows)]
            let mut file = {
                use std::os::windows::fs::OpenOptionsExt;
                // Deny data writes, but permit checked unlink while a move's
                // final verification lease is still alive.
                OpenOptions::new()
                    .read(true)
                    .share_mode(5)
                    .open(path)
                    .map_err(native)?
            };
            #[cfg(not(windows))]
            let mut file = fs::File::open(path).map_err(native)?;
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
        self.write_checked(path, content, true)
    }

    fn write_checked<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
        overwrite: bool,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        let path = path.to_owned();
        self.run(move |root, stop| {
            check_native_name(&path)?;
            let path = contained(root, &path)?;
            check_stop(stop)?;
            #[cfg(unix)]
            if let Ok(metadata) = fs::metadata(&path) {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() > 1 {
                    return Err(LocalError::new(LocalErrorKind::OutsideBoundary));
                }
            }
            let mut options = OpenOptions::new();
            options.write(true);
            if overwrite {
                options.create(true).truncate(true);
            } else {
                options.create_new(true);
            }
            let mut file = options.open(path).map_err(native)?;
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

    fn snapshot_tree<'a>(
        &'a self,
        path: &'a Path,
        max_entries: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<LocalEntry>, LocalError>> {
        let path = path.to_owned();
        self.run(move |root, stop| {
            let path = contained(root, &path)?;
            let mut pending = vec![PathBuf::new()];
            let mut result = Vec::new();
            while let Some(relative) = pending.pop() {
                check_stop(stop)?;
                if result.len() >= max_entries.get() || relative.components().count() > 256 {
                    return Err(LocalError::new(LocalErrorKind::Limit));
                }
                let selected = if relative.as_os_str().is_empty() {
                    path.clone()
                } else {
                    path.join(&relative)
                };
                let metadata = fs::symlink_metadata(&selected).map_err(native)?;
                let entry = snapshot_entry(relative.clone(), &metadata)?;
                if entry.kind == LocalKind::Directory {
                    let mut children = Vec::new();
                    for child in fs::read_dir(&selected).map_err(native)? {
                        check_stop(stop)?;
                        if result.len() + pending.len() + children.len() + 1 >= max_entries.get() {
                            return Err(LocalError::new(LocalErrorKind::Limit));
                        }
                        children.push(relative.join(child.map_err(native)?.file_name()));
                    }
                    children.sort();
                    pending.extend(children.into_iter().rev());
                }
                result.push(entry);
            }
            Ok(result)
        })
    }

    fn canonical_path<'a>(
        &'a self,
        path: &'a Path,
    ) -> CommandFuture<'a, Result<PathBuf, LocalError>> {
        let path = path.to_owned();
        self.run(move |root, _| {
            let path = contained(root, &path)?;
            let mut ancestor = path.as_path();
            let mut tail = Vec::new();
            loop {
                match fs::canonicalize(ancestor) {
                    Ok(mut canonical) => {
                        for name in tail.into_iter().rev() {
                            canonical.push(name);
                        }
                        return Ok(canonical);
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        tail.push(
                            ancestor
                                .file_name()
                                .ok_or_else(|| LocalError::new(LocalErrorKind::OutsideBoundary))?
                                .to_owned(),
                        );
                        ancestor = ancestor
                            .parent()
                            .ok_or_else(|| LocalError::new(LocalErrorKind::OutsideBoundary))?;
                    }
                    Err(error) => return Err(native(error)),
                }
            }
        })
    }

    fn create_directory<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<(), LocalError>> {
        let path = path.to_owned();
        self.run(move |root, stop| {
            check_native_name(&path)?;
            let path = contained(root, &path)?;
            check_stop(stop)?;
            fs::create_dir(path).map_err(native)
        })
    }

    fn remove_copied<'a>(
        &'a self,
        path: &'a Path,
        expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<(), LocalError>> {
        let path = path.to_owned();
        let expected = expected.clone();
        self.run(move |root, stop| {
            let path = contained(root, &path)?;
            check_stop(stop)?;
            let metadata = fs::symlink_metadata(&path).map_err(native)?;
            let actual = snapshot_entry(expected.relative.clone(), &metadata)?;
            if actual.kind != expected.kind || actual.kind == LocalKind::File && actual != expected
            {
                return Err(LocalError::new(LocalErrorKind::SourceChanged));
            }
            if actual.kind == LocalKind::Directory {
                fs::remove_dir(path).map_err(native)
            } else {
                fs::remove_file(path).map_err(native)
            }
        })
    }
}

const MAX_CHUNK_BYTES: usize = 4 * 1024 * 1024;

struct NativeReader {
    files: OsFileSystem,
    file: Arc<Mutex<fs::File>>,
    expected: LocalEntry,
}

impl LocalFileReader for NativeReader {
    fn len(&self) -> u64 {
        self.expected.len
    }

    fn read_at(
        &mut self,
        offset: u64,
        limit: NonZeroUsize,
    ) -> CommandFuture<'_, Result<Vec<u8>, LocalError>> {
        let file = self.file.clone();
        let expected = self.expected.clone();
        self.files.run(move |_, stop| {
            if limit.get() > MAX_CHUNK_BYTES || offset > expected.len {
                return Err(LocalError::new(LocalErrorKind::Limit));
            }
            let mut file = file
                .lock()
                .map_err(|_| LocalError::new(LocalErrorKind::WorkerUnavailable))?;
            verify_file(&file, &expected)?;
            let count = usize::try_from((expected.len - offset).min(limit.get() as u64))
                .map_err(|_| LocalError::new(LocalErrorKind::Limit))?;
            let mut content = vec![0; count];
            file.seek(SeekFrom::Start(offset)).map_err(native)?;
            for chunk in content.chunks_mut(16 * 1024) {
                check_stop(stop)?;
                file.read_exact(chunk).map_err(native)?;
            }
            verify_file(&file, &expected)?;
            Ok(content)
        })
    }
}

struct NativeWriter {
    files: OsFileSystem,
    file: Arc<Mutex<fs::File>>,
    length: u64,
    position: u64,
    active: bool,
}

impl LocalFileWriter for NativeWriter {
    fn append_at(
        &mut self,
        position: u64,
        content: Vec<u8>,
    ) -> CommandFuture<'_, Result<(), LocalError>> {
        Box::pin(async move {
            if !self.active
                || position != self.position
                || content.is_empty()
                || content.len() > MAX_CHUNK_BYTES
                || content.len() as u64 > self.length.saturating_sub(position)
            {
                return Err(LocalError::new(LocalErrorKind::Limit));
            }
            self.active = false;
            let file = self.file.clone();
            let count = content.len() as u64;
            self.files
                .run(move |_, stop| {
                    let mut written = 0;
                    let operation = (|| {
                        let mut file = file
                            .lock()
                            .map_err(|_| LocalError::new(LocalErrorKind::WorkerUnavailable))?;
                        file.seek(SeekFrom::Start(position)).map_err(native)?;
                        while written < content.len() {
                            check_stop(stop)?;
                            let end = content.len().min(written.saturating_add(16 * 1024));
                            let size = file.write(&content[written..end]).map_err(native)?;
                            if size == 0 {
                                return Err(native(io::Error::from(io::ErrorKind::WriteZero)));
                            }
                            written += size;
                        }
                        Ok(())
                    })();
                    operation.map_err(|mut error: LocalError| {
                        error.destination_opened = true;
                        error.bytes_written = usize::try_from(position)
                            .unwrap_or(usize::MAX)
                            .saturating_add(written);
                        error
                    })
                })
                .await?;
            self.position += count;
            self.active = true;
            Ok(())
        })
    }

    fn finish(&mut self) -> CommandFuture<'_, Result<u64, LocalError>> {
        Box::pin(async move {
            if !self.active || self.position != self.length {
                return Err(LocalError::new(LocalErrorKind::SourceChanged));
            }
            self.active = false;
            let file = self.file.clone();
            let position = self.position;
            self.files
                .run(move |_, stop| {
                    let operation = (|| {
                        check_stop(stop)?;
                        file.lock()
                            .map_err(|_| LocalError::new(LocalErrorKind::WorkerUnavailable))?
                            .flush()
                            .map_err(native)
                    })();
                    operation.map_err(|mut error: LocalError| {
                        error.destination_opened = true;
                        error.bytes_written = usize::try_from(position).unwrap_or(usize::MAX);
                        error
                    })
                })
                .await?;
            Ok(position)
        })
    }
}

fn verify_file(file: &fs::File, expected: &LocalEntry) -> Result<(), LocalError> {
    if snapshot_entry(expected.relative.clone(), &file.metadata().map_err(native)?)? != *expected {
        return Err(LocalError::new(LocalErrorKind::SourceChanged));
    }
    Ok(())
}

fn replace_entry(path: &Path, overwrite: bool) -> Result<(), LocalError> {
    if overwrite {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if snapshot_entry(PathBuf::new(), &metadata)?.kind != LocalKind::File {
                    return Err(LocalError::new(LocalErrorKind::OutsideBoundary));
                }
                // Unlink only this name. Truncating its identity could corrupt
                // any other source entry hard-linked to it, on Windows or Unix.
                fs::remove_file(path).map_err(native)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(native(error)),
        }
    }
    Ok(())
}

fn snapshot_entry(relative: PathBuf, metadata: &fs::Metadata) -> Result<LocalEntry, LocalError> {
    #[cfg(windows)]
    let link = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let link = metadata.file_type().is_symlink();
    if link {
        return Err(LocalError::new(LocalErrorKind::OutsideBoundary));
    }
    let kind = if metadata.is_file() {
        LocalKind::File
    } else if metadata.is_dir() {
        LocalKind::Directory
    } else {
        return Err(native(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Tree contains a special file",
        )));
    };
    Ok(LocalEntry {
        relative,
        kind,
        len: if kind == LocalKind::File {
            metadata.len()
        } else {
            0
        },
        modified: if kind == LocalKind::File {
            metadata.modified().ok()
        } else {
            None
        },
    })
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
    if !native_name_allowed(name) {
        return Err(LocalError::new(LocalErrorKind::InvalidName));
    }
    Ok(directory.join(name))
}

fn check_native_name(path: &Path) -> Result<(), LocalError> {
    #[cfg(windows)]
    if let Some(name) = path.file_name()
        && name.to_str().is_none_or(|name| !native_name_allowed(name))
    {
        return Err(LocalError::new(LocalErrorKind::InvalidName));
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

fn native_name_allowed(name: &str) -> bool {
    #[cfg(windows)]
    {
        if name.ends_with(['.', ' ']) || name.contains(':') {
            return false;
        }
        let stem = name
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_ascii_lowercase();
        if matches!(
            stem.as_str(),
            "con" | "prn" | "aux" | "nul" | "conin$" | "conout$" | "clock$"
        ) {
            return false;
        }
        if (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(
                &stem[3..],
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        {
            return false;
        }
    }
    #[cfg(not(windows))]
    let _ = name;
    true
}
