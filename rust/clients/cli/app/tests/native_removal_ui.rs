// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native removal UI through root argv, the released family and offline HTTP.
//! Source: cf67fd52ea8b47e24c725e73d792de6f4a08e2e4 workspace/tenant removal.

#![cfg(feature = "native-read")]
#![deny(unnameable_test_items)]

mod family_support;
#[path = "../src/native_terminal_ui.rs"]
mod terminal_ui;

use async_trait::async_trait;
use azure_core::http::{AsyncRawResponse, HttpClient, Request};
use fabric_cli::{
    console::{read_confirmation, read_selection_with_prompt},
    families::{FamilyEvidence, FamilyOutput, FamilySession},
    parser::parse_args,
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext, PathElement};
use fabric_cli_local::{ConfigStore, NavigationState};
use fabric_cli_output::Timestamp;
use fabric_sdk::{Audience, Client, CredentialSelection};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use family_support::{Harness, success};
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    io::{self, Cursor},
    sync::{Arc, Mutex},
    time::Duration,
};
use terminal_ui::NativeMutationUi;
use tokio::sync::{Notify, oneshot};

const OPERATION: &str = "33333333-3333-3333-3333-333333333333";

struct Terminal {
    input: Arc<Mutex<Cursor<Vec<u8>>>>,
    transcript: Arc<Mutex<Vec<u8>>>,
}

impl Terminal {
    fn new(answer: &str) -> Self {
        Self {
            input: Arc::new(Mutex::new(Cursor::new(answer.as_bytes().to_vec()))),
            transcript: Arc::default(),
        }
    }

    fn ui(&self) -> NativeMutationUi {
        NativeMutationUi::with_selection_io(
            {
                let input = Arc::clone(&self.input);
                let transcript = Arc::clone(&self.transcript);
                move |question| {
                    read_confirmation(
                        &mut *input.lock().unwrap(),
                        &mut *transcript.lock().unwrap(),
                        question,
                    )
                }
            },
            {
                let input = Arc::clone(&self.input);
                let transcript = Arc::clone(&self.transcript);
                move |prompt, names| {
                    read_selection_with_prompt(
                        &mut *input.lock().unwrap(),
                        &mut *transcript.lock().unwrap(),
                        prompt,
                        names,
                    )
                }
            },
            {
                let transcript = Arc::clone(&self.transcript);
                move |records| {
                    for record in records {
                        record.write_to(&mut Vec::new(), &mut *transcript.lock().unwrap())?;
                    }
                    Ok(())
                }
            },
        )
    }

    fn text(&self) -> String {
        String::from_utf8(self.transcript.lock().unwrap().clone()).unwrap()
    }
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == Duration::from_secs(240) {
            Box::pin(pending())
        } else {
            Box::pin(async {})
        }
    }
}

async fn execute(host: &mut Harness, args: &[&str], ui: &mut NativeMutationUi) -> FamilyOutput {
    let http = host.http.clone();
    execute_with_cancelled(host, args, ui, http, pending()).await
}

async fn execute_with_cancelled(
    host: &mut Harness,
    args: &[&str],
    ui: &mut NativeMutationUi,
    http: Arc<dyn HttpClient>,
    cancelled: impl Future<Output = ()> + Send,
) -> FamilyOutput {
    let invocation = parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap();
    let sdk = Client::new(
        host.credential.clone(),
        CredentialSelection::new("offline-account", "tenant", Audience::Fabric).unwrap(),
    );
    let executor = HttpExecutor::new(http, Arc::new(Timer), ExecutionOptions::default()).unwrap();
    let tenant = Element::tenant("Tenant", "tenant");
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: tenant.clone(),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2026-10-29T06:55:00Z").unwrap(),
        config: Default::default(),
    };
    let store = ConfigStore::new(&host.root);
    let mut navigation = NavigationState::new(
        PathElement::from_fabric(tenant),
        HomeContext::Unspecified,
        host.root.to_string_lossy().into_owned(),
        37,
        true,
    )
    .unwrap();
    let mut mutations = NativeMutationUi::with_io(
        |_| panic!("workspace/tenant input must use the explicit removal UI"),
        |_| Ok(()),
    );
    let transfer_ui = family_support::TransferUiCapture::default();
    let mut output = FamilySession {
        reads: &reads,
        http: &executor,
        store: &store,
        navigation: &mut navigation,
        local: &mut host.effects,
        mutations: &mut mutations,
        confirmation: None,
        transfer_ui: &transfer_ui,
        transfer_files: None,
        resource_files: None,
    }
    .dispatch_with_removal_ui(&invocation, ui, cancelled)
    .await
    .unwrap();
    let mut notices = std::mem::take(&mut ui.records);
    notices.append(&mut output.plan.records);
    output.plan.records = notices;
    assert!(host.http.replies.lock().unwrap().is_empty());
    output
}

