// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Thin injectable parser/host/output composition. Only the executable owns runtime and signals.
#![forbid(unsafe_code)]

pub mod lifecycle;
mod selection;

pub use selection::SelectionFile;

use async_trait::async_trait;
use azure_core::credentials::{TokenCredential, TokenRequestOptions};
use clap::{
    Arg, ArgAction, Command,
    error::{ContextKind, ContextValue, ErrorKind as ParseErrorKind},
};
use fabric_cli_command_contracts::CommandError;
use fabric_core::{
    Audience, CancellationToken, Clock, CoreError, ErrorKind, HttpExecutor, HttpTransport,
    OperationContext, OperationKind,
};
use fabric_identity::{AzureCliSession, Binding, BoundCredential, ProcessExecutor};
use fabric_sdk_api::{PollObserver, PollProgress};
use fabric_sdk_http::{HttpClient, SystemClock, bounded_with_clock};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{IsTerminal, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{mpsc, oneshot};

const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);
const TERMINAL_REFRESH: Duration = Duration::from_secs(1);

struct ProgressEvent {
    progress: ConsoleUpdate,
    written: oneshot::Sender<Result<(), CoreError>>,
}

enum ConsoleUpdate {
    Accepted(OperationKind, u16),
    Waiting(Duration, tokio::time::Instant),
    Polling(OperationKind),
    Status(PollProgress),
    Finished,
}

struct ConsoleProgress {
    events: mpsc::Sender<ProgressEvent>,
    failed: CancellationToken,
}
impl ConsoleProgress {
    async fn send(&self, progress: ConsoleUpdate) -> Result<(), CoreError> {
        let (written, received) = oneshot::channel();
        self.events
            .send(ProgressEvent { progress, written })
            .await
            .map_err(|_| progress_error())?;
        received.await.map_err(|_| progress_error())?
    }
}

#[async_trait]
impl PollObserver for ConsoleProgress {
    async fn accepted(&self, kind: OperationKind, status: u16) -> Result<(), CoreError> {
        self.send(ConsoleUpdate::Accepted(kind, status)).await
    }

    async fn waiting(&self, _: OperationKind, delay: Duration) -> Result<(), CoreError> {
        self.send(ConsoleUpdate::Waiting(delay, tokio::time::Instant::now()))
            .await
    }

    async fn polling(&self, kind: OperationKind) -> Result<(), CoreError> {
        self.send(ConsoleUpdate::Polling(kind)).await
    }

    async fn observe(&self, progress: PollProgress) -> Result<(), CoreError> {
        self.send(ConsoleUpdate::Status(progress)).await
    }
}

fn progress_error() -> CoreError {
    CoreError::new(ErrorKind::Transport, "Cannot write polling progress")
}

fn progress_line(
    message: &str,
    color: bool,
    tone: &str,
    stderr: &mut dyn Write,
) -> Result<(), CoreError> {
    let result = if color {
        writeln!(stderr, "\x1b[{tone}m{message}\x1b[0m")
    } else {
        writeln!(stderr, "{message}")
    };
    result
        .and_then(|()| stderr.flush())
        .map_err(|_| progress_error())
}

fn progress_label(kind: OperationKind) -> &'static str {
    if kind == OperationKind::FabricJob {
        "job"
    } else {
        "operation"
    }
}

fn status_text(progress: PollProgress) -> String {
    if let Some(percentage) = progress.percentage_complete() {
        format!("{} ({percentage}%)", progress.status())
    } else {
        progress.status().into()
    }
}

#[derive(Default)]
struct ProgressDisplay {
    started: Option<(OperationKind, tokio::time::Instant)>,
    latest: Option<PollProgress>,
    requesting: Option<tokio::time::Instant>,
    scheduled: Option<(Duration, tokio::time::Instant)>,
    transient: bool,
    transient_color: bool,
}

impl ProgressDisplay {
    fn clear(&mut self, stderr: &mut dyn Write) -> Result<(), CoreError> {
        if self.transient {
            let reset = if self.transient_color { "\x1b[0m" } else { "" };
            write!(stderr, "{reset}\r\x1b[2K\x1b[?7h")
                .and_then(|()| stderr.flush())
                .map_err(|_| progress_error())?;
            self.transient = false;
        }
        Ok(())
    }

