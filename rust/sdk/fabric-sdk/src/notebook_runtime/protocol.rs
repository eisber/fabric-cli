// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-pinned Jupyter 5.3/Synapse messages and bounded offline observation.
//!
//! These APIs encode/decode messages only. The caller must provide an
//! authenticated, scope-bound WebSocket host before recording any actual send.
//! No network transport, timer, replay, execution or remote cancellation is
//! installed. A terminal `execute_reply` is distinct from a control ACK, kernel
//! idle, saved cell outputs and the end of local observation.

use super::{CommId, ExecutionId, MessageId, ResumeBinding, SessionId, content::encoded_len};
use crate::runtime::{NativeEffect, NativeError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

/// Finite message and retained-execution budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolLimits {
    /// Maximum JSON bytes in one inbound or outbound frame.
    pub frame_bytes: usize,
    /// Maximum executable source bytes in one execute request.
    pub code_bytes: usize,
    /// Maximum distinct correlated inbound messages, including control replies.
    pub messages: usize,
    /// Maximum retained live output records.
    pub outputs: usize,
    /// Maximum encoded bytes across all retained live outputs.
    pub output_bytes: usize,
}

impl Default for ProtocolLimits {
    fn default() -> Self {
        Self {
            frame_bytes: 4 * 1024 * 1024,
            code_bytes: 1024 * 1024,
            messages: 20_000,
            outputs: 10_000,
            output_bytes: 16 * 1024 * 1024,
        }
    }
}

impl ProtocolLimits {
    fn validate(self) -> Result<(), NativeError> {
        if self.frame_bytes == 0
            || self.code_bytes == 0
            || self.messages == 0
            || self.outputs == 0
            || self.output_bytes == 0
        {
            return Err(NativeError::InvalidInput);
        }
        Ok(())
    }
}

/// Jupyter header preserving provider extensions.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageHeader {
    /// This message's ID, not its parent's ID.
    pub msg_id: String,
    /// Native message type.
    pub msg_type: String,
    /// Protocol sender name; not an authenticated user identity.
    #[serde(default)]
    pub username: String,
    /// Sender's protocol session; incoming kernel sessions may differ from the client.
    #[serde(default)]
    pub session: String,
    /// Jupyter protocol version, emitted as 5.3 for source parity.
    #[serde(default)]
    pub version: String,
    /// Unknown header fields retained without interpretation.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// JSON Jupyter channel envelope; deliberately not a WebSocket client.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct JupyterMessage {
    /// Native message header.
    pub header: MessageHeader,
    /// Original request header used for response correlation.
    #[serde(default)]
    pub parent_header: Map<String, Value>,
    /// Uninterpreted metadata, including provider extensions.
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// Native message content, preserving unknown fields and MIME bundles.
    pub content: Map<String, Value>,
    /// Native Jupyter channel (`shell`, `iopub`, or another declared channel).
    pub channel: String,
    /// Unknown envelope fields, including any provider extension.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl JupyterMessage {
    /// Decode one bounded JSON frame without interpreting it as execution success.
    ///
    /// # Errors
    /// Rejects oversized, malformed or structurally invalid messages.
    pub fn from_slice(bytes: &[u8], limits: ProtocolLimits) -> Result<Self, NativeError> {
        limits.validate()?;
        if bytes.len() > limits.frame_bytes {
            return Err(NativeError::Limit);
        }
        let message: Self =
            serde_json::from_slice(bytes).map_err(|_| NativeError::InvalidResponse)?;
        message.validate(limits)?;
        Ok(message)
    }

    /// Encode one bounded JSON frame; this performs no dispatch.
    ///
    /// # Errors
    /// Rejects invalid headers/channels and exceeded budgets.
    pub fn to_vec(&self, limits: ProtocolLimits) -> Result<Vec<u8>, NativeError> {
        self.validate(limits)?;
        serde_json::to_vec(self).map_err(|_| NativeError::InvalidInput)
    }

    /// Borrow the original request's message ID when supplied.
    pub fn parent_message_id(&self) -> Option<&str> {
        self.parent_header.get("msg_id").and_then(Value::as_str)
    }

    fn new(
        msg_id: &MessageId,
        session: &SessionId,
        msg_type: &str,
        content: Value,
    ) -> Result<Self, NativeError> {
        Ok(Self {
            header: MessageHeader {
                msg_id: msg_id.as_str().into(),
                msg_type: msg_type.into(),
                username: "notebook-cli".into(),
                session: session.as_str().into(),
                version: "5.3".into(),
                extra: Map::new(),
            },
            parent_header: Map::new(),
            metadata: Map::new(),
            content: content
                .as_object()
                .cloned()
                .ok_or(NativeError::InvalidInput)?,
            channel: "shell".into(),
            extra: Map::new(),
        })
    }

    fn validate(&self, limits: ProtocolLimits) -> Result<(), NativeError> {
        limits.validate()?;
        MessageId::new(self.header.msg_id.clone()).map_err(|_| NativeError::InvalidResponse)?;
        if self.header.msg_type.is_empty()
            || self.header.msg_type.chars().any(char::is_control)
            || self.channel.is_empty()
            || self.channel.chars().any(char::is_control)
            || self
                .parent_header
                .get("msg_id")
                .is_some_and(|id| !id.is_string())
        {
            return Err(NativeError::InvalidResponse);
        }
        let value = serde_json::to_value(self).map_err(|_| NativeError::InvalidResponse)?;
        encoded_len(&value, limits.frame_bytes)?;
        Ok(())
    }
}

