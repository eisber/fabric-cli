// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive offline coverage for explicitly bound operation sessions.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
};
use fabric_sdk_operations::{
    FabricOperation, OperationErrorKind, OperationExecutionError, OperationExecutor,
    OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod, ResourceOperation};
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::Duration,
};

const HOST: &str = "dailyapi.fabric.microsoft.com";
const ROOT: &str = "https://dailyapi.fabric.microsoft.com";
const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
    cancel_after_token: Option<Arc<AtomicBool>>,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
        if let Some(cancelled) = &self.cancel_after_token {
            cancelled.store(true, Ordering::SeqCst);
        }
        Ok(AccessToken::new(
            "synthetic-endpoint-test-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct RecordedRequest {
    method: ResourceMethod,
    url: String,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct Executor {
    responses: Arc<Mutex<VecDeque<OperationResponse>>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    cancel_after_response: Option<Arc<AtomicBool>>,
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.requests.lock().unwrap().push(RecordedRequest {
                method: request.method(),
                url: request.url().to_string(),
                body: request.body().to_vec(),
            });
            let response = self.responses.lock().unwrap().pop_front().unwrap();
            if let Some(cancelled) = &self.cancel_after_response {
                cancelled.store(true, Ordering::SeqCst);
            }
            Ok(response)
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn selection(audience: Audience) -> CredentialSelection {
    CredentialSelection::new("offline", "tenant", audience).unwrap()
}

fn context(credential: Arc<Credential>) -> RequestContext {
    RequestContext::with_endpoint(
        credential,
        selection(Audience::Fabric),
        EndpointBinding::with_host(ServiceOrigin::Fabric, HOST).unwrap(),
    )
}

fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> OperationResponse {
    OperationResponse {
        status,
        body: body.as_bytes().to_vec(),
        headers: headers
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

fn operation(method: ResourceMethod, completion: CompletionPolicy) -> FabricOperation {
    FabricOperation::new(method, "/v1/workspaces/ws", Vec::new(), completion).unwrap()
}

fn cancellation(flag: Arc<AtomicBool>) -> impl Future<Output = ()> + Send {
    let mut completed = false;
    poll_fn(move |_| {
        assert!(
            !completed,
            "a completed cancellation future was polled again"
        );
        if flag.load(Ordering::SeqCst) {
            completed = true;
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

#[tokio::test]
async fn selected_context_binds_mutation_every_poll_and_result_without_replay() {
    let credential = Arc::new(Credential::default());
    let context = context(credential.clone());
    let executor = Executor::default();
    let path = format!("/v1/operations/{ID}");
    let poll_url = format!("{ROOT}{path}");
    let result_url = format!("{poll_url}/result");
    executor.responses.lock().unwrap().extend([
        response(202, "{}", &[("Location", &poll_url), ("Retry-After", "0")]),
        response(
            200,
            r#"{"status":"Running"}"#,
            &[("Location", &poll_url), ("Retry-After", "0")],
        ),
        response(
            200,
            r#"{"status":"Succeeded"}"#,
            &[("Location", &result_url)],
        ),
        response(200, r#"{"id":"actual"}"#, &[]),
    ]);
    let mut session = OperationSession::with_context(
        &context,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let result = session
        .execute(operation(
            ResourceMethod::Post,
            CompletionPolicy::WaitForResult,
        ))
        .await
        .unwrap();
    assert_eq!(result.body, br#"{"id":"actual"}"#);
    assert_eq!(session.operation_id(), Some(ID));
    let error = session
        .execute(operation(
            ResourceMethod::Post,
            CompletionPolicy::WaitForResult,
        ))
        .await
        .unwrap_err();
    assert_eq!(error.kind, OperationErrorKind::InvalidInput);
    assert!(error.mutation_may_have_occurred);
    let requests = executor.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.url.as_str())
            .collect::<Vec<_>>(),
        [
            format!("{ROOT}/v1/workspaces/ws"),
            poll_url.clone(),
            poll_url,
            result_url,
        ]
    );
    assert_eq!(requests[0].method, ResourceMethod::Post);
    assert!(
        requests[1..]
            .iter()
            .all(|request| request.method == ResourceMethod::Get && request.body.is_empty())
    );
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        vec![vec![Audience::Fabric.scope().to_owned()]; 4]
    );
}

#[tokio::test]
async fn external_acceptance_uses_selected_binding_without_replaying_the_mutation() {
    for maximum in [2, 3] {
        let credential = Arc::new(Credential::default());
        let context = context(credential.clone());
        let executor = Executor::default();
        let poll_url = format!("{ROOT}/v1/operations/{ID}");
        let result_url = format!("{poll_url}/result");
        executor.responses.lock().unwrap().extend([
            response(
                200,
                r#"{"status":"Succeeded"}"#,
                &[("Location", &result_url)],
            ),
            response(200, r#"{"id":"actual"}"#, &[]),
        ]);
        let mut session = OperationSession::with_context(
            &context,
            executor.clone(),
            OperationLimits {
                max_requests: std::num::NonZeroUsize::new(maximum).unwrap(),
                ..OperationLimits::default()
            },
            pending(),
        );
        let result = session
            .reconcile_accepted(
                response(
                    202,
                    "{}",
                    &[
                        ("x-ms-operation-id", ID),
                        ("Location", &poll_url),
                        ("Retry-After", "0"),
                    ],
                ),
                CompletionPolicy::WaitForResult,
            )
            .await;
        if maximum == 3 {
            assert_eq!(result.unwrap().body, br#"{"id":"actual"}"#);
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind, OperationErrorKind::Limit);
            assert!(error.mutation_may_have_occurred);
        }
        assert_eq!(session.operation_id(), Some(ID));
        assert_eq!(credential.scopes.lock().unwrap().len(), maximum - 1);
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.len(), maximum - 1);
        assert_eq!(requests[0].url, poll_url);
        assert!(
            requests
                .iter()
                .all(|request| request.method == ResourceMethod::Get)
        );
        if maximum == 3 {
            assert_eq!(requests[1].url, result_url);
        }
    }
}

#[tokio::test]
async fn encoded_resource_paths_keep_query_spelling_and_bind_only_at_execution() {
    let credential = Arc::new(Credential::default());
    let context = context(credential);
    let executor = Executor::default();
    executor
        .responses
        .lock()
        .unwrap()
        .push_back(response(200, "{}", &[]));
    let resource = ResourceOperation::encoded(
        ResourceMethod::Get,
        &["workspaces", "ws", "shortcuts", "name +日%"],
        None,
        CompletionPolicy::ReturnAccepted,
    )
    .unwrap()
    .with_query(&[("filter", "*~ +日")])
    .unwrap()
    .with_continuation_token("opaque+/*~")
    .unwrap();
    let expected = format!("{ROOT}{}", resource.path());
    let mut session = OperationSession::with_context(
        &context,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    session
        .execute(FabricOperation::from_resource(&resource).unwrap())
        .await
        .unwrap();
    assert_eq!(executor.requests.lock().unwrap()[0].url, expected);
}

#[tokio::test]
async fn explicit_source_read_keeps_relative_locations_and_opaque_id_opt_in() {
    for explicit in [false, true] {
        let credential = Arc::new(Credential::default());
        let context = context(credential.clone());
        let executor = Executor::default();
        executor.responses.lock().unwrap().extend([
            response(
                202,
                "{}",
                &[
                    ("Location", "/v1/operations/opaque_id"),
                    ("x-ms-operation-id", "opaque_id"),
                    ("Retry-After", "0"),
                ],
            ),
            response(
                200,
                r#"{"status":"Succeeded"}"#,
                &[("Location", "/v1/operations/opaque_id/result")],
            ),
            response(200, r#"{"id":"actual"}"#, &[]),
        ]);
        let mut session = OperationSession::with_context(
            &context,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let operation = operation(ResourceMethod::Get, CompletionPolicy::PythonSynchronous);
        let operation = if explicit {
            operation.with_source_read_operation_ids().unwrap()
        } else {
            operation
        };
        let result = session.execute(operation).await;
        if explicit {
            assert_eq!(result.unwrap().body, br#"{"id":"actual"}"#);
            assert_eq!(session.operation_id(), Some("opaque_id"));
            let requests = executor.requests.lock().unwrap();
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[1].url, format!("{ROOT}/v1/operations/opaque_id"));
            assert_eq!(
                requests[2].url,
                format!("{ROOT}/v1/operations/opaque_id/result")
            );
        } else {
            assert_eq!(
                result.unwrap_err().kind,
                OperationErrorKind::InvalidResponse
            );
            assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        }
        assert!(
            !session
                .evidence(OperationErrorKind::Cancelled)
                .mutation_may_have_occurred
        );
    }
}

#[tokio::test]
async fn other_microsoft_hosts_are_not_trusted_for_initial_or_result_links() {
    for completion in [
        CompletionPolicy::ReturnAccepted,
        CompletionPolicy::WaitForResult,
        CompletionPolicy::PythonSynchronous,
    ] {
        for foreign in [
            "api.fabric.microsoft.com",
            "other.dailyapi.fabric.microsoft.com",
            "api.powerbi.com",
        ] {
            for at_result in [false, true] {
                if at_result && completion == CompletionPolicy::ReturnAccepted {
                    continue;
                }
                let credential = Arc::new(Credential::default());
                let context = context(credential.clone());
                let executor = Executor::default();
                let poll_url = format!("{ROOT}/v1/operations/{ID}");
                let foreign_url = format!("https://{foreign}/v1/operations/{ID}");
                executor.responses.lock().unwrap().extend([
                    response(
                        202,
                        "{}",
                        &[
                            ("Location", if at_result { &poll_url } else { &foreign_url }),
                            ("x-ms-operation-id", ID),
                            ("Retry-After", "0"),
                        ],
                    ),
                    response(
                        200,
                        r#"{"status":"Succeeded"}"#,
                        &[("Location", &format!("{foreign_url}/result"))],
                    ),
                ]);
                let mut session = OperationSession::with_context(
                    &context,
                    executor.clone(),
                    OperationLimits::default(),
                    pending(),
                );
                let error = session
                    .execute(operation(ResourceMethod::Post, completion))
                    .await
                    .unwrap_err();
                assert_eq!(error.kind, OperationErrorKind::InvalidLink);
                assert!(error.mutation_may_have_occurred);
                let expected = if at_result { 2 } else { 1 };
                assert_eq!(credential.scopes.lock().unwrap().len(), expected);
                assert_eq!(executor.requests.lock().unwrap().len(), expected);
            }
        }
    }
}

#[tokio::test]
async fn service_and_audience_mismatches_fail_before_credentials_or_mutation() {
    for (origin, audience, expected) in [
        (
            ServiceOrigin::PowerBi,
            Audience::Fabric,
            OperationErrorKind::InvalidInput,
        ),
        (
            ServiceOrigin::Fabric,
            Audience::Arm,
            OperationErrorKind::InvalidAudience,
        ),
    ] {
        let credential = Arc::new(Credential::default());
        let context = RequestContext::with_endpoint(
            credential.clone(),
            selection(audience),
            EndpointBinding::with_host(origin, HOST).unwrap(),
        );
        let executor = Executor::default();
        let mut session = OperationSession::with_context(
            &context,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let error = session
            .execute(operation(
                ResourceMethod::Post,
                CompletionPolicy::WaitForResult,
            ))
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(!error.mutation_may_have_occurred);
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert!(executor.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn core_request_cancellation_stays_latched_and_preserves_dispatch_evidence() {
    for boundary in 0..4 {
        let flag = Arc::new(AtomicBool::new(boundary == 0));
        let credential = Arc::new(Credential {
            cancel_after_token: (boundary == 1).then(|| flag.clone()),
            ..Credential::default()
        });
        let context = context(credential.clone());
        let executor = Executor {
            cancel_after_response: (boundary >= 2).then(|| flag.clone()),
            ..Executor::default()
        };
        executor.responses.lock().unwrap().push_back(response(
            202,
            "{}",
            &[(
                "x-ms-operation-id",
                if boundary == 3 { "opaque_id" } else { ID },
            )],
        ));
        let mut session = OperationSession::with_context(
            &context,
            executor.clone(),
            OperationLimits::default(),
            cancellation(flag),
        );
        let operation = if boundary == 3 {
            operation(ResourceMethod::Get, CompletionPolicy::PythonSynchronous)
                .with_source_read_operation_ids()
                .unwrap()
        } else {
            operation(ResourceMethod::Post, CompletionPolicy::WaitForResult)
        };
        let error = session.execute(operation).await.unwrap_err();
        assert_eq!(error.kind, OperationErrorKind::Cancelled);
        assert_eq!(error.mutation_may_have_occurred, boundary == 2);
        assert_eq!(
            error.operation_id.as_deref(),
            match boundary {
                2 => Some(ID),
                3 => Some("opaque_id"),
                _ => None,
            }
        );
        assert_eq!(
            credential.scopes.lock().unwrap().len(),
            usize::from(boundary != 0)
        );
        assert_eq!(
            executor.requests.lock().unwrap().len(),
            usize::from(boundary >= 2)
        );
        for _ in 0..2 {
            assert_eq!(
                session.check_cancelled().await.unwrap_err().kind,
                OperationErrorKind::Cancelled
            );
        }
    }
}

#[tokio::test]
async fn borrowed_constructor_still_uses_public_fabric() {
    let credential = Credential::default();
    let selection = selection(Audience::Fabric);
    let executor = Executor::default();
    executor
        .responses
        .lock()
        .unwrap()
        .push_back(response(200, "{}", &[]));
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    session
        .execute(operation(
            ResourceMethod::Get,
            CompletionPolicy::ReturnAccepted,
        ))
        .await
        .unwrap();
    assert_eq!(
        executor.requests.lock().unwrap()[0].url,
        "https://api.fabric.microsoft.com/v1/workspaces/ws"
    );
}
