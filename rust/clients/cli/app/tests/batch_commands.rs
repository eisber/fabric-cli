// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pinned source loop evidence plus isolated executable state/output contracts.
#![deny(unnameable_test_items)]

use fabric_cli::parser::batch::{
    self, BatchHost, BatchInvocation, CommandOutcome, CommandParseError,
};
use serde_json::{Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct SyntheticHost {
    events: Vec<Value>,
    remembered: Option<String>,
}

impl BatchHost for SyntheticHost {
    type Error = io::Error;

    fn execute(&mut self, arguments: &[&str]) -> io::Result<CommandOutcome> {
        match batch::parse_command(arguments) {
            Err(CommandParseError::UnknownCommand) => {
                self.events.push(json!(["error", "UnexpectedError"]));
                return Ok(CommandOutcome::Stop(1));
            }
            Err(CommandParseError::Arguments(_)) => return Ok(CommandOutcome::Stop(2)),
            Ok((_, true)) => return Ok(CommandOutcome::Stop(0)),
            Ok(_) => {}
        }
        self.events.push(json!(["execute", arguments]));
        let value = if arguments.starts_with(&["config", "set"]) {
            arguments.get(3).copied()
        } else {
            None
        };
        let exit = match value {
            Some("cancel") => {
                self.events
                    .push(json!(["error", "LongRunningOperationCancelled"]));
                return Ok(CommandOutcome::Stop(2));
            }
            Some("raise") => {
                self.events.push(json!(["error", "UnexpectedError"]));
                return Ok(CommandOutcome::Stop(1));
            }
            Some("remember") => {
                self.remembered = Some("remember".into());
                0
            }
            Some("return-1") => 1,
            Some("return-4") => 4,
            _ => 0,
        };
        if arguments == ["config", "get", "key"] {
            self.events.push(json!(["state", self.remembered]));
        }
        Ok(CommandOutcome::Returned(exit))
    }

    fn separator(&mut self) -> io::Result<()> {
        self.events.push(json!(["separator"]));
        Ok(())
    }

    fn summary(&mut self, invocation: &BatchInvocation) -> io::Result<()> {
        self.events.push(json!(["blank"]));
        self.events.push(json!([
            "summary",
            invocation.commands.len(),
            invocation.output_format
        ]));
        Ok(())
    }
}

#[test]
fn original_python_main_observations_match_order_errors_cancellation_and_state() {
    let evidence: Value = serde_json::from_str(include_str!("batch_source.json")).unwrap();
    assert_eq!(
        evidence["python_pin"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    for case in evidence["cases"].as_array().unwrap() {
        let invocation = BatchInvocation {
            commands: case["commands"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect(),
            output_format: case["output_format"].as_str().map(str::to_owned),
        };
        let mut host = SyntheticHost::default();
        let exit = batch::run(&invocation, &mut host).unwrap();
        assert_eq!(json!(exit), case["exit"], "{}", case["id"]);
        assert_eq!(json!(host.events), case["events"], "{}", case["id"]);
    }
}

struct BrokenWriter {
    executions: usize,
    fail_summary: bool,
}

impl BatchHost for BrokenWriter {
    type Error = io::Error;

    fn execute(&mut self, _: &[&str]) -> io::Result<CommandOutcome> {
        self.executions += 1;
        Ok(CommandOutcome::Returned(0))
    }

    fn separator(&mut self) -> io::Result<()> {
        if self.fail_summary {
            Ok(())
        } else {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    fn summary(&mut self, _: &BatchInvocation) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

#[test]
fn failed_output_never_replays_commands_or_reports_success() {
    let invocation = BatchInvocation {
        commands: vec!["version".into(), "version".into()],
        output_format: None,
    };
    for (fail_summary, count) in [(false, 1), (true, 2)] {
        let mut host = BrokenWriter {
            executions: 0,
            fail_summary,
        };
        assert_eq!(
            batch::run(&invocation, &mut host).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(host.executions, count);
    }
}

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "fabric-batch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn invoke(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("FAB_RUST_CONFIG_ROOT", root)
        .env("FAB_RUST_SESSION_ID", "37")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn success(output: &Output) {
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn executable_reuses_local_handlers_and_observes_settings_between_commands() {
    let root = Root::new();
    let output = invoke(
        &root.0,
        &[
            "-c",
            "config set show_hidden true",
            "-c",
            "config get show_hidden",
            "-c",
            "config set show_hidden false",
            "-c",
            "config get show_hidden",
        ],
    );
    success(&output);
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    let first = stdout.find("true\n").unwrap();
    let last = stdout.rfind("false\n").unwrap();
    assert!(first < last);
    assert!(
        stdout.ends_with("\n\n\n* 4 commands executed.\n\n"),
        "{stdout:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("------------------------------\n")
            .count(),
        3
    );
    let settings: Value =
        serde_json::from_slice(&fs::read(root.0.join("config.json")).unwrap()).unwrap();
    assert_eq!(settings["show_hidden"], "false");
}

#[test]
fn executable_last_handled_exit_wins_but_parse_and_unknown_failures_stop() {
    let root = Root::new();
    let handled = invoke(&root.0, &["-c", "config get missing", "-c", "version"]);
    success(&handled);
    assert!(String::from_utf8_lossy(&handled.stdout).contains("2 commands executed."));
    let failure_last = invoke(&root.0, &["-c", "version", "-c", "config get missing"]);
    assert_eq!(failure_last.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failure_last.stdout).contains("2 commands executed."));
    for (entry, exit) in [
        ("get", 2),
        ("private-batch-marker", 1),
        ("ls --token=private-batch-marker", 2),
    ] {
        let output = invoke(
            &root.0,
            &[
                "-c",
                "version",
                "-c",
                entry,
                "-c",
                "config set show_hidden true",
            ],
        );
        assert_eq!(output.status.code(), Some(exit));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("------------------------------\n"));
        assert!(!stderr.contains("private-batch-marker"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("commands executed."));
        let settings: Value =
            serde_json::from_slice(&fs::read(root.0.join("config.json")).unwrap()).unwrap();
        assert_eq!(settings["show_hidden"], "false");
    }
}

#[test]
fn executable_help_stops_but_bare_groups_and_empty_entries_keep_source_counting() {
    let root = Root::new();
    let help = invoke(
        &root.0,
        &[
            "-c",
            "version",
            "-c",
            "ls -help",
            "-c",
            "config set show_hidden true",
        ],
    );
    success(&help);
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage:"));
    assert!(!String::from_utf8_lossy(&help.stdout).contains("commands executed."));
    assert!(!root.0.join("config.json").exists());
    let group = invoke(&root.0, &["-c", "config", "-c", "version", "-c", ""]);
    success(&group);
    assert!(String::from_utf8_lossy(&group.stdout).contains("3 commands executed."));
    assert_eq!(
        group.stderr,
        b"------------------------------\n------------------------------\n"
    );
    let empty = invoke(&root.0, &["-c", "", "-c", " \t\r\n"]);
    success(&empty);
    assert!(empty.stdout.is_empty());
    assert!(empty.stderr.is_empty());
}

#[test]
fn executable_parser_help_variants_stop_before_the_next_state_change() {
    for help in ["ls --he", "ls --hel", "ls -lh", "ls -lah"] {
        let root = Root::new();
        let output = invoke(
            &root.0,
            &[
                "-c",
                "version",
                "-c",
                help,
                "-c",
                "config set show_hidden true",
            ],
        );
        success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("commands executed."));
        assert!(!root.0.join("config.json").exists(), "{help}");
    }
}

#[test]
fn executable_outer_format_is_summary_only_and_preserves_list_valued_command() {
    let root = Root::new();
    let output = invoke(
        &root.0,
        &[
            "--output_format",
            "json",
            "-c",
            "config get output_format",
            "-c",
            "version",
            "-c",
            "",
        ],
    );
    success(&output);
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    assert!(stdout.starts_with("text\n"), "{stdout:?}");
    let summary: Value = serde_json::from_str(&stdout[stdout.find("{\n").unwrap()..]).unwrap();
    assert_eq!(summary["status"], "Success");
    assert_eq!(
        summary["command"],
        json!(["config get output_format", "version", ""])
    );
    assert_eq!(
        summary["result"],
        json!({"message": "3 commands executed."})
    );
    assert_eq!(summary["timestamp"].as_str().unwrap().len(), 27);
    assert_eq!(
        output.stderr,
        b"------------------------------\n------------------------------\n"
    );
}

#[test]
fn executable_summary_reads_configuration_changed_by_the_batch() {
    let root = Root::new();
    let output = invoke(
        &root.0,
        &["-c", "config set output_format json", "-c", "version"],
    );
    success(&output);
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    let summary: Value = serde_json::from_str(&stdout[stdout.find("{\n").unwrap()..]).unwrap();
    assert_eq!(summary["result"]["message"], "2 commands executed.");
}

#[test]
fn executable_navigation_restores_once_and_preserves_its_in_process_state() {
    let root = Root::new();
    success(&invoke(
        &root.0,
        &["config", "set", "context_persistence_enabled", "true"],
    ));
    fs::write(root.0.join("context-37.json"), br#"{"path":"/"}"#).unwrap();
    let output = invoke(&root.0, &["-c", "pwd", "-c", "cd /", "-c", "pwd"]);
    success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("Command context path: root")
            .count(),
        1
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("\"local_path\"")
            .count(),
        2
    );
}

#[test]
#[cfg(feature = "native-read")]
fn native_batch_missing_selection_is_handled_and_last_returned_exit_wins() {
    let root = Root::new();
    let control = Root::new();
    let version = invoke(&control.0, &["version"]);
    success(&version);
    assert!(version.stderr.is_empty());
    let configured = invoke(&control.0, &["config", "set", "show_hidden", "true"]);
    success(&configured);
    let missing_helper = cfg!(all(target_os = "macos", feature = "interactive"));
    let message = if missing_helper {
        "Device-code login requires explicit tenant and client GUIDs; account is an optional UPN hint."
            .to_owned()
    } else if cfg!(windows) {
        "No Windows broker account is selected. Run fab-rs auth login, or set FAB_RUST_TENANT, FAB_RUST_ACCOUNT and FAB_RUST_CLIENT_ID explicitly."
            .to_owned()
    } else {
        fabric_cli::HostError::NotConfigured.to_string()
    };
    let error = json!({"error": {
        "code": if missing_helper { "invalid_configuration" } else { "NotConfigured" },
        "message": message
    }});
    let output = invoke(
        &root.0,
        &[
            "-c",
            "version",
            "-c",
            "cd Demo.Workspace",
            "-c",
            "config set show_hidden true",
        ],
    );
    if missing_helper {
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, version.stdout);
        assert_eq!(
            output.stderr,
            format!("------------------------------\n{error}\n").as_bytes()
        );
        assert!(!root.0.join("config.json").exists());
    } else {
        success(&output);
        assert_eq!(
            output.stdout,
            [
                version.stdout.as_slice(),
                configured.stdout.as_slice(),
                b"\n\n\n* 3 commands executed.\n\n",
            ]
            .concat()
        );
        assert_eq!(
            output.stderr,
            [
                format!(
                    "------------------------------\n{error}\n------------------------------\n"
                )
                .as_bytes(),
                configured.stderr.as_slice(),
            ]
            .concat()
        );
        assert_eq!(
            fs::read(root.0.join("config.json")).unwrap(),
            fs::read(control.0.join("config.json")).unwrap()
        );
    }
    assert!(!root.0.join("broker-session.json").exists());
    assert!(!root.0.join("context-37.json").exists());

    let failure_last = invoke(&root.0, &["-c", "version", "-c", "cd Demo.Workspace"]);
    assert_eq!(failure_last.status.code(), Some(1));
    assert_eq!(
        failure_last.stdout,
        if missing_helper {
            version.stdout.clone()
        } else {
            [
                version.stdout.as_slice(),
                b"\n\n\n* 2 commands executed.\n\n",
            ]
            .concat()
        }
    );
    assert_eq!(
        failure_last.stderr,
        format!("------------------------------\n{error}\n").as_bytes()
    );
    if missing_helper {
        assert!(!root.0.join("config.json").exists());
    } else {
        assert_eq!(
            fs::read(root.0.join("config.json")).unwrap(),
            fs::read(control.0.join("config.json")).unwrap()
        );
    }
}

#[test]
#[cfg(feature = "windows-broker")]
fn batch_auth_malformed_selection_is_not_cancellation_and_never_opens_a_provider() {
    let control = Root::new();
    let configured = invoke(&control.0, &["config", "set", "show_hidden", "true"]);
    success(&configured);
    let (code, message, exit) = if cfg!(windows) {
        (
            "invalid_configuration",
            "an explicit account, tenant GUID and client GUID are required",
            2,
        )
    } else if cfg!(feature = "interactive") {
        (
            "invalid_configuration",
            "Device-code login requires explicit tenant and client GUIDs; account is an optional UPN hint.",
            1,
        )
    } else {
        (
            "unsupported",
            "Device-code authentication requires an interactive feature build; no provider fallback was attempted.",
            1,
        )
    };
    let error = json!({"error": {"code": code, "message": message}});
    let rejected = "auth status --tenant tenant --account synthetic-account --output_format json";
    for rejected_first in [true, false] {
        let root = Root::new();
        let commands = if rejected_first {
            [rejected, "config set show_hidden true"]
        } else {
            ["config set show_hidden true", rejected]
        };
        let output = invoke(&root.0, &["-c", commands[0], "-c", commands[1]]);
        assert_eq!(
            output.status.code(),
            Some(if rejected_first { 0 } else { exit })
        );
        assert_eq!(
            output.stdout,
            [
                configured.stdout.as_slice(),
                b"\n\n\n* 2 commands executed.\n\n"
            ]
            .concat()
        );
        let diagnostic = format!("{error}\n");
        let separator = b"------------------------------\n";
        assert_eq!(
            output.stderr,
            if rejected_first {
                [
                    diagnostic.as_bytes(),
                    separator,
                    configured.stderr.as_slice(),
                ]
                .concat()
            } else {
                [
                    configured.stderr.as_slice(),
                    separator,
                    diagnostic.as_bytes(),
                ]
                .concat()
            }
        );
        assert_eq!(
            fs::read(root.0.join("config.json")).unwrap(),
            fs::read(control.0.join("config.json")).unwrap()
        );
        assert!(!root.0.join("broker-session.json").exists());
        assert!(!root.0.join(".broker-session.lock").exists());
        assert!(!root.0.join("context-37.json").exists());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-account"));
    }
}

#[cfg(feature = "windows-broker")]
struct AuthFailureHost<'a> {
    root: &'a Path,
    code: &'static str,
    format: fabric_cli_output::text::OutputFormat,
    executions: usize,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[cfg(feature = "windows-broker")]
impl BatchHost for AuthFailureHost<'_> {
    type Error = io::Error;

    fn execute(&mut self, arguments: &[&str]) -> io::Result<CommandOutcome> {
        self.executions += 1;
        if arguments == ["auth", "status"] {
            // Inject the provider result, not a malformed selector or a live sign-in.
            fabric_cli::auth_commands::write_outcome_with_format(
                Err((self.code, "Synthetic broker failure".into())),
                self.format,
                &mut self.stdout,
                &mut self.stderr,
            )
        } else {
            fs::write(self.root.join("config.json"), b"must not execute")?;
            Ok(CommandOutcome::Returned(0))
        }
    }

    fn separator(&mut self) -> io::Result<()> {
        self.stderr.extend_from_slice(b"unexpected separator\n");
        Ok(())
    }

    fn summary(&mut self, _: &BatchInvocation) -> io::Result<()> {
        self.stdout.extend_from_slice(b"unexpected summary\n");
        Ok(())
    }
}

#[test]
#[cfg(feature = "windows-broker")]
fn actual_auth_terminal_outcomes_stop_without_replay_later_mutation_or_success_summary() {
    use fabric_cli_output::text::OutputFormat;
    for format in [OutputFormat::Text, OutputFormat::Json] {
        for code in [
            "cancelled",
            "timed_out",
            "signal_failed",
            "cleanup_pending",
            "completion_unconfirmed",
        ] {
            let root = Root::new();
            let mut host = AuthFailureHost {
                root: &root.0,
                code,
                format,
                executions: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
            let invocation = BatchInvocation {
                commands: vec!["auth status".into(), "config set show_hidden true".into()],
                output_format: None,
            };
            assert_eq!(batch::run(&invocation, &mut host).unwrap(), 1);
            assert_eq!(host.executions, 1);
            assert!(host.stdout.is_empty());
            let mut expected_stderr = Vec::new();
            fabric_cli::auth_commands::write_error_with_format(
                code,
                "Synthetic broker failure",
                1,
                format,
                &mut expected_stderr,
            )
            .unwrap();
            assert_eq!(host.stderr, expected_stderr);
            assert!(!root.0.join("config.json").exists());
            assert!(!root.0.join("broker-session.json").exists());
        }
    }
}