    fn update(
        &mut self,
        update: ConsoleUpdate,
        color: bool,
        terminal: bool,
        stderr: &mut dyn Write,
    ) -> Result<(), CoreError> {
        if matches!(update, ConsoleUpdate::Finished) {
            return self.finish(color, stderr);
        }
        if let ConsoleUpdate::Waiting(delay, since) = update {
            self.scheduled = Some((delay, since));
            self.requesting = None;
            return if terminal {
                self.heartbeat(color, terminal, stderr)
            } else {
                Ok(())
            };
        }
        self.clear(stderr)?;
        let (message, tone) = match update {
            ConsoleUpdate::Accepted(kind, status) => {
                self.started = Some((kind, tokio::time::Instant::now()));
                let label = if kind == OperationKind::FabricJob {
                    "Job"
                } else {
                    "Operation"
                };
                (
                    format!("{label} accepted (HTTP {status}). Polling for completion..."),
                    "36",
                )
            }
            ConsoleUpdate::Polling(kind) => {
                self.requesting = Some(tokio::time::Instant::now());
                self.scheduled = None;
                (format!("Checking {} status...", progress_label(kind)), "36")
            }
            ConsoleUpdate::Status(progress) => {
                self.latest = Some(progress);
                self.requesting = None;
                let label = if progress.is_job() {
                    "Job instance status"
                } else {
                    "Operation status"
                };
                let tone = match progress.status() {
                    "Succeeded" | "Completed" | "Deduped" => "32",
                    "Failed" => "31",
                    "Cancelled" => "33",
                    _ => "36",
                };
                (format!("{label}: {}", status_text(progress)), tone)
            }
            ConsoleUpdate::Waiting(..) => unreachable!("handled before rendering permanent output"),
            ConsoleUpdate::Finished => unreachable!("handled before rendering permanent output"),
        };
        progress_line(&message, color, tone, stderr)
    }

    fn heartbeat(
        &mut self,
        color: bool,
        terminal: bool,
        stderr: &mut dyn Write,
    ) -> Result<(), CoreError> {
        let (kind, started) = self.started.ok_or_else(progress_error)?;
        let label = progress_label(kind);
        let waiting = if self.latest.is_some_and(|progress| {
            matches!(progress.status(), "Succeeded" | "Completed" | "Deduped")
        }) {
            format!("Waiting for {label} result")
        } else if let Some(since) = self.requesting {
            format!(
                "Waiting for {label} status response; {}s since this check started",
                since.elapsed().as_secs()
            )
        } else if let Some((delay, since)) = self.scheduled {
            let remaining = delay.saturating_sub(since.elapsed());
            let seconds = remaining
                .as_secs()
                .saturating_add(u64::from(remaining.subsec_nanos() > 0));
            format!("Waiting for {label} completion; next status check in {seconds}s")
        } else {
            format!("Waiting for {label} completion")
        };
        let last = self.latest.map_or_else(String::new, |progress| {
            format!("; Last reported status: {}", status_text(progress))
        });
        let message = format!(
            "{waiting}{last} ({}s elapsed)...",
            started.elapsed().as_secs()
        );
        if terminal {
            self.transient = true;
            self.transient_color = color;
            // Clip narrow-terminal output instead of wrapping/scrolling past the live line.
            let result = if color {
                write!(stderr, "\r\x1b[2K\x1b[?7l\x1b[90m{message}\x1b[0m\x1b[?7h")
            } else {
                write!(stderr, "\r\x1b[2K\x1b[?7l{message}\x1b[?7h")
            };
            result
                .and_then(|()| stderr.flush())
                .map_err(|_| progress_error())
        } else {
            progress_line(&message, color, "90", stderr)
        }
    }

    fn finish(&mut self, color: bool, stderr: &mut dyn Write) -> Result<(), CoreError> {
        self.clear(stderr)?;
        if let Some((kind, _)) = self.started {
            let label = if kind == OperationKind::FabricJob {
                "Job"
            } else {
                "Operation"
            };
            let message = if self.latest.is_some_and(|p| p.status() == "Deduped") {
                format!("{label} finished with status: Deduped.")
            } else {
                format!("{label} completed.")
            };
            progress_line(&message, color, "32", stderr)?;
        }
        Ok(())
    }
}

