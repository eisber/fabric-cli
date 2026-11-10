// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded, explicitly approved MirroredDatabase replication stop.
//!
//! The caller owns credentials, execution, cancellation and the total deadline.
//! Every request uses [`CompletionPolicy::ReturnAccepted`]; this module never
//! follows a monitor, polls, retries a write, or installs an HTTP client/runtime.
//! Existing v1 mirroring operations and their state policy remain unchanged.
//!
//! Approval records explicit caller intent, not provider authorization or a bypass
//! of the host's `ApprovalPolicy`. The host must enforce its full effect and
//! authorization policy before granting that intent. Shared descriptors,
//! `PreparedOperation`, `ApprovedOperation` and native wiring remain outside this
//! module; the SDK facade's `mirroring_stop` adapter supplies those boundaries.
//! This library does not install an executable or MCP host.
//!
//! Stopping replication does not delete the item, shut down the source database,
//! or guarantee draining or zero cost. For Azure SQL-backed mirrors, stopping and
//! starting mirroring fully reinitializes replication; this is not a claim about
//! every mirror provider.

use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde::Serialize;
use std::fmt;

/// Exact workspace and MirroredDatabase identifiers, without name discovery.
///
/// Construction validates both GUIDs locally and preserves their case spelling.
/// No unchecked constructor or deserialization can bypass that validation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MirroringTarget {
    workspace_id: String,
    item_id: String,
}

impl MirroringTarget {
    /// Validate two exact 36-character hexadecimal GUIDs without performing I/O.
    ///
    /// # Errors
    /// Returns `InvalidInput` for names, paths, URLs or non-GUID identifiers.
    pub fn new(workspace_id: &str, item_id: &str) -> Result<Self, StopError> {
        if !is_guid(workspace_id) || !is_guid(item_id) {
            return Err(StopError::local(
                "InvalidInput",
                "Mirroring stop requires exact workspace and item GUIDs",
            ));
        }
        Ok(Self {
            workspace_id: workspace_id.into(),
            item_id: item_id.into(),
        })
    }

    /// Return the workspace GUID exactly as supplied.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Return the MirroredDatabase GUID exactly as supplied.
    pub fn item_id(&self) -> &str {
        &self.item_id
    }
}

fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Explicit mutation intent; the default does not permit a stop submission.
///
/// This is not provider authorization or a substitute for host `ApprovalPolicy`.
/// The host must enforce its full effect and authorization policy before setting
/// `approved` to true.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct StopOptions {
    /// Record explicit intent for one stop submission from exact Running.
    ///
    /// Never grants provider permissions or bypasses the host's approval policy.
    pub approved: bool,
}

/// Exact provider mirroring states; unknown strings are invalid responses.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum MirroringState {
    /// Replication is initializing.
    Initializing,
    /// Replication has initialized.
    Initialized,
    /// Replication is starting.
    Starting,
    /// Replication is running; stopping requires explicit approval.
    Running,
    /// Replication is paused, not confirmed stopped.
    Paused,
    /// A stop is already in progress, not confirmed complete.
    Stopping,
    /// Replication was observed stopped.
    Stopped,
}

impl MirroringState {
    /// Return the exact, case-sensitive provider spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initializing => "Initializing",
            Self::Initialized => "Initialized",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Stopping => "Stopping",
            Self::Stopped => "Stopped",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "Initializing" => Some(Self::Initializing),
            "Initialized" => Some(Self::Initialized),
            "Starting" => Some(Self::Starting),
            "Running" => Some(Self::Running),
            "Paused" => Some(Self::Paused),
            "Stopping" => Some(Self::Stopping),
            "Stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

/// Distinct stop outcomes, never inferred from request acceptance alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum StopOutcome {
    /// Preflight observed Stopped; no mutation was submitted.
    AlreadyStopped,
    /// Preflight observed Stopping; no mutation or new operation was submitted.
    AlreadyStopping,
    /// The stop was accepted, without waiting or confirming Stopped.
    Accepted,
    /// The request completed, but Stopped has not been confirmed.
    RequestCompleted,
    /// A completed stop request was followed by an exact Stopped observation.
    Stopped,
}

impl StopOutcome {
    /// Return the stable outcome spelling, independent of provider data.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyStopped => "AlreadyStopped",
            Self::AlreadyStopping => "AlreadyStopping",
            Self::Accepted => "Accepted",
            Self::RequestCompleted => "RequestCompleted",
            Self::Stopped => "Stopped",
        }
    }
}

