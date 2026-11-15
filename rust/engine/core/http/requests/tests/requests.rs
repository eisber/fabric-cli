// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline URL/header validation before identity acquisition.
#![deny(unnameable_test_items)]
#![forbid(unsafe_code)]
use fabric_core::Audience;
use fabric_sdk_api::build_request;
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
