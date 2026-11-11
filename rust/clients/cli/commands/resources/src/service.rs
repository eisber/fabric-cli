// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Grouped service schema and injected Domain sync consumer; v1 is unchanged.

use clap::{Arg, Command};
pub use fabric_sdk_catalog::service;
use fabric_sdk_catalog::service::{
    DomainTarget, InputError, SyncError, SyncReport, synchronize,
    v2::{ApprovalPolicy, Outcome, PreparedOperation, Registry},
};
use service::v2;
use std::{
    ffi::OsString,
    fmt,
    future::Future,
    io::{self, Write},
    pin::Pin,
};

/// Parsed grouped operation; no credential, config or network work is performed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncCommand {
    /// Exact explicit path supplied to the caller's typed resolver.
    pub path: String,
    /// Validated supported native role; no generic ACL meaning is implied.
    pub role: String,
}

/// Own subtree for a separately composed v2 head, not a v1 parser replacement.
pub fn service_command() -> Command {
    Command::new("service")
        .about("Explicit native service operations, not raw HTTP or artifact jobs")
        .subcommand_required(true)
        .disable_help_subcommand(true)
        .subcommand(
            Command::new("domain")
                .subcommand_required(true)
                .disable_help_subcommand(true)
                .subcommand(
                    Command::new("sync")
                        .about("Synchronize Domain Contributors to subdomains; requires Fabric admin permission")
                        .arg(Arg::new("path").required(true).value_name("PATH"))
                        .arg(
                            Arg::new("role")
                                .long("role")
                                .required(true)
                                .value_parser(["Contributor"]),
                        ),
                ),
        )
}

/// Isolated executable schema for embedded consumers and offline validation.
pub fn command() -> Command {
    Command::new("fab-cli")
        .subcommand_required(true)
        .subcommand(service_command())
}

/// Parse selected grouped syntax, rejecting unsupported roles/verbs before I/O.
pub fn parse_args<I, T>(args: I) -> Result<SyncCommand, CommandError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = command()
        .try_get_matches_from(args)
        .map_err(|_| CommandError::Arguments)?;
    let leaf = matches
        .subcommand_matches("service")
        .and_then(|service| service.subcommand_matches("domain"))
        .and_then(|domain| domain.subcommand_matches("sync"))
        .ok_or(CommandError::Arguments)?;
    let path = leaf
        .get_one::<String>("path")
        .filter(|s| !s.trim().is_empty())
        .ok_or(CommandError::Arguments)?
        .clone();
    let role = leaf
        .get_one::<String>("role")
        .ok_or(CommandError::Arguments)?
        .clone();
    Ok(SyncCommand { path, role })
}

/// Caller-runtime future; this library installs no tasks, timer or runtime.
pub type ResolveFuture<'a, E> = Pin<Box<dyn Future<Output = Result<DomainTarget, E>> + Send + 'a>>;

/// Existing typed-path host supplies exact service identity without tenant fallback.
pub trait DomainResolver {
    /// Original resolution error, retained rather than interpreted as absence.
    type Error;
    /// Resolve one explicit Domain path and native UUID under the selected tenant.
    fn resolve<'a>(&'a mut self, path: &'a str) -> ResolveFuture<'a, Self::Error>;
}

/// Local failures, before the native adapter can send an action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandError {
    /// Unsupported spelling, role, missing path or option.
    Arguments,
    /// SDK owner/role/registration validation failed.
    Input(InputError),
}
impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid service Domain sync request; use service domain sync --help")
    }
}
impl std::error::Error for CommandError {}

/// Original resolver, approval or native adapter failures, without silent defaults.
#[derive(Debug)]
pub enum ExecuteError<R, P> {
    /// Original typed resolver failure.
    Resolution(R),
    /// Local role/owner/registry rejection.
    Input(InputError),
    /// Immediate exact-effect approval was refused.
    Approval(P),
    /// SDK local execution or receipt-construction failure.
    Native(SyncError),
}

/// Resolve/validate a request without authentication or action dispatch.
pub async fn prepare<R: DomainResolver>(
    command: &SyncCommand,
    registry: &Registry,
    resolver: &mut R,
) -> Result<PreparedOperation, ExecuteError<R::Error, std::convert::Infallible>> {
    if command.role != "Contributor" {
        return Err(ExecuteError::Input(InputError::Role));
    }
    let target = resolver
        .resolve(&command.path)
        .await
        .map_err(ExecuteError::Resolution)?;
    service::prepare(registry, &target, &command.role).map_err(ExecuteError::Input)
}

/// Authorize and execute this prepared operation over caller-owned native seams.
///
/// Approval is exact and immediate. The caller's cancellation future includes
/// its existing overall deadline; no force flag, remote cancellation or retries.
pub async fn execute<P: ApprovalPolicy>(
    prepared: &PreparedOperation,
    policy: &mut P,
    context: &service::RequestContext,
    executor: impl service::OperationExecutor,
    limits: service::OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<SyncReport, ExecuteError<std::convert::Infallible, P::Error>> {
    let approved = prepared.authorize(policy).map_err(ExecuteError::Approval)?;
    synchronize(context, &approved, executor, limits, cancelled)
        .await
        .map_err(ExecuteError::Native)
}

/// Write only the neutral receipt to the appropriate shared stream.
///
/// Output failure retains the complete report; native response/error bytes are
/// available separately and never dumped to the terminal implicitly.
pub fn write_report(
    report: SyncReport,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<(), Box<OutputError>> {
    let writer: &mut dyn Write = if matches!(report.receipt.outcome(), Outcome::Succeeded { .. }) {
        stdout
    } else {
        stderr
    };
    serde_json::to_writer(&mut *writer, &report.receipt)
        .map_err(io::Error::other)
        .and_then(|()| writeln!(writer))
        .and_then(|()| writer.flush())
        .map_err(|cause| Box::new(OutputError { cause, report }))
}

/// Failed presentation does not discard remote mutation evidence.
pub struct OutputError {
    /// Original writer failure.
    pub cause: io::Error,
    /// Original executed report, including response-loss or permission evidence.
    pub report: SyncReport,
}
impl fmt::Debug for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutputError")
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}
impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Domain sync receipt output failed; retain the executed report")
    }
}
impl std::error::Error for OutputError {}

/// Zero only for documented action completion, never denied/unknown/partial work.
pub fn exit_code(report: &SyncReport) -> u8 {
    u8::from(!matches!(
        report.receipt.outcome(),
        v2::Outcome::Succeeded { .. }
    ))
}
