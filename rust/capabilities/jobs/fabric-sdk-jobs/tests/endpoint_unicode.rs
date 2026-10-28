// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Preserve existing opaque identifier encoding while introducing context routing.

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use azure_core::http::Url;
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_jobs::{
    Audience, CredentialSelection, EndpointBinding, JobClient, JobLimits, JobTarget, JobType,
    RequestContext, ServiceOrigin,
};
use std::{future::pending, sync::Arc};
use support::*;

#[tokio::test]
async fn context_routing_preserves_existing_unicode_identifier_url_encoding() {
    for host in ["api.fabric.microsoft.com", "dailyapi.fabric.microsoft.com"] {
        let http = ScriptedHttp::new([response(200, r#"{"status":"Completed"}"#, &[])]);
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
                .with_endpoint(endpoint),
            }),
            JobLimits::default(),
        );
        client
            .status(
                &JobTarget::new("café", "é", JobType::Notebook).unwrap(),
                "λ",
                pending(),
            )
            .await
            .unwrap();
        let original = Url::parse(&format!(
            "https://{host}/v1/workspaces/café/items/é/jobs/instances/λ"
        ))
        .unwrap();
        assert_eq!(http.requests.lock().unwrap()[0].url(), &original);
    }
}
