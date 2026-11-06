// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real run/query/execute argv through prepared caller-owned backends and exports.

#![deny(unnameable_test_items)]

use fabric_cli::{
    native_tabular::{
        self, Approval, Backend, BackendFailure, BackendFuture, BackendResult, Capabilities,
        Evidence, ExecutionState, NativeCommand, Operation, PreparedOperation, TextSource,
    },
    parser::{Invocation, parse_args},
    tabular_export::{
        ExportDelivery, ExportRequest, ExportSink, FileSink, ReceiptWriteFailure, TableResult,
    },
};
use fabric_cli_output::tabular::{
    TabularColumn, TabularFormat, TabularTable, TabularType, TabularValue, encode,
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const ROOTS: &[(&str, &[&str])] = &[
    (
        "run",
        &["run", "Demo.Workspace/Functions.UserDataFunction/fn"],
    ),
    (
        "query",
        &[
            "query",
            "Demo.Workspace/Lake.Lakehouse/Tables/orders",
            "--text",
            "select 1",
        ],
    ),
    (
        "execute",
        &[
            "execute",
            "Demo.Workspace/Db.SQLDatabase",
            "--text",
            "update opaque",
        ],
    ),
];

fn rows() -> Vec<Value> {
    vec![
        json!({"id":1,"value":null}),
        json!({"id":2,"value":"quoted,\"$HOME\\n","later":true}),
    ]
}

fn complete() -> BackendResult {
    let mut result = TableResult::single("provider-label", rows());
    result.metadata = json!({"handle":"real-provider-handle","records_affected":7});
    BackendResult::complete(result)
}

fn capabilities() -> Capabilities {
    Capabilities {
        binding: Evidence::Proven,
        input: Evidence::Proven,
        permission: Evidence::Proven,
        read_only: Evidence::Proven,
        approval: Approval::Granted,
        tabular: Evidence::Proven,
        complete: Evidence::Proven,
        schema: Evidence::Proven,
        resultsets: Some(1),
        engines: vec!["spark-native".into()],
        metadata: json!({"binding":"prepared-native-binding"}),
    }
}

struct CountingBackend {
    capabilities: Capabilities,
    preparation_failure: Option<BackendFailure>,
    require_run_input: bool,
    prepares: AtomicUsize,
    executes: AtomicUsize,
    prepared_commands: Mutex<Vec<NativeCommand>>,
    executed_commands: Mutex<Vec<NativeCommand>>,
    outcome: Mutex<Option<Result<BackendResult, BackendFailure>>>,
}

impl CountingBackend {
    fn new(outcome: BackendResult) -> Self {
        Self {
            capabilities: capabilities(),
            preparation_failure: None,
            require_run_input: false,
            prepares: AtomicUsize::new(0),
            executes: AtomicUsize::new(0),
            prepared_commands: Mutex::default(),
            executed_commands: Mutex::default(),
            outcome: Mutex::new(Some(Ok(outcome))),
        }
    }
}

struct Prepared<'a> {
    backend: &'a CountingBackend,
    command: NativeCommand,
    capabilities: Capabilities,
}

impl Backend for CountingBackend {
    fn prepare<'a>(
        &'a self,
        command: &'a NativeCommand,
    ) -> BackendFuture<'a, Result<Box<dyn PreparedOperation<'a> + 'a>, BackendFailure>> {
        Box::pin(async move {
            self.prepares.fetch_add(1, Ordering::SeqCst);
            self.prepared_commands.lock().unwrap().push(command.clone());
            if let Some(error) = &self.preparation_failure {
                return Err(error.clone());
            }
            let mut capabilities = self.capabilities.clone();
            if self.require_run_input
                && matches!(&command.operation, Operation::Run(run) if run.input.is_none())
            {
                capabilities.input = Evidence::Denied;
            }
            Ok(Box::new(Prepared {
                backend: self,
                command: command.clone(),
                capabilities,
            }) as Box<dyn PreparedOperation<'a> + 'a>)
        })
    }
}

