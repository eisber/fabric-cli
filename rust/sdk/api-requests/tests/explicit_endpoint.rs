// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit endpoint rebinding retains service identity, route and opaque query.

#![deny(unnameable_test_items)]

use azure_core::http::Method;
use fabric_core::{EndpointBinding, ServiceOrigin};
use fabric_sdk_api::{ApiError, Origin, RawRequestOptions, Request, RequestBounds};

fn request() -> Request {
    Request::raw(RawRequestOptions {
        origin: Origin::RelevanceEngine,
        method: Method::Post,
        endpoint: "discoverArtifacts".into(),
        query: vec![("searchText".into(), "sales & margin + growth".into())],
        headers: vec![],
        body: vec![],
        bounds: RequestBounds::default(),
    })
    .unwrap()
}

#[test]
fn selected_host_cannot_change_the_discovery_route_or_query() {
    let original = request();
    let endpoint = EndpointBinding::with_host(
        ServiceOrigin::RelevanceEngine,
        "dailyapi.fabric.microsoft.com",
    )
    .unwrap();
    let rebound = original.clone().with_endpoint(&endpoint).unwrap();
    assert_eq!(rebound.origin(), Origin::RelevanceEngine);
    assert_eq!(
        rebound.url().host_str(),
        Some("dailyapi.fabric.microsoft.com")
    );
    assert_eq!(
        rebound.url().path(),
        "/relevanceengine/v1/discoverArtifacts"
    );
    assert_eq!(rebound.url().query(), original.url().query());
}

#[test]
fn explicit_binding_rejects_service_substitution_before_execution() {
    for origin in [
        ServiceOrigin::Fabric,
        ServiceOrigin::PowerBi,
        ServiceOrigin::OneLake,
        ServiceOrigin::Arm,
    ] {
        assert!(matches!(
            request().with_endpoint(&EndpointBinding::public(origin)),
            Err(ApiError::InvalidAudience)
        ));
    }
}
