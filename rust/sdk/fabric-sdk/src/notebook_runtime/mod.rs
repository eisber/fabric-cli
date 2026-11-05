// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native Notebook workload content, Jupyter REST and offline protocol capabilities.
//!
//! Source: FNTK `e250d5d32b04efcc5623a414a361727a93c271ce`, specifically
//! `constants.py`, `client/notebook_client.py`, `client/jupyter_protocol.py`,
//! `client/ws_client.py`, and `models/lakehouse.py`.
//!
//! The caller supplies the workload origin, capacity/workspace/artifact scope and
//! an existing authenticated [`NativeExecutor`]. Nothing discovers clusters,
//! credentials or tokens. Creating a Jupyter session does not establish Spark
//! readiness. A WebSocket transport/host remains explicitly unsupported here.
//! See `rust/docs/notebook-runtime-native.md` for source semantics and host gaps.

#![deny(missing_docs)]

pub mod content;
pub mod protocol;

use crate::runtime::{
    NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeMethod, NativeRequest,
    NativeResponse, validate_segment,
};
use content::{ContentLimits, NotebookDocument, WritePolicy, encoded_len, validate_document};
use serde_json::{Value, json};
use std::fmt;

/// Authoritative implementation source revision, not a live service version.
pub const SOURCE_REVISION: &str = "e250d5d32b04efcc5623a414a361727a93c271ce";

macro_rules! opaque_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Eq, Hash, PartialEq)]
        pub struct $name(String);

        impl $name {
            /// Validate an explicit opaque identifier without route-changing characters.
            ///
            /// # Errors
            /// Rejects empty identifiers, escapes, separators and control characters.
            pub fn new(value: impl Into<String>) -> Result<Self, NativeError> {
                let value = value.into();
                validate_segment(&value)?;
                if value.len() > 1024 {
                    return Err(NativeError::Limit);
                }
                Ok(Self(value))
            }

            /// Borrow this identifier for the matching native protocol field.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
    };
}

opaque_id!(
    SessionId,
    "A Jupyter session ID, never a kernel, Livy or execution ID."
);
opaque_id!(
    KernelId,
    "A kernel ID, never a Jupyter session or execution ID."
);
opaque_id!(
    MessageId,
    "A Jupyter message ID used for protocol correlation."
);
opaque_id!(CommId, "An explicitly opened Synapse comm channel ID.");
opaque_id!(
    ExecutionId,
    "A caller-local execution handle, distinct from the remote execute-request message ID."
);

/// Positive Jupyter REST creation input; no hidden reuse or Spark startup.
#[derive(Clone)]
pub struct CreateSession {
    body: Value,
}

impl CreateSession {
    /// Create a Notebook session payload using an explicit kernel name.
    ///
    /// An empty path retains the pinned source fallback `/notebook-cli/{name}`.
    ///
    /// # Errors
    /// Rejects empty name/kernel, control characters or non-object session options.
    pub fn new(
        name: &str,
        path: &str,
        kernel_name: &str,
        session_options: Option<Value>,
    ) -> Result<Self, NativeError> {
        if name.is_empty()
            || kernel_name.is_empty()
            || [name, path, kernel_name]
                .iter()
                .any(|value| value.chars().any(char::is_control))
            || session_options
                .as_ref()
                .is_some_and(|value| !value.is_object())
        {
            return Err(NativeError::InvalidInput);
        }
        let path = if path.is_empty() {
            format!("/notebook-cli/{name}")
        } else {
            path.into()
        };
        let mut body = json!({
            "name": name,
            "path": path,
            "type": "notebook",
            "kernel": {"name": kernel_name}
        });
        if let Some(options) = session_options
            .filter(|options| options.as_object().is_some_and(|object| !object.is_empty()))
        {
            body["kernel"]["synapseExtension"] = options;
        }
        Ok(Self { body })
    }

    /// Borrow the native body, including nonempty `kernel.synapseExtension` options.
    pub fn as_value(&self) -> &Value {
        &self.body
    }
}

/// A session response retaining unknown provider fields without ID substitution.
#[derive(Clone)]
pub struct JupyterSession {
    id: SessionId,
    kernel_id: Option<KernelId>,
    value: Value,
}

impl JupyterSession {
    /// The actual Jupyter session ID, not its kernel or Livy ID.
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// An independently typed kernel ID when the provider supplied a kernel.
    pub fn kernel_id(&self) -> Option<&KernelId> {
        self.kernel_id.as_ref()
    }

