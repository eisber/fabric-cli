// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in, one-shot Notebook run cancellation with caller-owned SDK and streams.
//!
//! JSON schema 0.1 reports acceptance, never terminal cancellation or rollback.
//! Only fixed allowlisted error identifiers and flags are projected. Unknown
//! provider fields, including property names, never enter diagnostics. The SDK
//! retains the original structured error for callers needing privileged access.
//!
//! Response bodies, `Location` and `Retry-After` never initiate status queries,
//! operation reconciliation or waits. Only the explicitly selected endpoint is
//! recorded. The caller's cancellation future reaches the one-shot SDK request;
//! a local interruption is not remote cancellation evidence. SDK mutation
//! uncertainty is preserved unchanged: acceptance records `requested: true`,
//! uncertain dispatch records `requested: null`, and known pre-dispatch failure
//! records `requested: false`. All three retain `confirmed: false`.
//! Only HTTP 202 is accepted. A missing `Location` is permitted because the run
//! is already known; a supplied `Location` must pass the SDK's exact endpoint
//! and scoped-instance validation before any accepted receipt is rendered.
//! Failed validation still retains the actual response status and allowlisted
//! request/correlation IDs. Other response headers and response bodies are not
//! dumped into diagnostics.
//! `responseBodyOmitted` reports SDK evidence truncation: null without a complete
//! response, false for a retained bounded response, and true when only response
//! metadata survived the SDK's body bound. An empty retained body is never used
//! to infer completeness.

use crate::parser::CancelCommand;
use fabric_sdk_jobs::{
    EndpointBinding, JobClient, JobError, JobExecutionError,
    cancellation::{self, CancellationError, NotebookRun},
};
use serde_json::{Value, json};
use std::{future::Future, io::Write};

/// A pre-I/O validation failure, containing no supplied argument values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InputError {
    /// Only an absolute typed path with three strict GUIDs is accepted.
    #[error("Cancel requires one absolute ID-only typed Notebook run path.")]
    InvalidInput,
    /// Other artifacts and facets have no cancellation implementation.
    #[error("Cancel supports only the Notebook runs facet.")]
    UnsupportedCapability,
    /// The new command does not inherit legacy presentation.
    #[error("Cancel supports only --format json; --output_format is not supported.")]
    ExplicitFormatNotSupported,
}

impl InputError {
    /// Stable machine-readable classification.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "InvalidInput",
            Self::UnsupportedCapability => "UnsupportedCapability",
            Self::ExplicitFormatNotSupported => "explicitFormatNotSupported",
        }
    }
}

/// Validate without credentials, metadata, current-directory resolution or I/O.
///
/// # Errors
///
/// Rejects malformed IDs/paths, non-Notebook facets and explicit non-v2 formats.
pub fn preflight(command: &CancelCommand) -> Result<NotebookRun, InputError> {
    if command.output_format.is_some()
        || command
            .format
            .as_deref()
            .is_some_and(|format| format != "json")
    {
        return Err(InputError::ExplicitFormatNotSupported);
    }
    let segments: Vec<_> = command.path.split('/').collect();
    let ["", workspace, item, facet, instance] = segments.as_slice() else {
        return Err(InputError::InvalidInput);
    };
    let (workspace, workspace_type) = workspace.rsplit_once('.').ok_or(InputError::InvalidInput)?;
    let (item, item_type) = item.rsplit_once('.').ok_or(InputError::InvalidInput)?;
    let target =
        NotebookRun::new(workspace, item, instance).map_err(|_| InputError::InvalidInput)?;
    if workspace_type != "Workspace" || item_type != "Notebook" || *facet != "runs" {
        return Err(InputError::UnsupportedCapability);
    }
    Ok(target)
}

fn handle(target: &NotebookRun) -> Value {
    json!({
        "kind": "itemJobRun",
        "workspaceId": target.workspace_id(),
        "itemId": target.item_id(),
        "instanceId": target.instance_id(),
    })
}

fn target_meta(target: &NotebookRun, endpoint: Option<&EndpointBinding>) -> Value {
    json!({
        "workspaceId": target.workspace_id(),
        "itemId": target.item_id(),
        "instanceId": target.instance_id(),
        "path": format!("/{}.Workspace/{}.Notebook/runs/{}",
            target.workspace_id(), target.item_id(), target.instance_id()),
        "endpoint": endpoint.map(|endpoint| endpoint.base_url().to_string()),
    })
}

