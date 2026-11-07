// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::RelevanceError;
use fabric_sdk_api::{ApiError, header};
use fabric_sdk_workspaces::workspaces::WorkspaceResponse;
use serde::Deserialize;
use serde_json::{Map, Value};

/// Source-backed Power BI service error, not the Catalog flat error contract.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ServiceError {
    /// Exact service error code.
    pub code: String,
    /// Optional untrusted service message; never an instruction.
    pub message: Option<String>,
    /// Optional service error target.
    pub target: Option<String>,
    /// Optional structured validation details.
    pub details: Option<Vec<ErrorDetail>>,
    /// Optional inner error metadata, not a local diagnostic.
    pub innererror: Option<InnerError>,
    /// Optional Power BI-specific error metadata, retained without interpretation.
    #[serde(rename = "pbi.error")]
    pub power_bi_error: Option<Map<String, Value>>,
    /// Forward-compatible optional error properties.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// One optional validation-error detail.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ErrorDetail {
    /// Optional service detail code.
    pub code: Option<String>,
    /// Optional untrusted service detail message.
    pub message: Option<String>,
    /// Optional service detail target.
    pub target: Option<String>,
    /// Forward-compatible optional properties.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// Optional service-side inner error data, not a local stack trace.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct InnerError {
    /// Optional untrusted service trace.
    pub trace: Option<String>,
    /// Optional untrusted service context.
    pub context: Option<String>,
    /// Forward-compatible optional metadata.
    #[serde(flatten)]
    pub additional: Map<String, Value>,
}

/// A failing HTTP response with preserved structured service metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceFailure {
    /// Actual status; never replaced by a body or header value.
    pub status: u16,
    /// Known nested error, or legacy-compatible top-level `code` error.
    /// May be absent for a bodyless gateway/model-validation failure.
    pub error: Option<ServiceError>,
    /// Source header error code, if supplied; does not overwrite the body.
    pub header_code: Option<String>,
    /// Opaque server Retry-After header; no retry is performed by this SDK.
    pub retry_after: Option<String>,
    /// Unrecognized optional envelope metadata.
    pub additional: Map<String, Value>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: Option<ServiceError>,
    code: Option<String>,
    message: Option<String>,
    target: Option<String>,
    details: Option<Vec<ErrorDetail>>,
    innererror: Option<InnerError>,
    #[serde(rename = "pbi.error")]
    power_bi_error: Option<Map<String, Value>>,
    #[serde(flatten)]
    additional: Map<String, Value>,
}

pub(crate) fn require_success(response: &WorkspaceResponse) -> Result<(), RelevanceError> {
    if response.status == 200 {
        return Ok(());
    }
    if (200..300).contains(&response.status) {
        return Err(ApiError::UnsupportedStatus(response.status).into());
    }
    let invalid = || RelevanceError::InvalidErrorResponse {
        status: response.status,
    };
    let (error, additional) = if response.body.is_empty() {
        (None, Map::new())
    } else {
        let envelope: ErrorEnvelope =
            serde_json::from_slice(&response.body).map_err(|_| invalid())?;
        let error = match envelope.error {
            Some(error) => error,
            None => ServiceError {
                code: envelope.code.ok_or_else(invalid)?,
                message: envelope.message,
                target: envelope.target,
                details: envelope.details,
                innererror: envelope.innererror,
                power_bi_error: envelope.power_bi_error,
                additional: Map::new(),
            },
        };
        if error.code.trim().is_empty() {
            return Err(invalid());
        }
        (Some(error), envelope.additional)
    };
    Err(RelevanceError::Service(Box::new(ServiceFailure {
        status: response.status,
        error,
        header_code: header(response, "x-ms-public-api-error-code")
            .or_else(|| header(response, "x-ms-error-code"))
            .map(str::to_owned),
        retry_after: header(response, "retry-after").map(str::to_owned),
        additional,
    })))
}