    /// Raw session metadata, including provider states and Synapse extension fields.
    ///
    /// An HTTP success or an unknown state is never converted into readiness.
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    fn parse(value: Value) -> Result<Self, NativeError> {
        let id = response_id(value.get("id").and_then(Value::as_str))?;
        let kernel_id = match value.get("kernel") {
            None | Some(Value::Null) => None,
            Some(Value::Object(kernel)) => Some(
                KernelId::new(
                    kernel
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or(NativeError::InvalidResponse)?,
                )
                .map_err(|_| NativeError::InvalidResponse)?,
            ),
            _ => return Err(NativeError::InvalidResponse),
        };
        Ok(Self {
            id,
            kernel_id,
            value,
        })
    }
}

/// A native mutation response; acceptance is not execution or readiness.
pub struct HttpOutcome {
    /// Actual status, including 202 when only accepted.
    pub status: u16,
    /// Actual provider JSON, or absent for an empty response.
    pub body: Option<Value>,
}

/// Exact existing Jupyter/kernel binding for reconnecting a caller-owned host.
///
/// Resume does not create a session, replay outputs or re-execute source.
/// The identifier types are intentionally not interchangeable:
///
/// ```compile_fail
/// use fabric_sdk::notebook_runtime::{KernelId, SessionId};
/// let kernel: KernelId = SessionId::new("session").unwrap();
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeBinding {
    /// Existing Jupyter session, not a Livy ID.
    pub session: SessionId,
    /// Existing kernel, not an execution message ID.
    pub kernel: KernelId,
}

/// Explicitly scoped Notebook workload operations over the shared runtime seam.
#[derive(Clone)]
pub struct NotebookRuntime {
    endpoint: NativeEndpoint,
    capacity: String,
    workspace: String,
    notebook: String,
    content_limits: ContentLimits,
    response_bytes: usize,
    session_limit: usize,
}

impl NotebookRuntime {
    /// Bind an explicit workload origin and independently validated scope.
    ///
    /// The endpoint must be an origin (path `/`), not a public Fabric `/v1`
    /// endpoint or an already-scoped Notebook route. Authorization remains host-owned.
    ///
    /// # Errors
    /// Rejects non-origin bases, invalid scope segments and zero budgets.
    pub fn new(
        workload_origin: NativeEndpoint,
        capacity: &str,
        workspace: &str,
        notebook: &str,
        content_limits: ContentLimits,
        response_bytes: usize,
        session_limit: usize,
    ) -> Result<Self, NativeError> {
        if workload_origin.base().path() != "/" || response_bytes == 0 || session_limit == 0 {
            return Err(NativeError::InvalidInput);
        }
        for id in [capacity, workspace, notebook] {
            validate_segment(id)?;
            if id.len() > 1024 {
                return Err(NativeError::Limit);
            }
        }
        content_limits.validate()?;
        Ok(Self {
            endpoint: workload_origin,
            capacity: capacity.into(),
            workspace: workspace.into(),
            notebook: notebook.into(),
            content_limits,
            response_bytes,
            session_limit,
        })
    }

    /// Build native NBS `GET /content` (full IPYNB, not item definition).
    ///
    /// # Errors
    /// Returns shared request-validation errors before any host access.
    pub fn content_get_request(&self) -> Result<NativeRequest, NativeError> {
        self.request(
            NativeMethod::Get,
            &["content"],
            &[],
            None,
            NativeEffect::Read,
        )
    }

    /// Build native NBS `HEAD /content`.
    ///
    /// The shared response currently carries no headers; no ETag/CAS is claimed.
    ///
    /// # Errors
    /// Returns shared request-validation errors.
    pub fn content_head_request(&self) -> Result<NativeRequest, NativeError> {
        self.request(
            NativeMethod::Head,
            &["content"],
            &[],
            None,
            NativeEffect::Read,
        )
    }

    /// Build a whole-notebook PUT, including for a one-cell or Lakehouse edit.
    ///
    /// # Errors
    /// Rejects guarded-write policy and documents exceeding this client's budgets.
    pub fn content_put_request(
        &self,
        document: &NotebookDocument,
        policy: WritePolicy,
    ) -> Result<NativeRequest, NativeError> {
        if policy != WritePolicy::AllowUnguarded {
            return Err(NativeError::UnsupportedCapability);
        }
        validate_document(document.as_value(), self.content_limits)?;
        self.request(
            NativeMethod::Put,
            &["content"],
            &[],
            Some(document.as_value().clone()),
            NativeEffect::Update,
        )
    }

