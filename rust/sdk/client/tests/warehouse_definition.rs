// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native POST, acceptance, shared observation and protected failures.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{credentials::AccessToken, time::OffsetDateTime};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    TokenCredential,
    observe::{ObserveErrorKind, ObserveOptions, observe},
    warehouse::{
        WarehouseDefinitionError as Error, WarehouseDefinitionFormat as Format,
        WarehouseDefinitionRead as Read, WarehouseDefinitionRequest, WarehouseRequests,
        WarehouseTarget, read_definition,
    },
};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationRequest,
    OperationResponse,
};
use fabric_sdk_sql_definition::{DefinitionErrorKind, DefinitionLimits};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::json;
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const WS: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OP: &str = "99999999-8888-7777-6666-555555555555";
const HOST: &str = "dailyapi.fabric.microsoft.com";

fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

fn limits() -> DefinitionLimits {
    DefinitionLimits {
        max_wire_bytes: nz(8192),
        max_decoded_bytes: nz(2048),
        max_part_bytes: nz(1024),
        max_parts: nz(16),
        max_path_bytes: nz(128),
        max_depth: nz(8),
    }
}

fn request(format: Option<Format>) -> WarehouseDefinitionRequest {
    WarehouseRequests::get_definition(
        &WarehouseTarget::new(WS.parse().unwrap(), ITEM.parse().unwrap(), "Warehouse").unwrap(),
        format,
        limits(),
    )
    .unwrap()
}

fn source(format: Format) -> Vec<u8> {
    let path = match format {
        Format::Sqlproj => "native.sqlproj",
        Format::Dacpac => "definition.dacpac",
    };
    serde_json::to_vec_pretty(&json!({"definition":{"parts":[
        {"path":path,"payload":STANDARD.encode(b"\xff\0\r\n"),"payloadType":"InlineBase64",
         "mediaType":"application/future; exact=1", "unknown":{"keep":true}},
        {"path":".platform","payload":STANDARD.encode(b"opaque"),"payloadType":"InlineBase64"}
    ]}}))
    .unwrap()
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
        _: Option<azure_core::credentials::TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        if self.fail {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Other,
                "private-credential-detail",
            ));
        }
        Ok(AccessToken::new(
            "synthetic-offline-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

fn client(credential: Arc<Credential>, audience: Audience) -> Client {
    Client::with_context(RequestContext::with_endpoint(
        credential,
        CredentialSelection::new("explicit-account", "explicit-tenant", audience).unwrap(),
        EndpointBinding::with_host(ServiceOrigin::Fabric, HOST).unwrap(),
    ))
}

fn response(status: u16, body: &[u8], headers: &[(&str, &str)]) -> OperationResponse {
    OperationResponse {
        status,
        body: body.into(),
        headers: headers
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect(),
    }
}

fn accepted(location: &str) -> OperationResponse {
    response(
        202,
        b"",
        &[
            ("x-ms-operation-id", OP),
            ("Location", location),
            ("Retry-After", "1"),
        ],
    )
}

#[derive(Debug)]
struct Recorded {
    method: ResourceMethod,
    url: String,
    body: Vec<u8>,
    bound: usize,
}

#[derive(Default)]
struct Executor {
    replies: Mutex<VecDeque<Result<OperationResponse, OperationExecutionError>>>,
    requests: Mutex<Vec<Recorded>>,
    waits: AtomicUsize,
    cancel_after_reply: Option<Arc<AtomicBool>>,
    pause: bool,
    started: tokio::sync::Notify,
    dropped: AtomicUsize,
}

impl Executor {
    fn new(replies: impl IntoIterator<Item = OperationResponse>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().map(Ok).collect()),
            ..Self::default()
        }
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.requests.lock().unwrap().push(Recorded {
                method: request.method(),
                url: request.url().to_string(),
                body: request.body().to_vec(),
                bound: request.max_response_bytes().get(),
            });
            if self.pause {
                let _guard = Dropped(&self.dropped);
                self.started.notify_one();
                pending::<()>().await;
            }
            if let Some(flag) = &self.cancel_after_reply {
                flag.store(true, Ordering::SeqCst);
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected replay/fallback/result GET")
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        Box::pin(ready(Ok(())))
    }
}

struct Dropped<'a>(&'a AtomicUsize);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn options() -> ObserveOptions {
    ObserveOptions {
        wait: false,
        result: true,
        max_requests: nz(4),
        max_response_bytes: nz(8192),
        max_delay: Duration::from_secs(2),
    }
}

