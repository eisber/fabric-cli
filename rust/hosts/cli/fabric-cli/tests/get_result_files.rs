// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pinned `cf67fd52` GET query_and_export/do_output through the actual read session.

#![deny(unnameable_test_items)]

#[cfg(feature = "native-read")]
#[allow(dead_code)]
mod family_support;
#[allow(dead_code)]
mod folder_support;

use fabric_cli::{
    ReadConfig,
    get_output::{ResultFileHost, serialize},
    parser::{Invocation, ReadCommand, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_output::{CommandError, Timestamp, text::TextPlan};
use fabric_cli_transfer::{LocalTransferFiles, TransferFiles};
use fabric_deploy::definitions::{
    DefinitionEntry, DefinitionFile, DefinitionFileSystem, DefinitionImportFileSystem,
    ExportStorage,
};
use folder_support::{Credential, Http, Reply, Timer, complete, executor, reply, sdk};
use serde_json::{Value, json};
use std::{
    fs,
    future::{Future, pending, poll_fn, ready},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
};

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::current_dir().unwrap().join(format!(
            ".get-result-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn files(&self) -> LocalTransferFiles {
        LocalTransferFiles::new(self.0.clone(), None, Default::default()).unwrap()
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn command(root: &Root, query: &str, format: &str) -> ReadCommand {
    let Invocation::Read(command) = parse_args([
        "fab-rs",
        "get",
        "Demo.Workspace/Book.Notebook",
        "-q",
        query,
        "-o",
        root.0.to_str().unwrap(),
        "--output_format",
        format,
    ])
    .unwrap() else {
        panic!("read required")
    };
    command
}

fn replies() -> Vec<Reply> {
    vec![
        reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]})),
        reply(json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]})),
        reply(
            json!({"id":"n","displayName":"Book","description":"","properties":{"name":"café 🚀","order":[2,1]}}),
        ),
    ]
}

fn run(
    command: &ReadCommand,
    files: &mut dyn TransferFiles,
    replies: Vec<Reply>,
    cancelled: impl Future<Output = ()> + Send,
) -> (TextPlan, Vec<String>) {
    complete(run_async(command, files, replies, cancelled))
}

async fn run_async(
    command: &ReadCommand,
    files: &mut dyn TransferFiles,
    replies: Vec<Reply>,
    cancelled: impl Future<Output = ()> + Send,
) -> (TextPlan, Vec<String>) {
    run_async_clock(command, files, replies, cancelled, &|| {
        Ok("20261102_135959".into())
    })
    .await
}

async fn run_async_clock(
    command: &ReadCommand,
    files: &mut dyn TransferFiles,
    replies: Vec<Reply>,
    cancelled: impl Future<Output = ()> + Send,
    clock: &(dyn Fn() -> Result<String, CommandError> + Send + Sync),
) -> (TextPlan, Vec<String>) {
    let http = Arc::new(Http::new(replies));
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
    let result = reads
        .dispatch_with_result_files(
            command,
            &mut ResultFileHost {
                files,
                clock,
                default_output_format: None,
            },
            cancelled,
        )
        .await
        .unwrap();
    http.assert_consumed();
    let requests = http.requests.lock().unwrap().clone();
    (result, requests)
}

