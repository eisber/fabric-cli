// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private, protocol-independent one-shot macOS helper ownership.

#[cfg(target_os = "macos")]
use ring::digest::SHA256;
#[cfg(all(feature = "interactive", any(target_os = "macos", test)))]
use std::path::Path;
use std::{
    fmt,
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::OwnedSemaphorePermit;
#[cfg(test)]
use tokio::sync::oneshot;
#[cfg(feature = "certificate-store-macos")]
use tokio::time::{Instant, sleep_until};

pub(crate) const MAC_HELPER_WIRE_LIMIT: usize = 64 * 1024;
const STDERR_LIMIT: usize = 4096;
pub(crate) const CLEANUP_WAIT: Duration = Duration::from_secs(5);
pub(crate) const MAX_DEADLINE: Duration = Duration::from_secs(120);
#[cfg(target_os = "macos")]
const HELPER_FILE_LIMIT: u64 = 64 * 1024 * 1024;
pub(crate) const CHILD_POLL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HelperError {
    InvalidInput,
    #[cfg(target_os = "macos")]
    HelperTrust,
    Helper,
    Cancelled,
    Deadline,
    CleanupPending,
    #[cfg(not(target_os = "macos"))]
    UnsupportedPlatform,
}

#[derive(Clone)]
pub(crate) struct Helper {
    pub(crate) executable: PathBuf,
    pub(crate) sha256: [u8; 32],
    pub(crate) deadline: Duration,
}

impl Helper {
    pub(crate) fn new(
        executable: PathBuf,
        sha256: [u8; 32],
        deadline: Duration,
    ) -> Result<Self, HelperError> {
        if !executable.is_absolute()
            || executable.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
            || executable
                .to_str()
                .is_none_or(|path| path.chars().any(char::is_control))
            || deadline.is_zero()
            || deadline > MAX_DEADLINE
        {
            return Err(HelperError::InvalidInput);
        }
        Ok(Self {
            executable,
            sha256,
            deadline,
        })
    }
}

impl fmt::Debug for Helper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Helper").finish_non_exhaustive()
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn validate_helper(helper: &Helper) -> Result<(), HelperError> {
    use ring::digest::Context;
    use std::{fs, os::unix::fs::MetadataExt};

    let rejected = || HelperError::HelperTrust;
    for ancestor in helper.executable.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(|_| rejected())?;
        if metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || (ancestor != helper.executable && !metadata.is_dir())
        {
            return Err(rejected());
        }
    }
    let before = fs::symlink_metadata(&helper.executable).map_err(|_| rejected())?;
    if !before.is_file()
        || before.nlink() != 1
        || before.mode() & 0o111 == 0
        || before.len() == 0
        || before.len() > HELPER_FILE_LIMIT
    {
        return Err(rejected());
    }
    let mut file = fs::File::open(&helper.executable).map_err(|_| rejected())?;
    let opened = file.metadata().map_err(|_| rejected())?;
    let same = |other: &fs::Metadata| {
        before.dev() == other.dev()
            && before.ino() == other.ino()
            && before.uid() == other.uid()
            && before.mode() == other.mode()
            && before.nlink() == other.nlink()
            && before.len() == other.len()
            && before.mtime() == other.mtime()
            && before.mtime_nsec() == other.mtime_nsec()
            && before.ctime() == other.ctime()
            && before.ctime_nsec() == other.ctime_nsec()
    };
    if !same(&opened) {
        return Err(rejected());
    }
    let mut context = Context::new(&SHA256);
    let mut buffer = [0; 8192];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer).map_err(|_| rejected())?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > HELPER_FILE_LIMIT {
            return Err(rejected());
        }
        context.update(&buffer[..count]);
    }
    let after = fs::symlink_metadata(&helper.executable).map_err(|_| rejected())?;
    if total != before.len()
        || !same(&file.metadata().map_err(|_| rejected())?)
        || !same(&after)
        || context.finish().as_ref() != &helper.sha256[..]
    {
        return Err(rejected());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn validate_helper(_: &Helper) -> Result<(), HelperError> {
    Err(HelperError::UnsupportedPlatform)
}

#[cfg(feature = "certificate-store-macos")]
pub(crate) async fn run_owned_exchange<T>(
    mut command: Command,
    request: Vec<u8>,
    deadline: Instant,
    publication: &mut tokio::sync::oneshot::Sender<T>,
    permit: OwnedSemaphorePermit,
    setup: impl FnOnce(&mut OwnedHelperChild, Vec<u8>) -> Result<(), HelperError> + Send + 'static,
) -> Result<Vec<u8>, HelperError> {
    let cancellation = Cancellation::new();
    let control = cancellation.flag();
    let mut operation = tokio::task::spawn_blocking(move || {
        blocking_exchange(
            &mut command,
            request,
            deadline.into_std(),
            control,
            permit,
            setup,
        )
    });
    tokio::select! {
        biased;
        _ = publication.closed() => Err(HelperError::Cancelled),
        _ = sleep_until(deadline) => Err(HelperError::Deadline),
        result = &mut operation => result.map_err(|_| HelperError::CleanupPending)?,
    }
}

pub(crate) struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Used only inside an owned blocking task that also owns the session file lock.
#[cfg(all(feature = "interactive", any(target_os = "macos", test)))]
pub(crate) fn trusted_blocking_exchange(
    helper: &Helper,
    request: Vec<u8>,
    deadline: std::time::Instant,
    cancelled: Arc<AtomicBool>,
    permit: OwnedSemaphorePermit,
    wire_limit: usize,
) -> Result<Vec<u8>, HelperError> {
    if request.is_empty() || request.len() > wire_limit || wire_limit > 384 * 1024 {
        return Err(HelperError::InvalidInput);
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(HelperError::Cancelled);
    }
    if std::time::Instant::now() >= deadline {
        return Err(HelperError::Deadline);
    }
    validate_helper(helper)?;
    let mut command = Command::new(&helper.executable);
    command.env_clear().current_dir(Path::new("/"));
    blocking_exchange(
        &mut command,
        request,
        deadline,
        cancelled,
        permit,
        move |child, bytes| {
            child.wire_limit = wire_limit;
            child.start_pipes(bytes)
        },
    )
}

pub(crate) fn read_bounded(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, HelperError> {
    let mut output = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let count = reader.read(&mut chunk).map_err(|_| HelperError::Helper)?;
        if count == 0 {
            return Ok(output);
        }
        if output.len() + count > limit {
            return Err(HelperError::Helper);
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

// Reserve BEFORE spawn; spawn uncertainty or unwinding quarantines the slot.
pub(crate) struct ChildPermit(pub(crate) Option<OwnedSemaphorePermit>);

impl ChildPermit {
    fn release(mut self) {
        drop(self.0.take());
    }
}

impl Drop for ChildPermit {
    fn drop(&mut self) {
        if let Some(permit) = self.0.take() {
            permit.forget();
        }
    }
}

fn io_driver_preflight() -> Result<(), HelperError> {
    // Register before spawn; no datagrams are sent and no global hooks changed.
    std::panic::catch_unwind(|| {
        tokio::runtime::Handle::try_current().map_err(|_| HelperError::Helper)?;
        let socket =
            std::net::UdpSocket::bind(("127.0.0.1", 0)).map_err(|_| HelperError::Helper)?;
        socket
            .set_nonblocking(true)
            .map_err(|_| HelperError::Helper)?;
        let socket = tokio::net::UdpSocket::from_std(socket).map_err(|_| HelperError::Helper)?;
        drop(socket);
        Ok(())
    })
    .map_err(|_| HelperError::Helper)?
}

type PipeResult = Result<Vec<u8>, HelperError>;

struct PipeWorker {
    output: bool,
    thread: Option<JoinHandle<PipeResult>>,
}

pub(crate) struct OwnedHelperChild {
    pub(crate) child: Child,
    permit: Option<ChildPermit>,
    pipes: Vec<PipeWorker>,
    pub(crate) exit: Option<ExitStatus>,
    output: Option<Vec<u8>>,
    cleanup_attempted: bool,
    wire_limit: usize,
    #[cfg(test)]
    pub(crate) exit_notice: Option<oneshot::Sender<ExitStatus>>,
}

impl OwnedHelperChild {
    pub(crate) fn spawn(
        command: &mut Command,
        permit: OwnedSemaphorePermit,
    ) -> Result<Self, HelperError> {
        io_driver_preflight()?;
        let permit = ChildPermit(Some(permit));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // std-first ownership precedes ANY fallible pipe worker registration.
        let child = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| command.spawn()))
            .map_err(|_| HelperError::CleanupPending)?
            .map_err(|_| HelperError::CleanupPending)?;
        Ok(Self {
            child,
            permit: Some(permit),
            pipes: Vec::new(),
            exit: None,
            output: None,
            cleanup_attempted: false,
            wire_limit: MAC_HELPER_WIRE_LIMIT,
            #[cfg(test)]
            exit_notice: None,
        })
    }

    pub(crate) fn add_pipe(
        &mut self,
        output: bool,
        work: impl FnOnce() -> PipeResult + Send + 'static,
    ) -> Result<(), HelperError> {
        if self.pipes.len() >= 3 {
            return Err(HelperError::Helper);
        }
        let thread = thread::Builder::new()
            .name("fabric-keychain-pipe".into())
            .spawn(work)
            .map_err(|_| HelperError::Helper)?;
        self.pipes.push(PipeWorker {
            output,
            thread: Some(thread),
        });
        Ok(())
    }

    pub(crate) fn start_pipes(&mut self, request: Vec<u8>) -> Result<(), HelperError> {
        let mut stdin = self.child.stdin.take().ok_or(HelperError::Helper)?;
        let stdout = self.child.stdout.take().ok_or(HelperError::Helper)?;
        let stderr = self.child.stderr.take().ok_or(HelperError::Helper)?;
        self.add_pipe(false, move || {
            stdin.write_all(&request).map_err(|_| HelperError::Helper)?;
            drop(stdin);
            Ok(Vec::new())
        })?;
        let limit = self.wire_limit;
        self.add_pipe(true, move || read_bounded(stdout, limit))?;
        self.add_pipe(false, move || {
            if read_bounded(stderr, STDERR_LIMIT)?.is_empty() {
                Ok(Vec::new())
            } else {
                Err(HelperError::Helper)
            }
        })
    }

    #[cfg(all(test, feature = "interactive"))]
    pub(crate) fn set_wire_limit(&mut self, limit: usize) {
        self.wire_limit = limit;
    }

    fn poll_exit(&mut self) -> Result<(), HelperError> {
        if self.exit.is_none() {
            self.exit = self.child.try_wait().map_err(|_| HelperError::Helper)?;
            #[cfg(test)]
            if let Some(status) = self.exit
                && let Some(notice) = self.exit_notice.take()
            {
                let _ = notice.send(status);
            }
        }
        Ok(())
    }

    fn poll_pipes(&mut self) -> Result<bool, HelperError> {
        let mut error = None;
        for pipe in &mut self.pipes {
            if pipe.thread.as_ref().is_some_and(JoinHandle::is_finished) {
                let result = pipe
                    .thread
                    .take()
                    .ok_or(HelperError::Helper)?
                    .join()
                    .unwrap_or(Err(HelperError::Helper));
                match result {
                    Ok(bytes) if pipe.output => self.output = Some(bytes),
                    Ok(_) => {}
                    Err(failure) => error = Some(failure),
                }
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(self.pipes.iter().all(|pipe| pipe.thread.is_none())),
        }
    }

    fn release_if_finished(&mut self) -> bool {
        if self.exit.is_some() && self.pipes.iter().all(|pipe| pipe.thread.is_none()) {
            if let Some(permit) = self.permit.take() {
                permit.release();
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn cleanup(&mut self) -> Result<(), HelperError> {
        if self.permit.is_none() {
            return Ok(());
        }
        if self.cleanup_attempted {
            return Err(HelperError::CleanupPending);
        }
        self.cleanup_attempted = true;
        drop(self.child.stdin.take());
        drop(self.child.stdout.take());
        drop(self.child.stderr.take());
        if self.exit.is_none() {
            let _ = self.child.kill();
        }
        let deadline = std::time::Instant::now() + CLEANUP_WAIT;
        loop {
            let _ = self.poll_exit();
            let _ = self.poll_pipes();
            if self.release_if_finished() {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                // Unconfirmed exit or detached pipe threads quarantine capacity.
                return Err(HelperError::CleanupPending);
            }
            thread::sleep(CHILD_POLL);
        }
    }
}

impl Drop for OwnedHelperChild {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

pub(crate) fn blocking_exchange(
    command: &mut Command,
    request: Vec<u8>,
    deadline: std::time::Instant,
    cancelled: Arc<AtomicBool>,
    permit: OwnedSemaphorePermit,
    setup: impl FnOnce(&mut OwnedHelperChild, Vec<u8>) -> Result<(), HelperError>,
) -> Result<Vec<u8>, HelperError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(HelperError::Cancelled);
    }
    if std::time::Instant::now() >= deadline {
        return Err(HelperError::Deadline);
    }
    let mut child = OwnedHelperChild::spawn(command, permit)?;
    let result = (|| {
        setup(&mut child, request)?;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(HelperError::Cancelled);
            }
            if std::time::Instant::now() >= deadline {
                return Err(HelperError::Deadline);
            }
            child.poll_exit()?;
            let pipes_finished = child.poll_pipes()?;
            if let Some(status) = child.exit {
                if !status.success() {
                    return Err(HelperError::Helper);
                }
                if pipes_finished {
                    return child.output.take().ok_or(HelperError::Helper);
                }
            }
            thread::sleep(CHILD_POLL);
        }
    })();
    child.cleanup()?;
    result
}
