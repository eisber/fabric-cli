// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit export parser, native adapter, delivery, and reusable result contracts.
//! Command-boundary contracts for shared tabular exports.

#![deny(unnameable_test_items)]

#[allow(dead_code, unused_imports)]
mod folder_support;

use fabric_cli::{
    parser::{Invocation, parse_args},
    tabular_export::{
        self, ExecutionFailure, ExportCommand, ExportRequest, ExportSink, FileSink, TableResult,
    },
};
use fabric_cli_output::tabular::{TabularFormat, TabularTable, encode};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    fs,
    future::ready,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

const LEAVES: &[(&[&str], &str)] = &[
    (&["ls", "/"], "ls"),
    (&["dir", "/"], "ls"),
    (
        &["job", "run-list", "Demo.Workspace/Book.Notebook"],
        "job run-list",
    ),
    (&["acl", "ls", "Demo.Workspace"], "acl ls"),
    (&["acl", "dir", "Demo.Workspace"], "acl ls"),
    (&["acl", "get", "Demo.Workspace", "-q", "."], "acl get"),
    (&["find", "*"], "find"),
    (&["label", "list-local"], "label list-local"),
    (&["config", "ls"], "config ls"),
    (&["config", "dir"], "config ls"),
    (
        &[
            "table",
            "schema",
            "Demo.Workspace/Lake.Lakehouse/Tables/orders",
        ],
        "table schema",
    ),
];

fn parsed(leaf: &[&str], format: &str, output: &Path) -> ExportCommand {
    let output = output.to_str().unwrap();
    let args = ["fab-rs"]
        .into_iter()
        .chain(leaf.iter().copied())
        .chain(["--format", format, "--output", output]);
    let Invocation::Tabular(command) = parse_args(args).unwrap() else {
        panic!("explicit export must retain its production command");
    };
    command
}

fn fixture_rows() -> Vec<Value> {
    vec![
        json!({"first": "a,\"b\n", "nested": {"x": 1}}),
        json!({"later": 7, "first": "last"}),
    ]
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
            Err(io::Error::other("preflight"))
        } else {
            Ok(())
        }
    }

    fn publish(&mut self, _: &ExportRequest, bytes: &[u8]) -> io::Result<()> {
        self.publishes += 1;
        if self.reject_publish {
            Err(io::Error::other("publish"))
        } else {
            self.bytes = bytes.to_vec();
            Ok(())
        }
    }
}

#[test]
fn all_declared_leaves_share_parser_dispatch_and_all_three_codecs() {
    for (leaf, source) in LEAVES {
        for (format, codec) in [
            ("csv", TabularFormat::Csv),
            ("json", TabularFormat::Json),
            ("parquet", TabularFormat::Parquet),
        ] {
            let export = parsed(leaf, format, Path::new("table.data"));
            let mut sink = Sink::default();
            let calls = Cell::new(0);
            let delivery = tabular_export::dispatch(&export, &mut sink, |command| {
                calls.set(calls.get() + 1);
                assert_eq!(tabular_export::source(command), Some(*source));
                ready(Ok(tabular_export::adapt(
                    command,
                    Some(Value::Array(fixture_rows())),
                )))
            })
            .now_or_never()
            .unwrap();
            assert_eq!(calls.get(), 1);
            assert_eq!(delivery.exit_code, 0, "{leaf:?}");
            assert_eq!(sink.preflights, 1);
            assert_eq!(sink.publishes, 1);
            assert_eq!(
                sink.bytes,
                encode(&TabularTable::from_rows(&fixture_rows()).unwrap(), codec).unwrap()
            );
            assert_eq!(delivery.receipt["source"], *source);
            assert_eq!(delivery.receipt["delivery"]["rows"], 2);
            let receipt = serde_json::to_string(&delivery.receipt).unwrap();
            assert!(
                receipt.contains("later"),
                "schema must retain later-only keys"
            );
            let mut stdout = Vec::new();
            delivery.write_to(&mut stdout).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&stdout).unwrap(),
                delivery.receipt
            );
            assert!(!stdout.starts_with(b"PAR1"));
        }
    }
}

#[test]
fn all_leaves_preserve_defaults_and_reject_incomplete_unknown_or_conflicting_flags() {
    for (leaf, _) in LEAVES {
        let legacy = parse_args(["fab-rs"].into_iter().chain(leaf.iter().copied())).unwrap();
        assert!(!matches!(legacy, Invocation::Tabular(_)));
        for flags in [
            &["--format", "csv"][..],
            &["--format", "xml", "--output", "table.data"][..],
            &[
                "--format",
                "csv",
                "--output",
                "table.data",
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
                .is_err(),
                "{leaf:?} {flags:?}"
            );
        }
        if leaf.get(1) != Some(&"get") {
            assert!(
                parse_args(
                    ["fab-rs"]
                        .into_iter()
                        .chain(leaf.iter().copied())
                        .chain(["--output", "table.data"])
                )
                .is_err()
            );
        }
    }
    // Existing ACL GET output keeps its legacy parsing when no export format is present.
    assert!(matches!(
        parse_args([
            "fab-rs",
            "acl",
            "get",
            "Demo.Workspace",
            "--output",
            "legacy"
        ])
        .unwrap(),
        Invocation::Access(_)
    ));
}

