// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_sdk_workspaces::mutations::ResourceError;
use std::fmt;

/// Local input failures, before any operation or credential dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlInputError {
    /// An identifier is not a native 8-4-4-4-12 hexadecimal GUID.
    InvalidGuid,
    /// The caller selected an item type other than exactly SQLDatabase.
    WrongItemType,
    /// A continuation token is empty or contains control characters.
    InvalidContinuationToken,
    /// A fixed resource descriptor could not be constructed.
    InvalidDescriptor,
}

impl fmt::Display for SqlInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidGuid => "SQLDatabase metadata requires native GUID identifiers",
            Self::WrongItemType => "SQLDatabase metadata requires the SQLDatabase item type",
            Self::InvalidContinuationToken => "invalid SQLDatabase continuation token",
            Self::InvalidDescriptor => "invalid SQLDatabase metadata descriptor",
        })
    }
}

impl std::error::Error for SqlInputError {}

/// Typed response failures without supplied data in implicit diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlResponseErrorKind {
    /// Only the documented completed HTTP 200 response is supported.
    UnexpectedStatus,
    /// Acceptance is not a completed metadata response.
    UnexpectedAcceptance,
    /// A completed response did not contain the required JSON body.
    MissingBody,
    /// Required fields, known field types or the SQLDatabase discriminator failed.
    InvalidShape,
    /// A response changed the requested workspace or item identity.
    IdentityMismatch,
}

/// Response rejection retaining available status and acceptance evidence.
#[derive(Clone, PartialEq)]
pub struct SqlResponseError {
    /// Sanitized response classification.
    pub kind: SqlResponseErrorKind,
    /// Actual status when supplied by the shared execution boundary.
    pub http_status: u16,
    /// Actual accepted handle, if the injected boundary supplied one.
    pub operation_id: Option<String>,
}

impl fmt::Debug for SqlResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlResponseError")
            .field("kind", &self.kind)
            .field("http_status", &self.http_status)
            .field("has_operation_id", &self.operation_id.is_some())
            .finish()
    }
}

impl fmt::Display for SqlResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SQLDatabase metadata response is invalid or incomplete")
    }
}

impl std::error::Error for SqlResponseError {}

/// Shared execution or typed metadata response failure.
///
/// The original shared error remains inspectable as data. Debug and Display do
/// not format its potentially sensitive service/provider contents.
#[derive(Clone, PartialEq)]
pub enum SqlManagementError {
    /// Original execution, cancellation, credential or service evidence.
    Operation(ResourceError),
    /// A response could not satisfy the selected metadata contract.
    Response(SqlResponseError),
}

impl fmt::Debug for SqlManagementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Operation(error) => f
                .debug_struct("SqlManagementError::Operation")
                .field("status", &error.status)
                .field("has_operation_id", &error.operation_id.is_some())
                .field(
                    "mutation_may_have_occurred",
                    &error.mutation_may_have_occurred,
                )
                .finish(),
            Self::Response(error) => fmt::Debug::fmt(error, f),
        }
    }
}

impl fmt::Display for SqlManagementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SQLDatabase metadata failed or is incomplete; inspect its typed evidence")
    }
}

impl std::error::Error for SqlManagementError {}
