// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

//! Grouped service parsing, approved native execution and receipt output contracts.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_resources::service::{
    self as cli, DomainResolver, ExecuteError, ResolveFuture,
    service::{
        DomainTarget, InputError, OperationExecutor, OperationLimits, RequestContext,
        v2::{
            ApprovalPolicy, Outcome, PreparedOperation, Registry, ResolvedTarget, Target,
            TargetKind, TenantIdentity,
        },
    },
};
use fabric_sdk_api::{Audience, CredentialSelection};
use fabric_sdk_operations::{OperationExecutionError, OperationRequest, OperationResponse};
use std::{
    collections::BTreeMap,
    future::{self, Future},
    io::{self, Write},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use azure_core::{
    Bytes,
    http::{AsyncRawResponse, HttpClient, Method, Request, headers::Headers},
};
use cli::service::{EndpointBinding, ServiceOrigin};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use std::sync::Mutex;

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const DOMAIN: &str = "22222222-2222-2222-2222-222222222222";

#[derive(Debug)]
struct Credential;
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Ok(AccessToken::new(
            "offline-test-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Resolver {
    calls: usize,
    failure: bool,
}
impl DomainResolver for Resolver {
    type Error = &'static str;
    fn resolve<'a>(&'a mut self, path: &'a str) -> ResolveFuture<'a, Self::Error> {
        self.calls += 1;
        let result = if self.failure {
            Err("AccessDenied")
        } else {
            DomainTarget::new(
                ResolvedTarget::tenant_collection(
                    path,
                    TenantIdentity::new(TENANT).unwrap(),
                    Target {
                        kind: TargetKind::TenantCollection,
                        owner_type: "Domain".into(),
                        facet: "roleAssignments".into(),
                        representation: "native".into(),
                        generation: None,
                    },
                )
                .unwrap(),
                DOMAIN,
            )
            .map_err(|_| "WrongOwner")
        };
        Box::pin(future::ready(result))
    }
}
struct Policy(bool);
impl ApprovalPolicy for Policy {
    type Error = &'static str;
    fn authorize(&mut self, _: &PreparedOperation) -> Result<(), Self::Error> {
        if self.0 {
            Ok(())
        } else {
            Err("ApprovalDenied")
        }
    }
}
#[derive(Clone)]
struct Executor {
    sends: Arc<AtomicUsize>,
    reply: Result<OperationResponse, OperationExecutionError>,
}
impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.sends.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.body(), br#"{"role":"Contributor"}"#);
        assert!(request.url().path().contains(DOMAIN));
        Box::pin(future::ready(self.reply.clone()))
    }
    fn executor(sends: Arc<AtomicUsize>, response_loss: bool) -> Executor {
        Executor {
            sends,
            reply: if response_loss {
                Err(OperationExecutionError::Failed)
            } else {
                Ok(OperationResponse {
                    status: 200,
                    body: vec![],
                    headers: BTreeMap::new(),
                })
            },
        }
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("Domain sync has no observation loop")
    }
}
fn context() -> RequestContext {
    RequestContext::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", TENANT, Audience::Fabric).unwrap(),
    )
}

#[test]
fn only_grouped_domain_contributor_syntax_is_registered() {
    let command = cli::parse_args([
        "fab-cli",
        "service",
        "domain",
        "sync",
        "/.domains/Contoso.Domain",
        "--role",
        "Contributor",
    ])
    .unwrap();
    assert_eq!(command.path, "/.domains/Contoso.Domain");
    assert_eq!(command.role, "Contributor");
    for args in [
        vec!["service", "domain", "sync", "/domain", "--role", "Admin"],
        vec!["service", "domain", "sync", "/domain"],
        vec!["service", "sync", "/domain", "--role", "Contributor"],
        vec![
            "service",
            "gateway",
            "sync",
            "/gateway",
            "--role",
            "Contributor",
        ],
        vec!["service", "api", "admin/domains"],
        vec!["service", "serve"],
        vec!["service", "capacity", "activate", "/capacity"],
        vec![
            "service",
            "domain",
            "sync",
            "/domain",
            "--role",
            "Contributor",
            "--force",
        ],
    ] {
        assert!(cli::parse_args(std::iter::once("fab-cli").chain(args)).is_err());
    }
    let help = cli::service_command().render_long_help().to_string();
    assert!(help.contains("domain"));
    assert!(!help.contains("serve"));
}