fn workspace(host: &Harness) {
    host.reply(
        200,
        json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
    );
}

fn items(host: &Harness) {
    host.reply(
        200,
        json!({"value":[
            {"id":"z","displayName":"Zeta","type":"Notebook"},
            {"id":"a","displayName":"Alpha","type":"Report"},
            {"id":"d","displayName":"Blocked","type":"Dashboard"}
        ]}),
    );
}

fn failure(output: &FamilyOutput, code: &str) -> String {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(output.write_to(&mut stdout, &mut stderr).unwrap(), 1);
    let envelopes = serde_json::Deserializer::from_slice(&stdout)
        .into_iter::<Value>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(envelopes.last().unwrap()["result"]["error_code"], code);
    String::from_utf8(stdout).unwrap()
}

#[tokio::test]
async fn workspace_selects_supported_names_flushes_notices_and_deletes_only_confirmed_subset() {
    let mut host = Harness::new();
    workspace(&host);
    items(&host);
    host.reply(202, Value::Null);
    let terminal = Terminal::new("2\nyes\n");
    let mut ui = terminal.ui();
    let output = execute(&mut host, &["rm", "Demo.Workspace", "--hard"], &mut ui).await;
    assert!(success(&output).contains("1 items deleted successfully"));
    let transcript = terminal.text();
    assert!(transcript.starts_with("Select items:\n  1. Alpha.Report\n  2. Zeta.Notebook\n"));
    assert!(!transcript.contains("Blocked.Dashboard"));
    assert!(
        transcript.contains("Zeta.Notebook\n------------------------------\nAre you sure? [Y/n] ")
    );
    assert!(ui.records.is_empty());
    let requests = host.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2]["method"], "DELETE");
    assert!(
        requests[2]["url"]
            .as_str()
            .unwrap()
            .ends_with("/items/z?hardDelete=true")
    );
    assert!(
        matches!(&output.evidence, FamilyEvidence::Removal(Ok(result))
        if result.source_deleted_count == 1 && result.effects.len() == 1
            && result.responses.len() == 1 && !result.declined)
    );
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
            .is_err()
    );
    assert_eq!(
        host.requests().len(),
        3,
        "delivery failure must not replay deletion"
    );
}

#[tokio::test]
async fn tenant_force_still_selects_and_confirms_before_source_sorted_workspace_deletes() {
    for force in [false, true] {
        let mut host = Harness::new();
        host.reply(
            200,
            json!({"value":[
                {"id":"a","displayName":"a","type":"Workspace"},
                {"id":"b","displayName":"B","type":"Workspace"},
                {"id":"A","displayName":"A","type":"Workspace"}
            ]}),
        );
        host.reply(202, Value::Null);
        host.reply(200, Value::Null);
        let terminal = Terminal::new("3,2\n\n");
        let mut ui = terminal.ui();
        let args = if force {
            vec!["rm", "/", "-f"]
        } else {
            vec!["rm", "/"]
        };
        let output = execute(&mut host, &args, &mut ui).await;
        assert!(success(&output).contains("2 workspaces deleted successfully"));
        let transcript = terminal.text();
        assert!(transcript.starts_with(
            "Select workspaces:\n  1. A.Workspace\n  2. B.Workspace\n  3. a.Workspace\n"
        ));
        assert_eq!(transcript.matches("Are you sure? [Y/n] ").count(), 1);
        let requests = host.requests();
        assert_eq!(requests.len(), 3);
        assert!(
            requests[1]["url"]
                .as_str()
                .unwrap()
                .ends_with("/workspaces/b")
        );
        assert!(
            requests[2]["url"]
                .as_str()
                .unwrap()
                .ends_with("/workspaces/a")
        );
        assert!(
            matches!(&output.evidence, FamilyEvidence::Removal(Ok(result))
            if result.source_deleted_count == 2 && result.effects.len() == 2)
        );
    }
}

