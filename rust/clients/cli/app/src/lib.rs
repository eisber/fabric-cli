// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Thin injectable parser/host/output composition. Only the executable owns runtime and signals.
#![forbid(unsafe_code)]

mod selection;

pub use selection::SelectionFile;

use async_trait::async_trait;
use clap::{Arg, ArgAction, Command, error::ErrorKind as ParseErrorKind};
use fabric_cli_command_contracts::CommandError;
use fabric_core::{
    Audience, CancellationToken, Clock, HttpExecutor, HttpTransport, OperationContext,
    TokenCredential,
};
use fabric_identity::{AzureCliSession, Binding, BoundCredential, ProcessExecutor};
use fabric_sdk_http::{HttpClient, SystemClock};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

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
        .subcommand(Command::new("auth").about("Bind to the current Azure CLI identity (run az login separately)")
            .disable_help_flag(true).disable_help_subcommand(true).arg(help_arg()).subcommand_required(true)
            .subcommand(auth_leaf("login").about("Validate and bind the current Azure CLI tenant/principal"))
            .subcommand(auth_leaf("status").about("Validate the selected Azure CLI identity"))
            .subcommand(auth_leaf("logout").about("Clear Rust selection only; leave Azure CLI signed in")))
        .subcommand(fabric_cli_api::command())
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

/// Run the real parser/host/output with injected effects and streams.
///
/// Supply the executable name as the first argument, as with `std::env::args_os`.
/// Metadata returns before touching any host hook, selection, credential, or network.
pub async fn run(
    args: impl IntoIterator<Item = OsString>,
    host: &dyn Host,
    cancellation: CancellationToken,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let args: Vec<OsString> = args.into_iter().collect();
    let name = command_name(&args);
    let matches = match parser().try_get_matches_from(args) {
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
        Err(_) => {
            // Clap diagnostics can echo raw input containing header credentials. Do not copy them.
            return write_failure(
                host,
                name,
                CommandError::new(
                    "InvalidInput",
                    "Invalid command arguments. Run fab-rs --help.",
                ),
                stdout,
                stderr,
            );
        }
    };
    let result = execute(&matches, host, cancellation).await;
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

async fn execute(
    matches: &clap::ArgMatches,
    host: &dyn Host,
    cancellation: CancellationToken,
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
                Ok(json!({"selected": true}))
            }
            Some("status") => {
                let binding = selected(host)?;
                let credential = host.credential(binding)?;
                let context = OperationContext::new(Duration::from_secs(240), cancellation);
                credential.token(Audience::Fabric.scope(), &context).await?;
                Ok(json!({"selected": true}))
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
                deadline: clock.now() + Duration::from_secs(240),
                cancellation: cancellation.clone(),
            };
            let initial = client
                .execute(prepared.request, prepared.audience, &context)
                .await?;
            let response = if prepared.asynchronous {
                initial
            } else {
                fabric_sdk_api::complete(
                    &client,
                    clock.as_ref(),
                    prepared.audience,
                    initial,
                    cancellation,
                )
                .await?
            };
            fabric_cli_api::response_data(
                response,
                prepared.show_headers,
                prepared.query.as_deref(),
            )
        }
        _ => Err(CommandError::new("InvalidInput", "A command is required")),
    }
}
