// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Metadata GET flags through direct hosts and the production read session.

#![deny(unnameable_test_items)]

#[allow(dead_code)]
mod folder_support;

use fabric_cli::{
    PythonQuery, ReadConfig, ReadHost, WriteError,
    parser::{Invocation, ReadCommand, parse_args},
    session::{ReadDispatch, ReadSession, get_options::is_metadata_property_query},
};
use fabric_cli_context::{Catalog, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use fabric_cli_output::{Timestamp, text::TextPlan};
use folder_support::{Credential, Http, Reply, Timer, complete, executor, reply, sdk};
use serde_json::{Value, json};
use std::{
    future::{Future, pending, poll_fn, ready},
    io::{self, Write},
    sync::Arc,
    task::Poll,
};

fn command(args: &[&str]) -> ReadCommand {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("read command required")
    };
    command
}

fn workspaces() -> Reply {
    reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}))
}

fn items() -> Reply {
    reply(json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]}))
}

fn metadata() -> Reply {
    reply(json!({
        "id":"n", "displayName":"Book", "description":"",
        "properties":{"level":{"deeper":{"still":{"value":true}}}}
    }))
}

fn session(
    command: &ReadCommand,
    replies: Vec<Reply>,
    cancelled: impl Future<Output = ()> + Send,
) -> (TextPlan, Vec<String>, usize) {
    let http = Arc::new(Http::new(replies));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone());
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let plan = complete(session.dispatch(command, cancelled)).unwrap();
    http.assert_consumed();
    let requests = http.requests.lock().unwrap().clone();
    let tokens = credential.scopes.lock().unwrap().len();
    (plan, requests, tokens)
}

fn session_evidence(
    command: &ReadCommand,
    replies: Vec<Reply>,
    cancelled: impl Future<Output = ()> + Send,
    source_operations: bool,
) -> (
    Result<ReadDispatch, fabric_cli_output::text::TextError>,
    Vec<String>,
    usize,
) {
    let http = Arc::new(Http::new(replies));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone());
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let result = if source_operations {
        complete(
            reads
                .with_source_operations(&executor)
                .dispatch_with_evidence(command, cancelled),
        )
    } else {
        complete(reads.dispatch_with_evidence(command, cancelled))
    };
    http.assert_consumed();
    let requests = http.requests.lock().unwrap().clone();
    let tokens = credential.scopes.lock().unwrap().len();
    (result, requests, tokens)
}

fn streams(plan: &TextPlan) -> (Vec<u8>, Vec<u8>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    plan.write_to(&mut out, &mut err).unwrap();
    (out, err)
}

fn assert_legacy_operation_envelope(dispatch: &ReadDispatch, command: &ReadCommand) {
    use fabric_cli_output::{
        CommandError, OutputEnvelope,
        text::{self, OutputFormat, Presentation, TextOptions},
    };

    let original = dispatch
        .operation_error
        .as_ref()
        .expect("original operation error");
    let mut input = fabric_cli_mutations::operation_error_output(original);
    input.command = Some(command.name.clone());
    assert_eq!(
        dispatch.error,
        Some(CommandError::new(
            input.message.clone(),
            input.error_code.clone()
        ))
    );
    let expected = text::render(
        &OutputEnvelope::new(Timestamp::parse("2000-01-01T00:00:00Z").unwrap(), input),
        OutputFormat::Json,
        &TextOptions {
            subcommand: Some(&command.name),
            ..TextOptions::default()
        },
        &text::ascii_width,
        Presentation::default(),
    )
    .unwrap();
    assert_eq!(dispatch.plan.exit_code, expected.exit_code);
    assert_eq!(streams(&dispatch.plan), streams(&expected));
}

fn failure_code(plan: &TextPlan) -> Value {
    let (out, err) = streams(plan);
    assert!(err.is_empty());
    let output: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(output["status"], "Failure");
    output["result"]["error_code"].clone()
}

