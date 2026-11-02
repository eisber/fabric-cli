// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The job adapter cannot fall back to a public origin from a different service binding.

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_cli_jobs::HttpJobExecutor;
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_jobs::{
    Audience, CredentialSelection, EndpointBinding, JobClient, JobError, JobLimits,
    JobRequestHeaders, JobTarget, JobType, ServiceOrigin,
};
use std::{
    future::pending,
    sync::{Arc, atomic::Ordering},
};
use support::*;

#[tokio::test]
async fn wrong_service_http_binding_cannot_use_header_public_origin_fallback() {
    for skill in [None, Some(""), Some("caller-skill")] {
        let http = ScriptedHttp::new([]);
        let credential = Arc::new(TestCredential::default());
        let wrong =
            EndpointBinding::with_host(ServiceOrigin::OneLake, "api.fabric.microsoft.com").unwrap();
        let executor = HttpJobExecutor::new(
            HttpExecutor::new(
                http.clone(),
                Arc::new(TestTimer::default()),
                ExecutionOptions::default(),
            )
            .unwrap()
            .with_endpoint(wrong.clone()),
        );
        assert_eq!(executor.endpoint(), &wrong);
        let client = JobClient::new(
            credential.clone(),
            CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
            Arc::new(executor),
            JobLimits::default(),
        )
        .with_request_headers(JobRequestHeaders::with_skill(skill).unwrap())
        .unwrap();
        assert!(matches!(
            client
                .status(
                    &JobTarget::new("ws", "item", JobType::Notebook).unwrap(),
                    "run",
                    pending()
                )
                .await,
            Err(JobError::Execution(_))
        ));
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        assert!(http.requests.lock().unwrap().is_empty());
    }
}
