// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline URL/header validation before identity acquisition.
#![deny(unnameable_test_items)]
#![forbid(unsafe_code)]
use fabric_core::{Audience, Endpoint};
use fabric_sdk_api::{build_request, build_request_with_endpoint};
use std::collections::BTreeMap;

#[test]
fn all_audiences_methods_and_encoded_query_preserved() {
    for audience in [
        Audience::Fabric,
        Audience::Storage,
        Audience::Azure,
        Audience::PowerBi,
    ] {
        for method in ["get", "post", "delete", "put", "patch"] {
            let request = build_request(
                audience,
                method,
                "items?value=a%2Fb%3D%26",
                BTreeMap::new(),
                vec![],
            )
            .unwrap();
            assert_eq!(request.url.query(), Some("value=a%2Fb%3D%26"));
            assert_eq!(request.method, method.to_ascii_uppercase());
            assert!(request.url.as_str().starts_with(audience.base_url()));
        }
    }
}

#[test]
fn configured_endpoint_controls_relative_and_absolute_requests() {
    let service =
        Endpoint::from_hostname(Audience::Fabric, "msitapi.fabric.microsoft.com").unwrap();
    let request =
        build_request_with_endpoint(&service, "GET", "items", BTreeMap::new(), vec![]).unwrap();
    assert_eq!(
        request.url.as_str(),
        "https://msitapi.fabric.microsoft.com/v1/items"
    );
    assert!(
        build_request_with_endpoint(
            &service,
            "GET",
            "https://api.fabric.microsoft.com/v1/items",
            BTreeMap::new(),
            vec![],
        )
        .is_err()
    );
}

#[test]
fn each_audience_uses_the_configured_host_and_expected_base_path() {
    for (audience, host, expected) in [
        (
            Audience::Fabric,
            "msitapi.fabric.microsoft.com",
            "https://msitapi.fabric.microsoft.com/v1/items",
        ),
        (
            Audience::Storage,
            "stage.dfs.fabric.microsoft.com",
            "https://stage.dfs.fabric.microsoft.com/items",
        ),
        (
            Audience::Azure,
            "stage.management.azure.com",
            "https://stage.management.azure.com/items",
        ),
        (
            Audience::PowerBi,
            "stage.powerbi.com",
            "https://stage.powerbi.com/v1.0/myorg/items",
        ),
    ] {
        let endpoint = Endpoint::from_hostname(audience, host).unwrap();
        let request =
            build_request_with_endpoint(&endpoint, "GET", "items", BTreeMap::new(), vec![])
                .unwrap();
        assert_eq!(request.url.as_str(), expected);
    }
}
#[test]
fn unsafe_urls_and_controls_fail_closed() {
    for endpoint in [
        "https://evil.invalid/x",
        "https://wabi-us-central-b-primary-redirect.analysis.windows.net/v1/operations/op-1",
        "//evil.invalid/x",
        "http://api.fabric.microsoft.com/x",
        "https://user:password@api.fabric.microsoft.com/v1/x",
        "items#fragment",
        "../items",
        "%2e%2e/items",
        "a/%2F/items",
        "a\\items",
        "items\ninjected",
    ] {
        assert!(
            build_request(Audience::Fabric, "GET", endpoint, BTreeMap::new(), vec![]).is_err(),
            "{endpoint}"
        );
    }
    for name in [
        "Authorization",
        "Host",
        "Connection",
        "Content-Length",
        "Transfer-Encoding",
        "Proxy-Authorization",
    ] {
        assert!(
            build_request(
                Audience::Fabric,
                "GET",
                "items",
                BTreeMap::from([(name.into(), "value".into())]),
                vec![]
            )
            .is_err()
        );
    }
    assert!(
        build_request(
            Audience::Fabric,
            "GET",
            "items",
            BTreeMap::from([("x-safe".into(), "value\r\nsecret".into())]),
            vec![]
        )
        .is_err()
    );
}
