// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Runtime-neutral, cancellation-aware authentication over injected execution.

use crate::{AccessToken, CredentialSelection, EndpointBinding, ServiceOrigin, TokenCredential};
use azure_core::http::Url;
use std::{
    fmt,
    future::{Future, poll_fn, ready},
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
};
use thiserror::Error;

/// Cancellation of caller-owned asynchronous work.
///
/// Dropping an operation does not imply that an already submitted service
/// mutation was rolled back.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("the request was cancelled")]
pub struct Cancelled;

/// Polls cancellation before the operation on every wake.
///
/// If both futures are ready, cancellation wins without polling the operation.
/// Cancellation drops the operation future; this helper creates no runtime,
/// tasks, timers, cache, retry policy, or transport.
///
/// # Errors
///
/// Returns [`Cancelled`] when the caller's cancellation future completes.
pub async fn cancel_with<T>(
    operation: impl Future<Output = T>,
    mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
) -> Result<T, Cancelled> {
    let mut operation = pin!(operation);
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(Cancelled));
        }
        operation.as_mut().poll(cx).map(Ok)
    })
    .await
}

/// An operation-specific executor receiving an authenticated, validated URL.
///
/// Implementations supply their own transport and behavioral policies. This
/// boundary does not request tokens again or imply that execution is replayable.
pub trait AuthenticatedExecution {
    /// The executor's operation-specific response.
    type Response;
    /// The executor's operation-specific failure.
    type Error;

    /// Executes once with the supplied URL and token on the caller's runtime.
    fn execute<'a>(
        &'a mut self,
        url: &'a Url,
        token: &'a AccessToken,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'a>>;
}

/// Authentication and execution failures, preserving executor failures by type.
///
/// Formatting never exposes the credential provider's diagnostic or the
/// executor's arbitrary error contents.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum RequestError<E> {
    /// The selection's audience does not match the bound service's audience.
    InvalidAudience,
    /// The service identity or URL does not match the endpoint binding.
    InvalidEndpoint,
    /// The injected credential could not acquire the selected service token.
    Credential,
    /// The injected executor failed; the original failure remains inspectable.
    Execution(E),
    /// Caller cancellation completed before the request could return.
    Cancelled,
}

impl<E> fmt::Debug for RequestError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidAudience => "InvalidAudience",
            Self::InvalidEndpoint => "InvalidEndpoint",
            Self::Credential => "Credential",
            Self::Execution(_) => "Execution(<redacted>)",
            Self::Cancelled => "Cancelled",
        })
    }
}

impl<E> fmt::Display for RequestError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidAudience => "the credential audience does not match the service",
            Self::InvalidEndpoint => "the request does not match the selected endpoint",
            Self::Credential => "the selected credential could not acquire a token",
            Self::Execution(_) => "the authenticated execution failed",
            Self::Cancelled => "the request was cancelled",
        })
    }
}

impl<E> std::error::Error for RequestError<E> {}

/// Immutable credential, identity selection, and endpoint metadata for requests.
///
/// The caller must configure the injected provider for the selected account and
/// tenant. Construction neither verifies token claims nor acquires a token.
#[derive(Clone)]
pub struct RequestContext {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
    endpoint: EndpointBinding,
}

impl RequestContext {
    /// Stores a credential and selection with the default public Fabric endpoint.
    ///
    /// Audience mismatches are deliberately retained until execution, preserving
    /// the existing construction behavior of service clients.
    pub fn new(credential: Arc<dyn TokenCredential>, selection: CredentialSelection) -> Self {
        Self::with_endpoint(
            credential,
            selection,
            EndpointBinding::public(ServiceOrigin::Fabric),
        )
    }

    /// Stores explicit endpoint and identity metadata without acquiring a token.
    pub fn with_endpoint(
        credential: Arc<dyn TokenCredential>,
        selection: CredentialSelection,
        endpoint: EndpointBinding,
    ) -> Self {
        Self {
            credential,
            selection,
            endpoint,
        }
    }

    /// Returns the caller-injected credential without probing it.
    pub fn credential(&self) -> &Arc<dyn TokenCredential> {
        &self.credential
    }