/// Actual target, submission and most recent valid observation evidence.
///
/// Native request data is intentionally not serialized or formatted implicitly.
/// A front-end must select safe fields rather than echo arbitrary service data.
#[derive(Clone, PartialEq)]
pub struct StopReceipt {
    /// The exact caller-supplied target.
    pub target: MirroringTarget,
    /// Whether this was a no-op, acceptance, completed request or verified stop.
    pub outcome: StopOutcome,
    /// Most recent valid state, possibly only the preflight observation.
    ///
    /// This is not a claim about the current state after a failed readback.
    pub observed_state: Option<MirroringState>,
    /// The actual stop submission outcome; absent when no stop was submitted.
    ///
    /// Accepted operation identifiers and completed response data are retained
    /// verbatim for SDK callers, not invented or used to start a monitor.
    pub request_outcome: Option<ResourceOutcome>,
    /// Whether a stop mutation may have reached the service, not a rollback claim.
    pub mutation_may_have_occurred: bool,
}

impl StopReceipt {
    /// Return true only when an exact Stopped state was actually observed.
    pub fn stopped_confirmed(&self) -> bool {
        self.observed_state == Some(MirroringState::Stopped)
    }
}

impl fmt::Debug for StopReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StopReceipt")
            .field("target", &self.target)
            .field("outcome", &self.outcome)
            .field("observed_state", &self.observed_state)
            .field("has_request_outcome", &self.request_outcome.is_some())
            .field(
                "mutation_may_have_occurred",
                &self.mutation_may_have_occurred,
            )
            .finish_non_exhaustive()
    }
}

/// A stable local failure with optional native evidence, never provider text.
///
/// `Display` and `Debug` are sanitized. Native evidence remains available through
/// explicit accessors and must not be blindly included in front-end diagnostics.
pub struct StopError {
    code: &'static str,
    message: &'static str,
    /// Whether a stop mutation may have reached the service.
    ///
    /// A status-read failure cannot establish that a stop was submitted. The
    /// native source's separate execution evidence is retained without alteration.
    pub mutation_may_have_occurred: bool,
    resource_error: Option<Box<ResourceError>>,
    receipt: Option<Box<StopReceipt>>,
    response_outcome: Option<Box<ResourceOutcome>>,
}

impl StopError {
    /// Return a stable local classification, independent of provider error codes.
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// Return a static diagnostic with no supplied values or service text.
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// Return the stop mutation uncertainty separately from native read evidence.
    pub const fn mutation_may_have_occurred(&self) -> bool {
        self.mutation_may_have_occurred
    }

    /// Return the unchanged native executor/service failure, when one occurred.
    pub fn resource_error(&self) -> Option<&ResourceError> {
        self.resource_error.as_deref()
    }

    /// Return the submission receipt retained after a later observation failure.
    ///
    /// An unexpected completed submission response also retains its actual
    /// outcome here, without claiming that the mirroring stop succeeded.
    pub fn receipt(&self) -> Option<&StopReceipt> {
        self.receipt.as_deref()
    }

    /// Return the native response rejected by state or response validation.
    ///
    /// This preserves unexpected acceptance, HTTP status and malformed parsed
    /// data for SDK inspection, without exposing them through error formatting.
    pub fn response_outcome(&self) -> Option<&ResourceOutcome> {
        self.response_outcome.as_deref()
    }

    fn local(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            mutation_may_have_occurred: false,
            resource_error: None,
            receipt: None,
            response_outcome: None,
        }
    }

    fn native(code: &'static str, message: &'static str, source: ResourceError) -> Self {
        Self {
            resource_error: Some(Box::new(source)),
            ..Self::local(code, message)
        }
    }

    fn response(mut self, outcome: ResourceOutcome) -> Self {
        self.response_outcome = Some(Box::new(outcome));
        self
    }

    fn after_submission(mut self, receipt: StopReceipt) -> Self {
        self.mutation_may_have_occurred = true;
        self.receipt = Some(Box::new(receipt));
        self
    }
}

impl fmt::Display for StopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl fmt::Debug for StopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StopError")
            .field("code", &self.code)
            .field(
                "mutation_may_have_occurred",
                &self.mutation_may_have_occurred,
            )
            .field("has_resource_error", &self.resource_error.is_some())
            .field("has_receipt", &self.receipt.is_some())
            .field("has_response_outcome", &self.response_outcome.is_some())
            .finish_non_exhaustive()
    }
}

impl std::error::Error for StopError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.resource_error()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

fn operation(target: &MirroringTarget, action: &str) -> Result<ResourceOperation, StopError> {
    ResourceOperation::new(
        ResourceMethod::Post,
        &[
            "workspaces",
            target.workspace_id(),
            "mirroredDatabases",
            target.item_id(),
            action,
        ],
        None,
        CompletionPolicy::ReturnAccepted,
    )
    .map_err(|source| {
        StopError::native(
            "InvalidInput",
            "Mirroring stop requires a valid resource operation",
            source,
        )
    })
}

#[derive(Clone, Copy)]
enum ReadStage {
    Preflight,
    Readback,
}

