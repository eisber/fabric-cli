// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Thin injectable parser/host/output composition. Only the executable owns runtime and signals.
#![forbid(unsafe_code)]

mod diagnostics;
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
    OperationContext, OperationKind, RecoveryInfo,
    diagnostics::{
        DiagnosticDuration, DiagnosticEvent, DiagnosticObserver, DiagnosticOutcome,
        DiagnosticPhase, DiagnosticStage,
    },
};
use fabric_identity::{AzureCliSession, Binding, BoundCredential, ProcessExecutor};
use fabric_sdk_api::{PollObserver, PollProgress};
use fabric_sdk_http::{HttpClient, SystemClock, bounded_with_clock};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{IsTerminal, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{mpsc, oneshot};

const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

struct ProgressEvent {
    progress: ConsoleUpdate,
    written: oneshot::Sender<Result<(), CoreError>>,
}

enum ConsoleUpdate {
    Progress(ProgressUpdate),
    Diagnostic(Box<DiagnosticEvent>, Duration),
}

enum ProgressUpdate {
    Started(OperationKind),
    Polling(OperationKind),
    Status(PollProgress),
}

struct ConsoleProgress {
    events: mpsc::Sender<ProgressEvent>,
    failed: CancellationToken,
    broken: AtomicBool,
    timing: Option<(Arc<dyn Clock>, Instant)>,
    cancellation: CancellationToken,
    reporting: Mutex<ReportingState>,
}

struct ReportingState {
    outcome: DiagnosticOutcome,
    recovery: Option<RecoveryInfo>,
    completed: bool,
}
impl ConsoleProgress {
    async fn send(&self, progress: ProgressUpdate) -> Result<(), CoreError> {
        self.send_update(ConsoleUpdate::Progress(progress)).await
    }

    async fn send_update(&self, progress: ConsoleUpdate) -> Result<(), CoreError> {
        let diagnostic = matches!(&progress, ConsoleUpdate::Diagnostic(..));
        let error = || {
            if diagnostic {
                diagnostics::error()
            } else {
                progress_error()
            }
        };
        if self.broken.load(Ordering::Relaxed) {
            return Err(error());
        }
        let (written, received) = oneshot::channel();
        self.events
            .send(ProgressEvent { progress, written })
            .await
            .map_err(|_| error())?;
        received.await.map_err(|_| error())?
    }

    fn elapsed(&self) -> Duration {
        self.timing
            .as_ref()
            .map_or(Duration::ZERO, |(clock, start)| {
                clock.now().saturating_duration_since(*start)
            })
    }

    async fn emit(&self, event: DiagnosticEvent) -> Result<(), CoreError> {
        if self.timing.is_none() {
            return Ok(());
        }
        self.send_update(ConsoleUpdate::Diagnostic(Box::new(event), self.elapsed()))
            .await
    }

    async fn stage(
        &self,
        stage: DiagnosticStage,
        phase: DiagnosticPhase,
        duration: Duration,
    ) -> Result<(), CommandError> {
        let Some((clock, _)) = &self.timing else {
            return Ok(());
        };
        let context = OperationContext {
            deadline: None,
            cancellation: self.cancellation.clone(),
        };
        bounded_with_clock(
            clock.as_ref(),
            &context,
            self.emit(DiagnosticEvent::Stage {
                stage,
                phase,
                duration: DiagnosticDuration::from_duration(duration),
            }),
        )
        .await?
        .map_err(|error| {
            if error.kind == ErrorKind::Diagnostic {
                self.reporting_error()
            } else {
                error.into()
            }
        })
    }

    async fn start(&self, stage: DiagnosticStage) -> Result<Duration, CommandError> {
        let started = self.elapsed();
        self.stage(stage, DiagnosticPhase::Started, Duration::ZERO)
            .await?;
        Ok(started)
    }

    async fn finish<T>(
        &self,
        stage: DiagnosticStage,
        started: Duration,
        result: Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        let phase = result.as_ref().map_or_else(
            |error| DiagnosticPhase::Failed(error.kind()),
            |_| DiagnosticPhase::Completed,
        );
        self.stage(stage, phase, self.elapsed().saturating_sub(started))
            .await?;
        result
    }

    fn remember(&self, event: &DiagnosticEvent, written: bool) {
        let mut state = self
            .reporting
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match event {
            DiagnosticEvent::HttpSend { .. } if written => {
                if !matches!(state.outcome, DiagnosticOutcome::Terminal(_)) {
                    state.outcome = DiagnosticOutcome::Uncertain;
                }
            }
            DiagnosticEvent::HttpResponse {
                status: Some(status),
                ..
            }
            | DiagnosticEvent::HttpFailure {
                status: Some(status),
                ..
            } => {
                if !matches!(state.outcome, DiagnosticOutcome::Terminal(_)) {
                    state.outcome = DiagnosticOutcome::Received(*status);
                }
            }
            DiagnosticEvent::PollCompleted { outcome } => {
                state.outcome = DiagnosticOutcome::Terminal(*outcome);
            }
            _ => {}
        }
    }

    fn reporting_error(&self) -> CommandError {
        let state = self
            .reporting
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut error: CommandError = CoreError::diagnostic_failure(state.outcome).into();
        if state.completed {
            error.message = format!("Command work completed. {}", error.message);
        }
        error.recovery = state
            .recovery
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok());
        error
    }
}

