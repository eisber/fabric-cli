// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller-selected job endpoints over the existing shared HTTP executor.

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_jobs::{
    Audience, CredentialSelection, EndpointBinding, JobClient, JobLimits, JobTarget, JobType,
    RequestContext, ServiceOrigin, TokenCredential,
};
use std::{
    future::pending,
    sync::{Arc, atomic::Ordering},
};
use support::*;

#[tokio::test]
async fn typed_status_reuses_the_original_credential_and_selected_host_with_fixed_fabric_scope() {
    for host in [
        "api.fabric.microsoft.com",
        "dailyapi.fabric.microsoft.com",
        "msitapi.fabric.microsoft.com",
    ] {
        let http = ScriptedHttp::new([response(200, r#"{"status":"Completed"}"#, &[])]);
        let credential: Arc<dyn TokenCredential> = Arc::new(TestCredential::default());
        let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap();
        let client = JobClient::with_context(
            RequestContext::with_endpoint(
                credential.clone(),
                CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
                endpoint.clone(),
            ),
            Arc::new(TestJobExecutor {
                http: HttpExecutor::new(
                    http.clone(),
                    Arc::new(TestTimer::default()),
                    ExecutionOptions::default(),
                )
                .unwrap()
                .with_endpoint(endpoint.clone()),
            }),
            JobLimits::default(),
        );
        assert!(Arc::ptr_eq(client.context().credential(), &credential));
        assert_eq!(client.context().endpoint(), &endpoint);
        let target = JobTarget::new("ws", "item", JobType::Notebook).unwrap();
        assert_eq!(
            client
                .status(&target, "run", pending())
                .await
                .unwrap()
                .status(),
            "Completed"
        );
        assert_eq!(
            http.requests.lock().unwrap()[0].url().as_str(),
            format!("https://{host}/v1/workspaces/ws/items/item/jobs/instances/run")
        );
    }
}

#[tokio::test]
async fn mismatched_audience_or_service_fails_before_credentials_or_http() {
    for (origin, audience) in [
        (ServiceOrigin::Fabric, Audience::OneLake),
        (ServiceOrigin::Fabric, Audience::Arm),
        (ServiceOrigin::OneLake, Audience::Fabric),
    ] {
        let http = ScriptedHttp::new([]);
        let credential = Arc::new(TestCredential::default());
        let endpoint = EndpointBinding::with_host(origin, "dailyapi.fabric.microsoft.com").unwrap();
        let client = JobClient::with_context(
            RequestContext::with_endpoint(
                credential.clone(),
                CredentialSelection::new("offline", "synthetic", audience).unwrap(),
                endpoint.clone(),
            ),
            Arc::new(TestJobExecutor {
                http: HttpExecutor::new(
                    http.clone(),
                    Arc::new(TestTimer::default()),
                    ExecutionOptions::default(),
                )
                .unwrap()
                .with_endpoint(endpoint),
            }),
            JobLimits::default(),
        );
        let error = client
            .status(
                &JobTarget::new("ws", "item", JobType::Notebook).unwrap(),
                "run",
                pending(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            fabric_sdk_jobs::JobError::InvalidAudience | fabric_sdk_jobs::JobError::InvalidEndpoint
        ));
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
        assert!(http.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_bound_run_handle_cannot_be_polled_using_a_different_client_endpoint() {
    let host = "dailyapi.fabric.microsoft.com";
    let location = format!("https://{host}/v1/workspaces/ws/items/item/jobs/instances/run");
    let http = ScriptedHttp::new([response(202, "", &[("Location", &location)])]);
    let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap();
    let client = JobClient::with_context(
        RequestContext::with_endpoint(
            Arc::new(TestCredential::default()),
            CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
            endpoint.clone(),
        ),
        Arc::new(TestJobExecutor {
            http: HttpExecutor::new(
                http.clone(),
                Arc::new(TestTimer::default()),
                ExecutionOptions::default(),
            )
            .unwrap()
            .with_endpoint(endpoint.clone()),
        }),
        JobLimits::default(),
    );
    let handle = client
        .submit(
            &JobTarget::new("ws", "item", JobType::Notebook).unwrap(),
            None,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(handle.endpoint(), &endpoint);
    let other_http = ScriptedHttp::new([]);
    let credential = Arc::new(TestCredential::default());
    let other = support::client(
        credential.clone(),
        other_http.clone(),
        Arc::new(TestTimer::default()),
        JobLimits::default(),
    );
    assert!(matches!(
        other
            .wait(
                &handle,
                fabric_sdk_jobs::WaitOptions::default(),
                |_| Ok(()),
                pending()
            )
            .await,
        Err(fabric_sdk_jobs::JobError::InvalidEndpoint)
    ));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(other_http.requests.lock().unwrap().is_empty());
}