async fn read_status(
    executor: &mut dyn ResourceOperations,
    target: &MirroringTarget,
    stage: ReadStage,
) -> Result<(MirroringState, ResourceOutcome), StopError> {
    let response = executor
        .execute(operation(target, "getMirroringStatus")?)
        .await
        .map_err(|source| match stage {
            ReadStage::Preflight => StopError::native(
                "PreconditionFailed",
                "Mirroring status preflight failed; no stop was submitted",
                source,
            ),
            ReadStage::Readback => StopError::native(
                "ReadbackFailed",
                "Mirroring status readback failed; stop submission evidence is retained",
                source,
            ),
        })?;
    let state = match &response {
        ResourceOutcome::Completed {
            status: 200,
            data: Some(data),
        } => data
            .as_object()
            .filter(|object| object.get("error").is_none_or(serde_json::Value::is_null))
            .and_then(|object| object.get("status"))
            .and_then(serde_json::Value::as_str)
            .and_then(MirroringState::parse),
        _ => None,
    };
    match state {
        Some(state) => Ok((state, response)),
        None => Err(StopError::local(
            "InvalidResponse",
            match stage {
                ReadStage::Preflight => {
                    "Mirroring status preflight returned an invalid response; no stop was submitted"
                }
                ReadStage::Readback => {
                    "Mirroring status readback returned an invalid response; stop submission evidence is retained"
                }
            },
        )
        .response(response)),
    }
}

/// Stop replication using one preflight, at most one write and one readback.
///
/// Exact Stopped and Stopping are no-write outcomes, even without approval.
/// Only exact Running permits a write, and only with `approved: true` recording
/// explicit intent. The caller must enforce full host effect and authorization
/// policy before granting that intent; this flag never grants provider access or
/// bypasses host `ApprovalPolicy`.
/// Acceptance returns immediately, retaining any actual operation identifier.
/// Only a completed HTTP 200 stop triggers one readback; only exact Stopped
/// confirms the stop. Other valid readback states are observations, not success.
///
/// Every call borrows the same caller-owned executor and budget. Dropping this
/// future does not roll back a dispatched mutation or run cleanup/replay. The
/// caller/executor remains responsible for preserving evidence on interruption
/// and output failure; do not retry a write merely because no receipt arrived.
///
/// # Errors
/// Returns `ApprovalRequired` for unapproved Running, `InvalidState` for other
/// known preflight states, and `InvalidResponse` for unexpected status/shape/state.
/// Native failures use `PreconditionFailed`, `SubmissionFailed` or `ReadbackFailed`.
/// Failures after a completed submission retain its receipt and mutation
/// uncertainty; no error causes a second write or monitor-following.
pub async fn stop(
    executor: &mut dyn ResourceOperations,
    target: &MirroringTarget,
    options: StopOptions,
) -> Result<StopReceipt, StopError> {
    let (state, preflight) = read_status(executor, target, ReadStage::Preflight).await?;
    let outcome = match state {
        MirroringState::Stopped => StopOutcome::AlreadyStopped,
        MirroringState::Stopping => StopOutcome::AlreadyStopping,
        MirroringState::Running if options.approved => StopOutcome::RequestCompleted,
        MirroringState::Running => {
            return Err(StopError::local(
                "ApprovalRequired",
                "Stopping running mirroring requires explicit approval",
            )
            .response(preflight));
        }
        _ => {
            return Err(StopError::local(
                "InvalidState",
                "Mirroring cannot be stopped from the observed preflight state",
            )
            .response(preflight));
        }
    };
    let mut receipt = StopReceipt {
        target: target.clone(),
        outcome,
        observed_state: Some(state),
        request_outcome: None,
        mutation_may_have_occurred: false,
    };
    if matches!(
        outcome,
        StopOutcome::AlreadyStopped | StopOutcome::AlreadyStopping
    ) {
        return Ok(receipt);
    }

    let submitted = executor
        .execute(operation(target, "stopMirroring")?)
        .await
        .map_err(|source| {
            let mutation_may_have_occurred = source.mutation_may_have_occurred;
            let mut error = StopError::native(
                "SubmissionFailed",
                "Mirroring stop submission failed; inspect native mutation evidence before any retry",
                source,
            );
            error.mutation_may_have_occurred = mutation_may_have_occurred;
            error
        })?;
    receipt.mutation_may_have_occurred = true;
    receipt.request_outcome = Some(submitted.clone());
    match submitted {
        ResourceOutcome::Accepted { .. } => {
            receipt.outcome = StopOutcome::Accepted;
            Ok(receipt)
        }
        ResourceOutcome::Completed { status: 200, .. } => {
            let (observed, _) = read_status(executor, target, ReadStage::Readback)
                .await
                .map_err(|error| error.after_submission(receipt.clone()))?;
            receipt.observed_state = Some(observed);
            if observed == MirroringState::Stopped {
                receipt.outcome = StopOutcome::Stopped;
            }
            Ok(receipt)
        }
        response => Err(StopError::local(
            "InvalidResponse",
            "Mirroring stop returned an unexpected response; submission evidence is retained",
        )
        .response(response)
        .after_submission(receipt)),
    }
}
