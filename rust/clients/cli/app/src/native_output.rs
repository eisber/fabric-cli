// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Receipt-neutral owned output with bounded abandonment on the caller's runtime.

use fabric_cli_output::OutputStream;
use std::{
    future::Future,
    io::{self, IsTerminal, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

pub(crate) enum OwnedWrite<T, F> {
    Complete(io::Result<T>),
    Deadline,
    Cancelled(F),
    WorkerFailed,
}

pub(crate) async fn write_owned<W, T, F>(
    writer: W,
    deadline: Instant,
    cancelled: impl Future<Output = F>,
    write: impl FnOnce(&mut W) -> io::Result<T> + Send + 'static,
) -> OwnedWrite<T, F>
where
    W: Write + Send + 'static,
    T: Send + 'static,
{
    // Existing hosts permit an already-started/queued output worker to finish.
    // Runtime reads explicitly supply a stop latch to select stricter admission.
    write_owned_with_policy(writer, deadline, cancelled, None, write).await
}

pub(crate) async fn write_owned_with_policy<W, T, F>(
    mut writer: W,
    deadline: Instant,
    cancelled: impl Future<Output = F>,
    stopped: Option<Arc<AtomicBool>>,
    write: impl FnOnce(&mut W) -> io::Result<T> + Send + 'static,
) -> OwnedWrite<T, F>
where
    W: Write + Send + 'static,
    T: Send + 'static,
{
    if Instant::now() >= deadline {
        return OwnedWrite::Deadline;
    }
    let mut cancelled = std::pin::pin!(cancelled);
    let ready = if stopped.is_some() {
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(match cancelled.as_mut().poll(cx) {
                std::task::Poll::Ready(failure) => Some(failure),
                std::task::Poll::Pending => None,
            })
        })
        .await
    } else {
        None
    };
    if let Some(failure) = ready {
        return OwnedWrite::Cancelled(failure);
    }
    let worker_stop = stopped.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        if worker_stop.is_some_and(|stop| stop.load(Ordering::SeqCst) || Instant::now() >= deadline)
        {
            return Err(io::ErrorKind::Interrupted.into());
        }
        write(&mut writer)
    });
    struct StopWorker {
        stopped: Arc<AtomicBool>,
        abort: tokio::task::AbortHandle,
    }
    impl Drop for StopWorker {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::SeqCst);
            self.abort.abort();
        }
    }
    let _stop = stopped.map(|stopped| StopWorker {
        stopped,
        abort: worker.abort_handle(),
    });
    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline.into()) => OwnedWrite::Deadline,
        failure = cancelled.as_mut() => OwnedWrite::Cancelled(failure),
        result = &mut worker => match result {
            Ok(result) => OwnedWrite::Complete(result),
            Err(_) => OwnedWrite::WorkerFailed,
        },
    }
}

pub(crate) fn owned_stream(stream: OutputStream, ascii_only: bool) -> io::Result<std::fs::File> {
    // Abandoned workers must never retain Rust's global stdout/stderr lock.
    #[cfg(windows)]
    let file = {
        use std::os::windows::io::AsHandle;
        let handle = match stream {
            OutputStream::Stdout => io::stdout().as_handle().try_clone_to_owned(),
            OutputStream::Stderr => io::stderr().as_handle().try_clone_to_owned(),
        }?;
        std::fs::File::from(handle)
    };
    #[cfg(unix)]
    let file = {
        use std::os::fd::AsFd;
        let handle = match stream {
            OutputStream::Stdout => io::stdout().as_fd().try_clone_to_owned(),
            OutputStream::Stderr => io::stderr().as_fd().try_clone_to_owned(),
        }?;
        std::fs::File::from(handle)
    };
    #[cfg(not(any(windows, unix)))]
    return Err(io::Error::from(io::ErrorKind::Unsupported));
    #[cfg(any(windows, unix))]
    {
        if cfg!(windows) && !ascii_only && file.is_terminal() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        Ok(file)
    }
}
