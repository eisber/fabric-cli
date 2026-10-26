// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Cross-crate public consumer of the shared pure CLI error projection seam.

use fabric_cli_mutations::operation_error_output;
use fabric_cli_output::{OutputEnvelope, Timestamp};
use fabric_sdk_operations::{OperationError, OperationErrorKind};
use serde_json::{Value, json};

fn failure(detail: Value, cancelled: bool) -> OperationError {
    OperationError {
        kind: OperationErrorKind::OperationFailed {
            status: if cancelled { "Cancelled" } else { "Failed" }.into(),
            error: None,
        },
        operation_id: Some("22222222-2222-2222-2222-222222222222".into()),
        mutation_may_have_occurred: true,
        raw_service_error: Some(Box::new(detail)),
    }
}

#[test]
fn public_projection_preserves_dict_list_none_string_unicode_and_original_sdk_evidence() {
    for (detail, repr) in [
        (
            json!({"z":null,"a":[true,false,"\u{65e5}",1.0]}),
            "{'z': None, 'a': [True, False, '\u{65e5}', 1.0]}",
        ),
        (json!([1, "x'y", null]), "[1, \"x'y\", None]"),
        (Value::Null, "None"),
        (json!("a plain string"), "a plain string"),
    ] {
        for cancelled in [false, true] {
            let error = failure(detail.clone(), cancelled);
            let original = error.clone();
            let output = OutputEnvelope::new(
                Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
                operation_error_output(&error),
            )
            .to_value()
            .unwrap();
            assert_eq!(
                output["result"]["error_code"],
                if cancelled {
                    "LongRunningOperationCancelled"
                } else {
                    "LongRunningOperationFailed"
                }
            );
            let prefix = if cancelled {
                "The operation was cancelled: "
            } else {
                "The operation failed: "
            };
            assert_eq!(output["result"]["message"], format!("{prefix}{repr}"));
            assert_eq!(error, original);
            assert_eq!(error.resource_error().operation_id, original.operation_id);
            assert!(error.resource_error().mutation_may_have_occurred);
            assert_eq!(
                error.resource_error().raw_service_error,
                original.raw_service_error
            );
        }
    }
}

#[test]
fn public_projection_does_not_fabricate_http_status_for_generic_service_failure() {
    let error = failure(json!({"message":"untrusted service text"}), false);
    assert_eq!(error.resource_error().status, None);
    assert!(!error.to_string().contains("untrusted service text"));
    assert!(
        !error
            .resource_error()
            .to_string()
            .contains("untrusted service text")
    );
    let result = OutputEnvelope::new(
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        operation_error_output(&error),
    );
    assert_eq!(
        result.result().message(),
        Some("The operation failed: {'message': 'untrusted service text'}")
    );
}

#[test]
fn public_projection_does_not_normalize_the_source_period_only_message_twice() {
    let error = OperationError {
        kind: OperationErrorKind::Http {
            status: 400,
            code: None,
            error: None,
        },
        operation_id: None,
        mutation_may_have_occurred: true,
        raw_service_error: Some(Box::new(json!({"message":"..."}))),
    };
    let output = operation_error_output(&error);
    assert_eq!(output.message.as_deref(), Some(""));
    assert_eq!(output.error_code.as_deref(), Some("UnknownError"));
}