/// Prepared ephemeral code execution, never a saved-cell execution or network send.
#[derive(Clone)]
pub struct ExecuteRequest {
    id: MessageId,
    session: SessionId,
    message: JupyterMessage,
}

impl ExecuteRequest {
    /// Encode the pinned source's `execute_request` payload.
    ///
    /// `allow_stdin=false`, `stop_on_error=true` and empty `user_expressions`
    /// are explicit. IDs come from the caller, not an ambient generator.
    ///
    /// # Errors
    /// Rejects exceeded frame/code budgets and invalid limits.
    pub fn new(
        id: MessageId,
        session: SessionId,
        code: &str,
        silent: bool,
        store_history: bool,
        limits: ProtocolLimits,
    ) -> Result<Self, NativeError> {
        limits.validate()?;
        if code.len() > limits.code_bytes {
            return Err(NativeError::Limit);
        }
        let message = JupyterMessage::new(
            &id,
            &session,
            "execute_request",
            json!({
                "code": code,
                "silent": silent,
                "store_history": store_history,
                "user_expressions": {},
                "allow_stdin": false,
                "stop_on_error": true
            }),
        )?;
        message.validate(limits)?;
        Ok(Self {
            id,
            session,
            message,
        })
    }

    /// Remote execute-request message ID; not the caller-local execution handle.
    pub fn id(&self) -> &MessageId {
        &self.id
    }

    /// Prepared native envelope; encoding it does not dispatch executable source.
    pub fn message(&self) -> &JupyterMessage {
        &self.message
    }

    /// Declared remote effect if a separately supplied host sends this message.
    pub const fn effect(&self) -> NativeEffect {
        NativeEffect::Execute
    }
}

/// Source-advertised deterministic Synapse comm targets (no model capabilities).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommTarget {
    /// Lifecycle, cancellation and connectivity controls.
    Control,
    /// Session status notifications.
    Status,
    /// Existing Spark job information queries.
    Query,
}

impl CommTarget {
    fn as_str(self) -> &'static str {
        match self {
            Self::Control => "synapse:control",
            Self::Status => "synapse:status",
            Self::Query => "synapse:query",
        }
    }
}

