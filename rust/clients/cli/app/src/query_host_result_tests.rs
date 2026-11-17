// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

#[path = "../../../../engine/data-plane/aihub-query/examples/support/mod.rs"]
mod support;

use super::*;
use crate::{
    parser::{Invocation, parse_args},
    query_host::{self, BoundTarget, Providers, QueryPolicy},
    tabular_export::FileSink,
};
use arrow_array::{Array, BinaryArray, Decimal128Array, UInt64Array};
use arrow_schema::{DataType, TimeUnit};
use azure_core::Bytes;
use fabric_cli_query_commands::{
    Artifact, DataAgentRoute, InputReader, InputSource, QueryTarget, Stage,
};
use fabric_sdk_aihub_query::Limits;
use fabric_sdk_query::QueryLimits;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    future::pending,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use support::*;

const PATH: &str = "/fixture.Workspace/agent.DataAgent/datasource";
const WORKSPACE: &str = "55555555-5555-4555-8555-555555555555";
const ARTIFACT: &str = "66666666-6666-4666-8666-666666666666";

struct NoInput;

impl InputReader for NoInput {
    fn read(&mut self, _: InputSource<'_>, _: usize) -> io::Result<Vec<u8>> {
        panic!("inline root query must not open another reader")
    }
}

fn rich_rowset(duplicate: bool, empty: bool) -> String {
    let rows = if empty {
        "[]"
    } else {
        r#"[[18446744073709551615,-9223372036854775808,"",1234567890123456.7890,"AP8=","1970-01-01T00:00:00.0000001"],[null,null,null,null,null,null]]"#
    };
    format!(
        r#"{{"schemaVersion":1,"resultId":"{RESULT}","columns":[
        {{"name":"wide","engineType":"UInt64","logicalType":"integer","encoding":"json-number"}},
        {{"name":"{}","engineType":"Int64","logicalType":"integer","encoding":"json-number"}},
        {{"name":"text","engineType":"String","logicalType":"string","encoding":"text"}},
        {{"name":"amount","engineType":"decimal(20,4)","logicalType":"decimal","encoding":"json-number"}},
        {{"name":"binary","engineType":"Binary","logicalType":"binary","encoding":"base64"}},
        {{"name":"instant","engineType":"DateTime","logicalType":"datetime","encoding":"iso8601"}}],
        "rows":{rows},"rowCount":{},"truncated":false,"maxRows":100,"totalRowCount":null}}"#,
        if duplicate { "wide" } else { "signed" },
        if empty { 0 } else { 2 },
    )
}

fn complete_content(text: &str) -> Value {
    json!({"content":[typed(text,"opaque:fixture"),
        {"type":"resource","resource":{"uri":"https://never-fetch.example.invalid/template",
         "mimeType":"text/plain","text":"auxiliary template"}}]})
}

fn root_command(export: Option<(&str, &Path)>) -> crate::native_tabular::NativeCommand {
    let mut args = vec![
        OsString::from("fab-rs"),
        OsString::from("query"),
        OsString::from(PATH),
        OsString::from("--text"),
        OsString::from("opaque root query"),
    ];
    if let Some((format, path)) = export {
        args.extend([
            OsString::from("--format"),
            OsString::from(format),
            OsString::from("--output"),
            path.as_os_str().to_owned(),
        ]);
    }
    let Invocation::NativeTabular(command) = parse_args(args).unwrap() else {
        panic!("real root parser selected another command");
    };
    command
}

fn target() -> BoundTarget {
    let agent = Artifact {
        workspace: WORKSPACE.into(),
        item: AGENT.into(),
    };
    let artifact = Artifact {
        workspace: WORKSPACE.into(),
        item: ARTIFACT.into(),
    };
    BoundTarget {
        path: PATH.into(),
        target: QueryTarget {
            requested_artifact: agent.clone(),
            execution_artifact: artifact.clone(),
            data_agent: Some(DataAgentRoute {
                engine: "advertised-agent".into(),
                agent,
                datasource: SOURCE.into(),
                bound_artifact: artifact,
                stage: Stage::Production,
            }),
            native_kql: Vec::new(),
        },
    }
}