impl<'a> PreparedOperation<'a> for Prepared<'a> {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn execute(self: Box<Self>) -> BackendFuture<'a, Result<BackendResult, BackendFailure>> {
        Box::pin(async move {
            self.backend.executes.fetch_add(1, Ordering::SeqCst);
            self.backend
                .executed_commands
                .lock()
                .unwrap()
                .push(self.command);
            self.backend
                .outcome
                .lock()
                .unwrap()
                .take()
                .expect("native operation must not replay")
        })
    }
}

#[derive(Default)]
struct Sink {
    preflights: usize,
    publishes: usize,
    reject_preflight: bool,
    reject_publish: bool,
    bytes: Vec<u8>,
}

impl ExportSink for Sink {
    fn preflight(&mut self, _: &ExportRequest) -> io::Result<()> {
        self.preflights += 1;
        if self.reject_preflight {
            Err(io::Error::other("destination exists"))
        } else {
            Ok(())
        }
    }

    fn publish(&mut self, _: &ExportRequest, bytes: &[u8]) -> io::Result<()> {
        self.publishes += 1;
        if self.reject_publish {
            Err(io::Error::other("publication failed"))
        } else {
            self.bytes = bytes.to_vec();
            Ok(())
        }
    }
}

fn parsed(leaf: &[&str], format: Option<&str>, file: &Path) -> NativeCommand {
    let mut arguments = vec!["fab-rs".to_owned()];
    arguments.extend(leaf.iter().map(|value| (*value).to_owned()));
    if let Some(format) = format {
        arguments.extend([
            "--format".into(),
            format.into(),
            "--output".into(),
            file.to_str().unwrap().into(),
        ]);
    }
    let Invocation::NativeTabular(command) = parse_args(arguments).unwrap() else {
        panic!("real native root required");
    };
    command
}

fn dispatch(
    command: &NativeCommand,
    backend: &dyn Backend,
    sink: &mut impl ExportSink,
) -> ExportDelivery {
    native_tabular::dispatch(command, Some(backend), sink)
        .now_or_never()
        .unwrap()
}

#[test]
fn each_real_root_executes_its_prepared_request_once_through_all_common_codecs() {
    for (name, leaf) in ROOTS {
        for (format, codec) in [
            ("csv", TabularFormat::Csv),
            ("json", TabularFormat::Json),
            ("parquet", TabularFormat::Parquet),
        ] {
            let command = parsed(leaf, Some(format), Path::new("native.data"));
            let backend = CountingBackend::new(complete());
            let mut sink = Sink::default();
            let delivery = dispatch(&command, &backend, &mut sink);
            assert_eq!(
                delivery.exit_code, 0,
                "{name} {format}: {:?}",
                delivery.receipt
            );
            assert_eq!(backend.prepares.load(Ordering::SeqCst), 1);
            assert_eq!(backend.executes.load(Ordering::SeqCst), 1);
            assert_eq!(backend.prepared_commands.lock().unwrap()[0].name(), *name);
            assert_eq!(backend.executed_commands.lock().unwrap()[0].name(), *name);
            assert_eq!(
                backend.executed_commands.lock().unwrap()[0].path,
                command.path
            );
            assert_eq!(sink.preflights, 1);
            assert_eq!(sink.publishes, 1);
            assert_eq!(
                sink.bytes,
                encode(&TabularTable::from_rows(&rows()).unwrap(), codec).unwrap()
            );
            assert_eq!(delivery.receipt["source"], *name);
            assert_eq!(
                delivery.receipt["execution"]["metadata"]["handle"],
                "real-provider-handle"
            );
            assert_eq!(delivery.receipt["delivery"]["rows"], 2);
            assert!(
                delivery.receipt["delivery"]["schema"]
                    .to_string()
                    .contains("later")
            );
            if format == "json" {
                let data: Value = serde_json::from_slice(&sink.bytes).unwrap();
                assert!(data[0].get("handle").is_none());
            }
        }
    }
}

