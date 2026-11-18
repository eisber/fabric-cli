// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Diagnostic constructors cannot preserve arbitrary external text.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use fabric_core::{
    CoreError, ErrorKind,
    diagnostics::{DiagnosticOutcome, HttpStatus, OperationOutcome, RequestId},
};

#[test]
fn request_ids_accept_only_canonical_uuid_shape_and_normalize_ascii_hex() {
    let id = RequestId::parse("ABCDEF12-3456-7890-ABCD-EF1234567890").unwrap();
    assert_eq!(id.to_string(), "abcdef12-3456-7890-abcd-ef1234567890");
    assert_eq!(format!("{id:?}"), id.to_string());
    for candidate in [
        "SECRET-canary",
        "abcdef1234567890abcdef1234567890",
        "{abcdef12-3456-7890-abcd-ef1234567890}",
        "abcdef12-3456-7890-abcd-ef1234567890\n",
        "abcdef12-3456-7890-abcd-ef12345678\u{1b}0",
        "abcdef12-3456-7890-abcd-ef123456789g",
    ] {
        assert!(RequestId::parse(candidate).is_none());
    }
}

#[test]
fn diagnostic_failures_report_fixed_truthful_outcomes_without_provider_errors() {
    for (outcome, phrase) in [
        (DiagnosticOutcome::BeforeSend, "no request was sent"),
        (DiagnosticOutcome::Uncertain, "outcome is uncertain"),
        (
            DiagnosticOutcome::Received(HttpStatus::new(202).unwrap()),
            "HTTP status 202",
        ),
        (DiagnosticOutcome::Accepted, "operation was accepted"),
        (
            DiagnosticOutcome::Terminal(OperationOutcome::Succeeded),
            "operation succeeded",
        ),
        (
            DiagnosticOutcome::Terminal(OperationOutcome::Failed),
            "operation failed",
        ),
        (
            DiagnosticOutcome::Terminal(OperationOutcome::Cancelled),
            "operation was cancelled",
        ),
    ] {
        let error = CoreError::diagnostic_failure(outcome);
        assert_eq!(error.kind, ErrorKind::Diagnostic);
        assert_eq!(error.diagnostic_outcome(), Some(outcome));
        assert!(error.message.contains(phrase));
        if outcome != DiagnosticOutcome::BeforeSend {
            assert!(!error.message.contains("no request was sent"));
        }
    }
}

#[test]
fn private_error_details_preserve_flat_serialization_without_exposing_outcomes() {
    use serde_json::{json, to_value};

    let plain = CoreError::new(ErrorKind::Protocol, "Invalid response");
    assert_eq!(
        to_value(&plain).unwrap(),
        json!({"kind": "Protocol", "message": "Invalid response", "recovery": null})
    );
    let service = CoreError::service_failure("Backend operation failed", Some("BackendFailure"));
    let cloned = service.clone();
    assert_eq!(cloned.service_code(), Some("BackendFailure"));
    assert_eq!(
        to_value(&cloned).unwrap(),
        json!({
            "kind": "Service",
            "message": "Backend operation failed",
            "recovery": null,
            "service_code": "BackendFailure"
        })
    );
    let diagnostic = CoreError::diagnostic_failure(DiagnosticOutcome::Accepted).clone();
    assert_eq!(
        diagnostic.diagnostic_outcome(),
        Some(DiagnosticOutcome::Accepted)
    );
    assert_eq!(
        to_value(&diagnostic).unwrap(),
        json!({"kind": "Diagnostic", "message": diagnostic.message, "recovery": null})
    );
}

#[test]
fn service_messages_drop_controls_url_queries_and_excess_length() {
    use fabric_core::{MAX_SERVICE_MESSAGE_CHARS, sanitize_service_message};
    assert_eq!(sanitize_service_message(" \t\n\u{1b}\u{202e} "), None);
    assert_eq!(
        sanitize_service_message("Item\u{1b}[31m  not\r\nfound\u{200b}"),
        Some("Item[31m not found".to_owned())
    );
    assert_eq!(
        sanitize_service_message(
            "See HTTPS://host/a?sig=SECRET-CANARY and url=https://b/c#SECRET-CANARY now"
        ),
        Some("See HTTPS://host/a and url=https://b/c now".to_owned())
    );
    let long = sanitize_service_message(&"é".repeat(MAX_SERVICE_MESSAGE_CHARS + 1)).unwrap();
    assert_eq!(long.chars().count(), MAX_SERVICE_MESSAGE_CHARS);
    assert!(long.ends_with('…'));
}
