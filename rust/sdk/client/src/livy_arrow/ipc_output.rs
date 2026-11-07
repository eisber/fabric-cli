// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One native IPC stream over a caller-owned synchronous sink.

use super::super::ArrowError;
use super::{NativeIpcSchema, native_error};
use arrow_array::RecordBatch;
use arrow_ipc::writer::{DictionaryHandling, IpcWriteOptions, StreamWriter};
use std::fmt;
use std::io::{self, Write};

/// Positive finite limits for one locally encoded output stream.
///
/// Byte accounting includes schema, dictionaries, padding, batches and EOS.
/// These limits do not bound native encoder scratch space or total process
/// memory. Input decoding must retain its separate `DecodeLimits`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpcOutputLimits {
    /// Maximum bytes accepted by the sink adapter, including the final EOS.
    pub max_output_bytes: usize,
    /// Maximum cumulative rows; zero-row batches do not consume this budget.
    pub max_rows: usize,
    /// Maximum cumulative batches, including zero-row batches.
    pub max_batches: usize,
}

impl IpcOutputLimits {
    pub(super) fn validate(self) -> Result<Self, ArrowError> {
        if self.max_output_bytes == 0 || self.max_rows == 0 || self.max_batches == 0 {
            return Err(ArrowError::InvalidInput);
        }
        Ok(self)
    }
}

/// Successful local EOS encoding and sink flush, not remote completion.
///
/// This says nothing about workload, framed-v2, HTTP, durable storage, or host
/// publication completion. A host must establish those conditions separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalIpcCompletion {
    /// Bytes accepted by the sink, including the schema and EOS.
    pub output_bytes: usize,
    /// Rows in successfully encoded batches.
    pub rows: usize,
    /// Successfully encoded batches.
    pub batches: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Open,
    Failed,
    Finished,
}

/// Encode independently decoded batches as exactly one standard Arrow stream.
///
/// The explicit schema must come from the native decoder's bounded inspection;
/// it is never inferred from absent data. Equality includes recursive dictionary
/// ordering, ignoring only stream-local dictionary IDs. The writer emits one
/// initial schema, canonical output dictionary IDs, replacement dictionaries
/// when needed, and exactly one EOS on successful [`Self::finish`].
///
/// The sink remains caller-owned. Calls apply synchronous backpressure; hosts
/// must offload blocking sinks rather than block an async executor. No file,
/// runtime, credential, global state, or result collection is installed here.
///
/// Any failed operation permanently poisons this instance, even if it failed
/// before writing bytes. Constructor failure may also leave partial output.
/// Abort/drop never finishes, deletes, rolls back, or certifies output. A sink
/// that buffers data may independently flush on its own eventual drop.
///
/// Native writer scratch and dictionary-tracker array/buffer references may
/// outlive a batch, including references sharing input body storage.
/// The byte cap is enforced before forwarding each write to the sink, not as a
/// preallocation bound on Apache's encoding buffers or a total-memory limit.
pub struct NativeIpcOutput<'a, W: Write + ?Sized> {
    writer: StreamWriter<CappedWrite<'a, W>>,
    schema: NativeIpcSchema,
    limits: IpcOutputLimits,
    rows: usize,
    batches: usize,
    state: State,
}

impl<W: Write + ?Sized> fmt::Debug for NativeIpcOutput<'_, W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeIpcOutput")
            .field("state", &self.state)
            .field("output_bytes", &self.bytes_written())
            .field("rows", &self.rows)
            .field("batches", &self.batches)
            .finish_non_exhaustive()
    }
}

impl<'a, W: Write + ?Sized> NativeIpcOutput<'a, W> {
    /// Begin one uncompressed IPC V5 stream with an explicitly observed schema.
    ///
    /// The caller must provide a fresh output destination; existing sink bytes
    /// are neither inspected nor truncated and are outside this byte counter.
    /// Writes the schema immediately. Invalid limits write nothing; an encoding
    /// or sink failure can leave partial schema bytes in the caller's sink.
    pub fn new(
        sink: &'a mut W,
        schema: NativeIpcSchema,
        limits: IpcOutputLimits,
    ) -> Result<Self, ArrowError> {
        let limits = limits.validate()?;
        let capped = CappedWrite {
            sink,
            limit: limits.max_output_bytes,
            written: 0,
            failure: None,
        };
        let writer = StreamWriter::try_new_with_options(
            capped,
            schema.native_schema(),
            IpcWriteOptions::default().with_dictionary_handling(DictionaryHandling::Resend),
        )
        .map_err(output_error)?;
        Ok(Self {
            writer,
            schema,
            limits,
            rows: 0,
            batches: 0,
            state: State::Open,
        })
    }

