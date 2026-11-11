// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

//! Actual host -> admin resolver -> immediate approval -> shared HTTP contracts.

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::v2_service_domain_host::{PreparedDomainSync, ResolveError};
use fabric_cli_resources::service::{
    self as command, ExecuteError,
    service::{
        Audience, EndpointBinding, InputError, RequestContext, ServiceOrigin,
        v2::{ApprovalPolicy, Outcome, Precondition, PreparedOperation},
    },
};
use fabric_sdk::CredentialSelection;
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{pending, ready},
    io::{self, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const DOMAIN: &str = "22222222-2222-2222-2222-222222222222";
const PATH: &str = "/.domains/Finance.Domain";

#[derive(Debug)]
struct Credential(AtomicUsize);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new("offline", OffsetDateTime::UNIX_EPOCH))
    }
}

#[derive(Debug)]
struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        assert_eq!(
            delay,
            ExecutionOptions::default().timeout,
            "no retry/poll wait"
        );
        Box::pin(pending())
    }
}

#[derive(Debug)]
enum Reply {
    Response(u16, Vec<u8>),
    Continued,
    Lost,
    Pending,
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Request>>,
    dispatched: tokio::sync::Notify,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        self.dispatched.notify_one();
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("bounded requests");
        match reply {
            Reply::Response(status, body) => {
                let mut headers = Headers::new();
                headers.insert("retry-after", "1");
                Ok(AsyncRawResponse::from_bytes(status.into(), headers, body))
            }
            Reply::Lost => Err(azure_core::Error::message(
                azure_core::error::ErrorKind::Io,
                "offline response loss",
            )),
            Reply::Continued => {
                let mut headers = Headers::new();
                headers.insert("x-ms-continuation", "next");
                Ok(AsyncRawResponse::from_bytes(
                    200.into(),
                    headers,
                    serde_json::to_vec(&json!({"domains":[metadata()]})).unwrap(),
                ))
            }
            Reply::Pending => pending().await,
        }
    }
}

struct Fixture {
    credential: Arc<Credential>,
    context: RequestContext,
    wire: Arc<Wire>,
    executor: Arc<HttpExecutor>,
}

impl Fixture {
    fn new(replies: Vec<Reply>, host: &str) -> Self {
        let credential = Arc::new(Credential(AtomicUsize::new(0)));
        let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap();
        let context = RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("offline", TENANT, Audience::Fabric).unwrap(),
            endpoint.clone(),
        );
        let wire = Arc::new(Wire {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
            dispatched: tokio::sync::Notify::new(),
        });
        let executor = Arc::new(
            HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default())
                .unwrap()
                .with_endpoint(endpoint),
        );
        Self {
            credential,
            context,
            wire,
            executor,
        }
    }

    async fn prepare(&self) -> PreparedDomainSync {
        PreparedDomainSync::prepare(
            &sync_command(PATH, "Contributor"),
            &self.context,
            self.executor.clone(),
            pending(),
        )
        .await
        .unwrap()
    }

    fn posts(&self) -> usize {
        self.wire
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method() == Method::Post)
            .count()
    }
}

fn sync_command(path: &str, role: &str) -> command::SyncCommand {
    command::SyncCommand {
        path: path.into(),
        role: role.into(),
    }
}

fn metadata() -> Value {
    json!({"id":DOMAIN,"displayName":"Finance","parentDomainId":null})
}

fn response(value: Value) -> Reply {
    Reply::Response(200, serde_json::to_vec(&value).unwrap())
}

fn replies(last: Reply) -> Vec<Reply> {
    vec![
        response(json!({"domains":[metadata()]})),
        response(metadata()),
        last,
    ]
}

struct Policy {
    expected: PreparedOperation,
    allow: bool,
    calls: usize,
}

impl Policy {
    fn new(prepared: &PreparedDomainSync, allow: bool) -> Self {
        Self {
            expected: prepared.operation().clone(),
            allow,
            calls: 0,
        }
    }
}

impl ApprovalPolicy for Policy {
    type Error = &'static str;
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        self.calls += 1;
        assert_eq!(operation, &self.expected);
        assert_eq!(
            operation.input(),
            &json!({"domainId":DOMAIN,"role":"Contributor"})
        );
        assert_eq!(
            operation.preconditions(),
            [
                Precondition::ExistingOnly,
                Precondition::UnguardedConcurrency
            ]
        );
        if self.allow { Ok(()) } else { Err("denied") }
    }
}