    /// Returns the caller's explicit account, tenant, and audience selection.
    pub fn selection(&self) -> &CredentialSelection {
        &self.selection
    }

    /// Returns the immutable endpoint selected for this context.
    pub fn endpoint(&self) -> &EndpointBinding {
        &self.endpoint
    }

    /// Borrows the credential and request metadata without cloning or I/O.
    pub fn authentication(&self) -> RequestAuthentication<'_> {
        RequestAuthentication::new(self.credential.as_ref(), &self.selection, &self.endpoint)
    }

    /// Authenticates and executes once using this context's endpoint binding.
    ///
    /// # Errors
    ///
    /// Returns the same validation, acquisition, execution, or cancellation
    /// failure as [`RequestAuthentication::execute`].
    pub async fn execute<A: AuthenticatedExecution>(
        &self,
        expected: ServiceOrigin,
        url: &Url,
        execution: &mut A,
        cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<A::Response, RequestError<A::Error>> {
        self.authentication()
            .execute(expected, url, execution, cancelled)
            .await
    }
}

impl fmt::Debug for RequestContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestContext")
            .field("credential", &"<injected>")
            .field("selection", &self.selection)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

/// A copyable borrowed authentication boundary for one explicit endpoint.
#[derive(Clone, Copy)]
pub struct RequestAuthentication<'a> {
    credential: &'a dyn TokenCredential,
    selection: &'a CredentialSelection,
    endpoint: &'a EndpointBinding,
}

impl<'a> RequestAuthentication<'a> {
    /// Borrows a provider and explicit metadata without validation or I/O.
    pub const fn new(
        credential: &'a dyn TokenCredential,
        selection: &'a CredentialSelection,
        endpoint: &'a EndpointBinding,
    ) -> Self {
        Self {
            credential,
            selection,
            endpoint,
        }
    }