#[test]
fn non_tabular_leaves_and_incomplete_native_arguments_reject_export_flags() {
    for leaf in [
        &["get", "Demo.Workspace"][..],
        &["api", "workspaces"][..],
        &[
            "job",
            "run-status",
            "Demo.Workspace/Book.Notebook",
            "--id",
            "id",
        ][..],
        &["job", "start", "Demo.Workspace/Book.Notebook"][..],
        &["config", "get", "output_format"][..],
        &["run"][..],
        &["query"][..],
        &["execute"][..],
    ] {
        assert!(
            parse_args(["fab-rs"].into_iter().chain(leaf.iter().copied()).chain([
                "--format",
                "json",
                "--output",
                "table.data"
            ]))
            .is_err(),
            "{leaf:?}"
        );
    }
    // TABLE load retains its input format; it never receives the export output flag.
    assert!(
        parse_args([
            "fab-rs",
            "table",
            "load",
            "Demo.Workspace/Lake.Lakehouse/Tables/orders",
            "--format",
            "csv",
            "--output",
            "table.data"
        ])
        .is_err()
    );
}

#[test]
fn preflight_failure_never_invokes_any_declared_operation() {
    for (leaf, _) in LEAVES {
        let export = parsed(leaf, "csv", Path::new("table.data"));
        let mut sink = Sink {
            reject_preflight: true,
            ..Default::default()
        };
        let delivery = tabular_export::dispatch(&export, &mut sink, |_| async {
            panic!("preflight must precede remote execution");
        })
        .now_or_never()
        .unwrap();
        assert_eq!(delivery.receipt["execution"]["status"], "not_started");
        assert_eq!(sink.publishes, 0);
    }
}

#[test]
fn operation_failure_never_publishes_partial_rows() {
    let export = parsed(&["ls", "/"], "json", Path::new("table.data"));
    let mut sink = Sink::default();
    let delivery = tabular_export::dispatch(&export, &mut sink, |_| {
        ready(Err(ExecutionFailure {
            code: "paging_failed".into(),
            message: "Later native page failed".into(),
        }))
    })
    .now_or_never()
    .unwrap();
    assert_eq!(delivery.receipt["execution"]["status"], "failed");
    assert_eq!(sink.publishes, 0);
}

#[test]
fn delivery_failure_retains_execution_success_without_replay() {
    let export = parsed(&["ls", "/"], "parquet", Path::new("table.data"));
    let mut sink = Sink {
        reject_publish: true,
        ..Default::default()
    };
    let calls = Cell::new(0);
    let delivery = tabular_export::dispatch(&export, &mut sink, |_| {
        calls.set(calls.get() + 1);
        ready(Ok(TableResult::single("ls", fixture_rows())))
    })
    .now_or_never()
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(delivery.exit_code, 1);
    assert_eq!(delivery.receipt["execution"]["status"], "succeeded");
    assert_eq!(delivery.receipt["delivery"]["status"], "failed");
    assert_eq!(sink.publishes, 1);
}

#[test]
fn scalar_metadata_missing_labels_and_non_object_projection_never_become_fake_rows() {
    for (leaf, data) in [
        (&["ls", "/"][..], Some(json!({"count": 1}))),
        (
            &["acl", "get", "Demo.Workspace"][..],
            Some(json!(["properties", "not rows"])),
        ),
        (&["label", "list-local"][..], None),
        (&["find", "*"][..], Some(json!(1))),
    ] {
        let export = parsed(leaf, "csv", Path::new("table.data"));
        let mut sink = Sink::default();
        let delivery = tabular_export::dispatch(&export, &mut sink, |command| {
            ready(Ok(tabular_export::adapt(command, data)))
        })
        .now_or_never()
        .unwrap();
        assert_eq!(delivery.receipt["execution"]["status"], "succeeded");
        assert_eq!(
            delivery.receipt["delivery"]["error"]["code"],
            "invalid_table"
        );
        assert_eq!(sink.publishes, 0);
    }
}

