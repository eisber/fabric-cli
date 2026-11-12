// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Receipt-neutral owned output with bounded abandonment on the caller's runtime.

use fabric_cli_output::OutputStream;
use std::{
    future::Future,
    io::{self, IsTerminal, Write},
    time::Instant,
};

pub(crate) enum OwnedWrite<T, F> {
    Complete(io::Result<T>),
    Deadline,
    Cancelled(F),
    WorkerFailed,
}

pub(crate) async fn write_owned<W, T, F>(
    mut writer: W,
    deadline: Instant,
    cancelled: impl Future<Output = F>,
    write: impl FnOnce(&mut W) -> io::Result<T> + Send + 'static,
) -> OwnedWrite<T, F>
where
    W: Write + Send + 'static,
    T: Send + 'static,
{
    if Instant::now() >= deadline {
        return OwnedWrite::Deadline;
    }
    let worker = tokio::task::spawn_blocking(move || write(&mut writer));
    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline.into()) => OwnedWrite::Deadline,
        failure = cancelled => OwnedWrite::Cancelled(failure),
        result = worker => match result {
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
