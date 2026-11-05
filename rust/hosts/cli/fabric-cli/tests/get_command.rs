// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline parser, injected-operation and exact-delivery contracts for v2 GET.

#![deny(unnameable_test_items)]

use fabric_cli::get_command::{
    DEFAULT_MAX_BYTES, DEFAULT_TIMEOUT, GetAcquireOptions, GetArgumentErrorKind, GetBackend,
    GetCommand, GetCommandError, GetDelivery, GetFormat, GetFuture, GetInterruption, GetLimits,
    GetOutput, GetPayload, GetStdoutStage, GetTargetKind, PreparedGet, ResolvedGet, parse_get,
};
use fabric_cli_storage_files::{ExactOutputErrorKind, ExactOutputStage};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    future::{Future, pending, ready},
    io::{self, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

fn parse(args: &[&str]) -> GetCommand {
    parse_get(args.iter().map(OsString::from)).unwrap()
}

fn payload(data: Value) -> GetPayload {
    GetPayload {
        data,
        raw: None,
        markdown: None,
        media_type: "application/vnd.native+json".to_owned(),
        complete: true,
    }
}

#[derive(Debug, Eq, PartialEq)]
struct NativeTarget(String);

struct Backend {
    kind: GetTargetKind,
    formats: Vec<GetFormat>,
    default_format: GetFormat,
    wait: bool,
    recursive: bool,
    payload: Mutex<Option<GetPayload>>,
    paths: Mutex<Vec<String>>,
    acquired: Mutex<Vec<(NativeTarget, GetAcquireOptions)>>,
    fail_resolve: bool,
    fail_retrieve: bool,
    stall_retrieve: bool,
    retrieve_dropped: AtomicBool,
}

impl Backend {
    fn new(payload: GetPayload) -> Self {
        Self {
            kind: GetTargetKind::Metadata,
            formats: vec![GetFormat::Json],
            default_format: GetFormat::Json,
            wait: false,
            recursive: false,
            payload: Mutex::new(Some(payload)),
            paths: Mutex::new(Vec::new()),
            acquired: Mutex::new(Vec::new()),
            fail_resolve: false,
            fail_retrieve: false,
            stall_retrieve: false,
            retrieve_dropped: AtomicBool::new(false),
        }
    }

    fn representation(mut self) -> Self {
        self.kind = GetTargetKind::Representation;
        self.formats = vec![GetFormat::Json, GetFormat::Markdown, GetFormat::Raw];
        self
    }

    fn counts(&self) -> (usize, usize) {
        (
            self.paths.lock().unwrap().len(),
            self.acquired.lock().unwrap().len(),
        )
    }
}

#[derive(Debug, thiserror::Error)]
#[error("synthetic native failure, not a message to parse")]
struct NativeFailure {
    status: u16,
}

struct DropEvidence<'a>(&'a AtomicBool);

impl Drop for DropEvidence<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl GetBackend for Backend {
    type Target = NativeTarget;

    fn resolve<'a>(
        &'a self,
        path: &'a str,
        _: GetLimits,
    ) -> GetFuture<'a, ResolvedGet<Self::Target>> {
        Box::pin(async move {
            self.paths.lock().unwrap().push(path.to_owned());
            if self.fail_resolve {
                return Err(GetCommandError::Backend {
                    code: "native_target_missing",
                    source: Box::new(NativeFailure { status: 404 }),
                });
            }
            Ok(ResolvedGet {
                target: NativeTarget("typed-sdk-resolved-operation".to_owned()),
                kind: self.kind,
                formats: self.formats.clone(),
                default_format: self.default_format,
                wait: self.wait,
                recursive: self.recursive,
            })
        })
    }

    fn retrieve<'a>(
        &'a self,
        resolved: &'a ResolvedGet<Self::Target>,
        options: GetAcquireOptions,
    ) -> GetFuture<'a, GetPayload> {
        Box::pin(async move {
            assert_eq!(resolved.target.0, "typed-sdk-resolved-operation");
            self.acquired
                .lock()
                .unwrap()
                .push((NativeTarget(resolved.target.0.clone()), options));
            let _drop_evidence = DropEvidence(&self.retrieve_dropped);
            if self.stall_retrieve {
                return pending().await;
            }
            if self.fail_retrieve {
                return Err(GetCommandError::Backend {
                    code: "native_unavailable",
                    source: Box::new(NativeFailure { status: 503 }),
                });
            }
            Ok(self.payload.lock().unwrap().take().unwrap())
        })
    }

    fn render_markdown(&self, data: &Value) -> Result<String, GetCommandError> {
        Ok(format!("# Native projection\n\n{data}"))
    }
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::current_dir().unwrap();
        loop {
            let root = base.join(format!(
                ".get-command-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Self { root },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create test-owned project directory: {error}"),
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove only this test's project directory");
    }
}