#[tokio::test]
async fn forced_workspace_still_reads_counts_and_deletes_without_consuming_terminal_input() {
    let mut host = Harness::new();
    workspace(&host);
    items(&host);
    host.reply(202, Value::Null);
    let terminal = Terminal::new("");
    let mut ui = terminal.ui();
    let output = execute(&mut host, &["rm", "Demo.Workspace", "-f"], &mut ui).await;
    assert!(success(&output).contains("'Demo.Workspace' is being deleted"));
    assert!(terminal.text().is_empty());
    assert_eq!(terminal.input.lock().unwrap().position(), 0);
    let requests = host.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1]["url"]
            .as_str()
            .unwrap()
            .ends_with("/workspaces/w/items")
    );
    assert_eq!(requests[2]["method"], "DELETE");
    assert!(
        requests[2]["url"]
            .as_str()
            .unwrap()
            .ends_with("/workspaces/w")
    );
    let mut stderr = Vec::new();
    output.write_to(&mut Vec::new(), &mut stderr).unwrap();
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .contains("3 underlying items")
    );
}

#[tokio::test]
async fn empty_selection_and_explicit_decline_never_dispatch_delete_even_for_tenant_force() {
    for tenant in [false, true] {
        for answer in ["\n", "1\nno\n"] {
            let mut host = Harness::new();
            workspace(&host);
            if !tenant {
                items(&host);
            }
            let terminal = Terminal::new(answer);
            let mut ui = terminal.ui();
            let args = if tenant {
                vec!["rm", "/", "-f"]
            } else {
                vec!["rm", "Demo.Workspace"]
            };
            let output = execute(&mut host, &args, &mut ui).await;
            success(&output);
            assert!(
                matches!(&output.evidence, FamilyEvidence::Removal(Ok(result))
                if result.declined && result.responses.is_empty() && result.effects.is_empty())
            );
            assert_eq!(
                terminal.text().contains("Are you sure?"),
                answer.starts_with('1')
            );
            assert!(
                host.requests()
                    .iter()
                    .all(|request| request["method"] == "GET")
            );
        }
    }
}

#[tokio::test]
async fn absent_invalid_and_failed_input_are_typed_errors_not_empty_selection_or_consent() {
    for tenant in [false, true] {
        for (answer, code) in [
            ("", "SelectionInputFailed"),
            ("1", "SelectionInputFailed"),
            ("bad\n", "SelectionInputFailed"),
            ("1\n", "ConfirmationInputFailed"),
            ("1\nmaybe\n", "ConfirmationInputFailed"),
        ] {
            let mut host = Harness::new();
            workspace(&host);
            if !tenant {
                items(&host);
            }
            let terminal = Terminal::new(answer);
            let mut ui = terminal.ui();
            let args = if tenant {
                vec!["rm", "/", "-f"]
            } else {
                vec!["rm", "Demo.Workspace"]
            };
            let output = execute(&mut host, &args, &mut ui).await;
            failure(&output, code);
            assert!(
                matches!(&output.evidence, FamilyEvidence::Removal(Err(error))
                if error.partial.responses.is_empty() && error.partial.effects.is_empty()
                    && error.error.operation.is_none())
            );
            assert!(
                host.requests()
                    .iter()
                    .all(|request| request["method"] == "GET")
            );
        }
    }
}

#[derive(Debug)]
struct PendingOperation {
    inner: Arc<family_support::Http>,
    started: Notify,
}

#[async_trait]
impl HttpClient for PendingOperation {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        if request.url().path() == format!("/v1/operations/{OPERATION}") {
            assert_eq!(request.method().to_string(), "GET");
            self.inner.requests.lock().unwrap().push(json!({
                "method":"GET", "url":request.url().as_str(),
            }));
            self.started.notify_one();
            pending().await
        } else {
            self.inner.execute_request(request).await
        }
    }
}

