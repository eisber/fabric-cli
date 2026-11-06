// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Injected host -> actual TableClient -> generic OneLake listing/reads, offline.

#![cfg(feature = "v2-discovery")]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli::v2_discovery::{
    DataSchemaProvider, DataSchemaRequest, DeltaDataSchemaProvider, DiscoveryDocument,
    DiscoveryFailure, DiscoveryFuture, DiscoveryHost, DiscoveryRequest, ParsedDiscovery,
    parse_from,
};
use fabric_cli_context::ItemType;
use fabric_cli_output::CommandError;
use fabric_cli_tables::{ResolvedTablePath, TableResolver};
use fabric_sdk_discovery::{Operation, TargetSelector, discover};
use fabric_sdk_onelake::{
    OneLakeError, OneLakeExecutionError, OneLakeExecutor, OneLakeMethod, OneLakeRequest,
    OneLakeResponse,
};
use fabric_sdk_tables::{
    Audience, CredentialSelection, LoadOutcome, TableClient, TableError, TableErrorKind,
    TableExecutor, TableFuture, TableLimits, TablePath, TableRequest, TableResponse,
    TableWithOneLake, schema_document::DataSchemaDocument,
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn, ready},
    io::{self, Write},
    num::NonZeroUsize,
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};

const TABLE: &str = "/Existing.Workspace/Book.Lakehouse/Tables/sales";

#[derive(Debug, Default)]
struct Credential(Mutex<Vec<String>>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0
            .lock()
            .unwrap()
            .extend(scopes.iter().map(|scope| (*scope).to_owned()));
        Ok(AccessToken {
            token: "offline-discovery-only".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

fn client(credential: Arc<Credential>) -> TableClient {
    TableClient::new(
        credential,
        &CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        &CredentialSelection::new("offline", "tenant", Audience::OneLake).unwrap(),
    )
    .unwrap()
}

struct Resolver {
    result: Result<ResolvedTablePath, CommandError>,
    calls: Mutex<Vec<(Vec<String>, bool)>>,
}

impl Resolver {
    fn new(kind: &str, path: &str) -> Self {
        Self {
            result: Ok(ResolvedTablePath {
                path: TablePath::new("ws", "item", path).unwrap(),
                item_type: ItemType::parse(kind).unwrap(),
            }),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl TableResolver for Resolver {
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
        must_exist: bool,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<ResolvedTablePath, CommandError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((path.to_owned(), must_exist));
            self.result.clone()
        })
    }
}

/// Panics if the read-only adapter ever attempts raw legacy reads or a load.
struct NoMutation;

impl TableExecutor for NoMutation {
    fn read<'a>(
        &'a self,
        _: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async { panic!("use the generic injected OneLake pager") })
    }

    fn load<'a>(
        &'a self,
        _: TableRequest<'a>,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async { panic!("schema discovery must never submit a mutation") })
    }
}

struct Storage {
    replies: Mutex<VecDeque<Result<OneLakeResponse, OneLakeExecutionError>>>,
    urls: Mutex<Vec<String>>,
}

impl Storage {
    fn new(replies: impl IntoIterator<Item = OneLakeResponse>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().map(Ok).collect()),
            urls: Mutex::new(Vec::new()),
        }
    }
}

impl OneLakeExecutor for Storage {
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> TableFuture<'a, Result<OneLakeResponse, OneLakeExecutionError>> {
        Box::pin(async move {
            assert_eq!(request.method(), OneLakeMethod::Get);
            assert!(request.body().is_empty());
            self.urls.lock().unwrap().push(request.url().to_string());
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("no unplanned endpoint probes or extra reads")
        })
    }
}

fn response(body: Value) -> OneLakeResponse {
    OneLakeResponse {
        status: 200,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn listing(commit: &str) -> OneLakeResponse {
    response(json!({
        "paths": [{"name": format!("item/Tables/sales/_delta_log/{commit}.json")}]
    }))
}

fn metadata(native: &Value) -> OneLakeResponse {
    response(json!({"metaData": {"schemaString": native.to_string()}}))
}

fn data_request() -> DiscoveryRequest {
    DiscoveryRequest::DataSchema(DataSchemaRequest::parse(TABLE).unwrap())
}

fn complete<T>(future: impl Future<Output = T>) -> T {
    future
        .now_or_never()
        .expect("offline scripted futures must be immediately ready")
}

fn parsed(args: &[&str]) -> DiscoveryRequest {
    let ParsedDiscovery::Request(request) =
        parse_from(std::iter::once("fab-cli").chain(args.iter().copied())).unwrap()
    else {
        panic!("request expected");
    };
    request
}

struct ForbiddenProvider;

impl DataSchemaProvider for ForbiddenProvider {
    fn max_output_bytes(&self) -> NonZeroUsize {
        panic!("operation discovery must not even consult the data output bound")
    }

    fn schema<'a>(
        &'a self,
        _: &'a DataSchemaRequest,
        _: DiscoveryFuture<'a, ()>,
    ) -> DiscoveryFuture<'a, Result<DataSchemaDocument, DiscoveryFailure>> {
        panic!("operation discovery must not call a data provider")
    }
}