    /// Build a Jupyter session collection read, optionally using `/sessions/active`.
    ///
    /// # Errors
    /// Returns shared request-validation errors.
    pub fn sessions_request(&self, active_only: bool) -> Result<NativeRequest, NativeError> {
        let mut route = vec!["jupyterApi", "versions", "1", "api", "sessions"];
        if active_only {
            route.push("active");
        }
        self.request(NativeMethod::Get, &route, &[], None, NativeEffect::Read)
    }

    /// Build a session GET with the source's optional `detailed=true` query.
    ///
    /// # Errors
    /// Returns shared request-validation errors.
    pub fn session_get_request(
        &self,
        session: &SessionId,
        detailed: bool,
    ) -> Result<NativeRequest, NativeError> {
        let query = if detailed {
            vec![("detailed", "true".into())]
        } else {
            Vec::new()
        };
        self.session_request(NativeMethod::Get, session, &query, None, NativeEffect::Read)
    }

    /// Build session creation only; full managed Spark startup requires a WS host.
    ///
    /// # Errors
    /// Rejects payloads exceeding the response/payload byte budget.
    pub fn session_create_request(
        &self,
        input: &CreateSession,
    ) -> Result<NativeRequest, NativeError> {
        encoded_len(input.as_value(), self.response_bytes)?;
        self.request(
            NativeMethod::Post,
            &["jupyterApi", "versions", "1", "api", "sessions"],
            &[],
            Some(input.body.clone()),
            NativeEffect::Create,
        )
    }

    /// Build the native provider-defined session PATCH without hidden defaults.
    ///
    /// # Errors
    /// Rejects non-object, empty or over-budget patch bodies.
    pub fn session_patch_request(
        &self,
        session: &SessionId,
        patch: Value,
    ) -> Result<NativeRequest, NativeError> {
        if !patch.as_object().is_some_and(|object| !object.is_empty()) {
            return Err(NativeError::InvalidInput);
        }
        self.session_request(
            NativeMethod::Patch,
            session,
            &[],
            Some(patch),
            NativeEffect::Update,
        )
    }

    /// Build deletion of one explicit session, with no local daemon/peer cleanup.
    ///
    /// # Errors
    /// Returns shared request-validation errors.
    pub fn session_delete_request(
        &self,
        session: &SessionId,
    ) -> Result<NativeRequest, NativeError> {
        self.session_request(
            NativeMethod::Delete,
            session,
            &[],
            None,
            NativeEffect::Delete,
        )
    }

    /// Build a kernel-wide restart POST, never an exact execution cancellation.
    ///
    /// # Errors
    /// Returns shared request-validation errors.
    pub fn kernel_restart_request(&self, kernel: &KernelId) -> Result<NativeRequest, NativeError> {
        self.kernel_request(kernel, "restart", NativeEffect::Lifecycle)
    }

    /// Build a kernel-wide interrupt POST, not an exact statement cancel.
    ///
    /// # Errors
    /// Returns shared request-validation errors.
    pub fn kernel_interrupt_request(
        &self,
        kernel: &KernelId,
    ) -> Result<NativeRequest, NativeError> {
        self.kernel_request(kernel, "interrupt", NativeEffect::Cancel)
    }

    /// Read full content exactly once through the caller's authenticated executor.
    ///
    /// # Errors
    /// Preserves executor/HTTP failures and rejects invalid or over-budget content.
    pub async fn get_content(
        &self,
        executor: &dyn NativeExecutor,
    ) -> Result<NotebookDocument, NativeError> {
        let response = self
            .send(
                executor,
                &self.content_get_request()?,
                self.content_limits.document_bytes,
            )
            .await?;
        if response.status != 200 {
            return Err(NativeError::InvalidResponse);
        }
        NotebookDocument::from_slice(&response.body, self.content_limits).map_err(response_error)
    }

    /// Execute the native HEAD; headers/ETag are not exposed by the shared contract.
    ///
    /// # Errors
    /// Preserves executor/HTTP failures; a successful HEAD must have no body.
    pub async fn head_content(&self, executor: &dyn NativeExecutor) -> Result<(), NativeError> {
        let response = self
            .send(executor, &self.content_head_request()?, self.response_bytes)
            .await?;
        if response.status != 200 || !response.body.is_empty() {
            return Err(NativeError::InvalidResponse);
        }
        Ok(())
    }

