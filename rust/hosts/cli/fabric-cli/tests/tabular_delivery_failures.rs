// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Production export path spelling and receipt-failure evidence contracts.

#![deny(unnameable_test_items)]

use fabric_cli::{
    parser::{Invocation, parse_args},
    tabular_export::{self, ExportDelivery, FileSink, ReceiptWriteFailure},
};
use fabric_cli_output::{OutputStream, RenderPlan, text::TextPlan};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "tabular-delivery-{}-{}",
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

fn terminal_spellings() -> Vec<&'static str> {
    #[cfg(windows)]
    {
        vec!["/", "/.", "\\", "\\."]
    }
    #[cfg(not(windows))]
    {
        vec!["/", "/."]
    }
}

fn local_delivery(root: &Path) -> (ExportDelivery, PathBuf) {
    let store = fabric_cli_local::ConfigStore::new(root.join("isolated-config"));
    let mut settings = store.initialize().unwrap();
    settings.insert(
        "default_capacity".into(),
        json!("private-table-cell-must-not-leak"),
    );
    store.write(&settings).unwrap();
    let file = root.join("config.json");
    let Invocation::Tabular(export) = parse_args([
        "fab-rs",
        "config",
        "ls",
        "--format",
        "json",
        "--output",
        file.to_str().unwrap(),
    ])
    .unwrap() else {
        panic!("expected explicit export")
    };
    let delivery =
        tabular_export::dispatch_local(&export, &store, &mut FileSink::default()).unwrap();
    assert_eq!(delivery.exit_code, 0);
    assert!(file.is_file());
    (delivery, file)
}

#[test]
fn directory_only_terminal_spelling_rejects_all_formats_before_execution() {
    let root = Scratch::new();
    for terminal in terminal_spellings() {
        for format in ["json", "csv", "parquet"] {
            let file = root.0.join(format!("not-a-directory.{format}"));
            let spelling = format!("{}{terminal}", file.to_str().unwrap());
            let Invocation::Tabular(export) = parse_args([
                "fab-rs", "config", "ls", "--format", format, "--output", &spelling,
            ])
            .unwrap() else {
                panic!("expected explicit export")
            };
            let delivery = tabular_export::dispatch(&export, &mut FileSink::default(), |_| async {
                panic!("directory-only spelling must fail before backend effects")
            })
            .now_or_never()
            .unwrap();
            assert_eq!(delivery.receipt["execution"]["status"], "not_started");
            assert_ne!(delivery.exit_code, 0);
            assert!(!file.exists());
        }
    }
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
}

#[test]
fn binary_rejects_directory_spelling_before_config_execution_or_file_creation() {
    let root = Scratch::new();
    for terminal in terminal_spellings() {
        let file = root.0.join("not-a-directory.json");
        let config = root.0.join("must-not-initialize");
        let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
            .args(["config", "ls", "--format", "json", "--output"])
            .arg(format!("{}{terminal}", file.to_str().unwrap()))
            .env("FAB_RUST_CONFIG_ROOT", &config)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["execution"]["status"],
            "not_started"
        );
        assert!(!config.exists());
        assert!(!file.exists());
    }
}

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed receipt stream"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn production_receipt_writer_carries_typed_evidence_through_io_error() {
    let root = Scratch::new();
    let (delivery, file) = local_delivery(&root.0);
    let original = fs::read(&file).unwrap();
    let mut stderr = Vec::new();
    let error = delivery
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
    assert_eq!(
        retained.delivery.receipt["delivery"]["file"],
        file.to_str().unwrap()
    );
    let diagnostic: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(diagnostic["error"]["code"], "export_receipt_write_failed");
    assert_eq!(
        diagnostic["retained_export"]["execution"]["status"],
        "succeeded"
    );
    assert_eq!(
        diagnostic["retained_export"]["delivery"]["status"],
        "succeeded"
    );
    assert_eq!(
        diagnostic["retained_export"]["delivery"]["rows"],
        fabric_cli_local::SETTINGS.len()
    );
    assert_eq!(
        diagnostic["retained_export"]["delivery"]["schema"][0]["name"],
        "setting"
    );
    assert!(!String::from_utf8_lossy(&stderr).contains("private-table-cell"));
    assert!(!String::from_utf8_lossy(&stderr).contains("cache_enabled"));
    assert_eq!(fs::read(file).unwrap(), original);
}

#[test]
fn native_prefix_failure_preserves_the_published_export_receipt() {
    let root = Scratch::new();
    let (delivery, file) = local_delivery(&root.0);
    let prefix = TextPlan {
        records: vec![RenderPlan {
            stream: OutputStream::Stdout,
            exit_code: 0,
            payload: b"prefix".to_vec(),
        }],
        exit_code: 0,
    };
    let mut stderr = Vec::new();
    let failure = delivery
        .write_with_prefix(&prefix, &mut BrokenWriter, &mut stderr)
        .unwrap_err();
    assert_eq!(failure.delivery.receipt["execution"]["status"], "succeeded");
    assert_eq!(failure.delivery.receipt["delivery"]["status"], "succeeded");
    assert!(file.is_file());
    let diagnostic: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(
        diagnostic["retained_export"]["delivery"]["status"],
        "succeeded"
    );
}

