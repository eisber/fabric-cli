// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Thin injectable parser/host/output composition. Only the executable owns runtime and signals.
#![forbid(unsafe_code)]

pub mod lifecycle;
mod resources;
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
    OperationContext,
};
use fabric_identity::{AzureCliSession, Binding, BoundCredential, ProcessExecutor};
use fabric_sdk_api::{PollObserver, PollProgress};
use fabric_sdk_http::{HttpClient, SystemClock, bounded_with_clock};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{BufRead, IsTerminal, Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{mpsc, oneshot};

struct ProgressEvent {
    progress: PollProgress,
    written: oneshot::Sender<Result<(), CoreError>>,
}

struct ConsoleProgress(mpsc::Sender<ProgressEvent>);

struct ConfirmationEvent {
    prompt: String,
    answered: oneshot::Sender<Result<bool, CommandError>>,
}

struct ConsoleConfirmation(mpsc::Sender<ConfirmationEvent>);

impl ConsoleConfirmation {
    async fn confirm(&self, prompt: String) -> Result<(), CommandError> {
        let (answered, received) = oneshot::channel();
        self.0
            .send(ConfirmationEvent { prompt, answered })
            .await
            .map_err(|_| CommandError::new("Cancelled", "Confirmation unavailable"))?;
        if received
            .await
            .map_err(|_| CommandError::new("Cancelled", "Confirmation unavailable"))??
        {
            Ok(())
        } else {
            Err(CommandError::new("Cancelled", "Deletion cancelled"))
        }
    }
}
#[async_trait]
impl PollObserver for ConsoleProgress {
    async fn observe(&self, progress: PollProgress) -> Result<(), CoreError> {
        let (written, received) = oneshot::channel();
        self.0
            .send(ProgressEvent { progress, written })
            .await
            .map_err(|_| progress_error())?;
        received.await.map_err(|_| progress_error())?
    }
}

fn progress_error() -> CoreError {
    CoreError::new(ErrorKind::Transport, "Cannot write polling progress")
}

fn confirmation_answer(line: &[u8]) -> Result<bool, CommandError> {
    if line.is_empty() {
        return Err(CommandError::new(
            "Cancelled",
            "Confirmation ended without input",
        ));
    }
    if line.len() > 1025
        || !line.ends_with(b"\n")
        || line
            .iter()
            .any(|byte| byte.is_ascii_control() && !matches!(*byte, b'\r' | b'\n'))
    {
        return Err(CommandError::new(
            "InvalidInput",
            "Invalid or oversized confirmation",
        ));
    }
    let text = std::str::from_utf8(line)
        .map_err(|_| CommandError::new("InvalidInput", "Invalid confirmation"))?;
    match text.trim().to_ascii_lowercase().as_str() {
        "" | "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Err(CommandError::new("InvalidInput", "Invalid confirmation")),
    }
}

fn write_progress(progress: PollProgress, stderr: &mut dyn Write) -> Result<(), CoreError> {
    let label = if progress.is_job() {
        "Job instance status"
    } else {
        "Operation status"
    };
    let result = if let Some(percentage) = progress.percentage_complete() {
        writeln!(stderr, "{label}: {} ({percentage}%)", progress.status())
    } else {
        writeln!(stderr, "{label}: {}", progress.status())
    };
    result
        .and_then(|()| stderr.flush())
        .map_err(|_| progress_error())
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
    /// Obtain a single bounded interactive confirmation; never prompt in the engine.
    async fn confirm(&self, _cancellation: CancellationToken) -> Result<bool, CommandError> {
        Err(CommandError::new(
            "InvalidInput",
            "Interactive confirmation is unavailable; use --force",
        ))
    }
    /// Drain safe static diagnostics for the host to write to stderr, never JSON stdout.
    fn take_diagnostics(&self) -> Vec<&'static str> {
        Vec::new()
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
    async fn confirm(&self, cancellation: CancellationToken) -> Result<bool, CommandError> {
        if !std::io::stdin().is_terminal() {
            return Err(CommandError::new(
                "InvalidInput",
                "Interactive confirmation requires a terminal; use --force",
            ));
        }
        let (sender, receiver) = oneshot::channel();
        std::thread::Builder::new()
            .spawn(move || {
                let stdin = std::io::stdin();
                let mut reader = std::io::BufReader::new(stdin.lock().take(1026));
                let mut line = Vec::new();
                let result = reader.read_until(b'\n', &mut line);
                let answer = match result {
                    Ok(_) => confirmation_answer(&line),
                    Err(_) => Err(CommandError::new(
                        "InvalidInput",
                        "Cannot read confirmation",
                    )),
                };
                let _ = sender.send(answer);
            })
            .map_err(|_| CommandError::new("InvalidInput", "Cannot start confirmation reader"))?;
        tokio::select! {
            _ = cancellation.cancelled() => Err(CommandError::new("Cancelled", "Confirmation cancelled")),
            result = receiver => result.map_err(|_| CommandError::new("InvalidInput", "Cannot read confirmation"))?,
        }
    }
    fn take_diagnostics(&self) -> Vec<&'static str> {
        std::mem::take(
            &mut *self
                .diagnostics
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
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
        .subcommand(resources::command("workspace"))
        .subcommand(resources::command("item"))
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
        (Some("workspace"), Some("list")) => "workspace list",
        (Some("workspace"), Some("get")) => "workspace get",
        (Some("workspace"), Some("create")) => "workspace create",
        (Some("workspace"), Some("update")) => "workspace update",
        (Some("workspace"), Some("delete")) => "workspace delete",
        (Some("item"), Some("list")) => "item list",
        (Some("item"), Some("get")) => "item get",
        (Some("item"), Some("create")) => "item create",
        (Some("item"), Some("update")) => "item update",
        (Some("item"), Some("delete")) => "item delete",
        _ => "",
    }
}

fn argument_error_message(args: &[OsString], error: &clap::Error) -> String {
    let invalid_args = match error.get(ContextKind::InvalidArg) {
        Some(ContextValue::String(arg)) => std::slice::from_ref(arg),
        Some(ContextValue::Strings(args)) => args.as_slice(),
        _ => &[],
    };
    if command_name(args) == "api"
        && error.kind() == ParseErrorKind::InvalidValue
        && invalid_args == ["--method <method>"]
    {
        return "Invalid HTTP method for --method. Supported values are lowercase: get, post, delete, put, patch. For example, use post instead of POST.".to_owned();
    }

    let mut definition = parser();
    definition.build();
    let mut command = &definition;
    let mut path = String::from("fab-rs");
    for arg in args.iter().skip(1) {
        let Some(subcommand) = arg.to_str().and_then(|name| command.find_subcommand(name)) else {
            break;
        };
        command = subcommand;
        path.push(' ');
        path.push_str(command.get_name());
    }
    // Error contexts may contain raw input. Emit only labels from our static command schema.
    let labels: Vec<String> = invalid_args
        .iter()
        .filter_map(|label| {
            command
                .get_arguments()
                .map(ToString::to_string)
                .find(|known| known == label)
        })
        .collect();
    let detail = match error.kind() {
        ParseErrorKind::MissingRequiredArgument if !labels.is_empty() => {
            format!("Missing required arguments: {}.", labels.join(", "))
        }
        ParseErrorKind::MissingSubcommand | ParseErrorKind::InvalidSubcommand => {
            let available = command
                .get_subcommands()
                .map(Command::get_name)
                .collect::<Vec<_>>()
                .join(", ");
            let problem = if error.kind() == ParseErrorKind::MissingSubcommand {
                "Missing subcommand"
            } else {
                "Unrecognized subcommand"
            };
            format!("{problem}. Available subcommands: {available}.")
        }
        ParseErrorKind::InvalidValue
        | ParseErrorKind::ValueValidation
        | ParseErrorKind::InvalidUtf8
        | ParseErrorKind::TooFewValues
        | ParseErrorKind::WrongNumberOfValues
            if !labels.is_empty() =>
        {
            let problem = if matches!(
                error.get(ContextKind::InvalidValue),
                Some(ContextValue::String(value)) if value.is_empty()
            ) {
                "A value is required for"
            } else {
                "Invalid value for"
            };
            format!("{problem} {}.", labels.join(", "))
        }
        ParseErrorKind::ArgumentConflict if !labels.is_empty() => format!(
            "Argument {} cannot be repeated or combined with a conflicting argument.",
            labels.join(", ")
        ),
        ParseErrorKind::UnknownArgument | ParseErrorKind::TooManyValues => {
            "Unexpected argument.".to_owned()
        }
        _ => "Invalid command arguments.".to_owned(),
    };
    format!("{detail} Run {path} --help.")
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
            return write_failure(
                host,
                name,
                CommandError::new("InvalidInput", argument_error_message(&args, &error)),
                stdout,
                stderr,
            );
        }
    };
    let (sender, mut events) = mpsc::channel(1);
    let observer = ConsoleProgress(sender);
    let (sender, mut confirmations) = mpsc::channel::<ConfirmationEvent>(1);
    let confirmer = ConsoleConfirmation(sender);
    let confirm_cancellation = cancellation.clone();
    let result = {
        let execution = execute(&matches, host, cancellation, &observer, &confirmer);
        tokio::pin!(execution);
        loop {
            tokio::select! {
                Some(event) = events.recv() => {
                    let result = write_progress(event.progress, stderr);
                    // Cancellation can drop the observer while its event is queued.
                    let _ = event.written.send(result);
                }
                Some(event) = confirmations.recv() => {
                    let answer = if write!(stderr, "{}", event.prompt).and_then(|()| stderr.flush()).is_err() {
                        Err(CommandError::new("InvalidInput", "Cannot write confirmation prompt"))
                    } else {
                        host.confirm(confirm_cancellation.clone()).await
                    };
                    let _ = event.answered.send(answer);
                }
                result = &mut execution => break result,
            }
        }
    };
    for diagnostic in host.take_diagnostics() {
        let _ = writeln!(stderr, "{diagnostic}");
    }
    match result {
        Ok(data) => {
            let output = if name.starts_with("workspace ") || name.starts_with("item ") {
                fabric_cli_output::resource_success(&timestamp(host), name, data)
            } else {
                fabric_cli_output::success(&timestamp(host), name, data)
            };
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
        "selected": true,
        "logged_in": true,
        "authentication_mode": "Azure CLI",
        "tenant_id": binding.tenant,
        "principal_id": binding.principal,
        "mfa_evidence": "not_checked",
        "service_access_verified": false,
    })
}