/// Host-owned effects; fake implementations prove parser and output behavior offline.
#[async_trait]
pub trait Host: Send + Sync {
    /// Explicit clock used for timestamps, deadlines, and polling.
    fn clock(&self) -> Arc<dyn Clock>;
    /// Read only this Rust CLI's non-secret selection.
    fn selection(&self) -> Result<Option<Binding>, CommandError>;
    /// Atomically persist tenant/principal only.
    fn save_selection(&self, binding: &Binding) -> Result<(), CommandError>;
    /// Clear only Rust selection; absent selection is success.
    fn clear_selection(&self) -> Result<(), CommandError>;
    /// Validate the current Azure CLI context, without signing in or mutating it.
    async fn login(&self, context: &OperationContext) -> Result<Binding, CommandError>;
    /// Bind an explicit existing selection to an injected credential source.
    fn credential(&self, binding: Binding) -> Result<Arc<dyn TokenCredential>, CommandError>;
    /// Construct transport only after request and selection validation.
    fn transport(&self) -> Result<Arc<dyn HttpTransport>, CommandError>;
    /// Drain safe static diagnostics for the host to write to stderr, never JSON stdout.
    fn take_diagnostics(&self) -> Vec<&'static str> {
        Vec::new()
    }
    /// Whether progress can use ANSI color. Injected/nonterminal streams default to plain text.
    fn progress_color(&self) -> bool {
        false
    }
    /// Whether stderr supports in-place terminal progress, independently of color.
    fn progress_terminal(&self) -> bool {
        false
    }
}

/// Production composition with lazy configuration discovery.
#[derive(Default)]
pub struct NativeHost {
    selection_path: Option<PathBuf>,
    diagnostics: Arc<Mutex<Vec<&'static str>>>,
}

impl NativeHost {
    /// Inject an explicit Rust selection path, without opening it.
    pub fn with_selection_path(path: PathBuf) -> Self {
        Self {
            selection_path: Some(path),
            ..Default::default()
        }
    }

    fn file(&self) -> Result<SelectionFile, CommandError> {
        self.selection_path
            .clone()
            .map(SelectionFile::new)
            .map(Ok)
            .unwrap_or_else(SelectionFile::discover)
    }

    fn session(&self) -> Result<Arc<AzureCliSession>, CommandError> {
        Ok(Arc::new(AzureCliSession::new(
            Arc::new(ProcessExecutor),
            self.clock(),
        )?))
    }
}

