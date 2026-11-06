// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One actual framed GET observation composed with native IPC decode and output.

use super::{
    NativeIpcDecoder,
    output::{IpcOutputLimits, LocalIpcCompletion, NativeIpcOutput},
};
use crate::{
    livy_arrow::{
        ArrowError, DecodeLimits, FramedResultStream, FramedStreamError, FramedStreamProgress,
        FramedStreamState, ResumeCursor, StatementHandle, TypedDecoder,
    },
    runtime::NativeError,
};
use std::{
    fmt, io,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

/// Why the representation failed; driver errors retain their original evidence.
#[derive(Clone, Debug)]
pub enum IpcResultCause {
    /// Actual HTTP, framing, replay, decoding or acknowledged sink failure.
    Stream(FramedStreamError),
    /// Native IPC or local output failure outside an acknowledgement.
    Arrow(ArrowError),
    /// The original local budget expired, including during synchronous output.
    Native(NativeError),
    /// A valid empty framed response supplied no schema at all.
    MissingSchema,
}

/// Partial observation and output evidence, without raw payloads or destinations.
#[derive(Clone)]
pub struct IpcResultError(Arc<Failure>);

struct Failure {
    cause: IpcResultCause,
    progress: FramedStreamProgress,
    output_bytes: usize,
    handle: StatementHandle,
    cursor: Option<ResumeCursor>,
}

impl IpcResultError {
    /// Original failure, not a generic sink-error replacement.
    pub fn cause(&self) -> &IpcResultCause {
        &self.0.cause
    }

    /// Exact progress at the failure/closure boundary.
    pub fn progress(&self) -> FramedStreamProgress {
        self.0.progress
    }

    /// Bytes accepted by the caller's sink, including possibly partial writes.
    pub fn output_bytes(&self) -> usize {
        self.0.output_bytes
    }

    /// Original owner binding, never inferred from an output file or response.
    pub fn handle(&self) -> &StatementHandle {
        &self.0.handle
    }

    /// Last completely acknowledged chunk, never a durable checkpoint.
    pub fn cursor(&self) -> Option<&ResumeCursor> {
        self.0.cursor.as_ref()
    }
}

impl fmt::Debug for IpcResultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IpcResultError")
            .field("cause", &self.0.cause)
            .field("progress", &self.0.progress)
            .field("output_bytes", &self.0.output_bytes)
            .field("has_cursor", &self.0.cursor.is_some())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for IpcResultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "IPC result observation failed: {:?}",
            self.0.cause
        )
    }
}

impl std::error::Error for IpcResultError {}

/// Complete local representation, not proof the overall remote workload succeeded.
#[derive(Clone, Debug)]
pub struct IpcResultReceipt {
    /// Validated framed terminal plus actual HTTP EOF and acknowledged delivery.
    pub stream: FramedStreamProgress,
    /// One locally finished IPC stream and successful flush.
    pub output: LocalIpcCompletion,
}

struct Observation<'a, 'host>(&'a mut FramedResultStream<'host>);

impl Observation<'_, '_> {
    fn error(&mut self, cause: IpcResultCause, bytes: usize) -> IpcResultError {
        self.0.close();
        IpcResultError(Arc::new(Failure {
            cause,
            progress: self.0.progress(),
            output_bytes: bytes,
            handle: self.0.handle().clone(),
            cursor: self.0.cursor().cloned(),
        }))
    }
}

impl Drop for Observation<'_, '_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

struct ObservedWrite<'a, W: Write + ?Sized> {
    inner: &'a mut W,
    bytes: &'a AtomicUsize,
}

impl<W: Write + ?Sized> Write for ObservedWrite<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        if count > bytes.len() {
            return Err(io::Error::other("invalid IPC sink write count"));
        }
        self.bytes.fetch_add(count, Ordering::Relaxed);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Consume one fresh framed GET stream into a single standard Arrow IPC stream.