/// Prepare `comm_open` for a caller-owned channel; this is not an established comm.
///
/// # Errors
/// Rejects invalid limits or an exceeded frame budget.
pub fn comm_open(
    id: &MessageId,
    session: &SessionId,
    comm: &CommId,
    target: CommTarget,
    limits: ProtocolLimits,
) -> Result<JupyterMessage, NativeError> {
    let message = JupyterMessage::new(
        id,
        session,
        "comm_open",
        json!({"comm_id": comm.as_str(), "target_name": target.as_str(), "data": {}}),
    )?;
    message.validate(limits)?;
    Ok(message)
}

/// Native deterministic controls; cancellation, lifecycle and replay stay distinct.
#[derive(Clone)]
pub enum SynapseControl {
    /// Request cancellation of one exact execute-request message, not a local handle.
    CancelExecution(MessageId),
    /// Explicit session-wide cancellation scope.
    CancelAll,
    /// Explicit output replay for an existing request; never automatically invoked.
    ReplayOutputs(MessageId),
    /// Start remote Livy compute; not REST session creation or reconnect.
    StartLivy(Value),
    /// Configure future Livy startup with provider-native options.
    SetLivyOptions(Value),
    /// Probe the established kernel control channel, not Spark readiness.
    Ping,
}

impl SynapseControl {
    fn wire(&self) -> Result<(&'static str, Value), NativeError> {
        match self {
            Self::CancelExecution(id) => Ok(("cancel_cell_execution", id.as_str().into())),
            Self::CancelAll => Ok(("cancel_all_cell_execution", json!({}))),
            Self::ReplayOutputs(id) => {
                Ok(("replay_outputs", json!({"parent_msg_id": id.as_str()})))
            }
            Self::StartLivy(options) | Self::SetLivyOptions(options) => {
                if !options.is_object() {
                    return Err(NativeError::InvalidInput);
                }
                let command = if matches!(self, Self::StartLivy(_)) {
                    "start_livy_session"
                } else {
                    "set_livy_session_options"
                };
                Ok((command, options.clone()))
            }
            Self::Ping => Ok(("ping", json!({}))),
        }
    }
}

/// Prepared Synapse `comm_msg`, using distinct outer/control and target message IDs.
#[derive(Clone)]
pub struct ControlRequest {
    id: MessageId,
    session: SessionId,
    comm: CommId,
    command: SynapseControl,
    message: JupyterMessage,
}

impl ControlRequest {
    /// Wrap one native control command exactly as the pinned source does.
    ///
    /// # Errors
    /// Rejects non-object options, reused cancel/target IDs and exceeded budgets.
    pub fn new(
        id: MessageId,
        session: SessionId,
        comm: CommId,
        command: SynapseControl,
        limits: ProtocolLimits,
    ) -> Result<Self, NativeError> {
        if matches!(&command, SynapseControl::CancelExecution(target) if target == &id) {
            return Err(NativeError::InvalidInput);
        }
        let (kind, content) = command.wire()?;
        let message = JupyterMessage::new(
            &id,
            &session,
            "comm_msg",
            json!({
                "comm_id": comm.as_str(),
                "data": {"type": kind, "content": content, "parent_msg_id": id.as_str()}
            }),
        )?;
        message.validate(limits)?;
        Ok(Self {
            id,
            session,
            comm,
            command,
            message,
        })
    }

    /// Native control request ID, distinct from any target execution ID.
    pub fn id(&self) -> &MessageId {
        &self.id
    }

    /// Prepared message, not evidence that a control was sent or accepted.
    pub fn message(&self) -> &JupyterMessage {
        &self.message
    }

    /// Control effect, distinct from executing source and from reconnecting locally.
    pub const fn effect(&self) -> NativeEffect {
        match &self.command {
            SynapseControl::CancelExecution(_) | SynapseControl::CancelAll => NativeEffect::Cancel,
            SynapseControl::ReplayOutputs(_) | SynapseControl::Ping => NativeEffect::Read,
            SynapseControl::StartLivy(_) | SynapseControl::SetLivyOptions(_) => {
                NativeEffect::Lifecycle
            }
        }
    }
}

