// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[allow(dead_code, unused_imports)]
mod folder_support;

use fabric_cli::{
    ReadConfig, jobs::JobReadSession,
    parser::{Invocation, parse_args}, session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_jobs::HttpJobExecutor;
use fabric_cli_output::{Timestamp, text::TextPlan};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use folder_support::{Credential, Http, Reply, Timer, complete, executor, reply, sdk};
use serde_json::{Value, json};
use std::{
    future::{Future, pending, poll_fn, ready},
    io::{self, Write},
    sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}},
    task::Poll,
    time::Duration,
};

fn metadata() -> Vec<Reply> {
    vec![
        reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]})),
        reply(json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]})),
    ]
}

fn dispatch(
    args: &[&str],
    http: Arc<Http>,
    credential: Arc<Credential>,
    cancelled: impl Future<Output = ()> + Send,
) -> TextPlan {
    let executor = executor(http, Arc::new(Timer::default()));
    dispatch_with(args, &executor, credential, "/", HomeContext::Unspecified, cancelled)
}

fn dispatch_with(
    args: &[&str], executor: &HttpExecutor, credential: Arc<Credential>,
    current: &str, home: HomeContext, cancelled: impl Future<Output = ()> + Send,
) -> TextPlan {
    let Invocation::JobRead(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else { panic!("job read required") };
    let sdk = sdk(credential);
    let reads = ReadSession {
        sdk: &sdk, executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: current, home,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    complete(JobReadSession {
        reads: &reads,
        jobs: Arc::new(HttpJobExecutor::new(executor.clone())),
    }.dispatch(&command, cancelled)).unwrap()
}

fn streams(plan: &TextPlan) -> (Vec<u8>, Vec<u8>) {
    let (mut out, mut err) = (vec![], vec![]);
    plan.write_to(&mut out, &mut err).unwrap();
    (out, err)
}

fn data(plan: &TextPlan) -> Value {
    let (out, err) = streams(plan);
    assert!(err.is_empty());
    serde_json::from_slice(&out).unwrap()
}

#[test]
fn list_pages_reuse_the_real_pager_credential_and_source_projection() {
    let mut replies = metadata();
    replies.extend([
        reply(json!({"value":[{"id":"z","status":null}],"continuationToken":"a +/="})),
        reply(json!({"value":[{"id":"a","later":true}]})),
    ]);
    let http = Arc::new(Http::new(replies));
    let credential = Arc::new(Credential::default());
    let plan = dispatch(&["job","run-list","Demo.Workspace/Book.Notebook"], http.clone(), credential.clone(), pending());
    assert_eq!(plan.exit_code, 0);
    assert_eq!(data(&plan)["result"]["data"], json!([{"id":"z","status":null},{"id":"a"}]));
    assert_eq!(*http.requests.lock().unwrap(), [
        "https://api.fabric.microsoft.com/v1/workspaces",
        "https://api.fabric.microsoft.com/v1/workspaces/w/items",
        "https://api.fabric.microsoft.com/v1/workspaces/w/items/n/jobs/instances",
        "https://api.fabric.microsoft.com/v1/workspaces/w/items/n/jobs/instances?continuationToken=a+%2B%2F%3D",
    ]);
    assert_eq!(credential.scopes.lock().unwrap().len(), 4);
    for headers in http.headers.lock().unwrap().iter() {
        for name in ["x-ms-fabric-skill", "x-ms-fabric-skill-version"] {
            assert!(headers.get_str(&name.into()).is_err());
        }
    }
    http.assert_consumed();
}

#[test]
fn later_page_failure_never_emits_partial_success() {
    let mut replies = metadata();
    replies.extend([
        reply(json!({"value":[{"id":"partial"}],"continuationToken":"next"})),
        Reply { status: 403, body: json!({"errorCode":"Forbidden","message":"private"}), headers: Default::default() },
    ]);
    let http = Arc::new(Http::new(replies));
    let plan = dispatch(&["job","run-list","Demo.Workspace/Book.Notebook"], http.clone(), Arc::default(), pending());
    assert_eq!(plan.exit_code, 1);
    let result = data(&plan);
    assert_eq!(result["command"], "job run-list");
    assert_eq!(result["result"]["error_code"], "Forbidden");
    assert!(result["result"].get("data").is_none());
    assert!(!String::from_utf8_lossy(&streams(&plan).0).contains("partial"));
    http.assert_consumed();
}

#[test]
fn unauthorized_exit_four_and_service_error_envelopes_retain_leaf_paths() {
    for (status, code, exit) in [(401,"Unauthorized",4),(403,"Forbidden",1),(404,"NotFound",1),(400,"BadRequest",1)] {
        let mut replies = metadata();
        replies.push(Reply { status, body: json!({"errorCode":code,"message":"source service detail"}), headers: Default::default() });
        let http = Arc::new(Http::new(replies));
        let plan = dispatch(&["job","run-status","Demo.Workspace/Book.Notebook","--id=run"], http.clone(), Arc::default(), pending());
        assert_eq!(plan.exit_code, exit);
        let result = data(&plan);
        assert_eq!(result["command"], "job run-status");
        assert_eq!(result["result"]["error_code"], code);
        assert!(result["result"].get("data").is_none());
        if status != 400 { assert!(!String::from_utf8_lossy(&streams(&plan).0).contains("source service detail")); }
        http.assert_consumed();
    }
}

#[test]
fn unsupported_skill_is_not_ignored_even_when_present_empty() {
    for skill in ["--skill=example", "--skill="] {
        let http = Arc::new(Http::new([]));
        let credential = Arc::new(Credential::default());
        let plan = dispatch(&["job","run-list","Demo.Workspace/Book.Notebook",skill], http.clone(), credential.clone(), pending());
        assert_eq!(data(&plan)["result"]["error_code"], "NotSupported");
        assert_eq!(plan.exit_code, 1);
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert!(http.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn cancellation_wins_before_any_credential_or_metadata_work() {
    let http = Arc::new(Http::new([]));
    let credential = Arc::new(Credential::default());
    let plan = dispatch(&["job","run-list","Demo.Workspace/Book.Notebook"], http.clone(), credential.clone(), ready(()));
    assert_eq!(plan.exit_code, 1);
    assert_eq!(data(&plan)["result"]["error_code"], "LongRunningOperationCancelled");
    assert!(http.requests.lock().unwrap().is_empty());
    assert!(credential.scopes.lock().unwrap().is_empty());
}

#[test]
fn cancellation_drops_discovery_status_and_later_page_http_without_replay() {
    for (hold, leaf) in [(1,"run-list"),(3,"run-status"),(4,"run-list")] {
        let mut replies = if hold == 1 { vec![] } else { metadata() };
        if hold == 4 { replies.push(reply(json!({"value":[{"id":"partial"}],"continuationToken":"next"}))); }
        let mut backend = Http::new(replies);
        backend.hold_after = Some(hold);
        let http = Arc::new(backend);
        let watched = http.clone();
        let cancelled = poll_fn(move |cx| {
            if watched.requests.lock().unwrap().len() >= hold { Poll::Ready(()) }
            else { cx.waker().wake_by_ref(); Poll::Pending }
        });
        let mut args = vec!["job",leaf,"Demo.Workspace/Book.Notebook"];
        if leaf == "run-status" { args.push("--id=run"); }
        let plan = dispatch(&args, http.clone(), Arc::default(), cancelled);
        assert_eq!(plan.exit_code, 1);
        assert_eq!(data(&plan)["result"]["error_code"], "LongRunningOperationCancelled");
        assert_eq!(http.requests.lock().unwrap().len(), hold);
        http.assert_consumed();
    }
}

struct Expired(AtomicUsize);

impl ExecutionTimer for Expired {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        let request = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            if request < 3 { pending::<()>().await; }
        })
    }
}

#[test]
fn shared_http_deadline_is_an_error_not_an_empty_list() {
    let mut backend = Http::new(metadata());
    backend.hold_after = Some(3);
    let http = Arc::new(backend);
    let executor = HttpExecutor::new(http.clone(), Arc::new(Expired(AtomicUsize::new(0))), ExecutionOptions::default()).unwrap();
    let plan = dispatch_with(&["job","run-list","Demo.Workspace/Book.Notebook"], &executor, Arc::default(), "/", HomeContext::Unspecified, pending());
    assert_eq!(plan.exit_code, 1);
    assert_ne!(data(&plan)["status"], "Success");
    http.assert_consumed();
}

#[test]
fn current_and_home_resolve_through_existing_metadata_scopes() {
    for (path, current, home) in [
        ("'Book.Notebook'", "/Demo.Workspace", HomeContext::Unspecified),
        ("~/Book.Notebook", "/", HomeContext::Workspace("Demo.Workspace".into())),
    ] {
        let mut replies = metadata();
        replies.push(reply(json!({"value":[]})));
        let http = Arc::new(Http::new(replies));
        let executor = executor(http.clone(), Arc::default());
        let plan = dispatch_with(&["job","run-list",path], &executor, Arc::default(), current, home, pending());
        assert_eq!(plan.exit_code, 0);
        assert_eq!(data(&plan)["result"]["message"], "No runs found");
        assert_eq!(http.requests.lock().unwrap().len(), 3);
        http.assert_consumed();
    }
}

#[test]
fn folder_context_uses_existing_graph_before_the_family_endpoint() {
    let http = Arc::new(Http::new([
        metadata().remove(0),
        reply(json!({"value":[{"id":"folder","displayName":"Parent"}]})),
        reply(json!({"value":[{"id":"n","displayName":"Book","type":"Notebook","folderId":"folder"}]})),
        reply(json!({"id":"run","status":"Completed"})),
    ]));
    let plan = dispatch(
        &["job","run-status","Demo.Workspace/Parent.Folder/Book.Notebook","--id=run"],
        http.clone(), Arc::default(), pending(),
    );
    assert_eq!(plan.exit_code, 0);
    assert_eq!(data(&plan)["result"]["data"], json!([{"id":"run","status":"Completed"}]));
    assert_eq!(*http.requests.lock().unwrap(), [
        "https://api.fabric.microsoft.com/v1/workspaces",
        "https://api.fabric.microsoft.com/v1/workspaces/w/folders?recursive=True",
        "https://api.fabric.microsoft.com/v1/workspaces/w/items",
        "https://api.fabric.microsoft.com/v1/workspaces/w/items/n/jobs/instances/run",
    ]);
    http.assert_consumed();
}

#[test]
fn unsupported_resource_and_missing_item_fail_before_job_requests() {
    for (path, expected) in [("Demo.Workspace/Other.Notebook","NotFound"),("Demo.Workspace","UnsupportedCommand")] {
        let replies = if path.ends_with("Other.Notebook") { metadata() } else { vec![metadata().remove(0)] };
        let http = Arc::new(Http::new(replies));
        let plan = dispatch(&["job","run-list",path], http.clone(), Arc::default(), pending());
        assert_eq!(plan.exit_code, 1);
        assert_eq!(data(&plan)["result"]["error_code"], expected);
        assert!(http.requests.lock().unwrap().iter().all(|path| !path.contains("/jobs/")));
        http.assert_consumed();
    }
}

struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::Error::other("synthetic writer failure")) }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