#[test]
fn future_native_result_entry_retains_labels_and_rejects_multiple_sets_explicitly() {
    let request = ExportRequest {
        format: TabularFormat::Json,
        output: "table.data".into(),
    };
    for source in ["run", "query", "execute"] {
        let mut sink = Sink::default();
        let mut result = TableResult::single(source, fixture_rows());
        result.metadata = json!({"records_affected":7,"duration_ms":3});
        let delivery =
            tabular_export::deliver(&request, source, Some(1), &mut sink, || ready(Ok(result)))
                .now_or_never()
                .unwrap();
        assert_eq!(delivery.receipt["source"], source);
        assert_eq!(delivery.exit_code, 0);
        assert_eq!(
            delivery.receipt["execution"]["metadata"]["records_affected"],
            7
        );
        let data: Value = serde_json::from_slice(&sink.bytes).unwrap();
        assert!(
            data[0].get("records_affected").is_none(),
            "operation metadata must not become fake result columns"
        );
        let rejected = tabular_export::deliver(&request, source, Some(2), &mut sink, || async {
            panic!("known multiple resultsets must fail before execution");
        })
        .now_or_never()
        .unwrap();
        assert_eq!(rejected.receipt["execution"]["status"], "not_started");
        let unknown = tabular_export::deliver(&request, source, None, &mut sink, || {
            ready(Ok(TableResult {
                source: source.into(),
                resultsets: vec![fixture_rows(), fixture_rows()],
                validation_error: None,
                json_columns: Vec::new(),
                metadata: Value::Null,
                empty_schema: None,
                declared_table: None,
            }))
        })
        .now_or_never()
        .unwrap();
        assert_eq!(unknown.receipt["execution"]["status"], "succeeded");
        assert_eq!(
            unknown.receipt["delivery"]["error"]["code"],
            "multiple_resultsets"
        );
    }
}