struct NativeClock {
    diagnostics: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl Clock for NativeClock {
    fn now(&self) -> Instant {
        SystemClock.now()
    }
    fn wall_now(&self) -> SystemTime {
        SystemClock.wall_now()
    }
    fn diagnostic(&self, message: &'static str) {
        self.diagnostics
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(message);
    }
    async fn sleep(
        &self,
        delay: Duration,
        context: &OperationContext,
    ) -> Result<(), fabric_core::CoreError> {
        SystemClock.sleep(delay, context).await
    }
}

#[async_trait]
impl Host for NativeHost {
    fn clock(&self) -> Arc<dyn Clock> {
        Arc::new(NativeClock {
            diagnostics: self.diagnostics.clone(),
        })
    }
    fn selection(&self) -> Result<Option<Binding>, CommandError> {
        self.file()?.read()
    }
    fn save_selection(&self, binding: &Binding) -> Result<(), CommandError> {
        self.file()?.write(binding)
    }
    fn clear_selection(&self) -> Result<(), CommandError> {
        self.file()?.clear()
    }
    async fn login(&self, context: &OperationContext) -> Result<Binding, CommandError> {
        Ok(self.session()?.login(context).await?)
    }
    fn credential(&self, binding: Binding) -> Result<Arc<dyn TokenCredential>, CommandError> {
        Ok(Arc::new(BoundCredential::new(self.session()?, binding)?))
    }
    fn transport(&self) -> Result<Arc<dyn HttpTransport>, CommandError> {
        Ok(Arc::new(fabric_sdk_transport::NativeHttpClient::new()?))
    }
    fn take_diagnostics(&self) -> Vec<&'static str> {
        std::mem::take(
            &mut *self
                .diagnostics
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }
    fn progress_color(&self) -> bool {
        self.progress_terminal() && std::env::var_os("NO_COLOR").is_none()
    }
    fn progress_terminal(&self) -> bool {
        std::io::stderr().is_terminal()
            && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
    }
}

fn help_arg() -> Arg {
    Arg::new("help").long("help").action(ArgAction::Help)
}

/// Build the complete small parser, without any host/provider/configuration effects.
pub fn parser() -> Command {
    let auth_leaf = |name| Command::new(name).disable_help_flag(true).arg(help_arg());
    Command::new("fab-rs")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Raw API and Azure CLI identity binding. Recognized operations poll by default; --async returns immediately.")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .disable_help_subcommand(true)
        .subcommand_required(true)
        .arg(help_arg())
        .arg(Arg::new("version").long("version").action(ArgAction::Version))
        .subcommand(Command::new("auth").about("Bind fab-rs to an existing Azure CLI identity.")
            .after_help("fab-rs does not sign in to or sign out of Azure CLI; run az login separately.")
            .disable_help_flag(true).disable_help_subcommand(true).arg(help_arg()).subcommand_required(true)
            .subcommand(auth_leaf("login").about("Validate and bind the current Azure CLI tenant/principal"))
            .subcommand(auth_leaf("status").about("Validate the selected Azure CLI identity"))
            .subcommand(auth_leaf("logout").about("Clear Rust selection only; leave Azure CLI signed in")))
        .subcommand(fabric_cli_api::command())
}

fn parse_arguments(args: &[OsString]) -> Result<clap::ArgMatches, clap::Error> {
    if args.len() == 2 && args[1] == "auth" {
        return parser()
            .try_get_matches_from(args.iter().cloned().chain([OsString::from("--help")]));
    }
    parser().try_get_matches_from(args)
}

fn command_name(args: &[OsString]) -> &'static str {
    match (
        args.get(1).and_then(|v| v.to_str()),
        args.get(2).and_then(|v| v.to_str()),
    ) {
        (Some("api"), _) => "api",
        (Some("auth"), Some("login")) => "auth login",
        (Some("auth"), Some("status")) => "auth status",
        (Some("auth"), Some("logout")) => "auth logout",
        (Some("auth"), _) => "auth",
        _ => "",
    }
}

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
    if args.len() == 1 {
        return welcome(stdout, stderr);
    }
    let name = command_name(&args);
    let matches = match parse_arguments(&args) {
        Ok(matches) => matches,
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
            // Clap diagnostics can echo raw input containing header credentials. Do not copy them.
            let message = if name == "api"
                && error.kind() == ParseErrorKind::InvalidValue
                && matches!(
                    error.get(ContextKind::InvalidArg),
                    Some(ContextValue::String(arg)) if arg == "--method <method>"
                ) {
                "Invalid HTTP method for --method. Supported values are lowercase: get, post, delete, put, patch. For example, use post instead of POST."
            } else {
                "Invalid command arguments. Run fab-rs --help."
            };
            return write_failure(
                host,
                name,
                CommandError::new("InvalidInput", message),
                stdout,
                stderr,
            );
        }
    };
    let (sender, mut events) = mpsc::channel(1);
    let observer = ConsoleProgress {
        events: sender,
        failed: CancellationToken::new(),
    };
    let result = {
        let execution = execute(&matches, host, cancellation, &observer);
        let interval = if host.progress_terminal() {
            TERMINAL_REFRESH
        } else {
            PROGRESS_INTERVAL
        };
        let heartbeat = tokio::time::sleep(interval);
        let mut display = ProgressDisplay::default();
        tokio::pin!(execution);
        tokio::pin!(heartbeat);
        loop {
            tokio::select! {
                biased;
                result = &mut execution => {
                    if let Err(error) = display.clear(stderr) {
                        let mut failure = result.err().unwrap_or_else(|| error.into());
                        failure.message.push_str("; cannot clear polling progress output");
                        break Err(failure);
                    }
                    break result;
                }
                Some(event) = events.recv() => {
                    let result = display.update(event.progress, host.progress_color(), host.progress_terminal(), stderr);
                    // Cancellation can drop the observer while its event is queued.
                    let _ = event.written.send(result);
                    heartbeat.as_mut().reset(tokio::time::Instant::now() + interval);
                }
                () = &mut heartbeat, if display.started.is_some() && !observer.failed.is_cancelled() => {
                    if display.heartbeat(host.progress_color(), host.progress_terminal(), stderr).is_err() {
                        observer.failed.cancel();
                    }
                    heartbeat.as_mut().reset(tokio::time::Instant::now() + interval);
                }
            }
        }
    };
    for diagnostic in host.take_diagnostics() {
        let _ = writeln!(stderr, "{diagnostic}");
    }
    match result {
        Ok(data) => {
            let output = fabric_cli_output::success(&timestamp(host), name, data);
            if stdout.write_all(output.as_bytes()).is_ok() {
                0
            } else {
                let _ = stderr.write_all(b"Cannot write JSON output\n");
                1
            }
        }
        Err(error) => write_failure(host, name, error, stdout, stderr),
    }
}