fn streams(plan: &TextPlan) -> (String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    plan.write_to(&mut out, &mut err).unwrap();
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn projected_unicode_object_writes_once_without_envelope_in_both_formats() {
    for format in ["json", "text"] {
        let root = Root::new();
        let (plan, requests) = run(
            &command(&root, "properties", format),
            &mut root.files(),
            replies(),
            pending(),
        );
        assert_eq!(plan.exit_code, 0, "{:?}", streams(&plan));
        assert_eq!(requests.len(), 3);
        let bytes = fs::read(
            root.0
                .join("Book.Notebook")
                .join("get_20261102_135959.json"),
        )
        .unwrap();
        let expected = if cfg!(windows) {
            "{\r\n    \"name\": \"café 🚀\",\r\n    \"order\": [\r\n        2,\r\n        1\r\n    ]\r\n}"
        } else {
            "{\n    \"name\": \"café 🚀\",\n    \"order\": [\n        2,\n        1\n    ]\n}"
        };
        assert_eq!(bytes, expected.as_bytes());
        let (out, err) = streams(&plan);
        assert!(out.contains("Export completed"));
        assert!(!out.contains("café"));
        assert!(err.starts_with("Exporting result to '"));
        assert!(err.ends_with("get_20261102_135959.json'...\n"));
    }
}

#[test]
fn empty_projection_is_exported_and_timestamp_collisions_overwrite_without_force() {
    let root = Root::new();
    let selected = command(&root, "description", "json");
    let path = root.0.join("Book.Notebook").join("get_20261102_135959");
    fs::create_dir(path.parent().unwrap()).unwrap();
    fs::write(&path, "old content").unwrap();
    for force in [false, true] {
        let mut selected = selected.clone();
        selected.force = force;
        let (plan, requests) = run(&selected, &mut root.files(), replies(), pending());
        assert_eq!(plan.exit_code, 0, "{:?}", streams(&plan));
        assert_eq!(requests.len(), 3);
        assert_eq!(fs::read(&path).unwrap(), b"");
    }
}

#[test]
fn source_storage_serialization_distinguishes_json_strings_text_and_scalars() {
    for (value, expected, json) in [
        (json!("false"), "false", true),
        (json!("null"), "null", true),
        (json!("\"café\""), "\"café\"", true),
        (json!("plain café"), "plain café", false),
        (json!(false), "False", false),
        (Value::Null, "None", false),
        (json!(0), "0", false),
        (json!([]), "[]", true),
    ] {
        assert_eq!(
            serialize(&value, ExportStorage::Local).unwrap(),
            (expected.as_bytes().to_vec(), json)
        );
    }
    assert_eq!(
        serialize(&json!({"é":"🚀"}), ExportStorage::Lakehouse).unwrap(),
        (br#"{"\u00e9": "\ud83d\ude80"}"#.to_vec(), true),
    );
    assert!(serialize(&json!(false), ExportStorage::Lakehouse).is_err());
}

#[test]
fn cancellation_invalid_query_and_missing_destination_never_create_output() {
    let root = Root::new();
    let selected = command(&root, "id", "json");
    let (plan, requests) = run(&selected, &mut root.files(), vec![], ready(()));
    assert_eq!(plan.exit_code, 1);
    assert!(requests.is_empty());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
    let mut invalid = selected.clone();
    invalid.query = Some("[".into());
    let (plan, requests) = run(&invalid, &mut root.files(), replies(), pending());
    assert_eq!(plan.exit_code, 1);
    assert_eq!(requests.len(), 3);
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
    let mut missing = selected;
    missing.output = Some(root.0.join("missing").to_str().unwrap().into());
    let (plan, requests) = run(&missing, &mut root.files(), replies(), pending());
    assert_eq!(plan.exit_code, 1);
    assert_eq!(requests.len(), 3);
    assert!(streams(&plan).1.is_empty());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
}

struct FailedFiles {
    root: PathBuf,
    writes: usize,
    interrupt: Option<Arc<AtomicBool>>,
}

impl DefinitionFileSystem for FailedFiles {
    fn preflight(&mut self, _: &Path, _: &[DefinitionFile]) -> io::Result<()> {
        Ok(())
    }
    fn create_directory(&mut self, _: &Path, _: &str) -> io::Result<()> {
        Ok(())
    }
    fn write(&mut self, _: &Path, _: &DefinitionFile) -> io::Result<()> {
        self.writes += 1;
        if let Some(interrupt) = &self.interrupt {
            fs::write(self.root.join("partial.bin"), b"partial")?;
            interrupt.store(true, Ordering::SeqCst);
            return Ok(());
        }
        Err(io::Error::other("synthetic partial write"))
    }
}
impl DefinitionImportFileSystem for FailedFiles {
    fn entries(&mut self, _: &Path, _: &str) -> io::Result<Vec<DefinitionEntry>> {
        unreachable!()
    }
    fn read(&mut self, _: &Path, _: &str, _: usize) -> io::Result<Vec<u8>> {
        unreachable!()
    }
}
impl TransferFiles for FailedFiles {
    fn resolve_directory(&mut self, _: &str) -> io::Result<PathBuf> {
        Ok(self.root.clone())
    }
    fn is_empty(&mut self, _: &Path) -> io::Result<bool> {
        unreachable!()
    }
}

#[test]
fn failed_write_retains_prior_stderr_notice_without_success_or_service_replay() {
    let root = Root::new();
    let mut files = FailedFiles {
        root: root.0.clone(),
        writes: 0,
        interrupt: None,
    };
    let (plan, requests) = run(
        &command(&root, "id", "json"),
        &mut files,
        replies(),
        pending(),
    );
    assert_eq!(plan.exit_code, 1);
    assert_eq!(files.writes, 1, "{:?}", streams(&plan));
    assert_eq!(requests.len(), 3);
    let (out, err) = streams(&plan);
    assert!(err.starts_with("Exporting result to '"));
    assert!(out.contains("synthetic partial write"));
    assert!(!out.contains("Export completed"));
}

#[test]
fn cancellation_after_write_retains_side_effects_and_never_replays_reads() {
    let root = Root::new();
    let interrupted = Arc::new(AtomicBool::new(false));
    let mut files = FailedFiles {
        root: root.0.clone(),
        writes: 0,
        interrupt: Some(interrupted.clone()),
    };
    let signal = poll_fn(|_| {
        if interrupted.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let (plan, requests) = run(&command(&root, "id", "json"), &mut files, replies(), signal);
    assert_eq!(plan.exit_code, 1);
    assert_eq!(requests.len(), 3);
    assert_eq!(files.writes, 1, "{:?}", streams(&plan));
    assert_eq!(fs::read(root.0.join("partial.bin")).unwrap(), b"partial");
    let (out, err) = streams(&plan);
    assert!(err.starts_with("Exporting result to '"));
    assert!(out.contains("not rolled back"));
    assert!(!out.contains("Export completed"));
}

#[test]
fn unsafe_outside_root_and_file_destinations_are_rejected_without_writes() {
    let root = Root::new();
    fs::write(root.0.join("ordinary-file"), b"keep").unwrap();
    for output in [
        root.0.join("..").join("outside"),
        root.0.join("ordinary-file"),
    ] {
        let mut selected = command(&root, "id", "json");
        selected.output = Some(output.to_str().unwrap().into());
        let (plan, requests) = run(&selected, &mut root.files(), replies(), pending());
        assert_eq!(plan.exit_code, 1);
        assert_eq!(requests.len(), 3);
        assert!(streams(&plan).1.is_empty());
        assert_eq!(fs::read(root.0.join("ordinary-file")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    }
}

#[test]
fn workspace_output_is_projected_after_all_source_enrichment() {
    let root = Root::new();
    let mut selected = command(&root, "sparkSettings.enabled", "json");
    selected.path = vec!["Demo.Workspace".into()];
    let (plan, requests) = run(
        &selected,
        &mut root.files(),
        vec![
            reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]})),
            reply(json!({"id":"w","displayName":"Demo"})),
            reply(json!({"value":[]})),
            reply(json!({"enabled":false})),
            reply(json!({"value":[]})),
        ],
        pending(),
    );
    assert_eq!(plan.exit_code, 0, "{:?}", streams(&plan));
    assert_eq!(requests.len(), 5);
    assert_eq!(
        fs::read(root.0.join("Demo.Workspace").join("get_20261102_135959")).unwrap(),
        b"False",
    );
}

#[cfg(feature = "native-read")]
#[test]
fn native_current_thread_session_uses_the_existing_async_filesystem_worker() {
    let root = Root::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut files = fabric_cli::families::transfer_files::TokioTransferFiles::defer_export_root(
            root.0.clone(),
            root.0.to_str().unwrap().into(),
            Default::default(),
            tokio::runtime::Handle::current(),
        );
        let (plan, requests) = run_async(
            &command(&root, "properties", "json"),
            &mut files,
            replies(),
            pending(),
        )
        .await;
        assert_eq!(plan.exit_code, 0, "{:?}", streams(&plan));
        assert_eq!(requests.len(), 3);
        assert!(
            root.0
                .join("Book.Notebook")
                .join("get_20261102_135959.json")
                .is_file()
        );
    });
}