#[test]
fn output_writer_failure_is_propagated() {
    let mut replies = metadata();
    replies.push(reply(json!({"value":[]})));
    let plan = dispatch(&["job","run-list","Demo.Workspace/Book.Notebook"], Arc::new(Http::new(replies)), Arc::default(), pending());
    assert!(plan.write_to(&mut Broken, &mut Vec::new()).is_err());
}

#[test]
fn unsupported_python_values_fail_before_any_lossy_sdk_decode() {
    for raw in [
        r#"{"value":[{"id":18446744073709551616}]}"#,
        r#"{"value":[{"id":-9223372036854775809}]}"#,
        r#"{"value":[{"id":NaN}]}"#,
        r#"{"value":[{"id":"\ud800"}]}"#,
    ] {
        let mut replies = metadata();
        replies.push(reply(Value::String(raw.into())));
        let http = Arc::new(Http::new(replies));
        let plan = dispatch(&["job","run-list","Demo.Workspace/Book.Notebook"], http.clone(), Arc::default(), pending());
        assert_eq!(plan.exit_code, 1);
        assert_eq!(data(&plan)["result"]["error_code"], "UnexpectedError");
        assert!(data(&plan)["result"].get("data").is_none());
        http.assert_consumed();
    }
}

    #[derive(Debug)]
    struct PendingCredential {
        calls: AtomicUsize,
        stall: usize,
        dropped: AtomicBool,
    }

    struct CredentialDrop<'a>(&'a AtomicBool);
    impl Drop for CredentialDrop<'_> {
        fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
    }

    #[async_trait::async_trait]
    impl azure_core::credentials::TokenCredential for PendingCredential {
        async fn get_token(
            &self,
            scopes: &[&str],
            _: Option<azure_core::credentials::TokenRequestOptions<'_>>,
        ) -> azure_core::Result<azure_core::credentials::AccessToken> {
            assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
            if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.stall {
                let _guard = CredentialDrop(&self.dropped);
                return pending().await;
            }
            Ok(azure_core::credentials::AccessToken::new(
                "offline-command-test", azure_core::time::OffsetDateTime::UNIX_EPOCH,
            ))
        }
    }

    #[test]
    fn cancellation_drops_discovery_and_job_credential_futures() {
        for stall in [1, 3] {
            let credential = Arc::new(PendingCredential {
                calls: AtomicUsize::new(0), stall, dropped: AtomicBool::new(false),
            });
            let http = Arc::new(Http::new(if stall == 1 { vec![] } else { metadata() }));
            let executor = executor(http.clone(), Arc::default());
            let sdk = fabric_sdk::Client::new(
                credential.clone(),
                fabric_sdk::CredentialSelection::new("offline-account", "offline-tenant", fabric_sdk::Audience::Fabric).unwrap(),
            );
            let reads = ReadSession {
                sdk: &sdk, executor: &executor,
                tenant: Element::tenant("Tenant", "offline-tenant"),
                current_path: "/", home: HomeContext::Unspecified,
                timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
                config: ReadConfig::default(),
            };
            let Invocation::JobRead(command) = parse_args(["fab-rs", "job", "run-list", "Demo.Workspace/Book.Notebook"]).unwrap() else { panic!("job read required") };
            let cancelled = poll_fn(|cx| {
                if credential.calls.load(Ordering::SeqCst) >= stall { Poll::Ready(()) }
                else { cx.waker().wake_by_ref(); Poll::Pending }
            });
            let plan = complete(JobReadSession {
                reads: &reads, jobs: Arc::new(HttpJobExecutor::new(executor.clone())),
            }.dispatch(&command, cancelled)).unwrap();
            assert_eq!(data(&plan)["result"]["error_code"], "LongRunningOperationCancelled");
            assert!(credential.dropped.load(Ordering::SeqCst));
            assert_eq!(credential.calls.load(Ordering::SeqCst), stall);
            assert_eq!(http.requests.lock().unwrap().len(), stall - 1);
            http.assert_consumed();
        }
    }
