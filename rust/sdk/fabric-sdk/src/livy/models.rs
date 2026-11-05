// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Immutable native payloads. HTTP success and workload success are independent.

use crate::runtime::NativeError;
use serde_json::Value;
use std::fmt;

mod sealed {
    pub trait Sealed {}
}

/// Sealed response decoding contract used by typed Livy requests.
pub trait ResponseModel: sealed::Sealed + Sized {
    /// Decode an immutable object without discarding unknown fields.
    ///
    /// # Errors
    /// Rejects a payload outside the response representation.
    #[doc(hidden)]
    fn decode(value: Value) -> Result<Self, NativeError>;

    /// Decode an acknowledgement without promoting it to a resource representation.
    ///
    /// # Errors
    /// Returns the model's acknowledgement representation error.
    #[doc(hidden)]
    fn decode_acknowledgement(value: Value) -> Result<Self, NativeError> {
        Self::decode(value)
    }
}

/// Evidence from a resource snapshot, never inferred from an HTTP acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Observation {
    /// A known startup/queued state.
    Pending,
    /// A session is idle and ready; not a completed application.
    Ready,
    /// Execution is in progress.
    Running,
    /// This model has explicit workload-success evidence.
    Succeeded,
    /// The resource explicitly reports cancellation.
    Cancelled,
    /// The state or output is missing, unrecognized or insufficient.
    Unknown,
}

/// Workload failure domain, separate from HTTP/protocol failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkloadFailureKind {
    /// Session/REPL/HC acquisition failed.
    Session,
    /// Batch failed.
    Batch,
    /// Statement state failed.
    StatementState,
    /// An available statement contains an execution error.
    StatementOutput,
    /// Serialized native job failed or reported an application error.
    Job,
}

/// Workload failure retaining the exact immutable JSON, with sanitized diagnostics.
#[derive(Clone)]
pub struct WorkloadFailure {
    kind: WorkloadFailureKind,
    raw: Value,
}

impl WorkloadFailure {
    /// The failure's model domain.
    pub const fn kind(&self) -> WorkloadFailureKind {
        self.kind
    }

    /// Inspect provider details explicitly; never log this untrusted payload.
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}

impl fmt::Debug for WorkloadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkloadFailure")
            .field("kind", &self.kind)
            .field("raw", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for WorkloadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Livy workload reported failure")
    }
}

impl std::error::Error for WorkloadFailure {}

fn fail(raw: &Value, kind: WorkloadFailureKind) -> WorkloadFailure {
    WorkloadFailure {
        kind,
        raw: raw.clone(),
    }
}

fn fields(
    value: &Value,
    keys: &[&str],
    valid: fn(&Value) -> bool,
) -> Result<(), NativeError> {
    if keys.iter().any(|key| {
        value.get(key).is_some_and(|field| !field.is_null() && !valid(field))
    }) {
        return Err(NativeError::InvalidResponse);
    }
    Ok(())
}

fn objects(value: &Value) -> bool {
    value.as_array().is_some_and(|items| items.iter().all(Value::is_object))
}

fn strings(value: &Value) -> bool {
    value.as_array().is_some_and(|items| items.iter().all(Value::is_string))
}

fn signed(value: &Value) -> bool {
    value.as_i64().is_some()
}

fn unsigned(value: &Value) -> bool {
    value.as_u64().is_some_and(|number| number <= i64::MAX as u64)
}