async fn stdout_delivery(
    backend: &dyn GetBackend<Target = NativeTarget>,
    command: GetCommand,
) -> Result<GetDelivery, GetCommandError> {
    // Stdout preflight must not require or inspect a working directory.
    PreparedGet::preflight(command, Path::new("not-an-absolute-cwd"))?
        .acquire(backend, pending())
        .await
}

async fn run_stdout(
    backend: &Backend,
    command: GetCommand,
    stdout: &mut Vec<u8>,
) -> Result<usize, GetCommandError> {
    stdout_delivery(backend, command).await?.write_to(stdout)
}

#[test]
fn parser_has_explicit_experimental_defaults_and_preserves_the_whole_path() {
    let path = "workspace/item with/slash/.metadata";
    let command = parse(&["get", path]);
    assert_eq!(command.path, path);
    assert_eq!(command.output, GetOutput::Stdout);
    assert_eq!(command.format, None);
    assert_eq!(command.project, None);
    assert_eq!(command.options.limits.max_bytes.get(), DEFAULT_MAX_BYTES);
    assert_eq!(command.options.limits.timeout, DEFAULT_TIMEOUT);
    assert!(!command.options.wait);
    assert!(!command.options.recursive);
    assert_eq!(command.options.depth, None);

    let projected = parse(&[
        "get",
        path,
        "-q",
        "rows[?enabled].{name: name}",
        "--recursive",
        "--depth",
        "0",
        "--max-bytes",
        "17",
        "--timeout",
        "1.5s",
        "--format",
        "json",
        "--output",
        "-",
    ]);
    assert_eq!(projected.path, path);
    assert_eq!(
        projected.project.as_deref(),
        Some("rows[?enabled].{name: name}")
    );
    assert_eq!(projected.options.depth, Some(0));
    assert_eq!(projected.options.limits.max_bytes.get(), 17);
    assert_eq!(
        projected.options.limits.timeout,
        Duration::from_millis(1500)
    );
    assert_eq!(projected.output, GetOutput::Stdout);
    assert_eq!(projected.format, Some(GetFormat::Json));
}

#[test]
fn parser_rejects_unknown_duplicate_flags_aliases_counts_and_invalid_combinations() {
    let rejected: &[&[&str]] = &[
        &[],
        &["fab-rs", "get", "path"],
        &["other", "path"],
        &["get"],
        &["get", ""],
        &["get", "path", "extra-path"],
        &["get", "path", "--unknown"],
        &["get", "path", "--rec"],
        &["get", "path", "--recursive", "--recursive"],
        &["get", "path", "--wait", "--wait"],
        &["get", "path", "--wait=true"],
        &["get", "path", "--output"],
        &["get", "path", "-o"],
        &["get", "path", "--output", "a", "b"],
        &["get", "path", "--output", "a", "--output", "b"],
        &["get", "path", "--output", "a", "-o", "b"],
        &["get", "path", "--format", "json", "--format", "raw"],
        &["get", "path", "--format", "yaml"],
        &["get", "path", "--format", "native"],
        &["get", "path", "--format", "md"],
        &["get", "path", "--output_format", "json"],
        &["get", "path", "--project"],
        &["get", "path", "--project", ""],
        &["get", "path", "--project", "x", "y"],
        &["get", "path", "--project", "x", "-q", "y"],
        &["get", "path", "--format", "raw", "-q", "value"],
        &["get", "path", "--depth", "1"],
        &["get", "path", "--recursive", "--depth", "-1"],
        &["get", "path", "--recursive", "--depth", "1.5"],
        &["get", "path", "--recursive", "--depth", "4294967296"],
        &["get", "path", "--recursive", "--depth", "1", "--depth", "2"],
        &["get", "path", "--max-bytes", "0"],
        &["get", "path", "--max-bytes", "-1"],
        &["get", "path", "--max-bytes", "1", "--max-bytes", "2"],
        &["get", "path", "--timeout", "1s", "--timeout", "2s"],
    ];
    for args in rejected {
        let result = parse_get(args.iter().map(OsString::from));
        assert!(result.is_err(), "must reject {args:?}");
        let error = result.unwrap_err();
        assert!(!error.to_string().contains("extra-path"));
    }
    assert_eq!(
        parse_get(["get", "path", "--depth", "1"].map(OsString::from))
            .unwrap_err()
            .kind,
        GetArgumentErrorKind::DepthWithoutRecursive
    );
}

#[test]
fn short_output_alias_preserves_exact_filename_and_stdout_dash() {
    let command = parse(&["get", "target", "-o", "chosen-name"]);
    assert_eq!(
        command.output,
        GetOutput::File(PathBuf::from("chosen-name"))
    );
    assert_eq!(
        parse(&["get", "target", "-o", "-"]).output,
        GetOutput::Stdout
    );
}