/// Terminal native execute-reply status. Unknown strings are not success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecuteStatus {
    /// Native `ok` reply.
    Ok,
    /// Native `error` reply, with the original error fields preserved.
    Error,
    /// Native `abort` reply; not inferred from a cancellation acknowledgement.
    Abort,
}

/// Typed error fields retaining the complete original payload alongside them.
#[derive(Clone, PartialEq)]
pub struct ExecutionError {
    /// Native exception name.
    pub name: String,
    /// Native exception value, potentially containing source/runtime data.
    pub value: String,
    /// Original traceback lines; never flattened or logged automatically.
    pub traceback: Vec<String>,
}

/// Native `execute_reply`, terminal for the source-pinned execution observation.
#[derive(Clone, PartialEq)]
pub struct ExecuteReply {
    /// Authoritative native reply status, independent of control ACKs.
    pub status: ExecuteStatus,
    /// Native execution counter, absent/null when not supplied.
    pub execution_count: Option<u64>,
    /// Typed exception fields on an error reply.
    pub error: Option<ExecutionError>,
    /// Full reply fields, preserving absent/null and provider extensions.
    pub content: Map<String, Value>,
}

/// Live output type; values remain distinct from stored IPYNB cell outputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputKind {
    /// Stream (`stdout` or `stderr`) content.
    Stream,
    /// Execution result with MIME data.
    ExecuteResult,
    /// Display content with MIME data.
    DisplayData,
    /// Error output (not by itself a terminal execution reply).
    Error,
}

/// One lossless live output with typed kind and complete native content.
#[derive(Clone, PartialEq)]
pub struct LiveOutput {
    /// Native output category.
    pub kind: OutputKind,
    /// Original fields, including arbitrary MIME data, metadata and traceback.
    pub content: Map<String, Value>,
}

impl LiveOutput {
    /// Borrow a MIME bundle without selecting `text/plain` or flattening rich data.
    pub fn mime_data(&self) -> Option<&Map<String, Value>> {
        self.content.get("data").and_then(Value::as_object)
    }

    /// Project error fields without discarding the original payload.
    ///
    /// # Errors
    /// Rejects malformed native error fields.
    pub fn error(&self) -> Result<Option<ExecutionError>, NativeError> {
        if self.kind == OutputKind::Error {
            Ok(Some(parse_error(&self.content)?))
        } else {
            Ok(None)
        }
    }
}

/// Actual Synapse control response; acceptance never means execution completion.
#[derive(Clone, PartialEq)]
pub struct ControlReply {
    /// Original control request ID.
    pub request: MessageId,
    /// Native status without unknown-to-success coercion.
    pub status: String,
    /// Complete original data, including response/error fields.
    pub data: Map<String, Value>,
}

impl ControlReply {
    /// Decode an actual correlated control response from an established host channel.
    ///
    /// The pinned source permits an omitted comm ID; when present it must match.
    /// Both outer and nested parent IDs must match the original control request.
    ///
    /// # Errors
    /// Rejects malformed/mismatched responses and exceeded frame budgets.
    pub fn from_message(
        message: &JupyterMessage,
        request: &ControlRequest,
        limits: ProtocolLimits,
    ) -> Result<Self, NativeError> {
        message.validate(limits)?;
        Self::parse(message, request)
    }

    /// Whether this is a native `ok` ACK with no error payload.
    pub fn accepted(&self) -> bool {
        self.status == "ok" && self.data.get("error").is_none_or(Value::is_null)
    }