#[test]
fn operation_discovery_does_not_consult_a_provider_or_execute_payload_validation() {
    let provider = ForbiddenProvider;
    let host = DiscoveryHost {
        data_schema: Some(&provider),
    };
    for (operation, path) in [
        (Operation::Create, "/Prospective.Workspace"),
        (Operation::Update, "/Existing.Workspace"),
        (Operation::Delete, "/Existing.Workspace"),
        (Operation::Run, "/Existing.Workspace/Analysis.Notebook"),
    ] {
        let request = parsed(&[operation.as_str(), path, "--schema"]);
        let document = complete(host.execute(&request, pending())).unwrap();
        let expected = discover(operation, &TargetSelector::parse(path).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(document).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            complete(host.run(
                ["fab-cli", operation.as_str(), path, "--schema"],
                pending(),
                &mut out,
                &mut err,
            ))
            .unwrap(),
            0
        );
        assert!(err.is_empty());
        assert_eq!(out.last(), Some(&b'\n'));
    }
    assert!(matches!(
        parse_from(["fab-cli", "create", "/Prospective.Workspace"]),
        Err(DiscoveryFailure::ExecutionNotImplemented)
    ));
}

#[test]
fn real_delta_adapter_reads_all_list_pages_before_selecting_native_metadata() {
    let native = json!({
        "type": "struct",
        "fields": [
            {"name": "required", "type": "long", "nullable": false},
            {
                "name": "nested 名",
                "type": {
                    "type": "array", "elementType": {"type": "struct", "fields": []},
                    "containsNull": false, "future": {"precision": 38}
                },
                "nullable": true,
                "metadata": {"delta.columnMapping.id": u64::MAX}
            },
            {"name": "unknown", "type": "future-native-type(7)"},
            {"name": "null", "type": "string", "nullable": null}
        ],
        "futureRoot": [null, false, 1]
    });
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let mut first = listing("00001");
    first
        .headers
        .insert("x-ms-continuation".into(), "next+/=".into());
    let storage = Storage::new([first, listing("00002"), metadata(&native)]);
    let executor = TableWithOneLake {
        tables: &NoMutation,
        storage: &storage,
    };
    let resolver = Resolver::new("Lakehouse", "Tables/sales");
    let provider = DeltaDataSchemaProvider {
        sdk: &sdk,
        executor: &executor,
        resolver: &resolver,
        limits: TableLimits::default(),
    };
    let host = DiscoveryHost {
        data_schema: Some(&provider),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        complete(host.run(["fab-cli", "schema", TABLE], pending(), &mut out, &mut err)).unwrap(),
        0
    );
    assert!(err.is_empty());
    let document: DataSchemaDocument = serde_json::from_slice(&out).unwrap();
    assert_eq!(document.native_schema, native);
    assert_eq!(document.columns[0].nullable, Some(false));
    assert_eq!(document.columns[1].nullable, Some(true));
    assert_eq!(document.columns[2].nullable, None);
    assert_eq!(document.columns[3].nullable, None);
    assert_eq!(document.columns[1].native_type, native["fields"][1]["type"]);
    assert_eq!(document.columns[2].native_type, "future-native-type(7)");
    assert_eq!(document.complete, None);
    assert!(!document.provenance.snapshot_consistency_established);
    assert_eq!(document.provenance.source_revision, None);
    assert!(document.coverage.json_commits_only);
    assert!(!document.coverage.checkpoints_reconstructed);
    let urls = storage.urls.lock().unwrap();
    assert_eq!(urls.len(), 3);
    assert!(urls[0].contains("directory=item%2FTables%2Fsales%2F_delta_log"));
    assert!(urls[1].contains("continuationToken=next%2B%2F%3D"));
    assert!(urls[2].ends_with("/ws/item/Tables/sales/_delta_log/00002.json"));
    assert_eq!(
        *resolver.calls.lock().unwrap(),
        [(vec![TABLE.to_owned()], true)]
    );
    assert_eq!(
        *credential.0.lock().unwrap(),
        vec![Audience::OneLake.scope().to_owned(); 3]
    );
}