#[test]
fn recovery_diagnostic_is_bounded_and_excludes_operation_metadata_and_table_data() {
    let root = Scratch::new();
    let (mut delivery, _) = local_delivery(&root.0);
    delivery.receipt["execution"]["metadata"] = json!({"private":"never-emit-operation-data"});
    delivery.receipt["delivery"]["file"] = json!("\u{0001}".repeat(50_000));
    delivery.receipt["delivery"]["schema"] = json!(
        (0..1024)
            .map(|_| json!({
                "name":"\u{0001}".repeat(2048), "type":"json", "nullable":true,
            }))
            .collect::<Vec<_>>()
    );
    let mut stderr = Vec::new();
    let failure = delivery
        .write_to_streams(&mut BrokenWriter, &mut stderr)
        .unwrap_err();
    assert!(stderr.len() <= 16 * 1024);
    let diagnostic: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(
        diagnostic["retained_export"]["delivery"]["file_truncated"],
        true
    );
    assert_eq!(
        diagnostic["retained_export"]["delivery"]["schema_truncated"],
        true
    );
    assert_eq!(
        diagnostic["retained_export"]["delivery"]["schema_columns"],
        1024
    );
    assert!(!String::from_utf8_lossy(&stderr).contains("never-emit-operation-data"));
    assert_eq!(
        failure.delivery.receipt["delivery"]["schema"]
            .as_array()
            .unwrap()
            .len(),
        1024
    );
}

#[test]
fn actual_binary_closed_stdout_pipe_retains_successful_file_evidence() {
    use std::{
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    let root = Scratch::new();
    let file = root.0.join("published.json");
    let (reader, writer) = io::pipe().unwrap();
    // Close before spawning to remove the scheduling race from closing the
    // parent's read end immediately afterward; the child sees the same failure.
    drop(reader);
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command
        .args(["config", "ls", "--format", "json", "--output"])
        .arg(&file)
        .env("FAB_RUST_CONFIG_ROOT", root.0.join("isolated-config"))
        .env("FAB_RUST_SESSION_ID", "tabular-export-broken-receipt")
        .stdout(writer)
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    drop(command);
    let stderr = child.stderr.take().unwrap();
    let capture = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.take(64 * 1024).read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait().unwrap();
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stderr = capture.join().unwrap();
    assert!(!timed_out, "local export child exceeded its deadline");
    assert!(!status.success());
    assert!(stderr.len() <= 16 * 1024);
    let diagnostic: Value = serde_json::from_slice(&stderr).unwrap_or_else(|_| {
        panic!(
            "missing retained JSON evidence: {}",
            String::from_utf8_lossy(&stderr)
        )
    });
    assert_eq!(diagnostic["error"]["code"], "export_receipt_write_failed");
    let retained = &diagnostic["retained_export"];
    assert_eq!(retained["execution"]["status"], "succeeded");
    assert_eq!(retained["delivery"]["status"], "succeeded");
    assert_eq!(retained["delivery"]["file"], file.to_str().unwrap());
    assert_eq!(retained["delivery"]["schema"][0]["name"], "setting");
    assert_eq!(retained["delivery"]["schema"][1]["name"], "value");
    let data: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(
        data.as_array().unwrap().len(),
        fabric_cli_local::SETTINGS.len()
    );
    assert!(!String::from_utf8_lossy(&stderr).contains("cache_enabled"));
}

#[cfg(target_os = "linux")]
#[test]
fn actual_binary_full_stdout_retains_successful_file_evidence_on_stderr() {
    use std::process::Stdio;
    let root = Scratch::new();
    let file = root.0.join("published.json");
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["config", "ls", "--format", "json", "--output"])
        .arg(&file)
        .env("FAB_RUST_CONFIG_ROOT", root.0.join("isolated-config"))
        .stdout(Stdio::from(full))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["error"]["code"], "export_receipt_write_failed");
    assert_eq!(
        diagnostic["retained_export"]["execution"]["status"],
        "succeeded"
    );
    assert_eq!(
        diagnostic["retained_export"]["delivery"]["status"],
        "succeeded"
    );
    assert_eq!(
        diagnostic["retained_export"]["delivery"]["file"],
        file.to_str().unwrap()
    );
    assert_eq!(
        diagnostic["retained_export"]["delivery"]["schema"][1]["name"],
        "value"
    );
    let data: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(
        data.as_array().unwrap().len(),
        fabric_cli_local::SETTINGS.len()
    );
}
