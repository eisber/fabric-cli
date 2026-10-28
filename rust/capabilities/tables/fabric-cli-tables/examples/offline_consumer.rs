// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standalone public consumer: registration -> handler -> SDK -> shared HTTP -> output.
//! Every boundary below is synthetic; no credential store or service is accessed.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_context::ItemType;
use fabric_cli_output::{CommandError, Timestamp};
use fabric_cli_tables::*;
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_operations::{OperationLimits, OperationWarning};
use fabric_sdk_tables::*;
use std::{
    future::pending,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Debug)]
struct OfflineCredential;
#[async_trait]
impl TokenCredential for OfflineCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Ok(AccessToken {
            token: "offline-example-only".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}
#[derive(Debug, Default)]
struct SyntheticNetwork(AtomicUsize);
#[async_trait]
impl HttpClient for SyntheticNetwork {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        let index = self.0.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            assert_eq!(request.method().to_string(), "HEAD");
            assert_eq!(
                request.url().host_str(),
                Some("onelake.dfs.fabric.microsoft.com")
            );
            assert_eq!(request.url().path(), "/ws/item/Files/input.csv");
            let mut headers = Headers::new();
            headers.insert("x-ms-resource-type", "file");
            return Ok(AsyncRawResponse::from_bytes(
                StatusCode::Ok,
                headers,
                Vec::new(),
            ));
        }
        if index == 1 {
            assert_eq!(request.method().to_string(), "POST");
            assert_eq!(
                request.url().path(),
                "/v1/workspaces/ws/lakehouses/item/tables/sales/load"
            );
            let mut headers = Headers::new();
            headers.insert("x-ms-operation-id", "22222222-2222-2222-2222-222222222222");
            headers.insert("retry-after", "0");
            return Ok(AsyncRawResponse::from_bytes(
                StatusCode::Accepted,
                headers,
                Vec::new(),
            ));
        }
        assert!(index <= 3, "unexpected replay or extra poll");
        assert_eq!(request.method().to_string(), "GET");
        assert_eq!(
            request.url().path(),
            "/v1/operations/22222222-2222-2222-2222-222222222222"
        );
        Ok(AsyncRawResponse::from_bytes(
            StatusCode::Ok,
            Headers::new(),
            if index == 2 {
                br#"{"status":"Running"}"#.to_vec()
            } else {
                br#"{"status":"Succeeded"}"#.to_vec()
            },
        ))
    }
}
struct OfflineTimer;
impl ExecutionTimer for OfflineTimer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == Duration::from_secs(240) {
            Box::pin(pending())
        } else {
            Box::pin(async {})
        }
    }
}
struct UnexpectedRawStorage;
impl StorageReads for UnexpectedRawStorage {
    fn read<'a>(
        &'a self,
        _: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async { panic!("Selected generic OneLake SDK must handle every storage read") })
    }
}
struct UnexpectedLifecycle;
struct OperationWarnings;
impl TableOperationObserver for OperationWarnings {
    fn warning(&self, warning: OperationWarning) -> Result<(), TableError> {
        panic!("fixture has no invalid polling headers: {warning:?}")
    }
}
impl MaintenanceExecutor for UnexpectedLifecycle {
    fn run<'a>(
        &'a self,
        _: &'a TablePath,
        _: serde_json::Value,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<MaintenanceOutcome, TableError>> {
        Box::pin(async { panic!("This load fixture must not invoke a maintenance job") })
    }
}
struct Snapshot;
impl TableResolver for Snapshot {
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
        must_exist: bool,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<ResolvedTablePath, CommandError>> {
        Box::pin(async move {
            assert!(!must_exist);
            assert!(matches!(
                path.join(" ").as_str(),
                "Tables/sales" | "Files/input.csv"
            ));
            Ok(ResolvedTablePath {
                path: TablePath::new("ws", "item", path.join(" ")).unwrap(),
                item_type: ItemType::parse("Lakehouse").unwrap(),
            })
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sdk = TableClient::new(
        Arc::new(OfflineCredential),
        &CredentialSelection::new("offline", "tenant", Audience::Fabric)?,
        &CredentialSelection::new("offline", "tenant", Audience::OneLake)?,
    )?;
    let network = Arc::new(SyntheticNetwork::default());
    let http = Arc::new(HttpExecutor::new(
        network.clone(),
        Arc::new(OfflineTimer),
        ExecutionOptions::default(),
    )?);
    let operations = FabricLoadCompletion {
        http: &http,
        limits: OperationLimits::default(),
        observer: &OperationWarnings,
    };
    let load_executor = SharedTableExecutor {
        http: &http,
        storage: &UnexpectedRawStorage,
        operations: &operations,
    };
    let storage = OneLakeHttpExecutor::new(http.clone());
    let executor = TableWithOneLake {
        tables: &load_executor,
        storage: &storage,
    };
    let host = TableHost {
        sdk: &sdk,
        executor: &executor,
        jobs: &UnexpectedLifecycle,
        resolver: &Snapshot,
        output_format: "json",
        text: None,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z")?,
        limits: TableLimits::default(),
    };
    let matches = command().try_get_matches_from([
        "table",
        "load",
        "Tables/sales",
        "--file",
        "Files/input.csv",
    ])?;
    let command = TableCommand::from_matches(&matches).expect("explicit load leaf");
    let exit = host
        .run(
            &command,
            pending(),
            &mut std::io::stdout().lock(),
            &mut std::io::stderr(),
        )
        .await?;
    assert_eq!(exit, 0);
    assert_eq!(
        network.0.load(Ordering::SeqCst),
        4,
        "one source HEAD, one POST and two GET polls"
    );
    Ok(())
}