#[test]
fn v1_parser_retains_its_separate_existing_get_flags() {
    let legacy = fabric_cli::parser::parse_args([
        "fab-rs",
        "get",
        "path",
        "--verbose",
        "--force",
        "--query",
        "id",
    ])
    .unwrap();
    let fabric_cli::parser::Invocation::Read(command) = legacy else {
        panic!("the existing v1 command must still be a legacy read");
    };
    assert!(command.verbose);
    assert!(command.force);
    assert_eq!(command.query.as_deref(), Some("id"));
    assert!(parse_get(["get", "path", "--verbose"].map(OsString::from)).is_err());
}

#[test]
fn duration_requires_finite_positive_decimal_with_exact_suffix() {
    for (spelling, expected) in [
        ("1ms", Duration::from_millis(1)),
        ("0.5ms", Duration::from_micros(500)),
        ("2s", Duration::from_secs(2)),
        ("1.5m", Duration::from_secs(90)),
    ] {
        assert_eq!(
            parse(&["get", "p", "--timeout", spelling])            .options
            .limits
            .timeout,
            expected
        );
    }
    for spelling in [
        "",
        "0s",
        "-1s",
        "+1s",
        "1",
        "1h",
        "1S",
        "NaNs",
        "infs",
        "1e2s",
        " 1s",
        "1 s",
        ".5s",
        "1.s",
        "1.2.3s",
        "0.00000000001ms",
        "999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999s",
    ] {
        assert!(
            parse_get(["get", "p", "--timeout", spelling].map(OsString::from)).is_err(),
            "must reject {spelling:?}"
        );
    }
}

#[tokio::test]
async fn typed_projection_changes_only_delivery_and_preserves_original_evidence() {
    for value in [
        json!(7),
        json!(1.5),
        json!(true),
        Value::Null,
        json!("text"),
        json!([1, false]),
        json!({"a": 1}),
    ] {
        let data = json!({"value":value,"coverage":{"complete":true}});
        let backend = Backend::new(payload(data.clone()));
        let delivery = stdout_delivery(
            &backend,
            parse(&["get", "name/containing/slashes", "-q", "value"]),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(delivery.bytes()).unwrap(),
            value
        );
        assert_eq!(delivery.data(), &data);
        assert_eq!(delivery.media_type(), "application/json");
        assert_eq!(
            backend.paths.lock().unwrap().as_slice(),
            ["name/containing/slashes"]
        );
        assert_eq!(backend.counts(), (1, 1));
        let acquisitions = backend.acquired.lock().unwrap();
        assert_eq!(
            acquisitions[0].0,
            NativeTarget("typed-sdk-resolved-operation".to_owned())
        );
    }
}

#[tokio::test]
async fn real_jmespath_operators_and_identity_preserve_native_json_types() {
    let data = json!({
        "rows": [
            {"name": "first", "enabled": true},
            {"name": "second", "enabled": false}
        ],
        "coverage": {"complete": true}
    });
    for (expression, expected) in [
        ("rows[?enabled].name", json!(["first"])),
        ("length(rows)", json!(2)),
        ("missing", Value::Null),
        (".", data.clone()),
    ] {
        let backend = Backend::new(payload(data.clone()));
        let delivery = stdout_delivery(
            &backend,
            parse(&["get", "same/path", "--project", expression]),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(delivery.bytes()).unwrap(),
            expected
        );
        assert_eq!(delivery.data(), &data);
        assert_eq!(backend.counts(), (1, 1));
        assert_eq!(backend.paths.lock().unwrap().as_slice(), ["same/path"]);
    }
}

#[test]
fn direct_stdout_dash_preflight_needs_no_absolute_cwd_or_filesystem() {
    let mut command = parse(&["get", "target"]);
    command.output = GetOutput::File(PathBuf::from("-"));
    PreparedGet::preflight(command, Path::new("not-absolute")).unwrap();
}

#[tokio::test]
async fn existing_file_fails_preflight_before_any_backend_call() {
    let fixture = Fixture::new();
    let path = fixture.root.join("preserve");
    fs::write(&path, b"existing bytes").unwrap();
    let backend = Backend::new(payload(json!({"value": 1})));
    let error = PreparedGet::preflight(
        parse(&["get", "target", "--output", "preserve"]),
        &fixture.root,
    )
    .unwrap_err();
    let GetCommandError::File(error) = error else {
        panic!("expected original file error");
    };
    assert_eq!(error.kind, ExactOutputErrorKind::OutputExists);
    assert_eq!(error.stage, ExactOutputStage::Preflight);
    assert!(!error.path_created);
    assert_eq!(error.bytes_written, 0);
    assert_eq!(backend.counts(), (0, 0));
    assert_eq!(fs::read(path).unwrap(), b"existing bytes");
}