fn validate_model(name: &str, value: &Value) -> Result<(), NativeError> {
    if !value.is_object() {
        return Err(NativeError::InvalidResponse);
    }
    if matches!(name, "SessionSnapshot" | "BatchSnapshot" | "StatementSnapshot" | "JobSnapshot") {
        fields(value, &["state", "name", "appId"], Value::is_string)?;
        fields(value, &["id"], |id| id.is_string() || signed(id))?;
    }
    match name {
        "SessionSnapshot" => {
            fields(value, &["sessionId", "replId", "sessionTag", "creatorId", "livyInstanceId", "acquiredAtUtc"], Value::is_string)?;
            fields(value, &["errorInfo"], objects)?;
        }
        "BatchSnapshot" => fields(value, &["result", "cancellationReason"], Value::is_string)?,
        "StatementSnapshot" => {
            fields(value, &["id"], signed)?;
            fields(value, &["code"], Value::is_string)?;
            fields(value, &["output"], Value::is_object)?;
            fields(value, &["next", "total"], signed)?;
        }
        "JobSnapshot" => {
            fields(value, &["id"], signed)?;
            fields(value, &["result", "error"], Value::is_string)?;
        }
        "ActivityList" => {
            fields(value, &["items", "sessions", "statements"], objects)?;
            fields(value, &["totalCountOfMatchedItems", "total", "total_statements", "pageSize"], unsigned)?;
        }
        "Logs" => {
            fields(value, &["log"], strings)?;
            fields(value, &["from", "total"], unsigned)?;
        }
        "Timeout" => fields(value, &["serverTimeout", "sessionTimeout", "replTimeout"], signed)?,
        "Completion" => fields(value, &["candidates"], strings)?,
        "Inspection" => fields(value, &["vars"], objects)?,
        "CancellationResult" => fields(value, &["msg"], Value::is_string)?,
        _ => {}
    }
    Ok(())
}

macro_rules! model {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, PartialEq)]
        pub struct $name(Value);

        impl sealed::Sealed for $name {}

        impl ResponseModel for $name {
            fn decode(value: Value) -> Result<Self, NativeError> {
                validate_model(stringify!($name), &value)?;
                Ok(Self(value))
            }
        }

        impl $name {
            /// Borrow every native field, including unrecognized nested fields.
            pub fn raw(&self) -> &Value {
                &self.0
            }

            /// Exact provider state; no case normalization is written into the payload.
            pub fn state(&self) -> Option<&str> {
                self.0.get("state").and_then(Value::as_str)
            }

            /// Native integer or string ID. No backing/acquisition identity substitution.
            pub fn id(&self) -> Option<&Value> {
                self.0.get("id").filter(|value| {
                    value.is_string() || value.as_i64().is_some()
                })
            }

            /// Explicit provider name, if supplied.
            pub fn name(&self) -> Option<&str> {
                self.0.get("name").and_then(Value::as_str)
            }

            /// Provider Spark application identity, not a session or statement ID.
            pub fn application_id(&self) -> Option<&str> {
                self.0.get("appId").and_then(Value::as_str)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
    };
}

model!(ServiceMetadata, "Opaque native service version metadata, not a workload state.");
model!(SessionSnapshot, "Classic session, REPL or HC management snapshot; identities remain distinct.");
model!(BatchSnapshot, "Batch application state, result and cancellation reason.");
model!(StatementSnapshot, "Statement state and raw output, including error and byte fragments.");
model!(JobSnapshot, "Serialized job state and opaque result; no deserialization is performed.");
model!(ActivityList, "Fabric items or native sessions/statements list, without paging inference.");
model!(Logs, "One native log page, not a complete Spark/container log inventory.");
model!(Timeout, "Timeout values in milliseconds, including inherited negative values.");
model!(Completion, "Interpreter-provided completion candidates.");
model!(Inspection, "Interpreter-provided variable metadata.");
model!(OpaqueJson, "Opaque JSON response, including embedded Jupyter debug messages.");
model!(CancellationResult, "A cancellation/interrupt/reset acknowledgement, not terminal evidence.");

/// A bodyless or textual native acknowledgement; raw bytes remain on the enclosing response.
#[derive(Clone, PartialEq)]
pub struct Acknowledgement(Option<Value>);

impl sealed::Sealed for Acknowledgement {}

impl ResponseModel for Acknowledgement {
    fn decode(value: Value) -> Result<Self, NativeError> {
        Ok(Self(if value.is_null() { None } else { Some(value) }))
    }
}

impl Acknowledgement {
    /// Optional JSON acknowledgement. Text and empty bodies are not fabricated as JSON.
    pub fn payload(&self) -> Option<&Value> {
        self.0.as_ref()
    }

    /// Acknowledgement alone never proves remote termination.
    pub const fn proves_terminal(&self) -> bool {
        false
    }
}

impl fmt::Debug for Acknowledgement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Acknowledgement(<redacted>)")
    }
}

/// Native reset has an acknowledgement; Fabric reset returns a session representation.
#[derive(Clone, PartialEq)]
pub struct SessionTimeoutReset {
    session: Option<SessionSnapshot>,
    acknowledgement: Option<Value>,
}

