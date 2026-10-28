// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller-selected job endpoints over the existing shared HTTP executor.

#[allow(dead_code)]
#[path = "../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_jobs::{
    Audience, CredentialSelection, EndpointBinding, JobClient, JobLimits, JobTarget, JobType,
    RequestContext, ServiceOrigin, TokenCredential,
};
use std::{future::pending, sync::Arc};
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
