// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Managed, once-only execution on an injected persistent JSON Notebook channel.
//!
//! Translates FNTK `e250d5d32b04efcc5623a414a361727a93c271ce`,
//! `client/ws_client.py::_handle_message`, `execute_code`, `cancel_execution`,
//! and `client/jupyter_protocol.py`. The caller must supply an already-ready
//! session, established control comm, authenticated channel and runtime alarm.
//! No WebSocket transport, startup, discovery, reconnect or replay is installed.

use super::{
    CommId, ExecutionId, MessageId, NotebookRuntime, ResumeBinding,
    protocol::{
        ControlRequest, ExecuteRequest, ExecuteStatus, ExecutionState, ExecutionTracker,
        LiveOutput, Observation, ProtocolLimits, SynapseControl,
    },
};
use crate::runtime::{NativeDeadline, NativeError, NativeFuture};
use std::{fmt, future::poll_fn, task::Poll};

/// Caller-attested ready session and established comm, scoped to one Notebook owner.
///
/// Construction is not readiness discovery: REST Jupyter creation alone is
/// insufficient. The host must attest this binding from its actual ready channel.
#[derive(Clone, Eq, PartialEq)]
pub struct ReadySession {
    origin: crate::runtime::NativeEndpoint,
    capacity: String,
    workspace: String,
    notebook: String,
    session: ResumeBinding,
    control_comm: CommId,
}

impl ReadySession {
    /// Bind an explicitly ready session/kernel/comm to the selected Notebook owner.
    pub fn new(notebook: &NotebookRuntime, session: ResumeBinding, control_comm: CommId) -> Self {
        Self {
            origin: notebook.endpoint.clone(),
            capacity: notebook.capacity.clone(),
            workspace: notebook.workspace.clone(),
            notebook: notebook.notebook.clone(),
            session,
            control_comm,
        }
    }

    /// Existing Jupyter and kernel identifiers; no Livy ID substitution is allowed.
    pub fn session(&self) -> &ResumeBinding {
        &self.session
    }

    /// Actual established Synapse control comm, not an execute request ID.
    pub fn control_comm(&self) -> &CommId {
        &self.control_comm
    }
}

impl fmt::Debug for ReadySession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReadySession(<owner-bound, caller-attested>)")
    }
}

/// Caller-owned persistent authenticated Notebook JSON channel.
///
/// Implementations must enforce the supplied absolute deadline while awaiting
/// I/O, not only after return, and must be cancellation-safe on future drop.
/// The driver additionally races every operation against an independent alarm.
/// Dropping the channel releases local socket resources only, never interrupts,
/// cancels, deletes or restarts remote compute. Its binding must remain immutable.
pub trait NotebookChannel: Send {
    /// Actual authenticated owner/session/kernel/comm attached to this channel.
    fn binding(&self) -> &ReadySession;

    /// Send this complete bounded JSON frame once, without backend retries.
    ///
    /// Any error or future drop may occur after a partial/complete remote send.
    fn send<'a>(
        &'a mut self,
        frame: &'a [u8],
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<(), NativeError>>;

    /// Pull one complete JSON frame, or `None` at actual channel closure.
    ///
    /// Enforce `max_bytes` before allocation and do not precollect/read ahead.
    /// Unsupported binary framing must be an explicit error, not decoded as JSON.
    fn receive<'a>(
        &'a mut self,
        max_bytes: usize,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<Option<Vec<u8>>, NativeError>>;

    /// Close this local channel only, without remote cancellation or session cleanup.
    ///
    /// The channel must also release local resources if this future is dropped.
    fn close<'a>(
        &'a mut self,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<(), NativeError>>;
}

/// Alarm supplied by the caller's existing async runtime; no runtime is installed.
pub trait DeadlineAlarm: Send + Sync {
    /// Wake and complete at the supplied deadline, never early.
    ///
    /// Implementations must register a runtime timer/waker using the remaining
    /// original budget. A pending I/O future need not wake itself for timeout.
    fn wait<'a>(&'a self, deadline: &'a NativeDeadline) -> NativeFuture<'a, ()>;
}

/// Backpressured destination for already-bounded live output records.
pub trait OutputSink: Send {
    /// Consume one output before the driver receives another frame.
    ///
    /// The record's encoded content is at most `max_bytes`, within the driver's
    /// total output budget. Implementations must bound their own buffering and
    /// deadline-bound any I/O. A failed/dropped write may have partially consumed
    /// the record: the driver poisons continuation and never redelivers it.
    fn write<'a>(
        &'a mut self,
        index: usize,
        output: &'a LiveOutput,
        max_bytes: usize,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<(), NativeError>>;
}