fn selected(host: &dyn Host) -> Result<Binding, CommandError> {
    host.selection()?.ok_or_else(|| {
        CommandError::new(
            "Unauthorized",
            "No Rust identity selected. Run fab-rs auth login.",
        )
    })
}

fn auth_data(binding: &Binding) -> Value {
    json!({
        "logged_in": true,
        "authentication_mode": "Azure CLI",
        "tenant_id": binding.tenant,
        "principal_id": binding.principal,
    })
}

async fn execute(
    matches: &clap::ArgMatches,
    host: &dyn Host,
    cancellation: CancellationToken,
    observer: &ConsoleProgress,
) -> Result<Value, CommandError> {
    match matches.subcommand() {
        Some(("auth", auth)) => match auth.subcommand_name() {
            Some("logout") => {
                host.clear_selection()?;
                Ok(json!({"selected": false}))
            }
            Some("login") => {
                let context = OperationContext::new(Duration::from_secs(240), cancellation);
                let binding = host.login(&context).await?;
                host.save_selection(&binding)?;
                let mut data = auth_data(&binding);
                data["selected"] = json!(true);
                data["mfa_evidence"] = json!("not_checked");
                data["service_access_verified"] = json!(false);
                Ok(data)
            }
            Some("status") => {
                let binding = selected(host)?;
                let credential = host.credential(binding.clone())?;
                let clock = host.clock();
                let context = OperationContext {
                    deadline: Some(clock.now() + Duration::from_secs(240)),
                    cancellation,
                };
                bounded_with_clock(
                    clock.as_ref(),
                    &context,
                    credential.get_token(
                        &[Audience::Fabric.scope()],
                        Some(TokenRequestOptions::default()),
                    ),
                )
                .await?
                .map_err(|_| {
                    CommandError::new(
                        "Unauthorized",
                        "Credential acquisition failed; run fab-rs auth login to validate the selected session",
                    )
                })?;
                Ok(auth_data(&binding))
            }
            _ => Err(CommandError::new(
                "InvalidInput",
                "An auth action is required",
            )),
        },
        Some(("api", api)) => {
            // Local input, origin, custom header, and query checks precede ALL host effects.
            let prepared = fabric_cli_api::prepare(api)?;
            let binding = selected(host)?;
            let clock = host.clock();
            let client =
                HttpClient::new(host.credential(binding)?, host.transport()?, clock.clone());
            let context = OperationContext {
                deadline: Some(clock.now() + Duration::from_secs(240)),
                cancellation: cancellation.clone(),
            };
            let initial = client
                .execute(prepared.request, prepared.audience, &context)
                .await?;
            // Capture only safe evidence; raw/async must not reject invalid operation handles.
            let recovery = match fabric_core::operation_handle(
                prepared.audience,
                initial.status,
                &initial.headers,
            ) {
                Ok(handle) => handle.map(|handle| handle.recovery()),
                Err(error) => error.recovery,
            };
            let attach_recovery = |mut error: CommandError| {
                if error.recovery.is_none() {
                    error.recovery = recovery
                        .as_ref()
                        .and_then(|value| serde_json::to_value(value).ok());
                }
                error
            };
            let response = if prepared.asynchronous {
                initial
            } else {
                fabric_cli_api::validate_response_body(&initial.body).map_err(attach_recovery)?;
                let reconciliation = fabric_sdk_api::complete_observed(
                    &client,
                    clock.as_ref(),
                    prepared.audience,
                    initial,
                    cancellation,
                    prepared.lro_config,
                    observer,
                );
                tokio::select! {
                    biased;
                    () = observer.failed.cancelled() => {
                        return Err(attach_recovery(progress_error().into()));
                    }
                    result = reconciliation => result?,
                }
            };
            let data = fabric_cli_api::response_data(
                response,
                prepared.show_headers,
                prepared.query.as_deref(),
            )
            .map_err(attach_recovery)?;
            if !prepared.asynchronous && recovery.is_some() {
                observer
                    .send(ConsoleUpdate::Finished)
                    .await
                    .map_err(|error| attach_recovery(error.into()))?;
            }
            Ok(data)
        }
        _ => Err(CommandError::new("InvalidInput", "A command is required")),
    }
}