impl sealed::Sealed for SessionTimeoutReset {}

impl ResponseModel for SessionTimeoutReset {
    fn decode(value: Value) -> Result<Self, NativeError> {
        SessionSnapshot::decode(value).map(|session| Self {
            session: Some(session),
            acknowledgement: None,
        })
    }

    fn decode_acknowledgement(value: Value) -> Result<Self, NativeError> {
        Ok(Self {
            session: None,
            acknowledgement: if value.is_null() { None } else { Some(value) },
        })
    }
}

impl SessionTimeoutReset {
    /// Fabric's returned session, if present; never fabricate one for native reset.
    pub fn session(&self) -> Option<&SessionSnapshot> {
        self.session.as_ref()
    }

    /// Native JSON acknowledgement, never presented as a Fabric session.
    pub fn acknowledgement(&self) -> Option<&Value> {
        self.acknowledgement.as_ref()
    }
}

impl fmt::Debug for SessionTimeoutReset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionTimeoutReset(<redacted>)")
    }
}

impl SessionSnapshot {
    /// Exact backing Livy ID supplied by the service; never falls back to acquisition `id`.
    pub fn backing_session_id(&self) -> Option<&str> {
        self.0.get("sessionId").and_then(Value::as_str)
    }

    /// Explicit REPL identity, if returned.
    pub fn repl_id(&self) -> Option<&str> {
        self.0.get("replId").and_then(Value::as_str)
    }

    /// Provider HC sharing tag, never used to guess an acquisition owner.
    pub fn session_tag(&self) -> Option<&str> {
        self.0.get("sessionTag").and_then(Value::as_str)
    }

    /// Explicit creator identity from the response.
    pub fn creator_id(&self) -> Option<&str> {
        self.0.get("creatorId").and_then(Value::as_str)
    }

    /// Provider Livy instance identity.
    pub fn livy_instance_id(&self) -> Option<&str> {
        self.0.get("livyInstanceId").and_then(Value::as_str)
    }

    /// Acquisition timestamp as reported, without timezone inference.
    pub fn acquired_at_utc(&self) -> Option<&str> {
        self.0.get("acquiredAtUtc").and_then(Value::as_str)
    }

    /// Structured native failure information, preserved without filtering.
    pub fn error_info(&self) -> Option<&Value> {
        self.0.get("errorInfo")
    }

    /// Classify readiness separately from application success.
    ///
    /// # Errors
    /// Reports native failed states or nonempty structured `errorInfo`.
    pub fn observe(&self) -> Result<Observation, WorkloadFailure> {
        if self.0.get("errorInfo").is_some_and(|value| {
            value.as_array().is_some_and(|items| !items.is_empty())
        }) {
            return Err(fail(&self.0, WorkloadFailureKind::Session));
        }
        match self.state().map(str::to_ascii_lowercase).as_deref() {
            Some("dead" | "error" | "killed" | "failed") => {
                Err(fail(&self.0, WorkloadFailureKind::Session))
            }
            Some("idle") => Ok(Observation::Ready),
            Some("busy") => Ok(Observation::Running),
            Some("starting" | "not_started" | "notstarted" | "shutting_down") => {
                Ok(Observation::Pending)
            }
            _ => Ok(Observation::Unknown),
        }
    }
}

impl BatchSnapshot {
    /// Raw application result, distinct from its HTTP and activity state.
    pub fn result(&self) -> Option<&str> {
        self.0.get("result").and_then(Value::as_str)
    }

    /// Raw provider cancellation reason.
    pub fn cancellation_reason(&self) -> Option<&str> {
        self.0.get("cancellationReason").and_then(Value::as_str)
    }

    /// Inspect this batch's actual state.
    ///
    /// # Errors
    /// Reports known failed terminal states.
    pub fn observe(&self) -> Result<Observation, WorkloadFailure> {
        match self.state().map(str::to_ascii_lowercase).as_deref() {
            Some("dead" | "error" | "killed" | "failed") => {
                Err(fail(&self.0, WorkloadFailureKind::Batch))
            }
            Some("success") => Ok(Observation::Succeeded),
            Some("cancelled" | "canceled") => Ok(Observation::Cancelled),
            Some("running" | "busy") => Ok(Observation::Running),
            Some("starting" | "not_started" | "queued" | "waiting") => Ok(Observation::Pending),
            _ => Ok(Observation::Unknown),
        }
    }
}