/// Frozen limits, including all traffic rather than only correlated messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DriverLimits {
    /// Existing bounded protocol/code/output limits.
    pub protocol: ProtocolLimits,
    /// Maximum total inbound frames, including unrelated traffic and duplicates.
    pub frames: usize,
    /// Maximum total inbound encoded bytes, including skipped traffic.
    pub received_bytes: usize,
}

impl Default for DriverLimits {
    fn default() -> Self {
        Self {
            protocol: ProtocolLimits::default(),
            frames: 20_000,
            received_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Explicit options for one execution on an already-ready channel.
pub struct DriverOptions {
    /// Caller-expected exact Notebook owner, Jupyter session, kernel and comm.
    pub ready: ReadySession,
    /// Caller-local handle, distinct from the explicit execute request message ID.
    pub execution: ExecutionId,
    /// Fixed lifetime budgets for this execution.
    pub limits: DriverLimits,
    /// One absolute budget shared by submit, receive, sink, cancel and close.
    pub deadline: NativeDeadline,
}

/// Local driver state, independent of the remote workload outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriverState {
    /// Fully validated offline; no send was attempted.
    Prepared,
    /// A send is in progress or was dropped; it must never be resubmitted.
    SubmissionUnknown,
    /// Submission returned successfully; consuming actual incoming frames.
    Observing,
    /// A receive/sink/control await is in progress or was dropped; no continuation.
    Interrupted,
    /// A correlated terminal execute reply was actually observed.
    Finished,
    /// An error poisoned continuation; partial observations remain available.
    Failed,
    /// The owned channel was locally disposed; no remote cleanup is implied.
    Closed,
}

/// Remote outcome evidence, not a local success or cancellation inference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteOutcome {
    /// No execution send was attempted.
    NotSubmitted,
    /// A send may have occurred, but no authoritative terminal reply was observed.
    Unknown,
    /// Actual terminal native execute-reply status, including error/abort.
    Finished(ExecuteStatus),
}

/// Sanitized local failure together with its independent remote evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DriverFailure {
    /// Sanitized cause; no code, message IDs, raw payloads or traceback are echoed.
    pub cause: NativeError,
    /// What is known about remote execution after this failure.
    pub remote: RemoteOutcome,
}

impl fmt::Display for DriverFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("managed Notebook execution observation failed")
    }
}

impl std::error::Error for DriverFailure {}

/// Progress from one actual received frame, after sink acknowledgement if needed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriverEvent {
    /// Unrelated, duplicate or unrecognized traffic within the finite budgets.
    Skipped,
    /// A correlated nonterminal kernel status.
    KernelStatus,
    /// One live output was acknowledged by the sink.
    Output(usize),
    /// Exact cancellation control response; acceptance is not execution completion.
    CancellationAck(bool),
    /// Actual authoritative execute reply, including workload error/abort.
    Finished(ExecuteStatus),
}

/// Once-only managed execution over one injected persistent owner-bound channel.
///
/// This object owns its channel, retains bounded partial outputs and never
/// resubmits after a send error, dropped future, lost channel or sink failure.
/// Drop releases local channel resources under the host's channel contract.
pub struct ManagedExecution {
    channel: Option<Box<dyn NotebookChannel>>,
    alarm: Box<dyn DeadlineAlarm>,
    ready: ReadySession,
    deadline: NativeDeadline,
    limits: DriverLimits,
    tracker: ExecutionTracker,
    frame: Vec<u8>,
    state: DriverState,
    attempted: bool,
    cancel_attempted: bool,
    cancel_id: Option<MessageId>,
    failure: Option<DriverFailure>,
    frames: usize,
    received_bytes: usize,
    delivered: usize,
}

impl fmt::Debug for ManagedExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedExecution")
            .field("state", &self.state)
            .field("frames", &self.frames)
            .field("delivered_outputs", &self.delivered)
            .finish_non_exhaustive()
    }
}