#[test]
fn root_dtos_preserve_opaque_text_paths_engine_and_execution_options() {
    let path = "  Demo.Workspace/quoted '$HOME' \u{4e2d}\\path  ";
    let text = "  DELETE opaque '\\u0041' \"quoted\" $HOME \u{03bb}\n-- not classified  ";
    let query = parsed(
        &[
            "query",
            path,
            "--text",
            text,
            "--engine",
            "spark-native",
            "--parameters",
            " params '$X'\\u.json ",
        ],
        Some("json"),
        Path::new("native.data"),
    );
    let backend = CountingBackend::new(complete());
    assert_eq!(
        dispatch(&query, &backend, &mut Sink::default()).exit_code,
        0
    );
    let commands = backend.executed_commands.lock().unwrap();
    assert_eq!(commands[0].path, path);
    let Operation::Query(query) = &commands[0].operation else {
        panic!("query must not reroute to execute")
    };
    assert_eq!(query.text, TextSource::Inline(text.into()));
    assert_eq!(query.engine.as_deref(), Some("spark-native"));
    assert_eq!(query.parameters.as_deref(), Some(" params '$X'\\u.json "));
    drop(commands);
    let run = parsed(
        &[
            "run",
            path,
            "--input",
            "-",
            "--wait",
            "--timeout",
            " duration '$T' ",
        ],
        Some("csv"),
        Path::new("native.data"),
    );
    let Operation::Run(run) = &run.operation else {
        panic!("run DTO required")
    };
    assert_eq!(run.input.as_deref(), Some("-"));
    assert!(run.wait);
    assert_eq!(run.timeout.as_deref(), Some(" duration '$T' "));
    let execute = parsed(
        &[
            "execute",
            path,
            "--file",
            " script '$X'\\n.sql ",
            "--parameters",
            "-",
            "--wait",
        ],
        Some("parquet"),
        Path::new("native.data"),
    );
    let Operation::Execute(execute) = execute.operation else {
        panic!("execute DTO required")
    };
    assert_eq!(
        execute.text,
        TextSource::File(" script '$X'\\n.sql ".into())
    );
    assert_eq!(execute.parameters.as_deref(), Some("-"));
    assert!(execute.wait);
}

#[test]
fn parser_rejects_ambiguous_stdin_blank_statements_and_wrong_kind_flags() {
    for arguments in [
        vec!["query", "p"],
        vec!["execute", "p"],
        vec!["query", "p", "--text", "x", "--file", "f"],
        vec!["execute", "p", "--text", "x", "--file", "f"],
        vec!["query", "p", "--text", " \n\t"],
        vec!["execute", "p", "--text", ""],
        vec!["query", "p", "--file", "-", "--parameters", "-"],
        vec!["execute", "p", "--file", "-", "--parameters", "-"],
        vec!["run", "p", "--text", "x"],
        vec!["run", "p", "--parameters", "f"],
        vec!["query", "p", "--text", "x", "--wait"],
        vec!["query", "p", "--text", "x", "--language", "sql"],
        vec!["execute", "p", "--text", "x", "--script", "f"],
        vec!["execute", "p", "--text", "x", "--engine", "spark-native"],
        vec!["execute", "p", "--text", "x", "--timeout", "1s"],
        vec!["run", "p", "--force"],
        vec!["execute", "p", "--text", "x", "--approved"],
        vec!["query", "p", "--text", "x", "--unknown"],
        vec!["run", " "],
        vec!["run", "p", "--input", ""],
    ] {
        assert!(
            parse_args(["fab-rs"].into_iter().chain(arguments.iter().copied())).is_err(),
            "{arguments:?}"
        );
    }
    let command = parsed(
        &["query", "p", "--text", "-", "--parameters", "-"],
        None,
        Path::new("unused"),
    );
    assert!(
        matches!(command.operation, Operation::Query(_)),
        "inline '-' does not read stdin"
    );
    assert!(parse_args(["fab-rs", "invoke", "p"]).is_err());
}