///
/// The caller supplies a fresh synchronous sink and separately bounded transport,
/// frame, decoded-data and output policies. Nothing opens files or submits code.
/// One chunk is validated, decoded and written before acknowledgement advances;
/// replay filtering occurs in the existing stream ledger, before the callback.
/// Schema-only chunks preserve empty known schemas; no chunks is an explicit
/// missing-schema error, never an invented empty schema.
///
/// Any failure closes local observation and leaves caller-owned partial bytes.
/// Output is finalized only after valid framed completion and actual HTTP EOF.
/// A flush/deadline failure may occur after EOS bytes were accepted; neither
/// rollback, durable storage nor complete remote workload success is implied.
/// Dropping this future closes the response and drops the unfinished writer.
///
/// Native decoding and `Write` are synchronous: hosts must offload blocking
/// sinks. Original-deadline checks bracket this work but cannot preempt it.
/// Encoder/dictionary scratch and sink-owned buffers are not a process-memory cap.
///
/// # Errors
/// Retains the exact stream/native/output cause, partial bytes, acknowledged
/// cursor and progress. Failed output is never replayed or automatically finished.
pub async fn write_ipc_result<W: Write + ?Sized>(
    stream: &mut FramedResultStream<'_>,
    sink: &mut W,
    decode_limits: DecodeLimits,
    output_limits: IpcOutputLimits,
) -> Result<IpcResultReceipt, IpcResultError> {
    let mut observation = Observation(stream);
    for result in [
        decode_limits.validate().map(|_| ()),
        output_limits.validate().map(|_| ()),
    ] {
        result.map_err(|error| observation.error(IpcResultCause::Arrow(error), 0))?;
    }
    if observation.0.progress().state != FramedStreamState::Ready
        || observation.0.progress().open_attempts != 0
    {
        return Err(observation.error(IpcResultCause::Arrow(ArrowError::InvalidState), 0));
    }
    let mut decoder = NativeIpcDecoder::new();
    let schema = match observation.0.next_chunk().await {
        Ok(Some(chunk)) => decoder
            .inspect_schema(chunk, decode_limits)
            .map_err(|error| IpcResultCause::Arrow(error)),
        Ok(None) => Err(IpcResultCause::MissingSchema),
        Err(error) => Err(IpcResultCause::Stream(error)),
    }
    .map_err(|cause| observation.error(cause, 0))?;
    observation
        .0
        .remaining_budget()
        .map_err(|error| observation.error(IpcResultCause::Native(error), 0))?;
    let bytes = AtomicUsize::new(0);
    let mut counted = ObservedWrite {
        inner: sink,
        bytes: &bytes,
    };
    let mut output =
        NativeIpcOutput::new(&mut counted, schema.clone(), output_limits).map_err(|error| {
            observation.error(IpcResultCause::Arrow(error), bytes.load(Ordering::Relaxed))
        })?;
    let handle = observation.0.handle().clone();
    loop {
        observation
            .0
            .acknowledge(&handle, |chunk| {
                let observed_schema = decoder.inspect_schema(chunk, decode_limits)?;
                if observed_schema != schema {
                    return Err(ArrowError::SchemaConflict);
                }
                decoder.decode(chunk, decode_limits, &mut |batch| output.write_batch(batch))
            })
            .map_err(|error| {
                observation.error(IpcResultCause::Stream(error), bytes.load(Ordering::Relaxed))
            })?;
        match observation.0.next_chunk().await {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(error) => {
                return Err(
                    observation.error(IpcResultCause::Stream(error), bytes.load(Ordering::Relaxed))
                );
            }
        }
    }
    observation.0.remaining_budget().map_err(|error| {
        observation.error(IpcResultCause::Native(error), bytes.load(Ordering::Relaxed))
    })?;
    let completed = output.finish().map_err(|error| {
        observation.error(IpcResultCause::Arrow(error), bytes.load(Ordering::Relaxed))
    })?;
    observation.0.remaining_budget().map_err(|error| {
        observation.error(IpcResultCause::Native(error), bytes.load(Ordering::Relaxed))
    })?;
    Ok(IpcResultReceipt {
        stream: observation.0.progress(),
        output: completed,
    })
}