#[tokio::test]
async fn raw_file_uses_explicit_cwd_exact_bytes_and_no_extension() {
    let fixture = Fixture::new();
    let raw = vec![0, 255, b'\r', b'\n', 128];
    let mut source = payload(json!({
        "contentBase64": "AP8NCoA=",
        "unselectedSibling": "x".repeat(4096)
    }));
    source.raw = Some(raw.clone());
    source.media_type = "application/octet-stream".to_owned();
    let backend = Backend::new(source).representation();
    let command = parse(&[
        "get",
        "item/.native",
        "--format",
        "raw",
        "--output",
        "chosen-name",
        "--max-bytes",
        "5",
    ]);
    let prepared = PreparedGet::preflight(command, &fixture.root).unwrap();
    assert_eq!(backend.counts(), (0, 0));
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
    let delivery = prepared.acquire(&backend, pending()).await.unwrap();
    assert_eq!(delivery.bytes(), raw);
    assert_eq!(delivery.media_type(), "application/octet-stream");
    assert_eq!(
        delivery.output_path(),
        Some(fixture.root.join("chosen-name").as_path())
    );
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
    let receipt = delivery.commit_file().unwrap();
    assert_eq!(receipt.path, fixture.root.join("chosen-name"));
    assert_eq!(receipt.bytes_written, raw.len());
    assert_eq!(fs::read(receipt.path).unwrap(), raw);
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
}

#[tokio::test]
async fn raw_binary_exact_n_budget_ignores_large_base64_json_and_markdown_siblings() {
    let bytes = vec![0, 255, 1, 128];
    let data = json!({
        "contentBase64": "AP8BgA==",
        "unselectedSibling": "x".repeat(4096)
    });
    let mut source = payload(data.clone());
    source.raw = Some(bytes.clone());
    source.markdown = Some("# Unselected Markdown".repeat(256));
    source.media_type = "application/octet-stream".to_owned();
    let backend = Backend::new(source).representation();
    let delivery = stdout_delivery(
        &backend,
        parse(&[
            "get",
            "item/Files/native",
            "--format",
            "raw",
            "--max-bytes",
            "4",
        ]),
    )
    .await
    .unwrap();
    assert_eq!(delivery.data(), &data);
    assert_eq!(delivery.bytes(), bytes);
    assert_eq!(delivery.media_type(), "application/octet-stream");
    let mut stdout = Vec::new();
    assert_eq!(delivery.write_stdout(&mut stdout).unwrap(), 4);
    assert_eq!(stdout, bytes);
}

#[tokio::test]
async fn raw_binary_n_plus_one_budget_fails_on_raw_length_not_json_overhead() {
    let mut source = payload(json!({
        "contentBase64": "AP8BgAc=",
        "unselectedSibling": "x".repeat(4096)
    }));
    source.raw = Some(vec![0, 255, 1, 128, 7]);
    source.markdown = Some("# Unselected Markdown".repeat(256));
    let backend = Backend::new(source).representation();
    let mut stdout = Vec::new();
    assert!(matches!(
        run_stdout(
            &backend,
            parse(&[
                "get",
                "item/src/definition/parts/native",
                "--format",
                "raw",
                "--max-bytes",
                "4"
            ]),
            &mut stdout,
        )
        .await,
        Err(GetCommandError::ByteLimit {
            limit: 4,
            observed: 5
        })
    ));
    assert!(stdout.is_empty());
}

#[tokio::test]
async fn raw_stdout_and_native_markdown_are_unchanged_without_newlines() {
    let mut source = payload(json!({}));
    source.raw = Some(vec![255, 0, b'x']);
    let backend = Backend::new(source).representation();
    let mut stdout = Vec::new();
    assert_eq!(
        run_stdout(
            &backend,
            parse(&["get", "representation", "--output", "-", "--format", "raw"]),
            &mut stdout,
        )
        .await
        .unwrap(),
        3
    );
    assert_eq!(stdout, [255, 0, b'x']);

    let mut source = payload(json!({"value": 1}));
    source.markdown = Some("# Exact native Markdown".to_owned());
    let backend = Backend::new(source).representation();
    let delivery = stdout_delivery(
        &backend,
        parse(&["get", "representation", "--format", "markdown"]),
    )
    .await
    .unwrap();
    assert_eq!(delivery.bytes(), b"# Exact native Markdown");
    assert_eq!(delivery.media_type(), "text/markdown");
}

#[tokio::test]
async fn omitted_format_uses_path_default_and_explicit_json_overrides_markdown() {
    for (flags, expected_format, expected_bytes) in [
        (
            vec!["get", "item/docs/view"],
            GetFormat::Markdown,
            b"# Native docs".to_vec(),
        ),
        (
            vec!["get", "item/docs/view", "--format", "json"],
            GetFormat::Json,
            br#"{"native":"docs"}"#.to_vec(),
        ),
    ] {
        let mut source = payload(json!({"native": "docs"}));
        source.markdown = Some("# Native docs".to_owned());
        let mut backend = Backend::new(source).representation();
        backend.default_format = GetFormat::Markdown;
        let delivery = stdout_delivery(&backend, parse(&flags)).await.unwrap();
        assert_eq!(delivery.format(), expected_format);
        assert_eq!(delivery.bytes(), expected_bytes);
        assert!(delivery.complete());
        assert_eq!(backend.counts(), (1, 1));
    }
}

