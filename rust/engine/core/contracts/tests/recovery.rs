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
fn operation_url_rejection_categories_do_not_disclose_inputs() {
    let oversized = format!("/v1/operations/{}?secret=CANARY", "x".repeat(16 * 1024));
    for (value, expected) in [
        (
            oversized.as_str(),
            "Unsafe operation URL: length limit exceeded",
        ),
        (
            "/v1/operations/op?secret=CANARY with spaces",
            "Unsafe operation URL: control or space characters rejected",
        ),
        (
            "/v1/operations/op?secret=CANARY\n",
            "Unsafe operation URL: control or space characters rejected",
        ),
        (
            "/v1/operations/%2e%2e/op?secret=CANARY",
            "Unsafe operation URL: unsafe path rejected",
        ),
        (
            "/v1/operations/op%2fCANARY",
            "Unsafe operation URL: unsafe path rejected",
        ),
        (
            "https://CANARY.invalid:8443/v1/operations/op?secret=CANARY",
            "Unsafe operation URL: selected HTTPS origin or normalized components rejected",
        ),
        (
            "https://api.fabric.microsoft.com:8443/v1/operations/op?secret=CANARY",
            "Unsafe operation URL: selected HTTPS origin or normalized components rejected",
        ),
        (
            "https://user:CANARY@api.fabric.microsoft.com/v1/operations/op",
            "Unsafe operation URL: selected HTTPS origin or normalized components rejected",
        ),
        (
            "/v1/operations/op#CANARY",
            "Unsafe operation URL: selected HTTPS origin or normalized components rejected",
        ),
    ] {
        let error = validate_operation_url(Audience::Fabric, value).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert_eq!(error.message, expected);
        assert!(!error.to_string().contains("CANARY"));
        assert!(error.recovery.is_none());
    }
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
fn arm_advertised_locations_use_python_endpoint_suffix_on_the_fixed_origin() {
    for (location, expected) in [
        (
            "https://management.azure.com/operations/one?api-version=1",
            "https://management.azure.com/operations/one?api-version=1",
        ),
        (
            "http://management.azure.com/operations/one?api-version=1",
            "https://management.azure.com/operations/one?api-version=1",
        ),
        (
            "https://management.azure.com.evil.invalid/operations/one",
            "https://management.azure.com/.evil.invalid/operations/one",
        ),
        (
            "https://user:password@management.azure.com:8443/operations/one",
            "https://management.azure.com/:8443/operations/one",
        ),
        (
            "https://evil.invalid/management.azure.com/operations/one",
            "https://management.azure.com/operations/one",
        ),
        (
            "/operations/one?api-version=1",
            "https://management.azure.com/operations/one?api-version=1",
        ),
    ] {
        for header in ["azure-asyncoperation", "location"] {
            let handle = operation_handle(Audience::Azure, 202, &headers(&[(header, location)]))
                .unwrap()
                .unwrap();
            assert_eq!(handle.url().as_str(), expected);
        }
    }
    assert!(
        operation_handle(
            Audience::Azure,
            202,
            &headers(&[("location", "https://evil.invalid/operations/one")]),
        )
        .is_err()
    );
    assert!(
        operation_handle(
            Audience::Azure,
            202,
            &headers(&[(
                "location",
                "https://management.azure.com/operations/%2e%2e/secret"
            )]),
        )
        .is_err(),
        "unsafe paths remain rejected after rebasing"
    );
    assert!(
        validate_operation_url(
            Audience::Azure,
            "https://management.azure.com.evil.invalid/operations/one"
        )
        .is_err(),
        "executable polling destinations remain bound to the Azure endpoint"
    );
}

#[test]
fn unsafe_head_cannot_expose_an_unvalidated_url_or_erase_known_safe_id() {
    for value in [
        "http://evil.invalid/operation?secret=synthetic",
        "http://api.fabric.microsoft.com/v1/operations/op-1",
        "https://user:synthetic@api.fabric.microsoft.com/v1/operations/op-1",
        "/v1/operations/%2e%2e/op-1",
        "/v1/operations/op-1#synthetic",
        "http://wabi-us-redirect.analysis.windows.net.evil.invalid/v1/items/item?secret=synthetic",
        "https://user:synthetic@wabi-us-redirect.analysis.windows.net/v1/items/item",
        "https://wabi-us-redirect.analysis.windows.net:444/v1/items/item",
        "https://wabi-us-redirect.analysis.windows.net/v1/items/%2e%2e/item",
        "https://wabi-us-redirect.analysis.windows.net/v1/items/item#synthetic",
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
    let fields = headers(&[("location", "http://evil.invalid/operation?secret=synthetic")]);
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
        "http://evil.invalid/v1/workspaces/workspace/items/item/jobs/instances/job",
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

#[test]
fn regional_fabric_handles_and_relative_results_preserve_origin_and_redact_queries() {
    for host in [
        "wabi-us-central-b-primary-redirect.analysis.windows.net",
        "wabi-west-europe-redirect.analysis.windows.net",
        "msitapi.fabric.microsoft.com",
        "df-msit-scus-redirect.analysis.windows.net",
        "service-advertised.example.test",
        "custom-cluster.example.test",
    ] {
        for (path, kind) in [
            ("/v1/operations/op-1", OperationKind::Fabric),
            (
                "/v1/workspaces/workspace/items/item/jobs/instances/job",
                OperationKind::FabricJob,
            ),
        ] {
            let location = format!("https://{host}{path}?secret=CANARY");
            let fields = headers(&[("Location", &location)]);
            let handle = operation_handle(Audience::Fabric, 202, &fields)
                .unwrap()
                .unwrap();
            assert_eq!(handle.kind(), kind);
            assert_eq!(handle.url().as_str(), location);
            let capture = ResponseHeadCapture::new(Audience::Fabric);
            capture.record(202, &fields).unwrap();
            assert_eq!(
                capture.recovery().unwrap().status_url,
                format!("https://{host}{path}")
            );
            assert!(!format!("{handle:?}{capture:?}").contains("CANARY"));
        }
        let status = validate_operation_url(
            Audience::Fabric,
            &format!("https://{host}/v1/operations/op-1"),
        )
        .unwrap();
        let result = resolve_operation_url(
            Audience::Fabric,
            "/v1/operations/op-1/result?secret=CANARY",
            &status,
        )
        .unwrap();
        assert_eq!(result.host_str(), Some(host));
        assert_eq!(result.query(), Some("secret=CANARY"));
        assert_eq!(result.path(), "/v1/operations/op-1/result");
        assert!(validate_url(Audience::Fabric, &status).is_err());
        for audience in [Audience::Azure, Audience::Storage, Audience::PowerBi] {
            assert!(validate_operation_url(audience, status.as_str()).is_err());
        }
    }
}

#[test]
fn fabric_operation_ids_select_status_routes_from_safe_location_origins() {
    for host in [
        "api.fabric.microsoft.com",
        "wabi-west-europe-redirect.analysis.windows.net",
        "msitapi.fabric.microsoft.com",
        "df-msit-scus-redirect.analysis.windows.net",
        "service-advertised.example.test",
    ] {
        for (path, query) in [
            ("/v1/workspaces/workspace/items/item", None),
            ("/v1/operations/op-1/result", None),
            ("/v1/operations/op-1", Some("secret=CANARY")),
        ] {
            let location = format!("https://{host}{path}?secret=CANARY");
            let fields = headers(&[("Location", &location), ("x-ms-operation-id", "op-1")]);
            let handle = operation_handle(Audience::Fabric, 202, &fields)
                .unwrap()
                .unwrap();
            assert_eq!(handle.kind(), OperationKind::Fabric);
            assert_eq!(handle.operation_id(), Some("op-1"));
            assert_eq!(handle.url().host_str(), Some(host));
            assert_eq!(handle.url().path(), "/v1/operations/op-1");
            assert_eq!(handle.url().query(), query);
            validate_operation_url(Audience::Fabric, handle.url().as_str()).unwrap();
            let capture = ResponseHeadCapture::new(Audience::Fabric);
            capture.record(202, &fields).unwrap();
            assert_eq!(
                capture.recovery().unwrap().status_url,
                format!("https://{host}/v1/operations/op-1")
            );
            assert!(!format!("{handle:?}{capture:?}").contains("CANARY"));
        }
    }
    let error = operation_handle(
        Audience::Fabric,
        202,
        &headers(&[
            ("Location", "/v1/operations/other/result"),
            ("x-ms-operation-id", "op-1"),
        ]),
    )
    .unwrap_err();
    assert!(error.message.contains("Conflicting"));
}

#[test]
fn fabric_result_locations_select_canonical_routes_without_promoting_resource_urls() {
    let handle = operation_handle(
        Audience::Fabric,
        202,
        &headers(&[(
            "Location",
            "https://wabi-west-europe-redirect.analysis.windows.net/v1/operations/op-1",
        )]),
    )
    .unwrap()
    .unwrap();
    for (location, expected) in [
        (
            "/v1/workspaces/workspace/items/item?secret=CANARY",
            "https://wabi-west-europe-redirect.analysis.windows.net/v1/operations/op-1/result",
        ),
        (
            "/v1/operations/op-1?secret=CANARY",
            "https://wabi-west-europe-redirect.analysis.windows.net/v1/operations/op-1/result",
        ),
        (
            "/v1/operations/op-1/result?secret=CANARY",
            "https://wabi-west-europe-redirect.analysis.windows.net/v1/operations/op-1/result?secret=CANARY",
        ),
        (
            "https://wabi-us-central-b-primary-redirect.analysis.windows.net/v1/items/item?secret=CANARY",
            "https://wabi-us-central-b-primary-redirect.analysis.windows.net/v1/operations/op-1/result",
        ),
        (
            "https://df-msit-scus-redirect.analysis.windows.net/v1/operations/op-1/result?secret=CANARY",
            "https://df-msit-scus-redirect.analysis.windows.net/v1/operations/op-1/result?secret=CANARY",
        ),
    ] {
        let result = handle.fabric_result_url(location).unwrap();
        assert_eq!(result.as_str(), expected);
        validate_operation_url(Audience::Fabric, result.as_str()).unwrap();
    }
    assert!(
        validate_operation_url(
            Audience::Fabric,
            "https://wabi-west-europe-redirect.analysis.windows.net/v1/items/item"
        )
        .is_err(),
        "an origin hint is not permission to execute its resource path"
    );
    for location in [
        "http://evil.invalid/v1/items/item?secret=CANARY",
        "https://user:CANARY@wabi-west-europe-redirect.analysis.windows.net/v1/items/item",
        "https://wabi-west-europe-redirect.analysis.windows.net:444/v1/items/item",
        "/v1/items/%2e%2e/item?secret=CANARY",
        "/v1/items/item#CANARY",
        "/v1/operations/other/result?secret=CANARY",
        "/v1/operations/other?secret=CANARY",
        "/v1/operations/other/?secret=CANARY",
    ] {
        let error = handle.fabric_result_url(location).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(!error.message.contains("CANARY"));
    }
}

#[test]
fn advertised_fabric_hosts_still_reject_unsafe_components_and_unrecognized_routes() {
    for host in [
        "wabi-us-redirect.analysis.windows.net:444",
        "user:CANARY@wabi-us-redirect.analysis.windows.net",
        "msitapi.fabric.microsoft.com:444",
        "user:CANARY@msitapi.fabric.microsoft.com",
        "df-msit-scus-redirect.analysis.windows.net:444",
        "user:CANARY@df-msit-scus-redirect.analysis.windows.net",
        "service-advertised.example.test:444",
        "user:CANARY@service-advertised.example.test",
    ] {
        let location = format!("https://{host}/v1/operations/op-1?secret=CANARY");
        let error = validate_operation_url(Audience::Fabric, &location).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(!error.message.contains("CANARY"));
    }
    for path in [
        "/v1/items",
        "/v1/operations/",
        "/v1/operations/op-1/cancel",
        "/v1/workspaces/workspace/items/item/jobs/instances",
        "/v1/workspaces/workspace/items/item/jobs/instances/job/cancel",
        "/v1/operations/%2e%2e/op-1",
        "/v1/operations/op%2f1",
        "/v1/operations/op-1#CANARY",
    ] {
        let location = format!("https://wabi-us-redirect.analysis.windows.net{path}");
        assert!(validate_operation_url(Audience::Fabric, &location).is_err());
        let location = format!("https://msitapi.fabric.microsoft.com{path}");
        assert!(validate_operation_url(Audience::Fabric, &location).is_err());
        let location = format!("https://df-msit-scus-redirect.analysis.windows.net{path}");
        assert!(validate_operation_url(Audience::Fabric, &location).is_err());
        let location = format!("https://service-advertised.example.test{path}");
        assert!(validate_operation_url(Audience::Fabric, &location).is_err());
    }
}