impl StatementSnapshot {
    /// Exact numeric statement identity; never a source cell ID.
    pub fn statement_id(&self) -> Option<i64> {
        self.0.get("id").and_then(Value::as_i64)
    }

    /// Executable code returned by the service; explicitly accessible but never logged.
    pub fn code(&self) -> Option<&str> {
        self.0.get("code").and_then(Value::as_str)
    }

    /// Statement output with native MIME/schema/data/error fields unchanged.
    pub fn output(&self) -> Option<&Value> {
        self.0.get("output")
    }

    /// Native output schema if supplied; no inferred dataframe schema.
    pub fn schema(&self) -> Option<&Value> {
        self.output()?.get("data")?.get("schema")
    }

    /// Exact direct or nested byte cursor, including `-1` and `-2` sentinels.
    pub fn next_offset(&self) -> Option<i64> {
        self.cursor_metadata()?.get("next").and_then(Value::as_i64)
    }

    /// Provider total metadata; not proof that all original rows were retained.
    pub fn total(&self) -> Option<i64> {
        self.0.get("total").and_then(Value::as_i64)
            .or_else(|| self.output()?.get("data")?.get("total")?.as_i64())
    }

    /// Inspect statement state and output status independently.
    ///
    /// # Errors
    /// `available` with output `status=error` is workload failure, not success.
    pub fn observe(&self) -> Result<Observation, WorkloadFailure> {
        if self.output().and_then(|output| output.get("status")).and_then(Value::as_str)
            == Some("error")
        {
            return Err(fail(&self.0, WorkloadFailureKind::StatementOutput));
        }
        match self.state().map(str::to_ascii_lowercase).as_deref() {
            Some("error" | "failed") => Err(fail(&self.0, WorkloadFailureKind::StatementState)),
            Some("cancelled" | "canceled") => Ok(Observation::Cancelled),
            Some("available") => {
                if self.output().and_then(|output| output.get("status")).and_then(Value::as_str)
                    == Some("ok")
                {
                    Ok(Observation::Succeeded)
                } else {
                    Ok(Observation::Unknown)
                }
            }
            Some("running") => Ok(Observation::Running),
            Some("waiting" | "queued" | "cancelling") => Ok(Observation::Pending),
            _ => Ok(Observation::Unknown),
        }
    }

    pub(super) fn cursor_metadata(&self) -> Option<&Value> {
        if self.0.get("next").is_some() {
            Some(&self.0)
        } else {
            self.output()?.get("data")
        }
    }
}

impl JobSnapshot {
    /// Opaque serialized result string. Never decoded or executed by the SDK.
    pub fn result(&self) -> Option<&str> {
        self.0.get("result").and_then(Value::as_str)
    }

    /// Raw application error, never included in Debug or Display.
    pub fn error(&self) -> Option<&str> {
        self.0.get("error").and_then(Value::as_str)
    }

    /// Classify job state without interpreting application serialization.
    ///
    /// # Errors
    /// Reports failed states and explicit nonempty application errors.
    pub fn observe(&self) -> Result<Observation, WorkloadFailure> {
        if self.error().is_some_and(|value| !value.is_empty()) {
            return Err(fail(&self.0, WorkloadFailureKind::Job));
        }
        match self.state().map(str::to_ascii_lowercase).as_deref() {
            Some("failed" | "error") => Err(fail(&self.0, WorkloadFailureKind::Job)),
            Some("succeeded") => Ok(Observation::Succeeded),
            Some("cancelled" | "canceled") => Ok(Observation::Cancelled),
            Some("started" | "running") => Ok(Observation::Running),
            Some("queued" | "sent") => Ok(Observation::Pending),
            _ => Ok(Observation::Unknown),
        }
    }
}

impl ActivityList {
    /// Native list entries or Fabric `items`; missing/malformed lists remain explicit.
    ///
    /// # Errors
    /// Rejects missing/non-array/non-object entries rather than returning an empty-success fallback.
    pub fn items(&self) -> Result<&[Value], NativeError> {
        let value = ["items", "sessions", "statements"]
            .iter()
            .find_map(|key| self.0.get(key))
            .and_then(Value::as_array)
            .ok_or(NativeError::InvalidResponse)?;
        if value.iter().any(|item| !item.is_object()) {
            return Err(NativeError::InvalidResponse);
        }
        Ok(value)
    }

