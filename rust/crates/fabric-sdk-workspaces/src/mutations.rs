// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Resource operations consumed by the shared, caller-owned operation policy.
//!
//! Endpoint clients describe operations; the executor owns credentials, bounded
//! transport, cancellation and reconciliation. It must never replay a mutation.

use serde_json::{Value, json};
use std::{fmt, future::Future, pin::Pin};

/// A finite HTTP method supported by resource operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceMethod {
    /// Read metadata.
    Get,
    /// Create a resource or invoke an action.
    Post,
    /// Update metadata.
    Patch,
    /// Delete a resource.
    Delete,
}

/// How an accepted operation is returned to its consumer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionPolicy {
    /// Python's default synchronous policy: poll when operation headers exist,
    /// retrieve a result only when advertised, otherwise retain the original body.
    PythonSynchronous,
    /// Preserve acceptance, without claiming completion.
    ReturnAccepted,
    /// Reconcile the operation and retrieve its result.
    WaitForResult,
    /// Reconcile completion without retrieving a resource result.
    WaitForCompletion,
}

/// A validated, relative Fabric operation, never an arbitrary URL.
#[derive(Clone)]
pub struct ResourceOperation {
    method: ResourceMethod,
    path: String,
    body: Option<Value>,
    completion: CompletionPolicy,
}

impl ResourceOperation {
    /// Construct a finite public Fabric path; no arbitrary origin is accepted.
    ///
    /// # Errors
    /// Rejects empty, traversing, query-bearing or encoded path components.
    pub fn new(
        method: ResourceMethod,
        segments: &[&str],
        body: Option<Value>,
        completion: CompletionPolicy,
    ) -> Result<Self, ResourceError> {
        if segments.is_empty()
            || segments.iter().any(|segment| {
                segment.is_empty()
                    || matches!(*segment, "." | "..")
                    || !segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
            || body.as_ref().is_some_and(|body| !body.is_object())
            || (method == ResourceMethod::Get && body.is_some())
        {
            return Err(ResourceError::input());
        }
        Ok(Self {
            method,
            path: format!("/v1/{}", segments.join("/")),
            body,
            completion,
        })
    }

    /// HTTP method.
    pub const fn method(&self) -> ResourceMethod {
        self.method
    }

    /// Fixed-origin relative Fabric path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact JSON payload; `None` differs from an empty object.
    pub fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }

    /// Required acceptance/completion behavior.
    pub const fn completion(&self) -> CompletionPolicy {
        self.completion
    }

    /// Add Python's exact hard-delete query spelling to an item DELETE.
    ///
    /// # Errors
    /// Rejects applying this option to another method.
    pub fn with_hard_delete(mut self) -> Result<Self, ResourceError> {
        if self.method != ResourceMethod::Delete {
            return Err(ResourceError::input());
        }
        self.path.push_str("?hardDelete=true");
        Ok(self)
    }
}

impl fmt::Debug for ResourceOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResourceOperation")
            .field("method", &self.method)
            .field("path", &"<resource>")
            .field("body", &"<resource metadata>")
            .field("completion", &self.completion)
            .finish()
    }
}

/// Actual completion or explicit acceptance, never interchangeable.
#[derive(Clone, Debug, PartialEq)]
pub enum ResourceOutcome {
    /// A complete response, retaining its actual status and optional data.
    Completed {
        /// Actual completed HTTP status.
        status: u16,
        /// Parsed resource result, absent for empty responses.
        data: Option<Value>,
    },
    /// Service acceptance only; not evidence of a completed side effect.
    Accepted {
        /// Validated operation identifier for reconciliation, if supplied.
        operation_id: Option<String>,
    },
}

/// Explicit operation failure and reconciliation evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceError {
    /// Python-compatible or explicitly classified safety error code.
    pub code: String,
    /// Sanitized diagnostic, never transport/provider text.
    pub message: String,
    /// HTTP status when a service response was observed.
    pub status: Option<u16>,
    /// Structured service diagnostics retained as data, never formatted implicitly.
    pub service_error: Option<Box<crate::FabricServiceError>>,
    /// Accepted operation identifier, when known.
    pub operation_id: Option<String>,
    /// Whether a mutation may have reached the service; cancellation is not rollback.
    pub mutation_may_have_occurred: bool,
}

impl ResourceError {
    /// Create a local validation failure before any request was dispatched.
    pub fn input() -> Self {
        Self {
            code: "InvalidInput".into(),
            message: "Invalid Fabric resource operation".into(),
            status: None,
            service_error: None,
            operation_id: None,
            mutation_may_have_occurred: false,
        }
    }
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ResourceError {}

/// Caller-injected shared operation policy, not a second transport or LRO stack.
///
/// Implementations must use explicitly injected Azure credentials, enforce
/// cancellation and finite request/body/poll limits, validate operation links,
/// preserve service errors, and report uncertain mutations without replay.
pub trait ResourceOperations: Send {
    /// Execute one operation using the caller's existing transport and policy.
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>>;
}

/// Workspace endpoint operations, independent of CLI or identity implementations.
pub struct WorkspaceMutations;

impl WorkspaceMutations {
    /// Create a workspace from an exact source-shaped JSON payload.
    ///
    /// # Errors
    /// Rejects non-object payloads and propagates policy/service failures.
    pub async fn create(
        executor: &mut dyn ResourceOperations,
        payload: Value,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces"],
                Some(payload),
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Patch workspace metadata or its separately routed Spark settings.
    ///
    /// # Errors
    /// Rejects unsafe IDs and propagates explicit operation failures.
    pub async fn update(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        payload: Value,
        spark_settings: bool,
    ) -> Result<ResourceOutcome, ResourceError> {
        let mut segments = vec!["workspaces", workspace];
        if spark_settings {
            segments.extend(["spark", "settings"]);
        }
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Patch,
                &segments,
                Some(payload),
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Delete a workspace; acceptance remains distinct from completion.
    ///
    /// # Errors
    /// Rejects unsafe IDs and propagates failures without replay.
    pub async fn delete(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Delete,
                &["workspaces", workspace],
                None,
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Assign a workspace to a capacity.
    ///
    /// # Errors
    /// Rejects unsafe workspace IDs and propagates policy/service failures.
    pub async fn assign_capacity(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        capacity: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces", workspace, "assignToCapacity"],
                Some(json!({"capacityId": capacity})),
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Unassign a workspace from its current capacity.
    ///
    /// # Errors
    /// Rejects unsafe workspace IDs and propagates policy/service failures.
    pub async fn unassign_capacity(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces", workspace, "unassignFromCapacity"],
                None,
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }
}