#[test]
fn schema_metadata_cells_remain_structured_and_are_delegated_to_the_common_codec() {
    let export = parsed(
        &[
            "table",
            "schema",
            "Demo.Workspace/Lake.Lakehouse/Tables/orders",
        ],
        "json",
        Path::new("schema.json"),
    );
    let rows = vec![
        json!({
            "name": "amount",
            "type": {"type": "decimal", "precision": 18, "scale": 2},
            "nullable": true,
            "metadata": {"comment": "Declared JSON metadata, not typed engine values"},
        }),
        json!({"name":"id","type":"long","nullable":false,"metadata":{}}),
    ];
    let mut sink = Sink::default();
    let delivery = tabular_export::dispatch(&export, &mut sink, |command| {
        ready(Ok(tabular_export::adapt(command, Some(json!(rows)))))
    })
    .now_or_never()
    .unwrap();
    assert_eq!(delivery.exit_code, 0);
    let data: Value = serde_json::from_slice(&sink.bytes).unwrap();
    assert_eq!(data[0]["type"]["precision"], 18);
    assert!(data[0]["metadata"].is_object());
    assert_eq!(
        data[1]["type"], "long",
        "explicit JSON metadata must preserve scalar type descriptions"
    );
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "tabular-export-{}-{}",
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

#[test]
fn actual_local_parser_dispatch_writes_data_only_and_json_receipts() {
    let root = Scratch::new();
    let store = fabric_cli_local::ConfigStore::new(root.0.join("config"));
    let labels = root.0.join("labels.json");
    fs::write(&labels, br#"{"labels":[{"name":"Public","id":"label-1"}]}"#).unwrap();
    let mut settings = store.initialize().unwrap();
    settings.insert("local_definition_labels".into(), json!(labels));
    store.write(&settings).unwrap();
    for (leaf, source) in [
        (&["config", "ls"][..], "config ls"),
        (&["config", "dir"][..], "config ls"),
        (&["label", "list-local"][..], "label list-local"),
    ] {
        let file = root.0.join(format!("{}.json", leaf.join("-")));
        let export = parsed(leaf, "json", &file);
        let delivery =
            tabular_export::dispatch_local(&export, &store, &mut FileSink::default()).unwrap();
        assert_eq!(delivery.exit_code, 0);
        assert_eq!(delivery.receipt["source"], source);
        let data: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        assert!(data.is_array());
        assert!(data.get("execution").is_none());
    }
}

#[test]
fn native_file_sink_is_atomic_no_clobber_and_receipt_failure_keeps_file_evidence() {
    let root = Scratch::new();
    let file = root.0.join("result.parquet");
    let export = parsed(&["ls", "/"], "parquet", &file);
    let mut sink = FileSink::default();
    let delivery = tabular_export::dispatch(&export, &mut sink, |_| {
        ready(Ok(TableResult::single("ls", fixture_rows())))
    })
    .now_or_never()
    .unwrap();
    assert_eq!(delivery.exit_code, 0);
    let original = fs::read(&file).unwrap();
    assert!(original.starts_with(b"PAR1") && original.ends_with(b"PAR1"));
    let rejected = tabular_export::dispatch(&export, &mut FileSink::default(), |_| async {
        panic!("existing destination must prevent execution");
    })
    .now_or_never()
    .unwrap();
    assert_eq!(rejected.receipt["execution"]["status"], "not_started");
    assert_eq!(fs::read(&file).unwrap(), original);
    assert!(delivery.write_to(&mut BrokenWriter).is_err());
    assert_eq!(delivery.receipt["execution"]["status"], "succeeded");
    assert_eq!(delivery.receipt["delivery"]["status"], "succeeded");
    assert_eq!(fs::read(file).unwrap(), original);
}

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("broken receipt stream"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_destination_created_during_execution_is_not_overwritten() {
    let root = Scratch::new();
    let file = root.0.join("result.json");
    let export = parsed(&["ls", "/"], "json", &file);
    let delivery = tabular_export::dispatch(&export, &mut FileSink::default(), |_| {
        assert!(!file.exists());
        fs::write(&file, b"other writer").unwrap();
        ready(Ok(TableResult::single("ls", fixture_rows())))
    })
    .now_or_never()
    .unwrap();
    assert_eq!(delivery.receipt["execution"]["status"], "succeeded");
    assert_eq!(delivery.receipt["delivery"]["status"], "failed");
    assert_eq!(fs::read(&file).unwrap(), b"other writer");
    assert_eq!(
        fs::read_dir(&root.0).unwrap().count(),
        1,
        "staging files must be removed"
    );
}

#[test]
fn binary_preflight_precedes_remote_host_or_configuration_access() {
    let root = Scratch::new();
    let file = root.0.join("exists.json");
    fs::write(&file, b"preserve").unwrap();
    let config = root.0.join("must-not-initialize");
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["ls", "/", "--format", "json", "--output"])
        .arg(&file)
        .env("FAB_RUST_CONFIG_ROOT", &config)
        .env("FAB_RUST_TENANT", "invalid")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["execution"]["status"],
        "not_started"
    );
    assert!(!config.exists());
    assert_eq!(fs::read(file).unwrap(), b"preserve");
}

#[cfg(unix)]
#[test]
fn symlinks_in_destination_or_parent_are_not_followed() {
    use std::os::unix::fs::symlink;
    let root = Scratch::new();
    let real = root.0.join("real");
    fs::create_dir(&real).unwrap();
    let link = root.0.join("link");
    symlink(&real, &link).unwrap();
    let request = ExportRequest {
        format: TabularFormat::Csv,
        output: link.join("data.csv"),
    };
    assert!(tabular_export::preflight(&request).is_err());
    let destination = root.0.join("destination.csv");
    symlink(real.join("missing.csv"), &destination).unwrap();
    let request = ExportRequest {
        output: destination,
        ..request
    };
    assert!(tabular_export::preflight(&request).is_err());
}

#[cfg(unix)]
#[test]
fn parent_replacement_during_execution_cannot_redirect_the_export_through_a_link() {
    use std::os::unix::fs::symlink;
    let root = Scratch::new();
    let selected = root.0.join("selected");
    let original = root.0.join("original");
    let other = root.0.join("other");
    fs::create_dir(&selected).unwrap();
    fs::create_dir(&other).unwrap();
    let export = parsed(&["ls", "/"], "json", &selected.join("data.json"));
    let delivery = tabular_export::dispatch(&export, &mut FileSink::default(), |_| {
        fs::rename(&selected, &original).unwrap();
        symlink(&other, &selected).unwrap();
        ready(Ok(TableResult::single("ls", fixture_rows())))
    })
    .now_or_never()
    .unwrap();
    assert!(
        !other.join("data.json").exists(),
        "no ancestor link may redirect publication"
    );
    assert_ne!(delivery.exit_code, 0);
    assert_eq!(delivery.receipt["execution"]["status"], "succeeded");
    assert!(
        !original.join("data.json").exists(),
        "a changed directory binding must not produce a success file"
    );
}

#[test]
fn actual_native_job_export_preserves_later_page_columns_and_never_publishes_partial_data() {
    use fabric_cli::{ReadConfig, jobs::JobReadSession, session::ReadSession};
    use fabric_cli_context::{Element, HomeContext};
    use fabric_cli_output::Timestamp;
    use folder_support::{Credential, Http, Reply, Timer, complete, executor, reply, sdk};
    use std::sync::Arc;
    for failure in [
        None,
        Some((401, "Unauthorized", 4)),
        Some((403, "Forbidden", 1)),
    ] {
        let http = Arc::new(Http::new([
            reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]})),
            reply(json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]})),
            reply(json!({"value":[{"id":"first","status":null}],"continuationToken":"next"})),
            if let Some((status, code, _)) = failure {
                Reply {
                    status,
                    body: json!({"errorCode":code,"message":"Offline denial"}),
                    headers: Default::default(),
                }
            } else {
                reply(json!({"value":[{"id":"last","later":true}]}))
            },
        ]));
        let credential = Arc::new(Credential::default());
        let executor = executor(http.clone(), Arc::new(Timer::default()));
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
        let jobs = JobReadSession {
            reads: &reads,
            jobs: Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor.clone())),
        };
        let export = parsed(
            &["job", "run-list", "Demo.Workspace/Book.Notebook"],
            "json",
            Path::new("table.data"),
        );
        let mut sink = Sink::default();
        let delivery = complete(tabular_export::dispatch(
            &export,
            &mut sink,
            |command| async move {
                let Invocation::JobRead(job) = command else {
                    panic!("expected job command")
                };
                tabular_export::job_rows(&jobs, job).await
            },
        ));
        assert_eq!(
            http.requests.lock().unwrap().len(),
            4,
            "execution must not repeat"
        );
        http.assert_consumed();
        if let Some((_, code, exit)) = failure {
            assert_eq!(delivery.exit_code, exit);
            assert_eq!(delivery.receipt["execution"]["status"], "failed");
            assert_eq!(delivery.receipt["delivery"]["error"]["code"], code);
            assert_eq!(sink.publishes, 0);
            assert!(sink.bytes.is_empty());
        } else {
            assert_eq!(delivery.exit_code, 0);
            let data: Value = serde_json::from_slice(&sink.bytes).unwrap();
            assert_eq!(
                data[1]["later"], true,
                "legacy first-row projection must not truncate an explicit export"
            );
            assert_eq!(data[0]["later"], Value::Null);
        }
    }
}