#[tokio::test]
async fn role_resolution_and_approval_denials_never_send() {
    let mut registry = Registry::default();
    cli::service::register(&mut registry).unwrap();
    let mut resolver = Resolver {
        calls: 0,
        failure: false,
    };
    let mut command = cli::SyncCommand {
        path: "/.domains/Contoso.Domain".into(),
        role: "Admin".into(),
    };
    assert!(matches!(
        cli::prepare(&command, &registry, &mut resolver).await,
        Err(ExecuteError::Input(InputError::Role))
    ));
    assert_eq!(resolver.calls, 0);
    command.role = "Contributor".into();
    resolver.failure = true;
    assert!(matches!(
        cli::prepare(&command, &registry, &mut resolver).await,
        Err(ExecuteError::Resolution("AccessDenied"))
    ));
    resolver.failure = false;
    let prepared = cli::prepare(&command, &registry, &mut resolver)
        .await
        .unwrap();
    let sends = Arc::new(AtomicUsize::new(0));
    assert!(matches!(
        cli::execute(
            &prepared,
            &mut Policy(false),
            &context(),
            executor(sends.clone(), false),
            OperationLimits::default(),
            future::pending()
        )
        .await,
        Err(ExecuteError::Approval("ApprovalDenied"))
    ));
    assert_eq!(sends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn grouped_parse_through_native_dispatch_retains_receipt_and_streams() {
    let mut registry = Registry::default();
    cli::service::register(&mut registry).unwrap();
    let command = cli::parse_args([
        "fab-cli",
        "service",
        "domain",
        "sync",
        "/.domains/Contoso.Domain",
        "--role",
        "Contributor",
    ])
    .unwrap();
    let prepared = cli::prepare(
        &command,
        &registry,
        &mut Resolver {
            calls: 0,
            failure: false,
        },
    )
    .await
    .unwrap();
    for response_loss in [false, true] {
        let sends = Arc::new(AtomicUsize::new(0));
        let report = cli::execute(
            &prepared,
            &mut Policy(true),
            &context(),
            executor(sends.clone(), response_loss),
            OperationLimits::default(),
            future::pending(),
        )
        .await
        .unwrap();
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        assert_eq!(cli::exit_code(&report), u8::from(response_loss));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        cli::write_report(report, &mut stdout, &mut stderr).unwrap();
        let selected = if response_loss {
            assert!(stdout.is_empty());
            stderr
        } else {
            assert!(stderr.is_empty());
            stdout
        };
        let value: serde_json::Value = serde_json::from_slice(&selected).unwrap();
        assert_eq!(
            value["status"],
            if response_loss {
                "unknown"
            } else {
                "succeeded"
            }
        );
        assert_eq!(value["meta"]["operationId"], cli::service::OPERATION_ID);
        assert_eq!(value["meta"]["sources"][0]["details"]["domainId"], DOMAIN);
        assert_eq!(
            value["meta"]["sources"][0]["details"]["role"],
            "Contributor"
        );
        assert_eq!(value["meta"]["target"]["canonicalPath"], command.path);
        assert!(
            value["meta"]["evidence"]["handles"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic writer failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn writer_failure_preserves_completed_action_without_replay() {
    let mut registry = Registry::default();
    cli::service::register(&mut registry).unwrap();
    let prepared = cli::prepare(
        &cli::SyncCommand {
            path: "/.domains/Contoso.Domain".into(),
            role: "Contributor".into(),
        },
        &registry,
        &mut Resolver {
            calls: 0,
            failure: false,
        },
    )
    .await
    .unwrap();
    let sends = Arc::new(AtomicUsize::new(0));
    let report = cli::execute(
        &prepared,
        &mut Policy(true),
        &context(),
        executor(sends.clone(), false),
        OperationLimits::default(),
        future::pending(),
    )
    .await
    .unwrap();
    let failure = cli::write_report(report, &mut BrokenWriter, &mut Vec::new()).unwrap_err();
    assert!(matches!(
        failure.report.receipt.outcome(),
        Outcome::Succeeded { .. }
    ));
    assert_eq!(failure.report.receipt.evidence().http_status, Some(200));
    assert_eq!(sends.load(Ordering::SeqCst), 1);
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(future::pending())
    }
}

#[derive(Debug)]
struct Wire {
    status: u16,
    requests: Mutex<Vec<Request>>,
}
#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let mut headers = Headers::new();
        headers.insert("Retry-After", "1");
        Ok(AsyncRawResponse::from_bytes(
            self.status.into(),
            headers,
            if self.status == 200 {
                vec![]
            } else {
                br#"{"errorCode":"DomainDenied"}"#.to_vec()
            },
        ))
    }
}

#[tokio::test]
async fn actual_shared_http_adapter_preserves_context_and_never_retries_post() {
    let mut registry = Registry::default();
    cli::service::register(&mut registry).unwrap();
    let prepared = cli::prepare(
        &cli::SyncCommand {
            path: "/.domains/Contoso.Domain".into(),
            role: "Contributor".into(),
        },
        &registry,
        &mut Resolver {
            calls: 0,
            failure: false,
        },
    )
    .await
    .unwrap();
    for host in ["api.fabric.microsoft.com", "dailyapi.fabric.microsoft.com"] {
        for status in [200, 403, 429] {
            let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap();
            let context = RequestContext::with_endpoint(
                Arc::new(Credential),
                CredentialSelection::new("offline", TENANT, Audience::Fabric).unwrap(),
                endpoint.clone(),
            );
            let wire = Arc::new(Wire {
                status,
                requests: Mutex::new(Vec::new()),
            });
            let executor =
                HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default())
                    .unwrap()
                    .with_endpoint(endpoint);
            let report = cli::execute(
                &prepared,
                &mut Policy(true),
                &context,
                executor,
                OperationLimits::default(),
                future::pending(),
            )
            .await
            .unwrap();
            assert_eq!(report.receipt.evidence().http_status, Some(status));
            assert_eq!(cli::exit_code(&report), u8::from(status != 200));
            let requests = wire.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method(), Method::Post);
            assert_eq!(requests[0].url().host_str(), Some(host));
            assert_eq!(
                requests[0].url().path(),
                format!("/v1/admin/domains/{DOMAIN}/roleAssignments/syncToSubdomains")
            );
            assert_eq!(
                Bytes::from(requests[0].body()).as_ref(),
                br#"{"role":"Contributor"}"#
            );
            assert_eq!(
                requests[0]
                    .headers()
                    .get_str(&"content-type".into())
                    .unwrap(),
                "application/json"
            );
        }

        #[tokio::test]
        async fn contradictory_void_responses_and_protocol_statuses_never_exit_zero() {
            let mut registry = Registry::default();
            cli::service::register(&mut registry).unwrap();
            let prepared = cli::prepare(
                &cli::SyncCommand {
                    path: "/.domains/Contoso.Domain".into(),
                    role: "Contributor".into(),
                },
                &registry,
                &mut Resolver {
                    calls: 0,
                    failure: false,
                },
            )
            .await
            .unwrap();
            for (status, body) in [
                (200, b"[]".as_slice()),
                (200, b"malformed"),
                (200, br#"{"errorCode":"Denied"}"#),
                (200, br#"{"status":"Pending"}"#),
                (206, b""),
                (302, b""),
            ] {
                let sends = Arc::new(AtomicUsize::new(0));
                let report = cli::execute(
                    &prepared,
                    &mut Policy(true),
                    &context(),
                    Executor {
                        sends: sends.clone(),
                        reply: Ok(OperationResponse {
                            status,
                            body: body.to_vec(),
                            headers: BTreeMap::new(),
                        }),
                    },
                    OperationLimits::default(),
                    future::pending(),
                )
                .await
                .unwrap();
                assert_eq!(cli::exit_code(&report), 1);
                assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
                assert!(report.receipt.evidence().mutation_may_have_occurred);
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                cli::write_report(report, &mut stdout, &mut stderr).unwrap();
                assert!(stdout.is_empty());
                let receipt: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
                assert_eq!(receipt["meta"]["sources"][0]["details"]["domainId"], DOMAIN);
                assert_eq!(sends.load(Ordering::SeqCst), 1);
            }
        }

        #[tokio::test]
        async fn receipt_failure_retains_approved_domain_after_native_dispatch() {
            let mut registry = Registry::default();
            cli::service::register(&mut registry).unwrap();
            let prepared = cli::prepare(
                &cli::SyncCommand {
                    path: "/.domains/Contoso.Domain".into(),
                    role: "Contributor".into(),
                },
                &registry,
                &mut Resolver {
                    calls: 0,
                    failure: false,
                },
            )
            .await
            .unwrap();
            let sends = Arc::new(AtomicUsize::new(0));
            let failure = cli::execute(
                &prepared,
                &mut Policy(true),
                &context(),
                Executor {
                    sends: sends.clone(),
                    reply: Ok(OperationResponse {
                        status: 600,
                        body: vec![],
                        headers: BTreeMap::new(),
                    }),
                },
                OperationLimits::default(),
                future::pending(),
            )
            .await
            .unwrap_err();
            let ExecuteError::Native(cli::service::SyncError::Receipt {
                prepared: retained,
                source,
                cause,
                ..
            }) = failure
            else {
                panic!("expected receipt-validation failure")
            };
            assert_eq!(*retained, prepared);
            assert_eq!(retained.input()["domainId"], DOMAIN);
            assert_eq!(retained.input()["role"], "Contributor");
            assert_eq!(
                retained.target().canonical_path(),
                "/.domains/Contoso.Domain"
            );
            assert_eq!(source.details.as_ref().unwrap()["domainId"], DOMAIN);
            assert!(cause.evidence.mutation_may_have_occurred);
            assert_eq!(sends.load(Ordering::SeqCst), 1);
        }
    }
}