impl ManagedExecution {
    /// Preflight owner, IDs, code/frame/output budgets and the original deadline.
    ///
    /// This performs no network send and does not create/start/configure a session.
    ///
    /// # Errors
    /// Rejects owner mismatch, invalid/zero limits, expired deadline and oversized
    /// requests before transport access. The consumed channel is dropped on error.
    pub fn new(
        channel: Box<dyn NotebookChannel>,
        alarm: Box<dyn DeadlineAlarm>,
        request: &ExecuteRequest,
        options: DriverOptions,
    ) -> Result<Self, NativeError> {
        options.deadline.remaining()?;
        if channel.binding() != &options.ready
            || options.limits.frames == 0
            || options.limits.received_bytes == 0
        {
            return Err(NativeError::InvalidInput);
        }
        let tracker = ExecutionTracker::new(
            options.execution,
            options.ready.session.clone(),
            request,
            options.limits.protocol,
        )?;
        let frame = request.message().to_vec(options.limits.protocol)?;
        options.deadline.remaining()?;
        Ok(Self {
            channel: Some(channel),
            alarm,
            ready: options.ready,
            deadline: options.deadline,
            limits: options.limits,
            tracker,
            frame,
            state: DriverState::Prepared,
            attempted: false,
            cancel_attempted: false,
            cancel_id: None,
            failure: None,
            frames: 0,
            received_bytes: 0,
            delivered: 0,
        })
    }

    /// Current local state, not proof of remote cancellation or readiness.
    pub const fn state(&self) -> DriverState {
        self.state
    }

    /// Read-only partial outputs, terminal reply and cancellation acknowledgement.
    pub fn observer(&self) -> &ExecutionTracker {
        &self.tracker
    }

    /// Remote evidence retained even after timeout, sink failure or local close.
    pub fn remote_outcome(&self) -> RemoteOutcome {
        match self.tracker.state() {
            ExecutionState::Finished(status) => RemoteOutcome::Finished(status),
            _ if self.attempted => RemoteOutcome::Unknown,
            _ => RemoteOutcome::NotSubmitted,
        }
    }

    /// Number of outputs actually acknowledged by the sink, not just received.
    pub const fn delivered_outputs(&self) -> usize {
        self.delivered
    }

    /// Whether an explicit cancellation send was attempted, even without a receipt.
    ///
    /// A failed/dropped send can reach the remote kernel while the observer still
    /// has no confirmed cancellation request or ACK. This flag is never acceptance.
    pub const fn cancellation_attempted(&self) -> bool {
        self.cancel_attempted
    }

    /// Actual inbound frame count, including skipped traffic.
    pub const fn received_frames(&self) -> usize {
        self.frames
    }

    /// Total actual inbound encoded bytes, including skipped traffic.
    pub const fn received_bytes(&self) -> usize {
        self.received_bytes
    }

    /// Most recent explicit failure; future drop is instead visible in local state.
    pub const fn failure(&self) -> Option<DriverFailure> {
        self.failure
    }

    /// Send the encoded source-backed execute request exactly once.
    ///
    /// # Errors
    /// Rejects repeat attempts. Any send failure/drop leaves remote outcome unknown;
    /// no retry, reconnect, source replay or automatic cancel is performed.
    pub async fn submit(&mut self) -> Result<(), DriverFailure> {
        if self.state != DriverState::Prepared {
            return Err(self.current_failure());
        }
        self.check_ready()?;
        if let Err(error) = self.deadline.remaining() {
            return Err(self.fail(error));
        }
        self.attempted = true;
        self.state = DriverState::SubmissionUnknown;
        let result = {
            let channel = self.channel.as_mut().expect("validated owned channel");
            until(
                &self.deadline,
                self.alarm.as_ref(),
                channel.send(&self.frame, &self.deadline),
            )
            .await
        };
        if let Err(error) = result {
            return Err(self.fail(error));
        }
        self.check_ready()?;
        if let Err(error) = self.tracker.record_submission() {
            return Err(self.fail(error));
        }
        self.state = DriverState::Observing;
        Ok(())
    }