/// A retained, single-delivery receipt, including failures and known target IDs.
///
/// Never replay a request after writing this value fails.
#[derive(Debug)]
pub struct Dispatch {
    value: Value,
    exit: u8,
}

impl Dispatch {
    /// Inspect the stable JSON envelope without losing the target or uncertainty.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Exit status: accepted 0, service/host/output failure 1, invalid input 2.
    pub const fn exit_code(&self) -> u8 {
        self.exit
    }

    /// Represent a pre-dispatch host failure without fabricating mutation evidence.
    pub fn unavailable(
        target: Option<&NotebookRun>,
        endpoint: Option<&EndpointBinding>,
        code: &str,
        message: &str,
        exit: u8,
    ) -> Self {
        Self {
            value: envelope(
                target,
                endpoint,
                false,
                false,
                json!({
                    "code": code, "message": message,
                    "source": null, "correlation": {}, "serviceError": null,
                }),
            ),
            exit,
        }
    }

    /// Retain the native interruption reason without overwriting SDK evidence.
    pub fn interrupted(mut self, code: &str) -> Self {
        if self.exit != 0 {
            self.value["error"]["interruption"] = json!(code);
        }
        self
    }

    /// Deliver JSON once to the proper stream, then flush.
    ///
    /// On output failure, best-effort stderr retains the original target,
    /// acceptance and mutation evidence. No request is retried and exit is 1.
    pub fn write_to(&self, stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
        let written = if self.exit == 0 {
            write_json(stdout, &self.value)
        } else {
            write_json(stderr, &self.value)
        };
        if written.is_ok() {
            return self.exit;
        }
        let mut fallback = self.value.clone();
        fallback["status"] = json!("error");
        fallback["error"] = json!({
            "code": "OutputFailure",
            "message": "Receipt delivery failed. Retain the target handle; do not replay cancellation.",
            "source": self.value["error"],
        });
        let _ = serde_json::to_writer(&mut *stderr, &fallback);
        let _ = writeln!(stderr);
        let _ = stderr.flush();
        1
    }
}

fn write_json(writer: &mut dyn Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(std::io::Error::other)?;
    writeln!(writer)?;
    writer.flush()
}

fn envelope(
    target: Option<&NotebookRun>,
    endpoint: Option<&EndpointBinding>,
    accepted: bool,
    mutation_may_have_occurred: bool,
    error: Value,
) -> Value {
    json!({
        "schemaVersion": "0.1",
        "status": if accepted { "accepted" } else { "error" },
        "data": target.map(handle),
        "meta": {
            "target": target.map(|target| target_meta(target, endpoint)),
            "cancellation": {
                "requested": if accepted { json!(true) }
                    else if mutation_may_have_occurred { Value::Null } else { json!(false) },
                "confirmed": false,
            },
            "mutationMayHaveOccurred": mutation_may_have_occurred,
            "mutation": {
                "accepted": accepted,
                "responseStatus": null,
                "responseBodyOmitted": null,
            },
            "effects": {"completeness": "unknown"},
        },
        "error": error,
    })
}

/// Preflight and execute one SDK cancellation, without waits or metadata queries.
///
/// The caller supplies its existing runtime, credential/endpoint-bound client
/// and cooperative cancellation signal. Dropping local work never means rollback.
pub async fn dispatch(
    command: &CancelCommand,
    client: &JobClient,
    cancelled: impl Future<Output = ()> + Send,
) -> Dispatch {
    let target = match preflight(command) {
        Ok(target) => target,
        Err(error) => {
            return Dispatch::unavailable(None, None, error.code(), &error.to_string(), 2);
        }
    };
    match cancellation::request(client, &target, cancelled).await {
        Ok(receipt) => {
            let mut value = envelope(
                Some(receipt.target()),
                Some(receipt.endpoint()),
                true,
                true,
                Value::Null,
            );
            value["meta"]["mutation"]["responseStatus"] = json!(receipt.response().status);
            value["meta"]["mutation"]["responseBodyOmitted"] = json!(false);
            value["meta"]["correlation"] = correlation(&receipt.response().headers);
            Dispatch { value, exit: 0 }
        }
        Err(error) => sdk_failure(&error),
    }
}

