// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Output consumers retain actual stream order, explicit properties and I/O errors.

#[path = "../../../tests/compatibility/jobs/presentation_consumer.rs"]
mod consumer;

use consumer::support::*;
use fabric_cli_context::{Catalog, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use fabric_cli_jobs::{
    Confirmation, JobConfig, JobEvent, JobHost, JobOutput, JobRenderError, JobRenderOptions, JobUi,
    NoFiles, command_from_matches, job_subcommand,
};
use fabric_cli_output::{
    CommandError, OutputInput, OutputStream, Timestamp,
    text::{self, OutputFormat, Presentation, TextError, TextOptions},
};
use serde_json::{Value, json};
use std::{
    future::pending,
    io::{self, Write},
    sync::{Arc, Mutex},
};

struct Ui;
impl JobUi for Ui {
    fn emit(&self, _: JobEvent) -> Result<(), CommandError> {
        panic!("duplicate UI write")
    }
    fn confirm(&self) -> Confirmation<'_> {
        Box::pin(async { Ok(false) })
    }
}

fn options() -> JobRenderOptions<'static> {
    JobRenderOptions {
        format: OutputFormat::Text,
        text: TextOptions::default(),
        widths: &consumer::fixture_width,
        presentation: Presentation::default(),
    }
}
fn command(args: &[&str]) -> fabric_cli_jobs::JobCommand {
    command_from_matches(
        &job_subcommand()
            .try_get_matches_from(std::iter::once("job").chain(args.iter().copied()))
            .unwrap(),
    )
    .unwrap()
}
fn context() -> (Arc<Element>, Catalog) {
    let ws = Element::child(
        "demo",
        Some("ws".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("tenant", "tenant"),
    )
    .unwrap();
    let item = Element::child(
        "item",
        Some("item".into()),
        NodeKind::Item(ItemType::parse("DataPipeline").unwrap()),
        ws.clone(),
    )
    .unwrap();
    (ws.clone(), Catalog::new(vec![ws, item]))
}

#[tokio::test]
async fn all_presentation_fixture_consumers_execute_and_separate_full_streams() {
    let cases: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/jobs/presentation_cases.json"
    ))
    .unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let observed = consumer::observe(case).await.unwrap();
        assert!(observed["stdout"].is_string(), "{}", case["name"]);
        assert!(observed["stderr"].is_string(), "{}", case["name"]);
        assert!(observed["exit"].as_u64().is_some(), "{}", case["name"]);
        assert_eq!(
            observed["requests"].as_array().unwrap().len(),
            case["replies"].as_array().unwrap().len(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn result_intents_and_source_logging_notices_use_the_existing_text_renderer() {
    let result = JobOutput {
        input: OutputInput {
            command: Some("job".into()),
            data: Some(json!({"id":"run"})),
            message: Some("Job instance 'run' created".into()),
            ..OutputInput::default()
        },
        show_headers: false,
        show_key_value_list: true,
        after_output: Vec::new(),
    };
    let plan = result
        .render(
            Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            &options(),
        )
        .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        "ID: run\n\n* Job instance 'run' created\n\n"
    );
    assert!(stderr.is_empty());
    let event = JobEvent::LogWarning("service deduped.".into())
        .render(&options())
        .unwrap();
    assert_eq!(event.records[0].stream, OutputStream::Stderr);
    assert_eq!(event.records[0].payload, b"! service deduped\n");
}

#[test]
fn partial_rendering_and_unicode_properties_are_retained_not_guessed() {
    let result = JobOutput {
        input: OutputInput {
            data: Some(json!({"id":"run","camelKey":1})),
            ..OutputInput::default()
        },
        show_headers: false,
        show_key_value_list: true,
        after_output: Vec::new(),
    };
    let Err(JobRenderError::Text(error)) = result.render(
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        &options(),
    ) else {
        panic!("invalid title key accepted");
    };
    let partial = error
        .partial_output()
        .expect("source prints first key before later failure");
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    partial.write_to(&mut stdout, &mut stderr).unwrap();
    assert_eq!(stdout, b"ID: run\n");
    assert!(stderr.is_empty());
    let selected = JobRenderOptions {
        widths: &text::ascii_width,
        ..options()
    };
    assert!(matches!(
        JobEvent::Table(json!({"name":"日"})).render(&selected),
        Err(JobRenderError::Text(TextError::MissingWidth(_)))
    ));
}

#[derive(Clone)]
struct Writer {
    records: OrderedWrites,
    stderr: bool,
}
type OrderedWrites = Arc<Mutex<Vec<(bool, Vec<u8>)>>>;
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.records
            .lock()
            .unwrap()
            .push((self.stderr, bytes.to_vec()));
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn complete_runner_writes_result_before_hint_and_does_not_duplicate_ui_output() {
    let http = ScriptedHttp::new([accepted()]);
    let sdk = sdk(http.clone(), Arc::new(TestTimer::default()));
    let (ws, catalog) = context();
    let host = JobHost {
        sdk: &sdk,
        operations: None,
        lookup: &catalog,
        current: ws,
        home: HomeContext::Unspecified,
        files: &NoFiles,
        ui: &Ui,
        config: JobConfig::default(),
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
    };
    let records = Arc::new(Mutex::new(Vec::new()));
    let mut stdout = Writer {
        records: records.clone(),
        stderr: false,
    };
    let mut stderr = Writer {
        records: records.clone(),
        stderr: true,
    };
    assert_eq!(
        host.run(
            &command(&["start", "item.DataPipeline"]),
            pending(),
            &options(),
            &mut stdout,
            &mut stderr
        )
        .await
        .unwrap(),
        0
    );
    let writes = records.lock().unwrap();
    assert!(writes[0].0);
    assert!(String::from_utf8_lossy(&writes[0].1).contains("Starting job"));
    assert_eq!(writes[1], (true, b"\n".to_vec()));
    assert!(!writes[2].0);
    assert!(String::from_utf8_lossy(&writes[2].1).starts_with("ID: run\n"));
    assert!(writes[3].0);
    assert!(String::from_utf8_lossy(&writes[3].1).contains("To see status"));
    assert_eq!(writes.len(), 4);
}

#[tokio::test]
async fn output_errors_stop_local_work_without_more_requests_or_success_fallback() {
    for stderr_fails in [true, false] {
        let http = ScriptedHttp::new([accepted()]);
        let sdk = sdk(http.clone(), Arc::new(TestTimer::default()));
        let (ws, catalog) = context();
        let host = JobHost {
            sdk: &sdk,
            operations: None,
            lookup: &catalog,
            current: ws,
            home: HomeContext::Unspecified,
            files: &NoFiles,
            ui: &Ui,
            config: JobConfig::default(),
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        };
        let mut other = Vec::new();
        let result = if stderr_fails {
            host.run(
                &command(&["start", "item.DataPipeline"]),
                pending(),
                &options(),
                &mut other,
                &mut Broken,
            )
            .await
        } else {
            host.run(
                &command(&["start", "item.DataPipeline"]),
                pending(),
                &options(),
                &mut Broken,
                &mut other,
            )
            .await
        };
        assert!(matches!(result, Err(JobRenderError::Io(_))));
        assert_eq!(
            http.requests.lock().unwrap().len(),
            usize::from(!stderr_fails)
        );
        assert!(!String::from_utf8_lossy(&other).contains("To see status"));
    }
}

#[tokio::test]
async fn cancellation_and_invalid_format_are_visible_without_service_work() {
    let http = ScriptedHttp::new([]);
    let sdk = sdk(http.clone(), Arc::new(TestTimer::default()));
    let (ws, catalog) = context();
    let host = JobHost {
        sdk: &sdk,
        operations: None,
        lookup: &catalog,
        current: ws,
        home: HomeContext::Unspecified,
        files: &NoFiles,
        ui: &Ui,
        config: JobConfig::default(),
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        host.run(
            &command(&["start", "item.DataPipeline"]),
            async {},
            &options(),
            &mut stdout,
            &mut stderr
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        stdout,
        b"x job start: [LongRunningOperationCancelled] Operation cancelled\n"
    );
    assert!(stderr.is_empty());
    let mut bad = command(&["start", "item.DataPipeline"]);
    bad.output_format = Some("xml".into());
    assert!(matches!(
        host.run(&bad, pending(), &options(), &mut stdout, &mut stderr)
            .await,
        Err(JobRenderError::Text(TextError::UnsupportedFormat(_)))
    ));
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn generic_operation_json_stays_exact_and_missing_normalized_text_boundary_keeps_evidence() {
    let original = fabric_sdk_operations::OperationError {
        kind: fabric_sdk_operations::OperationErrorKind::Http {
            status: 400,
            code: Some("UnknownError".into()),
            error: None,
        },
        operation_id: Some("11111111-1111-1111-1111-111111111111".into()),
        mutation_may_have_occurred: true,
        raw_service_error: Some(Box::new(json!({"message":"..."}))),
    };
    let error = fabric_cli_jobs::JobCommandError::Operation(Box::new(original.clone()));
    let timestamp = Timestamp::parse("2000-01-01T00:00:00Z").unwrap();
    let json = error
        .render(
            timestamp.clone(),
            Some("job run-cancel"),
            &JobRenderOptions {
                format: OutputFormat::Json,
                ..options()
            },
        )
        .unwrap();
    let output: Value = serde_json::from_slice(&json.records[0].payload).unwrap();
    assert_eq!(output["result"]["message"], "");
    assert_eq!(output["result"]["error_code"], "UnknownError");
    let Err(JobRenderError::OperationTextProjectionRequired(retained)) =
        error.render(timestamp, Some("job run-cancel"), &options())
    else {
        panic!("operation must not gain a fabricated text diagnostic");
    };
    assert_eq!(*retained, original);
}

#[tokio::test]
async fn a_partial_result_write_is_not_replayed_or_followed_by_a_success_hint() {
    struct Partial {
        bytes: Vec<u8>,
    }
    impl Write for Partial {
        fn write(&mut self, value: &[u8]) -> io::Result<usize> {
            if self.bytes.is_empty() {
                let count = 4.min(value.len());
                self.bytes.extend_from_slice(&value[..count]);
                Ok(count)
            } else {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("runner must not flush caller-owned streams")
        }
    }
    let http = ScriptedHttp::new([accepted()]);
    let sdk = sdk(http.clone(), Arc::new(TestTimer::default()));
    let (ws, catalog) = context();
    let host = JobHost {
        sdk: &sdk,
        operations: None,
        lookup: &catalog,
        current: ws,
        home: HomeContext::Unspecified,
        files: &NoFiles,
        ui: &Ui,
        config: JobConfig::default(),
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
    };
    let mut stdout = Partial { bytes: Vec::new() };
    let mut stderr = Vec::new();
    let result = host
        .run(
            &command(&["start", "item.DataPipeline"]),
            pending(),
            &options(),
            &mut stdout,
            &mut stderr,
        )
        .await;
    assert!(matches!(result, Err(JobRenderError::Io(_))));
    assert_eq!(stdout.bytes, b"ID: ");
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    assert!(!String::from_utf8_lossy(&stderr).contains("To see status"));
}
