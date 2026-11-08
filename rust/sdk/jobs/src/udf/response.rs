// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{UdfJsonLimits, UdfValidationError, json};
use serde_json::Value;
use std::fmt;

/// Why received data cannot establish a complete synchronous native outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UdfProtocolIssue {
    /// The observed HTTP status is outside its protocol range.
    InvalidHttpStatus,
    /// The native JSON could not be decoded without ambiguity or type loss.
    InvalidJson(UdfValidationError),
    /// The response body exceeded a caller-selected bound and was not retained.
    ResponseLimit,
    /// The transport did not establish complete body delivery, even if JSON parses.
    IncompleteBody,
    /// The native body is not an object.
    NotEnvelope,
    /// The response names a different function, or has no string function name.
    FunctionMismatch,
    /// The correlation ID is missing, empty or not a string.
    MissingCorrelation,
    /// The native status is missing or not a nonempty string.
    MissingStatus,
    /// The errors field is missing or not an array.
    InvalidErrors,
    /// The output member is missing; present JSON null is not missing.
    MissingOutput,
    /// Succeeded contradicts a non-200 HTTP status or nonempty native errors.
    ContradictorySuccess,
    /// The native status is not one of the documented terminal statuses.
    UnknownStatus,
    /// HTTP 202 does not establish a supported asynchronous UDF protocol.
    UnsupportedAsync,
    /// An unexpected successful/redirect/informational HTTP status was received.
    UnexpectedHttp,
}

/// Function-specific interpretation, not a scheduler or global run receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UdfResponseKind {
    /// HTTP 200 and the complete matching native Succeeded envelope agree.
    Succeeded,
    /// A complete matching envelope reports a documented native failure.
    NativeFailure,
    /// An actual non-success HTTP status was received.
    HttpFailure,
    /// Available observations do not establish the synchronous native protocol.
    ProtocolFailure(UdfProtocolIssue),
}

/// Bounded observed body and native fields, independent of local delivery.
///
/// Correlation is only log-correlation evidence. This object cannot be polled,
/// cancelled or resumed. Even a failure may follow external write effects.
/// A host must retain this evidence separately if later output delivery fails.
pub struct UdfResponse {
    status: u16,
    body: Option<Box<[u8]>>,
    native: Option<Value>,
    kind: UdfResponseKind,
}

impl UdfResponse {
    /// Interpret one actual response, without retries or successful text fallback.
    ///
    /// The host must enforce these budgets while streaming, not prebuffer an
    /// unlimited body. This second check protects consumers from faulty injected
    /// transports. An oversized body is omitted entirely, never clipped into JSON.
    /// Native HTTP status is retained even when the body cannot be interpreted.
    pub fn from_received(
        function_name: &str,
        status: u16,
        body: &[u8],
        limits: UdfJsonLimits,
    ) -> Self {
        if body.len() > limits.max_bytes.get() {
            return Self {
                status,
                body: None,
                native: None,
                kind: UdfResponseKind::ProtocolFailure(UdfProtocolIssue::ResponseLimit),
            };
        }
        let parsed = json::parse(body, limits);
        let kind = match &parsed {
            Ok(native) => classify(function_name, status, native),
            Err(error) => UdfResponseKind::ProtocolFailure(UdfProtocolIssue::InvalidJson(*error)),
        };
        Self {
            status,
            body: Some(body.into()),
            native: parsed.ok(),
            kind,
        }
    }

    /// Retain a partial response without promoting parseable bytes to success.
    ///
    /// Use this after a stream/read/delivery failure before verified HTTP EOF.
    /// A prefix can be a valid native JSON envelope while transport completeness
    /// is still unknown. Bounded parseable native fields remain inspectable, but
    /// neither their status nor output overrides the incomplete observation.
    pub fn from_partial(
        function_name: &str,
        status: u16,
        body: &[u8],
        limits: UdfJsonLimits,
    ) -> Self {
        let mut response = Self::from_received(function_name, status, body, limits);
        if response.body.is_some() {
            response.kind = UdfResponseKind::ProtocolFailure(UdfProtocolIssue::IncompleteBody);
        }
        response
    }

    /// Actual received HTTP status; not a permission or completion classifier.
    pub fn http_status(&self) -> u16 {
        self.status
    }

    /// Complete retained bytes, or none when the byte budget was exceeded.
    pub fn body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }

    /// Parsed native JSON, including extra fields and contradictory evidence.
    pub fn native(&self) -> Option<&Value> {
        self.native.as_ref()
    }

    /// Explicit synchronous classification without an accepted-success variant.
    pub fn kind(&self) -> UdfResponseKind {
        self.kind
    }

    /// True only for the complete, matching HTTP 200 native Succeeded envelope.
    pub fn succeeded(&self) -> bool {
        self.kind == UdfResponseKind::Succeeded
    }

    /// Observed status string, including unknown or contradictory native states.
    pub fn native_status(&self) -> Option<&str> {
        self.native.as_ref()?.get("status")?.as_str()
    }

    /// Observed invocation correlation; never a monitor/cancel/run handle.
    pub fn invocation_id(&self) -> Option<&str> {
        self.native.as_ref()?.get("invocationId")?.as_str()
    }

    /// Observed output, preserving false, zero, null, arrays and strings.
    ///
    /// Output alone never establishes success. `None` means the member is absent
    /// or no native object was decoded; `Some(Value::Null)` is a present output.
    pub fn output(&self) -> Option<&Value> {
        self.native.as_ref()?.get("output")
    }

    /// Observed native errors, even when their shape invalidates the envelope.
    pub fn errors(&self) -> Option<&Value> {
        self.native.as_ref()?.get("errors")
    }
}

impl fmt::Debug for UdfResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UdfResponse")
            .field("http_status", &self.status)
            .field("kind", &self.kind)
            .field("body", &"<redacted>")
            .finish()
    }
}

fn classify(function_name: &str, status: u16, native: &Value) -> UdfResponseKind {
    let issue = if !(100..=599).contains(&status) {
        UdfProtocolIssue::InvalidHttpStatus
    } else if status == 202 {
        UdfProtocolIssue::UnsupportedAsync
    } else if !native.is_object() {
        UdfProtocolIssue::NotEnvelope
    } else if function_name.is_empty()
        || native.get("functionName").and_then(Value::as_str) != Some(function_name)
    {
        UdfProtocolIssue::FunctionMismatch
    } else if !native
        .get("invocationId")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        UdfProtocolIssue::MissingCorrelation
    } else if !native
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| !status.is_empty())
    {
        UdfProtocolIssue::MissingStatus
    } else if !native.get("errors").is_some_and(Value::is_array) {
        UdfProtocolIssue::InvalidErrors
    } else if native.get("output").is_none() {
        UdfProtocolIssue::MissingOutput
    } else {
        match native.get("status").and_then(Value::as_str) {
            Some("Succeeded")
                if status == 200
                    && native
                        .get("errors")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty) =>
            {
                return UdfResponseKind::Succeeded;
            }
            Some("Succeeded") => UdfProtocolIssue::ContradictorySuccess,
            Some("Failed" | "BadRequest" | "Timeout" | "ResponseTooLarge") => {
                if status == 200 || (400..=599).contains(&status) {
                    return UdfResponseKind::NativeFailure;
                }
                UdfProtocolIssue::UnexpectedHttp
            }
            _ => UdfProtocolIssue::UnknownStatus,
        }
    };
    if (400..=599).contains(&status) && !matches!(issue, UdfProtocolIssue::ContradictorySuccess) {
        UdfResponseKind::HttpFailure
    } else {
        UdfResponseKind::ProtocolFailure(issue)
    }
}