#[test]
fn metadata_roots_match_the_exact_source_string_rule() {
    for root in [
        "id",
        "type",
        "displayName",
        "description",
        "workspaceId",
        "folderId",
        "properties",
    ] {
        assert!(is_metadata_property_query(root));
        assert!(is_metadata_property_query(&format!("{root}.nested[0]")));
    }
    for query in [
        "",
        ".",
        "Id",
        " id",
        "id ",
        "id[0]",
        "properties[0]",
        "[id]",
        "definition.parts",
        "connections",
        "schedules",
        "notproperties.x",
        "'properties.x'",
    ] {
        assert!(!is_metadata_property_query(query), "{query}");
    }
}

#[test]
fn metadata_flags_preserve_no_option_bytes_requests_and_empty_silence() {
    for query in ["id", "description", "properties", "properties.level.deeper"] {
        for format in ["json", "text"] {
            let baseline = command(&[
                "get",
                "Demo.Workspace/Book.Notebook",
                "-q",
                query,
                "--output_format",
                format,
            ]);
            let (expected, expected_requests, _) = session(
                &baseline,
                vec![workspaces(), items(), metadata()],
                pending(),
            );
            for (force, verbose) in [(true, false), (false, true), (true, true)] {
                let mut selected = baseline.clone();
                selected.force = force;
                selected.verbose = verbose;
                let (actual, requests, _) = session(
                    &selected,
                    vec![workspaces(), items(), metadata()],
                    pending(),
                );
                assert_eq!(actual.exit_code, expected.exit_code);
                assert_eq!(streams(&actual), streams(&expected));
                assert_eq!(requests, expected_requests);
                assert_eq!(
                    requests.last().unwrap(),
                    "https://api.fabric.microsoft.com/v1/workspaces/w/notebooks/n"
                );
            }
        }
    }
}

#[test]
fn workspace_flags_do_not_change_depth_projection_or_streams() {
    for query in [
        None,
        Some(""),
        Some("."),
        Some("sparkSettings.level.deep.more.value"),
    ] {
        let mut baseline = command(&["get", "Demo.Workspace"]);
        baseline.query = query.map(str::to_owned);
        let replies = || {
            vec![
                workspaces(),
                reply(json!({"id":"w","displayName":"Demo"})),
                reply(json!({"value":[]})),
                reply(json!({"level":{"deep":{"more":{"value":true}}}})),
                reply(json!({"value":[]})),
            ]
        };
        let (expected, expected_requests, _) = session(&baseline, replies(), pending());
        for (force, verbose) in [(true, false), (false, true), (true, true)] {
            let mut selected = baseline.clone();
            selected.force = force;
            selected.verbose = verbose;
            let (actual, requests, _) = session(&selected, replies(), pending());
            assert_eq!(actual.exit_code, expected.exit_code);
            assert_eq!(streams(&actual), streams(&expected));
            assert_eq!(requests, expected_requests);
        }
    }
}

#[test]
fn definition_queries_stay_explicitly_unsupported_after_target_resolution() {
    for query in [
        None,
        Some(""),
        Some("."),
        Some("definition.parts"),
        Some("properties[0]"),
    ] {
        for (force, verbose) in [(true, false), (false, true), (true, true)] {
            let mut selected = command(&["get", "Demo.Workspace/Book.Notebook"]);
            selected.query = query.map(str::to_owned);
            selected.force = force;
            selected.verbose = verbose;
            let (plan, requests, _) = session(&selected, vec![workspaces(), items()], pending());
            assert_eq!(plan.exit_code, 1);
            assert_eq!(failure_code(&plan), "NotSupported");
            assert_eq!(requests.len(), 2);
            assert!(
                !requests
                    .iter()
                    .any(|request| request.contains("getDefinition"))
            );
        }
    }
}

#[test]
fn output_combinations_never_fall_through_to_success_or_file_io() {
    for output in ["", "not-written.json"] {
        for (force, verbose) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut selected = command(&["get", "Demo.Workspace", "-q", "id"]);
            selected.output = Some(output.into());
            selected.force = force;
            selected.verbose = verbose;
            let (plan, requests, tokens) = session(&selected, vec![], pending());
            assert_eq!(failure_code(&plan), "NotSupported");
            assert!(requests.is_empty());
            assert_eq!(tokens, 0);
        }
    }
}