#[test]
fn actual_native_ls_export_and_no_clobber_use_the_existing_read_session() {
    use fabric_cli::{ReadConfig, session::ReadSession};
    use fabric_cli_context::{Element, HomeContext};
    use fabric_cli_output::Timestamp;
    use folder_support::{Http, Timer, complete, executor, reply, sdk};
    use std::sync::Arc;
    let http = Arc::new(Http::new([reply(
        json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
    )]));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let sdk = sdk(Arc::default());
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let export = parsed(&["dir", "/"], "csv", Path::new("table.data"));
    let mut sink = Sink::default();
    let reads_ref = &reads;
    let delivery = complete(tabular_export::dispatch(
        &export,
        &mut sink,
        |command| async move {
            let Invocation::Read(read) = command else {
                panic!("expected read command")
            };
            tabular_export::read_rows(reads_ref, read).await
        },
    ));
    assert_eq!(delivery.exit_code, 0);
    assert!(
        String::from_utf8(sink.bytes)
            .unwrap()
            .contains("Demo.Workspace")
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    http.assert_consumed();
    let mut rejected = Sink {
        reject_preflight: true,
        ..Default::default()
    };
    let delivery = complete(tabular_export::dispatch(
        &export,
        &mut rejected,
        |command| async move {
            let Invocation::Read(read) = command else {
                panic!("expected read command")
            };
            tabular_export::read_rows(reads_ref, read).await
        },
    ));
    assert_eq!(delivery.receipt["execution"]["status"], "not_started");
    assert_eq!(
        http.requests.lock().unwrap().len(),
        1,
        "preflight rejection cannot send another request"
    );
}

#[test]
fn actual_binary_config_ls_exports_all_codecs_without_authentication() {
    let root = Scratch::new();
    let config = root.0.join("isolated-config");
    for format in ["json", "csv", "parquet"] {
        let file = root.0.join(format!("config.{format}"));
        let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
            .args(["config", "ls", "--format", format, "--output"])
            .arg(&file)
            .env("FAB_RUST_CONFIG_ROOT", &config)
            .env("FAB_RUST_AUTH_MODE", "deliberately-invalid")
            .env("FAB_RUST_TENANT", "deliberately-invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{format}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["source"], "config ls");
        assert_eq!(receipt["execution"]["status"], "succeeded");
        assert_eq!(receipt["delivery"]["status"], "succeeded");
        assert_eq!(
            receipt["delivery"]["rows"],
            fabric_cli_local::SETTINGS.len()
        );
        let bytes = fs::read(file).unwrap();
        match format {
            "json" => {
                let rows: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    rows.as_array().unwrap().len(),
                    fabric_cli_local::SETTINGS.len()
                );
                assert!(rows[0].get("setting").is_some());
                assert!(rows[0].get("execution").is_none());
            }
            "csv" => assert!(bytes.starts_with(b"setting,value\r\n")),
            "parquet" => assert!(bytes.starts_with(b"PAR1") && bytes.ends_with(b"PAR1")),
            _ => unreachable!("fixed format list"),
        }
    }
}

#[test]
fn reusable_argument_builder_parses_identical_flags_without_registering_backend_roots() {
    let adapter = || {
        tabular_export::arguments(
            clap::Command::new("adapter").arg(
                clap::Arg::new("output_format")
                    .long("output_format")
                    .value_parser(["json", "text"]),
            ),
            Some("output_format"),
        )
        .unwrap()
    };
    for (name, expected) in [
        ("csv", TabularFormat::Csv),
        ("json", TabularFormat::Json),
        ("parquet", TabularFormat::Parquet),
    ] {
        let matches = adapter()
            .try_get_matches_from([
                "adapter",
                "--format",
                name,
                "--output",
                "native-result.data",
            ])
            .unwrap();
        let request = tabular_export::request(&matches).unwrap();
        assert_eq!(request.format, expected);
        assert_eq!(request.output, Path::new("native-result.data"));
    }
    for flags in [
        &["--format", "csv"][..],
        &["--output", "result.data"][..],
        &["--format", "xml", "--output", "result.data"][..],
        &[
            "--format",
            "json",
            "--output",
            "result.data",
            "--output_format",
            "text",
        ][..],
    ] {
        assert!(
            adapter()
                .try_get_matches_from(["adapter"].into_iter().chain(flags.iter().copied()))
                .is_err()
        );
    }
    let with_input_format =
        clap::Command::new("adapter").arg(clap::Arg::new("input_format").long("format"));
    assert_eq!(
        tabular_export::arguments(with_input_format.clone(), None)
            .unwrap_err()
            .flag,
        "format"
    );
    assert!(
        with_input_format
            .try_get_matches_from(["adapter", "--format", "native-input"])
            .is_ok()
    );
    let with_output =
        clap::Command::new("adapter").arg(clap::Arg::new("destination").long("output"));
    assert_eq!(
        tabular_export::arguments(with_output, None)
            .unwrap_err()
            .flag,
        "output"
    );
    let without_presentation =
        tabular_export::arguments(clap::Command::new("adapter"), None).unwrap();
    assert!(
        without_presentation
            .try_get_matches_from(["adapter", "--format", "csv", "--output", "result.data"])
            .is_ok()
    );
}

#[test]
fn public_result_validation_is_source_agnostic_and_keeps_metadata_out_of_columns() {
    use tabular_export::ResultValidationError;
    for source in ["run", "query", "execute"] {
        let mut result = TableResult::single(source, fixture_rows());
        result.metadata = json!({"records_affected":7});
        let table = tabular_export::validate_result(&result).unwrap();
        assert_eq!(table.rows().len(), 2);
        assert_eq!(table.columns().len(), 3);
        assert!(
            !table
                .columns()
                .iter()
                .any(|column| column.name() == "records_affected")
        );
        let mut multiple = result.clone();
        multiple.resultsets.push(fixture_rows());
        assert!(matches!(
            tabular_export::validate_result(&multiple),
            Err(ResultValidationError::ResultSetCount(2))
        ));
        assert!(matches!(
            tabular_export::validate_result(&TableResult::invalid(source)),
            Err(ResultValidationError::InvalidShape)
        ));
        let conflicting =
            TableResult::single(source, vec![json!({"value":1}), json!({"value":"text"})]);
        assert!(matches!(
            tabular_export::validate_result(&conflicting),
            Err(ResultValidationError::Table(_))
        ));
    }
}

#[test]
fn config_value_metadata_uses_common_json_columns_for_all_native_value_types() {
    let root = Scratch::new();
    let store = fabric_cli_local::ConfigStore::new(root.0.join("config"));
    let mut settings = store.initialize().unwrap();
    settings.insert(
        "default_capacity".into(),
        json!({"id":"capacity","tags":["native"]}),
    );
    settings.insert("default_az_location".into(), json!(7));
    settings.insert("cache_enabled".into(), json!(true));
    settings.insert("default_open_experience".into(), Value::Null);
    store.write(&settings).unwrap();
    let result = tabular_export::config_rows(&store).unwrap();
    assert_eq!(result.json_columns, ["value"]);
    let table = tabular_export::validate_result(&result).unwrap();
    for (name, format) in [
        ("json", TabularFormat::Json),
        ("csv", TabularFormat::Csv),
        ("parquet", TabularFormat::Parquet),
    ] {
        let export = parsed(&["config", "ls"], name, Path::new("config.data"));
        let mut sink = Sink::default();
        let output = tabular_export::dispatch_local(&export, &store, &mut sink).unwrap();
        assert_eq!(output.exit_code, 0, "{:?}", output.receipt);
        assert_eq!(sink.bytes, encode(&table, format).unwrap());
        assert_eq!(output.receipt["delivery"]["schema"][1]["type"], "json");
        if name == "json" {
            let rows: Vec<Value> = serde_json::from_slice(&sink.bytes).unwrap();
            for setting in [
                "default_capacity",
                "default_az_location",
                "cache_enabled",
                "default_open_experience",
            ] {
                let row = rows.iter().find(|row| row["setting"] == setting).unwrap();
                assert_eq!(
                    row["value"], settings[setting],
                    "metadata must retain native JSON values"
                );
            }
        }
    }
}

fn native_empty_export(
    leaf: &[&str],
    format: &str,
    replies: Vec<folder_support::Reply>,
) -> (tabular_export::ExportDelivery, Sink, usize) {
    use fabric_cli::{ReadConfig, jobs::JobReadSession, session::ReadSession};
    use fabric_cli_context::{Element, HomeContext};
    use fabric_cli_output::Timestamp;
    use folder_support::{Http, Timer, complete, executor, sdk};
    use std::sync::Arc;
    let http = Arc::new(Http::new(replies));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let sdk = sdk(Arc::default());
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let jobs = JobReadSession {
        reads: &reads,
        jobs: Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor.clone())),
    };
    let export = parsed(leaf, format, Path::new("empty.data"));
    let mut sink = Sink::default();
    let reads_ref = &reads;
    let jobs_ref = &jobs;
    let output = complete(tabular_export::dispatch(
        &export,
        &mut sink,
        |command| async move {
            match command {
                Invocation::Read(read) => tabular_export::read_rows(reads_ref, read).await,
                Invocation::JobRead(job) => tabular_export::job_rows(jobs_ref, job).await,
                _ => panic!("expected native read/list command"),
            }
        },
    ));
    let requests = http.requests.lock().unwrap().len();
    http.assert_consumed();
    (output, sink, requests)
}