    fn parse(message: &JupyterMessage, request: &ControlRequest) -> Result<Self, NativeError> {
        if !matches!(message.channel.as_str(), "shell" | "iopub")
            || message.header.msg_type != "comm_msg"
            || message.parent_message_id() != Some(request.id.as_str())
            || message
                .content
                .get("comm_id")
                .is_some_and(|id| id.as_str() != Some(request.comm.as_str()))
        {
            return Err(NativeError::InvalidResponse);
        }
        let data = message
            .content
            .get("data")
            .and_then(Value::as_object)
            .ok_or(NativeError::InvalidResponse)?;
        if data.get("parent_msg_id").and_then(Value::as_str) != Some(request.id.as_str()) {
            return Err(NativeError::InvalidResponse);
        }
        let status = data
            .get("status")
            .and_then(Value::as_str)
            .filter(|status| !status.is_empty())
            .ok_or(NativeError::InvalidResponse)?;
        Ok(Self {
            request: request.id.clone(),
            status: status.into(),
            data: data.clone(),
        })
    }
}

/// Native Synapse Livy status notification, separate from a code execution reply.
#[derive(Clone, PartialEq)]
pub struct LivyStatus {
    /// Provider session state as supplied, including unknown future strings.
    pub session_state: Option<String>,
    /// All native content, including startup errors and application/runtime fields.
    pub content: Map<String, Value>,
}

impl LivyStatus {
    /// Read a source-pinned `livy_session_status` notification without claiming readiness.
    ///
    /// The caller must route this from the correct established status comm channel.
    ///
    /// # Errors
    /// Rejects malformed status payloads and exceeded budgets.
    pub fn from_message(
        message: &JupyterMessage,
        limits: ProtocolLimits,
    ) -> Result<Option<Self>, NativeError> {
        message.validate(limits)?;
        if message.header.msg_type != "comm_msg" {
            return Ok(None);
        }
        let Some(data) = message.content.get("data").and_then(Value::as_object) else {
            return Ok(None);
        };
        if data.get("status_type").and_then(Value::as_str) != Some("livy_session_status") {
            return Ok(None);
        }
        let content = data
            .get("content")
            .and_then(Value::as_object)
            .ok_or(NativeError::InvalidResponse)?;
        let session_state = match content.get("session_state") {
            None | Some(Value::Null) => None,
            Some(Value::String(state)) => Some(state.clone()),
            _ => return Err(NativeError::InvalidResponse),
        };
        Ok(Some(Self {
            session_state,
            content: content.clone(),
        }))
    }
}

/// Execution lifecycle, independent of local waiting and cancellation requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionState {
    /// Prepared offline only; not submitted by this library.
    Prepared,
    /// The caller recorded an actual send; no terminal reply has been observed.
    AwaitingReply,
    /// A correlated terminal execute reply was observed.
    Finished(ExecuteStatus),
}

/// Local observation lifecycle; no variant implies remote cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationState {
    /// No actual caller-host send has been recorded.
    NotStarted,
    /// Accepting correlated messages from the caller-owned host.
    Observing,
    /// A local deadline expired; remote execution may still be running.
    TimedOut,
    /// Local observation was cancelled; remote execution may still be running.
    Cancelled,
    /// The caller's channel disconnected; no automatic reconnect or replay occurs.
    Disconnected,
}

/// Cancellation lifecycle separate from the authoritative execution reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationState {
    /// No cancellation has been sent.
    NotRequested,
    /// The caller recorded an actual exact-execution control send.
    AwaitingAck,
    /// Native control ACK only; completion can still win the race.
    Accepted,
    /// The control reply rejected cancellation or returned an unknown status.
    Rejected,
}

/// Result of processing one scoped inbound message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Observation {
    /// Unrelated, duplicate, or later-than-terminal execution message.
    Ignored,
    /// One partial live output was retained.
    Output,
    /// A correlated native terminal execute reply was retained.
    Terminal,
    /// An exact cancellation control reply was retained.
    CancellationAck,
    /// A correlated kernel status was seen; idle is not a terminal reply.
    KernelStatus,
}