#[tokio::test]
async fn cancellation_after_prior_delete_retains_partial_effects_operation_id_and_uncertainty() {
    let mut host = Harness::new();
    workspace(&host);
    host.reply(
        200,
        json!({"value":[
            {"id":"a","displayName":"A","type":"Notebook"},
            {"id":"b","displayName":"B","type":"Notebook"}
        ]}),
    );
    host.reply(202, Value::Null);
    let location = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    host.headers(
        202,
        Value::Null,
        &[
            ("Location", &location),
            ("x-ms-operation-id", OPERATION),
            ("Retry-After", "0"),
        ],
    );
    let http = Arc::new(PendingOperation {
        inner: host.http.clone(),
        started: Notify::new(),
    });
    let terminal = Terminal::new("all\nyes\n");
    let mut ui = terminal.ui();
    let (cancel, cancelled) = oneshot::channel();
    let output = {
        let dispatch = execute_with_cancelled(
            &mut host,
            &["rm", "Demo.Workspace"],
            &mut ui,
            http.clone(),
            async { cancelled.await.unwrap() },
        );
        tokio::pin!(dispatch);
        tokio::select! {
            () = http.started.notified() => {}
            _ = &mut dispatch => panic!("operation must stay pending until cancelled"),
            () = tokio::time::sleep(Duration::from_secs(2)) => panic!("operation poll did not start"),
        }
        cancel.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), &mut dispatch)
            .await
            .unwrap()
    };
    let stdout = failure(&output, "LongRunningOperationCancelled");
    assert!(stdout.contains("'A.Notebook' deleted"));
    assert!(!stdout.contains("items deleted successfully"));
    assert!(
        matches!(&output.evidence, FamilyEvidence::Removal(Err(error))
        if error.partial.responses.len() == 1 && error.partial.effects.len() == 1
            && error.partial.source_deleted_count == 1
            && error.error.operation.as_ref().is_some_and(|operation|
                operation.operation_id.as_deref() == Some(OPERATION)
                    && operation.mutation_may_have_occurred))
    );
    let requests = host.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "DELETE")
            .count(),
        2
    );
    assert_eq!(requests.len(), 5);
}

#[tokio::test]
async fn native_selection_delivery_failure_precedes_root_confirmation_or_deletion() {
    let mut host = Harness::new();
    workspace(&host);
    items(&host);
    let mut ui = NativeMutationUi::with_selection_io(
        |_| panic!("failed selection must not confirm"),
        |_, _| {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "selection prompt closed",
            ))
        },
        |_| Ok(()),
    );
    let output = execute(&mut host, &["rm", "Demo.Workspace"], &mut ui).await;
    failure(&output, "SelectionInputFailed");
    assert_eq!(host.requests().len(), 2);
}

#[tokio::test]
async fn dropping_root_selection_wait_cannot_delete_after_a_late_all_answer() {
    let mut host = Harness::new();
    workspace(&host);
    items(&host);
    let (release, wait) = std::sync::mpsc::channel();
    let wait = Mutex::new(wait);
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let (finished, completion) = oneshot::channel();
    let finished = Mutex::new(Some(finished));
    let mut ui = NativeMutationUi::with_selection_io(
        |_| panic!("late selection must not reach confirmation"),
        move |_, names| {
            started.send(()).unwrap();
            wait.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .map_err(io::Error::other)?;
            finished.lock().unwrap().take().unwrap().send(()).unwrap();
            Ok(names.to_vec())
        },
        |_| Ok(()),
    );
    {
        let dispatch = execute(&mut host, &["rm", "Demo.Workspace"], &mut ui);
        tokio::pin!(dispatch);
        tokio::select! {
            _ = &mut dispatch => panic!("selection completed before input"),
            result = observed.recv() => assert_eq!(result, Some(())),
            () = tokio::time::sleep(Duration::from_secs(2)) => panic!("selection did not start"),
        }
    }
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), completion)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(host.requests().len(), 2);
    assert!(
        host.requests()
            .iter()
            .all(|request| request["method"] == "GET")
    );
}

#[tokio::test]
async fn genuinely_absent_removal_ui_retains_the_explicit_library_capability_error() {
    for tenant in [false, true] {
        let mut host = Harness::new();
        let args = if tenant {
            vec!["rm", "/", "-f"]
        } else {
            workspace(&host);
            vec!["rm", "Demo.Workspace"]
        };
        let output = host.execute(&args).await;
        failure(&output, "NotSupported");
        assert!(
            host.requests()
                .iter()
                .all(|request| request["method"] == "GET")
        );
    }
}
