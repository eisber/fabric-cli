// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit native read composition, without credential or endpoint discovery.
//!
//! The caller must supply an executor authorized for the exact selected SDK
//! scope. This host does not make Fabric `/v1` credentials valid for MWC.

use super::{
    OutputSink, OwnerPath, ParsedInvocation, ReadError, ReadErrorKind, ReadOptions, ReadReceipt,
    ResolvedTarget, RuntimeHead, RuntimeResolver, TypedPath,
};
use fabric_cli_output::OutputStream;
use fabric_sdk::runtime::{NativeError, NativeExecutor};
use std::{
    future::Future,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};

#[path = "../native_output.rs"]
mod owned_output;

/// One caller-selected human owner and its actual SDK scope; no name search.
pub struct ExplicitResolver {
    owner: OwnerPath,
    target: ResolvedTarget,
}

impl ExplicitResolver {
    /// Bind one typed owner to a caller-authorized SDK object, without I/O.
    ///
    /// # Errors
    /// Rejects inconsistent type/suffixes and invalid human paths.
    pub fn new(owner: OwnerPath, target: ResolvedTarget) -> Result<Self, ReadError> {
        let path = TypedPath::parse(&format!("/{}/{}", owner.workspace, owner.artifact))?;
        if path.owner != owner || target.kind() != owner.kind {
            return Err(ReadError::new(ReadErrorKind::WrongOwner));
        }
        Ok(Self { owner, target })
    }
}

impl RuntimeResolver for ExplicitResolver {
    fn resolve(&self, path: &TypedPath) -> Result<ResolvedTarget, NativeError> {
        if path.owner != self.owner {
            return Err(NativeError::InvalidInput);
        }
        Ok(match &self.target {
            ResolvedTarget::Notebook(value) => ResolvedTarget::Notebook(value.clone()),
            ResolvedTarget::Lakehouse(value) => ResolvedTarget::Lakehouse(value.clone()),
            ResolvedTarget::LivyService(value) => ResolvedTarget::LivyService(value.clone()),
        })
    }
}

/// Sanitized host failure, independent of provider/source payloads.
pub struct HostError {
    /// Read/validation/interruption category.
    pub kind: ReadErrorKind,
    /// Real HTTP status, when a completed read supplied one.
    pub http_status: Option<u16>,
    /// Whether a completed remote observation exists, not proof of delivery.
    pub read_completed: bool,
    /// Whether this host created a destination file.
    pub file_created: bool,
    /// Bytes accepted by the destination before local failure/interruption.
    pub bytes_written: usize,
    /// Full completed observation retained for caller-owned reconciliation only.
    ///
    /// This can contain sensitive source/outputs. Diagnostics never serialize it.
    /// Retention is not proof of delivery and never authorizes replay.
    pub receipt: Option<Box<ReadReceipt>>,
}

impl From<ReadError> for HostError {
    fn from(error: ReadError) -> Self {
        Self {
            kind: error.kind,
            http_status: error.http_status,
            read_completed: error.receipt.is_some(),
            file_created: false,
            bytes_written: 0,
            receipt: error.receipt,
        }
    }
}

impl std::fmt::Debug for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostError")
            .field("kind", &self.kind)
            .field("http_status", &self.http_status)
            .field("read_completed", &self.read_completed)
            .field("file_created", &self.file_created)
            .field("bytes_written", &self.bytes_written)
            .field("receipt_retained", &self.receipt.is_some())
            .finish()
    }
}

impl HostError {
    fn new(kind: ReadErrorKind) -> Self {
        ReadError::new(kind).into()
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native runtime read failed: {:?}", self.kind)
    }
}

impl std::error::Error for HostError {}

/// A preflighted explicit output destination; existing files are never replaced.
pub struct Destination {
    label: String,
    target: Target,
}

enum Target {
    Stdout(std::fs::File),
    File(PathBuf),
}

impl Destination {
    /// Check an output selection without creating a file or accessing identity.
    ///
    /// This performs filesystem calls. Native front ends must call it on an
    /// owned worker under their original deadline, as [`prepare_destination`] does.
    ///
    /// # Errors
    /// Rejects unavailable streams, existing paths, missing parents and unsafe labels.
    pub fn check(label: &str, cwd: &Path) -> io::Result<Self> {
        if label.is_empty() || label.len() > 4096 || label.chars().any(char::is_control) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let target = if label == "-" {
            // Preserve UTF-8 bytes; the existing policy rejects raw Windows consoles.
            Target::Stdout(owned_output::owned_stream(OutputStream::Stdout, false)?)
        } else {
            let path = cwd.join(label);
            match path.symlink_metadata() {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
                Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
            }
            if !path.parent().is_some_and(Path::is_dir) {
                return Err(io::ErrorKind::NotFound.into());
            }
            Target::File(path)
        };
        Ok(Self {
            label: label.to_owned(),
            target,
        })
    }
}