#[tokio::test]
async fn exact_native_post_body_query_owner_and_full_raw_content_use_selected_endpoint() {
    for (format, query) in [
        (None, ""),
        (Some(Format::Sqlproj), "?format=sqlproj"),
        (Some(Format::Dacpac), "?format=dacpac"),
    ] {
        let request = request(format);
        let body = source(format.unwrap_or_default());
        let executor = Executor::new([response(200, &body, &[])]);
        let credential = Arc::new(Credential::default());
        let client = client(credential.clone(), Audience::Fabric);
        let Read::Completed(value) = read_definition(&client, &executor, &request, pending())
            .await
            .unwrap()
        else {
            panic!("immediate definition was not completed");
        };
        assert_eq!(value.owner, request.owner());
        assert_eq!(value.data.response_bytes(), body);
        assert_eq!(value.data.parts()[0].bytes, b"\xff\0\r\n");
        assert_eq!(value.data.format(), format.unwrap_or_default());
        assert!(value.data.has_unknown_fields());
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, ResourceMethod::Post);
        assert_eq!(
            requests[0].url,
            format!("https://{HOST}/v1/workspaces/{WS}/warehouses/{ITEM}/getDefinition{query}")
        );
        assert!(requests[0].body.is_empty());
        assert_eq!(requests[0].bound, limits().max_wire_bytes.get());
        assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn accepted_definition_uses_only_existing_observer_for_readiness_and_result() {
    let request = request(None);
    let location = format!("https://{HOST}/v1/operations/{OP}");
    let body = source(Format::Sqlproj);
    let executor = Executor::new([
        accepted(&location),
        response(200, br#"{"status":"Running","future":"retained"}"#, &[]),
        response(
            200,
            br#"{"status":"Succeeded"}"#,
            &[("Location", &format!("{location}/result"))],
        ),
        response(200, &body, &[("Content-Type", "application/json; exact=1")]),
    ]);
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let read = read_definition(&client, &executor, &request, pending())
        .await
        .unwrap();
    assert!(matches!(&read,Read::Accepted {operation_id:Some(id), ..} if id==OP));
    assert!(!format!("{read:?}").contains(OP));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
    let handle = read.accepted_handle().unwrap();
    let not_ready = observe(&client, &executor, &handle, options(), pending())
        .await
        .unwrap_err();
    assert_eq!(not_ready.kind, ObserveErrorKind::ResultNotReady);
    assert_eq!(
        not_ready.last_observation.unwrap().raw_status["future"],
        "retained"
    );
    let observed = observe(&client, &executor, &handle, options(), pending())
        .await
        .unwrap();
    assert!(observed.work_succeeded);
    let result = observed.result.unwrap();
    assert_eq!(
        result.media_type.as_deref(),
        Some("application/json; exact=1")
    );
    let definition = request.decode_completed_result(&result.bytes).unwrap();
    assert_eq!(definition.owner, request.owner());
    assert_eq!(definition.data.response_bytes(), body);
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == ResourceMethod::Post)
            .count(),
        1
    );
    assert!(
        requests[1..]
            .iter()
            .all(|request| request.method == ResourceMethod::Get && request.body.is_empty())
    );
    assert_eq!(requests[3].url, format!("{location}/result"));
    assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn acceptance_without_a_native_handle_is_incomplete_not_fabricated() {
    let executor = Executor::new([response(202, b"", &[])]);
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let read = read_definition(&client, &executor, &request(None), pending())
        .await
        .unwrap();
    assert!(matches!(
        read,
        Read::Accepted {
            operation_id: None,
            ..
        }
    ));
    assert!(read.accepted_handle().is_none());
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn location_only_acceptance_retains_actual_native_id_without_polling() {
    let location = format!("https://{HOST}/v1/operations/{OP}");
    let executor = Executor::new([response(202, b"", &[("Location", &location)])]);
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let read = read_definition(&client, &executor, &request(None), pending())
        .await
        .unwrap();
    assert!(matches!(read,Read::Accepted {operation_id:Some(id),..} if id==OP));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
    assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn foreign_malformed_or_conflicting_acceptance_links_are_never_followed() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    for response in [
        accepted(&format!("https://foreign.invalid/v1/operations/{OP}")),
        accepted(&format!(
            "https://{HOST}/v1/operations/00000000-0000-0000-0000-000000000000"
        )),
        response(202, b"", &[("x-ms-operation-id", "not-a-guid")]),
    ] {
        let executor = Executor::new([response]);
        assert!(matches!(
            read_definition(&client, &executor, &request(None), pending()).await,
            Err(Error::Operation(_))
        ));
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
        assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn shared_observer_rejects_foreign_result_without_fetching_it() {
    let location = format!("https://{HOST}/v1/operations/{OP}");
    let executor = Executor::new([
        accepted(&location),
        response(
            200,
            br#"{"status":"Succeeded"}"#,
            &[(
                "Location",
                &format!("https://foreign.invalid/v1/operations/{OP}/result"),
            )],
        ),
    ]);
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let handle = read_definition(&client, &executor, &request(None), pending())
        .await
        .unwrap()
        .accepted_handle()
        .unwrap();
    let error = observe(&client, &executor, &handle, options(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.operation_error.kind, OperationErrorKind::InvalidLink);
    assert_eq!(executor.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn shared_observer_result_denial_preserves_native_id_and_partial_status_evidence() {
    let location = format!("https://{HOST}/v1/operations/{OP}");
    let raw = br#"{"errorCode":"Denied","future":{"keep":true}}"#;
    let executor = Executor::new([
        accepted(&location),
        response(
            200,
            br#"{"status":"Succeeded","future":"keep"}"#,
            &[("Location", &format!("{location}/result"))],
        ),
        response(403, raw, &[]),
    ]);
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let handle = read_definition(&client, &executor, &request(None), pending())
        .await
        .unwrap()
        .accepted_handle()
        .unwrap();
    let error = observe(&client, &executor, &handle, options(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.operation_error.operation_id.as_deref(), Some(OP));
    assert_eq!(error.last_observation.unwrap().raw_status["future"], "keep");
    assert_eq!(
        error.operation_error.raw_service_error.as_deref(),
        Some(&serde_json::from_slice::<serde_json::Value>(raw).unwrap())
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn native_denial_not_supported_throttle_and_service_errors_never_fall_back() {
    for (status, code) in [
        (401, "Unauthorized"),
        (403, "EncryptedLabelDenied"),
        (400, "OperationNotSupportedForItem"),
        (429, "RateLimited"),
        (500, "FutureError"),
    ] {
        let raw = json!({"errorCode":code,"message":"private-service-detail","future":{"keep":1}});
        let executor = Executor::new([response(status, &serde_json::to_vec(&raw).unwrap(), &[])]);
        let client = client(Arc::new(Credential::default()), Audience::Fabric);
        let error = read_definition(&client, &executor, &request(None), pending())
            .await
            .unwrap_err();
        assert!(!format!("{error} {error:?}").contains("private-service-detail"));
        let Error::Operation(error) = error else {
            panic!("service error became metadata")
        };
        assert!(matches!(error.kind,OperationErrorKind::Http {status:actual,..} if actual==status));
        assert_eq!(error.raw_service_error.as_deref(), Some(&raw));
        assert!(error.mutation_may_have_occurred);
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn invalid_completed_bodies_retain_exact_partial_evidence_and_never_succeed() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    for (raw, kind) in [
        (b"{}".as_slice(), DefinitionErrorKind::MissingDefinition),
        (b"{\"definition\":null}", DefinitionErrorKind::InvalidShape),
        (
            b"{\"definition\":{\"format\":\"future\",\"parts\":[]}}",
            DefinitionErrorKind::UnsupportedFormat,
        ),
        (
            b"{\"definition\":{\"parts\":[],\"parts\":[]}}",
            DefinitionErrorKind::InvalidShape,
        ),
        (
            b"{\"status\":\"Succeeded\"}",
            DefinitionErrorKind::MissingDefinition,
        ),
        (
            b"not-json-private-content",
            DefinitionErrorKind::InvalidJson,
        ),
    ] {
        let executor = Executor::new([response(200, raw, &[])]);
        let error = read_definition(&client, &executor, &request(None), pending())
            .await
            .unwrap_err();
        assert!(!format!("{error:?} {error}").contains("private-content"));
        match error {
            Error::Content {
                error,
                response,
                operation_id,
            } => {
                assert_eq!(error.kind, kind);
                assert_eq!(&*response, raw);
                assert!(operation_id.is_none());
            }
            _ => panic!("missing protected content error"),
        }
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
    for status in [201, 204] {
        let executor = Executor::new([response(status, b"", &[])]);
        assert!(
            matches!(read_definition(&client,&executor,&request(None),pending()).await,
            Err(Error::UnexpectedStatus {status:actual,..}) if actual==status)
        );
    }
}

#[tokio::test]
async fn cancellation_wrong_audience_credentials_timeout_and_response_bounds_are_explicit() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone(), Audience::Fabric);
    let executor = Executor::default();
    assert!(
        matches!(read_definition(&client,&executor,&request(None),ready(())).await,
        Err(Error::Operation(error)) if error.kind==OperationErrorKind::Cancelled)
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(executor.requests.lock().unwrap().is_empty());
    for (audience, fail) in [(Audience::Arm, false), (Audience::Fabric, true)] {
        let credential = Arc::new(Credential {
            fail,
            ..Credential::default()
        });
        let client = self::client(credential, audience);
        let error = read_definition(&client, &executor, &request(None), pending())
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Operation(_)));
        assert!(!format!("{error:?} {error}").contains("private-credential-detail"));
        assert!(executor.requests.lock().unwrap().is_empty());
    }
    for failure in [
        OperationExecutionError::TimedOut,
        OperationExecutionError::Cancelled,
        OperationExecutionError::Failed,
    ] {
        let executor = Executor {
            replies: Mutex::new(VecDeque::from([Err(failure)])),
            ..Executor::default()
        };
        let Error::Operation(error) =
            read_definition(&client, &executor, &request(None), pending())
                .await
                .unwrap_err()
        else {
            panic!("executor failure was swallowed")
        };
        assert_eq!(error.kind, OperationErrorKind::Execution(failure));
        assert!(error.mutation_may_have_occurred);
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
    let oversized = vec![b' '; limits().max_wire_bytes.get() + 1];
    let executor = Executor::new([response(200, &oversized, &[])]);
    assert!(
        matches!(read_definition(&client,&executor,&request(None),pending()).await,
        Err(Error::Operation(error)) if error.kind==OperationErrorKind::Limit)
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_after_acceptance_preserves_real_handle_without_resubmitting() {
    let flag = Arc::new(AtomicBool::new(false));
    let location = format!("https://{HOST}/v1/operations/{OP}");
    let executor = Executor {
        cancel_after_reply: Some(flag.clone()),
        ..Executor::new([accepted(&location)])
    };
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let cancelled = poll_fn(|_| {
        if flag.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let Error::Operation(error) = read_definition(&client, &executor, &request(None), cancelled)
        .await
        .unwrap_err()
    else {
        panic!("cancellation was lost");
    };
    assert_eq!(error.kind, OperationErrorKind::Cancelled);
    assert_eq!(error.operation_id.as_deref(), Some(OP));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn in_flight_cancellation_drops_only_local_request_and_never_resubmits() {
    let executor = Executor {
        pause: true,
        ..Executor::default()
    };
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let request = request(None);
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let read = read_definition(&client, &executor, &request, async {
        let _ = receive.await;
    });
    let cancel = async {
        executor.started.notified().await;
        send.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(read, cancel);
    let Error::Operation(error) = result.unwrap_err() else {
        panic!("cancellation was lost")
    };
    assert_eq!(error.kind, OperationErrorKind::Cancelled);
    assert!(error.mutation_may_have_occurred);
    assert!(error.operation_id.is_none());
    assert_eq!(executor.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}
