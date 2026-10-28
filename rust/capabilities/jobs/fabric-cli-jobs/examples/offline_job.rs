// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent, secret-free composition of actual job handlers and shared HTTP.
//! Synthetic time/HTTP only; this example never contacts Fabric or an identity service.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_context::{Catalog, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use fabric_cli_jobs::{
    Confirmation, HttpJobExecutor, JobConfig, JobEvent, JobHost, JobUi, NoFiles,
    command_from_matches, job_subcommand,
};
use fabric_cli_output::{CommandError, Timestamp};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_jobs::{Audience, CredentialSelection, JobClient, JobLimits};
use std::{
    future::pending,
    io::Write,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Debug)]
struct SyntheticCredential;

#[async_trait]
impl TokenCredential for SyntheticCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        Ok(AccessToken {
            token: "offline-example-only".into(),
            expires_on: OffsetDateTime::from_unix_timestamp(1_900_000_000).unwrap(),
        })
    }
}

#[derive(Debug, Default)]
struct SyntheticHttp(Mutex<usize>);

#[async_trait]
impl HttpClient for SyntheticHttp {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        let mut calls = self.0.lock().unwrap();
        let mut headers = Headers::new();
        let response = match (*calls, request.method(), request.url().path()) {
            (0, Method::Post, "/v1/workspaces/ws/items/item/jobs/instances") => {
                assert_eq!(request.url().query(), Some("jobType=Pipeline"));
                headers.insert("location", "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances/run");
                AsyncRawResponse::from_bytes(StatusCode::Accepted, headers, Vec::new())
            }
            (1, Method::Get, "/v1/workspaces/ws/items/item/jobs/instances/run") => {
                AsyncRawResponse::from_bytes(
                    StatusCode::Ok,
                    headers,
                    br#"{"id":"run","status":"Completed"}"#.to_vec(),
                )
            }
            _ => {
                return Err(azure_core::Error::new(
                    azure_core::error::ErrorKind::Other,
                    "unexpected synthetic request",
                ));
            }
        };
        *calls += 1;
        Ok(response)
    }
}

struct SyntheticTime;

impl ExecutionTimer for SyntheticTime {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(async move {
            if delay == Duration::from_secs(240) {
                pending::<()>().await;
            }
        })
    }
}

struct Console;

impl JobUi for Console {
    fn emit(&self, event: JobEvent) -> Result<(), CommandError> {
        let text = match event {
            JobEvent::Grey(text) | JobEvent::Progress(text) | JobEvent::LogWarning(text) => text,
            JobEvent::Warning(text) => format!("! {text}"),
            JobEvent::Table(_) => {
                return Err(CommandError::unexpected(Some(
                    "unexpected table in the bounded completed-run example".into(),
                )));
            }
        };
        writeln!(std::io::stderr(), "{text}")
            .map_err(|error| CommandError::unexpected(Some(error.to_string())))
    }
    fn confirm(&self) -> Confirmation<'_> {
        Box::pin(async { Ok(false) })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let http = Arc::new(SyntheticHttp::default());
    let executor = HttpExecutor::new(
        http.clone(),
        Arc::new(SyntheticTime),
        ExecutionOptions::default(),
    )?;
    let sdk = JobClient::new(
        Arc::new(SyntheticCredential),
        CredentialSelection::new("offline-account", "synthetic-tenant", Audience::Fabric)?,
        Arc::new(HttpJobExecutor::new(executor)),
        JobLimits::default(),
    );
    let tenant = Element::tenant("synthetic", "tenant");
    let workspace = Element::child(
        "demo",
        Some("ws".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        tenant,
    )?;
    let item = Element::child(
        "pipeline",
        Some("item".into()),
        NodeKind::Item(ItemType::parse("DataPipeline")?),
        workspace.clone(),
    )?;
    let catalog = Catalog::new(vec![workspace.clone(), item]);
    let host = JobHost {
        sdk: &sdk,
        operations: None,
        lookup: &catalog,
        current: workspace,
        home: HomeContext::Unspecified,
        files: &NoFiles,
        ui: &Console,
        config: JobConfig::default(),
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z")?,
    };
    let matches = job_subcommand().try_get_matches_from(["job", "run", "pipeline.DataPipeline"])?;
    let command =
        command_from_matches(&matches).expect("the static example uses valid job arguments");
    let exit = host
        .run_json(
            &command,
            pending(),
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        )
        .await?;
    assert_eq!(exit, 0);
    assert_eq!(*http.0.lock().unwrap(), 2);
    Ok(())
}
