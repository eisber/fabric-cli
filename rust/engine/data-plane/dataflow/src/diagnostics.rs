// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Advisory diagnostics from existing typed execution failures only.
//!
//! No message, M document, connection string or undocumented PQ metadata schema
//! is inspected. A documented code identifies a service-reported condition, not
//! a verified root cause or proof that a suggested check will repair execution.

use crate::{ExecuteQueryError, ExecuteQueryErrorKind};
use fabric_sdk_operations::{OperationError, OperationErrorKind};
use fabric_sdk_workspaces::FabricServiceError;
use serde::Serialize;
use serde_json::Value;

const MAX_DETAILS: usize = 8;
const MAX_CODE_BYTES: usize = 128;

/// Service-reported condition, distinct from HTTP status and remote completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiagnosticCondition {
    /// Existing evidence does not establish a documented condition.
    Unknown,
    /// Broad execution failure; does not establish a connection problem.
    QueryExecutionFailed,
    /// Service explicitly reports that the gateway cannot be reached.
    GatewayUnavailable,
    /// Gateway cannot access its source; not proof of invalid credentials.
    GatewaySourceAccess,
    /// The injected execution credential failed, independently of source auth.
    ExecutionCredentialUnavailable,
}

/// Evidence supporting the advisory, never an inferred human-message match.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticBasis {
    /// No documented code or applicable local typed evidence.
    Unclassified,
    /// An exact code documented by Microsoft was returned by the service.
    DocumentedServiceCode,
    /// The shared engine reported failure of the execution credential.
    ExecutionCredential,
}

/// Small allowlist of publicly documented codes; unknown strings are not echoed.
///
/// The Execute Query reference documents `DataflowExecuteQueryError`. Microsoft's
/// Power BI gateway troubleshooting documents the other two codes; these are
/// recognized only when actually returned, not assumed to be Dataflow codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ServiceCode {
    /// Broad query-name, mashup or evaluation failure.
    DataflowExecuteQueryError,
    /// Gateway reachability condition, not a credential diagnosis.
    GatewayNotReachable,
    /// Source access through the gateway failed.
    #[serde(rename = "DM_GWPipeline_Gateway_DataSourceAccessError")]
    GatewayDataSourceAccessError,
}

impl ServiceCode {
    /// Exact documented code, suitable for presentation without service text.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DataflowExecuteQueryError => "DataflowExecuteQueryError",
            Self::GatewayNotReachable => "GatewayNotReachable",
            Self::GatewayDataSourceAccessError => "DM_GWPipeline_Gateway_DataSourceAccessError",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        if value.len() > MAX_CODE_BYTES {
            return None;
        }
        match value {
            "DataflowExecuteQueryError" => Some(Self::DataflowExecuteQueryError),
            "GatewayNotReachable" => Some(Self::GatewayNotReachable),
            "DM_GWPipeline_Gateway_DataSourceAccessError" => {
                Some(Self::GatewayDataSourceAccessError)
            }
            _ => None,
        }
    }
}

/// Source-safe advisory DTO; raw evidence stays on the original error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    condition: DiagnosticCondition,
    basis: DiagnosticBasis,
    service_code: Option<ServiceCode>,
}

impl Diagnostic {
    /// Explicit unclassified fallback; never a successful execution result.
    pub const fn unknown() -> Self {
        Self {
            condition: DiagnosticCondition::Unknown,
            basis: DiagnosticBasis::Unclassified,
            service_code: None,
        }
    }

    /// Inspect only an existing native operation failure, without changing it.
    pub fn from_query_error(error: &ExecuteQueryError) -> Self {
        if error.kind() != ExecuteQueryErrorKind::Operation {
            return Self::unknown();
        }
        error
            .operation_error()
            .map_or_else(Self::unknown, Self::from_operation_error)
    }

    /// Classify typed local credential failure or exact structured service codes.
    ///
    /// HTTP 401/403 alone is not a source-authentication or execute-permission
    /// diagnosis. Cancellation, uncertain dispatch and malformed response errors
    /// remain unknown, even if raw evidence contains a familiar-looking message.
    pub fn from_operation_error(error: &OperationError) -> Self {
        let code = match &error.kind {
            OperationErrorKind::Credential => {
                return Self {
                    condition: DiagnosticCondition::ExecutionCredentialUnavailable,
                    basis: DiagnosticBasis::ExecutionCredential,
                    service_code: None,
                };
            }
            OperationErrorKind::Http {
                code,
                error: service_error,
                ..
            } => {
                if let Some(error) = service_error {
                    if code
                        .as_deref()
                        .is_some_and(|code| code != error.error_code.as_str())
                    {
                        return Self::unknown();
                    }
                    structured_code(error)
                } else if error.raw_service_error.is_none() {
                    code.as_deref().and_then(ServiceCode::parse)
                } else {
                    // Parsed JSON without a typed service error is not verified
                    // header-only evidence, even when its header code is known.
                    None
                }
            }
            OperationErrorKind::OperationFailed {
                status,
                error: Some(error),
            } if status == "Failed" => structured_code(error),
            _ => None,
        };
        code.map_or_else(Self::unknown, Self::service)
    }

