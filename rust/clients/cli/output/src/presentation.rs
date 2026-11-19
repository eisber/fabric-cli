// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure host-selected presentation followed by counted, caller-owned delivery.
//!
//! No execution callback, credential, clock, terminal probe or retry is accepted.
//! Native typed evidence survives either rendering or delivery failure.

use crate::{
    Indent, OutputEnvelope, OutputError, OutputStream, RenderPlan,
    outcome::Recovery,
    tabular::{self, TabularError, TabularFormat, TabularTable},
    text::{TextError, TextPlan},
};
use serde_json::Value;
use std::io::{self, Write};

/// Explicit presentation input; arbitrary JSON never selects a tabular codec.
pub enum PresentationSource<'a> {
    /// Source-silent output, distinct from JSON null, an empty array or empty bytes.
    Silent,
    /// Raw JSON with one trailing LF, without envelope normalization.
    Json(&'a Value),
    /// Exact bytes, without newline insertion or text decoding.
    Bytes(&'a [u8]),
    /// Existing Python-compatible envelope and indentation.
    Envelope(&'a OutputEnvelope, Indent),
    /// Existing ordered text/mixed-stream plan.
    Text(&'a TextPlan),
    /// An explicitly declared table, including an explicit empty schema.
    Tabular(&'a TabularTable, TabularFormat),
}

/// Rendering failures remain separate from the operation's status and effects.
#[derive(Debug, thiserror::Error)]
pub enum PresentationError {
    /// Raw JSON serialization failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Existing envelope rendering failed.
    #[error(transparent)]
    Envelope(#[from] OutputError),
    /// Existing text rendering failed, possibly retaining partial source output.
    #[error(transparent)]
    Text(#[from] TextError),
    /// An explicitly selected table codec rejected the data or output budget.
    #[error(transparent)]
    Tabular(#[from] TabularError),
}

/// Render without I/O, preserving existing format-specific bytes.
///
/// Text plans retain their ordered streams. Other sources use the host's
/// selected stream. The process status is explicitly host-owned in either case.
pub fn render(
    source: PresentationSource<'_>,
    stream: OutputStream,
    exit_code: u8,
) -> Result<TextPlan, PresentationError> {
    let payload = match source {
        PresentationSource::Silent => {
            return Ok(TextPlan {
                records: Vec::new(),
                exit_code,
            });
        }
        PresentationSource::Text(plan) => {
            let mut plan = plan.clone();
            plan.exit_code = exit_code;
            return Ok(plan);
        }
        PresentationSource::Json(value) => {
            let mut bytes = serde_json::to_vec(value)?;
            bytes.push(b'\n');
            bytes
        }
        PresentationSource::Bytes(bytes) => bytes.to_vec(),
        PresentationSource::Envelope(envelope, indent) => envelope.render(indent)?.payload,
        PresentationSource::Tabular(table, format) => tabular::encode(table, format)?,
    };
    Ok(TextPlan {
        records: vec![RenderPlan {
            stream,
            exit_code,
            payload,
        }],
        exit_code,
    })
}

/// Original command evidence retained when pure rendering fails.
#[derive(Debug, thiserror::Error)]
#[error("Rendering command output failed: {error}")]
pub struct RenderFailure<T, E> {
    /// Original typed outcome, not a reconstructed error-shaped receipt.
    pub outcome: T,
    /// Actual renderer failure.
    #[source]
    pub error: E,
    /// Recovery constraint of the original operation.
    pub recovery: Recovery,
}

/// Materialized presentation and the unmodified typed command evidence.
#[derive(Debug)]
pub struct PreparedOutput<T> {
    /// Original typed outcome.
    pub outcome: T,
    /// Inspectable bytes, streams and process status.
    pub plan: TextPlan,
    /// Recovery constraint, not changed by presentation or delivery.
    pub recovery: Recovery,
}

impl<T> PreparedOutput<T> {
    /// Pair already-rendered output with its original execution evidence.
    ///
    /// A renderer failure returns that same evidence without calling an operation.
    pub fn new<E>(
        outcome: T,
        rendered: Result<TextPlan, E>,
        recovery: Recovery,
    ) -> Result<Self, Box<RenderFailure<T, E>>> {
        match rendered {
            Ok(plan) => Ok(Self {
                outcome,
                plan,
                recovery,
            }),
            Err(error) => Err(Box::new(RenderFailure {
                outcome,
                error,
                recovery,
            })),
        }
    }

    /// Deliver once to caller-owned streams, retaining partial-write evidence.
    ///
    /// A flush failure does not erase successfully written bytes. Neither a
    /// write nor flush failure changes operation status or implies rollback.
    pub fn deliver(
        self,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
        flush: FlushPolicy,
    ) -> Result<Self, Box<DeliveryFailure<T>>> {
        let mut stdout = CountedWriter {
            inner: stdout,
            written: 0,
        };
        let mut stderr = CountedWriter {
            inner: stderr,
            written: 0,
        };
        let written = self.plan.write_to(&mut stdout, &mut stderr);
        let (result, phase) = match written {
            Err(error) => (Err(error), DeliveryPhase::Write),
            Ok(()) => (
                match flush {
                    FlushPolicy::None => Ok(()),
                    FlushPolicy::Stream(OutputStream::Stdout) => stdout.flush(),
                    FlushPolicy::Stream(OutputStream::Stderr) => stderr.flush(),
                },
                DeliveryPhase::Flush,
            ),
        };
        match result {
            Ok(()) => Ok(self),
            Err(error) => Err(Box::new(DeliveryFailure {
                prepared: self,
                stdout_written: stdout.written,
                stderr_written: stderr.written,
                phase,
                error,
            })),
        }
    }
}

/// Host-owned flush policy, independent of serialization and operation success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlushPolicy {
    /// Preserve a caller whose existing delivery does not flush.
    None,
    /// Flush the selected stream after all ordered records were written.
    Stream(OutputStream),
}

/// Local failure stage, never a remote operation status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryPhase {
    /// A write failed, possibly after some bytes reached either stream.
    Write,
    /// All bytes were written but flushing failed.
    Flush,
}

/// Failed local delivery with original evidence and confirmed byte counts.
#[derive(Debug, thiserror::Error)]
#[error("Writing command output failed: {error}")]
pub struct DeliveryFailure<T> {
    /// Original typed evidence, recovery constraint and complete intended bytes.
    pub prepared: PreparedOutput<T>,
    /// Confirmed stdout bytes accepted before failure.
    pub stdout_written: usize,
    /// Confirmed stderr bytes accepted before failure.
    pub stderr_written: usize,
    /// Whether writing or flushing failed.
    pub phase: DeliveryPhase,
    /// Actual local I/O error.
    #[source]
    pub error: io::Error,
}

struct CountedWriter<'a> {
    inner: &'a mut dyn Write,
    written: usize,
}

impl Write for CountedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.written += count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