    /// Validates, acquires one token, and invokes the executor exactly once.
    ///
    /// Cancellation is checked first, between acquisition and execution, and
    /// after execution. Origin, audience, and URL validation precede credential
    /// access. Only the endpoint's fixed audience scope is requested, with no
    /// token request options. No refresh cache or retry policy is installed.
    /// Validation shares the acquisition cancellation boundary, preserving the
    /// acquisition, dispatch, and post-dispatch checkpoints.
    ///
    /// # Errors
    ///
    /// Returns [`RequestError::InvalidEndpoint`] for an expected-service or URL
    /// mismatch, [`RequestError::InvalidAudience`] for a selection mismatch,
    /// [`RequestError::Credential`] for acquisition failure, or the executor's
    /// [`RequestError::Execution`]. Cancellation wins at each checked boundary.
    pub async fn execute<A: AuthenticatedExecution>(
        &self,
        expected: ServiceOrigin,
        url: &Url,
        execution: &mut A,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<A::Response, RequestError<A::Error>> {
        let token = cancel_with(
            async {
                if expected != self.endpoint.origin() {
                    return Err(RequestError::InvalidEndpoint);
                }
                if self.selection.audience() != self.endpoint.audience() {
                    return Err(RequestError::InvalidAudience);
                }
                self.endpoint
                    .validate_url(url)
                    .map_err(|_| RequestError::InvalidEndpoint)?;
                let scopes = [self.endpoint.audience().scope()];
                self.credential
                    .get_token(&scopes, None)
                    .await
                    .map_err(|_| RequestError::Credential)
            },
            cancelled.as_mut(),
        )
        .await
        .map_err(|_| RequestError::Cancelled)??;
        let result = cancel_with(
            async { execution.execute(url, &token).await },
            cancelled.as_mut(),
        )
        .await
        .map_err(|_| RequestError::Cancelled)?;
        cancel_with(ready(()), cancelled.as_mut())
            .await
            .map_err(|_| RequestError::Cancelled)?;
        result.map_err(RequestError::Execution)
    }
}

impl fmt::Debug for RequestAuthentication<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestAuthentication")
            .field("credential", &"<injected>")
            .field("selection", &self.selection)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Audience, TokenRequestOptions};
    use azure_core::{error::ErrorKind, time::OffsetDateTime};
    use std::{
        future::pending,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        task::{Context, Waker},
    };

    const TEST_TOKEN: &str = "offline-only-token-not-a-real-credential";

    #[derive(Default)]
    struct Credential {
        scopes: Mutex<Vec<Vec<String>>>,
        fail: bool,
        pending: bool,
        dropped: AtomicUsize,
        cancel_after_token: Option<Arc<AtomicBool>>,
    }

    impl fmt::Debug for Credential {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("credential-debug-must-not-be-formatted")
        }
    }

    // Spell out the Azure async-trait ABI to keep tests dependency-free.
    impl TokenCredential for Credential {
        fn get_token<'s, 'scopes, 'scope, 'options, 'future>(
            &'s self,
            scopes: &'scopes [&'scope str],
            options: Option<TokenRequestOptions<'options>>,
        ) -> Pin<Box<dyn Future<Output = azure_core::Result<AccessToken>> + Send + 'future>>
        where
            's: 'future,
            'scopes: 'future,
            'scope: 'future,
            'options: 'future,
            Self: 'future,
        {
            Box::pin(async move {
                assert!(options.is_none());
                self.scopes
                    .lock()
                    .unwrap()
                    .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
                if self.pending {
                    let _guard = DropCounter(&self.dropped);
                    pending::<()>().await;
                }
                if self.fail {
                    return Err(azure_core::Error::with_message(
                        ErrorKind::Other,
                        "private-provider-error",
                    ));
                }
                if let Some(cancelled) = &self.cancel_after_token {
                    cancelled.store(true, Ordering::SeqCst);
                }
                Ok(AccessToken::new(TEST_TOKEN, OffsetDateTime::UNIX_EPOCH))
            })
        }
    }

    struct DropCounter<'a>(&'a AtomicUsize);

    impl Drop for DropCounter<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct Execution {
        calls: usize,
        fail: bool,
        pending: bool,
        dropped: Arc<AtomicUsize>,
        cancel_after_execution: Option<Arc<AtomicBool>>,
    }

    impl AuthenticatedExecution for Execution {
        type Response = String;
        type Error = &'static str;

        fn execute<'a>(
            &'a mut self,
            url: &'a Url,
            token: &'a AccessToken,
        ) -> Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'a>>
        {
            self.calls += 1;
            Box::pin(async move {
                assert_eq!(token.token.secret(), TEST_TOKEN);
                if self.pending {
                    let _guard = DropCounter(&self.dropped);
                    pending::<()>().await;
                }
                if let Some(cancelled) = &self.cancel_after_execution {
                    cancelled.store(true, Ordering::SeqCst);
                }
                if self.fail {
                    Err("private-executor-error")
                } else {
                    Ok(url.as_str().to_owned())
                }
            })
        }
    }

    fn selection(audience: Audience) -> CredentialSelection {
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap()
    }

    fn run_ready<F: Future>(future: F) -> F::Output {
        let mut future = pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("offline test future unexpectedly pending"),
        }
    }

    fn signal(cancelled: &AtomicBool) -> impl Future<Output = ()> + Send + '_ {
        poll_fn(move |_| {
            if cancelled.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
    }

    #[test]
    fn context_retains_identity_and_does_not_acquire_or_format_credentials() {
        let credential = Arc::new(Credential::default());
        let context = RequestContext::new(credential.clone(), selection(Audience::Arm));
        assert_eq!(context.endpoint().origin(), ServiceOrigin::Fabric);
        assert_eq!(context.selection().audience(), Audience::Arm);
        assert_eq!(Arc::strong_count(context.credential()), 2);
        assert!(credential.scopes.lock().unwrap().is_empty());
        for debug in [
            format!("{context:?}"),
            format!("{:?}", context.authentication()),
        ] {
            assert!(!debug.contains("credential-debug-must-not-be-formatted"));
            assert!(!debug.contains(TEST_TOKEN));
        }
    }

    #[test]
    fn validation_failures_precede_all_credential_and_execution_access() {
        let credential = Arc::new(Credential::default());
        let fabric = EndpointBinding::public(ServiceOrigin::Fabric);
        let url = fabric.url("/v1/items").unwrap();
        let mut execution = Execution::default();
        let mut never = pin!(pending());
        let context = RequestContext::new(credential.clone(), selection(Audience::Arm));
        assert_eq!(
            run_ready(context.execute(ServiceOrigin::Fabric, &url, &mut execution, never.as_mut())),
            Err(RequestError::InvalidAudience)
        );
        let context = RequestContext::new(credential.clone(), selection(Audience::Fabric));
        assert_eq!(
            run_ready(context.execute(
                ServiceOrigin::PowerBi,
                &url,
                &mut execution,
                never.as_mut()
            )),
            Err(RequestError::InvalidEndpoint)
        );
        for uri in [
            "https://dailyapi.fabric.microsoft.com/v1/items",
            "https://api.powerbi.com/v1/items",
            "http://api.fabric.microsoft.com/v1/items",
            "https://user@api.fabric.microsoft.com/v1/items",
            "https://api.fabric.microsoft.com:444/v1/items",
            "https://api.fabric.microsoft.com/v1/items#private",
        ] {
            assert_eq!(
                run_ready(context.execute(
                    ServiceOrigin::Fabric,
                    &Url::parse(uri).unwrap(),
                    &mut execution,
                    never.as_mut()
                )),
                Err(RequestError::InvalidEndpoint)
            );
        }
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert_eq!(execution.calls, 0);
    }

    #[test]
    fn every_origin_and_daily_override_requests_one_fixed_scope() {
        for origin in [
            ServiceOrigin::Fabric,
            ServiceOrigin::OneLake,
            ServiceOrigin::Arm,
            ServiceOrigin::PowerBi,
        ] {
            for endpoint in [
                EndpointBinding::public(origin),
                EndpointBinding::with_host(origin, "dailyapi.fabric.microsoft.com").unwrap(),
            ] {
                let credential = Arc::new(Credential::default());
                let context = RequestContext::with_endpoint(
                    credential.clone(),
                    selection(origin.audience()),
                    endpoint,
                );
                let url = context.endpoint().url("/an%2Fencoded/item?key=x").unwrap();
                let mut execution = Execution::default();
                assert_eq!(
                    run_ready(context.execute(origin, &url, &mut execution, pin!(pending()))),
                    Ok(url.as_str().to_owned())
                );
                assert_eq!(
                    *credential.scopes.lock().unwrap(),
                    vec![vec![origin.audience().scope().to_owned()]]
                );
                assert_eq!(execution.calls, 1);
            }
        }
    }

    #[test]
    fn cancellation_wins_before_validation_or_operation_polling() {
        let credential = Arc::new(Credential::default());
        let context = RequestContext::new(credential.clone(), selection(Audience::Arm));
        let mut execution = Execution::default();
        assert_eq!(
            run_ready(context.execute(
                ServiceOrigin::PowerBi,
                &Url::parse("http://example.com/").unwrap(),
                &mut execution,
                pin!(ready(()))
            )),
            Err(RequestError::Cancelled)
        );
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert_eq!(execution.calls, 0);
        let polled = AtomicBool::new(false);
        let operation = async { polled.store(true, Ordering::SeqCst) };
        assert_eq!(
            run_ready(cancel_with(operation, pin!(ready(())))),
            Err(Cancelled)
        );
        assert!(!polled.load(Ordering::SeqCst));
    }

    #[test]
    fn cancellation_after_token_prevents_executor_creation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let credential = Arc::new(Credential {
            cancel_after_token: Some(cancelled.clone()),
            ..Credential::default()
        });
        let context = RequestContext::new(credential.clone(), selection(Audience::Fabric));
        let url = context.endpoint().url("/v1/items").unwrap();
        let mut execution = Execution::default();
        assert_eq!(
            run_ready(context.execute(
                ServiceOrigin::Fabric,
                &url,
                &mut execution,
                pin!(signal(&cancelled))
            )),
            Err(RequestError::Cancelled)
        );
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert_eq!(execution.calls, 0);
    }

    #[test]
    fn cancellation_after_execution_suppresses_success_and_error() {
        for fail in [false, true] {
            let cancelled = Arc::new(AtomicBool::new(false));
            let context =
                RequestContext::new(Arc::new(Credential::default()), selection(Audience::Fabric));
            let url = context.endpoint().url("/v1/items").unwrap();
            let mut execution = Execution {
                fail,
                cancel_after_execution: Some(cancelled.clone()),
                ..Execution::default()
            };
            assert_eq!(
                run_ready(context.execute(
                    ServiceOrigin::Fabric,
                    &url,
                    &mut execution,
                    pin!(signal(&cancelled))
                )),
                Err(RequestError::Cancelled)
            );
            assert_eq!(execution.calls, 1);
        }
    }

    #[test]
    fn cancellation_drops_in_flight_credential_or_execution() {
        for credential_pending in [true, false] {
            let cancelled = AtomicBool::new(false);
            let credential = Arc::new(Credential {
                pending: credential_pending,
                ..Credential::default()
            });
            let context = RequestContext::new(credential.clone(), selection(Audience::Fabric));
            let url = context.endpoint().url("/v1/items").unwrap();
            let mut execution = Execution {
                pending: !credential_pending,
                ..Execution::default()
            };
            let mut cancellation = pin!(signal(&cancelled));
            {
                let mut request = pin!(context.execute(
                    ServiceOrigin::Fabric,
                    &url,
                    &mut execution,
                    cancellation.as_mut()
                ));
                let mut cx = Context::from_waker(Waker::noop());
                assert!(request.as_mut().poll(&mut cx).is_pending());
                cancelled.store(true, Ordering::SeqCst);
                assert_eq!(
                    request.as_mut().poll(&mut cx),
                    Poll::Ready(Err(RequestError::Cancelled))
                );
            }
            assert_eq!(
                credential.dropped.load(Ordering::SeqCst),
                usize::from(credential_pending)
            );
            assert_eq!(
                execution.dropped.load(Ordering::SeqCst),
                usize::from(!credential_pending)
            );
            assert_eq!(execution.calls, usize::from(!credential_pending));
        }
    }

    #[test]
    fn credential_and_executor_failures_are_distinct_and_sanitized() {
        for credential_fail in [true, false] {
            let credential = Arc::new(Credential {
                fail: credential_fail,
                ..Credential::default()
            });
            let context = RequestContext::new(credential, selection(Audience::Fabric));
            let url = context.endpoint().url("/v1/items").unwrap();
            let mut execution = Execution {
                fail: true,
                ..Execution::default()
            };
            let result = run_ready(context.execute(
                ServiceOrigin::Fabric,
                &url,
                &mut execution,
                pin!(pending()),
            ));
            assert_eq!(
                result,
                Err(if credential_fail {
                    RequestError::Credential
                } else {
                    RequestError::Execution("private-executor-error")
                })
            );
            let error = result.unwrap_err();
            assert!(!format!("{error:?} {error}").contains("private-"));
            assert_eq!(execution.calls, usize::from(!credential_fail));
        }
    }

    #[test]
    fn request_errors_compare_payloads_without_requiring_payload_debug() {
        #[derive(Eq, PartialEq)]
        struct PrivateError(&'static str);

        assert_eq!(
            RequestError::<PrivateError>::InvalidEndpoint,
            RequestError::InvalidEndpoint
        );
        assert_eq!(
            RequestError::Execution(PrivateError("private-executor-error")),
            RequestError::Execution(PrivateError("private-executor-error"))
        );
        assert_ne!(
            RequestError::Execution(PrivateError("private-executor-error")),
            RequestError::Execution(PrivateError("different-private-error"))
        );
    }

    #[test]
    fn ready_request_preserves_three_cancellation_checkpoints() {
        let credential = Arc::new(Credential::default());
        let context = RequestContext::new(credential.clone(), selection(Audience::Fabric));
        let url = context.endpoint().url("/v1/items").unwrap();
        let mut execution = Execution::default();
        let polls = AtomicUsize::new(0);
        let cancellation = poll_fn(|_| {
            polls.fetch_add(1, Ordering::SeqCst);
            Poll::Pending
        });
        assert_eq!(
            run_ready(context.execute(
                ServiceOrigin::Fabric,
                &url,
                &mut execution,
                pin!(cancellation)
            )),
            Ok(url.as_str().to_owned())
        );
        assert_eq!(polls.load(Ordering::SeqCst), 3);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert_eq!(execution.calls, 1);
    }
}