#[tokio::test]
async fn actual_host_resolves_admin_identity_and_posts_exactly_once_on_selected_endpoint() {
    for host in ["api.fabric.microsoft.com", "dailyapi.fabric.microsoft.com"] {
        let fixture = Fixture::new(replies(Reply::Response(200, vec![])), host);
        let prepared = fixture.prepare().await;
        let mut policy = Policy::new(&prepared, true);
        let report = prepared
            .execute(&fixture.context, &mut policy, pending())
            .await
            .unwrap();
        assert_eq!(command::exit_code(&report), 0);
        assert_eq!(policy.calls, 1);
        assert_eq!(fixture.posts(), 1);
        let requests = fixture.wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].method(), Method::Get);
        assert_eq!(requests[0].url().path(), "/v1/admin/domains");
        assert_eq!(requests[1].method(), Method::Get);
        assert_eq!(
            requests[1].url().path(),
            format!("/v1/admin/domains/{DOMAIN}")
        );
        let post = &requests[2];
        assert_eq!(post.url().host_str(), Some(host));
        assert_eq!(
            post.url().path(),
            format!("/v1/admin/domains/{DOMAIN}/roleAssignments/syncToSubdomains")
        );
        assert!(post.url().query().is_none());
        assert_eq!(
            Bytes::from(post.body()).as_ref(),
            br#"{"role":"Contributor"}"#
        );
        assert_eq!(
            post.headers().get_str(&"content-type".into()).unwrap(),
            "application/json"
        );
        assert!(post.headers().get_str(&"if-match".into()).is_err());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        command::write_report(report, &mut stdout, &mut stderr).unwrap();
        assert!(stderr.is_empty());
        let receipt: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(receipt["meta"]["sources"][0]["details"]["domainId"], DOMAIN);
        assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 3);
    }
}