    /// Save a full notebook exactly once; no retry, merge or remote guard is installed.
    ///
    /// # Errors
    /// Rejects guarded policy before dispatch and preserves uncertain/executor failures.
    pub async fn put_content(
        &self,
        executor: &dyn NativeExecutor,
        document: &NotebookDocument,
        policy: WritePolicy,
    ) -> Result<HttpOutcome, NativeError> {
        self.mutate(executor, &self.content_put_request(document, policy)?)
            .await
    }

    /// List actual Jupyter sessions, rejecting truncated/unknown collection envelopes.
    ///
    /// # Errors
    /// Preserves HTTP failures and rejects malformed rows, paging or exceeded limits.
    pub async fn list_sessions(
        &self,
        executor: &dyn NativeExecutor,
        active_only: bool,
    ) -> Result<Vec<JupyterSession>, NativeError> {
        let response = self
            .send(
                executor,
                &self.sessions_request(active_only)?,
                self.response_bytes,
            )
            .await?;
        let value = required_json(response)?;
        let rows = match value {
            Value::Array(rows) => rows,
            Value::Object(mut object) => {
                for field in ["continuationToken", "continuationUri", "nextLink"] {
                    if object
                        .get(field)
                        .is_some_and(|value| !value.is_null() && value != "")
                    {
                        return Err(NativeError::UnsupportedCapability);
                    }
                }
                object
                    .remove("value")
                    .and_then(|value| value.as_array().cloned())
                    .ok_or(NativeError::InvalidResponse)?
            }
            _ => return Err(NativeError::InvalidResponse),
        };
        if rows.len() > self.session_limit {
            return Err(NativeError::Limit);
        }
        rows.into_iter().map(JupyterSession::parse).collect()
    }

    /// Get exactly the requested session without interpreting readiness.
    ///
    /// # Errors
    /// Rejects mismatched response session IDs and preserves HTTP/executor failures.
    pub async fn get_session(
        &self,
        executor: &dyn NativeExecutor,
        session: &SessionId,
        detailed: bool,
    ) -> Result<JupyterSession, NativeError> {
        let response = self
            .send(
                executor,
                &self.session_get_request(session, detailed)?,
                self.response_bytes,
            )
            .await?;
        let result = JupyterSession::parse(required_json(response)?)?;
        if result.id() != session {
            return Err(NativeError::InvalidResponse);
        }
        Ok(result)
    }

    /// Create one Jupyter session without an implicit connect/start/retry.
    ///
    /// # Errors
    /// Preserves HTTP/executor failures and rejects non-authoritative session bodies.
    pub async fn create_session(
        &self,
        executor: &dyn NativeExecutor,
        input: &CreateSession,
    ) -> Result<JupyterSession, NativeError> {
        let response = self
            .send(
                executor,
                &self.session_create_request(input)?,
                self.response_bytes,
            )
            .await?;
        JupyterSession::parse(required_json(response)?)
    }

    /// Patch one exact session and retain its native response fields.
    ///
    /// # Errors
    /// Rejects invalid patches and response-ID changes; never replays mutations.
    pub async fn patch_session(
        &self,
        executor: &dyn NativeExecutor,
        session: &SessionId,
        patch: Value,
    ) -> Result<JupyterSession, NativeError> {
        let response = self
            .send(
                executor,
                &self.session_patch_request(session, patch)?,
                self.response_bytes,
            )
            .await?;
        let result = JupyterSession::parse(required_json(response)?)?;
        if result.id() != session {
            return Err(NativeError::InvalidResponse);
        }
        Ok(result)
    }

    /// Delete one session exactly once; no replacement session is created.
    ///
    /// # Errors
    /// Preserves HTTP/executor errors, including unknown outcomes.
    pub async fn delete_session(
        &self,
        executor: &dyn NativeExecutor,
        session: &SessionId,
    ) -> Result<HttpOutcome, NativeError> {
        self.mutate(executor, &self.session_delete_request(session)?)
            .await
    }

    /// Restart the explicitly identified kernel, not a session or message ID.
    ///
    /// # Errors
    /// Preserves HTTP/executor errors and never retries the lifecycle operation.
    pub async fn restart_kernel(
        &self,
        executor: &dyn NativeExecutor,
        kernel: &KernelId,
    ) -> Result<HttpOutcome, NativeError> {
        self.mutate(executor, &self.kernel_restart_request(kernel)?)
            .await
    }