async fn observe(
    content: Value,
    export: Option<(&str, &Path)>,
    output: &mut impl ExportSink,
    deadline: &NativeDeadline,
) -> (QueryObservation, Arc<Wire>) {
    let command = root_command(export);
    let wire = Wire::new(setup().into_iter().chain([rpc(3, content)]));
    let client = client(
        wire.clone(),
        Arc::new(Credential::default()),
        Arc::new(Timer::default()),
        Limits::default(),
    );
    let prepared = query_host::prepare(
        &command,
        target(),
        Providers {
            aihub: Some(&client),
            kql: None,
        },
        QueryPolicy {
            request_id: "query;typed-root-fixture".into(),
            server_timeout: Duration::from_secs(5),
            kql_limits: QueryLimits::new(1024 * 1024, 1024 * 1024, 100, 8, 32, 32).unwrap(),
            aihub_max_rows: 100,
        },
        &mut NoInput,
        output,
    )
    .unwrap();
    (prepared.execute(deadline, pending()).await, wire)
}

fn finish(
    observed: QueryObservation,
    sink: &mut impl ExportSink,
    deadline: &NativeDeadline,
) -> QueryDelivery {
    let canceled = AtomicBool::new(false);
    observed.finish(
        ConversionLimits::new(8, 100, 32, 1024 * 1024, 8, 16).unwrap(),
        &ConversionControl::new(Instant::now() + deadline.remaining().unwrap(), &canceled),
        OutputLimits::new(100, 1024 * 1024, 8).unwrap(),
        deadline,
        &canceled,
        sink,
    )
}

#[tokio::test]
async fn actual_root_sdk_bridge_json_csv_and_parquet_preserve_declared_values() {
    for format in ["json", "csv", "parquet"] {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("explicit-format.wrong-extension");
        let mut sink = FileSink::default();
        let deadline = deadline();
        let (observed, wire) = observe(
            complete_content(&rich_rowset(false, false)),
            Some((format, &output)),
            &mut sink,
            &deadline,
        )
        .await;
        let delivery = finish(observed, &mut sink, &deadline);
        assert!(delivery.error.is_none());
        assert_eq!(delivery.receipt.exit_code, 0);
        assert_eq!(delivery.receipt.receipt["execution"]["status"], "succeeded");
        assert_eq!(delivery.receipt.receipt["delivery"]["status"], "succeeded");
        assert_eq!(wire.requests.lock().unwrap().len(), 4);
        let Some(ConvertedQuery::AiHub(converted)) = &delivery.converted else {
            panic!("actual Q3a AIHub conversion expected");
        };
        assert_eq!(converted.datasets.len(), 1);
        assert_eq!(converted.resources.len(), 1);
        assert_eq!(converted.datasets[0].result_id, RESULT);
        let table = &converted.datasets[0].table;
        let bytes = std::fs::read(&output).unwrap();
        match format {
            "json" => {
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    value["rows"][0],
                    json!([
                        "18446744073709551615",
                        "-9223372036854775808",
                        "",
                        "1234567890123456.7890",
                        "AP8=",
                        "100"
                    ])
                );
                assert_eq!(
                    value["rows"][1],
                    json!([null, null, null, null, null, null])
                );
                assert_eq!(value["schema"].as_array().unwrap().len(), 6);
            }
            "csv" => assert_eq!(
                String::from_utf8(bytes).unwrap(),
                "\"wide\",\"signed\",\"text\",\"amount\",\"binary\",\"instant\"\r\n\
                 \"18446744073709551615\",\"-9223372036854775808\",\"\",\"1234567890123456.7890\",\"AP8=\",\"100\"\r\n\
                 ,,,,,\r\n"
            ),
            "parquet" => {
                assert_eq!(&bytes[..4], b"PAR1");
                let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes)).unwrap();
                assert_eq!(builder.schema().as_ref(), table.schema().as_ref());
                let batches = builder
                    .build()
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert_eq!(batches.as_slice(), table.batches());
                let integers = batches[0]
                    .column(0)
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .unwrap();
                assert_eq!(integers.value(0), u64::MAX);
                assert!(integers.is_null(1));
                let decimals = batches[0]
                    .column(3)
                    .as_any()
                    .downcast_ref::<Decimal128Array>()
                    .unwrap();
                assert_eq!(decimals.value(0), 12_345_678_901_234_567_890_i128);
                let binary = batches[0]
                    .column(4)
                    .as_any()
                    .downcast_ref::<BinaryArray>()
                    .unwrap();
                assert_eq!(binary.value(0), &[0, 255]);
                assert_eq!(
                    batches[0].schema().field(5).data_type(),
                    &DataType::Timestamp(TimeUnit::Nanosecond, None)
                );
            }
            _ => unreachable!(),
        }
        drop(delivery);
        drop(sink);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}