#[test]
fn real_empty_native_fields_are_not_an_absent_or_unconfigured_provider() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential);
    let storage = Storage::new([listing("00001"), metadata(&json!({"fields": []}))]);
    let executor = TableWithOneLake {
        tables: &NoMutation,
        storage: &storage,
    };
    let resolver = Resolver::new("Lakehouse", "Tables/sales");
    let provider = DeltaDataSchemaProvider {
        sdk: &sdk,
        executor: &executor,
        resolver: &resolver,
        limits: TableLimits::default(),
    };
    let host = DiscoveryHost {
        data_schema: Some(&provider),
    };
    let DiscoveryDocument::Data(document) =
        complete(host.execute(&data_request(), pending())).unwrap()
    else {
        panic!("data document expected");
    };
    assert!(document.columns.is_empty());
    assert_eq!(document.native_schema, json!({"fields": []}));
    assert_eq!(document.complete, None);
    assert!(matches!(
        complete(DiscoveryHost::default().execute(&data_request(), pending())),
        Err(DiscoveryFailure::NotConfigured)
    ));
}

#[test]
fn boundary_quotes_are_rejected_before_resolver_credentials_or_reads() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let storage = Storage::new([]);
    let executor = TableWithOneLake {
        tables: &NoMutation,
        storage: &storage,
    };
    let resolver = Resolver::new("Lakehouse", "Tables/sales");
    let provider = DeltaDataSchemaProvider {
        sdk: &sdk,
        executor: &executor,
        resolver: &resolver,
        limits: TableLimits::default(),
    };
    let host = DiscoveryHost {
        data_schema: Some(&provider),
    };
    for path in [
        format!("'{TABLE}"),
        format!("\"{TABLE}"),
        format!("{TABLE}'"),
        format!("{TABLE}\""),
        format!("'{TABLE}'"),
        format!("\"{TABLE}\""),
        format!("'\"{TABLE}\"'"),
        "'Existing.Workspace/Book.Lakehouse/Tables/sales".to_owned(),
        "\"Existing.Workspace/Book.Lakehouse/Tables/sales".to_owned(),
    ] {
        assert!(matches!(
            DataSchemaRequest::parse(&path),
            Err(DiscoveryFailure::Unsupported)
        ));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            complete(host.run(
                ["fab-cli", "schema", path.as_str()],
                pending(),
                &mut out,
                &mut err,
            ))
            .unwrap(),
            1
        );
        assert!(out.is_empty());
        let error: Value = serde_json::from_slice(&err).unwrap();
        assert_eq!(error["error"]["code"], "unsupported");
        assert!(!String::from_utf8(err).unwrap().contains(&path));
    }
    assert!(resolver.calls.lock().unwrap().is_empty());
    assert!(credential.0.lock().unwrap().is_empty());
    assert!(storage.urls.lock().unwrap().is_empty());
}

#[test]
fn literal_internal_quotes_are_preserved_through_the_real_delta_adapter() {
    for table in ["sales'archive", "sales\"archive", "'sales", "\"sales"] {
        let path = format!("/Existing.Workspace/Book.Lakehouse/Tables/{table}");
        assert_eq!(DataSchemaRequest::parse(&path).unwrap().path(), path);
        let credential = Arc::new(Credential::default());
        let sdk = client(credential);
        let native = json!({"fields": []});
        let storage = Storage::new([
            response(json!({
                "paths": [{"name": format!("item/Tables/{table}/_delta_log/00001.json")}]
            })),
            metadata(&native),
        ]);
        let executor = TableWithOneLake {
            tables: &NoMutation,
            storage: &storage,
        };
        let resolver = Resolver::new("Lakehouse", &format!("Tables/{table}"));
        let provider = DeltaDataSchemaProvider {
            sdk: &sdk,
            executor: &executor,
            resolver: &resolver,
            limits: TableLimits::default(),
        };
        let host = DiscoveryHost {
            data_schema: Some(&provider),
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            complete(host.run(
                ["fab-cli", "schema", path.as_str()],
                pending(),
                &mut out,
                &mut err,
            ))
            .unwrap(),
            0
        );
        assert!(err.is_empty());
        let document: DataSchemaDocument = serde_json::from_slice(&out).unwrap();
        assert_eq!(document.native_schema, native);
        assert_eq!(*resolver.calls.lock().unwrap(), [(vec![path], true)]);
        let urls = storage.urls.lock().unwrap();
        assert_eq!(urls.len(), 2);
        let listing = azure_core::http::Url::parse(&urls[0]).unwrap();
        let directory = listing
            .query_pairs()
            .find(|(name, _)| name == "directory")
            .unwrap()
            .1;
        assert_eq!(directory, format!("item/Tables/{table}/_delta_log"));
    }
}