#[tokio::test]
async fn incomplete_results_only_deliver_whole_native_json_receipts_to_stdout() {
    let receipt = json!({
        "status": "accepted",
        "handle": {"path": "/operations/00000000-0000-0000-0000-000000000001"},
        "coverage": {"complete": false}
    });
    for flags in [
        vec!["get", "returned-handle"],
        vec!["get", "returned-handle", "--format", "json"],
    ] {
        let mut source = payload(receipt.clone());
        source.complete = false;
        source.raw = Some(b"never deliver partial raw content".to_vec());
        let backend = Backend::new(source);
        let delivery = stdout_delivery(&backend, parse(&flags)).await.unwrap();
        assert!(!delivery.complete());
        assert_eq!(delivery.format(), GetFormat::Json);
        assert_eq!(delivery.data(), &receipt);
        let mut stdout = Vec::new();
        delivery.write_to(&mut stdout).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&stdout).unwrap(), receipt);
    }

    let mut source = payload(receipt.clone());
    source.complete = false;
    source.markdown = Some("# Partial docs must not be delivered".to_owned());
    let mut backend = Backend::new(source).representation();
    backend.default_format = GetFormat::Markdown;
    let delivery = stdout_delivery(&backend, parse(&["get", "docs/view"]))
        .await
        .unwrap();
    assert!(!delivery.complete());
    assert_eq!(delivery.format(), GetFormat::Json);
    assert_eq!(
        serde_json::from_slice::<Value>(delivery.bytes()).unwrap(),
        receipt
    );
}

#[tokio::test]
async fn incomplete_receipts_cannot_be_projected_or_mislabeled_as_markdown() {
    for flags in [
        vec!["get", "docs/view", "--project", "value"],
        vec!["get", "docs/view", "--format", "markdown"],
    ] {
        let mut source = payload(json!({"value": 1, "coverage": {"complete": false}}));
        source.complete = false;
        source.markdown = Some("# Incomplete".to_owned());
        let mut backend = Backend::new(source).representation();
        backend.default_format = GetFormat::Markdown;
        let mut stdout = Vec::new();
        assert!(matches!(
            run_stdout(&backend, parse(&flags), &mut stdout).await,
            Err(GetCommandError::Incomplete { .. })
        ));
        assert!(stdout.is_empty());
    }
}

#[tokio::test]
async fn incomplete_or_failed_raw_acquisition_never_writes_a_stdout_prefix() {
    let mut incomplete = payload(json!({"coverage": {"complete": false, "remaining": 1}}));
    incomplete.complete = false;
    incomplete.raw = Some(b"successful-looking partial bytes".to_vec());
    let backend = Backend::new(incomplete).representation();
    let mut stdout = Vec::new();
    let error = run_stdout(
        &backend,
        parse(&["get", "representation", "--format", "raw"]),
        &mut stdout,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        GetCommandError::Incomplete { data }
            if data["coverage"]["remaining"] == json!(1)
    ));
    assert!(stdout.is_empty());

    let mut backend = Backend::new(payload(json!({}))).representation();
    backend.fail_retrieve = true;
    let error = run_stdout(
        &backend,
        parse(&["get", "representation", "--format", "raw"]),
        &mut stdout,
    )
    .await
    .unwrap_err();
    let GetCommandError::Backend { code, source } = error else {
        panic!("expected typed backend failure");
    };
    assert_eq!(code, "native_unavailable");
    assert_eq!(source.downcast_ref::<NativeFailure>().unwrap().status, 503);
    assert!(stdout.is_empty());
}

#[tokio::test]
async fn incomplete_acquisition_creates_no_file() {
    let fixture = Fixture::new();
    let mut source = payload(json!({
        "status": "accepted",
        "handle": {"operationId": "exact-native-handle"},
        "coverage": {"complete": false}
    }));
    source.complete = false;
    let backend = Backend::new(source);
    let prepared = PreparedGet::preflight(
        parse(&["get", "target", "--output", "must-not-exist"]),
        &fixture.root,
    )
    .unwrap();
    let error = prepared.acquire(&backend, pending()).await.unwrap_err();
    let GetCommandError::Incomplete { data } = error else {
        panic!("accepted source must not become an output file");
    };
    assert_eq!(data["status"], json!("accepted"));
    assert_eq!(data["handle"]["operationId"], json!("exact-native-handle"));
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[tokio::test]
async fn typed_resolution_error_preserves_native_source_without_content_or_stdout() {
    let mut backend = Backend::new(payload(json!({})));
    backend.fail_resolve = true;
    let mut stdout = Vec::new();
    let error = run_stdout(&backend, parse(&["get", "missing-target"]), &mut stdout)
        .await
        .unwrap_err();
    let GetCommandError::Backend { code, source } = error else {
        panic!("expected the original resolution failure");
    };
    assert_eq!(code, "native_target_missing");
    assert_eq!(source.downcast_ref::<NativeFailure>().unwrap().status, 404);
    assert_eq!(backend.counts(), (1, 0));
    assert!(stdout.is_empty());
}

#[tokio::test]
async fn unadvertised_formats_metadata_raw_and_wait_fail_before_content() {
    for kind in [GetTargetKind::Metadata, GetTargetKind::Representation] {
        let mut backend = Backend::new(payload(json!({})));
        backend.kind = kind;
        let error = stdout_delivery(&backend, parse(&["get", "target", "--format", "markdown"]))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            GetCommandError::UnsupportedFormat(GetFormat::Markdown)
        ));
        assert_eq!(backend.counts(), (1, 0));

        let error = stdout_delivery(&backend, parse(&["get", "target", "--wait"]))
            .await
            .unwrap_err();
        assert!(matches!(error, GetCommandError::WaitRequiresHandle));
        assert_eq!(backend.counts(), (2, 0));
    }
    let mut backend = Backend::new(payload(json!({})));
    backend.formats.push(GetFormat::Raw);
    assert!(matches!(
        stdout_delivery(&backend, parse(&["get", "metadata", "--format", "raw"])).await,
        Err(GetCommandError::UnsupportedFormat(GetFormat::Raw))
    ));
    assert_eq!(backend.counts(), (1, 0));
}