#[tokio::test]
async fn no_export_renders_all_actual_native_sets_and_retains_auxiliary_resources() {
    let first = rich_rowset(true, false);
    let second = rich_rowset(false, true).replace(RESULT, "77777777-7777-4777-8777-777777777777");
    let content = json!({"content":[typed(&first,"opaque:first"),typed(&second,"opaque:second"),
        {"type":"resource","resource":{"uri":"opaque:template","mimeType":"text/plain","text":"retained"}}]});
    let mut sink = FileSink::default();
    let deadline = deadline();
    let (observed, wire) = observe(content, None, &mut sink, &deadline).await;
    let delivery = finish(observed, &mut sink, &deadline);
    assert!(delivery.error.is_none());
    let value: Value = serde_json::from_slice(delivery.presentation.as_ref().unwrap()).unwrap();
    assert_eq!(value["resultsets"].as_array().unwrap().len(), 2);
    assert_eq!(value["resultsets"][0]["schema"][0]["name"], "wide");
    assert_eq!(value["resultsets"][0]["schema"][1]["name"], "wide");
    assert_eq!(value["resultsets"][1]["rows"], json!([]));
    assert_eq!(
        value["resultsets"][1]["schema"].as_array().unwrap().len(),
        6
    );
    let Some(ConvertedQuery::AiHub(converted)) = &delivery.converted else {
        panic!("AIHub expected");
    };
    assert_eq!(converted.datasets.len(), 2);
    assert_eq!(converted.resources.len(), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 4);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        delivery.write_to_streams(&mut stdout, &mut stderr).unwrap(),
        0
    );
    assert!(stderr.is_empty());
    let written: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(written, value);
    assert!(written.get("delivery").is_none());
}

#[tokio::test]
async fn multiple_sets_fail_single_file_without_selecting_or_publishing_first() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("multiple.parquet");
    let first = rich_rowset(false, false);
    let second = rich_rowset(false, true).replace(RESULT, "77777777-7777-4777-8777-777777777777");
    let content = json!({"content":[typed(&first,"opaque:first"),typed(&second,"opaque:second")]});
    let mut sink = FileSink::default();
    let deadline = deadline();
    let (observed, wire) = observe(content, Some(("parquet", &output)), &mut sink, &deadline).await;
    let delivery = finish(observed, &mut sink, &deadline);
    assert!(matches!(
        delivery.error,
        Some(QueryDeliveryError::Encoding(
            QueryEncodingError::ResultSetCount(2)
        ))
    ));
    assert_eq!(delivery.receipt.receipt["execution"]["status"], "succeeded");
    let Some(ConvertedQuery::AiHub(converted)) = &delivery.converted else {
        panic!("AIHub expected");
    };
    assert_eq!(converted.datasets.len(), 2);
    assert!(!output.exists());
    assert_eq!(wire.requests.lock().unwrap().len(), 4);
    drop(sink);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn late_no_clobber_failure_retains_completed_query_and_only_cleans_own_staging() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("contended.parquet");
    let sentinel = directory.path().join("unrelated");
    std::fs::write(&sentinel, b"preserve").unwrap();
    let mut sink = FileSink::default();
    let deadline = deadline();
    let (observed, wire) = observe(
        complete_content(&rich_rowset(false, false)),
        Some(("parquet", &output)),
        &mut sink,
        &deadline,
    )
    .await;
    std::fs::write(&output, b"other-writer").unwrap();
    let delivery = finish(observed, &mut sink, &deadline);
    assert!(matches!(delivery.error, Some(QueryDeliveryError::File(_))));
    assert_eq!(delivery.receipt.receipt["execution"]["status"], "succeeded");
    assert_eq!(delivery.receipt.receipt["delivery"]["status"], "failed");
    assert!(delivery.converted.is_some());
    assert_eq!(wire.requests.lock().unwrap().len(), 4);
    assert_eq!(std::fs::read(&output).unwrap(), b"other-writer");
    drop(sink);
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserve");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
}