#[test]
fn resolved_backend_and_facet_are_rechecked_before_credentials_or_storage() {
    for (kind, path) in [
        ("Warehouse", "Tables/sales"),
        ("Notebook", "Tables/sales"),
        ("Lakehouse", "Files/sales"),
        ("Lakehouse", "Tables"),
        ("Lakehouse", "Tables/sales/extra/facet"),
        ("Lakehouse", "Tables/*"),
    ] {
        let credential = Arc::new(Credential::default());
        let sdk = client(credential.clone());
        let storage = Storage::new([]);
        let executor = TableWithOneLake {
            tables: &NoMutation,
            storage: &storage,
        };
        let resolver = Resolver::new(kind, path);
        let provider = DeltaDataSchemaProvider {
            sdk: &sdk,
            executor: &executor,
            resolver: &resolver,
            limits: TableLimits::default(),
        };
        let host = DiscoveryHost {
            data_schema: Some(&provider),
        };
        assert!(matches!(
            complete(host.execute(&data_request(), pending())),
            Err(DiscoveryFailure::Unsupported)
        ));
        assert!(credential.0.lock().unwrap().is_empty());
        assert!(storage.urls.lock().unwrap().is_empty());
    }
}

#[test]
fn resolver_failure_retains_classification_without_echoing_provider_text() {
    let sdk = client(Arc::new(Credential::default()));
    let error = CommandError::new(
        Some("private-provider-details".into()),
        Some("Forbidden".into()),
    );
    let resolver = Resolver {
        result: Err(error.clone()),
        calls: Mutex::new(Vec::new()),
    };
    let provider = DeltaDataSchemaProvider {
        sdk: &sdk,
        executor: &NoMutation,
        resolver: &resolver,
        limits: TableLimits::default(),
    };
    let host = DiscoveryHost {
        data_schema: Some(&provider),
    };
    let failure = complete(host.execute(&data_request(), pending())).unwrap_err();
    assert!(matches!(&failure, DiscoveryFailure::Resolver(actual) if actual == &error));
    assert_eq!(failure.code(), "resolution_failed");
    assert!(!failure.to_string().contains("private-"));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        complete(host.run(["fab-cli", "schema", TABLE], pending(), &mut out, &mut err)).unwrap(),
        1
    );
    assert!(out.is_empty());
    assert!(!String::from_utf8(err).unwrap().contains("private-"));
}

#[test]
fn cancellation_precedes_resolution_credentials_and_even_unconfigured_host_failure() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let resolver = Resolver::new("Lakehouse", "Tables/sales");
    let provider = DeltaDataSchemaProvider {
        sdk: &sdk,
        executor: &NoMutation,
        resolver: &resolver,
        limits: TableLimits::default(),
    };
    let request = DataSchemaRequest::parse(TABLE).unwrap();
    assert!(matches!(
        complete(provider.schema(&request, Box::pin(ready(())))),
        Err(DiscoveryFailure::Cancelled)
    ));
    let host = DiscoveryHost {
        data_schema: Some(&provider),
    };
    for host in [&host, &DiscoveryHost::default()] {
        assert!(matches!(
            complete(host.execute(&data_request(), ready(()))),
            Err(DiscoveryFailure::Cancelled)
        ));
    }
    assert!(resolver.calls.lock().unwrap().is_empty());
    assert!(credential.0.lock().unwrap().is_empty());
}