/// Preflight output on a worker under the original whole-command deadline.
///
/// # Errors
/// Returns interruption or unavailable-output errors without HTTP or file creation.
pub async fn prepare_destination(
    label: String,
    cwd: PathBuf,
    deadline: Instant,
    cancelled: impl Future<Output = ()>,
) -> Result<Destination, HostError> {
    before(deadline, cancelled, async {
        tokio::task::spawn_blocking(move || Destination::check(&label, &cwd))
            .await
            .map_err(|_| HostError::new(ReadErrorKind::OutputUnavailable))?
            .map_err(|_| HostError::new(ReadErrorKind::OutputUnavailable))
    })
    .await
}

/// One complete observation with an owned, bounded JSON buffer, not yet delivered.
pub struct PendingRead {
    receipt: ReadReceipt,
    bytes: Vec<u8>,
}

/// Execute the existing SDK head once under the original deadline/cancellation.
///
/// Output must already be preflighted. No endpoint, credentials, retry, polling,
/// remote cancel or leaf-specific engine is installed by this composition seam.
///
/// # Errors
/// Invalid input, provider failures, budgets and local interruption remain errors.
pub async fn acquire(
    invocation: &ParsedInvocation,
    executor: &dyn NativeExecutor,
    resolver: &dyn RuntimeResolver,
    options: ReadOptions,
    destination: &Destination,
    deadline: Instant,
    cancelled: impl Future<Output = ()>,
) -> Result<PendingRead, HostError> {
    let mut buffer = Buffer {
        label: &destination.label,
        bytes: Vec::new(),
    };
    let receipt = before(deadline, cancelled, async {
        RuntimeHead::inject(executor, resolver, options)?
            .run(invocation, &mut buffer)
            .await
            .map_err(HostError::from)
    })
    .await?;
    Ok(PendingRead {
        receipt,
        bytes: buffer.bytes,
    })
}

async fn before<T>(
    deadline: Instant,
    cancelled: impl Future<Output = ()>,
    work: impl Future<Output = Result<T, HostError>>,
) -> Result<T, HostError> {
    if Instant::now() >= deadline {
        return Err(HostError::new(ReadErrorKind::Timeout));
    }
    let result = tokio::select! {
        biased;
        _ = cancelled => Err(HostError::new(ReadErrorKind::Cancelled)),
        _ = tokio::time::sleep_until(deadline.into()) => Err(HostError::new(ReadErrorKind::Timeout)),
        result = work => result,
    };
    // A synchronous provider/serializer may finish late in the same poll.
    if Instant::now() >= deadline {
        return Err(HostError::new(ReadErrorKind::Timeout));
    }
    result
}

impl PendingRead {
    /// Deliver once to an owned stream or create-new file under the same deadline.
    ///
    /// A failed/abandoned worker may have written a prefix. It never retains
    /// Rust's global stream locks and is never replayed. File creation is checked
    /// atomically again here. Native callers must bound runtime shutdown too.
    ///
    /// # Errors
    /// Returns explicit partial-delivery evidence without echoing content or paths.
    pub async fn deliver(
        self,
        destination: Destination,
        deadline: Instant,
        cancelled: impl Future<Output = ()>,
    ) -> Result<ReadReceipt, HostError> {
        let created = Arc::new(AtomicBool::new(false));
        let written = Arc::new(AtomicUsize::new(0));
        let worker_created = Arc::clone(&created);
        let worker_written = Arc::clone(&written);
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stopped);
        let bytes = self.bytes;
        let result = owned_output::write_owned_stoppable(
            io::sink(),
            deadline,
            cancelled,
            stopped,
            move |_| {
                if Instant::now() >= deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                if worker_stop.load(Ordering::SeqCst) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let mut writer = match destination.target {
                    Target::Stdout(file) => file,
                    Target::File(path) => {
                        let file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(path)?;
                        worker_created.store(true, Ordering::SeqCst);
                        file
                    }
                };
                write_counted(&mut writer, &bytes, deadline, &worker_written, &worker_stop)
            },
        )
        .await;
        let kind = match result {
            owned_output::OwnedWrite::Complete(Ok(())) if Instant::now() < deadline => {
                return Ok(self.receipt);
            }
            owned_output::OwnedWrite::Deadline => ReadErrorKind::Timeout,
            owned_output::OwnedWrite::Cancelled(()) => ReadErrorKind::Cancelled,
            owned_output::OwnedWrite::Complete(Err(ref error))
                if error.kind() == io::ErrorKind::TimedOut =>
            {
                ReadErrorKind::Timeout
            }
            _ if Instant::now() >= deadline => ReadErrorKind::Timeout,
            _ => ReadErrorKind::Delivery,
        };
        Err(HostError {
            kind,
            http_status: Some(self.receipt.meta.http_status),
            read_completed: true,
            file_created: created.load(Ordering::SeqCst),
            bytes_written: written.load(Ordering::SeqCst),
            receipt: Some(Box::new(self.receipt)),
        })
    }
}

