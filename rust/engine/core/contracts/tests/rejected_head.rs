// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Invalid response metadata cannot erase an independent, unique operation ID.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use fabric_core::*;
use std::collections::BTreeMap;

fn fields(id_fields: &[(&str, &str)]) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::from([
        ("Retry-After".into(), "1".into()),
        ("retry-after".into(), "2".into()),
        (
            "location".into(),
            "https://evil.invalid/status?secret=synthetic".into(),
        ),
    ]);
    fields.extend(
        id_fields
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string())),
    );
    fields
}
fn assert_safe_id(error: &CoreError) {
    assert_eq!(error.kind, ErrorKind::Protocol);
    let recovery = error.recovery.as_ref().unwrap();
    assert_eq!(recovery.operation_id.as_deref(), Some("op-1"));
    assert_eq!(
        recovery.status_url,
        "https://api.fabric.microsoft.com/v1/operations/op-1"
    );
    assert!(recovery.outcome_uncertain);
    assert!(!format!("{error:?}").contains("synthetic"));
    assert!(!format!("{error:?}").contains("evil.invalid"));
}

#[test]
fn rejected_materialized_head_preserves_only_independent_unique_id() {
    let headers = fields(&[("X-MS-Operation-ID", "op-1")]);
    let error = operation_handle(Audience::Fabric, 202, &headers).unwrap_err();
    assert_safe_id(&error);
    let head = ResponseHeadCapture::new(Audience::Fabric);
    let error = head.record(202, &headers).unwrap_err();
    assert_safe_id(&error);
    assert_eq!(
        head.recovery().unwrap().operation_id.as_deref(),
        Some("op-1")
    );
    let later = head.attach(CoreError::new(ErrorKind::Cancelled, "Cancelled locally"));
    assert_eq!(later.kind, ErrorKind::Cancelled);
    assert_eq!(
        later.recovery.unwrap().operation_id.as_deref(),
        Some("op-1")
    );
}

#[test]
fn duplicate_identical_conflicting_or_malformed_ids_are_never_selected() {
    for ids in [
        vec![("X-MS-Operation-ID", "op-1"), ("x-ms-operation-id", "op-1")],
        vec![
            ("X-MS-Operation-ID", "op-1"),
            ("x-ms-operation-id", "other"),
        ],
        vec![
            ("X-MS-Operation-ID", "bad/id"),
            ("x-ms-operation-id", "op-1"),
        ],
        vec![
            ("X-MS-Operation-ID", "op-1"),
            ("x-ms-operation-id", "bad/id"),
        ],
        vec![("x-ms-operation-id", "bad/id")],
        vec![],
    ] {
        let mut headers = fields(&ids);
        // Even a safe Location must not be salvaged from an invalid head.
        headers.insert(
            "location".into(),
            "/v1/operations/location-only?secret=synthetic".into(),
        );
        let error = operation_handle(Audience::Fabric, 202, &headers).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.recovery.is_none());
        let head = ResponseHeadCapture::new(Audience::Fabric);
        assert!(head.record(202, &headers).unwrap_err().recovery.is_none());
        assert!(head.recovery().is_none());
    }
}

#[test]
fn rejected_raw_header_scan_counts_all_id_fields_without_location_salvage() {
    for ids in [
        vec![
            ("x-ms-operation-id", b"op-1".as_slice()),
            ("X-MS-Operation-ID", b"op-1".as_slice()),
        ],
        vec![
            ("x-ms-operation-id", b"op-1".as_slice()),
            ("X-MS-Operation-ID", b"other".as_slice()),
        ],
        vec![("x-ms-operation-id", b"\xff".as_slice())],
    ] {
        let mut raw = vec![
            ("retry-after", b"1".as_slice()),
            ("retry-after", b"2".as_slice()),
        ];
        raw.extend(ids);
        raw.push(("location", b"/v1/operations/location-only".as_slice()));
        let head = ResponseHeadCapture::new(Audience::Fabric);
        let error = head.reject_headers(
            202,
            raw,
            CoreError::new(ErrorKind::Protocol, "Duplicate control header"),
        );
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.recovery.is_none());
        assert!(head.recovery().is_none());
    }
}

#[test]
fn rejected_header_recovery_is_limited_to_accepted_fabric_ids() {
    let headers = fields(&[("x-ms-operation-id", "op-1")]);
    for (audience, status) in [
        (Audience::Fabric, 200),
        (Audience::Fabric, 400),
        (Audience::Azure, 202),
        (Audience::Storage, 202),
        (Audience::PowerBi, 202),
    ] {
        let error = operation_handle(audience, status, &headers).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.recovery.is_none());
    }
}
