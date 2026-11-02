// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit source policy through the same production session and shared HTTP.

#[allow(dead_code)]
mod folder_support;

use fabric_cli::{
    ReadConfig,
    parser::{Invocation, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_output::Timestamp;
use folder_support::{Credential, Http, Reply, Timer, complete, executor, reply, sdk};
use serde_json::{Value, json};
use std::{
    future::{pending, poll_fn},
    io,
    sync::Arc,
    task::Poll,
    time::Duration,
};

const ROOT: &str = "https://api.fabric.microsoft.com";

fn discovery() -> Reply {
    reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}))
}

fn accepted() -> Reply {
    Reply {
        status: 202,
        body: json!({}),
        headers: [
            ("x-ms-operation-id".into(), "op".into()),
            ("Retry-After".into(), "0".into()),
        ]
        .into(),
    }
}

fn session<'a>(
    sdk: &'a fabric_sdk::Client,
    executor: &'a fabric_sdk_http::HttpExecutor,
) -> ReadSession<'a> {
    ReadSession {
        sdk,
        executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    }
}

fn command() -> fabric_cli::parser::ReadCommand {
    let Invocation::Read(command) =
        parse_args(["fab-rs", "get", "Demo.Workspace", "-q", "."]).unwrap()
    else {
        panic!("read required")
    };
    command
}

#[test]
fn source_primary_non_200_emits_nothing_without_enrichment_and_default_remains_strict() {
    for status in [201, 202, 204] {
        let primary = Reply {
            status,
            ..reply(json!({"id":"w","displayName":"Demo"}))
        };
        let http = Arc::new(Http::new([discovery(), primary.clone()]));
        let executor = executor(http.clone(), Arc::default());
        let credential = Arc::new(Credential::default());
        let sdk = sdk(credential.clone());
        let session = session(&sdk, &executor);
        let plan = complete(
            session
                .with_source_operations(&executor)
                .dispatch(&command(), pending()),
        )
        .unwrap();
        assert_eq!(plan.exit_code, 0);
        assert!(plan.records.is_empty());
        assert_eq!(credential.scopes.lock().unwrap().len(), 2);
        assert_eq!(http.requests.lock().unwrap().len(), 2);
        http.assert_consumed();
    }
    let http = Arc::new(Http::new([
        discovery(),
        Reply {
            status: 202,
            ..reply(json!({}))
        },
    ]));
    let executor = executor(http.clone(), Arc::default());
    let sdk = sdk(Arc::default());
    let plan = complete(session(&sdk, &executor).dispatch(&command(), pending())).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    plan.write_to(&mut out, &mut err).unwrap();
    assert_eq!(plan.exit_code, 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["result"]["error_code"],
        "ReadFailed"
    );
    assert!(err.is_empty());
    http.assert_consumed();
}

#[test]
fn source_poll_and_result_complete_before_the_existing_three_enrichments() {
    let mut initial = accepted();
    initial.headers.insert("Retry-After".into(), "1".into());
    let mut polled = reply(json!({"status":"Succeeded"}));
    polled
        .headers
        .insert("Location".into(), format!("{ROOT}/v1/operations/op/result"));
    let http = Arc::new(Http::new([
        discovery(),
        initial,
        polled,
        reply(json!({"id":"w","displayName":"Demo"})),
        reply(json!({"value":[]})),
        reply(json!({"enabled":true})),
        reply(json!({"value":[]})),
    ]));
    let timer = Arc::new(Timer::default());
    let executor = executor(http.clone(), timer.clone());
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone());
    let plan = complete(
        session(&sdk, &executor)
            .with_source_operations(&executor)
            .dispatch(&command(), pending()),
    )
    .unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    plan.write_to(&mut out, &mut err).unwrap();
    assert_eq!(plan.exit_code, 0);
    assert!(err.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["result"]["data"],
        json!([{
            "id":"w", "displayName":"Demo", "managedPrivateEndpoints":[],
            "sparkSettings":{"enabled":true}, "roleAssignments":[]
        }])
    );
    assert_eq!(
        *http.requests.lock().unwrap(),
        [
            format!("{ROOT}/v1/workspaces"),
            format!("{ROOT}/v1/workspaces/w"),
            format!("{ROOT}/v1/operations/op"),
            format!("{ROOT}/v1/operations/op/result"),
            format!("{ROOT}/v1/workspaces/w/managedPrivateEndpoints"),
            format!("{ROOT}/v1/workspaces/w/spark/settings"),
            format!("{ROOT}/v1/workspaces/w/roleAssignments"),
        ]
    );
    assert_eq!(credential.scopes.lock().unwrap().len(), 7);
    assert_eq!(
        timer
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|&&delay| delay == Duration::from_secs(1))
            .count(),
        1
    );
    http.assert_consumed();
}

