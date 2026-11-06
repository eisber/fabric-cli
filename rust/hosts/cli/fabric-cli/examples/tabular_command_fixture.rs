// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline consumer of the real run/query/execute frontdoors and shared exporter.
//!
//! The explicitly configured fixture backend performs no service or input I/O.
//! Supply an existing empty output directory as the sole positional argument.

use fabric_cli::{
    native_tabular::{
        self, Approval, Backend, BackendFailure, BackendFuture, BackendResult, Capabilities,
        Evidence, ExecutionState, NativeCommand, PreparedOperation,
    },
    tabular_export::{FileSink, TableResult},
};
use fabric_cli_output::tabular::{TabularColumn, TabularTable, TabularType};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    error::Error,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture {
    prepares: AtomicUsize,
    executions: AtomicUsize,
    capabilities: Capabilities,
}

impl Fixture {
    fn new() -> Self {
        Self {
            prepares: AtomicUsize::new(0),
            executions: AtomicUsize::new(0),
            capabilities: Capabilities {
                binding: Evidence::Proven,
                input: Evidence::Proven,
                permission: Evidence::Proven,
                read_only: Evidence::Proven,
                approval: Approval::NotRequired,
                tabular: Evidence::Proven,
                complete: Evidence::Proven,
                schema: Evidence::Proven,
                resultsets: Some(1),
                engines: vec!["offline-fixture".into()],
                metadata: json!({"binding":"explicit-offline-fixture","service":false}),
            },
        }
    }
}

struct PreparedFixture<'a>(&'a Fixture);

impl Backend for Fixture {
    fn prepare<'a>(
        &'a self,
        _: &'a NativeCommand,
    ) -> BackendFuture<'a, Result<Box<dyn PreparedOperation<'a> + 'a>, BackendFailure>> {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(Box::new(PreparedFixture(self)) as Box<dyn PreparedOperation<'a> + 'a>)
        })
    }
}

impl<'a> PreparedOperation<'a> for PreparedFixture<'a> {
    fn capabilities(&self) -> &Capabilities {
        &self.0.capabilities
    }

    fn execute(self: Box<Self>) -> BackendFuture<'a, Result<BackendResult, BackendFailure>> {
        self.0.executions.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            let rows = vec![
                json!({
                    "signed": i64::MIN,
                    "unsigned": u64::MAX,
                    "decimal": 1.5,
                    "text": "comma, \"quotes\"\r\n$HOME\\literal \u{96ea}",
                    "active": true,
                    "optional": null
                }),
                json!({
                    "signed": 0,
                    "unsigned": 1_u64,
                    "decimal": 0.0,
                    "text": "",
                    "active": false,
                    "optional": ""
                }),
            ];
            let failure = |_| BackendFailure {
                code: "fixture_schema_failed".into(),
                message: "The explicit fixture schema is invalid.".into(),
                execution: ExecutionState::Failed,
                metadata: json!({"synthetic":true}),
            };
            let inferred = TabularTable::from_rows(&rows).map_err(failure)?;
            let table = TabularTable::new(
                vec![
                    TabularColumn::new("signed", TabularType::Int64, false),
                    TabularColumn::new("unsigned", TabularType::UInt64, false),
                    TabularColumn::new("decimal", TabularType::Float64, false),
                    TabularColumn::new("text", TabularType::String, false),
                    TabularColumn::new("active", TabularType::Boolean, true),
                    TabularColumn::new("optional", TabularType::String, true),
                ],
                inferred.rows().to_vec(),
            )
            .map_err(failure)?;
            let mut result = TableResult::from_table("fixture", table);
            result.metadata = json!({"synthetic":true,"backend_executions":1});
            Ok(BackendResult::complete(result))
        })
    }
}

fn parse(command: &str, format: &str, output: &Path) -> Result<NativeCommand, Box<dyn Error>> {
    let mut args = vec![
        "fab-rs".to_owned(),
        command.to_owned(),
        "/Fixture.Workspace/Fixture.Item".to_owned(),
    ];
    if command != "run" {
        args.extend([
            "--text".into(),
            "fixture text: \"quotes\" $money \\literal".into(),
        ]);
    }
    args.extend([
        "--format".into(),
        format.into(),
        "--output".into(),
        output
            .to_str()
            .ok_or_else(|| io::Error::other("Output directory must be UTF-8"))?
            .into(),
    ]);
    Ok(native_tabular::parse_args(args)?)
}

fn run(directory: PathBuf) -> Result<Value, Box<dyn Error>> {
    if !directory.is_dir() {
        return Err(io::Error::other("Supply an existing output directory").into());
    }
    let backend = Fixture::new();
    let mut results = Vec::new();
    for command in ["run", "query", "execute"] {
        for format in ["json", "csv", "parquet"] {
            let path = directory.join(format!("{command}.{format}"));
            let request = parse(command, format, &path)?;
            let delivery =
                native_tabular::dispatch(&request, Some(&backend), &mut FileSink::default())
                    .now_or_never()
                    .ok_or_else(|| {
                        io::Error::other("Fixture operation must complete immediately")
                    })?;
            if delivery.exit_code != 0 {
                return Err(io::Error::other(delivery.receipt.to_string()).into());
            }
            results.push(delivery.receipt);
        }
    }
    let prepares = backend.prepares.load(Ordering::SeqCst);
    let executions = backend.executions.load(Ordering::SeqCst);
    if prepares != 9 || executions != 9 {
        return Err(io::Error::other("Each fixture operation must execute exactly once").into());
    }
    Ok(json!({
        "backend":"explicit-offline-fixture",
        "service_calls":0,
        "preparations":prepares,
        "executions":executions,
        "results":results
    }))
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let directory = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("Supply an existing output directory"))?;
    if arguments.next().is_some() {
        return Err(io::Error::other("Expected exactly one output directory").into());
    }
    println!("{}", run(directory)?);
    Ok(())
}
