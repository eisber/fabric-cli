// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The production metadata client is exact-destination and never a service client.
#![deny(unnameable_test_items)]

use super::*;
use crate::test_server::{Reply, Server};
use azure_core::http::{HttpClient, Method, Request, Url};

#[test]
fn metadata_construction_refuses_dns_remote_addresses_proxy_and_url_credentials() {
    for endpoint in [
        "http://localhost/token",
        "http://10.0.0.1/token",
        "http://192.168.1.1/token",
        "http://8.8.8.8/token",
        "https://127.0.0.1/token",
        "http://user:secret@127.0.0.1/token",
        "http://127.0.0.1/token?secret=value",
        "http://127.0.0.1/token#secret",
        "http://127.0.0.1:0/token",
    ] {
        let error = NativeHttpClient::for_managed_identity(
            options(Vec::new()),
            Url::parse(endpoint).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error, TransportError::InvalidConfiguration);
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
    let mut configuration = options(Vec::new());
    configuration.proxy =
        ProxyPolicy::StaticHttp(StaticHttpProxy::new("http://127.0.0.1:8080").unwrap());
    assert_eq!(
        NativeHttpClient::for_managed_identity(
            configuration,
            Url::parse("http://127.0.0.1/token").unwrap()
        )
        .unwrap_err(),
        TransportError::InvalidConfiguration
    );
}

#[tokio::test]
async fn dedicated_metadata_client_sends_actual_http_but_refuses_all_other_destinations() {
    let server = Server::start(vec![Reply::body(200, b"metadata".to_vec())], None).await;
    let endpoint = Url::parse(&format!("http://{}/msi/token", server.address)).unwrap();
    let client =
        NativeHttpClient::for_managed_identity(options(Vec::new()), endpoint.clone()).unwrap();
    for (url, method) in [
        (
            "https://api.fabric.microsoft.com/v1/workspaces".to_owned(),
            Method::Get,
        ),
        (format!("http://{}/other", server.address), Method::Get),
        (format!("http://{}/msi/token", server.address), Method::Post),
        ("http://127.0.0.1:1/msi/token".to_owned(), Method::Get),
    ] {
        assert!(
            client
                .execute_request(&Request::new(Url::parse(&url).unwrap(), method))
                .await
                .is_err()
        );
    }
    assert!(server.state.requests.lock().unwrap().is_empty());
    let mut endpoint = endpoint;
    endpoint.set_query(Some(
        "api-version=2019-08-01&resource=https%3A%2F%2Fapi.fabric.microsoft.com",
    ));
    let mut request = Request::new(endpoint, Method::Get);
    request.insert_header("x-identity-header", "synthetic-platform-header");
    let response = client.execute_request(&request).await.unwrap();
    assert_eq!(bytes(response).await, b"metadata");
    let requests = server.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = String::from_utf8_lossy(&requests[0]);
    assert!(request.contains("/msi/token?api-version=2019-08-01"));
    assert!(request.contains("x-identity-header: synthetic-platform-header"));
}

#[tokio::test]
async fn vm_metadata_wire_hook_keeps_exact_host_and_never_follows_redirects() {
    let server = Server::start(
        vec![
            Reply::body(302, b"synthetic-redirect".to_vec())
                .headers(&[("Location", "https://evil.example/token")]),
        ],
        None,
    )
    .await;
    let endpoint = Url::parse("http://169.254.169.254/metadata/identity/oauth2/token").unwrap();
    let client = NativeHttpClient::for_managed_identity_test(
        options(Vec::new()),
        endpoint.clone(),
        server.address,
    )
    .unwrap();
    let response = client
        .execute_request(&Request::new(endpoint.clone(), Method::Get))
        .await
        .unwrap();
    assert_eq!(u16::from(response.status()), 302);
    assert_eq!(bytes(response).await, b"synthetic-redirect");
    assert_eq!(server.state.requests.lock().unwrap().len(), 1);
    assert!(
        String::from_utf8_lossy(&server.state.requests.lock().unwrap()[0])
            .contains("host: 169.254.169.254")
    );
    let normal = NativeHttpClient::new(options(Vec::new())).unwrap();
    assert!(
        normal
            .execute_request(&Request::new(endpoint, Method::Get))
            .await
            .is_err()
    );
}
