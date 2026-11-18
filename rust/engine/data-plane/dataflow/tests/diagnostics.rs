// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline evidence-only diagnostic contracts; no service or credential probes.
#![deny(unnameable_test_items)]

use fabric_sdk_dataflow::diagnostics::{Diagnostic, DiagnosticBasis, DiagnosticCondition, ServiceCode};
use fabric_sdk_operations::{OperationError, OperationErrorKind};
use serde_json::{Value, json};

const PRIVATE: &str = "synthetic-password-and-connection-string";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";

fn http(code: Option<&str>, body: Option<Value>) -> OperationError {
    OperationError {
        kind: OperationErrorKind::Http {
            status: 403,
            code: code.map(str::to_owned),
            error: body
                .as_ref()
                .map(|body| Box::new(serde_json::from_value(body.clone()).unwrap())),
        },
        operation_id: Some(OPERATION.into()),
        mutation_may_have_occurred: true,
        raw_service_error: body.map(Box::new),
    }
}

fn service(code: &str) -> Value {
    json!({ "errorCode": code, "message": PRIVATE, "connectionString": PRIVATE })
}

#[test]
fn exact_documented_codes_are_advisory_and_leave_original_evidence_unchanged() {
    for (code, condition) in [
        (
            ServiceCode::DataflowExecuteQueryError,
            DiagnosticCondition::QueryExecutionFailed,
        ),
        (
            ServiceCode::GatewayNotReachable,
            DiagnosticCondition::GatewayUnavailable,
        ),
        (
            ServiceCode::GatewayDataSourceAccessError,
            DiagnosticCondition::GatewaySourceAccess,
        ),
    ] {
        let error = http(Some(code.as_str()), Some(service(code.as_str())));
        let original = error.clone();
        let diagnostic = Diagnostic::from_operation_error(&error);
        assert_eq!(diagnostic.condition(), condition);
        assert_eq!(diagnostic.basis(), DiagnosticBasis::DocumentedServiceCode);
        assert_eq!(diagnostic.service_code(), Some(code));
        assert_eq!(error, original);
        assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
        assert!(error.mutation_may_have_occurred);
        let presentation = format!(
            "{diagnostic:?} {} {:?} {}",
            diagnostic.summary(),
            diagnostic.checks(),
            serde_json::to_string(&diagnostic).unwrap(),
        );
        assert!(!presentation.contains(PRIVATE));
        assert!(!presentation.contains("arrowVersion"));
        assert!(!presentation.contains("pq-arrow-version=1"));
    }
}

#[test]
fn status_or_misleading_message_never_establishes_a_known_condition() {
    for code in [
        None,
        Some("UnknownCode"),
        Some("gatewaynotreachable"),
        Some(" GatewayNotReachable"),
        Some("GatewayNotReachable\n"),
        Some("DM_GWPipeline_Gateway_InvalidConnectionCredentials"),
        Some("DMTS_DatasourceHasNoCredentialError"),
        Some("UnverifiedArrow2GatewayError"),
    ] {
        let error = http(
            code,
            code.map(|code| {
                json!({
                    "errorCode": code,
                    "message": "GatewayNotReachable: credentials invalid; use Arrow V1",
                    "moreDetails": [service("GatewayNotReachable")],
                })
            }),
        );
        let diagnostic = Diagnostic::from_operation_error(&error);
        assert_eq!(diagnostic, Diagnostic::unknown());
        assert!(diagnostic.summary().contains("unconfirmed"));
        assert!(
            diagnostic
                .checks()
                .iter()
                .any(|check| check.contains("saved binding"))
        );
        assert!(
            diagnostic
                .checks()
                .iter()
                .any(|check| check.contains("execute permission"))
        );
    }
}

#[test]
fn documented_immediate_details_can_refine_only_a_known_wrapper() {
    let mut body = service("DataflowExecuteQueryError");
    body["moreDetails"] = json!([service("DM_GWPipeline_Gateway_DataSourceAccessError")]);
    let error = http(Some("DataflowExecuteQueryError"), Some(body));
    let diagnostic = Diagnostic::from_operation_error(&error);
    assert_eq!(
        diagnostic.condition(),
        DiagnosticCondition::GatewaySourceAccess
    );
    assert_eq!(
        diagnostic.service_code(),
        Some(ServiceCode::GatewayDataSourceAccessError)
    );
    assert!(
        diagnostic
            .summary()
            .contains("invalid credentials are not established")
    );
}