/// Bounded state for one local handle and one distinct native execute message.
///
/// Message processing trusts only the caller's established scope-bound channel.
/// This object neither sends messages nor proves a channel exists. All source
/// and output data stay in memory and are excluded from `Debug`.
pub struct ExecutionTracker {
    execution: ExecutionId,
    request: MessageId,
    binding: ResumeBinding,
    limits: ProtocolLimits,
    state: ExecutionState,
    observation: ObservationState,
    cancellation: CancellationState,
    cancellation_request: Option<ControlRequest>,
    cancellation_reply: Option<ControlReply>,
    reply: Option<ExecuteReply>,
    outputs: Vec<LiveOutput>,
    output_bytes: usize,
    seen: HashSet<String>,
}

impl ExecutionTracker {
    /// Prepare local tracking without claiming dispatch or readiness.
    ///
    /// # Errors
    /// Rejects a mismatched session binding, reused local/remote ID or invalid limits.
    pub fn new(
        execution: ExecutionId,
        binding: ResumeBinding,
        request: &ExecuteRequest,
        limits: ProtocolLimits,
    ) -> Result<Self, NativeError> {
        limits.validate()?;
        request.message.validate(limits)?;
        if request
            .message
            .content
            .get("code")
            .and_then(Value::as_str)
            .is_some_and(|code| code.len() > limits.code_bytes)
        {
            return Err(NativeError::Limit);
        }
        if binding.session != request.session || execution.as_str() == request.id.as_str() {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self {
            execution,
            request: request.id.clone(),
            binding,
            limits,
            state: ExecutionState::Prepared,
            observation: ObservationState::NotStarted,
            cancellation: CancellationState::NotRequested,
            cancellation_request: None,
            cancellation_reply: None,
            reply: None,
            outputs: Vec::new(),
            output_bytes: 0,
            seen: HashSet::new(),
        })
    }