#[test]
fn clock_failure_is_explicit_and_never_creates_output_or_reports_success() {
    let root = Root::new();
    let clock = || {
        Err(CommandError::new(
            Some("The local timezone offset could not be determined for GET output".into()),
            Some("ClockFailed".into()),
        ))
    };
    let (plan, requests) = complete(run_async_clock(
        &command(&root, "id", "json"),
        &mut root.files(),
        replies(),
        pending(),
        &clock,
    ));
    assert_eq!(plan.exit_code, 1);
    assert_eq!(requests.len(), 3);
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
    let (out, err) = streams(&plan);
    assert!(err.is_empty());
    assert!(out.contains("ClockFailed"));
    assert!(!out.contains("Export completed"));
}

#[cfg(feature = "native-read")]
#[test]
fn injected_non_utc_clock_preserves_source_local_date_across_utc_day_boundary() {
    let local = time::Date::from_calendar_date(2026, time::Month::November, 2)
        .unwrap()
        .with_hms(0, 30, 0)
        .unwrap()
        .assume_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap());
    assert_eq!(
        fabric_cli::get_output::filename_stamp(local),
        "20261102_003000"
    );
    let utc_stamp = fabric_cli::get_output::filename_stamp(local.to_offset(time::UtcOffset::UTC));
    assert_eq!(utc_stamp, "20261101_223000");
    let root = Root::new();
    let (plan, requests) = complete(run_async_clock(
        &command(&root, "id", "json"),
        &mut root.files(),
        replies(),
        pending(),
        &|| Ok(fabric_cli::get_output::filename_stamp(local)),
    ));
    assert_eq!(plan.exit_code, 0, "{:?}", streams(&plan));
    assert_eq!(requests.len(), 3);
    assert!(
        root.0
            .join("Book.Notebook")
            .join("get_20261102_003000")
            .is_file()
    );
    assert!(
        !root
            .0
            .join("Book.Notebook")
            .join(format!("get_{utc_stamp}"))
            .exists()
    );
}