fn assert_empty_table(
    output: &tabular_export::ExportDelivery,
    sink: &Sink,
    format: &str,
    names: &[&str],
) {
    assert_eq!(output.exit_code, 0, "{:?}", output.receipt);
    assert_eq!(output.receipt["delivery"]["rows"], 0);
    assert_eq!(sink.publishes, 1);
    let schema = output.receipt["delivery"]["schema"].as_array().unwrap();
    assert_eq!(
        schema
            .iter()
            .map(|column| column["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        names
    );
    match format {
        "json" => assert_eq!(
            serde_json::from_slice::<Value>(&sink.bytes).unwrap(),
            json!([])
        ),
        "csv" => assert_eq!(sink.bytes, format!("{}\r\n", names.join(",")).into_bytes()),
        "parquet" => assert!(sink.bytes.starts_with(b"PAR1") && sink.bytes.ends_with(b"PAR1")),
        _ => unreachable!("fixed format list"),
    }
}

#[test]
fn actual_empty_native_ls_uses_resolved_command_schema_for_all_formats() {
    use folder_support::reply;
    for (leaf, names, workspace) in [
        (&["ls", "/"][..], &["name"][..], false),
        (
            &["dir", "/", "--long"][..],
            &["name", "id", "capacityName", "capacityId", "capacityRegion"][..],
            false,
        ),
        (&["ls", "Demo.Workspace"][..], &["name"][..], true),
        (
            &["dir", "Demo.Workspace", "--long"][..],
            &["name", "id"][..],
            true,
        ),
    ] {
        for format in ["json", "csv", "parquet"] {
            let mut replies = Vec::new();
            if workspace {
                replies.push(reply(
                    json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
                ));
            }
            replies.push(reply(json!({"value":[]})));
            let (output, sink, requests) = native_empty_export(leaf, format, replies);
            assert_empty_table(&output, &sink, format, names);
            assert_eq!(requests, if workspace { 2 } else { 1 });
        }
    }
}

#[test]
fn actual_empty_native_folder_ls_retains_schema_when_source_ignores_query() {
    use folder_support::reply;
    for long in [false, true] {
        let mut leaf = vec!["ls", "Demo.Workspace/Empty.Folder", "-q", "[]"];
        if long {
            leaf.push("--long");
        }
        for format in ["json", "csv", "parquet"] {
            let graph = reply(json!({"value":[{"id":"empty","displayName":"Empty"}]}));
            let replies = vec![
                reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]})),
                graph.clone(),
                reply(json!({"value":[]})),
                graph,
            ];
            let (output, sink, requests) = native_empty_export(&leaf, format, replies);
            assert_empty_table(
                &output,
                &sink,
                format,
                if long { &["name", "id"] } else { &["name"] },
            );
            assert_eq!(requests, 4);
        }
    }
}

