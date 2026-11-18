// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-safe presentation of an existing execution failure, without any I/O.

use super::{CommandOutcome, Delivery, ExecutionEvidence, ExecutionFailure};
use fabric_sdk_dataflow::diagnostics::Diagnostic;
use fabric_sdk_operations::OperationErrorKind;
use fabric_sdk_tabular::TabularError;
use serde_json::{Value, json};

pub(super) fn failure(error: &ExecutionFailure) -> Delivery {
    let (code, message, evidence, exit, guidance) = match error {
        ExecutionFailure::Query(error) => {
            let operation = error.operation_error();
            let status = operation.and_then(|error| match &error.kind {
                OperationErrorKind::Http { status, .. } => Some(*status),
                _ => None,
            });
            let (code, exit) = match status {
                Some(401) => ("Unauthorized", 4),
                Some(403) => ("Forbidden", 1),
                _ => ("execution_failed", 1),
            };
            let message = status.map_or_else(
                || format!(
                    "Native Dataflow query failed or is incomplete. Classification: {:?}. No query was replayed.",
                    error.kind()
                ),
                |status| format!(
                    "Native Dataflow query was rejected with HTTP {status}. No query was replayed."
                ),
            );
            (
                code,
                message,
                operation.map(ExecutionEvidence::from),
                exit,
                Diagnostic::from_query_error(error),
            )
        }
        ExecutionFailure::Result { error, evidence, .. } => (
            "execution_failed",
            if matches!(error, TabularError::Metadata { .. }) {
                "The Dataflow result contains unclassified native diagnostic metadata. HTTP success does not establish query success. No result was published and no query was replayed."
            } else {
                "The complete Dataflow result could not be decoded or serialized. No result was published and no query was replayed."
            }
            .to_owned(),
            Some(ExecutionEvidence::from(evidence.as_ref())),
            1,
            // The public REST contract names the column, not its value schema.
            // Do not parse JSON-looking PQ strings into service error objects.
            Diagnostic::unknown(),
        ),
    };
    let mut delivery = Delivery::failed(code, &message, evidence, exit);
    if let ExecutionFailure::Result { error, .. } = error {
        delivery = delivery.retain_tabular_evidence(error);
        delivery.outcome = CommandOutcome::Stop(delivery.plan.exit_code);
    }
    let record = &mut delivery.plan.records[0];
    let mut payload: Value =
        serde_json::from_slice(&record.payload).expect("owned execution diagnostic");
    payload["guidance"] = json!({
        "classification": guidance,
        "summary": guidance.summary(),
        "checks": guidance.checks(),
        "automaticActionsTaken": false,
        "queryReplayed": false,
    });
    record.payload = serde_json::to_vec(&payload).expect("static advisory values serialize");
    record.payload.push(b'\n');
    delivery
}

pub(super) fn metadata_summary(error: &TabularError) -> Option<Value> {
    let TabularError::Metadata { evidence } = error else {
        return None;
    };
    Some(json!({
        "count": evidence.len(),
        "redacted": true,
        "entries": evidence.iter().take(8).map(|entry| json!({
            "location": format!("{:?}", entry.location),
            "classification": format!("{:?}", entry.classification),
            "key": if entry.key == "PQ Arrow Metadata" {
                "PQ Arrow Metadata"
            } else {
                "<redacted>"
            },
            "value": "<redacted>",
        })).collect::<Vec<_>>(),
    }))
}