#[cfg(feature = "native-read")]
#[tokio::test]
async fn native_lakehouse_result_uses_real_storage_bytes_and_never_activates_local_fallback() {
    use std::{collections::VecDeque, sync::Mutex};
    const WS: &str = "11111111-1111-1111-1111-111111111111";
    const ITEM: &str = "22222222-2222-2222-2222-222222222222";
    const LAKE: &str = "66666666-6666-6666-6666-666666666666";
    for flush in [200, 400] {
        let root = Root::new();
        let workspaces = json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]});
        let items = json!({"value":[
            {"id":ITEM,"displayName":"Book","type":"Notebook"},
            {"id":LAKE,"displayName":"Lake","type":"Lakehouse"},
        ]});
        let http = Arc::new(family_support::Http {
            replies: Mutex::new(VecDeque::from([
                (200, workspaces.clone(), Default::default()),
                (200, items.clone(), Default::default()),
                (
                    200,
                    json!({"id":ITEM,"displayName":"Book","properties":{"name":"café 🚀"}}),
                    Default::default(),
                ),
                (200, workspaces, Default::default()),
                (200, items, Default::default()),
                (201, Value::Null, Default::default()),
                (202, Value::Null, Default::default()),
                (flush, Value::Null, Default::default()),
            ])),
            requests: Mutex::default(),
        });
        let executor = fabric_sdk_http::HttpExecutor::new(
            http.clone(),
            Arc::new(Timer::default()),
            Default::default(),
        )
        .unwrap();
        let sdk = fabric_sdk::Client::new(
            Arc::new(family_support::Credential::default()),
            fabric_sdk::CredentialSelection::new(
                "offline-account",
                "offline-tenant",
                fabric_sdk::Audience::Fabric,
            )
            .unwrap(),
        );
        let reads = ReadSession {
            sdk: &sdk,
            executor: &executor,
            tenant: Element::tenant("Tenant", "offline-tenant"),
            current_path: "/",
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            config: ReadConfig::default(),
        };
        let mut selected = command(&root, "properties", "json");
        selected.output = Some("Demo.Workspace/Lake.Lakehouse/Files".into());
        selected.output_format = Some("text".into());
        let mut local = fabric_cli::families::transfer_files::TokioTransferFiles::defer_export_root(
            root.0.join("must-not-exist"),
            selected.output.clone().unwrap(),
            Default::default(),
            tokio::runtime::Handle::current(),
        );
        let mut files = fabric_cli::get_output::native_files(&mut local, &reads, &executor);
        let plan = reads
            .dispatch_with_result_files(
                &selected,
                &mut ResultFileHost {
                    files: &mut files,
                    clock: &|| Ok("20261102_135959".into()),
                    default_output_format: Some("json"),
                },
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(
            plan.exit_code,
            u8::from(flush != 200),
            "{:?}",
            streams(&plan)
        );
        assert!(http.replies.lock().unwrap().is_empty());
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 8);
        assert!(requests[5]["url"].as_str().unwrap().contains(&format!(
            "/{WS}/{LAKE}/Files/Book.Notebook/get_20261102_135959.json/?resource=file"
        )));
        let expected = r#"{"name": "caf\u00e9 \ud83d\ude80"}"#;
        assert_eq!(requests[6]["raw_body"], expected);
        assert!(
            requests[7]["url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("action=flush&position={}", expected.len()))
        );
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
        let (out, err) = streams(&plan);
        assert!(err.starts_with("Exporting result to '"));
        assert_eq!(out.contains("Export completed"), flush == 200);
        if flush == 200 {
            let result: Value = serde_json::from_str(&out).unwrap();
            assert!(result.get("command").is_none());
        } else {
            assert!(!out.starts_with('{'));
        }
    }
}