#[test]
fn failed_later_listing_page_is_typed_failure_never_partial_success() {
    let sdk = client(Arc::new(Credential::default()));
    let mut first = listing("00001");
    first
        .headers
        .insert("x-ms-continuation".into(), "next".into());
    let mut denied = response(json!({"message": "private-service-details"}));
    denied.status = 403;
    denied
        .headers
        .insert("x-ms-error-code".into(), "private-service-code".into());
    let storage = Storage::new([first, denied]);
    let executor = TableWithOneLake {
        tables: &NoMutation,
        storage: &storage,
    };
    let resolver = Resolver::new("Lakehouse", "Tables/sales");
    let provider = DeltaDataSchemaProvider {
        sdk: &sdk,
        executor: &executor,
        resolver: &resolver,
        limits: TableLimits::default(),
    };
    let failure = complete(provider.schema(
        &DataSchemaRequest::parse(TABLE).unwrap(),
        Box::pin(pending()),
    ))
    .unwrap_err();
    assert_eq!(failure.code(), "service_error");
    let DiscoveryFailure::Table(error) = failure else {
        panic!("typed SDK failure expected");
    };
    assert!(matches!(
        &error.kind,
        TableErrorKind::OneLake(storage)
        if matches!(storage.as_ref(), OneLakeError::Http { status: 403, .. })
    ));
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(storage.urls.lock().unwrap().len(), 2);
}

#[test]
fn repeated_continuation_and_typed_executor_failures_remain_non_success() {
    let sdk = client(Arc::new(Credential::default()));
    let mut repeated = listing("00001");
    repeated
        .headers
        .insert("x-ms-continuation".into(), "same-token".into());
    let replies = vec![Ok(repeated.clone()), Ok(repeated)];
    for (replies, expected) in [
        (replies, "repeated_continuation"),
        (
            vec![Err(OneLakeExecutionError::ResponseLimit)],
            "limit_exceeded",
        ),
        (vec![Err(OneLakeExecutionError::TimedOut)], "timed_out"),
        (vec![Err(OneLakeExecutionError::Cancelled)], "cancelled"),
        (vec![Err(OneLakeExecutionError::Failed)], "execution_failed"),
    ] {
        let storage = Storage {
            replies: Mutex::new(replies.into()),
            urls: Mutex::new(Vec::new()),
        };
        let executor = TableWithOneLake {
            tables: &NoMutation,
            storage: &storage,
        };
        let resolver = Resolver::new("Lakehouse", "Tables/sales");
        let provider = DeltaDataSchemaProvider {
            sdk: &sdk,
            executor: &executor,
            resolver: &resolver,
            limits: TableLimits::default(),
        };
        let host = DiscoveryHost {
            data_schema: Some(&provider),
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            complete(host.run(["fab-cli", "schema", TABLE], pending(), &mut out, &mut err))
                .unwrap(),
            1
        );
        assert!(out.is_empty());
        let error: Value = serde_json::from_slice(&err).unwrap();
        assert_eq!(error["error"]["code"], expected);
    }
}

#[test]
fn read_limits_and_malformed_or_checkpoint_only_metadata_never_return_success() {
    let sdk = client(Arc::new(Credential::default()));
    for (replies, limits, expected) in [
        (
            vec![listing("00001")],
            TableLimits {
                max_requests: NonZeroUsize::new(1).unwrap(),
                ..TableLimits::default()
            },
            "limit_exceeded",
        ),
        (
            vec![listing("00001")],
            TableLimits {
                max_response_bytes: NonZeroUsize::new(8).unwrap(),
                ..TableLimits::default()
            },
            "limit_exceeded",
        ),
        (
            vec![response(json!({"paths": [
                {"name": "item/Tables/sales/_delta_log/00001.json"},
                {"name": "item/Tables/sales/_delta_log/00002.json"}
            ]}))],
            TableLimits {
                max_logs: NonZeroUsize::new(1).unwrap(),
                ..TableLimits::default()
            },
            "limit_exceeded",
        ),
        (
            vec![response(json!({"paths": [
                {"name": "item/Tables/sales/_delta_log/00001.checkpoint.parquet"}
            ]}))],
            TableLimits::default(),
            "invalid_delta_table",
        ),
        (
            vec![
                listing("00001"),
                metadata(&json!({"fields": "not-an-array"})),
            ],
            TableLimits::default(),
            "invalid_response",
        ),
    ] {
        let storage = Storage::new(replies);
        let executor = TableWithOneLake {
            tables: &NoMutation,
            storage: &storage,
        };
        let resolver = Resolver::new("Lakehouse", "Tables/sales");
        let provider = DeltaDataSchemaProvider {
            sdk: &sdk,
            executor: &executor,
            resolver: &resolver,
            limits,
        };
        let host = DiscoveryHost {
            data_schema: Some(&provider),
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            complete(host.run(["fab-cli", "schema", TABLE], pending(), &mut out, &mut err))
                .unwrap(),
            1
        );
        assert!(out.is_empty());
        let error: Value = serde_json::from_slice(&err).unwrap();
        assert_eq!(error["error"]["code"], expected);
    }
}