#[test]
fn invalid_format_cancellation_and_missing_target_precede_metadata_reads() {
    let mut selected = command(&[
        "get",
        "Demo.Workspace/Book.Notebook",
        "-q",
        "id",
        "-f",
        "-v",
    ]);
    selected.output_format = Some("invalid".into());
    let http = Arc::new(Http::new([]));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone());
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    assert!(complete(reads.dispatch(&selected, pending())).is_err());
    assert!(http.requests.lock().unwrap().is_empty());
    assert!(credential.scopes.lock().unwrap().is_empty());
    selected.output_format = None;
    let (plan, requests, tokens) = session(&selected, vec![], ready(()));
    assert_eq!(failure_code(&plan), "LongRunningOperationCancelled");
    assert!(requests.is_empty());
    assert_eq!(tokens, 0);
    selected.path = vec!["Missing.Workspace/Book.Notebook".into()];
    let (plan, requests, _) = session(&selected, vec![workspaces()], pending());
    assert_eq!(failure_code(&plan), "NotFound");
    assert_eq!(requests.len(), 1);
}

#[test]
fn service_failure_remains_failure_with_its_existing_exit_and_stream_contract() {
    for status in [401, 403, 404] {
        let code = match status {
            401 => "Unauthorized",
            403 => "Forbidden",
            _ => "NotFound",
        };
        let mut denied = reply(json!({"errorCode":code, "message":"private-service-detail"}));
        denied.status = status;
        let selected = command(&[
            "get",
            "Demo.Workspace/Book.Notebook",
            "-q",
            "id",
            "-f",
            "-v",
        ]);
        let (plan, requests, _) =
            session(&selected, vec![workspaces(), items(), denied], pending());
        assert_eq!(plan.exit_code, if status == 401 { 4 } else { 1 });
        assert_eq!(failure_code(&plan), code);
        assert!(!String::from_utf8_lossy(&streams(&plan).0).contains("private-service-detail"));
        assert_eq!(requests.len(), 3);
    }
}

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "synthetic writer failure",
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("the caller's writer must not be flushed")
    }
}