    /// Caller-local handle, not valid as a Jupyter message ID.
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution
    }

    /// Original remote message ID; reconnect never generates a replacement.
    pub fn message_id(&self) -> &MessageId {
        &self.request
    }

    /// Exact existing Jupyter/kernel binding for this execution.
    pub fn binding(&self) -> &ResumeBinding {
        &self.binding
    }

    /// Current authoritative execution lifecycle.
    pub const fn state(&self) -> ExecutionState {
        self.state
    }

    /// Current local observation lifecycle, not remote execution status.
    pub const fn observation_state(&self) -> ObservationState {
        self.observation
    }

    /// Current cancellation ACK state, not terminal execution status.
    pub const fn cancellation_state(&self) -> CancellationState {
        self.cancellation
    }

    /// Partial live outputs retained even after timeout, cancellation or error.
    pub fn outputs(&self) -> &[LiveOutput] {
        &self.outputs
    }

    /// Actual terminal execute reply, absent until observed.
    pub fn reply(&self) -> Option<&ExecuteReply> {
        self.reply.as_ref()
    }

    /// Actual cancellation reply, which can arrive after execution finished.
    pub fn cancellation_reply(&self) -> Option<&ControlReply> {
        self.cancellation_reply.as_ref()
    }

    /// Record the host's actual once-only send; this method itself sends nothing.
    ///
    /// # Errors
    /// Rejects a second submission, including after timeout or disconnect.
    pub fn record_submission(&mut self) -> Result<(), NativeError> {
        if self.state != ExecutionState::Prepared {
            return Err(NativeError::InvalidInput);
        }
        self.state = ExecutionState::AwaitingReply;
        self.observation = ObservationState::Observing;
        Ok(())
    }

    /// Record an actual exact-execution cancellation sent by the caller's host.
    ///
    /// # Errors
    /// Rejects wrong target/session, non-cancel controls, repeated cancellation
    /// and controls recorded before execution was actually submitted.
    pub fn record_cancellation_sent(
        &mut self,
        control: &ControlRequest,
    ) -> Result<(), NativeError> {
        if self.state != ExecutionState::AwaitingReply
            || self.cancellation != CancellationState::NotRequested
            || control.session != self.binding.session
            || !matches!(&control.command, SynapseControl::CancelExecution(id) if id == &self.request)
        {
            return Err(NativeError::InvalidInput);
        }
        control.message.validate(self.limits)?;
        self.cancellation_request = Some(control.clone());
        self.cancellation = CancellationState::AwaitingAck;
        Ok(())
    }

    /// Stop local waiting at a deadline; sends no cancel, replay or new execution.
    pub fn timeout(&mut self) {
        self.observation = ObservationState::TimedOut;
    }

    /// Cancel local observation only; partial outputs and remote state are retained.
    pub fn cancel_observation(&mut self) {
        self.observation = ObservationState::Cancelled;
    }

    /// Record a channel disconnect without changing the execution outcome.
    pub fn disconnect(&mut self) {
        self.observation = ObservationState::Disconnected;
    }

    /// Resume local observation after the host reconnects the exact existing binding.
    ///
    /// This does not open a socket, replay outputs, start Livy or resubmit code.
    ///
    /// # Errors
    /// Rejects another kernel/session binding and executions never submitted.
    pub fn resume_observation(&mut self, binding: &ResumeBinding) -> Result<(), NativeError> {
        if binding != &self.binding || self.state == ExecutionState::Prepared {
            return Err(NativeError::InvalidInput);
        }
        self.observation = ObservationState::Observing;
        Ok(())
    }

    /// Process one bounded message from the host's already scoped channel.
    ///
    /// # Errors
    /// Rejects malformed correlated payloads and exceeded budgets, preserving
    /// prior outputs. Local timeout/cancellation/disconnect rejects observation
    /// until explicitly resumed; none implies a remote cancel or automatic replay.
    pub fn observe(&mut self, message: &JupyterMessage) -> Result<Observation, NativeError> {
        match self.observation {
            ObservationState::TimedOut => return Err(NativeError::TimedOut),
            ObservationState::Cancelled => return Err(NativeError::Cancelled),
            ObservationState::NotStarted | ObservationState::Disconnected => {
                return Err(NativeError::InvalidInput);
            }
            ObservationState::Observing => {}
        }
        message.validate(self.limits)?;
        let parent = message.parent_message_id();
        let control_matches = self
            .cancellation_request
            .as_ref()
            .is_some_and(|request| parent == Some(request.id.as_str()));
        if parent != Some(self.request.as_str()) && !control_matches {
            return Ok(Observation::Ignored);
        }
        if self.seen.contains(&message.header.msg_id) {
            return Ok(Observation::Ignored);
        }
        if self.seen.len() >= self.limits.messages {
            return Err(NativeError::Limit);
        }
        if control_matches {
            if self.cancellation != CancellationState::AwaitingAck {
                return Ok(Observation::Ignored);
            }
            let request = self
                .cancellation_request
                .as_ref()
                .ok_or(NativeError::InvalidInput)?;
            let reply = ControlReply::parse(message, request)?;
            self.cancellation = if reply.accepted() {
                CancellationState::Accepted
            } else {
                CancellationState::Rejected
            };
            self.cancellation_reply = Some(reply);
            self.seen.insert(message.header.msg_id.clone());
            return Ok(Observation::CancellationAck);
        }
        if matches!(self.state, ExecutionState::Finished(_)) {
            return Ok(Observation::Ignored);
        }
        let outcome = match message.header.msg_type.as_str() {
            "execute_reply" => {
                if message.channel != "shell" {
                    return Err(NativeError::InvalidResponse);
                }
                let reply = parse_execute_reply(&message.content)?;
                self.state = ExecutionState::Finished(reply.status);
                self.reply = Some(reply);
                Observation::Terminal
            }
            "stream" | "execute_result" | "display_data" | "error" => {
                if message.channel != "iopub" {
                    return Err(NativeError::InvalidResponse);
                }
                let output = parse_output(message)?;
                if self.outputs.len() >= self.limits.outputs {
                    return Err(NativeError::Limit);
                }
                let bytes = encoded_len(
                    &Value::Object(output.content.clone()),
                    self.limits.output_bytes.saturating_sub(self.output_bytes),
                )?;
                self.output_bytes += bytes;
                self.outputs.push(output);
                Observation::Output
            }
            "status" => {
                if message.channel != "iopub"
                    || message
                        .content
                        .get("execution_state")
                        .and_then(Value::as_str)
                        .is_none()
                {
                    return Err(NativeError::InvalidResponse);
                }
                Observation::KernelStatus
            }
            _ => Observation::Ignored,
        };
        self.seen.insert(message.header.msg_id.clone());
        Ok(outcome)
    }
}

