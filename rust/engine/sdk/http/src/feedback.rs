// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fixed-endpoint OCV feedback submission.
//!
//! The caller acquires an opaque access token for exactly [`Environment::scope`]
//! in the selected environment. This adapter does not inspect token claims,
//! acquire credentials, or extend the general service-origin bindings.

use crate::{ExecutionError, HttpExecutor};
use azure_core::{
    credentials::AccessToken,
    http::{Method, Url},
};
use fabric_sdk::workspaces::WorkspaceResponse;
use std::num::NonZeroUsize;

const MAX_FEEDBACK_BYTES: usize = 64 * 1024;

/// Closed set of supported OCV endpoint and delegated-scope pairs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Environment {
    /// Production OCV feedback service.
    Production,
    /// Pre-production OCV feedback service.
    Ppe,
}

impl Environment {
    /// Returns the fixed authenticated-feedback HTTPS endpoint.
    pub const fn endpoint(self) -> &'static str {
        match self {
            Self::Production => "https://petrol.office.microsoft.com/v1/feedback/authenticated",
            Self::Ppe => "https://petrol-int.office.microsoft.com/v1/feedback/authenticated",
        }
    }

    /// Returns the exact delegated scope required for this environment.
    ///
    /// The caller must acquire its token for this scope. Tokens are opaque to
    /// the executor, so it cannot verify their audience or permissions.
    pub const fn scope(self) -> &'static str {
        match self {
            Self::Production => "https://petrol.office.microsoft.com/Feedback.ReadWrite",
            Self::Ppe => "https://petrol-int.office.microsoft.com/Feedback.ReadWrite",
        }
    }
}

impl HttpExecutor {
    /// Submits one authenticated JSON POST to the fixed OCV feedback endpoint.
    ///
    /// The caller must supply a token acquired for exactly
    /// [`Environment::scope`] in the selected environment; opaque token claims
    /// cannot be verified here. No credential lookup or refresh is performed.
    /// The caller serializes the JSON body, which must contain 1 through 65,536
    /// bytes. The response is also capped at 65,536 decoded bytes.
    ///
    /// This adapter accepts no URL or header overrides and does not use or
    /// widen [`Self::with_endpoint`] bindings. It delegates to the existing
    /// execution engine with the caller's timer and overall deadline. The POST
    /// is never replayed, even on throttling, transport failure or timeout.
    /// Raw status, headers and body are returned using the shared response
    /// shape; acceptance does not imply completed service work. Dropping the
    /// future cancels local work, not a possibly dispatched submission.
    ///
    /// # Errors
    /// Returns [`ExecutionError::InvalidInput`] for an empty/oversized body or
    /// an empty token or one containing control characters, before transport
    /// I/O. Transport/stream failures, response bounds and deadlines return the
    /// corresponding sanitized [`ExecutionError`] without replay.
    pub async fn execute_feedback(
        &self,
        environment: Environment,
        token: &AccessToken,
        body: &[u8],
    ) -> Result<WorkspaceResponse, ExecutionError> {
        if body.is_empty() || body.len() > MAX_FEEDBACK_BYTES {
            return Err(ExecutionError::InvalidInput);
        }
        Self::validate_token(token)?;
        let url = Url::parse(environment.endpoint()).map_err(|_| ExecutionError::InvalidInput)?;
        let limit = NonZeroUsize::new(MAX_FEEDBACK_BYTES).ok_or(ExecutionError::InvalidInput)?;
        self.execute_request(
            Self::authenticated_request(Method::Post, &url, token, body),
            limit,
        )
        .await
    }
}
