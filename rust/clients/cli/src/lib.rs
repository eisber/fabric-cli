// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Thin injectable parser/host/output composition. Only the executable owns runtime and signals.
#![forbid(unsafe_code)]

mod console;
mod dispatch;
mod host;
pub mod lifecycle;
mod parser;

pub use fabric_cli_command_contracts::Host;
pub use fabric_cli_selection::SelectionFile;
pub use host::NativeHost;
pub use parser::parser;

use clap::error::ErrorKind as ParseErrorKind;
use console::{ConsoleProgress, ConsoleUpdate, ReportingState};
use dispatch::execute;
use fabric_cli_command_contracts::{CommandDiagnostics, CommandError};
use fabric_cli_logging as diagnostics;
use fabric_cli_polling::{PROGRESS_INTERVAL, ProgressDisplay, progress_heartbeat};
use fabric_core::{
    CancellationToken, OperationContext,
    diagnostics::{
        DiagnosticDuration, DiagnosticEvent, DiagnosticOutcome, DiagnosticPhase, DiagnosticStage,
    },
};
use parser::{
    command_name, parse_arguments, parse_failure_message, parsed_command_name, welcome_requested,
};
use std::{
    ffi::OsString,
    io::Write,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

fn timestamp(host: &dyn Host) -> String {
    let date: chrono::DateTime<chrono::Utc> = host.clock().wall_now().into();
    let precision = if date.timestamp_subsec_micros() == 0 {
        chrono::SecondsFormat::Secs
    } else {
        chrono::SecondsFormat::Micros
    };
    date.to_rfc3339_opts(precision, true)
}

fn write_failure(
    host: &dyn Host,
    command: &str,
    error: CommandError,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let code = error.exit_code();
    let output = fabric_cli_output::failure(&timestamp(host), command, &error);
    if stdout.write_all(output.as_bytes()).is_err() {
        let _ = stderr.write_all(b"Cannot write JSON output\n");
        return 1;
    }
    code
}

fn welcome(stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
    let output = "Welcome to fab-rs.\n\n\n\
                  Get started:\n  fab-rs api workspaces   List workspaces\n  fab-rs auth             Authentication commands\n  fab-rs --help           All commands\n";
    if stdout.write_all(output.as_bytes()).is_err() {
        let _ = stderr.write_all(b"Cannot write welcome output\n");
        return 1;
    }
    0
}

/// Run the real parser/host/output with injected effects and streams.
///
/// Supply the executable name as the first argument, as with `std::env::args_os`.
/// Metadata returns before touching any host hook, selection, credential, or network.
/// No arguments shows a static welcome screen without touching host hooks.
pub async fn run(
    args: impl IntoIterator<Item = OsString>,
    host: &dyn Host,
    cancellation: CancellationToken,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let args: Vec<OsString> = args.into_iter().collect();
    if welcome_requested(&args) {
        return welcome(stdout, stderr);
    }
    let name = command_name(&args);
    let invocation = match parse_arguments(&args) {
        Ok(invocation) => invocation,
        Err(error)
            if matches!(
                error.kind(),
                ParseErrorKind::DisplayHelp | ParseErrorKind::DisplayVersion
            ) =>
        {
            return if stdout.write_all(error.to_string().as_bytes()).is_ok() {
                0
            } else {
                1
            };
        }
        Err(error) => {
            // Clap diagnostics can echo raw input containing header credentials. Only
            // argument definitions from the parser (never user-supplied values) are copied.
            let message = parse_failure_message(name, &error);
            return write_failure(
                host,
                name,
                CommandError::new("InvalidInput", message.as_str()),
                stdout,
                stderr,
            );
        }
    };
    let matches = invocation.matches();
    let name = parsed_command_name(matches);
    let (sender, mut events) = mpsc::channel(1);
    let observer = Arc::new(ConsoleProgress {
        events: sender,
        failed: CancellationToken::new(),
        broken: AtomicBool::new(false),
        timing: matches.get_flag("verbose").then(|| {
            let clock = host.clock();
            let start = clock.now();
            (clock, start)
        }),
        cancellation: cancellation.clone(),
        heartbeat_clock: OnceLock::new(),
        reporting: Mutex::new(ReportingState {
            outcome: DiagnosticOutcome::BeforeSend,
            recovery: None,
            completed: false,
        }),
    });
    let mut result = {
        let execution = async {
            observer
                .stage(
                    DiagnosticStage::Command,
                    DiagnosticPhase::Started,
                    Duration::ZERO,
                )
                .await?;
            execute(&invocation, host, cancellation, &observer).await
        };
        let interval = PROGRESS_INTERVAL;
        let heartbeat_context = OperationContext::with_deadline(None, CancellationToken::new());
        let mut heartbeat = progress_heartbeat(
            observer.heartbeat_clock.get().map(Arc::as_ref),
            interval,
            &heartbeat_context,
        );
        let mut display = ProgressDisplay::default();
        tokio::pin!(execution);
        loop {
            tokio::select! {
                biased;
                result = &mut execution => {
                    break result;
                }
                Some(event) = events.recv() => {
                    let result = match event.progress {
                        ConsoleUpdate::Progress(update) => {
                            let result = display.update(update, host.progress_color(), stderr);
                            heartbeat = progress_heartbeat(
                                observer.heartbeat_clock.get().map(Arc::as_ref),
                                interval,
                                &heartbeat_context,
                            );
                            result
                        }
                        ConsoleUpdate::Diagnostic(event, timestamp) => {
                            observer.remember(&event, false);
                            let result = diagnostics::write(&event, timestamp, host.progress_color(), stderr);
                            if result.is_ok() {
                                observer.remember(&event, true);
                            }
                            result
                        }
                    };
                    if result.is_err() && observer.timing.is_some() {
                        observer.broken.store(true, Ordering::Relaxed);
                    }
                    // Cancellation can drop the observer while its event is queued.
                    let _ = event.written.send(result);
                }
                result = &mut heartbeat, if display.needs_heartbeat() && !observer.failed.is_cancelled() && !observer.broken.load(Ordering::Relaxed) => {
                    if let Err(error) = result {
                        break Err(error.into());
                    }
                    if display.heartbeat(host.progress_color(), stderr).is_err() {
                        if observer.timing.is_some() {
                            observer.broken.store(true, Ordering::Relaxed);
                        }
                        observer.failed.cancel();
                    }
                    heartbeat = progress_heartbeat(
                        observer.heartbeat_clock.get().map(Arc::as_ref),
                        interval,
                        &heartbeat_context,
                    );
                }
            }
        }
    };
    if let Some((clock, _)) = &observer.timing
        && !observer.broken.load(Ordering::Relaxed)
    {
        let phase = match &result {
            Ok(_) => {
                observer
                    .reporting
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .completed = true;
                DiagnosticPhase::Completed
            }
            Err(error) => DiagnosticPhase::Failed(error.kind()),
        };
        let elapsed = observer.elapsed();
        let event = DiagnosticEvent::Stage {
            stage: DiagnosticStage::Command,
            phase,
            duration: DiagnosticDuration::from_duration(elapsed),
        };
        if diagnostics::write(&event, clock.wall_now(), host.progress_color(), stderr).is_err() {
            observer.broken.store(true, Ordering::Relaxed);
            let mut error = observer.reporting_error();
            if let Err(original) = &result {
                if error.recovery.is_none() {
                    error.recovery = original.recovery.clone();
                }
                error.message.push_str(&format!(
                    " Command failed ({}).",
                    diagnostics::category(original.kind())
                ));
            }
            result = Err(error);
        }
    }
    for diagnostic in host.take_diagnostics() {
        if !observer.broken.load(Ordering::Relaxed) {
            let _ = writeln!(stderr, "{diagnostic}");
        }
    }
    match result {
        Ok(data) => {
            let output = fabric_cli_output::success(&timestamp(host), name, data);
            if stdout.write_all(output.as_bytes()).is_ok() {
                0
            } else {
                if !observer.broken.load(Ordering::Relaxed) {
                    let _ = stderr.write_all(b"Cannot write JSON output\n");
                }
                1
            }
        }
        Err(error) => {
            if observer.broken.load(Ordering::Relaxed) {
                write_failure(host, name, error, stdout, &mut std::io::sink())
            } else {
                write_failure(host, name, error, stdout, stderr)
            }
        }
    }
}