#[test]
fn failed_output_delivery_remains_an_error_not_a_successful_write() {
    let selected = command(&[
        "get",
        "Demo.Workspace/Book.Notebook",
        "-q",
        "id",
        "-f",
        "-v",
    ]);
    let (plan, _, _) = session(
        &selected,
        vec![workspaces(), items(), metadata()],
        pending(),
    );
    assert_eq!(
        plan.write_to(&mut BrokenWriter, &mut Vec::new())
            .unwrap_err()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(plan.exit_code, 0);
}

#[test]
fn direct_read_host_uses_the_same_option_gate_without_a_session_wrapper() {
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential);
    let http = Arc::new(Http::new([metadata(), metadata()]));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let tenant = Element::tenant("Tenant", "offline-tenant");
    let workspace = Element::child(
        "Demo",
        Some("w".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        tenant.clone(),
    )
    .unwrap();
    let item = Element::child(
        "Book",
        Some("n".into()),
        NodeKind::Item(ItemType::parse("Notebook").unwrap()),
        workspace.clone(),
    )
    .unwrap();
    let lookup = Catalog::new(vec![workspace.clone(), item]);
    let host = ReadHost {
        sdk: &sdk,
        workspaces: &executor,
        items: &executor,
        capacities: &executor,
        lookup: &lookup,
        current: workspace,
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
        projection: &PythonQuery,
    };
    let selected = command(&["get", "Book.Notebook", "-q", "id", "-f", "-v"]);
    let plan = complete(host.dispatch(&selected, pending())).unwrap();
    assert_eq!(plan.exit_code, 0);
    let mut rejected = selected.clone();
    rejected.query = Some("definition.parts".into());
    assert_eq!(
        complete(host.dispatch(&rejected, pending()))
            .unwrap()
            .exit_code,
        1
    );
    rejected = selected.clone();
    rejected.kind = fabric_cli::parser::ReadKind::List;
    assert_eq!(
        complete(host.dispatch(&rejected, pending()))
            .unwrap()
            .exit_code,
        1
    );
    let error =
        complete(host.run(&selected, pending(), &mut BrokenWriter, &mut Vec::new())).unwrap_err();
    assert!(matches!(error, WriteError::Io(error) if error.kind() == io::ErrorKind::BrokenPipe));
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    http.assert_consumed();
}

#[test]
fn read_dispatch_evidence_preserves_success_and_quiet_noop_plans() {
    for query in ["id", "description"] {
        let selected = command(&["get", "Demo.Workspace/Book.Notebook", "-q", query]);
        let replies = || vec![workspaces(), items(), metadata()];
        let (expected, expected_requests, expected_tokens) =
            session(&selected, replies(), pending());
        for source_operations in [false, true] {
            let (result, requests, tokens) =
                session_evidence(&selected, replies(), pending(), source_operations);
            let dispatch = result.unwrap();
            assert!(dispatch.error.is_none());
            assert!(dispatch.operation_error.is_none());
            assert_eq!(dispatch.plan.exit_code, expected.exit_code);
            assert_eq!(streams(&dispatch.plan), streams(&expected));
            assert_eq!(requests, expected_requests);
            assert_eq!(tokens, expected_tokens);
            if query == "description" {
                assert!(dispatch.plan.records.is_empty());
                assert_eq!(dispatch.plan.exit_code, 0);
            }
        }
    }
}

#[test]
fn read_dispatch_evidence_keeps_original_cancellation_without_requests() {
    let selected = command(&["get", "Demo.Workspace/Book.Notebook", "-q", "id"]);
    for source_operations in [false, true] {
        let (result, requests, tokens) =
            session_evidence(&selected, vec![], ready(()), source_operations);
        let dispatch = result.unwrap();
        assert!(dispatch.operation_error.is_none());
        assert_eq!(
            dispatch.error,
            Some(fabric_cli_output::CommandError::new(
                Some("Operation cancelled".into()),
                Some("LongRunningOperationCancelled".into()),
            ))
        );
        assert_eq!(
            failure_code(&dispatch.plan),
            "LongRunningOperationCancelled"
        );
        assert!(requests.is_empty());
        assert_eq!(tokens, 0);
    }
}

#[test]
fn read_dispatch_evidence_keeps_original_service_and_unexpected_errors_without_plan_inference() {
    for (status, code, message) in [
        (401, "Unauthorized", "Access is unauthorized"),
        (
            403,
            "Forbidden",
            "Access is forbidden. You do not have permission to access this resource",
        ),
        (404, "NotFound", "The requested resource could not be found"),
        (400, "UnexpectedError", "synthetic service failure"),
    ] {
        let selected = command(&["get", "Demo.Workspace/Book.Notebook", "-q", "id"]);
        let replies = || {
            let mut denied =
                reply(json!({"errorCode":code, "message":"synthetic service failure"}));
            denied.status = status;
            vec![workspaces(), items(), denied]
        };
        let (expected, expected_requests, expected_tokens) =
            session(&selected, replies(), pending());
        for source_operations in [false, true] {
            let (result, requests, tokens) =
                session_evidence(&selected, replies(), pending(), source_operations);
            let dispatch = result.unwrap();
            assert!(dispatch.operation_error.is_none());
            assert_eq!(
                dispatch.error,
                Some(fabric_cli_output::CommandError::new(
                    Some(message.into()),
                    Some(code.into()),
                ))
            );
            assert_eq!(dispatch.plan.exit_code, expected.exit_code);
            assert_eq!(streams(&dispatch.plan), streams(&expected));
            assert_eq!(requests, expected_requests);
            assert_eq!(tokens, expected_tokens);
        }
    }
}

#[test]
fn read_dispatch_evidence_returns_render_failures_without_command_error_substitution() {
    let mut selected = command(&["get", "Demo.Workspace/Book.Notebook", "-q", "id"]);
    selected.output_format = Some("invalid".into());
    for source_operations in [false, true] {
        let (result, requests, tokens) =
            session_evidence(&selected, vec![], pending(), source_operations);
        assert!(matches!(
            result.unwrap_err(),
            fabric_cli_output::text::TextError::UnsupportedFormat(format) if format == "invalid"
        ));
        assert!(requests.is_empty());
        assert_eq!(tokens, 0);
    }
}

#[test]
fn read_dispatch_evidence_retains_source_workspace_operation_errors() {
    let selected = command(&["get", "Demo.Workspace", "-q", "id"]);
    let mut denied = reply(json!({
        "errorCode":"UnexpectedError",
        "message":"synthetic operation failure"
    }));
    denied.status = 400;
    let (result, requests, _) =
        session_evidence(&selected, vec![workspaces(), denied], pending(), true);
    let dispatch = result.unwrap();
    let original = dispatch.operation_error.as_ref().unwrap();
    assert!(matches!(
        &original.kind,
        fabric_sdk_operations::OperationErrorKind::Http { status: 400, code: Some(code), .. }
            if code == "UnexpectedError"
    ));
    assert_eq!(
        original.raw_service_error.as_deref(),
        Some(&json!({"errorCode":"UnexpectedError","message":"synthetic operation failure"}))
    );
    assert!(original.operation_id.is_none());
    assert!(!original.mutation_may_have_occurred);
    assert_legacy_operation_envelope(&dispatch, &selected);
    assert_eq!(
        dispatch.error,
        Some(fabric_cli_output::CommandError::new(
            Some("synthetic operation failure".into()),
            Some("UnexpectedError".into()),
        ))
    );
    assert_eq!(failure_code(&dispatch.plan), "UnexpectedError");
    assert_eq!(requests.len(), 2);
}

#[test]
fn read_dispatch_preserves_empty_operation_message_in_legacy_and_evidence_output() {
    let selected = command(&["get", "Demo.Workspace", "-q", "id"]);
    let expected_stdout = concat!(
        "{\n",
        "    \"timestamp\": \"2000-01-01T00:00:00Z\",\n",
        "    \"status\": \"Failure\",\n",
        "    \"command\": \"get\",\n",
        "    \"result\": {\n",
        "        \"error_code\": \"UnexpectedError\",\n",
        "        \"message\": \"\"\n",
        "    }\n",
        "}\n",
    )
    .as_bytes();
    for with_evidence in [false, true] {
        let mut denied = reply(json!({"errorCode":"UnexpectedError","message":"."}));
        denied.status = 400;
        let http = Arc::new(Http::new([workspaces(), denied]));
        let executor = executor(http.clone(), Arc::new(Timer::default()));
        let sdk = sdk(Arc::new(Credential::default()));
        let reads = ReadSession {
            sdk: &sdk,
            executor: &executor,
            tenant: Element::tenant("Tenant", "offline-tenant"),
            current_path: "/",
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            config: ReadConfig::default(),
        };
        let source = reads.with_source_operations(&executor);
        let plan = if with_evidence {
            let dispatch = complete(source.dispatch_with_evidence(&selected, pending())).unwrap();
            assert_legacy_operation_envelope(&dispatch, &selected);
            assert_eq!(
                dispatch
                    .operation_error
                    .as_ref()
                    .unwrap()
                    .raw_service_error
                    .as_deref(),
                Some(&json!({"errorCode":"UnexpectedError","message":"."}))
            );
            dispatch.plan
        } else {
            complete(source.dispatch(&selected, pending())).unwrap()
        };
        let (stdout, stderr) = streams(&plan);
        assert_eq!(stdout, expected_stdout);
        assert!(stderr.is_empty());
        assert_eq!(plan.exit_code, 1);
        assert_eq!(http.requests.lock().unwrap().len(), 2);
        http.assert_consumed();
    }
}

#[test]
fn read_dispatch_evidence_retains_inflight_source_workspace_operation_cancellation() {
    let selected = command(&["get", "Demo.Workspace", "-q", "id"]);
    let mut http = Http::new([workspaces()]);
    http.hold_after = Some(2);
    let http = Arc::new(http);
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential);
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let signal_http = http.clone();
    let cancelled = poll_fn(move |cx| {
        if signal_http.requests.lock().unwrap().len() >= 2 {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    });
    let dispatch = complete(
        reads
            .with_source_operations(&executor)
            .dispatch_with_evidence(&selected, cancelled),
    )
    .unwrap();
    let original = dispatch.operation_error.as_ref().unwrap();
    assert!(matches!(
        original.kind,
        fabric_sdk_operations::OperationErrorKind::Cancelled
    ));
    assert!(original.operation_id.is_none());
    assert!(!original.mutation_may_have_occurred);
    assert!(original.raw_service_error.is_none());
    assert_legacy_operation_envelope(&dispatch, &selected);
    assert_eq!(
        failure_code(&dispatch.plan),
        "LongRunningOperationCancelled"
    );
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    http.assert_consumed();
}

#[test]
fn read_dispatch_evidence_retains_terminal_cancelled_operation_id_raw_error_and_output_failure() {
    const OPERATION: &str = "33333333-3333-3333-3333-333333333333";
    let selected = command(&["get", "Demo.Workspace", "-q", "id"]);
    let mut accepted = reply(json!({}));
    accepted.status = 202;
    accepted
        .headers
        .insert("x-ms-operation-id".into(), OPERATION.into());
    accepted.headers.insert("retry-after".into(), "0".into());
    let detail = json!({
        "errorCode":"SyntheticCancellation",
        "message":"synthetic terminal cancellation",
        "details":[{"field":"retained"}]
    });
    let terminal = reply(json!({"status":"Cancelled","error":detail}));
    let (result, requests, _) = session_evidence(
        &selected,
        vec![workspaces(), accepted, terminal],
        pending(),
        true,
    );
    let dispatch = result.unwrap();
    let original = dispatch.operation_error.as_ref().unwrap();
    assert!(matches!(
        &original.kind,
        fabric_sdk_operations::OperationErrorKind::OperationFailed { status, .. }
            if status == "Cancelled"
    ));
    assert_eq!(original.operation_id.as_deref(), Some(OPERATION));
    assert_eq!(original.raw_service_error.as_deref(), Some(&detail));
    assert!(!original.mutation_may_have_occurred);
    assert_legacy_operation_envelope(&dispatch, &selected);
    assert_eq!(
        failure_code(&dispatch.plan),
        "LongRunningOperationCancelled"
    );
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2],
        format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}")
    );
    let before = original.as_ref().clone();
    assert_eq!(
        dispatch
            .plan
            .write_to(&mut BrokenWriter, &mut Vec::new())
            .unwrap_err()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(dispatch.operation_error.as_deref(), Some(&before));
}