    /// Sanitized condition; does not replace the original native error kind.
    pub const fn condition(self) -> DiagnosticCondition {
        self.condition
    }

    /// Whether a documented service code or local typed evidence supports it.
    pub const fn basis(self) -> DiagnosticBasis {
        self.basis
    }

    /// Recognized static code only; the original preserves all unknown codes.
    pub const fn service_code(self) -> Option<ServiceCode> {
        self.service_code
    }

    /// Fixed source-safe explanation, never copied from service messages.
    pub const fn summary(self) -> &'static str {
        match self.condition {
            DiagnosticCondition::Unknown => {
                "The cause is unconfirmed; existing evidence does not establish a connection, gateway, credential, permission or Arrow-version fault."
            }
            DiagnosticCondition::QueryExecutionFailed => {
                "The service reported a broad Dataflow query execution failure; this does not establish a connection fault."
            }
            DiagnosticCondition::GatewayUnavailable => {
                "The service reported that a gateway could not be reached; the underlying cause is unconfirmed."
            }
            DiagnosticCondition::GatewaySourceAccess => {
                "The service reported that the gateway could not access its data source; invalid credentials are not established."
            }
            DiagnosticCondition::ExecutionCredentialUnavailable => {
                "The configured execution credential could not be acquired; this is separate from the data source's saved credentials."
            }
        }
    }

    /// Optional manual checks, not repairs, probes or authorization to replay.
    pub const fn checks(self) -> &'static [&'static str] {
        match self.condition {
            DiagnosticCondition::GatewayUnavailable => &[
                "Ask the connection or gateway owner to check the saved connection binding and gateway availability.",
            ],
            DiagnosticCondition::GatewaySourceAccess => &[
                "Ask the connection owner to check the saved connection binding and the data source's credential configuration and access permissions.",
                "Ask the gateway owner to check source availability from the configured gateway; this code does not establish which check will resolve the failure.",
            ],
            DiagnosticCondition::ExecutionCredentialUnavailable => &[
                "Check the execution host's explicitly selected credential configuration; no identity or scope was changed.",
            ],
            DiagnosticCondition::QueryExecutionFailed | DiagnosticCondition::Unknown => &[
                "Check the requested query name and explicit mashup, if supplied, without publishing or saving changes.",
                "If the query uses a connection, ask its owner to check the saved binding, gateway availability, and the data source's credentials and permissions. These are possible checks, not a confirmed diagnosis.",
                "Separately verify Dataflow execute permission and the applicable Dataflow.Execute.All or Item.Execute.All delegated scope; these differ from data-source access.",
            ],
        }
    }

    const fn service(code: ServiceCode) -> Self {
        Self {
            condition: match code {
                ServiceCode::DataflowExecuteQueryError => DiagnosticCondition::QueryExecutionFailed,
                ServiceCode::GatewayNotReachable => DiagnosticCondition::GatewayUnavailable,
                ServiceCode::GatewayDataSourceAccessError => {
                    DiagnosticCondition::GatewaySourceAccess
                }
            },
            basis: DiagnosticBasis::DocumentedServiceCode,
            service_code: Some(code),
        }
    }
}

fn structured_code(error: &FabricServiceError) -> Option<ServiceCode> {
    let mut code = ServiceCode::parse(&error.error_code)?;
    let Some(details) = error.additional_fields.get("moreDetails") else {
        return Some(code);
    };
    let details = details.as_array()?;
    if details.len() > MAX_DETAILS {
        return None;
    }
    for detail in details {
        let detail = detail.as_object()?;
        // Only the documented immediate ErrorResponseDetails shape is read.
        if detail.contains_key("moreDetails")
            || detail
                .get("message")
                .is_some_and(|value| !value.is_string())
            || !valid_context(detail.get("parameters"), detail.get("relatedResource"))
        {
            return None;
        }
        let next = ServiceCode::parse(detail.get("errorCode")?.as_str()?)?;
        code = match (code, next) {
            (ServiceCode::DataflowExecuteQueryError, next) => next,
            (code, ServiceCode::DataflowExecuteQueryError) => code,
            (code, next) if code == next => code,
            _ => return None,
        };
    }
    Some(code)
}

fn valid_context(parameters: Option<&Value>, resource: Option<&Value>) -> bool {
    let parameters_valid = parameters.is_none_or(|value| {
        value.as_array().is_some_and(|parameters| {
            parameters.len() <= MAX_DETAILS
                && parameters.iter().all(|value| {
                    value.as_object().is_some_and(|parameter| {
                        parameter.get("name").is_some_and(Value::is_string)
                            && parameter.get("value").is_some_and(Value::is_string)
                            && parameter.get("message").is_none_or(Value::is_string)
                    })
                })
        })
    });
    let resource_valid = resource.is_none_or(|value| {
        value.as_object().is_some_and(|resource| {
            resource.get("resourceId").is_some_and(Value::is_string)
                && resource.get("resourceType").is_some_and(Value::is_string)
        })
    });
    parameters_valid && resource_valid
}