#[test]
fn shared_flags_defaults_and_batch_registry_are_real_without_service_defaults() {
    for (name, leaf) in ROOTS {
        let command = parsed(leaf, None, Path::new("unused"));
        assert_eq!(command.name(), *name);
        assert!(command.export.is_none());
        match &command.operation {
            Operation::Run(run) => {
                assert_eq!(run.input, None);
                assert!(!run.wait);
                assert_eq!(run.timeout, None);
            }
            Operation::Query(query) => {
                assert_eq!(query.engine, None);
                assert_eq!(query.parameters, None);
            }
            Operation::Execute(execute) => {
                assert!(!execute.wait);
                assert_eq!(execute.parameters, None);
            }
        }
        let (batch, exits) = fabric_cli::parser::batch::parse_command(leaf).unwrap();
        assert!(matches!(batch, Invocation::NativeTabular(_)));
        assert!(!exits);
        for flags in [
            &["--format", "csv"][..],
            &["--output", "native.data"][..],
            &["--format", "xml", "--output", "native.data"][..],
            &[
                "--format",
                "json",
                "--output",
                "native.data",
                "--output_format",
                "text",
            ][..],
        ] {
            assert!(
                parse_args(
                    ["fab-rs"]
                        .into_iter()
                        .chain(leaf.iter().copied())
                        .chain(flags.iter().copied())
                )
                .is_err()
            );
        }
        assert_eq!(tabular_export_source(&command), None);
    }
    let names = fabric_cli::parser::command()
        .get_subcommands()
        .map(|command| command.get_name().to_owned())
        .collect::<Vec<_>>();
    let new_names = names
        .iter()
        .filter(|name| matches!(name.as_str(), "run" | "query" | "execute"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(new_names, ["run", "query", "execute"]);
    assert!(matches!(
        parse_args(["fab-rs", "dir", "/"]).unwrap(),
        Invocation::Read(_)
    ));
    assert!(matches!(
        parse_args(["fab-rs", "job", "run", "Book.Notebook"]).unwrap(),
        Invocation::JobRun(_)
    ));
}

fn tabular_export_source(command: &NativeCommand) -> Option<&'static str> {
    fabric_cli::tabular_export::source(&Invocation::NativeTabular(command.clone()))
}

#[test]
fn prepared_read_only_approval_and_capability_rejections_never_execute() {
    for (_, leaf) in ROOTS {
        for rejected in 0..10 {
            let command = parsed(leaf, Some("json"), Path::new("native.data"));
            let mut backend = CountingBackend::new(complete());
            match rejected {
                0 => backend.capabilities.binding = Evidence::Unknown,
                1 => backend.capabilities.input = Evidence::Denied,
                2 => backend.capabilities.permission = Evidence::Denied,
                3 => backend.capabilities.tabular = Evidence::Unknown,
                4 => backend.capabilities.complete = Evidence::Denied,
                5 => backend.capabilities.schema = Evidence::Unknown,
                6 => backend.capabilities.approval = Approval::Required,
                7 => backend.capabilities.approval = Approval::Denied,
                8 => backend.capabilities.approval = Approval::Unknown,
                9 => {
                    if matches!(command.operation, Operation::Query(_)) {
                        backend.capabilities.read_only = Evidence::Unknown;
                    } else {
                        backend.capabilities.permission = Evidence::Unknown;
                    }
                }
                _ => unreachable!(),
            }
            let mut sink = Sink::default();
            let delivery = dispatch(&command, &backend, &mut sink);
            assert_ne!(delivery.exit_code, 0);
            assert_eq!(delivery.receipt["execution"]["status"], "not_started");
            assert_eq!(
                delivery.receipt["execution"]["metadata"]["binding"],
                "prepared-native-binding"
            );
            assert_eq!(backend.executes.load(Ordering::SeqCst), 0);
            assert_eq!(sink.preflights, 0);
            assert_eq!(sink.publishes, 0);
        }
    }
    let command = parsed(
        &[
            "query",
            "p",
            "--text",
            "SELECT opaque",
            "--engine",
            "not-advertised",
        ],
        Some("json"),
        Path::new("native.data"),
    );
    let backend = CountingBackend::new(complete());
    let output = dispatch(&command, &backend, &mut Sink::default());
    assert_eq!(
        output.receipt["delivery"]["error"]["code"],
        "unsupported_engine"
    );
    assert_eq!(backend.executes.load(Ordering::SeqCst), 0);
}

#[test]
fn optional_run_input_is_decided_only_by_provider_preparation() {
    let command = parsed(&["run", "p"], Some("json"), Path::new("native.data"));
    let mut backend = CountingBackend::new(complete());
    backend.require_run_input = true;
    let output = dispatch(&command, &backend, &mut Sink::default());
    assert_eq!(
        output.receipt["delivery"]["error"]["code"],
        "input_not_prepared"
    );
    assert_eq!(backend.executes.load(Ordering::SeqCst), 0);
    let backend = CountingBackend::new(complete());
    assert_eq!(
        dispatch(&command, &backend, &mut Sink::default()).exit_code,
        0
    );
    assert_eq!(backend.executes.load(Ordering::SeqCst), 1);
}

#[test]
fn preparation_failure_retains_binding_metadata_without_invoking_or_preflighting() {
    let command = parsed(
        &[
            "query",
            "p",
            "--file",
            "unreadable '$X'.sql",
            "--parameters",
            "unreadable.json",
        ],
        Some("json"),
        Path::new("native.data"),
    );
    let mut backend = CountingBackend::new(complete());
    backend.preparation_failure = Some(BackendFailure {
        code: "input_binding_failed".into(),
        message: "Provider input preparation did not complete.".into(),
        execution: ExecutionState::NotStarted,
        metadata: json!({"binding":"prepared-reference","input_validated":false}),
    });
    let mut sink = Sink::default();
    let output = dispatch(&command, &backend, &mut sink);
    assert_eq!(output.receipt["execution"]["status"], "not_started");
    assert_eq!(
        output.receipt["execution"]["metadata"]["binding"],
        "prepared-reference"
    );
    assert_eq!(
        output.receipt["delivery"]["error"]["code"],
        "input_binding_failed"
    );
    assert_eq!(backend.prepares.load(Ordering::SeqCst), 1);
    assert_eq!(backend.executes.load(Ordering::SeqCst), 0);
    assert_eq!(sink.preflights, 0);
    assert_eq!(sink.publishes, 0);
}

#[test]
fn missing_provider_and_unsupported_default_rendering_do_not_touch_inputs_or_sink() {
    for (_, leaf) in ROOTS {
        for format in [None, Some("parquet")] {
            let command = parsed(leaf, format, Path::new("native.data"));
            let mut sink = Sink::default();
            let output = native_tabular::dispatch(&command, None, &mut sink)
                .now_or_never()
                .unwrap();
            assert_eq!(output.receipt["delivery"]["error"]["code"], "NotConfigured");
            assert_eq!(output.receipt["execution"]["status"], "not_started");
            assert_eq!(sink.preflights, 0);
            assert_eq!(sink.publishes, 0);
            if format.is_none() {
                let backend = CountingBackend::new(complete());
                let output = dispatch(&command, &backend, &mut sink);
                assert_eq!(
                    output.receipt["delivery"]["error"]["code"],
                    "explicit_export_required"
                );
                assert_eq!(backend.prepares.load(Ordering::SeqCst), 0);
                assert_eq!(backend.executes.load(Ordering::SeqCst), 0);
            }
        }
    }
}

#[test]
fn execution_and_incomplete_result_metadata_survive_without_exporting_partial_data() {
    for (_, leaf) in ROOTS {
        for (outcome, state) in [
            (
                BackendResult::Accepted(json!({"handle":"accepted-native"})),
                "accepted",
            ),
            (
                BackendResult::Partial(json!({"partial_rows":3,"handle":"partial-native"})),
                "partial",
            ),
            (
                BackendResult::Unknown(json!({"unknown_effects":true,"handle":"unknown-native"})),
                "unknown",
            ),
        ] {
            let command = parsed(leaf, Some("parquet"), Path::new("native.data"));
            let backend = CountingBackend::new(outcome);
            let mut sink = Sink::default();
            let output = dispatch(&command, &backend, &mut sink);
            assert_ne!(output.exit_code, 0);
            assert_eq!(output.receipt["execution"]["status"], state);
            assert!(output.receipt["execution"]["metadata"]["handle"].is_string());
            assert_eq!(backend.executes.load(Ordering::SeqCst), 1);
            assert_eq!(sink.publishes, 0);
        }
        for state in [
            ExecutionState::Failed,
            ExecutionState::Unknown,
            ExecutionState::Succeeded,
        ] {
            let command = parsed(leaf, Some("json"), Path::new("native.data"));
            let backend = CountingBackend::new(complete());
            *backend.outcome.lock().unwrap() = Some(Err(BackendFailure {
                code: "native_result_failed".into(),
                message: "Native result is unavailable.".into(),
                execution: state,
                metadata: json!({"handle":"retained-native-effect"}),
            }));
            let mut sink = Sink::default();
            let output = dispatch(&command, &backend, &mut sink);
            assert_eq!(output.receipt["execution"]["status"], state.name());
            assert_eq!(
                output.receipt["execution"]["metadata"]["handle"],
                "retained-native-effect"
            );
            assert_eq!(sink.publishes, 0);
            assert_eq!(backend.executes.load(Ordering::SeqCst), 1);
        }
    }
}

#[test]
fn known_multiple_sets_reject_before_execute_and_unexpected_sets_are_never_truncated() {
    for (_, leaf) in ROOTS {
        let command = parsed(leaf, Some("json"), Path::new("native.data"));
        let mut backend = CountingBackend::new(complete());
        backend.capabilities.resultsets = Some(2);
        let mut sink = Sink::default();
        let output = dispatch(&command, &backend, &mut sink);
        assert_eq!(output.receipt["execution"]["status"], "not_started");
        assert_eq!(
            output.receipt["delivery"]["error"]["code"],
            "multiple_resultsets"
        );
        assert_eq!(backend.executes.load(Ordering::SeqCst), 0);
        assert_eq!(sink.preflights, 0);
        let mut result = TableResult::single("provider", rows());
        result.resultsets.push(rows());
        result.metadata = json!({"handle":"native-multiple","sets":2});
        let mut backend = CountingBackend::new(BackendResult::complete(result));
        backend.capabilities.resultsets = None;
        let output = dispatch(&command, &backend, &mut sink);
        assert_eq!(output.receipt["execution"]["status"], "succeeded");
        assert_eq!(output.receipt["execution"]["metadata"]["sets"], 2);
        assert_eq!(
            output.receipt["delivery"]["error"]["code"],
            "multiple_resultsets"
        );
        assert_eq!(backend.executes.load(Ordering::SeqCst), 1);
        assert_eq!(sink.publishes, 0);
    }
}

#[test]
fn shared_preflight_and_delivery_errors_never_replay_the_native_operation() {
    for (_, leaf) in ROOTS {
        let command = parsed(leaf, Some("json"), Path::new("native.data"));
        let backend = CountingBackend::new(complete());
        let mut sink = Sink {
            reject_preflight: true,
            ..Default::default()
        };
        let output = dispatch(&command, &backend, &mut sink);
        assert_eq!(output.receipt["execution"]["status"], "not_started");
        assert_eq!(backend.executes.load(Ordering::SeqCst), 0);
        let backend = CountingBackend::new(complete());
        let mut sink = Sink {
            reject_publish: true,
            ..Default::default()
        };
        let output = dispatch(&command, &backend, &mut sink);
        assert_eq!(output.receipt["execution"]["status"], "succeeded");
        assert_eq!(output.receipt["delivery"]["status"], "failed");
        assert_eq!(
            output.receipt["execution"]["metadata"]["handle"],
            "real-provider-handle"
        );
        assert_eq!(backend.executes.load(Ordering::SeqCst), 1);
        assert_eq!(sink.publishes, 1);
    }
}

#[test]
fn native_empty_schema_and_unexpected_schema_absence_stay_truthful() {
    for (_, leaf) in ROOTS {
        for format in ["json", "csv", "parquet"] {
            let command = parsed(leaf, Some(format), Path::new("empty.data"));
            let mut result = TableResult::single("provider", Vec::new());
            result.empty_schema = Some(
                TabularTable::new(
                    vec![TabularColumn::new("native_id", TabularType::Int64, true)],
                    Vec::new(),
                )
                .unwrap(),
            );
            let backend = CountingBackend::new(BackendResult::complete(result));
            let mut sink = Sink::default();
            let output = dispatch(&command, &backend, &mut sink);
            assert_eq!(output.exit_code, 0);
            assert_eq!(output.receipt["delivery"]["rows"], 0);
            assert_eq!(output.receipt["delivery"]["schema"][0]["name"], "native_id");
            let backend = CountingBackend::new(BackendResult::complete(TableResult::single(
                "provider",
                Vec::new(),
            )));
            let mut sink = Sink::default();
            let output = dispatch(&command, &backend, &mut sink);
            assert_eq!(output.receipt["execution"]["status"], "succeeded");
            assert_eq!(
                output.receipt["delivery"]["error"]["code"],
                "schema_unavailable"
            );
            assert_eq!(sink.publishes, 0);
        }
    }
}

#[test]
fn native_declared_types_and_nullability_are_preserved_without_inference() {
    let table = TabularTable::new(
        vec![
            TabularColumn::new("unsigned", TabularType::UInt64, true),
            TabularColumn::new("all_null", TabularType::String, true),
        ],
        vec![vec![TabularValue::UInt64(1), TabularValue::Null]],
    )
    .unwrap();
    for (_, leaf) in ROOTS {
        for (format, encoding) in [
            ("json", TabularFormat::Json),
            ("csv", TabularFormat::Csv),
            ("parquet", TabularFormat::Parquet),
        ] {
            let command = parsed(leaf, Some(format), Path::new("typed.data"));
            let result = TableResult::from_table("native", table.clone());
            let backend = CountingBackend::new(BackendResult::complete(result));
            let mut sink = Sink::default();
            let output = dispatch(&command, &backend, &mut sink);
            assert_eq!(output.exit_code, 0, "{:?}", output.receipt);
            assert_eq!(output.receipt["delivery"]["rows"], 1);
            assert_eq!(output.receipt["delivery"]["schema"][0]["type"], "uint64");
            assert_eq!(output.receipt["delivery"]["schema"][0]["nullable"], true);
            assert_eq!(output.receipt["delivery"]["schema"][1]["type"], "string");
            assert_eq!(sink.bytes, encode(&table, encoding).unwrap());
            assert_eq!(backend.executes.load(Ordering::SeqCst), 1);
        }
    }
}

#[test]
fn conflicting_native_typed_and_raw_data_never_silently_discard_rows() {
    let table = TabularTable::new(
        vec![TabularColumn::new("unsigned", TabularType::UInt64, true)],
        vec![vec![TabularValue::UInt64(1)]],
    )
    .unwrap();
    let mut result = TableResult::from_table("native", table);
    result.resultsets[0].push(json!({"unsigned":2}));
    let command = parsed(&["run", "p"], Some("json"), Path::new("conflicting.data"));
    let backend = CountingBackend::new(BackendResult::complete(result));
    let mut sink = Sink::default();
    let output = dispatch(&command, &backend, &mut sink);
    assert_ne!(output.exit_code, 0);
    assert_eq!(output.receipt["execution"]["status"], "succeeded");
    assert_eq!(output.receipt["delivery"]["status"], "failed");
    assert_eq!(sink.publishes, 0);
    assert_eq!(backend.executes.load(Ordering::SeqCst), 1);
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "native-tabular-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed receipt"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn actual_native_root_host_file_and_receipt_boundaries_keep_published_effects() {
    let root = Scratch::new();
    for (name, leaf) in ROOTS {
        for format in ["json", "csv", "parquet"] {
            let file = root.0.join(format!("{name}.{format}"));
            let command = parsed(leaf, Some(format), &file);
            let backend = CountingBackend::new(complete());
            let output = dispatch(&command, &backend, &mut FileSink::default());
            assert_eq!(output.exit_code, 0);
            let bytes = fs::read(&file).unwrap();
            assert!(!bytes.is_empty());
            let mut stderr = Vec::new();
            let error = output
                .write_to_streams(&mut BrokenWriter, &mut stderr)
                .map_err(ReceiptWriteFailure::into_io_error)
                .unwrap_err();
            let retained = error
                .get_ref()
                .unwrap()
                .downcast_ref::<ReceiptWriteFailure>()
                .unwrap();
            assert_eq!(
                retained.delivery.receipt["execution"]["status"],
                "succeeded"
            );
            assert_eq!(retained.delivery.receipt["delivery"]["status"], "succeeded");
            assert_eq!(backend.executes.load(Ordering::SeqCst), 1);
            assert_eq!(fs::read(&file).unwrap(), bytes);
            let backend = CountingBackend::new(complete());
            let output = dispatch(&command, &backend, &mut FileSink::default());
            assert_eq!(output.receipt["execution"]["status"], "not_started");
            assert_eq!(backend.executes.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn real_binary_missing_provider_never_reads_stdin_files_config_or_identity() {
    let root = Scratch::new();
    for (name, leaf) in [
        ("run", &["run", "arbitrary-executable", "--input", "-"][..]),
        ("query", &["query", "arbitrary-table", "--file", "-"][..]),
        (
            "execute",
            &["execute", "arbitrary-database", "--file", "-"][..],
        ),
    ] {
        for export in [false, true] {
            let file = root.0.join(format!("{name}-{export}.parquet"));
            let config = root.0.join("must-not-initialize");
            let mut process = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
            process
                .args(leaf)
                .env("FAB_RUST_CONFIG_ROOT", &config)
                .env("FAB_RUST_AUTH_MODE", "invalid")
                .env("FAB_RUST_CONTEXT", "invalid")
                .env("FAB_RUST_TENANT", "invalid")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if export {
                process.args(["--format", "parquet", "--output"]).arg(&file);
            }
            let mut child = process.spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut timed_out = false;
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            drop(child.stdin.take());
            let output = child.wait_with_output().unwrap();
            assert!(
                !timed_out,
                "an unconfigured root must not wait on stdin or invoke a backend"
            );
            assert!(!output.status.success());
            let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(receipt["delivery"]["error"]["code"], "NotConfigured");
            assert_eq!(receipt["execution"]["status"], "not_started");
            assert!(!file.exists());
            assert!(!config.exists());
            assert!(!String::from_utf8_lossy(&output.stdout).contains("arbitrary-"));
        }
    }
}