fn write_counted(
    writer: &mut impl Write,
    mut bytes: &[u8],
    deadline: Instant,
    written: &AtomicUsize,
    stopped: &AtomicBool,
) -> io::Result<()> {
    while !bytes.is_empty() {
        if stopped.load(Ordering::SeqCst) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match writer.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => {
                written.fetch_add(count, Ordering::SeqCst);
                bytes = &bytes[count..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    if stopped.load(Ordering::SeqCst) || Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    writer.flush()
}

struct Buffer<'a> {
    label: &'a str,
    bytes: Vec<u8>,
}

impl OutputSink for Buffer<'_> {
    fn check(&mut self, destination: &str) -> io::Result<()> {
        if destination != self.label {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(())
    }

    fn write(&mut self, destination: &str, receipt: &[u8]) -> io::Result<()> {
        self.check(destination)?;
        if !self.bytes.is_empty() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        self.bytes.extend_from_slice(receipt);
        Ok(())
    }
}

/// Emit one small sanitized diagnostic on owned stderr, with no blocking fallback.
///
/// # Errors
/// Returns failure when stderr cannot finish within the original deadline.
pub async fn diagnostic(error: &HostError, deadline: Instant) -> io::Result<()> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "error": {"code": error.kind, "message": "Native runtime read did not complete delivery."},
        "httpStatus": error.http_status,
        "readCompleted": error.read_completed,
        "fileCreated": error.file_created,
        "bytesWrittenAtObservation": error.bytes_written,
        "remoteCancelled": false,
        "remoteReplayed": false
    }))?;
    write_stream(OutputStream::Stderr, bytes, deadline).await
}

/// Write bounded ASCII help/configuration diagnostics without global stream locks.
///
/// # Errors
/// Rejects non-ASCII/oversized text and unavailable, failed or blocked streams.
pub async fn write_stream(
    stream: OutputStream,
    mut bytes: Vec<u8>,
    deadline: Instant,
) -> io::Result<()> {
    if bytes.len() > 16 * 1024 || !bytes.is_ascii() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    bytes.push(b'\n');
    let file = owned_output::owned_stream(stream, true)?;
    match owned_output::write_owned(file, deadline, std::future::pending::<()>(), move |file| {
        write_counted(
            file,
            &bytes,
            deadline,
            &AtomicUsize::new(0),
            &AtomicBool::new(false),
        )
    })
    .await
    {
        owned_output::OwnedWrite::Complete(result) if Instant::now() < deadline => result,
        _ => Err(io::ErrorKind::TimedOut.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::pending, sync::mpsc, time::Duration};

    #[test]
    fn partial_writer_retains_exact_observed_prefix_without_replay() {
        struct ShortWriter(usize);
        impl Write for ShortWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.0 != 0 {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                self.0 += 1;
                Ok(bytes.len().min(2))
            }
            fn flush(&mut self) -> io::Result<()> {
                panic!("failed output must not be flushed or replayed");
            }
        }
        let count = AtomicUsize::new(0);
        assert!(
            write_counted(
                &mut ShortWriter(0),
                b"receipt",
                Instant::now() + Duration::from_secs(1),
                &count,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn blocked_owned_writer_is_abandoned_with_no_synchronous_diagnostic_fallback() {
        struct BlockingWriter(mpsc::Receiver<()>);
        impl Write for BlockingWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.recv().unwrap();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (release, wait) = mpsc::channel();
        let result = owned_output::write_owned(
            BlockingWriter(wait),
            Instant::now() + Duration::from_millis(20),
            pending::<()>(),
            |writer| writer.write_all(b"one bounded diagnostic"),
        )
        .await;
        assert!(matches!(result, owned_output::OwnedWrite::Deadline));
        release.send(()).unwrap();
    }

    #[tokio::test]
    async fn already_cancelled_output_does_not_start_a_worker() {
        let result = owned_output::write_owned(
            io::sink(),
            Instant::now() + Duration::from_secs(1),
            async {},
            |_| -> io::Result<()> { panic!("no worker after latched cancellation") },
        )
        .await;
        assert!(matches!(result, owned_output::OwnedWrite::Cancelled(())));
    }
}
