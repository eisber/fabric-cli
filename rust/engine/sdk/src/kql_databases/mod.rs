// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed KQLDatabase management reads and inert Kusto context descriptors.
//!
//! Metadata execution borrows the existing Fabric operation policy. Context
//! resolution never acquires tokens, probes an endpoint, selects a provider or
//! submits Kusto queries, commands or ingestion. Query consumers must independently
//! enforce their native read-only policy and effective permissions.

mod binding;
mod metadata;
mod registration;
mod requests;

pub use crate::sql_databases::{Observed, PageCoverage};
pub use binding::{DatabaseSelector, KustoContext, KustoResource};
pub use metadata::{DatabaseKind, KqlDatabase, KqlDatabasePage, KqlProperties};
pub use registration::register;
pub use requests::{KqlDatabaseRequests, KqlListOptions, KqlRequest};

use std::fmt;

/// Sanitized input, response or binding failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KqlErrorKind {
    /// An owner, facet or concrete identity did not match.
    TargetMismatch,
    /// A caller selected an unsupported representation or native option.
    UnsupportedCapability,
    /// Invalid identifiers, tokens, URIs or database selector.
    InvalidInput,
    /// The shared context is not a Fabric metadata context.
    InvalidContext,
    /// Required native metadata was missing, null or unknown.
    MissingBinding,
    /// Malformed known fields or an invalid native metadata envelope.
    InvalidResponse,
    /// Metadata GET/LIST unexpectedly returned acceptance instead of a result.
    UnexpectedAcceptance,
    /// The status was not a documented completed metadata response.
    UnexpectedStatus,
    /// A service continuation attempted to change the bound origin or route.
    InvalidContinuation,
}

/// A typed failure retaining shared execution evidence without diagnostic echo.
pub enum KqlError {
    /// Local validation or decoding failed.
    Contract {
        /// Failure classification.
        kind: KqlErrorKind,
        /// Observed HTTP status, when decoding a response.
        status: Option<u16>,
        /// Unexpected accepted operation ID, retained for reconciliation only.
        operation_id: Option<String>,
    },
    /// Original shared authentication, transport, service or cancellation error.
    Operation(fabric_sdk_operations::OperationError),
}

impl KqlError {
    pub(super) fn new(kind: KqlErrorKind) -> Self {
        Self::Contract {
            kind,
            status: None,
            operation_id: None,
        }
    }

    /// Local classification, or None for an unchanged shared operation error.
    pub fn kind(&self) -> Option<KqlErrorKind> {
        match self {
            Self::Contract { kind, .. } => Some(*kind),
            Self::Operation(_) => None,
        }
    }
}

impl fmt::Debug for KqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract { kind, status, .. } => f
                .debug_struct("KqlError")
                .field("kind", kind)
                .field("status", status)
                .finish_non_exhaustive(),
            Self::Operation(_) => f.write_str("KqlError::Operation(<shared evidence>)"),
        }
    }
}
impl fmt::Display for KqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KQL database metadata or context binding failed; inspect typed evidence")
    }
}
impl std::error::Error for KqlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Operation(error) => Some(error),
            Self::Contract { .. } => None,
        }
    }
}