fn sdk_failure(error: &CancellationError) -> Dispatch {
    let (code, projection, headers, status) = match &error.source {
        JobError::Http {
            status,
            code,
            service_error,
            raw_service_error,
            headers,
        } => (
            "ServiceError",
            raw_service_error
                .as_deref()
                .map(service_data)
                .or_else(|| {
                    service_error.as_deref().map(|error| {
                        let mut fields = service_fields(|key| error.additional_fields.get(key));
                        fields.insert("errorCode".into(), safe_identifier(&error.error_code));
                        Value::Object(fields)
                    })
                })
                .unwrap_or(Value::Null),
            correlation(headers),
            json!({"httpStatus":status,"serviceCode":safe_identifier(code)}),
        ),
        JobError::UnexpectedStatus(status) => (
            "UnexpectedStatus",
            Value::Null,
            json!({}),
            json!({"httpStatus":status}),
        ),
        source => (source_code(source), Value::Null, json!({}), Value::Null),
    };
    let mut value = envelope(
        Some(&error.target),
        Some(&error.endpoint),
        false,
        error.mutation_may_have_occurred,
        json!({
            "code": code,
            "message": "Cancellation was not confirmed. Retain the target handle; do not automatically replay.",
            "source": {"classification":source_code(&error.source), "details":status},
            "correlation": headers,
            "serviceError": projection,
        }),
    );
    if let JobError::Http { status, .. } | JobError::UnexpectedStatus(status) = &error.source {
        value["meta"]["mutation"]["responseStatus"] = json!(status);
    }
    if let Some(response) = &error.response {
        value["meta"]["mutation"]["responseStatus"] = json!(response.status);
        value["meta"]["mutation"]["responseBodyOmitted"] = json!(error.response_body_omitted);
        value["error"]["correlation"] = correlation(&response.headers);
        if value["error"]["source"]["details"].is_null() {
            value["error"]["source"]["details"] = json!({"httpStatus": response.status});
        }
    }
    Dispatch { value, exit: 1 }
}

fn source_code(error: &JobError) -> &'static str {
    match error {
        JobError::Credential => "Credential",
        JobError::Cancelled => "Cancelled",
        JobError::Execution(JobExecutionError::TimedOut) | JobError::TimedOut { .. } => "TimedOut",
        JobError::Execution(JobExecutionError::Cancelled) => "Cancelled",
        JobError::Execution(JobExecutionError::ResponseLimit) => "ResponseLimit",
        JobError::Execution(JobExecutionError::RetriesExhausted) => "RetriesExhausted",
        JobError::Execution(_) => "Execution",
        JobError::Http { .. } => "Http",
        JobError::InvalidAudience => "InvalidAudience",
        JobError::InvalidEndpoint => "InvalidEndpoint",
        JobError::InvalidIdentifier => "InvalidIdentifier",
        JobError::InvalidRequestHeaders => "InvalidRequestHeaders",
        JobError::RequestHeadersUnsupported => "RequestHeadersUnsupported",
        JobError::UnexpectedStatus(_) => "UnexpectedStatus",
        JobError::InvalidResponse => "InvalidResponse",
        _ => "JobError",
    }
}

fn safe_identifier(value: &str) -> Value {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    {
        json!(value)
    } else {
        json!("[redacted]")
    }
}

fn correlation(headers: &std::collections::BTreeMap<String, String>) -> Value {
    Value::Object(
        headers
            .iter()
            .filter_map(|(key, value)| {
                let key = key.to_ascii_lowercase();
                matches!(
                    key.as_str(),
                    "request-id"
                        | "x-ms-request-id"
                        | "x-ms-correlation-request-id"
                        | "x-ms-correlation-id"
                        | "activity-id"
                        | "x-ms-activity-id"
                )
                .then(|| (key, safe_identifier(value)))
            })
            .collect(),
    )
}

fn service_fields<'a>(get: impl Fn(&str) -> Option<&'a Value>) -> serde_json::Map<String, Value> {
    let mut fields = serde_json::Map::new();
    for key in ["code", "errorCode", "requestId", "correlationId"] {
        if let Some(value) = get(key).and_then(Value::as_str) {
            fields.insert(key.into(), safe_identifier(value));
        }
    }
    if let Some(value) = get("isRetriable").and_then(Value::as_bool) {
        fields.insert("isRetriable".into(), json!(value));
    }
    fields
}

fn service_data(value: &Value) -> Value {
    if !value.is_object() {
        return Value::Null;
    }
    let mut fields = service_fields(|key| value.get(key));
    // Exactly one known wrapper is supported; never traverse arbitrary data.
    if let Some(error) = value.get("error").filter(|error| error.is_object()) {
        fields.insert(
            "error".into(),
            Value::Object(service_fields(|key| error.get(key))),
        );
    }
    Value::Object(fields)
}