    /// Encode one batch in order without retaining a result collection.
    ///
    /// Schema/row/batch checks precede encoding. A mismatch or any failure
    /// poisons the composer; never retry the affected output.
    pub fn write_batch(&mut self, batch: RecordBatch) -> Result<(), ArrowError> {
        self.require_open()?;
        self.state = State::Failed;
        if !self.schema.matches_native(batch.schema().as_ref()) {
            return Err(ArrowError::SchemaConflict);
        }
        i64::try_from(batch.num_rows()).map_err(|_| ArrowError::Limit)?;
        let rows = self
            .rows
            .checked_add(batch.num_rows())
            .filter(|rows| *rows <= self.limits.max_rows)
            .ok_or(ArrowError::Limit)?;
        let batches = self
            .batches
            .checked_add(1)
            .filter(|batches| *batches <= self.limits.max_batches)
            .ok_or(ArrowError::Limit)?;
        if let Err(error) = self.writer.write(&batch) {
            return Err(self.encoding_error(error));
        }
        self.rows = rows;
        self.batches = batches;
        self.state = State::Open;
        Ok(())
    }

    /// Write the single EOS and flush; report only local encoding completion.
    ///
    /// A flush error still fails even if the sink already accepted a valid EOS.
    /// Repeated finish, or any operation after failure/abort, is invalid.
    pub fn finish(&mut self) -> Result<LocalIpcCompletion, ArrowError> {
        self.require_open()?;
        self.state = State::Failed;
        if let Err(error) = self.writer.finish() {
            return Err(self.encoding_error(error));
        }
        self.state = State::Finished;
        Ok(LocalIpcCompletion {
            output_bytes: self.bytes_written(),
            rows: self.rows,
            batches: self.batches,
        })
    }

    /// Permanently abandon unfinished output without writing or flushing.
    ///
    /// Already-written bytes remain caller-owned partial output. The caller
    /// decides whether/how to discard it; this never deletes or rolls it back.
    pub fn abort(&mut self) -> Result<(), ArrowError> {
        self.require_open()?;
        self.state = State::Failed;
        Ok(())
    }

    /// Bytes accepted so far, including partial writes; not a completion signal.
    pub fn bytes_written(&self) -> usize {
        self.writer.get_ref().written
    }

    fn require_open(&self) -> Result<(), ArrowError> {
        if self.state != State::Open {
            return Err(ArrowError::InvalidState);
        }
        Ok(())
    }

    fn encoding_error(&self, error: arrow_schema::ArrowError) -> ArrowError {
        self.writer
            .get_ref()
            .failure
            .unwrap_or_else(|| output_error(error))
    }
}

fn output_error(error: arrow_schema::ArrowError) -> ArrowError {
    match error {
        arrow_schema::ArrowError::IoError(_, error)
            if error.kind() == io::ErrorKind::FileTooLarge =>
        {
            ArrowError::Limit
        }
        arrow_schema::ArrowError::IoError(_, _) => ArrowError::SinkFailed,
        other => native_error(other),
    }
}

struct CappedWrite<'a, W: Write + ?Sized> {
    sink: &'a mut W,
    limit: usize,
    written: usize,
    failure: Option<ArrowError>,
}

impl<W: Write + ?Sized> CappedWrite<'_, W> {
    fn fail(&mut self, error: ArrowError) -> io::Error {
        self.failure.get_or_insert(error);
        let kind = if self.failure == Some(ArrowError::Limit) {
            io::ErrorKind::FileTooLarge
        } else {
            io::ErrorKind::Other
        };
        io::Error::new(kind, "IPC output failed")
    }
}

impl<W: Write + ?Sized> Write for CappedWrite<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(self.fail(ArrowError::InvalidState));
        }
        if self
            .written
            .checked_add(bytes.len())
            .is_none_or(|end| end > self.limit)
        {
            return Err(self.fail(ArrowError::Limit));
        }
        match self.sink.write(bytes) {
            Ok(count) if count <= bytes.len() && (count != 0 || bytes.is_empty()) => {
                self.written += count;
                Ok(count)
            }
            Ok(_) | Err(_) => Err(self.fail(ArrowError::SinkFailed)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.failure.is_some() {
            return Err(self.fail(ArrowError::InvalidState));
        }
        match self.sink.flush() {
            Ok(()) => Ok(()),
            Err(_) => Err(self.fail(ArrowError::SinkFailed)),
        }
    }
}
