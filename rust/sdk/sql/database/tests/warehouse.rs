// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native Warehouse management reads; no live provider or database access.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{credentials::AccessToken, time::OffsetDateTime};
use fabric_core::{Audience, CredentialSelection, TokenCredential, TokenRequestOptions};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use fabric_sdk_sql_database::warehouse::{
    ConnectionOptions, ListWarehouses, PrivateLinkType, RestorePointId, WarehouseError,
    WarehouseId, WarehousePermission, WarehouseRead, WarehouseRequests, WarehouseTarget,
};
use fabric_sdk_sql_database::{
    ContinuationToken, FolderScope, Observed, PageCoverage, SqlResponseErrorKind,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod, ResourceOutcome};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{Future, pending, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const WAREHOUSE: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OTHER: &str = "00000000-0000-0000-0000-000000000000";

fn target() -> WarehouseTarget {
    WarehouseTarget::new(
        WORKSPACE.parse().unwrap(),
        WAREHOUSE.parse().unwrap(),
        "Warehouse",
    )
    .unwrap()
}

fn row() -> Value {
    json!({"id":WAREHOUSE,"workspaceId":WORKSPACE,"type":"Warehouse","displayName":"not-a-catalog"})
}

fn completed(data: Value) -> ResourceOutcome {
    ResourceOutcome::Completed {
        status: 200,
        data: Some(data),
    }
}

fn error_kind<T>(result: Result<T, WarehouseError>) -> SqlResponseErrorKind {
    match result {
        Err(WarehouseError::Response(error)) => error.kind,
        _ => panic!("expected response failure"),
    }
}

#[test]
fn native_ids_and_exact_owner_reject_other_sql_artifacts() {
    assert_eq!(
        WAREHOUSE
            .to_uppercase()
            .parse::<WarehouseId>()
            .unwrap()
            .to_string(),
        WAREHOUSE
    );
    for invalid in [
        "",
        "../escape",
        "Warehouse",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee ",
        OTHER.strip_prefix('0').unwrap(),
    ] {
        assert!(invalid.parse::<WarehouseId>().is_err());
    }
    for owner in [
        "SQLDatabase",
        "WarehouseSnapshot",
        "SQLEndpoint",
        "Lakehouse",
        "warehouse",
        "Warehouse ",
    ] {
        assert_eq!(
            WarehouseTarget::new(
                WORKSPACE.parse().unwrap(),
                WAREHOUSE.parse().unwrap(),
                owner
            ),
            Err(WarehouseError::WrongOwner),
        );
    }
    for id in ["", ".", "..", "a/b", "a\\b", "a\n", "a\0"] {
        assert!(RestorePointId::new(id).is_err());
    }
    for id in ["1726617378000", "future-key", "a?b#c%2F+"] {
        assert_eq!(RestorePointId::new(id).unwrap().as_str(), id);
    }
}

#[test]
fn every_read_is_get_without_body_wait_or_mutation_inventory() {
    assert_eq!(WarehouseRead::ALL.len(), 5);
    for read in WarehouseRead::ALL {
        assert_eq!(read.method(), ResourceMethod::Get);
        assert!(!read.delegated_scopes().contains(&"Warehouse.Restore.All"));
        assert!(!read.delegated_scopes().contains(&"Item.Audit.All"));
    }
    let get = WarehouseRequests::get(&target()).unwrap();
    assert_eq!(
        get.operation().path(),
        format!("/v1/workspaces/{WORKSPACE}/warehouses/{WAREHOUSE}")
    );
    assert_eq!(get.operation().method(), ResourceMethod::Get);
    assert!(get.operation().body().is_none());
    assert_eq!(
        get.operation().completion(),
        CompletionPolicy::ReturnAccepted
    );
    assert_eq!(get.read().permission(), WarehousePermission::ItemRead);

    let connection = WarehouseRequests::connection(
        &target(),
        ConnectionOptions {
            guest_tenant_id: Some(OTHER.parse().unwrap()),
            private_link_type: Some(PrivateLinkType::Workspace),
        },
    )
    .unwrap();
    assert_eq!(
        connection.operation().path(),
        format!(
            "/v1/workspaces/{WORKSPACE}/warehouses/{WAREHOUSE}/connectionString?guestTenantId={OTHER}&privateLinkType=Workspace"
        ),
    );
    assert_eq!(
        connection.read().permission(),
        WarehousePermission::ItemReadAndWorkspaceViewer
    );
    let public = WarehouseRequests::connection(
        &target(),
        ConnectionOptions {
            private_link_type: Some(PrivateLinkType::None),
            ..ConnectionOptions::default()
        },
    )
    .unwrap();
    assert!(public.operation().path().ends_with("?privateLinkType=None"));
    let omitted = WarehouseRequests::connection(&target(), ConnectionOptions::default()).unwrap();
    assert!(omitted.operation().path().ends_with("/connectionString"));

    let point =
        WarehouseRequests::restore_point(&target(), &RestorePointId::new("a?b#c%2F+").unwrap())
            .unwrap();
    assert_eq!(
        point.operation().path(),
        format!("/v1/workspaces/{WORKSPACE}/warehouses/{WAREHOUSE}/restorePoints/a%3Fb%23c%252F+")
    );
    assert!(point.operation().body().is_none());
    assert_eq!(
        point.operation().completion(),
        CompletionPolicy::ReturnAccepted
    );
}

#[test]
fn page_descriptors_encode_native_continuation_and_explicit_folder_choices() {
    for recursive in [false, true] {
        let list = WarehouseRequests::list_page(
            &WORKSPACE.parse().unwrap(),
            &ListWarehouses {
                recursive,
                root_folder: FolderScope::Workspace,
                continuation_token: None,
            },
        )
        .unwrap();
        assert_eq!(
            list.operation().path(),
            format!("/v1/workspaces/{WORKSPACE}/warehouses?recursive={recursive}")
        );
        assert_eq!(
            list.read().delegated_scopes(),
            ["Workspace.Read.All", "Workspace.ReadWrite.All"]
        );
    }
    let list = WarehouseRequests::list_page(
        &WORKSPACE.parse().unwrap(),
        &ListWarehouses {
            recursive: false,
            root_folder: FolderScope::Folder(OTHER.parse().unwrap()),
            continuation_token: Some(ContinuationToken::new("a+b/%2F &?=").unwrap()),
        },
    )
    .unwrap();
    assert_eq!(
        list.operation().path(),
        format!(
            "/v1/workspaces/{WORKSPACE}/warehouses?recursive=false&rootFolderId={OTHER}&continuationToken=a%2Bb%2F%252F+%26%3F%3D"
        )
    );
    let token = ContinuationToken::new("a+b/%2F &?=").unwrap();
    let points = WarehouseRequests::restore_points_page(&target(), Some(&token)).unwrap();
    assert_eq!(
        points.operation().path(),
        format!(
            "/v1/workspaces/{WORKSPACE}/warehouses/{WAREHOUSE}/restorePoints?continuationToken=a%2Bb%2F%252F+%26%3F%3D"
        )
    );
}

#[test]
fn metadata_checks_required_fields_native_shapes_and_foreign_owners() {
    let request = WarehouseRequests::get(&target()).unwrap();
    for field in ["id", "workspaceId", "displayName", "type"] {
        let mut missing = row();
        missing.as_object_mut().unwrap().remove(field);
        assert_eq!(
            error_kind(request.decode(completed(missing))),
            SqlResponseErrorKind::InvalidShape
        );
        let mut null = row();
        null[field] = Value::Null;
        assert_eq!(
            error_kind(request.decode(completed(null))),
            SqlResponseErrorKind::InvalidShape
        );
    }
    for (field, value) in [
        ("type", json!("WarehouseSnapshot")),
        ("properties", json!([])),
        ("properties", json!({"connectionString":[]})),
        ("folderId", json!("folder")),
    ] {
        let mut invalid = row();
        invalid[field] = value;
        assert_eq!(
            error_kind(request.decode(completed(invalid))),
            SqlResponseErrorKind::InvalidShape
        );
    }
    for field in ["id", "workspaceId"] {
        let mut foreign = row();
        foreign[field] = json!(OTHER);
        foreign["logicalId"] = json!(WAREHOUSE);
        assert_eq!(
            error_kind(request.decode(completed(foreign))),
            SqlResponseErrorKind::IdentityMismatch
        );
    }
    let mut data = row();
    data["description"] = Value::Null;
    data["properties"] =
        json!({"connectionString":"inert-sensitive-host","createdDate":null,"future":{}});
    let result = request.decode(completed(data)).unwrap();
    assert_eq!(result.item_type(), "Warehouse");
    assert_eq!(result.description, Observed::Null);
    assert_eq!(result.folder_id, Observed::Missing);
    match result.properties {
        Observed::Value(properties) => {
            assert_eq!(properties.created_date, Observed::Null);
            assert_eq!(properties.last_updated_time, Observed::Missing);
            assert!(!format!("{properties:?}").contains("inert-sensitive-host"));
        }
        _ => panic!("properties missing"),
    }
}

#[test]
fn native_connection_is_inert_and_request_owner_is_explicit_not_a_catalog_guess() {
    let request = WarehouseRequests::connection(&target(), ConnectionOptions::default()).unwrap();
    let result = request
        .decode(completed(
            json!({"connectionString":"native-host.datawarehouse.fabric.microsoft.com"}),
        ))
        .unwrap();
    assert_eq!(result.owner, target());
    assert_eq!(
        result.data.connection_string.as_str(),
        "native-host.datawarehouse.fabric.microsoft.com"
    );
    assert!(!format!("{result:?}").contains("native-host"));
    for data in [
        json!(["native-host"]),
        json!({}),
        json!({"connectionString":null}),
        json!({"connectionString":""}),
        json!({"connectionString":" \t"}),
        json!({"connectionString":123}),
        json!({"databaseName":"not-a-catalog"}),
    ] {
        assert_eq!(
            error_kind(request.decode(completed(data))),
            SqlResponseErrorKind::InvalidShape
        );
    }
}

#[test]
fn restore_point_ids_and_events_stay_native_without_expiry_or_newest_inference() {
    let id = RestorePointId::new("future-key").unwrap();
    let request = WarehouseRequests::restore_point(&target(), &id).unwrap();
    let result = request.decode(completed(json!({
        "id":"future-key","displayName":null,"creationMode":"FutureMode",
        "creationDetails":{"eventDateTime":"uninterpreted","eventInitiator":null,"future":{"x":1}},
    }))).unwrap();
    assert_eq!(result.owner, target());
    assert_eq!(result.data.id, id);
    assert_eq!(result.data.display_name, Observed::Null);
    assert_eq!(result.data.description, Observed::Missing);
    assert_eq!(
        result.data.creation_mode,
        Observed::Value("FutureMode".into())
    );
    match result.data.creation_details {
        Observed::Value(details) => assert_eq!(details["future"], json!({"x":1})),
        _ => panic!("missing event"),
    }
    assert_eq!(
        error_kind(request.decode(completed(json!({"id":"newer"})))),
        SqlResponseErrorKind::IdentityMismatch
    );
    for data in [
        json!({}),
        json!({"id":123}),
        json!({"id":"future-key","creationDetails":[]}),
        json!({"id":"future-key","creationMode":3}),
    ] {
        assert_eq!(
            error_kind(request.decode(completed(data))),
            SqlResponseErrorKind::InvalidShape
        );
    }
}

#[test]
fn pages_preserve_order_duplicates_and_incomplete_nonresumable_uri_evidence() {
    let request = WarehouseRequests::restore_points_page(&target(), None).unwrap();
    let page = request
        .decode(completed(json!({
            "value":[{"id":"z"},{"id":"a"},{"id":"z"}],
            "continuationToken":null,"continuationUri":"https://foreign.invalid/never-follow",
        })))
        .unwrap();
    assert_eq!(page.owner, target());
    assert_eq!(
        page.data
            .value
            .iter()
            .map(|point| point.id.as_str())
            .collect::<Vec<_>>(),
        ["z", "a", "z"]
    );
    assert_eq!(page.data.coverage(), PageCoverage::Incomplete);
    assert!(!format!("{page:?}").contains("foreign.invalid"));
    let empty = request.decode(completed(json!({"value":[]}))).unwrap();
    assert_eq!(empty.data.coverage(), PageCoverage::TerminalPage);
    for data in [
        json!({}),
        json!({"value":null}),
        json!({"value":[{}]}),
        json!({"value":[],"continuationToken":false}),
    ] {
        assert_eq!(
            error_kind(request.decode(completed(data))),
            SqlResponseErrorKind::InvalidShape
        );
    }
    let list = WarehouseRequests::list_page(
        &WORKSPACE.parse().unwrap(),
        &ListWarehouses {
            recursive: false,
            root_folder: FolderScope::Workspace,
            continuation_token: None,
        },
    )
    .unwrap();
    let mut foreign = row();
    foreign["workspaceId"] = json!(OTHER);
    assert_eq!(
        error_kind(list.decode(completed(json!({"value":[row(),foreign]})))),
        SqlResponseErrorKind::IdentityMismatch
    );
}

#[test]
fn acceptance_and_non_native_statuses_are_never_metadata_success() {
    let request = WarehouseRequests::get(&target()).unwrap();
    for handle in [Some("native-operation".into()), None] {
        match request.decode(ResourceOutcome::Accepted {
            operation_id: handle.clone(),
        }) {
            Err(WarehouseError::Response(error)) => {
                assert_eq!(error.kind, SqlResponseErrorKind::UnexpectedAcceptance);
                assert_eq!(error.http_status, 202);
                assert_eq!(error.operation_id, handle);
            }
            _ => panic!("acceptance claimed success"),
        }
    }
    for status in [201, 202, 204, 206, 401, 403, 404, 429, 500] {
        assert_eq!(
            error_kind(request.decode(ResourceOutcome::Completed {
                status,
                data: Some(row())
            })),
            SqlResponseErrorKind::UnexpectedStatus
        );
    }
    assert_eq!(
        error_kind(request.decode(ResourceOutcome::Completed {
            status: 200,
            data: None
        })),
        SqlResponseErrorKind::MissingBody
    );
}

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    fail: bool,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Other,
                "sensitive-provider-detail",
            ));
        }
        Ok(AccessToken::new(
            "offline-synthetic-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Clone)]
struct Executor {
    response: OperationResponse,
    requests: Arc<Mutex<Vec<String>>>,
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        assert_eq!(request.method(), ResourceMethod::Get);
        assert!(request.body().is_empty());
        self.requests
            .lock()
            .unwrap()
            .push(request.url().to_string());
        Box::pin(ready(Ok(self.response.clone())))
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("Warehouse read must not poll or retry")
    }
}

fn executor(status: u16, data: Value) -> Executor {
    Executor {
        response: OperationResponse {
            status,
            body: serde_json::to_vec(&data).unwrap(),
            headers: BTreeMap::new(),
        },
        requests: Arc::default(),
    }
}

fn limits() -> OperationLimits {
    OperationLimits {
        max_requests: NonZeroUsize::new(1).unwrap(),
        max_response_bytes: NonZeroUsize::new(16_384).unwrap(),
        max_poll_delay: Duration::ZERO,
    }
}

#[tokio::test]
async fn connection_read_uses_shared_policy_one_fabric_get_and_no_foreign_monitor() {
    let credential = Credential::default();
    let selection =
        CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric).unwrap();
    let executor = executor(
        200,
        json!({
            "connectionString":"inert-host","continuationUri":"https://foreign.invalid/no",
        }),
    );
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        limits(),
        pending(),
    );
    let value = WarehouseRequests::connection(&target(), ConnectionOptions::default())
        .unwrap()
        .execute(&mut session)
        .await
        .unwrap();
    assert_eq!(value.owner, target());
    assert_eq!(
        *executor.requests.lock().unwrap(),
        [format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/warehouses/{WAREHOUSE}/connectionString"
        )]
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shared_denial_cancellation_and_credential_errors_preserve_evidence_without_fallback() {
    for status in [403, 404] {
        let credential = Credential::default();
        let selection =
            CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric)
                .unwrap();
        let executor = executor(
            status,
            json!({"errorCode":"Denied","message":"sensitive-native-detail"}),
        );
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            limits(),
            pending(),
        );
        match WarehouseRequests::get(&target())
            .unwrap()
            .execute(&mut session)
            .await
        {
            Err(WarehouseError::Operation(error)) => assert_eq!(error.status, Some(status)),
            _ => panic!("denial became missing or success"),
        }
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
    let credential = Credential {
        fail: true,
        ..Credential::default()
    };
    let selection =
        CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric).unwrap();
    let executor = executor(200, row());
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        limits(),
        pending(),
    );
    let error = WarehouseRequests::get(&target())
        .unwrap()
        .execute(&mut session)
        .await
        .unwrap_err();
    assert!(matches!(error, WarehouseError::Operation(_)));
    assert!(!format!("{error:?} {error}").contains("sensitive-provider-detail"));
    assert!(executor.requests.lock().unwrap().is_empty());

    let credential = Credential::default();
    let mut cancelled = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        limits(),
        ready(()),
    );
    assert!(
        WarehouseRequests::get(&target())
            .unwrap()
            .execute(&mut cancelled)
            .await
            .is_err()
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(executor.requests.lock().unwrap().is_empty());
}