#[tokio::test]
async fn local_preflight_rejects_role_path_tenant_audience_and_executor_before_credentials_or_send()
{
    let fixture = Fixture::new(vec![], "api.fabric.microsoft.com");
    for path in [
        "relative.Domain",
        "/.domains/X.Workspace",
        "/.domains/.Domain",
        "/.domains/../X.Domain",
        "/.domains/X.Domain/roleAssignments",
        "/.domains/X%2fY.Domain",
        "/.domains/X\n.Domain",
        "/Other.Workspace/X.Domain",
    ] {
        assert!(
            PreparedDomainSync::prepare(
                &sync_command(path, "Contributor"),
                &fixture.context,
                fixture.executor.clone(),
                pending()
            )
            .await
            .is_err()
        );
    }
    assert!(
        PreparedDomainSync::prepare(
            &sync_command(PATH, "Admin"),
            &fixture.context,
            fixture.executor.clone(),
            pending()
        )
        .await
        .is_err()
    );
    for (tenant, audience, endpoint) in [
        (
            "not-a-tenant",
            Audience::Fabric,
            fixture.context.endpoint().clone(),
        ),
        (TENANT, Audience::Arm, fixture.context.endpoint().clone()),
        (
            TENANT,
            Audience::Fabric,
            EndpointBinding::public(ServiceOrigin::Arm),
        ),
        (
            TENANT,
            Audience::Fabric,
            EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com")
                .unwrap(),
        ),
    ] {
        let context = RequestContext::with_endpoint(
            fixture.credential.clone(),
            CredentialSelection::new("offline", tenant, audience).unwrap(),
            endpoint,
        );
        assert!(matches!(
            PreparedDomainSync::prepare(
                &sync_command(PATH, "Contributor"),
                &context,
                fixture.executor.clone(),
                pending()
            )
            .await,
            Err(ResolveError::Context)
        ));
    }
    assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 0);
    assert!(fixture.wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn explicit_uuid_requires_verified_singleton_not_tenant_discovery() {
    let fixture = Fixture::new(
        vec![response(metadata()), Reply::Response(200, vec![])],
        "api.fabric.microsoft.com",
    );
    let prepared = PreparedDomainSync::prepare(
        &sync_command(&format!("/.domains/{DOMAIN}.Domain"), "Contributor"),
        &fixture.context,
        fixture.executor.clone(),
        pending(),
    )
    .await
    .unwrap();
    let mut policy = Policy::new(&prepared, true);
    prepared
        .execute(&fixture.context, &mut policy, pending())
        .await
        .unwrap();
    assert_eq!(fixture.wire.requests.lock().unwrap().len(), 2);
    assert_eq!(fixture.posts(), 1);
}

#[tokio::test]
async fn admin_denial_ambiguity_continuation_bounds_and_identity_changes_never_post() {
    for script in [
        vec![Reply::Continued],
        vec![Reply::Response(403, br#"{"errorCode":"Denied"}"#.to_vec())],
        vec![response(json!({"domains":[metadata(),metadata()]}))],
        vec![response(
            json!({"domains":[metadata()],"continuationToken":"next"}),
        )],
        vec![response(json!({"domains":[]}))],
        vec![response(
            json!({"domains":[{"id":"invalid","displayName":"Finance"}]}),
        )],
        vec![response(json!({"domains":vec![metadata(); 1001]}))],
        vec![Reply::Response(200, vec![b' '; 1024 * 1024 + 1])],
        vec![
            response(json!({"domains":[metadata()]})),
            Reply::Response(403, b"{}".to_vec()),
        ],
        vec![
            response(json!({"domains":[metadata()]})),
            response(json!({"id":DOMAIN,"displayName":"Changed"})),
        ],
        vec![
            response(json!({"domains":[metadata()]})),
            response(json!({"id":TENANT,"displayName":"Finance"})),
        ],
    ] {
        let fixture = Fixture::new(script, "api.fabric.microsoft.com");
        assert!(
            PreparedDomainSync::prepare(
                &sync_command(PATH, "Contributor"),
                &fixture.context,
                fixture.executor.clone(),
                pending()
            )
            .await
            .is_err()
        );
        assert_eq!(fixture.posts(), 0);
    }

    #[tokio::test]
    async fn exact_row_and_byte_limits_are_accepted_without_traversal() {
        let mut rows: Vec<_> = (0..999)
            .map(|id| {
                json!({
                    "id":format!("00000000-0000-0000-0000-{id:012x}"),
                    "displayName":format!("Other{id}")
                })
            })
            .collect();
        rows.push(metadata());
        let mut bytes = serde_json::to_vec(&json!({"domains":rows})).unwrap();
        bytes.resize(1024 * 1024, b' ');
        let fixture = Fixture::new(
            vec![
                Reply::Response(200, bytes),
                response(metadata()),
                Reply::Response(200, vec![]),
            ],
            "api.fabric.microsoft.com",
        );
        let prepared = fixture.prepare().await;
        let mut policy = Policy::new(&prepared, true);
        let report = prepared
            .execute(&fixture.context, &mut policy, pending())
            .await
            .unwrap();
        assert_eq!(command::exit_code(&report), 0);
        assert_eq!(fixture.posts(), 1);
        assert_eq!(fixture.wire.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn approval_denial_and_each_context_swap_cannot_acquire_a_mutation_token_or_post() {
    for swap in 0..6 {
        let fixture = Fixture::new(
            replies(Reply::Response(200, vec![])),
            "api.fabric.microsoft.com",
        );
        let prepared = fixture.prepare().await;
        let mut policy = Policy::new(&prepared, swap != 0);
        let mut context = fixture.context.clone();
        if swap != 0 {
            context = RequestContext::with_endpoint(
                if swap == 1 {
                    Arc::new(Credential(AtomicUsize::new(0)))
                } else {
                    fixture.context.credential().clone()
                },
                CredentialSelection::new(
                    if swap == 2 {
                        "different-account"
                    } else {
                        "offline"
                    },
                    if swap == 3 { DOMAIN } else { TENANT },
                    if swap == 4 {
                        Audience::Arm
                    } else {
                        Audience::Fabric
                    },
                )
                .unwrap(),
                if swap == 5 {
                    EndpointBinding::with_host(
                        ServiceOrigin::Fabric,
                        "dailyapi.fabric.microsoft.com",
                    )
                    .unwrap()
                } else {
                    fixture.context.endpoint().clone()
                },
            );
        }
        let result = prepared.execute(&context, &mut policy, pending()).await;
        if swap == 0 {
            assert!(matches!(result, Err(ExecuteError::Approval("denied"))));
        } else {
            assert!(matches!(
                result,
                Err(ExecuteError::Input(InputError::Context))
            ));
        }
        assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.posts(), 0);
    }
}

struct BrokenOutput {
    flush_only: bool,
}
impl Write for BrokenOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.flush_only {
            Ok(bytes.len())
        } else {
            Err(io::Error::other("offline output failure"))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("offline flush failure"))
    }
}

#[tokio::test]
async fn output_write_or_flush_failure_retains_the_executed_report_without_replay() {
    for flush_only in [false, true] {
        let fixture = Fixture::new(
            replies(Reply::Response(200, vec![])),
            "api.fabric.microsoft.com",
        );
        let prepared = fixture.prepare().await;
        let mut policy = Policy::new(&prepared, true);
        let report = prepared
            .execute(&fixture.context, &mut policy, pending())
            .await
            .unwrap();
        let failure =
            command::write_report(report, &mut BrokenOutput { flush_only }, &mut Vec::new())
                .unwrap_err();
        assert!(matches!(
            failure.report.receipt.outcome(),
            Outcome::Succeeded { .. }
        ));
        assert_eq!(
            failure.report.receipt.sources()[0]
                .details
                .as_ref()
                .unwrap()["domainId"],
            DOMAIN
        );
        assert_eq!(fixture.posts(), 1);
    }
}

#[tokio::test]
async fn uncertain_loss_throttle_and_accepted_are_never_replayed_or_polled() {
    for reply in [
        Reply::Lost,
        Reply::Response(429, b"{}".to_vec()),
        Reply::Response(202, vec![]),
    ] {
        let fixture = Fixture::new(replies(reply), "api.fabric.microsoft.com");
        let prepared = fixture.prepare().await;
        let mut policy = Policy::new(&prepared, true);
        let report = prepared
            .execute(&fixture.context, &mut policy, pending())
            .await
            .unwrap();
        assert_eq!(command::exit_code(&report), 1);
        assert_eq!(fixture.posts(), 1);
        assert_eq!(fixture.wire.requests.lock().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn cancellation_before_lookup_before_post_and_after_dispatch_never_replays() {
    let fixture = Fixture::new(vec![], "api.fabric.microsoft.com");
    assert!(
        PreparedDomainSync::prepare(
            &sync_command(PATH, "Contributor"),
            &fixture.context,
            fixture.executor.clone(),
            ready(())
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 0);
    assert!(fixture.wire.requests.lock().unwrap().is_empty());

    let fixture = Fixture::new(
        replies(Reply::Response(200, vec![])),
        "api.fabric.microsoft.com",
    );
    let prepared = fixture.prepare().await;
    let mut policy = Policy::new(&prepared, true);
    let report = prepared
        .execute(&fixture.context, &mut policy, ready(()))
        .await
        .unwrap();
    assert_eq!(command::exit_code(&report), 1);
    assert_eq!(fixture.posts(), 0);
    assert!(!report.receipt.evidence().mutation_may_have_occurred);

    let fixture = Fixture::new(replies(Reply::Pending), "api.fabric.microsoft.com");
    let prepared = fixture.prepare().await;
    let mut policy = Policy::new(&prepared, true);
    let cancelled = async {
        while fixture.posts() == 0 {
            fixture.wire.dispatched.notified().await;
        }
    };
    let report = prepared
        .execute(&fixture.context, &mut policy, cancelled)
        .await
        .unwrap();
    assert!(matches!(report.receipt.outcome(), Outcome::Unknown { .. }));
    assert!(report.receipt.evidence().mutation_may_have_occurred);
    assert_eq!(fixture.posts(), 1);
    assert_eq!(fixture.wire.requests.lock().unwrap().len(), 3);
}

#[test]
#[cfg(feature = "headless")]
fn executable_mount_preserves_selected_grammar_and_refuses_unapproved_nonterminal_execution() {
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    let help = run(&["service", "domain", "sync", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout).unwrap().contains("--role"));
    for args in [
        vec!["service", "domain", "sync", PATH, "--role", "Admin"],
        vec!["service", "domain", "sync", PATH],
        vec![
            "service",
            "domain",
            "sync",
            PATH,
            "--role",
            "Contributor",
            "--force",
        ],
        vec![
            "service",
            "domain",
            "sync",
            "/wrong",
            "--role",
            "Contributor",
        ],
    ] {
        let result = run(&args);
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
    }
    let result = run(&["service", "domain", "sync", PATH, "--role", "Contributor"]);
    assert_eq!(result.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error"]["code"], "ApprovalRequired");
}
