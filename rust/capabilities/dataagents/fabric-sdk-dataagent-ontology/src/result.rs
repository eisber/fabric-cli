// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit endpoint effects, acceptance and lossless bounded failure evidence.

use crate::{ValidationError, scope::{CallerScope, Resource, Validator}};
use fabric_sdk_ontology_workload::workload::{
    DispatchState, WorkloadError, WorkloadResponse,
};
use std::fmt;

/// Source-backed effects; GET is not a promise of an inert server operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Effects {
    /// Authored DataAgent configuration can change, never the Ontology item.
    pub edits_agent_configuration: bool,
    /// The service may migrate/reconcile legacy DataAgent configuration on interaction.
    pub may_migrate_configuration: bool,
    /// Caller-specific ontology/schema caches can refresh, including on GET.
    pub may_refresh_caller_cache: bool,
    /// Source-side example validation/embedding maintenance can run.
    pub may_validate_examples: bool,
    /// Whole-agent staging publication, including non-ontology configuration.
    pub publishes_whole_agent: bool,
    /// A newly attached ontology auto-selects its current live entities.
    pub auto_selects_entities: bool,
    /// Removal deletes retained authored setup beneath the targeted scope.
    pub removes_authored_setup: bool,
}

impl Effects {
    pub(crate) fn read() -> Self {
        Self { may_migrate_configuration: true, may_refresh_caller_cache: true, ..Self::default() }
    }

    pub(crate) fn write() -> Self {
        Self { edits_agent_configuration: true, ..Self::read() }
    }
}

/// Endpoint completion with an exact response and resource-scoped validator.
#[derive(Clone, Debug)]
pub struct Completed<T> {
    /// Exact agent/stage/ontology/source scope, present even without an ETag.
    pub resource: Resource,
    /// Caller cache partition; metadata is not proof of provider claims.
    pub caller: CallerScope,
    /// Parsed source projection, not generated authoring input.
    pub value: T,
    /// Caller/resource-scoped returned ETag, absent when the source omits it.
    pub validator: Option<Validator>,
    /// Original observed response; sensitive authored content is explicit data.
    pub response: WorkloadResponse,
    /// Declared source effects, including cache/migration behavior on reads.
    pub effects: Effects,
}

/// Endpoint acceptance is not completion; this package never polls or replays it.
#[derive(Clone, Debug)]
pub enum Outcome<T> {
    /// The expected endpoint status and response shape were verified.
    Completed(Completed<T>),
    /// The original accepted response is retained for caller-owned reconciliation.
    Accepted {
        /// Original agent/stage/resource for caller-owned reconciliation.
        resource: Resource,
        /// Original caller cache partition.
        caller: CallerScope,
        /// Exact status, headers and bounded body, including Location/operation ID.
        response: WorkloadResponse,
        /// Effects that may follow acceptance.
        effects: Effects,
    },
}

/// Source-based failure classes, preserving the original provider response separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// Validation rejected before dispatch.
    Validation(ValidationError),
    /// Credential/transport/routing failure from the injected boundary.
    Execution,
    /// Local cancellation, not rollback.
    Cancelled,
    /// Source rejected request syntax, identity, feature applicability or validation.
    InvalidInput,
    /// HTTP 401.
    Unauthorized,
    /// HTTP 403, including source-access denial.
    Forbidden,
    /// HTTP 404; not interpreted as an empty collection.
    NotFound,
    /// HTTP 409, including cleanup of a still-available source.
    Conflict,
    /// HTTP 412; original optional condition is never removed or retried.
    PreconditionFailed,
    /// Multi-page traversal observed a changed or missing snapshot validator.
    SnapshotChanged,
    /// HTTP 429; this SDK does not retry.
    Throttled,
    /// Other provider status, retained unchanged.
    Provider,
    /// Unexpected media, JSON shape, status or returned identity.
    InvalidResponse,
    /// A finite request/response allocation bound was exceeded.
    Limit,
}

/// Failure evidence with explicit dispatch certainty and no automatic replay.
#[derive(Clone)]
pub struct Error {
    /// Original resource when an operation was prepared; never inferred from response text.
    pub resource: Option<Box<Resource>>,
    /// Original caller partition when an operation was prepared.
    pub caller: Option<Box<CallerScope>>,
    /// Sanitized classification.
    pub kind: ErrorKind,
    /// Whether submission/completion is known.
    pub dispatch: DispatchState,
    /// Original bounded response when observed.
    pub response: Option<WorkloadResponse>,
    /// True only when a misbehaving executor returned more bytes than permitted.
    pub response_truncated: bool,
    /// Original private executor error, including its dispatch evidence.
    pub workload_error: Option<Box<WorkloadError>>,
    /// Existing public shared operation failure, never replaced with an empty result.
    pub public_error: Option<Box<fabric_sdk_operations::OperationError>>,
    /// Declared source effects.
    pub effects: Effects,
}

impl Error {
    pub(crate) fn local(error: ValidationError) -> Self {
        Self {
            resource: None, caller: None,
            kind: ErrorKind::Validation(error),
            dispatch: DispatchState::NotSent,
            response: None,
            response_truncated: false,
            workload_error: None,
            public_error: None,
            effects: Effects::default(),
        }
    }

    pub(crate) fn observed(kind: ErrorKind, response: WorkloadResponse, effects: Effects) -> Self {
        Self {
            resource: None, caller: None,
            kind,
            dispatch: if effects.edits_agent_configuration && response.status < 300 {
                DispatchState::Unknown
            } else {
                DispatchState::Completed
            },
            response: Some(response),
            response_truncated: false,
            workload_error: None,
            public_error: None,
            effects,
        }
    }

    pub(crate) fn with_scope(mut self, caller: CallerScope, resource: Resource) -> Self {
        self.caller = Some(Box::new(caller));
        self.resource = Some(Box::new(resource));
        self
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Error")
            .field("kind", &self.kind)
            .field("resource", &self.resource)
            .field("dispatch", &self.dispatch)
            .field("response", &self.response)
            .field("response_truncated", &self.response_truncated)
            .field("effects", &self.effects)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DataAgent ontology operation failed or is incomplete; inspect structured evidence")
    }
}

impl std::error::Error for Error {}

pub(crate) fn status_kind(status: u16) -> ErrorKind {
    match status {
        400 => ErrorKind::InvalidInput,
        401 => ErrorKind::Unauthorized,
        403 => ErrorKind::Forbidden,
        404 => ErrorKind::NotFound,
        409 => ErrorKind::Conflict,
        412 => ErrorKind::PreconditionFailed,
        429 => ErrorKind::Throttled,
        _ => ErrorKind::Provider,
    }
}