#[async_trait]
impl DiagnosticObserver for ConsoleProgress {
    async fn observe(&self, event: DiagnosticEvent) -> Result<(), CoreError> {
        self.emit(event).await
    }
}

#[async_trait]
impl PollObserver for ConsoleProgress {
    async fn started(&self, kind: OperationKind) -> Result<(), CoreError> {
        self.send(ProgressUpdate::Started(kind)).await
    }

    async fn polling(&self, kind: OperationKind) -> Result<(), CoreError> {
        self.send(ProgressUpdate::Polling(kind)).await
    }

    async fn observe(&self, progress: PollProgress) -> Result<(), CoreError> {
        self.send(ProgressUpdate::Status(progress)).await
    }

    async fn diagnostic(&self, event: DiagnosticEvent) -> Result<(), CoreError> {
        self.emit(event).await
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
    requesting: bool,
}

impl ProgressDisplay {
    fn update(
        &mut self,
        update: ProgressUpdate,
        color: bool,
        stderr: &mut dyn Write,
    ) -> Result<(), CoreError> {
        let (message, tone) = match update {
            ProgressUpdate::Started(kind) => {
                self.started = Some((kind, tokio::time::Instant::now()));
                (
                    format!("Monitoring {} execution...", progress_label(kind)),
                    "36",
                )
            }
            ProgressUpdate::Polling(kind) => {
                self.requesting = true;
                (format!("Checking {} status...", progress_label(kind)), "36")
            }
            ProgressUpdate::Status(progress) => {
                self.latest = Some(progress);
                self.requesting = false;
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
        };
        progress_line(&message, color, tone, stderr)
    }

    fn heartbeat(&self, color: bool, stderr: &mut dyn Write) -> Result<(), CoreError> {
        let (kind, started) = self.started.ok_or_else(progress_error)?;
        let label = progress_label(kind);
        let waiting = if self.latest.is_some_and(|progress| {
            matches!(progress.status(), "Succeeded" | "Completed" | "Deduped")
        }) {
            format!("Waiting for {label} result")
        } else if self.requesting {
            format!("Waiting for {label} status response")
        } else {
            format!("Waiting for next {label} status check")
        };
        let last = self.latest.map_or_else(
            || "; no status received yet".into(),
            |progress| format!("; last status: {}", status_text(progress)),
        );
        progress_line(
            &format!(
                "{waiting}{last} ({}s elapsed)...",
                started.elapsed().as_secs()
            ),
            color,
            "36",
            stderr,
        )
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
        std::io::stderr().is_terminal()
            && std::env::var_os("NO_COLOR").is_none()
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
        .arg(Arg::new("verbose").long("verbose").global(true).action(ArgAction::SetTrue)
            .help("Print privacy-preserving debug diagnostics to stderr"))
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
    let mut command = args.iter().skip(1).filter(|arg| *arg != "--verbose");
    if command.next().is_some_and(|arg| arg == "auth") && command.next().is_none() {
        return parser()
            .try_get_matches_from(args.iter().cloned().chain([OsString::from("--help")]));
    }
    parser().try_get_matches_from(args)
}

fn command_name(args: &[OsString]) -> &'static str {
    let mut command = args.iter().skip(1).skip_while(|arg| *arg == "--verbose");
    let first = command.next().and_then(|arg| arg.to_str());
    let second = command
        .skip_while(|arg| *arg == "--verbose")
        .next()
        .and_then(|arg| arg.to_str());
    match (first, second) {
        (Some("api"), _) => "api",
        (Some("auth"), Some("login")) => "auth login",
        (Some("auth"), Some("status")) => "auth status",
        (Some("auth"), Some("logout")) => "auth logout",
        (Some("auth"), _) => "auth",
        _ => "",
    }
}

fn parsed_command_name(matches: &clap::ArgMatches) -> &'static str {
    match matches.subcommand() {
        Some(("api", _)) => "api",
        Some(("auth", auth)) => match auth.subcommand_name() {
            Some("login") => "auth login",
            Some("status") => "auth status",
            Some("logout") => "auth logout",
            _ => "auth",
        },
        _ => "",
    }
}

fn welcome_requested(args: &[OsString]) -> bool {
    args.len() == 1 || (args.len() == 2 && args[1] == "--verbose")
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
    if welcome_requested(&args) {
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
    let name = parsed_command_name(&matches);
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
            execute(&matches, host, cancellation, &observer).await
        };
        let heartbeat = tokio::time::sleep(PROGRESS_INTERVAL);
        let mut display = ProgressDisplay::default();
        tokio::pin!(execution);
        tokio::pin!(heartbeat);
        loop {
            tokio::select! {
                biased;
                result = &mut execution => break result,
                Some(event) = events.recv() => {
                    let result = match event.progress {
                        ConsoleUpdate::Progress(update) => {
                            let result = display.update(update, host.progress_color(), stderr);
                            heartbeat.as_mut().reset(tokio::time::Instant::now() + PROGRESS_INTERVAL);
                            result
                        }
                        ConsoleUpdate::Diagnostic(event, elapsed) => {
                            observer.remember(&event, false);
                            let result = diagnostics::write(&event, elapsed, host.progress_color(), stderr);
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
                () = &mut heartbeat, if display.started.is_some() && !observer.failed.is_cancelled() && !observer.broken.load(Ordering::Relaxed) => {
                    if display.heartbeat(host.progress_color(), stderr).is_err() {
                        if observer.timing.is_some() {
                            observer.broken.store(true, Ordering::Relaxed);
                        }
                        observer.failed.cancel();
                    }
                    heartbeat.as_mut().reset(tokio::time::Instant::now() + PROGRESS_INTERVAL);
                }
            }
        }
    };
    if observer.timing.is_some() && !observer.broken.load(Ordering::Relaxed) {
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
        if diagnostics::write(&event, elapsed, host.progress_color(), stderr).is_err() {
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
    observer: &Arc<ConsoleProgress>,
) -> Result<Value, CommandError> {
    match matches.subcommand() {
        Some(("auth", auth)) => match auth.subcommand_name() {
            Some("logout") => {
                let started = observer.start(DiagnosticStage::Selection).await?;
                observer
                    .finish(DiagnosticStage::Selection, started, host.clear_selection())
                    .await?;
                Ok(json!({"selected": false}))
            }
            Some("login") => {
                let started = observer.start(DiagnosticStage::Authentication).await?;
                let context = OperationContext::new(Duration::from_secs(240), cancellation);
                let binding = observer
                    .finish(
                        DiagnosticStage::Authentication,
                        started,
                        host.login(&context).await,
                    )
                    .await?;
                let started = observer.start(DiagnosticStage::Selection).await?;
                observer
                    .finish(
                        DiagnosticStage::Selection,
                        started,
                        host.save_selection(&binding),
                    )
                    .await?;
                let mut data = auth_data(&binding);
                data["selected"] = json!(true);
                data["mfa_evidence"] = json!("not_checked");
                data["service_access_verified"] = json!(false);
                Ok(data)
            }
            Some("status") => {
                let started = observer.start(DiagnosticStage::Selection).await?;
                let binding = observer
                    .finish(DiagnosticStage::Selection, started, selected(host))
                    .await?;
                let started = observer.start(DiagnosticStage::Authentication).await?;
                let credential = host.credential(binding.clone())?;
                let clock = host.clock();
                let context = OperationContext {
                    deadline: Some(clock.now() + Duration::from_secs(240)),
                    cancellation,
                };
                let result = bounded_with_clock(
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
                });
                observer
                    .finish(DiagnosticStage::Authentication, started, result)
                    .await?;
                Ok(auth_data(&binding))
            }
            _ => Err(CommandError::new(
                "InvalidInput",
                "An auth action is required",
            )),
        },
        Some(("api", api)) => {
            // Validate local inputs before identity, configuration, or transport effects.
            let started = observer.start(DiagnosticStage::Prepare).await?;
            let prepared = observer
                .finish(
                    DiagnosticStage::Prepare,
                    started,
                    fabric_cli_api::prepare(api),
                )
                .await?;
            let started = observer.start(DiagnosticStage::Selection).await?;
            let binding = observer
                .finish(DiagnosticStage::Selection, started, selected(host))
                .await?;
            let clock = host.clock();
            let started = observer.start(DiagnosticStage::Authentication).await?;
            let credential = observer
                .finish(
                    DiagnosticStage::Authentication,
                    started,
                    host.credential(binding),
                )
                .await?;
            let mut client = HttpClient::new(credential, host.transport()?, clock.clone());
            if observer.timing.is_some() {
                client = client.with_diagnostics(observer.clone());
            }
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
            observer
                .reporting
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .recovery = recovery.clone();
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
                    observer.as_ref(),
                );
                tokio::select! {
                    biased;
                    () = observer.failed.cancelled() => {
                        return Err(attach_recovery(progress_error().into()));
                    }
                    result = reconciliation => result?,
                }
            };
            let started = observer.start(DiagnosticStage::Projection).await?;
            let result = fabric_cli_api::response_data(
                response,
                prepared.show_headers,
                prepared.query.as_deref(),
            )
            .map_err(attach_recovery);
            observer
                .finish(DiagnosticStage::Projection, started, result)
                .await
        }
        _ => Err(CommandError::new("InvalidInput", "A command is required")),
    }
}