    /// Interrupt the entire explicitly selected kernel.
    ///
    /// # Errors
    /// Preserves HTTP/executor errors; acceptance is not terminal cancellation.
    pub async fn interrupt_kernel(
        &self,
        executor: &dyn NativeExecutor,
        kernel: &KernelId,
    ) -> Result<HttpOutcome, NativeError> {
        self.mutate(executor, &self.kernel_interrupt_request(kernel)?)
            .await
    }

    /// Require the missing authenticated WebSocket transport/caller host explicitly.
    ///
    /// # Errors
    /// Always returns [`NativeError::UnsupportedCapability`]; offline protocol
    /// construction does not establish a connection or execute code.
    pub fn require_websocket_host(&self) -> Result<(), NativeError> {
        Err(NativeError::UnsupportedCapability)
    }

    /// Request reconnect to an existing binding, without starting or replaying work.
    ///
    /// # Errors
    /// Always returns [`NativeError::UnsupportedCapability`] until a caller-owned
    /// WebSocket host is composed; REST session existence is insufficient.
    pub fn resume(&self, _binding: &ResumeBinding) -> Result<(), NativeError> {
        self.require_websocket_host()
    }

    fn request(
        &self,
        method: NativeMethod,
        suffix: &[&str],
        query: &[(&str, String)],
        body: Option<Value>,
        effect: NativeEffect,
    ) -> Result<NativeRequest, NativeError> {
        if let Some(body) = &body {
            let limit = if suffix == ["content"] {
                self.content_limits.document_bytes
            } else {
                self.response_bytes
            };
            encoded_len(body, limit)?;
        }
        let mut segments = vec![
            "webapi",
            "capacities",
            &self.capacity,
            "workloads",
            "Notebook",
            "Data",
            "Automatic",
            "api",
            "workspaces",
            &self.workspace,
            "artifacts",
            &self.notebook,
        ];
        segments.extend_from_slice(suffix);
        self.endpoint
            .request(method, &segments, query, body, effect)
    }

    fn session_request(
        &self,
        method: NativeMethod,
        session: &SessionId,
        query: &[(&str, String)],
        body: Option<Value>,
        effect: NativeEffect,
    ) -> Result<NativeRequest, NativeError> {
        if session.as_str() == "active" {
            return Err(NativeError::InvalidInput);
        }
        self.request(
            method,
            &[
                "jupyterApi",
                "versions",
                "1",
                "api",
                "sessions",
                session.as_str(),
            ],
            query,
            body,
            effect,
        )
    }

    fn kernel_request(
        &self,
        kernel: &KernelId,
        action: &str,
        effect: NativeEffect,
    ) -> Result<NativeRequest, NativeError> {
        self.request(
            NativeMethod::Post,
            &[
                "jupyterApi",
                "versions",
                "1",
                "api",
                "kernels",
                kernel.as_str(),
                action,
            ],
            &[],
            None,
            effect,
        )
    }

    async fn send(
        &self,
        executor: &dyn NativeExecutor,
        request: &NativeRequest,
        max_bytes: usize,
    ) -> Result<NativeResponse, NativeError> {
        let response = request.execute(executor, max_bytes).await?;
        if !(200..300).contains(&response.status) {
            return Err(NativeError::Http(response.status));
        }
        Ok(response)
    }

    async fn mutate(
        &self,
        executor: &dyn NativeExecutor,
        request: &NativeRequest,
    ) -> Result<HttpOutcome, NativeError> {
        let response = self.send(executor, request, self.response_bytes).await?;
        let body = if response.body.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&response.body).map_err(|_| NativeError::InvalidResponse)?)
        };
        Ok(HttpOutcome {
            status: response.status,
            body,
        })
    }
}

fn response_id(value: Option<&str>) -> Result<SessionId, NativeError> {
    SessionId::new(value.ok_or(NativeError::InvalidResponse)?)
        .map_err(|_| NativeError::InvalidResponse)
}

fn required_json(response: NativeResponse) -> Result<Value, NativeError> {
    if !matches!(response.status, 200 | 201) {
        return Err(NativeError::InvalidResponse);
    }
    serde_json::from_slice(&response.body).map_err(|_| NativeError::InvalidResponse)
}

fn response_error(error: NativeError) -> NativeError {
    match error {
        NativeError::InvalidInput => NativeError::InvalidResponse,
        error => error,
    }
}