#[test]
fn unknown_conflicting_malformed_and_excess_details_fail_closed() {
    for details in [
        Value::Null,
        json!({ "errorCode": "GatewayNotReachable" }),
        json!([null]),
        json!([{}]),
        json!([{ "errorCode": 7 }]),
        json!([{ "errorCode": "GatewayNotReachable", "message": 7 }]),
        json!([{ "errorCode": "GatewayNotReachable", "message": null }]),
        json!([{ "errorCode": "GatewayNotReachable", "parameters": {} }]),
        json!([{
            "errorCode": "GatewayNotReachable",
            "parameters": [{"name": "source", "value": null}],
        }]),
        json!([{
            "errorCode": "GatewayNotReachable",
            "parameters": vec![json!({"name": "source", "value": PRIVATE}); 9],
        }]),
        json!([{
            "errorCode": "GatewayNotReachable",
            "relatedResource": {"resourceId": 7, "resourceType": "Connection"},
        }]),
        json!([service("UnknownCode")]),
        json!([
            service("GatewayNotReachable"),
            service("DM_GWPipeline_Gateway_DataSourceAccessError"),
        ]),
        json!([{
            "errorCode": "GatewayNotReachable",
            "moreDetails": [service("GatewayNotReachable")],
        }]),
        json!(vec![service("GatewayNotReachable"); 9]),
    ] {
        let mut body = service("DataflowExecuteQueryError");
        body["moreDetails"] = details;
        let error = http(Some("DataflowExecuteQueryError"), Some(body));
        let original = error.clone();
        assert_eq!(
            Diagnostic::from_operation_error(&error),
            Diagnostic::unknown()
        );
        assert_eq!(error, original);
    }
    let conflicting = http(
        Some("GatewayNotReachable"),
        Some(service("DataflowExecuteQueryError")),
    );
    assert_eq!(
        Diagnostic::from_operation_error(&conflicting),
        Diagnostic::unknown()
    );
    let boundary = json!({
        "errorCode": "DataflowExecuteQueryError",
        "message": PRIVATE,
        "moreDetails": vec![service("GatewayNotReachable"); 8],
    });
    assert_eq!(
        Diagnostic::from_operation_error(&http(None, Some(boundary))).condition(),
        DiagnosticCondition::GatewayUnavailable
    );
}

#[test]
fn typed_execution_credential_failure_is_not_data_source_authentication() {
    let error = OperationError {
        kind: OperationErrorKind::Credential,
        operation_id: None,
        mutation_may_have_occurred: false,
        raw_service_error: None,
    };
    let diagnostic = Diagnostic::from_operation_error(&error);
    assert_eq!(
        diagnostic.condition(),
        DiagnosticCondition::ExecutionCredentialUnavailable
    );
    assert_eq!(diagnostic.basis(), DiagnosticBasis::ExecutionCredential);
    assert_eq!(diagnostic.service_code(), None);
    assert!(diagnostic.summary().contains("separate from the data source"));
}

#[test]
fn cancellation_uncertain_dispatch_and_unclassified_native_metadata_stay_unknown() {
    for kind in [
        OperationErrorKind::Cancelled,
        OperationErrorKind::InvalidResponse,
        OperationErrorKind::InvalidInput,
        OperationErrorKind::Limit,
    ] {
        let error = OperationError {
            kind,
            operation_id: Some(OPERATION.into()),
            mutation_may_have_occurred: true,
            raw_service_error: Some(Box::new(service("GatewayNotReachable"))),
        };
        let original = error.clone();
        assert_eq!(
            Diagnostic::from_operation_error(&error),
            Diagnostic::unknown()
        );
        assert_eq!(error, original);
    }
}

#[test]
fn terminal_failed_operation_uses_its_typed_error_not_transport_success() {
    let mut error = http(None, None);
    error.kind = OperationErrorKind::OperationFailed {
        status: "Failed".into(),
        error: Some(Box::new(
            serde_json::from_value(service("GatewayNotReachable")).unwrap(),
        )),
    };
    assert_eq!(
        Diagnostic::from_operation_error(&error).condition(),
        DiagnosticCondition::GatewayUnavailable
    );
    if let OperationErrorKind::OperationFailed { status, .. } = &mut error.kind {
        *status = "Cancelled".into();
    }
    assert_eq!(
        Diagnostic::from_operation_error(&error),
        Diagnostic::unknown()
    );
}

#[test]
fn header_only_exact_codes_are_recognized_without_inspecting_raw_body_values() {
    let mut error = http(Some("GatewayNotReachable"), None);
    assert_eq!(
        Diagnostic::from_operation_error(&error).service_code(),
        Some(ServiceCode::GatewayNotReachable)
    );
    if let OperationErrorKind::Http { code, .. } = &mut error.kind {
        *code = Some("x".repeat(129));
    }
    assert_eq!(
        Diagnostic::from_operation_error(&error),
        Diagnostic::unknown()
    );
}

#[test]
fn malformed_structured_bodies_cannot_be_promoted_by_a_known_header() {
    for body in [
        json!({ "errorCode": "UnknownCode", "message": null }),
        json!({ "errorCode": "GatewayNotReachable", "message": 7 }),
        json!({ "message": PRIVATE }),
        Value::Null,
        json!([service("GatewayNotReachable")]),
    ] {
        let mut error = http(Some("GatewayNotReachable"), None);
        error.raw_service_error = Some(Box::new(body));
        assert_eq!(
            Diagnostic::from_operation_error(&error),
            Diagnostic::unknown()
        );
    }
}