#[test]
fn actual_empty_native_job_instance_and_schedule_lists_use_documented_schemas() {
    use folder_support::reply;
    for schedule in [false, true] {
        let mut leaf = vec!["job", "run-list", "Demo.Workspace/Book.Notebook"];
        if schedule {
            leaf.push("--schedule");
        }
        let names = if schedule {
            &[
                "id",
                "enabled",
                "createdDateTime",
                "configuration",
                "owner",
                "executionData",
            ][..]
        } else {
            &[
                "id",
                "itemId",
                "jobType",
                "invokeType",
                "status",
                "rootActivityId",
                "startTimeUtc",
                "endTimeUtc",
                "failureReason",
            ][..]
        };
        for format in ["json", "csv", "parquet"] {
            let replies = vec![
                reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]})),
                reply(json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]})),
                reply(json!({"value":[]})),
            ];
            let (output, sink, requests) = native_empty_export(&leaf, format, replies);
            assert_empty_table(&output, &sink, format, names);
            assert_eq!(requests, 3);
        }
    }
}

#[test]
fn empty_arbitrary_projection_requires_schema_for_parquet_without_reexecution() {
    use folder_support::reply;
    let (output, sink, requests) = native_empty_export(
        &["ls", "/", "-q", "[?name=='missing']"],
        "parquet",
        vec![reply(
            json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
        )],
    );
    assert_eq!(output.receipt["execution"]["status"], "succeeded");
    assert_eq!(
        output.receipt["delivery"]["error"]["code"],
        "schema_unavailable"
    );
    assert_eq!(output.receipt["delivery"]["rows"], 0);
    assert_eq!(sink.publishes, 0);
    assert_eq!(requests, 1);
    let request = ExportRequest {
        format: TabularFormat::Parquet,
        output: "empty.data".into(),
    };
    let mut sink = Sink::default();
    let output = tabular_export::deliver(&request, "query", Some(1), &mut sink, || {
        ready(Ok(TableResult::single("query", Vec::new())))
    })
    .now_or_never()
    .unwrap();
    assert_eq!(
        output.receipt["delivery"]["error"]["code"],
        "schema_unavailable"
    );
    assert_eq!(sink.publishes, 0);
}