#[tokio::test]
async fn returned_handles_and_recursive_depth_are_backend_selected_and_bounded() {
    let mut backend = Backend::new(payload(json!({"status": "succeeded"})));
    backend.kind = GetTargetKind::ReturnedHandle;
    backend.wait = true;
    stdout_delivery(&backend, parse(&["get", "returned-handle", "--wait"]))
        .await
        .unwrap();
    assert!(backend.acquired.lock().unwrap()[0].1.wait);

    let mut backend = Backend::new(payload(json!({"children": []})));
    assert!(matches!(
        stdout_delivery(
            &backend,
            parse(&["get", "target", "--recursive", "--depth", "0"])
        )
        .await,
        Err(GetCommandError::RecursiveUnsupported)
    ));
    assert_eq!(backend.counts(), (1, 0));
    backend.recursive = true;
    stdout_delivery(
        &backend,
        parse(&["get", "target", "--recursive", "--depth", "0"]),
    )
    .await
    .unwrap();
    let acquisitions = backend.acquired.lock().unwrap();
    assert!(acquisitions[0].1.recursive);
    assert_eq!(acquisitions[0].1.depth, Some(0));
}

#[tokio::test]
async fn returned_handle_kind_does_not_implicitly_advertise_wait() {
    let mut backend = Backend::new(payload(json!({"status": "running"})));
    backend.kind = GetTargetKind::ReturnedHandle;
    assert!(matches!(
        stdout_delivery(&backend, parse(&["get", "returned-handle", "--wait"])).await,
        Err(GetCommandError::WaitRequiresHandle)
    ));
    assert_eq!(backend.counts(), (1, 0));
}

#[tokio::test]
async fn advertised_read_effect_observer_acquires_once_and_never_promotes_accepted() {
    let receipt = json!({
        "status": "accepted",
        "handle": {"operationId": "exact-native-handle"},
        "coverage": {"complete": false}
    });
    let mut source = payload(receipt.clone());
    source.complete = false;
    let mut backend = Backend::new(source).representation();
    backend.wait = true;
    let delivery = stdout_delivery(&backend, parse(&["get", "item/src/definition", "--wait"]))
        .await
        .unwrap();
    assert_eq!(backend.counts(), (1, 1));
    assert!(backend.acquired.lock().unwrap()[0].1.wait);
    assert!(!delivery.complete());
    assert_eq!(delivery.format(), GetFormat::Json);
    assert_eq!(delivery.data(), &receipt);
    assert_eq!(
        serde_json::from_slice::<Value>(delivery.bytes()).unwrap(),
        receipt
    );
}

#[tokio::test]
async fn default_json_must_be_path_advertised_before_content() {
    let mut backend = Backend::new(payload(json!({}))).representation();
    backend.formats = vec![GetFormat::Raw];
    assert!(matches!(
        stdout_delivery(&backend, parse(&["get", "native"])).await,
        Err(GetCommandError::UnsupportedFormat(GetFormat::Json))
    ));
    assert_eq!(backend.counts(), (1, 0));
}

#[tokio::test]
async fn metadata_cannot_infer_raw_and_implicit_raw_cannot_be_projected() {
    let mut backend = Backend::new(payload(json!({})));
    backend.default_format = GetFormat::Raw;
    backend.formats.push(GetFormat::Raw);
    assert!(matches!(
        stdout_delivery(&backend, parse(&["get", "metadata"])).await,
        Err(GetCommandError::UnsupportedFormat(GetFormat::Raw))
    ));
    assert_eq!(backend.counts(), (1, 0));

    let mut backend = Backend::new(payload(json!({}))).representation();
    backend.default_format = GetFormat::Raw;
    assert!(matches!(
        stdout_delivery(&backend, parse(&["get", "native", "-q", "value"])).await,
        Err(GetCommandError::Arguments(error))
            if error.kind == GetArgumentErrorKind::RawProjection
    ));
    assert_eq!(backend.counts(), (1, 0));
}