#[test]
fn read_dispatch_evidence_retains_accepted_id_when_cancelled_before_polling_without_replay() {
    const OPERATION: &str = "33333333-3333-3333-3333-333333333333";
    let selected = command(&["get", "Demo.Workspace", "-q", "id"]);
    let mut accepted = reply(json!({}));
    accepted.status = 202;
    accepted
        .headers
        .insert("x-ms-operation-id".into(), OPERATION.into());
    accepted.headers.insert("retry-after".into(), "0".into());
    let http = Arc::new(Http::new([workspaces(), accepted]));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let sdk = sdk(Arc::new(Credential::default()));
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let signal_http = http.clone();
    let cancelled = poll_fn(move |cx| {
        if signal_http.requests.lock().unwrap().len() >= 2 {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    });
    let dispatch = complete(
        reads
            .with_source_operations(&executor)
            .dispatch_with_evidence(&selected, cancelled),
    )
    .unwrap();
    let original = dispatch.operation_error.as_ref().unwrap();
    assert!(matches!(
        original.kind,
        fabric_sdk_operations::OperationErrorKind::Cancelled
    ));
    assert_eq!(original.operation_id.as_deref(), Some(OPERATION));
    assert!(!original.mutation_may_have_occurred);
    assert_legacy_operation_envelope(&dispatch, &selected);
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    http.assert_consumed();
}
