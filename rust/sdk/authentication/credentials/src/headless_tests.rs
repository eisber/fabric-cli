// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure contract checks without environment mutation or HTTP.
#![deny(unnameable_test_items)]

use super::*;

#[test]
fn managed_host_contract_rejects_changes_without_mutating_the_environment() {
    let host = ManagedHost::VirtualMachine;
    assert!(host.validate_with(|_| Ok(None)).is_ok());
    for selected in MI_VARIABLES {
        assert!(
            host.validate_with(|name| {
                Ok((name == selected).then(|| "unexpected-host-input".into()))
            })
            .is_err()
        );
    }
    let host = ManagedHost::app_service("http://127.0.0.1:49152/msi/token").unwrap();
    let selected = |name: &str| {
        Ok(match name {
            "IDENTITY_ENDPOINT" => Some("http://127.0.0.1:49152/msi/token".into()),
            "IDENTITY_HEADER" => Some("synthetic-header".into()),
            _ => None,
        })
    };
    assert!(host.validate_with(selected).is_ok());
    assert!(
        host.validate_with(|name| {
            if name == "IDENTITY_HEADER" {
                Ok(Some("changed-header".into()))
            } else {
                selected(name)
            }
        })
        .is_ok()
    );
    assert!(
        host.validate_request_header(Some("rotated-header"), |_| Ok(Some(
            "rotated-header".into()
        )))
        .is_ok()
    );
    assert!(
        host.validate_request_header(Some("old-header"), |_| Ok(Some("rotated-header".into())))
            .is_err()
    );
    assert!(
        host.validate_request_header(Some("bad\r\nheader"), |_| Ok(Some("bad\r\nheader".into())))
            .is_err()
    );
    assert!(
        host.validate_with(|name| {
            if name == "IDENTITY_ENDPOINT" {
                Ok(Some("http://127.0.0.1:49153/msi/token".into()))
            } else {
                selected(name)
            }
        })
        .is_err()
    );
}

#[derive(Debug)]
struct Never;

#[async_trait]
impl HttpClient for Never {
    async fn execute_request(&self, _: &Request) -> azure_core::Result<AsyncRawResponse> {
        panic!("unexpected HTTP");
    }
}

#[tokio::test]
async fn transport_guard_rejects_wrong_authority_path_method_and_redirect_before_inner_http() {
    let guard = CredentialHttp {
        inner: Arc::new(Never),
        endpoint: Url::parse("https://login.microsoftonline.com/tenant/oauth2/v2.0/token").unwrap(),
        host: None,
        managed_client: None,
    };
    for (url, method) in [
        (
            "https://evil.example/tenant/oauth2/v2.0/token",
            Method::Post,
        ),
        (
            "https://login.microsoftonline.com/other/oauth2/v2.0/token",
            Method::Post,
        ),
        (
            "https://login.microsoftonline.com/tenant/oauth2/v2.0/token",
            Method::Get,
        ),
        (
            "https://login.microsoftonline.com/tenant/oauth2/v2.0/token?secret=value",
            Method::Post,
        ),
        (
            "https://user@login.microsoftonline.com/tenant/oauth2/v2.0/token",
            Method::Post,
        ),
    ] {
        assert!(
            guard
                .execute_request(&Request::new(Url::parse(url).unwrap(), method))
                .await
                .is_err()
        );
    }
}
