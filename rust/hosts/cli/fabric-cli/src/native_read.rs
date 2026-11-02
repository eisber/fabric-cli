// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit opt-in binary composition. Libraries never read these host inputs.

use crate::repl_state::{CommandOutcome, ReplState};
use fabric_cli::{
    HostError,
    parser::{Invocation, JobMutationCommand, JobReadCommand, JobRunCommand, ReadCommand},
};
use fabric_sdk::{EndpointBinding, ServiceOrigin};
use serde_json::json;
use std::io::{self, Write};

type Failure = (&'static str, String, u8);

struct NativeOutput {
    plan: fabric_cli_output::text::TextPlan,
    outcome: CommandOutcome,
}

#[cfg(windows)]
impl NativeOutput {
    fn read(output: fabric_cli::session::ReadDispatch) -> Self {
        let outcome = output.error.as_ref().map_or(
            CommandOutcome::from_code(output.output_error_code.as_deref(), output.plan.exit_code),
            |error| crate::repl_state::error_outcome(error, output.plan.exit_code),
        );
        Self {
            plan: output.plan,
            outcome,
        }
    }

    fn job(output: fabric_cli::jobs::JobDispatch) -> Self {
        let outcome = output.error.as_ref().map_or(
            CommandOutcome::Returned(output.plan.exit_code),
            |error| {
                let envelope = error.envelope(
                    fabric_cli_output::Timestamp::parse("2000-01-01T00:00:00Z")
                        .expect("constant timestamp"),
                    None,
                );
                CommandOutcome::from_code(envelope.result().error_code(), output.plan.exit_code)
            },
        );
        Self {
            plan: output.plan,
            outcome,
        }
    }
}

#[cfg(windows)]
#[path = "native_settings.rs"]
mod settings;

#[cfg(windows)]
#[path = "native_transfer_paths.rs"]
mod transfer_paths;

#[cfg(windows)]
#[path = "native_deploy_inputs.rs"]
mod deploy_inputs;

#[cfg(windows)]
#[path = "native_terminal_ui.rs"]
mod terminal_ui;

#[cfg(windows)]
use terminal_ui::NativeMutationUi;

#[derive(Clone, Copy)]
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "The selected host is explicitly unsupported off Windows."
    )
)]
enum NativeCommand<'a> {
    Read(&'a ReadCommand),
    Job(&'a JobReadCommand),
    JobExecution(&'a JobRunCommand),
    JobMutation(&'a JobMutationCommand),
    Family(&'a Invocation),
}

fn has_bound_context(command: NativeCommand<'_>) -> bool {
    match command {
        NativeCommand::Family(Invocation::Deploy(_)) => true,
        NativeCommand::Family(Invocation::Transfer(transfer)) => {
            transfer.kind != fabric_cli_transfer::TransferKind::Deploy
        }
        NativeCommand::Family(_) => false,
        _ => true,
    }
}

fn failure(code: &'static str, message: impl Into<String>, exit: u8) -> Failure {
    (code, message.into(), exit)
}

#[cfg(windows)]
fn selected_read_options(command: &ReadCommand) -> Result<(), Failure> {
    fabric_cli::session::get_options::preflight_with_files(command).map_err(|_| {
        failure(
            "NotSupported",
            "This GET option combination is not supported by the native host.",
            1,
        )
    })
}

#[cfg(windows)]
fn consumes_definition_files(command: &Invocation) -> bool {
    matches!(command, Invocation::Deploy(_) | Invocation::Transfer(_))
}

#[cfg(any(windows, test))]
fn interrupted_failure(code: &'static str, message: &str, exit: u8) -> Failure {
    failure(
        code,
        format!(
            "{message} Local waiting has stopped; in-flight remote or filesystem work may still complete. No rollback is implied."
        ),
        exit,
    )
}

#[cfg(any(windows, test))]
fn finish_runtime<T>(runtime: tokio::runtime::Runtime, result: T) -> T {
    // Runtime Drop waits indefinitely for blocking syscalls; a bounded wait does
    // not cancel those calls or erase their possible filesystem effects.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}

#[cfg(any(windows, test))]
async fn wait_for_deadline(deadline: Option<std::time::Duration>) {
    match deadline {
        Some(deadline) => tokio::time::sleep(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interruption {
    Cancelled,
    TimedOut,
    SignalFailed,
}

#[cfg(any(windows, test))]
type NativeCancellation =
    futures_util::future::Shared<futures_util::future::BoxFuture<'static, Interruption>>;

#[cfg(any(windows, test))]
fn cancellation_signal(
    signal: impl std::future::Future<Output = io::Result<()>> + Send + 'static,
    deadline: Option<std::time::Duration>,
) -> NativeCancellation {
    use futures_util::FutureExt;
    async move {
        tokio::select! {
            biased;
            signal = signal => match signal {
                Ok(()) => Interruption::Cancelled,
                Err(_) => Interruption::SignalFailed,
            },
            () = wait_for_deadline(deadline) => Interruption::TimedOut,
        }
    }
    .boxed()
    .shared()
}

#[cfg(any(windows, test))]
async fn drive_native<T>(
    command: NativeCommand<'_>,
    cancelled: NativeCancellation,
    execution: impl std::future::Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    let physical_transfer = matches!(
        command,
        NativeCommand::Family(Invocation::Resource(
            fabric_cli_mutations::frontdoor::ResourceCommand::Copy { .. }
                | fabric_cli_mutations::frontdoor::ResourceCommand::Move { .. }
        ))
    );
    tokio::pin!(execution);
    tokio::select! {
        biased;
        interruption = cancelled => {
            if physical_transfer && interruption != Interruption::SignalFailed {
                // The handler owns cleanup and reconciliation receipts. Its
                // shared cancellation future is now ready; never drop it here.
                execution.await
            } else {
                Err(match interruption {
                    Interruption::Cancelled => interrupted_failure("cancelled", "Read cancelled.", 2),
                    Interruption::TimedOut => interrupted_failure("timed_out", "The read command deadline elapsed.", 1),
                    Interruption::SignalFailed => failure("signal_failed", "The cancellation handler could not start.", 1),
                })
            }
        },
        result = &mut execution => result,
    }
}

fn write_error((code, message, exit): Failure) -> io::Result<CommandOutcome> {
    let mut stderr = io::stderr().lock();
    serde_json::to_writer(
        &mut stderr,
        &json!({"error": {"code": code, "message": message}}),
    )
    .map_err(io::Error::other)?;
    writeln!(stderr)?;
    Ok(
        if matches!(code, "invalid_configuration" | "invalid_arguments") {
            CommandOutcome::Stop(exit)
        } else {
            CommandOutcome::from_code(Some(code), exit)
        },
    )
}

fn required(name: &str) -> Result<String, Failure> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Err(std::env::VarError::NotPresent) => Err(failure(
            "NotConfigured",
            HostError::NotConfigured.to_string(),
            1,
        )),
        _ => Err(failure(
            "invalid_configuration",
            "Invalid native read selector.",
            2,
        )),
    }
}

fn selected_endpoint(value: Option<std::ffi::OsString>) -> Result<EndpointBinding, Failure> {
    match value {
        None => Ok(EndpointBinding::public(ServiceOrigin::Fabric)),
        Some(value) => value
            .to_str()
            .and_then(|host| EndpointBinding::with_host(ServiceOrigin::Fabric, host).ok())
            .ok_or_else(|| {
                failure(
                    "invalid_configuration",
                    "Invalid Fabric endpoint hostname.",
                    2,
                )
            }),
    }
}

pub(crate) fn run(command: &ReadCommand) -> io::Result<CommandOutcome> {
    run_command(NativeCommand::Read(command), None)
}

pub(crate) fn run_job(command: &JobReadCommand) -> io::Result<CommandOutcome> {
    run_command(NativeCommand::Job(command), None)
}

pub(crate) fn run_job_execution(command: &JobRunCommand) -> io::Result<CommandOutcome> {
    run_command(NativeCommand::JobExecution(command), None)
}

pub(crate) fn run_job_mutation(command: &JobMutationCommand) -> io::Result<CommandOutcome> {
    run_command(NativeCommand::JobMutation(command), None)
}

pub(crate) fn run_family(command: &Invocation) -> io::Result<CommandOutcome> {
    run_command(NativeCommand::Family(command), None)
}

#[cfg(feature = "repl")]
pub(crate) fn run_interactive(
    command: &Invocation,
    state: &mut ReplState,
) -> io::Result<CommandOutcome> {
    let command = match command {
        Invocation::Read(command) => NativeCommand::Read(command),
        Invocation::JobRead(command) => NativeCommand::Job(command),
        Invocation::JobRun(command) => NativeCommand::JobExecution(command),
        Invocation::JobMutation(command) => NativeCommand::JobMutation(command),
        command => NativeCommand::Family(command),
    };
    run_command(command, Some(state))
}

fn run_command(
    command: NativeCommand<'_>,
    state: Option<&mut ReplState>,
) -> io::Result<CommandOutcome> {
    let result = (|| {
        // Public selectors only, never token/secret environment inputs.
        let endpoint = selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC"))?;
        if endpoint != EndpointBinding::public(ServiceOrigin::Fabric) && !has_bound_context(command)
        {
            return Err(failure(
                "NotSupported",
                "The selected endpoint is not yet wired for this command family.",
                1,
            ));
        }
        let tenant = required("FAB_RUST_TENANT")?;
        let account = required("FAB_RUST_ACCOUNT")?;
        let client = required("FAB_RUST_CLIENT_ID")?;
        run_selected(command, &tenant, &account, &client, endpoint, state)
    })();
    match result {
        Ok(output) => {
            output
                .plan
                .write_to(&mut io::stdout().lock(), &mut io::stderr().lock())?;
            Ok(output.outcome)
        }
        Err(error) => write_error(error),
    }
}

#[cfg(not(windows))]
fn run_selected(
    _: NativeCommand<'_>,
    _: &str,
    _: &str,
    _: &str,
    _: EndpointBinding,
    _: Option<&mut ReplState>,
) -> Result<NativeOutput, Failure> {
    Err(failure(
        "unsupported",
        "Native read credentials currently require Windows.",
        1,
    ))
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn get_output_clock_never_substitutes_utc_when_local_offset_is_unavailable() {
        let error = get_filename_clock(Err(time::error::IndeterminateOffset)).unwrap_err();
        let output = error.envelope(
            fabric_cli_output::Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            Some("get".into()),
        );
        assert_eq!(output.result().error_code(), Some("ClockFailed"));
        assert!(
            output
                .result()
                .message()
                .unwrap()
                .contains("Check system timezone")
        );
        assert!(
            output
                .result()
                .message()
                .unwrap()
                .contains("No UTC fallback was used")
        );
    }

    #[cfg(windows)]
    #[test]
    fn get_output_selects_the_explicit_file_host_before_native_credentials() {
        let Invocation::Read(command) = fabric_cli::parser::parse_args([
            "fab-rs",
            "get",
            "Demo.Workspace",
            "-q",
            "id",
            "-o",
            "existing-directory",
        ])
        .unwrap() else {
            panic!("read required")
        };
        assert!(selected_read_options(&command).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn physical_copy_does_not_activate_the_definition_filesystem_boundary() {
        for verb in ["cp", "mv"] {
            let command =
                fabric_cli::parser::parse_args(["fab-rs", verb, "source.bin", "target.bin"])
                    .unwrap();
            assert!(!consumes_definition_files(&command));
        }
        let command = fabric_cli::parser::parse_args([
            "fab-rs",
            "export",
            "Demo.Workspace/Book.Notebook",
            "-o",
            "output",
            "-f",
        ])
        .unwrap();
        assert!(consumes_definition_files(&command));
    }

    #[cfg(windows)]
    #[test]
    fn native_get_option_gate_allows_file_output_and_preserves_metadata_flags() {
        for output in [None, Some("private-output-path")] {
            for (force, verbose) in [(false, false), (true, false), (false, true), (true, true)] {
                let mut args = vec!["fab-rs", "get", "Demo.Workspace/Book.Notebook", "-q", "id"];
                if force {
                    args.push("--force");
                }
                if verbose {
                    args.push("--verbose");
                }
                if let Some(output) = output {
                    args.extend(["--output", output]);
                }
                let Invocation::Read(command) = fabric_cli::parser::parse_args(args).unwrap()
                else {
                    panic!("expected GET invocation");
                };
                let original = command.clone();
                selected_read_options(&command).unwrap();
                assert_eq!(command, original);
                assert_eq!(command.output.as_deref(), output);
                assert_eq!(command.force, force);
                assert_eq!(command.verbose, verbose);
                assert_eq!(command.query.as_deref(), Some("id"));
            }
        }
        let Invocation::Read(base) = fabric_cli::parser::parse_args(["fab-rs", "ls", "/"]).unwrap()
        else {
            panic!("expected listing invocation");
        };
        for (force, verbose, output) in [
            (true, false, None),
            (false, true, None),
            (false, false, Some("private-output-path")),
        ] {
            let mut invalid = base.clone();
            invalid.force = force;
            invalid.verbose = verbose;
            invalid.output = output.map(str::to_owned);
            let original = invalid.clone();
            let (code, message, exit) = selected_read_options(&invalid).unwrap_err();
            assert_eq!(invalid, original);
            assert_eq!(code, "NotSupported");
            assert_eq!(exit, 1);
            assert!(!message.contains("private-output-path"));
        }
        assert!(
            fabric_cli::parser::parse_args(["fab-rs", "get", "Demo.Workspace", "--output",])
                .is_err()
        );
    }

    #[test]
    fn default_public_daily_and_msit_keep_one_fabric_scope() {
        assert_eq!(
            selected_endpoint(None).unwrap(),
            EndpointBinding::public(ServiceOrigin::Fabric)
        );
        for host in [
            "api.fabric.microsoft.com",
            "dailyapi.fabric.microsoft.com",
            "msitapi.fabric.microsoft.com",
        ] {
            let endpoint = selected_endpoint(Some(host.into())).unwrap();
            assert_eq!(endpoint.host(), host);
            assert_eq!(endpoint.origin(), ServiceOrigin::Fabric);
            assert_eq!(
                endpoint.audience().scope(),
                "https://api.fabric.microsoft.com/.default"
            );
        }
    }

    #[test]
    fn invalid_endpoint_inputs_fail_without_echoing_the_host() {
        for host in [
            "",
            "https://msitapi.fabric.microsoft.com",
            "api.fabric.microsoft.com:443",
            "api.fabric.microsoft.com/path",
            "api.fabric.microsoft.com.example.test",
            "unknown.example.test",
            "user@api.fabric.microsoft.com",
        ] {
            let (code, message, exit) = selected_endpoint(Some(host.into())).unwrap_err();
            assert_eq!(code, "invalid_configuration");
            assert_eq!(message, "Invalid Fabric endpoint hostname.");
            assert_eq!(exit, 2);
        }
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use std::{
        future::{Future, poll_fn},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::Duration,
    };

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .unwrap()
    }

    #[test]
    fn no_watchdog_stays_pending_and_a_finite_watchdog_elapses() {
        runtime().block_on(async {
            let mut unbounded = std::pin::pin!(wait_for_deadline(None));
            poll_fn(|context| {
                assert!(unbounded.as_mut().poll(context).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            tokio::time::timeout(
                Duration::from_secs(5),
                wait_for_deadline(Some(Duration::ZERO)),
            )
            .await
            .expect("an explicit finite watchdog must still elapse");
        });
    }

    #[test]
    fn shutdown_preserves_the_original_success_or_typed_failure() {
        for result in [
            Ok(7_u8),
            Err(failure("synthetic_failed", "Synthetic failure.", 4)),
        ] {
            assert_eq!(finish_runtime(runtime(), result.clone()), result);
        }
    }

    #[test]
    fn shutdown_is_bounded_while_blocking_work_can_still_finish_with_effects() {
        let runtime = runtime();
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let (completed, completion) = mpsc::channel();
        let effect = Arc::new(AtomicBool::new(false));
        let worker_effect = Arc::clone(&effect);
        let _worker = runtime.spawn_blocking(move || {
            entered.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(30)).unwrap();
            worker_effect.store(true, Ordering::SeqCst);
            completed.send(()).unwrap();
        });
        started.recv_timeout(Duration::from_secs(10)).unwrap();
        let (returned, result) = mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            returned
                .send(finish_runtime(
                    runtime,
                    Err::<u8, _>(interrupted_failure("cancelled", "Read cancelled.", 2)),
                ))
                .unwrap();
        });

        // Release before asserting, so a regression to ordinary runtime Drop
        // fails the test without stranding its blocking worker.
        let observed = result.recv_timeout(Duration::from_secs(10));
        let effect_before_release = effect.load(Ordering::SeqCst);
        release.send(()).unwrap();
        completion.recv_timeout(Duration::from_secs(10)).unwrap();
        shutdown.join().unwrap();

        assert_eq!(
            observed.unwrap(),
            Err(interrupted_failure("cancelled", "Read cancelled.", 2))
        );
        assert!(!effect_before_release);
        assert!(effect.load(Ordering::SeqCst));
    }

    #[test]
    fn interruption_diagnostics_retain_code_exit_and_mutation_uncertainty() {
        for (code, message, exit) in [
            ("cancelled", "Read cancelled.", 2),
            ("timed_out", "The read command deadline elapsed.", 1),
        ] {
            let (actual_code, actual_message, actual_exit) =
                interrupted_failure(code, message, exit);
            assert_eq!(actual_code, code);
            assert_eq!(actual_exit, exit);
            assert!(actual_message.starts_with(message));
            assert!(actual_message.contains("may still complete"));
            assert!(actual_message.contains("No rollback is implied"));
        }
    }

    #[test]
    fn executable_wrapper_awaits_physical_cancellation_and_preserves_partial_delete_receipts() {
        use fabric_cli_storage_files::{TreeError, TreeFailure, TreePaths, TreeResult, TreeStage};
        use std::{path::PathBuf, sync::atomic::AtomicUsize};

        for verb in ["cp", "mv"] {
            runtime().block_on(async {
                let invocation =
                    fabric_cli::parser::parse_args(["fab-rs", verb, "source.bin", "target.bin"])
                        .unwrap();
                let (interrupt, signal) = tokio::sync::oneshot::channel();
                let registrations = Arc::new(AtomicUsize::new(0));
                let registered = Arc::clone(&registrations);
                let cancelled = cancellation_signal(
                    async move {
                        registered.fetch_add(1, Ordering::SeqCst);
                        signal.await.map_err(io::Error::other)
                    },
                    None,
                );
                let handler_cancelled = cancelled.clone();
                let cleanup_finished = Arc::new(AtomicBool::new(false));
                let cleaned = Arc::clone(&cleanup_finished);
                let execution = async move {
                    interrupt.send(()).unwrap();
                    assert_eq!(handler_cancelled.await, Interruption::Cancelled);
                    tokio::task::yield_now().await;
                    cleaned.store(true, Ordering::SeqCst);
                    Ok(TreeError {
                        stage: TreeStage::SourceDelete,
                        failure: TreeFailure::Cancelled,
                        partial: Box::new(TreeResult {
                            local_writes: vec![(PathBuf::from("target/first.bin"), 3)],
                            local_deletes: vec![PathBuf::from("source/first.bin")],
                            ..TreeResult::default()
                        }),
                        source_delete_may_have_occurred: true,
                        local_write_may_have_occurred: false,
                        paths: Box::new(TreePaths {
                            source: Some("source/second.bin".into()),
                            destination: Some("target/second.bin".into()),
                        }),
                    })
                };
                let error = drive_native(NativeCommand::Family(&invocation), cancelled, execution)
                    .await
                    .unwrap();
                assert!(cleanup_finished.load(Ordering::SeqCst));
                assert_eq!(registrations.load(Ordering::SeqCst), 1);
                assert_eq!(error.stage, TreeStage::SourceDelete);
                assert!(matches!(error.failure, TreeFailure::Cancelled));
                assert_eq!(error.partial.local_writes.len(), 1);
                assert_eq!(
                    error.partial.local_deletes,
                    [PathBuf::from("source/first.bin")]
                );
                assert!(error.source_delete_may_have_occurred);
                assert_eq!(error.paths.source.as_deref(), Some("source/second.bin"));
                assert_eq!(
                    error.paths.destination.as_deref(),
                    Some("target/second.bin")
                );
            });
        }
    }

    #[test]
    fn executable_wrapper_keeps_nonphysical_interrupts_and_signal_setup_failures_bounded() {
        runtime().block_on(async {
            let Invocation::Read(read) = fabric_cli::parser::parse_args(["fab-rs", "ls"]).unwrap()
            else {
                panic!("read required");
            };
            let result = drive_native(
                NativeCommand::Read(&read),
                cancellation_signal(async { Ok(()) }, None),
                std::future::pending::<Result<(), Failure>>(),
            )
            .await;
            assert_eq!(result.unwrap_err().0, "cancelled");
            let invocation = fabric_cli::parser::parse_args(["fab-rs", "cp", "a", "b"]).unwrap();
            let started = Arc::new(AtomicBool::new(false));
            let invoked = Arc::clone(&started);
            let result = drive_native(
                NativeCommand::Family(&invocation),
                cancellation_signal(
                    async { Err(io::Error::other("synthetic setup failure")) },
                    None,
                ),
                async move {
                    invoked.store(true, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
            assert_eq!(result.unwrap_err().0, "signal_failed");
            assert!(!started.load(Ordering::SeqCst));
        });
    }
}

#[cfg(windows)]
fn run_selected(
    command: NativeCommand<'_>,
    tenant: &str,
    account: &str,
    client: &str,
    endpoint: EndpointBinding,
    mut state: Option<&mut ReplState>,
) -> Result<NativeOutput, Failure> {
    let options = fabric_identity::LoginOptions::new(tenant, client, account)
        .map_err(|e| failure("invalid_configuration", e.to_string(), 2))?;
    let (current, restored) = state
        .as_ref()
        .map_or_else(crate::local_host::context_selection, |state| {
            state.path().map(|path| (path, false))
        })
        .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    let home = std::env::var("FAB_RUST_HOME").map_or_else(
        |e| match e {
            std::env::VarError::NotPresent => Ok(fabric_cli_context::HomeContext::Unspecified),
            _ => Err(failure(
                "invalid_configuration",
                "Invalid home selector.",
                2,
            )),
        },
        selected_home,
    )?;
    if let Some(state) = state.as_deref_mut()
        && matches!(command, NativeCommand::Family(_))
    {
        state
            .bind(tenant, account, client, home.clone())
            .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    }
    if let NativeCommand::Read(command) = command {
        selected_read_options(command)?;
    }
    let skill = match command {
        NativeCommand::Read(_) => None,
        NativeCommand::Job(command) => command.skill.as_deref(),
        NativeCommand::JobExecution(command) => command.skill.as_deref(),
        NativeCommand::JobMutation(command) => command.skill.as_deref(),
        NativeCommand::Family(_) => None,
    };
    fabric_sdk_jobs::JobRequestHeaders::with_skill(skill).map_err(|_| {
        failure(
            "invalid_arguments",
            "Invalid job request header selection.",
            2,
        )
    })?;
    let saved = if std::env::var_os("FAB_RUST_CONFIG_ROOT").is_some() {
        Some(
            crate::local_host::store()
                .and_then(|store| store.initialize())
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?,
        )
    } else {
        None
    };
    let settings = settings::NativeSettings::select(saved.as_ref())
        .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    let job = match command {
        NativeCommand::JobExecution(command) => Some(command.family_command()),
        NativeCommand::JobMutation(command) => Some(command.family_command()),
        _ => None,
    };
    let deadline = settings::command_watchdog(job.as_ref(), settings.jobs)
        .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
    if deadline.is_some_and(|deadline| std::time::Instant::now().checked_add(deadline).is_none()) {
        return Err(failure(
            "invalid_configuration",
            "The selected job timeout exceeds the native deadline range.",
            2,
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| failure("runtime_failed", "The read runtime could not start.", 1))?;
    let result = runtime.block_on(async {
        let cancelled = cancellation_signal(tokio::signal::ctrl_c(), deadline);
        drive_native(
            command,
            cancelled.clone(),
            execute(
                command,
                options,
                (&current, restored),
                home,
                endpoint,
                (settings, cancelled),
                state,
            ),
        )
        .await
    });
    finish_runtime(runtime, result)
}

#[cfg(windows)]
fn selected_home(value: String) -> Result<fabric_cli_context::HomeContext, Failure> {
    let valid = value.rsplit_once('.').is_some_and(|(name, suffix)| {
        !name.is_empty()
            && !name.contains(['\n', '\r'])
            && (suffix.eq_ignore_ascii_case("Workspace") || suffix.eq_ignore_ascii_case("Personal"))
    });
    if !valid {
        return Err(failure(
            "invalid_configuration",
            "Invalid home selector.",
            2,
        ));
    }
    Ok(fabric_cli_context::HomeContext::Workspace(value))
}

#[cfg(windows)]
async fn execute(
    command: NativeCommand<'_>,
    options: fabric_identity::LoginOptions,
    context: (&str, bool),
    home: fabric_cli_context::HomeContext,
    endpoint: EndpointBinding,
    execution: (settings::NativeSettings, NativeCancellation),
    state: Option<&mut ReplState>,
) -> Result<NativeOutput, Failure> {
    use fabric_cli::session::ReadSession;
    use fabric_cli_context::Element;
    use fabric_sdk::{Audience, Client, CredentialSelection, RequestContext};
    use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
    use fabric_sdk_transport::{
        NativeHttpClient, ProxyPolicy, TokioTimer, TransportOptions, TrustPolicy,
    };
    use std::{num::NonZeroUsize, sync::Arc, time::Duration};

    let (current, restored) = context;
    let (settings, cancelled) = execution;
    let selection = CredentialSelection::new(options.account(), options.tenant(), Audience::Fabric)
        .map_err(|_| failure("invalid_configuration", "Invalid credential selection.", 2))?;
    let tenant = Element::tenant(options.tenant(), options.tenant());
    let credential = fabric_identity::WindowsCredential::connect(options)
        .await
        .map_err(|error| {
            let (code, message) = crate::auth::broker_error(error);
            failure(code, message, 1)
        })?;
    let sdk = Client::with_context(RequestContext::with_endpoint(
        credential,
        selection,
        endpoint.clone(),
    ));
    let client = NativeHttpClient::new(TransportOptions {
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(30),
        write_timeout: Duration::from_secs(30),
        pool_idle_timeout: Duration::from_secs(60),
        max_idle_per_host: NonZeroUsize::new(4).expect("nonzero constant"),
        trust: TrustPolicy::Platform {
            additional_roots: Vec::new(),
        },
        proxy: ProxyPolicy::None,
    })
    .map_err(|_| {
        failure(
            "transport_failed",
            "The native HTTP client could not start.",
            1,
        )
    })?;
    let executor = HttpExecutor::new(
        Arc::new(client),
        Arc::new(TokioTimer),
        ExecutionOptions::default(),
    )
    .map_err(|_| failure("transport_failed", "The HTTP executor could not start.", 1))?
    .with_endpoint(endpoint);
    let now = azure_core::time::OffsetDateTime::now_utc();
    let timestamp = fabric_cli_output::Timestamp::parse(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.microsecond(),
    ))
    .map_err(|_| {
        failure(
            "clock_failed",
            "The UTC clock is outside the supported timestamp range.",
            1,
        )
    })?;
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant,
        current_path: current,
        home,
        timestamp,
        config: settings.reads,
    };
    let result = match command {
        NativeCommand::Read(command) => {
            let command = settings
                .read_command(command)
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
            let default_format = settings
                .output(None)
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
            execute_read(&session, &executor, &command, &default_format).await
        }
        NativeCommand::Job(command) => {
            let mut command = command.clone();
            command.output_format = Some(
                settings
                    .output(command.output_format.as_deref())
                    .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?,
            );
            fabric_cli::jobs::JobReadSession {
                reads: &session,
                jobs: Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor.clone())),
            }
            .dispatch_result(&command, std::future::pending())
            .await
            .map(NativeOutput::job)
        }
        NativeCommand::JobExecution(command) => {
            let command = settings
                .job_command(&command.family_command())
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
            execute_job(&session, &executor, &command, settings.jobs).await
        }
        NativeCommand::JobMutation(command) => {
            let command = settings
                .job_command(&command.family_command())
                .map_err(|error| failure("invalid_configuration", error.to_string(), 2))?;
            execute_job(&session, &executor, &command, settings.jobs).await
        }
        NativeCommand::Family(command) => {
            execute_family(&session, &executor, command, state, cancelled).await
        }
    };
    match result {
        Ok(mut output) => {
            if restored {
                use fabric_cli_output::text::{self, Presentation};
                let message = format!(
                    "Command context path: {}",
                    if current == "/" { "root" } else { current }
                );
                let mut warning = text::render_notice(
                    text::Notice::Warning(&message),
                    None,
                    Presentation::default(),
                );
                warning.records.append(&mut output.plan.records);
                output.plan.records = warning.records;
            }
            Ok(output)
        }
        Err(error) => {
            // Retain source output-before-error, rather than silently dropping it.
            if let Some(partial) = error.partial_output() {
                partial
                    .write_to(&mut io::stdout().lock(), &mut io::stderr().lock())
                    .map_err(|_| {
                        failure("output_failed", "Writing partial read output failed.", 1)
                    })?;
            }
            Err(failure("render_failed", error.to_string(), 1))
        }
    }
}

#[cfg(windows)]
fn get_filename_clock(
    local: Result<time::OffsetDateTime, time::error::IndeterminateOffset>,
) -> Result<String, fabric_cli_output::CommandError> {
    local
        .map(fabric_cli::get_output::filename_stamp)
        .map_err(|_| {
            fabric_cli_output::CommandError::new(
                Some(
                    "The local timezone offset could not be determined for GET output. Check system timezone configuration and, on Unix, use a host where offset discovery is safe. No UTC fallback was used."
                        .into(),
                ),
                Some("ClockFailed".into()),
            )
        })
}

#[cfg(windows)]
async fn execute_read(
    session: &fabric_cli::session::ReadSession<'_>,
    executor: &fabric_sdk_http::HttpExecutor,
    command: &ReadCommand,
    default_output_format: &str,
) -> Result<NativeOutput, fabric_cli_output::text::TextError> {
    let Some(output) = command
        .output
        .as_deref()
        .filter(|output| !output.is_empty())
    else {
        let mut command = command.clone();
        command.output = None;
        return session
            .with_source_operations(executor)
            .dispatch_result(&command, std::future::pending())
            .await
            .map(NativeOutput::read);
    };
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(_) => {
            use fabric_cli_output::{CommandError, text};
            let error = CommandError::new(
                Some("The local output directory could not be selected".into()),
                Some("InvalidPath".into()),
            );
            let plan = text::render(
                &error.envelope(session.timestamp.clone(), Some(command.name.clone())),
                text::OutputFormat::resolve(command.output_format.as_deref(), Some("json"))?,
                &text::TextOptions::default(),
                &text::ascii_width,
                text::Presentation::default(),
            )?;
            let outcome = crate::repl_state::error_outcome(&error, plan.exit_code);
            return Ok(NativeOutput { plan, outcome });
        }
    };
    let candidate = transfer_paths::candidate(
        output,
        &current,
        transfer_paths::home_from_environment().as_deref(),
        std::env::var_os("USERNAME").as_deref(),
    );
    let mut local = fabric_cli::families::transfer_files::TokioTransferFiles::defer_export_root(
        candidate,
        output.to_owned(),
        Default::default(),
        tokio::runtime::Handle::current(),
    );
    let mut files = fabric_cli::get_output::native_files(&mut local, session, executor);
    let clock = || get_filename_clock(time::OffsetDateTime::now_local());
    session
        .with_source_operations(executor)
        .dispatch_result_with_files(
            command,
            &mut fabric_cli::get_output::ResultFileHost {
                files: &mut files,
                clock: &clock,
                default_output_format: Some(default_output_format),
            },
            std::future::pending(),
        )
        .await
        .map(NativeOutput::read)
}

#[cfg(windows)]
async fn execute_job(
    session: &fabric_cli::session::ReadSession<'_>,
    executor: &fabric_sdk_http::HttpExecutor,
    command: &fabric_cli_jobs::JobCommand,
    config: fabric_cli_jobs::JobConfig,
) -> Result<NativeOutput, fabric_cli_output::text::TextError> {
    fabric_cli::jobs::JobSession {
        reads: session,
        jobs: std::sync::Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor.clone())),
        files: &fabric_cli::jobs::SelectedJobFiles,
        ui: &NativeJobUi,
        config,
    }
    .dispatch_result(command, std::future::pending())
    .await
    .map(NativeOutput::job)
}

#[cfg(windows)]
async fn execute_family(
    session: &fabric_cli::session::ReadSession<'_>,
    executor: &fabric_sdk_http::HttpExecutor,
    command: &Invocation,
    mut state: Option<&mut ReplState>,
    cancelled: NativeCancellation,
) -> Result<NativeOutput, fabric_cli_output::text::TextError> {
    use fabric_cli_context::PathElement;
    use fabric_cli_local::NavigationState;
    use fabric_cli_output::text::{self, OutputFormat, Presentation, TextOptions};

    let before = state
        .as_ref()
        .and_then(|state| state.navigation.as_ref())
        .map(|navigation| std::sync::Arc::clone(navigation.current()));
    let result = async {
        let store = crate::local_host::store()?;
        store.initialize()?;
        let local_root = std::env::current_dir()?;
        let physical_files =
            fabric_cli::families::native_resource_files::NativeResourceFileInputs::new(
                local_root.clone(),
                tokio::runtime::Handle::current(),
            );
        let mut navigation = NavigationState::new(
            PathElement::from_fabric(session.tenant.clone()),
            session.home.clone(),
            local_root.to_string_lossy().into_owned(),
            crate::local_host::session_id()?,
            state.is_none(),
        )?;
        let navigation = match state.as_deref_mut() {
            Some(state) => state
                .navigation
                .as_mut()
                .expect("interactive native context bound"),
            None => &mut navigation,
        };
        let mut effects = NativeLocalEffects {
            base: crate::local_host::Effects,
            identity: fabric_cli_local::BrowserIdentity {
                user: true,
                claims: vec![
                    ("upn".into(), session.sdk.selection().account().into()),
                    ("tid".into(), session.sdk.selection().tenant().into()),
                ],
            },
            capacities: None,
        };
        if matches!(command, Invocation::Local(local)
                if matches!(&local.command, fabric_cli_local::LocalCommand::ConfigSet { key, .. }
                    if key.eq_ignore_ascii_case("default_capacity")))
        {
            let list = session
                .sdk
                .list_capacities(session.executor, Default::default(), std::future::pending())
                .await
                .map_err(|_| {
                    fabric_cli_local::LocalError::command(
                        "ReadFailed",
                        "Capacity metadata did not complete",
                    )
                })?;
            effects.capacities = Some(
                list.value
                    .into_iter()
                    .map(|capacity| fabric_cli_local::Capacity {
                        display_name: capacity.display_name,
                        id: capacity.id,
                    })
                    .collect(),
            );
        }
        let mut mutations = NativeMutationUi::default();
        let mut confirmation = terminal_ui::NativeAccessConfirmation::default();
        let mut files = if consumes_definition_files(command) {
            let files: Box<dyn fabric_cli_transfer::TransferFiles> =
                if let Invocation::Deploy(deploy) = command {
                    let argument = deploy.config.clone().ok_or_else(|| {
                        fabric_cli_local::LocalError::command(
                            "InvalidInput",
                            "A deployment configuration path is required",
                        )
                    })?;
                    let home = transfer_paths::home_from_environment();
                    let candidate = transfer_paths::candidate(
                        &argument,
                        &local_root,
                        home.as_deref(),
                        std::env::var_os("USERNAME").as_deref(),
                    );
                    Box::new(
                        fabric_cli::families::transfer_files::TokioDeployFiles::defer_config(
                            candidate,
                            argument,
                            Default::default(),
                            tokio::runtime::Handle::current(),
                        ),
                    )
                } else if let Invocation::Transfer(transfer) = command
                    && matches!(
                        transfer.kind,
                        fabric_cli_transfer::TransferKind::Import
                            | fabric_cli_transfer::TransferKind::Export
                            | fabric_cli_transfer::TransferKind::BulkExport
                    )
                {
                    let tokens = if transfer.kind == fabric_cli_transfer::TransferKind::Import {
                        transfer.input.as_deref()
                    } else {
                        transfer.output.as_deref()
                    };
                    let input =
                        tokens
                            .map(fabric_cli_transfer::process_tokens)
                            .ok_or_else(|| {
                                fabric_cli_local::LocalError::command(
                                    "InvalidInput",
                                    "A transfer path is required",
                                )
                            })?;
                    let home = transfer_paths::home_from_environment();
                    let candidate = transfer_paths::candidate(
                        &input,
                        &local_root,
                        home.as_deref(),
                        std::env::var_os("USERNAME").as_deref(),
                    );
                    if transfer.kind == fabric_cli_transfer::TransferKind::Import {
                        Box::new(
                        fabric_cli::families::transfer_files::TokioTransferFiles::defer_import_root(
                            candidate,
                            input,
                            Default::default(),
                            tokio::runtime::Handle::current(),
                        ),
                    )
                    } else {
                        Box::new(
                        fabric_cli::families::transfer_files::TokioTransferFiles::defer_export_root(
                            candidate,
                            input,
                            Default::default(),
                            tokio::runtime::Handle::current(),
                        ),
                    )
                    }
                } else {
                    Box::new(
                        fabric_cli::families::transfer_files::TokioTransferFiles::new(
                            local_root,
                            None,
                            Default::default(),
                            tokio::runtime::Handle::current(),
                        )
                        .await?,
                    )
                };
            Some(files)
        } else {
            None
        };
        let mut families = fabric_cli::families::FamilySession {
            reads: session,
            http: executor,
            store: &store,
            navigation,
            local: &mut effects,
            mutations: &mut mutations,
            confirmation: Some(&mut confirmation),
            transfer_ui: &NativeTransferUi,
            transfer_files: files
                .as_mut()
                .map(|files| files.as_mut() as &mut dyn fabric_cli_transfer::TransferFiles),
            resource_files: None,
        };
        let mut output = if matches!(command, Invocation::Deploy(_)) {
            let inputs = deploy_inputs::snapshot_environment().map_err(|error| {
                fabric_cli_local::LocalError::command("InvalidInput", error.to_string())
            })?;
            families
                .dispatch_with_deploy_inputs(command, &inputs, std::future::pending())
                .await?
        } else if matches!(
            command,
            Invocation::Resource(fabric_cli_mutations::frontdoor::ResourceCommand::Remove { .. })
        ) {
            let mut removal = NativeMutationUi::default();
            let mut output = families
                .dispatch_with_removal_ui(command, &mut removal, std::future::pending())
                .await?;
            let mut notices = std::mem::take(&mut removal.records);
            notices.append(&mut output.plan.records);
            output.plan.records = notices;
            output
        } else if matches!(
            command,
            Invocation::Resource(
                fabric_cli_mutations::frontdoor::ResourceCommand::Copy { .. }
                    | fabric_cli_mutations::frontdoor::ResourceCommand::Move { .. }
            )
        ) {
            families
                .dispatch_with_physical_files(command, &physical_files, async {
                    let _ = cancelled.await;
                })
                .await?
        } else {
            families.dispatch(command, std::future::pending()).await?
        };
        let mut notices = std::mem::take(&mut mutations.records);
        notices.append(&mut output.plan.records);
        output.plan.records = notices;
        let outcome = crate::repl_state::family_outcome(&output);
        Ok::<_, fabric_cli_local::LocalError>(NativeOutput {
            plan: output.plan,
            outcome,
        })
    }
    .await;
    if let Some(state) = state
        && let Some(before) = before
    {
        state.commit_navigation(&before);
    }
    match result {
        Ok(output) => Ok(output),
        Err(fabric_cli_local::LocalError::Text(error)) => Err(error),
        Err(error) => {
            let outcome_error = error.command_error();
            let plan = text::render(
                &error
                    .command_error()
                    .envelope(session.timestamp.clone(), None),
                OutputFormat::Json,
                &TextOptions::default(),
                &text::ascii_width,
                Presentation::default(),
            )?;
            let outcome = crate::repl_state::error_outcome(&outcome_error, plan.exit_code);
            Ok(NativeOutput { plan, outcome })
        }
    }
}

#[cfg(windows)]
struct NativeLocalEffects {
    base: crate::local_host::Effects,
    identity: fabric_cli_local::BrowserIdentity,
    capacities: Option<Vec<fabric_cli_local::Capacity>>,
}

#[cfg(windows)]
impl fabric_cli_local::LocalEffects for NativeLocalEffects {
    fn clear_cli_cache(&mut self) -> Result<(), fabric_cli_local::LocalError> {
        self.base.clear_cli_cache()
    }
    fn capacities(
        &mut self,
    ) -> Result<Vec<fabric_cli_local::Capacity>, fabric_cli_local::LocalError> {
        self.capacities.clone().ok_or_else(|| {
            fabric_cli_local::LocalError::command(
                "NotConfigured",
                "Capacity metadata is not loaded",
            )
        })
    }
    fn browser_identity(
        &self,
    ) -> Result<fabric_cli_local::BrowserIdentity, fabric_cli_local::LocalError> {
        Ok(self.identity.clone())
    }
    fn open_browser(&mut self, url: &str) -> Result<(), fabric_cli_local::LocalError> {
        self.base.open_browser(url)
    }
    fn clear_terminal(&mut self) -> Result<(), fabric_cli_local::LocalError> {
        self.base.clear_terminal()
    }
}

#[cfg(windows)]
struct NativeTransferUi;

#[cfg(windows)]
impl fabric_cli_transfer::TransferUi for NativeTransferUi {
    fn emit(
        &self,
        event: fabric_cli_transfer::TransferEvent,
    ) -> Result<(), fabric_cli_output::CommandError> {
        use fabric_cli_output::{
            CommandError, OutputStream,
            text::{self, Presentation, Style},
        };
        let plan = match event {
            fabric_cli_transfer::TransferEvent::Grey(message) => text::render_line(
                &message,
                Style::Grey,
                OutputStream::Stderr,
                Presentation::default(),
            ),
            fabric_cli_transfer::TransferEvent::Warning(message) => text::render_notice(
                text::Notice::Warning(&message),
                None,
                Presentation::default(),
            ),
        };
        plan.write_to(&mut io::stdout().lock(), &mut io::stderr().lock())
            .map_err(|error| {
                CommandError::new(Some(error.to_string()), Some("LocalIOError".into()))
            })
    }
    fn confirm<'a>(
        &'a self,
        question: &'a str,
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<bool, fabric_cli_output::CommandError>>
    {
        Box::pin(async move { fabric_cli::console::confirm(question).await })
    }
    fn select<'a>(
        &'a self,
        names: &'a [String],
    ) -> fabric_cli_transfer::TransferFuture<'a, Result<Vec<String>, fabric_cli_output::CommandError>>
    {
        Box::pin(async move { fabric_cli::console::select_items(names).await })
    }
}
#[cfg(windows)]
struct NativeJobUi;

#[cfg(windows)]
impl fabric_cli_jobs::JobUi for NativeJobUi {
    fn emit(
        &self,
        event: fabric_cli_jobs::JobEvent,
    ) -> Result<(), fabric_cli_output::CommandError> {
        let plan = fabric_cli::jobs::render_job_event(&event).map_err(|error| {
            fabric_cli_output::CommandError::new(
                Some(error.to_string()),
                Some("UnexpectedError".into()),
            )
        })?;
        plan.write_to(&mut io::stdout().lock(), &mut io::stderr().lock())
            .map_err(|_| {
                fabric_cli_output::CommandError::new(
                    Some("Writing job output failed".into()),
                    Some("UnexpectedError".into()),
                )
            })
    }

    fn confirm(&self) -> fabric_cli_jobs::Confirmation<'_> {
        Box::pin(async { fabric_cli::console::confirm("Are you sure?").await })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn home_selection_preserves_resolver_escaping_and_suffix_case() {
        for name in ["A\\/B.Workspace", "Demo.workspace", "Demo.Personal"] {
            assert!(matches!(
                selected_home(name.into()).unwrap(),
                fabric_cli_context::HomeContext::Workspace(value) if value == name
            ));
        }
        for name in ["", "Demo", ".Workspace", "Demo.Notebook"] {
            assert_eq!(
                selected_home(name.into()).unwrap_err().0,
                "invalid_configuration"
            );
        }
    }

    #[test]
    fn typed_read_control_is_retained_before_rendering_without_guessing_from_exit_two() {
        use fabric_cli_output::{CommandError, text::TextPlan};
        for (code, expected) in [
            (
                "LongRunningOperationCancelled",
                CommandOutcome::Cancelled(2),
            ),
            ("ReadFailed", CommandOutcome::Returned(2)),
        ] {
            for source_output in [false, true] {
                let output = fabric_cli::session::ReadDispatch {
                    plan: TextPlan {
                        records: Vec::new(),
                        exit_code: 2,
                    },
                    error: (!source_output).then(|| CommandError::new(None, Some(code.into()))),
                    output_error_code: source_output.then(|| code.into()),
                };
                assert_eq!(NativeOutput::read(output).outcome, expected);
            }
        }
    }
}