    /// Receive one frame, correlate it and await bounded sink backpressure.
    ///
    /// # Errors
    /// Malformed/oversized frames, budgets, deadline, lost channel, owner mismatch
    /// and sink failure permanently poison continuation without resubmitting.
    /// Dropping this future leaves `Interrupted`, also forbidding continuation.
    pub async fn observe_next(
        &mut self,
        sink: &mut dyn OutputSink,
    ) -> Result<DriverEvent, DriverFailure> {
        if self.state != DriverState::Observing {
            return Err(self.current_failure());
        }
        self.check_ready()?;
        if self.frames >= self.limits.frames || self.received_bytes >= self.limits.received_bytes {
            return Err(self.fail(NativeError::Limit));
        }
        if let Err(error) = self.deadline.remaining() {
            return Err(self.fail(error));
        }
        let max_bytes = self
            .limits
            .protocol
            .frame_bytes
            .min(self.limits.received_bytes - self.received_bytes);
        self.state = DriverState::Interrupted;
        let result = {
            let channel = self.channel.as_mut().expect("validated owned channel");
            until(
                &self.deadline,
                self.alarm.as_ref(),
                channel.receive(max_bytes, &self.deadline),
            )
            .await
        };
        let frame = match result {
            Ok(Some(frame)) => frame,
            Ok(None) => return Err(self.fail(NativeError::OutcomeUnknown)),
            Err(error) => return Err(self.fail(error)),
        };
        self.check_ready()?;
        self.frames += 1;
        let Some(received_bytes) = self.received_bytes.checked_add(frame.len()) else {
            return Err(self.fail(NativeError::Limit));
        };
        self.received_bytes = received_bytes;
        if frame.len() > max_bytes {
            return Err(self.fail(NativeError::Limit));
        }
        let message =
            match super::protocol::JupyterMessage::from_slice(&frame, self.limits.protocol) {
                Ok(message) => message,
                Err(error) => return Err(self.fail(error)),
            };
        if let Some(session) = message.parent_header.get("session") {
            let Some(session) = session.as_str() else {
                return Err(self.fail(NativeError::InvalidResponse));
            };
            let correlated = message.parent_message_id()
                == Some(self.tracker.message_id().as_str())
                || self
                    .cancel_id
                    .as_ref()
                    .is_some_and(|id| message.parent_message_id() == Some(id.as_str()));
            if correlated && session != self.ready.session.session.as_str() {
                return Err(self.fail(NativeError::InvalidResponse));
            }
        }
        if let Err(error) = self.deadline.remaining() {
            return Err(self.fail(error));
        }
        let event = match self.tracker.observe(&message) {
            Ok(event) => event,
            Err(error) => return Err(self.fail(error)),
        };
        let event = match event {
            Observation::Output => {
                let index = self.tracker.outputs().len() - 1;
                let output = &self.tracker.outputs()[index];
                let bytes = match super::content::encoded_len(
                    &output.content,
                    self.limits.protocol.output_bytes,
                ) {
                    Ok(bytes) => bytes,
                    Err(error) => return Err(self.fail(error)),
                };
                if let Err(error) = self.deadline.remaining() {
                    return Err(self.fail(error));
                }
                let result = until(
                    &self.deadline,
                    self.alarm.as_ref(),
                    sink.write(index, output, bytes, &self.deadline),
                )
                .await;
                if let Err(error) = result {
                    return Err(self.fail(error));
                }
                self.check_ready()?;
                self.delivered += 1;
                DriverEvent::Output(index)
            }
            Observation::Terminal => {
                let ExecutionState::Finished(status) = self.tracker.state() else {
                    return Err(self.fail(NativeError::InvalidResponse));
                };
                if let Err(error) = self.deadline.remaining() {
                    return Err(self.fail(error));
                }
                self.state = DriverState::Finished;
                return Ok(DriverEvent::Finished(status));
            }
            Observation::CancellationAck => DriverEvent::CancellationAck(
                self.tracker
                    .cancellation_reply()
                    .is_some_and(|reply| reply.accepted()),
            ),
            Observation::KernelStatus => DriverEvent::KernelStatus,
            Observation::Ignored => DriverEvent::Skipped,
        };
        self.state = DriverState::Observing;
        Ok(event)
    }

    /// Submit once (if still prepared) and consume frames through the terminal reply.
    ///
    /// `Ok(Error)` and `Ok(Abort)` are successful observation of failed/aborted
    /// workloads, not workload success. Inspect the retained native reply/error.
    ///
    /// # Errors
    /// Preserves all submit/observation failures and never resumes poisoned state.
    pub async fn run(&mut self, sink: &mut dyn OutputSink) -> Result<ExecuteStatus, DriverFailure> {
        if self.state == DriverState::Prepared {
            self.submit().await?;
        }
        if let (DriverState::Finished, RemoteOutcome::Finished(status)) =
            (self.state, self.remote_outcome())
        {
            return Ok(status);
        }
        loop {
            if let DriverEvent::Finished(status) = self.observe_next(sink).await? {
                return Ok(status);
            }
        }
    }