    /// Provider-reported total, not a claim that all pages were retrieved.
    pub fn total(&self) -> Option<u64> {
        ["totalCountOfMatchedItems", "total", "total_statements"]
            .iter()
            .find_map(|key| self.0.get(key).and_then(Value::as_u64))
    }

    /// Provider-reported page size, if present.
    pub fn page_size(&self) -> Option<u64> {
        self.0.get("pageSize").and_then(Value::as_u64)
    }

    /// Decode this list's entries as session snapshots without losing the list's raw fields.
    ///
    /// # Errors
    /// Rejects missing/malformed entries or known session field types.
    pub fn sessions(&self) -> Result<Vec<SessionSnapshot>, NativeError> {
        self.items()?.iter().cloned().map(SessionSnapshot::decode).collect()
    }

    /// Decode this list's entries as batch snapshots.
    ///
    /// # Errors
    /// Rejects malformed batch entries.
    pub fn batches(&self) -> Result<Vec<BatchSnapshot>, NativeError> {
        self.items()?.iter().cloned().map(BatchSnapshot::decode).collect()
    }

    /// Decode this list's entries as statement snapshots.
    ///
    /// # Errors
    /// Rejects malformed statement entries.
    pub fn statements(&self) -> Result<Vec<StatementSnapshot>, NativeError> {
        self.items()?.iter().cloned().map(StatementSnapshot::decode).collect()
    }
}

impl Logs {
    /// Exact requested log-line offset reported by the provider.
    pub fn offset(&self) -> Option<u64> {
        self.0.get("from").and_then(Value::as_u64)
    }

    /// Provider total log-line count, not a complete log-inventory claim.
    pub fn total(&self) -> Option<u64> {
        self.0.get("total").and_then(Value::as_u64)
    }

    /// Exactly this page's log lines.
    ///
    /// # Errors
    /// Rejects malformed/missing lines instead of silently filtering them.
    pub fn lines(&self) -> Result<Vec<&str>, NativeError> {
        self.0
            .get("log")
            .and_then(Value::as_array)
            .ok_or(NativeError::InvalidResponse)?
            .iter()
            .map(|line| line.as_str().ok_or(NativeError::InvalidResponse))
            .collect()
    }
}

impl Timeout {
    /// Server default milliseconds, including inherited negative values.
    pub fn server_timeout(&self) -> Option<i64> {
        self.0.get("serverTimeout").and_then(Value::as_i64)
    }

    /// Session milliseconds; `-1` can mean inherited, not a setter value.
    pub fn session_timeout(&self) -> Option<i64> {
        self.0.get("sessionTimeout").and_then(Value::as_i64)
    }

    /// REPL milliseconds; `-1` can mean inherited.
    pub fn repl_timeout(&self) -> Option<i64> {
        self.0.get("replTimeout").and_then(Value::as_i64)
    }
}

impl Completion {
    /// Candidates from this interpreter.
    ///
    /// # Errors
    /// Rejects malformed or absent candidate lists.
    pub fn candidates(&self) -> Result<Vec<&str>, NativeError> {
        self.0.get("candidates").and_then(Value::as_array)
            .ok_or(NativeError::InvalidResponse)?
            .iter()
            .map(|item| item.as_str().ok_or(NativeError::InvalidResponse))
            .collect()
    }
}

impl Inspection {
    /// Unmodified variable metadata objects.
    ///
    /// # Errors
    /// Rejects malformed or missing variable lists.
    pub fn variables(&self) -> Result<&[Value], NativeError> {
        let variables = self.0.get("vars").and_then(Value::as_array)
            .ok_or(NativeError::InvalidResponse)?;
        if variables.iter().any(|value| !value.is_object()) {
            return Err(NativeError::InvalidResponse);
        }
        Ok(variables)
    }
}

impl CancellationResult {
    /// Provider acknowledgement message, not proof of cancellation.
    pub fn message(&self) -> Option<&str> {
        self.0.get("msg").and_then(Value::as_str)
    }

    /// An acknowledgement cannot establish the target's terminal state.
    pub const fn proves_terminal(&self) -> bool {
        false
    }
}