#[tokio::test]
async fn missing_native_representation_is_an_error_not_an_inferred_export() {
    for format in ["raw", "markdown"] {
        let backend = Backend::new(payload(json!({}))).representation();
        assert!(matches!(
            stdout_delivery(&backend, parse(&["get", "native", "--format", format])).await,
            Err(GetCommandError::MissingRepresentation(_))
        ));
    }
}

#[tokio::test]
async fn unprojected_bytes_and_final_rendered_bytes_are_both_bounded() {
    let backend = Backend::new(payload(json!({"value": 1, "other": "too large"})));
    let mut stdout = Vec::new();
    assert!(matches!(
        run_stdout(
            &backend,
            parse(&["get", "target", "-q", "value", "--max-bytes", "4"]),
            &mut stdout,
        )
        .await,
        Err(GetCommandError::ByteLimit { limit: 4, .. })
    ));
    assert!(stdout.is_empty());

    let mut source = payload(json!({}));
    source.raw = Some(vec![0; 5]);
    let backend = Backend::new(source).representation();
    assert!(matches!(
        run_stdout(
            &backend,
            parse(&["get", "native", "--format", "raw", "--max-bytes", "4"]),
            &mut stdout,
        )
        .await,
        Err(GetCommandError::ByteLimit {
            limit: 4,
            observed: 5
        })
    ));
    assert!(stdout.is_empty());

    let mut source = payload(json!({}));
    source.markdown = Some("large".to_owned());
    let backend = Backend::new(source).representation();
    assert!(matches!(
        run_stdout(
            &backend,
            parse(&["get", "native", "--format", "markdown", "--max-bytes", "4"]),
            &mut stdout,
        )
        .await,
        Err(GetCommandError::ByteLimit {
            limit: 4,
            observed: 5
        })
    ));
    assert!(stdout.is_empty());

    let backend = Backend::new(payload(json!({"value": 1}))).representation();
    assert!(matches!(
        run_stdout(
            &backend,
            parse(&[
                "get",
                "native",
                "--format",
                "markdown",
                "-q",
                "value",
                "--max-bytes",
                "11",
            ]),
            &mut stdout,
        )
        .await,
        Err(GetCommandError::ByteLimit { limit: 11, .. })
    ));
    assert!(stdout.is_empty());
}

#[tokio::test]
async fn real_typed_query_errors_preserve_their_kind_and_deliver_no_prefix() {
    for (expression, kind) in [
        ("invalid[", fabric_cli_query::QueryErrorKind::Syntax),
        (
            "length(value)",
            fabric_cli_query::QueryErrorKind::InvalidType,
        ),
    ] {
        let backend = Backend::new(payload(json!({"value": 1})));
        let mut stdout = Vec::new();
        assert!(matches!(
            run_stdout(
                &backend,
                parse(&["get", "target", "-q", expression]),
                &mut stdout,
            )
            .await,
            Err(GetCommandError::Projection(error)) if error.kind() == kind
        ));
        assert_eq!(
            backend.counts(),
            if kind == fabric_cli_query::QueryErrorKind::Syntax {
                (0, 0)
            } else {
                (1, 1)
            }
        );
        assert!(stdout.is_empty());
    }
}

#[tokio::test]
async fn projection_never_masks_a_failed_acquisition() {
    let mut backend = Backend::new(payload(json!({})));
    backend.fail_retrieve = true;
    let mut stdout = Vec::new();
    assert!(matches!(
        run_stdout(
            &backend,
            parse(&["get", "target", "-q", "length(value)"]),
            &mut stdout,
        )
        .await,
        Err(GetCommandError::Backend {
            code: "native_unavailable",
            ..
        })
    ));
    assert_eq!(backend.counts(), (1, 1));
    assert!(stdout.is_empty());
}

#[tokio::test]
async fn already_cancelled_or_expired_command_never_calls_the_backend() {
    for interruption in [
        GetInterruption::Cancelled,
        GetInterruption::DeadlineExceeded,
    ] {
        let backend = Backend::new(payload(json!({})));
        let prepared =
            PreparedGet::preflight(parse(&["get", "target"]), Path::new("unused-relative-cwd"))
                .unwrap();
        assert!(matches!(
            prepared.acquire(&backend, ready(interruption)).await,
            Err(GetCommandError::Interrupted(reason)) if reason == interruption
        ));
        assert_eq!(backend.counts(), (0, 0));
    }
}

struct InterruptAfterFirstPoll(bool);