#[test]
fn source_operation_failures_use_existing_formatter_without_second_message_normalization() {
    for (status, code, message, expected, exit) in [
        (400, "Bad", ".", "", 1),
        (
            401,
            "Unauthorized",
            "sensitive",
            "Access is unauthorized",
            4,
        ),
        (
            403,
            "Forbidden",
            "sensitive",
            "Access is forbidden. You do not have permission to access this resource",
            1,
        ),
    ] {
        let http = Arc::new(Http::new([
            discovery(),
            Reply {
                status,
                ..reply(json!({"errorCode":code,"message":message}))
            },
        ]));
        let executor = executor(http.clone(), Arc::default());
        let sdk = sdk(Arc::default());
        let plan = complete(
            session(&sdk, &executor)
                .with_source_operations(&executor)
                .dispatch(&command(), pending()),
        )
        .unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        plan.write_to(&mut out, &mut err).unwrap();
        let envelope: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(plan.exit_code, exit);
        assert_eq!(envelope["status"], "Failure");
        assert_eq!(envelope["result"]["error_code"], code);
        assert_eq!(envelope["result"]["message"], expected);
        assert!(err.is_empty());
        http.assert_consumed();
    }
}

#[test]
fn source_operation_cancellation_drops_the_poll_and_never_starts_enrichment() {
    let mut http = Http::new([discovery(), accepted()]);
    http.hold_after = Some(3);
    let http = Arc::new(http);
    let executor = executor(http.clone(), Arc::default());
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone());
    let cancelled = poll_fn(|cx| {
        if http.requests.lock().unwrap().len() >= 3 {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    });
    let session = session(&sdk, &executor);
    let plan = complete(
        session
            .with_source_operations(&executor)
            .dispatch(&command(), cancelled),
    )
    .unwrap();
    assert_eq!(plan.exit_code, 1);
    assert_eq!(http.requests.lock().unwrap().len(), 3);
    assert_eq!(credential.scopes.lock().unwrap().len(), 3);
    let mut out = Vec::new();
    plan.write_to(&mut out, &mut Vec::new()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["result"]["error_code"],
        "LongRunningOperationCancelled"
    );
    http.assert_consumed();
}

#[test]
fn source_operation_rejects_foreign_links_and_preserves_output_writer_errors() {
    let mut initial = accepted();
    initial.headers.insert(
        "Location".into(),
        "https://untrusted.example/v1/operations/op".into(),
    );
    let http = Arc::new(Http::new([discovery(), initial]));
    let executor = executor(http.clone(), Arc::default());
    let sdk = sdk(Arc::default());
    let plan = complete(
        session(&sdk, &executor)
            .with_source_operations(&executor)
            .dispatch(&command(), pending()),
    )
    .unwrap();
    assert_eq!(plan.exit_code, 1);
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    struct Broken;
    impl io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(
        plan.write_to(&mut Broken, &mut Vec::new())
            .unwrap_err()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    http.assert_consumed();
}