    /// Explicitly send one source-backed exact-execution cancellation control.
    ///
    /// Uses the same channel/session/comm and original execute ID. A subsequent
    /// control ACK does not terminate execution; continue observing its original
    /// reply. This is never called by timeout, future drop or local close.
    ///
    /// # Errors
    /// Rejects repeat/invalid controls before sending; a failed/dropped control
    /// send poisons continuation and is never retried.
    pub async fn cancel(&mut self, control_id: MessageId) -> Result<(), DriverFailure> {
        if self.state != DriverState::Observing || self.cancel_attempted {
            return Err(self.current_failure());
        }
        self.check_ready()?;
        let control = match ControlRequest::new(
            control_id,
            self.ready.session.session.clone(),
            self.ready.control_comm.clone(),
            SynapseControl::CancelExecution(self.tracker.message_id().clone()),
            self.limits.protocol,
        ) {
            Ok(control) => control,
            Err(error) => return Err(self.fail(error)),
        };
        let frame = match control.message().to_vec(self.limits.protocol) {
            Ok(frame) => frame,
            Err(error) => return Err(self.fail(error)),
        };
        if let Err(error) = self.deadline.remaining() {
            return Err(self.fail(error));
        }
        self.cancel_attempted = true;
        self.cancel_id = Some(control.id().clone());
        self.state = DriverState::Interrupted;
        let result = {
            let channel = self.channel.as_mut().expect("validated owned channel");
            until(
                &self.deadline,
                self.alarm.as_ref(),
                channel.send(&frame, &self.deadline),
            )
            .await
        };
        if let Err(error) = result {
            return Err(self.fail(error));
        }
        self.check_ready()?;
        if let Err(error) = self.tracker.record_cancellation_sent(&control) {
            return Err(self.fail(error));
        }
        self.state = DriverState::Observing;
        Ok(())
    }

    /// Dispose the owned local channel once, without remote compute cleanup.
    ///
    /// Uses the original deadline. Even on error, expiry or future drop, the
    /// channel is dropped and never reused. Repeated local disposal is a no-op.
    ///
    /// # Errors
    /// Preserves close/deadline errors without changing remote outcome evidence.
    pub async fn close(&mut self) -> Result<(), DriverFailure> {
        let Some(mut channel) = self.channel.take() else {
            return Ok(());
        };
        self.state = DriverState::Closed;
        if let Err(cause) = self.deadline.remaining() {
            let failure = DriverFailure {
                cause,
                remote: self.remote_outcome(),
            };
            self.failure = Some(failure);
            return Err(failure);
        }
        let result = until(
            &self.deadline,
            self.alarm.as_ref(),
            channel.close(&self.deadline),
        )
        .await;
        if let Err(cause) = result {
            let failure = DriverFailure {
                cause,
                remote: self.remote_outcome(),
            };
            self.failure = Some(failure);
            return Err(failure);
        }
        Ok(())
    }

    fn check_ready(&mut self) -> Result<(), DriverFailure> {
        if !self
            .channel
            .as_ref()
            .is_some_and(|channel| channel.binding() == &self.ready)
        {
            return Err(self.fail(NativeError::InvalidInput));
        }
        Ok(())
    }

    fn current_failure(&self) -> DriverFailure {
        self.failure.unwrap_or(DriverFailure {
            cause: match self.state {
                DriverState::SubmissionUnknown => NativeError::OutcomeUnknown,
                DriverState::Interrupted => NativeError::Cancelled,
                _ => NativeError::InvalidInput,
            },
            remote: self.remote_outcome(),
        })
    }

    fn fail(&mut self, cause: NativeError) -> DriverFailure {
        let failure = DriverFailure {
            cause,
            remote: self.remote_outcome(),
        };
        self.state = DriverState::Failed;
        self.failure = Some(failure);
        match cause {
            NativeError::TimedOut => self.tracker.timeout(),
            NativeError::Cancelled => self.tracker.cancel_observation(),
            _ => self.tracker.disconnect(),
        }
        failure
    }
}

async fn until<T>(
    deadline: &NativeDeadline,
    alarm: &dyn DeadlineAlarm,
    mut operation: NativeFuture<'_, Result<T, NativeError>>,
) -> Result<T, NativeError> {
    deadline.remaining()?;
    let mut timeout = alarm.wait(deadline);
    poll_fn(|context| {
        if deadline.remaining().is_err() || timeout.as_mut().poll(context).is_ready() {
            return Poll::Ready(Err(NativeError::TimedOut));
        }
        match operation.as_mut().poll(context) {
            Poll::Ready(result) => Poll::Ready(deadline.remaining().and(result)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}
