// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native Warehouse definition retrieval over the existing shared execution policy.
//!
//! This POST is a semantic retrieval requiring read/write permissions. It never
//! falls back to metadata, builds a project, extracts files or deploys a schema.

use crate::{Client, observe::ReadHandle, representations::BorrowedExecutor};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationExecutor, OperationLimits, OperationSession,
};
use fabric_sdk_sql_definition::DefinitionError;
use std::{fmt, future::Future, num::NonZeroUsize, time::Duration};

pub use fabric_sdk_sql_database::warehouse::{
    NativeMediaType, Owned, WarehouseDefinition, WarehouseDefinitionFormat,
    WarehouseDefinitionIdentity, WarehouseDefinitionRequest, WarehouseRequests, WarehouseTarget,
};

/// Actual completed content or acceptance; neither is an operation-state body.
pub enum WarehouseDefinitionRead {
    /// Entire admitted native definition bound to the selected request owner.
    Completed(Owned<WarehouseDefinition>),
    /// Native HTTP 202 only, never permission to replay the POST.
    Accepted {
        /// Explicit request owner, not a provider-echoed identity.
        owner: WarehouseTarget,
        /// Requested effective native representation.
        format: WarehouseDefinitionFormat,
        /// Validated actual native ID, absent if the provider supplied no handle.
        operation_id: Option<String>,
    },
}

impl fmt::Debug for WarehouseDefinitionRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed(value) => f.debug_tuple("Completed").field(value).finish(),
            Self::Accepted { operation_id, .. } => f
                .debug_struct("Accepted")
                .field("has_operation_id", &operation_id.is_some())
                .finish_non_exhaustive(),
        }
    }
}

impl WarehouseDefinitionRead {
    /// Existing-operation handle for the shared observer, only when actually known.
    ///
    /// Use the same Client/immutable identity and endpoint. This performs no
    /// observation and never fabricates a handle for headerless acceptance.
    pub fn accepted_handle(&self) -> Option<ReadHandle> {
        match self {
            Self::Accepted {
                operation_id: Some(id),
                ..
            } => Some(ReadHandle::Operation {
                operation_id: id.clone(),
            }),
            _ => None,
        }
    }
}

/// Protected failure retaining original operation or completed-response evidence.
pub enum WarehouseDefinitionError {
    /// Shared endpoint, credential, service, cancellation, timeout or bound error.
    Operation(OperationError),
    /// A non-200/202 success status cannot be interpreted as a definition.
    UnexpectedStatus {
        /// Actual HTTP response status.
        status: u16,
        /// Complete bounded original body, not implicitly displayed.
        response: Box<[u8]>,
        /// Native operation ID if actually observed.
        operation_id: Option<String>,
    },
    /// Completed body failed shared native admission; no partial definition succeeds.
    Content {
        /// Stable content/limit classification and optional part ordinal.
        error: DefinitionError,
        /// Complete bounded original body for explicit partial-evidence inspection.
        response: Box<[u8]>,
        /// Native operation ID if actually observed.
        operation_id: Option<String>,
    },
}

impl fmt::Debug for WarehouseDefinitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Operation(_) => f.write_str("WarehouseDefinitionError::Operation(<evidence>)"),
            Self::UnexpectedStatus { status, .. } => f
                .debug_struct("UnexpectedStatus")
                .field("status", status)
                .finish_non_exhaustive(),
            Self::Content { error, .. } => f
                .debug_struct("Content")
                .field("error", error)
                .finish_non_exhaustive(),
        }
    }
}

impl fmt::Display for WarehouseDefinitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "Warehouse definition retrieval failed or is incomplete; inspect typed evidence",
        )
    }
}

impl std::error::Error for WarehouseDefinitionError {}

/// Submit exactly one typed native getDefinition POST without hidden observation.
///
/// Credentials, endpoint binding, raw-response bounds and cancellation use the
/// existing shared OperationSession. Original bytes reach the shared codec, not
/// a Value reserialization that could hide duplicate keys or unknown data.
/// HTTP 202 returns acceptance only; use the existing shared observer for readiness
/// and result retrieval, then request.decode_completed_result on its actual result.
///
/// # Errors
/// Preserves native denial/not-supported/service errors with no metadata fallback.
/// Encrypted-label usage rights can deny retrieval even with read/write scopes.
/// Local cancellation is not a remote cancellation or rollback, and POST is never
/// automatically retried. Content failures retain the complete bounded raw body.
pub async fn read_definition(
    client: &Client,
    executor: &dyn OperationExecutor,
    request: &WarehouseDefinitionRequest,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<WarehouseDefinitionRead, WarehouseDefinitionError> {
    let operation = FabricOperation::from_resource(request.operation()).map_err(|kind| {
        WarehouseDefinitionError::Operation(OperationError {
            kind,
            operation_id: None,
            mutation_may_have_occurred: false,
            raw_service_error: None,
        })
    })?;
    let mut session = OperationSession::with_context(
        client.context(),
        BorrowedExecutor(executor),
        OperationLimits {
            max_requests: NonZeroUsize::new(1).unwrap(),
            max_response_bytes: request.limits().max_wire_bytes,
            max_poll_delay: Duration::ZERO,
        },
        cancelled,
    );
    let response = session
        .execute(operation)
        .await
        .map_err(WarehouseDefinitionError::Operation)?;
    session
        .check_cancelled()
        .await
        .map_err(WarehouseDefinitionError::Operation)?;
    if response.status == 202 {
        return Ok(WarehouseDefinitionRead::Accepted {
            owner: request.owner(),
            format: request.format(),
            operation_id: session.operation_id().map(str::to_owned),
        });
    }
    if response.status != 200 {
        return Err(WarehouseDefinitionError::UnexpectedStatus {
            status: response.status,
            response: response.body.into(),
            operation_id: session.operation_id().map(str::to_owned),
        });
    }
    let definition = request
        .decode_completed_result(&response.body)
        .map_err(|error| WarehouseDefinitionError::Content {
            error,
            response: response.body.into(),
            operation_id: session.operation_id().map(str::to_owned),
        })?;
    session
        .check_cancelled()
        .await
        .map_err(WarehouseDefinitionError::Operation)?;
    Ok(WarehouseDefinitionRead::Completed(definition))
}
