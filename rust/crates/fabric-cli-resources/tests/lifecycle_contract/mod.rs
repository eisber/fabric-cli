// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use std::{
    future::Future,
    pin::pin,
    sync::atomic::AtomicBool,
    task::{Context, Poll, Waker},
};

struct DropGuard(Arc<AtomicBool>);
impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct PendingWire {
    started: AtomicBool,
    dropped: Arc<AtomicBool>,
}
#[async_trait]
impl HttpClient for PendingWire {
    async fn execute_request(&self, _: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.started.store(true, Ordering::SeqCst);
        let _guard = DropGuard(self.dropped.clone());
        pending().await
    }
}

#[tokio::test]
async fn in_flight_cancellation_drops_shared_http_work_and_never_returns_success() {
    let dropped = Arc::new(AtomicBool::new(false));
    let wire = Arc::new(PendingWire {
        started: AtomicBool::new(false),
        dropped: dropped.clone(),
    });
    let credential = Arc::new(Credential::default());
    let executor = Arc::new(
        HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap(),
    );
    let api = ApiClient::new(
        credential.clone(),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        Arc::new(fabric_cli_resources::ResourceHttpExecutor(executor)),
    );
    let client = AccessClient::new(api);
    let target = resource("workspace");
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let mut operation = pin!(client.list(&target, Limits::default(), async {
        let _ = receive.await;
    }));
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        operation.as_mut().poll(&mut context),
        Poll::Pending
    ));
    assert!(wire.started.load(Ordering::SeqCst));
    assert!(!dropped.load(Ordering::SeqCst));
    send.send(()).unwrap();
    let Poll::Ready(Err(error)) = operation.as_mut().poll(&mut context) else {
        panic!("cancellation must complete as an error");
    };
    assert_eq!(error.error, ApiError::Cancelled);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_of_dispatched_acl_mutation_retains_uncertainty_without_replaying_or_suppressing_it()
 {
    use azure_core::http::Method;
    let dropped = Arc::new(AtomicBool::new(false));
    let wire = Arc::new(PendingWire {
        started: AtomicBool::new(false),
        dropped: dropped.clone(),
    });
    let executor = Arc::new(
        HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap(),
    );
    let api = ApiClient::new(
        Arc::new(Credential::default()),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        Arc::new(fabric_cli_resources::ResourceHttpExecutor(executor)),
    );
    let request =
        fabric_sdk_api::Request::fabric(Method::Post, &["admin", "items", "bulkSetLabels"])
            .unwrap()
            .with_json(&json!({"items":[]}))
            .unwrap();
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let mut operation =
        pin!(
            api.send_completed(&request, NonZeroUsize::new(4096).unwrap(), async {
                let _ = receive.await;
            })
        );
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        operation.as_mut().poll(&mut context),
        Poll::Pending
    ));
    assert!(wire.started.load(Ordering::SeqCst));
    send.send(()).unwrap();
    let Poll::Ready(Err(error)) = operation.as_mut().poll(&mut context) else {
        panic!("cancel must terminate with uncertainty");
    };
    let ApiError::MutationUncertain(cause) = error else {
        panic!("dispatched write must not claim no mutation");
    };
    assert_eq!(*cause, ApiError::Cancelled);
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn measured_row_byte_and_cycle_limits_do_not_become_incomplete_success() {
    let (api, wire, _) = setup(vec![reply(
        200,
        r#"{"value":[{"id":"a"},{"id":"b"}]}"#,
        &[],
    )]);
    let e = AccessClient::new(api)
        .list(
            &resource("workspace"),
            Limits {
                max_items: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(e.error, ApiError::Limit);
    assert_eq!(e.partial.pages, 0);
    assert!(e.partial.rows.is_empty());
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    let body = r#"{"value":[{"id":"a"}]}"#;
    for extra in [false, true] {
        let (api, _, _) = setup(vec![reply(
            200,
            &format!("{body}{}", if extra { " " } else { "" }),
            &[],
        )]);
        let result = AccessClient::new(api)
            .list(
                &resource("workspace"),
                Limits {
                    max_response_bytes: NonZeroUsize::new(body.len()).unwrap(),
                    ..Limits::default()
                },
                pending(),
            )
            .await;
        if extra {
            assert_eq!(
                result.unwrap_err().error,
                ApiError::Execution(fabric_sdk_api::ExecutionError::ResponseLimit)
            );
        } else {
            assert_eq!(result.unwrap().rows.len(), 1);
        }
    }
    let page = r#"{"value":[],"continuationToken":"same"}"#;
    let (api, wire, _) = setup(vec![reply(200, page, &[]), reply(200, page, &[])]);
    let e = AccessClient::new(api)
        .list(&resource("workspace"), Limits::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(e.error, ApiError::Continuation);
    assert_eq!(e.partial.pages, 2);
    assert_eq!(wire.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn shared_query_pager_preserves_original_params_and_rejects_encoded_uri_path_rewriting() {
    use fabric_sdk_api::{Origin, Paging, RawRequestOptions, Request as ApiRequest, RequestBounds};
    let request = ApiRequest::raw(RawRequestOptions {
        origin: Origin::Fabric,
        method: azure_core::http::Method::Get,
        endpoint: "resources?filter= *~".into(),
        query: Vec::new(),
        headers: Vec::new(),
        body: Vec::new(),
        bounds: RequestBounds::default(),
    })
    .unwrap();
    let (api, wire, _) = setup(vec![
        reply(200, r#"{"value":[],"continuationToken":" *~"}"#, &[]),
        reply(200, r#"{"value":[]}"#, &[]),
    ]);
    api.collect(
        request,
        "value",
        Paging::Query,
        Limits::default(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        wire.requests.lock().unwrap()[1].url().query(),
        Some("filter=+%2A~&continuationToken=+%2A~")
    );
    let (api, wire, _) = setup(vec![reply(
        200,
        r#"{"value":[],"continuationToken":"next","continuationUri":"https://api.fabric.microsoft.com/v1/workspaces/other/%2e%2e/resource/roleAssignments"}"#,
        &[],
    )]);
    assert_eq!(
        AccessClient::new(api)
            .list(&resource("workspace"), Limits::default(), pending())
            .await
            .unwrap_err()
            .error,
        ApiError::Continuation
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}