#[tokio::test]
async fn unsupported_native_decimal_metadata_is_typed_failure_not_a_success_file() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("unsupported.json");
    let content = complete_content(&rich_rowset(false, false).replace("decimal(20,4)", "decimal"));
    let mut sink = FileSink::default();
    let deadline = deadline();
    let (observed, wire) = observe(content, Some(("json", &output)), &mut sink, &deadline).await;
    let delivery = finish(observed, &mut sink, &deadline);
    assert!(matches!(
        delivery.error,
        Some(QueryDeliveryError::Conversion(_))
    ));
    assert_eq!(delivery.receipt.receipt["execution"]["status"], "succeeded");
    assert_eq!(
        delivery.receipt.receipt["execution"]["metadata"]["resultsets"][0]["resultId"],
        RESULT
    );
    assert!(!output.exists());
    assert_eq!(wire.requests.lock().unwrap().len(), 4);
    drop(sink);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn declared_empty_native_schema_survives_actual_parquet_file_readback() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("empty.data");
    let mut sink = FileSink::default();
    let deadline = deadline();
    let (observed, _) = observe(
        complete_content(&rich_rowset(false, true)),
        Some(("parquet", &output)),
        &mut sink,
        &deadline,
    )
    .await;
    let delivery = finish(observed, &mut sink, &deadline);
    assert!(delivery.error.is_none());
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(Bytes::from(std::fs::read(&output).unwrap()))
            .unwrap();
    let Some(ConvertedQuery::AiHub(converted)) = &delivery.converted else {
        panic!("AIHub expected");
    };
    assert_eq!(
        builder.schema().as_ref(),
        converted.datasets[0].table.schema().as_ref()
    );
    assert_eq!(builder.schema().fields().len(), 6);
    assert_eq!(builder.metadata().file_metadata().num_rows(), 0);
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("output unavailable"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn receipt_writer_failure_keeps_actual_published_file_and_query_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("delivered.parquet");
    let mut sink = FileSink::default();
    let deadline = deadline();
    let (observed, wire) = observe(
        complete_content(&rich_rowset(false, false)),
        Some(("parquet", &output)),
        &mut sink,
        &deadline,
    )
    .await;
    let delivery = finish(observed, &mut sink, &deadline);
    let expected = std::fs::read(&output).unwrap();
    let failure = delivery
        .write_to_streams(&mut BrokenWriter, &mut Vec::new())
        .unwrap_err();
    assert!(failure.delivery.converted.is_some());
    assert_eq!(
        failure.delivery.receipt.receipt["execution"]["status"],
        "succeeded"
    );
    assert_eq!(
        failure.delivery.receipt.receipt["delivery"]["status"],
        "succeeded"
    );
    assert_eq!(std::fs::read(&output).unwrap(), expected);
    assert_eq!(wire.requests.lock().unwrap().len(), 4);
}
