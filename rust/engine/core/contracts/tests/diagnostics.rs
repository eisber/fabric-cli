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