async fn execute(
    matches: &clap::ArgMatches,
    host: &dyn Host,
    cancellation: CancellationToken,
    observer: &dyn PollObserver,
    confirmer: &ConsoleConfirmation,
) -> Result<Value, CommandError> {
    match matches.subcommand() {
        Some((kind @ ("workspace" | "item"), args)) => {
            let kind = if kind == "workspace" {
                "workspace"
            } else {
                "item"
            };
            let prepared = resources::prepare(kind, args)?;
            let binding = selected(host)?;
            let clock = host.clock();
            let client =
                HttpClient::new(host.credential(binding)?, host.transport()?, clock.clone());
            let context = OperationContext {
                deadline: Some(clock.now() + Duration::from_secs(240)),
                cancellation: cancellation.clone(),
            };
            resources::execute(
                prepared,
                &client,
                clock.as_ref(),
                &context,
                observer,
                confirmer,
            )
            .await
        }

        Some(("auth", auth)) => match auth.subcommand_name() {
            Some("logout") => {
                host.clear_selection()?;
                Ok(json!({"selected": false}))
            }
            Some("login") => {
                let context = OperationContext::new(Duration::from_secs(240), cancellation);
                let binding = host.login(&context).await?;
                host.save_selection(&binding)?;
                Ok(auth_data(&binding))
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
                fabric_sdk_api::complete_observed(
                    &client,
                    clock.as_ref(),
                    prepared.audience,
                    initial,
                    cancellation,
                    prepared.lro_config,
                    observer,
                )
                .await?
            };
            fabric_cli_api::response_data(
                response,
                prepared.show_headers,
                prepared.query.as_deref(),
            )
            .map_err(attach_recovery)
        }
        _ => Err(CommandError::new("InvalidInput", "A command is required")),
    }
}

#[cfg(test)]
mod tests {
    use super::confirmation_answer;

    #[test]
    fn confirmation_fails_closed() {
        for line in [b"\n".as_slice(), b"y\n", b"YES\r\n"] {
            assert!(confirmation_answer(line).unwrap());
        }
        for line in [b"n\n".as_slice(), b"NO\n"] {
            assert!(!confirmation_answer(line).unwrap());
        }
        for line in [b"".as_slice(), b"yes", b"maybe\n", b"\xff\n", b"\t\n"] {
            assert!(confirmation_answer(line).is_err());
        }
        assert!(confirmation_answer(&[b'x'; 1026]).is_err());
    }
}
