// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline safe operation-head recovery shared by transport and reconciliation.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use fabric_core::*;
use std::collections::BTreeMap;

fn headers(values: &[(&str, &str)]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

#[test]
fn fabric_and_arm_safe_head_capture_redacts_queries() {
    for (audience, status, fields, expected) in [
        (
            Audience::Fabric,
            202,
            vec![("x-ms-operation-id", "op-1")],
            "https://api.fabric.microsoft.com/v1/operations/op-1",
        ),
        (
            Audience::Fabric,
            202,
            vec![("Location", "/v1/operations/op-1?secret=synthetic")],
            "https://api.fabric.microsoft.com/v1/operations/op-1",
        ),
        (
            Audience::Azure,
            201,
            vec![
                ("azure-asyncoperation", "/status?secret=synthetic"),
                ("location", "/ignored"),
            ],
            "https://management.azure.com/status",
        ),
        (
            Audience::Azure,
            202,
            vec![("location", "/location-status?secret=synthetic")],
            "https://management.azure.com/location-status",
        ),
    ] {
        let capture = ResponseHeadCapture::new(audience);
        let fields = headers(&fields);
        capture.record(status, &fields).unwrap();
        let error = capture.attach(CoreError::new(ErrorKind::Transport, "Body stream failed"));
        let recovery = error.recovery.unwrap();
        assert_eq!(recovery.status_url, expected);
        assert!(recovery.outcome_uncertain);
        let handle = operation_handle(audience, status, &fields)
            .unwrap()
            .unwrap();
        assert!(!format!("{handle:?}").contains("synthetic"));
        assert!(!format!("{capture:?}").contains("synthetic"));
    }
}

#[test]
fn unsafe_head_cannot_expose_an_unvalidated_url_or_erase_known_safe_id() {
    for value in [
        "https://evil.invalid/operation?secret=synthetic",
        "http://api.fabric.microsoft.com/v1/operations/op-1",
        "https://user:synthetic@api.fabric.microsoft.com/v1/operations/op-1",
        "/v1/operations/%2e%2e/op-1",
        "/v1/operations/op-1#synthetic",
    ] {
        let capture = ResponseHeadCapture::new(Audience::Fabric);
        let fields = headers(&[("location", value), ("x-ms-operation-id", "known-safe")]);
        let error = operation_handle(Audience::Fabric, 202, &fields).unwrap_err();
        capture.record(202, &fields).unwrap();
        assert_eq!(error.kind, ErrorKind::Protocol);
        let recovery = capture.recovery().unwrap();
        assert_eq!(recovery.operation_id.as_deref(), Some("known-safe"));
        assert_eq!(
            recovery.status_url,
            "https://api.fabric.microsoft.com/v1/operations/known-safe"
        );
        assert!(!error.to_string().contains("synthetic"));
    }
    let capture = ResponseHeadCapture::new(Audience::Fabric);
    let fields = headers(&[(
        "location",
        "https://evil.invalid/operation?secret=synthetic",
    )]);
    let error = operation_handle(Audience::Fabric, 202, &fields).unwrap_err();
    capture.record(202, &fields).unwrap();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(capture.recovery().is_none());
}

#[test]
fn conflicting_and_duplicate_handles_fail_explicitly() {
    let capture = ResponseHeadCapture::new(Audience::Fabric);
    let fields = headers(&[
        ("location", "/v1/operations/location-id?secret=synthetic"),
        ("x-ms-operation-id", "different-id"),
    ]);
    let error = operation_handle(Audience::Fabric, 202, &fields).unwrap_err();
    capture.record(202, &fields).unwrap();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(error.message.contains("Conflicting"));
    assert_eq!(
        error.recovery.unwrap().operation_id.as_deref(),
        Some("location-id")
    );
    let timeout = capture.attach(CoreError::new(ErrorKind::Timeout, "Body deadline"));
    assert_eq!(timeout.kind, ErrorKind::Timeout);
    assert!(timeout.message.contains("Conflicting"));
    let error = capture
        .record(
            202,
            &headers(&[
                ("Location", "/v1/operations/first"),
                ("location", "/v1/operations/second"),
            ]),
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(error.message.contains("duplicate"));
}

#[test]
fn raw_statuses_jobs_and_other_audiences_do_not_invent_recovery() {
    for (audience, status, fields) in [
        (Audience::Fabric, 200, vec![("x-ms-operation-id", "op-1")]),
        (
            Audience::Fabric,
            202,
            vec![
                ("location", "/v1/items/item/jobs/instances/job"),
                ("x-ms-operation-id", "op-1"),
            ],
        ),
        (
            Audience::Storage,
            202,
            vec![("location", "/v1/operations/op-1")],
        ),
        (Audience::PowerBi, 202, vec![("x-ms-operation-id", "op-1")]),
    ] {
        let capture = ResponseHeadCapture::new(audience);
        capture.record(status, &headers(&fields)).unwrap();
        assert!(capture.recovery().is_none());
    }
}

#[test]
fn job_handles_require_exact_instance_routes_and_safe_origins() {
    for location in [
        "/v1/workspaces/workspace/items/item/jobs/instances/job?secret=CANARY",
        "https://api.fabric.microsoft.com/v1/workspaces/workspace/items/item/jobs/instances/job",
    ] {
        for id in [None, Some("separate-operation-id")] {
            let mut fields = vec![("Location", location)];
            if let Some(id) = id {
                fields.push(("x-ms-operation-id", id));
            }
            let fields = headers(&fields);
            let handle = operation_handle(Audience::Fabric, 202, &fields)
                .unwrap()
                .unwrap();
            assert_eq!(handle.kind(), OperationKind::FabricJob);
            assert_eq!(handle.operation_id(), None);
            assert!(!handle.recovery().status_url.contains("CANARY"));
            let capture = ResponseHeadCapture::new(Audience::Fabric);
            capture.record(202, &fields).unwrap();
            assert_eq!(
                capture.recovery().unwrap().status_url,
                handle.recovery().status_url
            );
        }
    }
    for location in [
        "/v1/workspaces/workspace/items/item/jobs/instances?jobType=Execute",
        "/v1/workspaces/workspace/items/item/jobs/instances/job/cancel",
        "/v1/workspaces/workspace/items/item/jobs/instances/",
    ] {
        assert!(
            operation_handle(Audience::Fabric, 202, &headers(&[("location", location)]))
                .unwrap()
                .is_none()
        );
    }
    for location in [
        "https://evil.invalid/v1/workspaces/workspace/items/item/jobs/instances/job",
        "https://user:CANARY@api.fabric.microsoft.com/v1/workspaces/workspace/items/item/jobs/instances/job",
        "/v1/workspaces/workspace/items/item/jobs/instances/%2e%2e/job",
        "/v1/workspaces/workspace/items/item/jobs/instances/job#CANARY",
    ] {
        let error = operation_handle(Audience::Fabric, 202, &headers(&[("location", location)]))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.recovery.is_none());
        assert!(!error.message.contains("CANARY"));
    }
}