#[test]
fn serialized_output_bound_rejects_an_entire_real_document_without_clipping() {
    let sdk = client(Arc::new(Credential::default()));
    let storage = Storage::new([listing("00001"), metadata(&json!({"fields": []}))]);
    let executor = TableWithOneLake {
        tables: &NoMutation,
        storage: &storage,
    };
    let resolver = Resolver::new("Lakehouse", "Tables/sales");
    let provider = DeltaDataSchemaProvider {
        sdk: &sdk,
        executor: &executor,
        resolver: &resolver,
        limits: TableLimits {
            max_response_bytes: NonZeroUsize::new(180).unwrap(),
            ..TableLimits::default()
        },
    };
    let host = DiscoveryHost {
        data_schema: Some(&provider),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        complete(host.run(["fab-cli", "schema", TABLE], pending(), &mut out, &mut err)).unwrap(),
        1
    );
    assert!(out.is_empty());
    let error: Value = serde_json::from_slice(&err).unwrap();
    assert_eq!(error["error"]["code"], "output_limit_exceeded");
    assert_eq!(storage.urls.lock().unwrap().len(), 2);
}

struct DropFlag<'a>(&'a AtomicBool);

impl Drop for DropFlag<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct PendingStorage {
    entered: AtomicBool,
    dropped: AtomicBool,
}

impl OneLakeExecutor for PendingStorage {
    fn execute<'a>(
        &'a self,
        _: OneLakeRequest<'a>,
    ) -> TableFuture<'a, Result<OneLakeResponse, OneLakeExecutionError>> {
        Box::pin(async move {
            let _guard = DropFlag(&self.dropped);
            self.entered.store(true, Ordering::SeqCst);
            pending().await
        })
    }
}

#[test]
fn mid_read_cancellation_drops_the_pending_storage_future_without_a_result() {
    let sdk = client(Arc::new(Credential::default()));
    let storage = PendingStorage {
        entered: AtomicBool::new(false),
        dropped: AtomicBool::new(false),
    };
    let executor = TableWithOneLake {
        tables: &NoMutation,
        storage: &storage,
    };
    let resolver = Resolver::new("Lakehouse", "Tables/sales");
    let provider = DeltaDataSchemaProvider {
        sdk: &sdk,
        executor: &executor,
        resolver: &resolver,
        limits: TableLimits::default(),
    };
    let host = DiscoveryHost {
        data_schema: Some(&provider),
    };
    let request = data_request();
    let cancellation = poll_fn(|_| {
        if storage.entered.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut work = pin!(host.execute(&request, cancellation));
    let mut context = Context::from_waker(Waker::noop());
    assert!(work.as_mut().poll(&mut context).is_pending());
    assert!(storage.entered.load(Ordering::SeqCst));
    assert!(matches!(
        work.as_mut().poll(&mut context),
        Poll::Ready(Err(DiscoveryFailure::Cancelled))
    ));
    assert!(storage.dropped.load(Ordering::SeqCst));
}

struct BrokenWriter {
    fail_flush: bool,
}

impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_flush {
            Ok(bytes.len())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "scripted write failure",
            ))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "scripted flush failure",
        ))
    }
}

#[test]
fn stdout_stderr_and_flush_failures_propagate_instead_of_success() {
    let host = DiscoveryHost::default();
    for fail_flush in [false, true] {
        let mut writer = BrokenWriter { fail_flush };
        let mut other = Vec::new();
        for args in [
            vec!["fab-cli", "create", "/Prospective.Workspace", "--schema"],
            vec!["fab-cli", "--help"],
            vec!["fab-cli", "--version"],
        ] {
            let error = complete(host.run(args, pending(), &mut writer, &mut other)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
            assert!(other.is_empty());
        }
        let error = complete(host.run(
            ["fab-cli", "schema", TABLE],
            pending(),
            &mut other,
            &mut writer,
        ))
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert!(other.is_empty());
    }
}