#[test]
fn real_driver_schema_enables_empty_results_but_nonempty_schema_tables_are_rejected() {
    use fabric_cli_output::tabular::{TabularColumn, TabularType};
    use tabular_export::ResultValidationError;
    for source in ["run", "query", "execute"] {
        let mut result = TableResult::single(source, Vec::new());
        result.empty_schema = Some(
            TabularTable::new(
                vec![TabularColumn::new("value", TabularType::Int64, true)],
                Vec::new(),
            )
            .unwrap(),
        );
        let table = tabular_export::validate_result(&result).unwrap();
        assert_eq!(table.row_count(), 0);
        assert_eq!(table.columns()[0].name(), "value");
        result.empty_schema = Some(TabularTable::from_rows(&fixture_rows()).unwrap());
        assert!(matches!(
            tabular_export::validate_result(&result),
            Err(ResultValidationError::InvalidShape)
        ));
    }
}

#[test]
fn empty_dynamic_label_definition_requires_real_columns_for_parquet() {
    let root = Scratch::new();
    let labels = root.0.join("labels.json");
    fs::write(&labels, br#"{"labels":[]}"#).unwrap();
    let store = fabric_cli_local::ConfigStore::new(root.0.join("config"));
    let mut settings = store.initialize().unwrap();
    settings.insert("local_definition_labels".into(), json!(labels));
    store.write(&settings).unwrap();
    let export = parsed(
        &["label", "list-local"],
        "parquet",
        Path::new("labels.parquet"),
    );
    let mut sink = Sink::default();
    let output = tabular_export::dispatch_local(&export, &store, &mut sink).unwrap();
    assert_eq!(output.receipt["execution"]["status"], "succeeded");
    assert_eq!(
        output.receipt["delivery"]["error"]["code"],
        "schema_unavailable"
    );
    assert_eq!(sink.publishes, 0);
}

#[test]
fn future_native_result_adapters_keep_execution_and_delivery_failures_distinct() {
    let request = ExportRequest {
        format: TabularFormat::Json,
        output: "native-result.data".into(),
    };
    for source in ["run", "query", "execute"] {
        let calls = Cell::new(0);
        let mut sink = Sink::default();
        let execution_failure =
            tabular_export::deliver(&request, source, Some(1), &mut sink, || {
                calls.set(calls.get() + 1);
                ready(Err(ExecutionFailure {
                    code: "native_failed".into(),
                    message: "Native operation failed".into(),
                }))
            })
            .now_or_never()
            .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(execution_failure.receipt["source"], source);
        assert_eq!(execution_failure.receipt["execution"]["status"], "failed");
        assert_eq!(execution_failure.receipt["delivery"]["stage"], "execution");
        assert_eq!(sink.publishes, 0);
        let mut sink = Sink {
            reject_publish: true,
            ..Default::default()
        };
        let delivery_failure =
            tabular_export::deliver(&request, source, Some(1), &mut sink, || {
                calls.set(calls.get() + 1);
                let mut result = TableResult::single(source, fixture_rows());
                result.metadata = json!({"records_affected":7});
                ready(Ok(result))
            })
            .now_or_never()
            .unwrap();
        assert_eq!(
            calls.get(),
            2,
            "each explicitly requested operation executes exactly once"
        );
        assert_eq!(delivery_failure.receipt["source"], source);
        assert_eq!(delivery_failure.receipt["execution"]["status"], "succeeded");
        assert_eq!(
            delivery_failure.receipt["execution"]["metadata"]["records_affected"],
            7
        );
        assert_eq!(delivery_failure.receipt["delivery"]["status"], "failed");
        assert_eq!(delivery_failure.receipt["delivery"]["rows"], 2);
        assert!(delivery_failure.receipt["delivery"]["schema"].is_array());
        assert_eq!(sink.publishes, 1);
    }
}