fn parse_execute_reply(content: &Map<String, Value>) -> Result<ExecuteReply, NativeError> {
    let status = match content.get("status").and_then(Value::as_str) {
        Some("ok") => ExecuteStatus::Ok,
        Some("error") => ExecuteStatus::Error,
        Some("abort") => ExecuteStatus::Abort,
        _ => return Err(NativeError::InvalidResponse),
    };
    let execution_count = count(content.get("execution_count"))?;
    let error = if status == ExecuteStatus::Error {
        Some(parse_error(content)?)
    } else {
        None
    };
    Ok(ExecuteReply {
        status,
        execution_count,
        error,
        content: content.clone(),
    })
}

fn parse_output(message: &JupyterMessage) -> Result<LiveOutput, NativeError> {
    let content = &message.content;
    let kind = match message.header.msg_type.as_str() {
        "stream" => {
            if !matches!(
                content.get("name").and_then(Value::as_str),
                Some("stdout" | "stderr")
            ) {
                return Err(NativeError::InvalidResponse);
            }
            validate_text(content.get("text"))?;
            OutputKind::Stream
        }
        "display_data" | "execute_result" => {
            if !content.get("data").is_some_and(Value::is_object)
                || content
                    .get("metadata")
                    .is_some_and(|value| !value.is_null() && !value.is_object())
            {
                return Err(NativeError::InvalidResponse);
            }
            count(content.get("execution_count"))?;
            if message.header.msg_type == "execute_result" {
                OutputKind::ExecuteResult
            } else {
                OutputKind::DisplayData
            }
        }
        "error" => {
            parse_error(content)?;
            OutputKind::Error
        }
        _ => return Err(NativeError::InvalidResponse),
    };
    Ok(LiveOutput {
        kind,
        content: content.clone(),
    })
}

fn count(value: Option<&Value>) -> Result<Option<u64>, NativeError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or(NativeError::InvalidResponse),
    }
}

fn validate_text(value: Option<&Value>) -> Result<(), NativeError> {
    match value {
        Some(Value::String(_)) => Ok(()),
        Some(Value::Array(lines)) if lines.iter().all(Value::is_string) => Ok(()),
        _ => Err(NativeError::InvalidResponse),
    }
}

fn parse_error(content: &Map<String, Value>) -> Result<ExecutionError, NativeError> {
    let text = |field: &str| match content.get(field) {
        None => Ok(""),
        Some(Value::String(value)) => Ok(value.as_str()),
        _ => Err(NativeError::InvalidResponse),
    };
    let name = text("ename")?;
    let value = text("evalue")?;
    let lines = match content.get("traceback") {
        None => &[][..],
        Some(Value::Array(lines)) => lines.as_slice(),
        _ => return Err(NativeError::InvalidResponse),
    };
    let traceback = lines
        .iter()
        .map(|line| {
            line.as_str()
                .map(String::from)
                .ok_or(NativeError::InvalidResponse)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ExecutionError {
        name: name.into(),
        value: value.into(),
        traceback,
    })
}
