// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit v2 run policy, independent of legacy job wire and wait semantics.
//!
//! Artifact adapters supply native payloads and response interpretation. Hosts
//! inject the existing request context/executor, monotonic time and output sink.
//! There is no credential discovery, HTTP client, runtime, remote cancellation,
//! or mutation retry here. Raw retained evidence is untrusted and not for logs.

mod deadline;
mod preflight;
mod session;

pub use deadline::{RunClock, RunDeadline, RunTimeout};
pub use preflight::{
    RunCapabilities, RunFormat, RunInput, RunInputRule, RunOptions, RunOutput, RunOwner,
    RunOwnerKind, RunPreflight, RunTarget, RunTargetKind,
};
pub use session::{
    RunEvidence, RunHandle, RunHandleKind, RunObservation, RunObserver, RunOutcome, RunSession,
    SchedulerObserver,
};
use std::{
    fmt,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

/// Bounded response evidence available even while a streaming future is pending.
///
/// Native adapters publish actual headers/status and body progress before each
/// suspension. Dropping their future does not discard this independent recorder.
#[derive(Clone)]
pub struct RunResponseRecorder {
    limit: NonZeroUsize,
    observed: Arc<Mutex<RecordedResponse>>,
}

#[derive(Clone, Default)]
pub(crate) struct RecordedResponse {
    pub response: Option<crate::JobResponse>,
    pub complete: bool,
    pub body_omitted: bool,
}

impl RunResponseRecorder {
    pub(crate) fn new(limit: NonZeroUsize) -> Self {
        Self {
            limit,
            observed: Arc::new(Mutex::new(RecordedResponse::default())),
        }
    }

    /// Publish actual observed response coverage without retaining oversized bytes.
    ///
    /// False means headers or body prefix, not a fabricated complete response.
    /// Oversized bodies are omitted while status/headers remain available.
    pub fn record(&self, response: &crate::JobResponse, complete: bool) -> Result<(), RunError> {
        let body_omitted = response.body.len() > self.limit.get();
        let recorded = RecordedResponse {
            response: Some(crate::JobResponse {
                status: response.status,
                body: if body_omitted {
                    Vec::new()
                } else {
                    response.body.clone()
                },
                headers: response.headers.clone(),
            }),
            complete: complete && !body_omitted,
            body_omitted,
        };
        *self.observed.lock().map_err(|_| RunError::Transport)? = recorded;
        if body_omitted {
            Err(RunError::ResponseLimit)
        } else {
            Ok(())
        }
    }

    /// Retain actual headers while explicitly omitting an oversized body.
    ///
    /// Called only after the transport observed the decoded-byte bound being
    /// exceeded; no oversized allocation or fabricated complete empty body.
    pub fn record_omitted(&self, response: &crate::JobResponse) -> Result<(), RunError> {
        *self.observed.lock().map_err(|_| RunError::Transport)? = RecordedResponse {
            response: Some(crate::JobResponse {
                status: response.status,
                headers: response.headers.clone(),
                body: Vec::new(),
            }),
            complete: false,
            body_omitted: true,
        };
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> Result<RecordedResponse, RunError> {
        self.observed
            .lock()
            .map(|recorded| recorded.clone())
            .map_err(|_| RunError::Transport)
    }
}

/// V2 exchange evidence; failed streaming can still expose actual acceptance.
///
/// Partial response fields are actual observed bytes/headers, never fabricated.
/// The foundation enforces its body bound and marks incomplete response coverage.
pub enum RunExchange {
    /// A complete streamed response.
    Complete(crate::JobResponse),
    /// Failed/interrupted exchange with any already observed response evidence.
    Failed {
        /// Original sanitized transport failure.
        error: crate::JobExecutionError,
        /// Actual headers/status and bounded body prefix, if observed.
        observed: Option<crate::JobResponse>,
    },
}

/// Sanitized v2 local/protocol/lifecycle failure, never a provider diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RunError {
    /// A duration is not a positive integer with ms/s/m/h or exceeds 24 hours.
    #[error("invalid run timeout; use a positive integer with ms, s, m or h, at most 24h")]
    InvalidTimeout,
    /// A target identifier would change its route.
    #[error("invalid run target identifier")]
    InvalidTarget,
    /// The artifact owner does not match the selected target.
    #[error("run target has the wrong artifact owner")]
    WrongOwner,
    /// The selected target, wait mode or format is not advertised.
    #[error("unsupported run capability")]
    UnsupportedCapability,
    /// Required input is missing or its native schema is invalid.
    #[error("invalid run input")]
    InvalidInput,
    /// The local output destination failed no-clobber preflight or commit.
    #[error("run output delivery failed")]
    Output,
    /// The monotonic total budget has expired.
    #[error("run deadline elapsed")]
    Deadline,
    /// Local interruption won; no remote cancellation was requested.
    #[error("run was interrupted locally")]
    Interrupted,
    /// The injected executor cannot honor a v2 remaining-budget policy.
    #[error("the injected executor does not support the v2 run budget")]
    UnsupportedExecutor,
    /// The selected endpoint or audience is not the original context.
    #[error("run endpoint or audience does not match the selected context")]
    Context,
    /// The injected credential failed, with no fallback.
    #[error("run credential acquisition failed")]
    Credential,
    /// Transport, streaming or an injected delay failed.
    #[error("run transport or response streaming failed")]
    Transport,
    /// A bounded response exceeded its configured decoded byte limit.
    #[error("run response exceeded its byte limit")]
    ResponseLimit,
    /// An actual HTTP rejection; the response remains in evidence.
    #[error("run request was rejected with HTTP {0}")]
    Http(u16),
    /// Native response, scoped handle or Retry-After is invalid.
    #[error("invalid native run response")]
    Protocol,
    /// A second mutation attempt is forbidden, even after a failed first one.
    #[error("run mutation has already been attempted; replay is forbidden")]
    Replay,
}

/// A non-successful local phase with original remote evidence preserved.
pub struct RunFailure {
    /// Sanitized local failure.
    pub source: RunError,
    /// Snapshot of all already observed bounded remote evidence.
    pub evidence: RunEvidence,
}

impl fmt::Debug for RunFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunFailure")
            .field("source", &self.source)
            .field("evidence", &self.evidence)
            .finish()
    }
}

impl fmt::Display for RunFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}

impl std::error::Error for RunFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