impl Future for InterruptAfterFirstPoll {
    type Output = GetInterruption;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.0 {
            Poll::Ready(GetInterruption::DeadlineExceeded)
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[tokio::test]
async fn whole_deadline_drops_inflight_acquisition_without_local_effects() {
    let fixture = Fixture::new();
    let mut backend = Backend::new(payload(json!({})));
    backend.stall_retrieve = true;
    let prepared = PreparedGet::preflight(
        parse(&["get", "target", "--output", "never-created"]),
        &fixture.root,
    )
    .unwrap();
    assert!(matches!(
        prepared
            .acquire(&backend, InterruptAfterFirstPoll(false))
            .await,
        Err(GetCommandError::Interrupted(
            GetInterruption::DeadlineExceeded
        ))
    ));
    assert_eq!(backend.counts(), (1, 1));
    assert!(backend.retrieve_dropped.load(Ordering::SeqCst));
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[tokio::test]
async fn interruption_is_checked_again_after_complete_bounded_encoding() {
    let backend = Backend::new(payload(json!({"value": 1})));
    let prepared =
        PreparedGet::preflight(parse(&["get", "target"]), Path::new("unused-relative-cwd"))
            .unwrap();
    assert!(matches!(
        prepared
            .acquire(&backend, InterruptAfterFirstPoll(false))
            .await,
        Err(GetCommandError::Interrupted(
            GetInterruption::DeadlineExceeded
        ))
    ));
    assert_eq!(backend.counts(), (1, 1));
}

#[test]
fn local_output_requires_explicit_absolute_cwd_and_existing_parent() {
    let error = PreparedGet::preflight(
        parse(&["get", "target", "--output", "filename"]),
        Path::new("relative"),
    )
    .unwrap_err();
    assert!(matches!(error, GetCommandError::InvalidWorkingDirectory));

    let fixture = Fixture::new();
    let mut command = parse(&["get", "target"]);
    command.output = GetOutput::File(Path::new("missing").join("filename"));
    let error = PreparedGet::preflight(command, &fixture.root).unwrap_err();
    assert!(matches!(
        error,
        GetCommandError::File(error) if error.kind == ExactOutputErrorKind::MissingParent
    ));
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}

#[tokio::test]
async fn racing_file_creation_preserves_the_original_exact_output_error() {
    let fixture = Fixture::new();
    let backend = Backend::new(payload(json!({"value": 1})));
    let prepared = PreparedGet::preflight(
        parse(&["get", "target", "--output", "raced"]),
        &fixture.root,
    )
    .unwrap();
    let delivery = prepared.acquire(&backend, pending()).await.unwrap();
    fs::write(fixture.root.join("raced"), b"competitor").unwrap();
    let GetCommandError::File(error) = delivery.commit_file().unwrap_err() else {
        panic!("expected original exact-output error");
    };
    assert_eq!(error.kind, ExactOutputErrorKind::OutputExists);
    assert_eq!(error.stage, ExactOutputStage::Create);
    assert!(!error.path_created);
    assert_eq!(error.bytes_written, 0);
    assert_eq!(fs::read(fixture.root.join("raced")).unwrap(), b"competitor");
}

struct FaultWriter {
    bytes: Vec<u8>,
    quota: usize,
    fail_flush: bool,
}

impl Write for FaultWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.quota.min(bytes.len());
        if count == 0 {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        self.bytes.extend_from_slice(&bytes[..count]);
        self.quota -= count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn stdout_write_and_flush_errors_retain_original_partial_effects() {
    for (quota, fail_flush, expected_stage, expected_count) in [
        (2, false, GetStdoutStage::Write, 2),
        (4, true, GetStdoutStage::Flush, 4),
    ] {
        let backend = Backend::new(payload(json!(true)));
        let delivery = stdout_delivery(&backend, parse(&["get", "target", "--max-bytes", "4"]))
            .await
            .unwrap();
        let mut writer = FaultWriter {
            bytes: Vec::new(),
            quota,
            fail_flush,
        };
        let GetCommandError::Stdout(error) = delivery.write_to(&mut writer).unwrap_err() else {
            panic!("expected original stdout error");
        };
        assert_eq!(error.stage, expected_stage);
        assert_eq!(error.bytes_written, expected_count);
        assert_eq!(writer.bytes, b"true"[..expected_count]);
        assert_eq!(
            error.io_error.kind(),
            if fail_flush {
                io::ErrorKind::BrokenPipe
            } else {
                io::ErrorKind::StorageFull
            }
        );
    }
}

#[tokio::test]
async fn file_and_stdout_delivery_cannot_be_silently_substituted() {
    let fixture = Fixture::new();
    let backend = Backend::new(payload(json!({})));
    let delivery =
        PreparedGet::preflight(parse(&["get", "target", "--output", "file"]), &fixture.root)
            .unwrap()
            .acquire(&backend, pending())
            .await
            .unwrap();
    let mut stdout = Vec::new();
    assert!(matches!(
        delivery.write_to(&mut stdout),
        Err(GetCommandError::DeliveryMismatch)
    ));
    assert!(stdout.is_empty());
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);

    let backend = Backend::new(payload(json!({})));
    let delivery = stdout_delivery(&backend, parse(&["get", "target"]))
        .await
        .unwrap();
    assert!(matches!(
        delivery.commit_file(),
        Err(GetCommandError::DeliveryMismatch)
    ));
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
}
