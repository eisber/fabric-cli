// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in workspace read contracts, separate from the Python-compatible host.
//!
//! The caller owns identity, HTTP, clocks, cancellation and deadlines. Only exact
//! UUID-addressed primary metadata and one bounded native items page are exposed.

use clap::{Arg, ArgAction, Command, error::ErrorKind};
use fabric_cli_output::Timestamp;
use fabric_sdk_api::{
    ApiClient,
    workspace_reads::{
        WorkspaceReadError, WorkspaceReadKind, WorkspaceReadRequest, read_workspace,
    },
};
use serde::Serialize;
use serde_json::Value;
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    future::{Future, poll_fn},
    io::{self, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    pin::pin,
    sync::atomic::{AtomicU64, Ordering},
    task::Poll,
    time::Duration,
};

/// Parsed display-only output or an explicitly bounded read.
#[derive(Debug)]
pub enum ParsedWorkspaceInvocation {
    /// Help or version, without configuration, credentials or filesystem access.
    Display(String),
    /// A validated, explicit workspace read.
    Read(WorkspaceCommand),
}

/// One validated read, identical for common and workspace facade spellings.
#[derive(Clone)]
pub struct WorkspaceCommand {
    request: WorkspaceReadRequest,
    timeout: Duration,
    project: Option<String>,
    output: Option<PathBuf>,
}

impl std::fmt::Debug for WorkspaceCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceCommand")
            .field("request", &self.request)
            .field("timeout", &self.timeout)
            .field("has_projection", &self.project.is_some())
            .field("file_output", &self.output.is_some())
            .finish()
    }
}

impl WorkspaceCommand {
    /// The immutable resource-native request.
    pub fn request(&self) -> &WorkspaceReadRequest {
        &self.request
    }

    /// Total caller-enforced deadline after local validation/preflight.
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Explicit local destination; absent means stdout.
    pub fn output(&self) -> Option<&Path> {
        self.output.as_deref()
    }
}

fn read_command(name: &'static str, list: bool) -> Command {
    let mut command = Command::new(name)
        .arg(Arg::new("path").required(true))
        .arg(
            Arg::new("max-bytes")
                .long("max-bytes")
                .required(true)
                .value_parser(clap::value_parser!(NonZeroUsize)),
        )
        .arg(Arg::new("timeout").long("timeout").required(true))
        .arg(Arg::new("format").long("format").value_parser(["json"]))
        .arg(Arg::new("project").long("project").short('q'))
        .arg(
            Arg::new("output")
                .long("output")
                .short('o')
                .value_parser(clap::value_parser!(PathBuf)),
        );
    if list {
        command = command
            .arg(
                Arg::new("limit")
                    .long("limit")
                    .required(true)
                    .value_parser(clap::value_parser!(NonZeroUsize)),
            )
            .arg(
                Arg::new("recursive")
                    .long("recursive")
                    .required(true)
                    .action(ArgAction::Set)
                    .value_parser(clap::value_parser!(bool)),
            );
    }
    command
}

/// Parse the limited experimental surface without reading configuration.
///
/// All numeric budgets and list recursion are explicit; unapproved defaults,
/// names, profiles, cursor formats and other capabilities are not inferred.
pub fn parse_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<ParsedWorkspaceInvocation, WorkspaceReadError> {
    let command = Command::new("fab-cli-workspace")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Experimental v2 workspace reads; explicit UUID paths and budgets only")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(read_command("get", false))
        .subcommand(read_command("list", true))
        .subcommand(
            Command::new("artifact")
                .subcommand_required(true)
                .subcommand(
                    Command::new("workspace")
                        .subcommand_required(true)
                        .subcommand(read_command("get", false))
                        .subcommand(read_command("list", true)),
                ),
        );
    let matches = match command.try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return Ok(ParsedWorkspaceInvocation::Display(error.to_string()));
        }
        Err(_) => return Err(WorkspaceReadError::InvalidInput),
    };
    let (verb, matches) = matches
        .subcommand()
        .ok_or(WorkspaceReadError::InvalidInput)?;
    let (verb, matches) = if verb == "artifact" {
        matches
            .subcommand_matches("workspace")
            .and_then(clap::ArgMatches::subcommand)
            .ok_or(WorkspaceReadError::InvalidInput)?
    } else {
        (verb, matches)
    };
    let path = matches
        .get_one::<String>("path")
        .ok_or(WorkspaceReadError::InvalidInput)?;
    let workspace = addressed_workspace(path, verb == "list")?;
    let bytes = *matches
        .get_one::<NonZeroUsize>("max-bytes")
        .ok_or(WorkspaceReadError::InvalidInput)?;
    let request = match verb {
        "get" => WorkspaceReadRequest::metadata(workspace, bytes)?,
        "list" => WorkspaceReadRequest::items(
            workspace,
            *matches
                .get_one::<NonZeroUsize>("limit")
                .ok_or(WorkspaceReadError::InvalidInput)?,
            bytes,
            *matches
                .get_one::<bool>("recursive")
                .ok_or(WorkspaceReadError::InvalidInput)?,
        )?,
        _ => return Err(WorkspaceReadError::UnsupportedCapability),
    };
    let timeout = parse_duration(
        matches
            .get_one::<String>("timeout")
            .ok_or(WorkspaceReadError::InvalidInput)?,
    )?;
    let project = matches.get_one::<String>("project").cloned();
    if let Some(expression) = &project {
        if expression.len() > 4096 {
            return Err(WorkspaceReadError::InvalidInput);
        }
        fabric_cli_query::validate_json_projection(expression)
            .map_err(|_| WorkspaceReadError::InvalidInput)?;
    }
    let output = matches
        .get_one::<PathBuf>("output")
        .filter(|path| path.as_os_str() != "-")
        .cloned();
    Ok(ParsedWorkspaceInvocation::Read(WorkspaceCommand {
        request,
        timeout,
        project,
        output,
    }))
}

fn addressed_workspace(path: &str, list: bool) -> Result<&str, WorkspaceReadError> {
    let path = path
        .strip_prefix('/')
        .ok_or(WorkspaceReadError::InvalidInput)?;
    let resource = if list {
        path.strip_suffix("/items").unwrap_or(path)
    } else {
        path
    };
    let workspace = resource
        .strip_suffix(".Workspace")
        .ok_or(WorkspaceReadError::UnsupportedCapability)?;
    if workspace.contains(['/', '\\', '*', '?', '#', '%']) {
        return Err(WorkspaceReadError::InvalidInput);
    }
    Ok(workspace)
}

fn parse_duration(value: &str) -> Result<Duration, WorkspaceReadError> {
    let (number, factor) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1_u64)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3_600_000)
    } else {
        return Err(WorkspaceReadError::InvalidInput);
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(WorkspaceReadError::InvalidInput);
    }
    let millis = number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(factor))
        .filter(|millis| (1..=86_400_000).contains(millis))
        .ok_or(WorkspaceReadError::InvalidInput)?;
    Ok(Duration::from_millis(millis))
}

/// Serialized v2 result, retaining unprojected target, identity and coverage.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEnvelope {
    schema_version: &'static str,
    status: &'static str,
    data: Value,
    meta: WorkspaceMeta,
    error: Option<ReadDiagnostic>,
}

impl std::fmt::Debug for WorkspaceEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceEnvelope")
            .field("status", &self.status)
            .field("error_code", &self.error.as_ref().map(|error| error.code))
            .finish_non_exhaustive()
    }
}

impl WorkspaceEnvelope {
    /// Zero only for a completed, successfully represented read.
    pub fn exit_code(&self) -> u8 {
        if self.status == "succeeded" {
            0
        } else if self
            .error
            .as_ref()
            .is_some_and(|error| matches!(error.code, "InvalidInput" | "UnsupportedCapability"))
        {
            2
        } else {
            1
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceMeta {
    target: Option<ReadTarget>,
    observed_at: Option<String>,
    context: Option<ReadContext>,
    representation: &'static str,
    provenance: &'static str,
    coverage: ReadCoverage,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<ReadDelivery>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadDelivery {
    published: bool,
    cleanup_complete: bool,
    prior_error: Option<ReadDiagnostic>,
}

#[derive(Clone, Debug, Serialize)]
struct ReadTarget {
    path: String,
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    scope: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct ReadContext {
    account: String,
    tenant: String,
    audience: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct ReadCoverage {
    complete: Option<bool>,
    truncated: bool,
    continuation: Option<String>,
    resumable: bool,
}

#[derive(Clone, Debug, Serialize)]
struct ReadDiagnostic {
    code: &'static str,
    message: String,
    retryable: bool,
    hints: Vec<&'static str>,
}

fn diagnostic(error: WorkspaceReadError) -> ReadDiagnostic {
    ReadDiagnostic {
        code: error.code(),
        message: error.to_string(),
        retryable: false,
        hints: if error == WorkspaceReadError::BudgetExceeded {
            vec!["This slice has no resumable cursor; no provider token is returned."]
        } else {
            Vec::new()
        },
    }
}

fn metadata(command: Option<&WorkspaceCommand>, observed_at: Option<&str>) -> WorkspaceMeta {
    let target = command.map(|command| {
        let request = command.request();
        let items = request.kind() == WorkspaceReadKind::Items;
        ReadTarget {
            path: format!(
                "/{}.Workspace{}",
                request.workspace_id(),
                if items { "/items" } else { "" },
            ),
            id: request.workspace_id().to_owned(),
            kind: if items { "WorkspaceItems" } else { "Workspace" },
            scope: "workspace",
        }
    });
    WorkspaceMeta {
        target,
        observed_at: observed_at.map(str::to_owned),
        context: None,
        representation: if command
            .is_some_and(|command| command.request().kind() == WorkspaceReadKind::Items)
        {
            "items"
        } else {
            "primary-metadata"
        },
        provenance: "Fabric REST v1; no admin or optional enrichment requests",
        coverage: ReadCoverage {
            complete: None,
            truncated: false,
            continuation: None,
            resumable: false,
        },
        delivery: None,
    }
}

/// Construct a sanitized failure without implicit identity or clock discovery.
pub fn failure_envelope(
    command: Option<&WorkspaceCommand>,
    error: WorkspaceReadError,
    observed_at: Option<&str>,
) -> WorkspaceEnvelope {
    WorkspaceEnvelope {
        schema_version: "0.1",
        status: "failed",
        data: Value::Null,
        meta: metadata(command, observed_at),
        error: Some(diagnostic(error)),
    }
}

/// Execute with caller-injected cancellation and one total deadline.
///
/// Budget checks occur in the SDK before data-only JSON projection. This
/// function never creates a timer, opens a file or chooses a credential.
pub async fn execute(
    client: &ApiClient,
    command: &WorkspaceCommand,
    observed_at: &str,
    cancelled: impl Future<Output = ()> + Send,
    deadline: impl Future<Output = ()> + Send,
) -> WorkspaceEnvelope {
    if Timestamp::parse(observed_at).is_err() {
        return failure_envelope(Some(command), WorkspaceReadError::InvalidInput, None);
    }
    let mut cancelled = pin!(cancelled);
    let mut deadline = pin!(deadline);
    let mut operation = pin!(async {
        let result = read_workspace(client, command.request(), std::future::pending()).await?;
        let data = match &command.project {
            Some(expression) => fabric_cli_query::project_json(&result.data, expression)
                .map_err(|_| WorkspaceReadError::InvalidInput)?,
            None => result.data,
        };
        Ok::<_, WorkspaceReadError>((data, result.complete, result.truncated))
    });
    let result = poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(WorkspaceReadError::Cancelled));
        }
        if deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(WorkspaceReadError::Timeout));
        }
        match operation.as_mut().poll(cx) {
            Poll::Ready(result) => {
                if cancelled.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Err(WorkspaceReadError::Cancelled))
                } else if deadline.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Err(WorkspaceReadError::Timeout))
                } else {
                    Poll::Ready(result)
                }
            }
            Poll::Pending => Poll::Pending,
        }
    })
    .await;
    let mut envelope = match result {
        Ok((data, complete, truncated)) => {
            let mut meta = metadata(Some(command), Some(observed_at));
            meta.coverage.complete = Some(complete);
            meta.coverage.truncated = truncated;
            WorkspaceEnvelope {
                schema_version: "0.1",
                status: if complete { "succeeded" } else { "partial" },
                data,
                meta,
                error: (!complete).then(|| diagnostic(WorkspaceReadError::BudgetExceeded)),
            }
        }
        Err(error) => failure_envelope(Some(command), error, Some(observed_at)),
    };
    envelope.meta.context = Some(ReadContext {
        account: client.selection().account().to_owned(),
        tenant: client.selection().tenant().to_owned(),
        audience: "https://api.fabric.microsoft.com",
    });
    envelope
}

/// Explicit local output preflight and no-clobber reservation.
///
/// Uses a sibling temporary file and atomic hard-link publication. Filesystems
/// without hard-link support fail explicitly, never fall back to overwriting.
/// The caller must consume the reservation with [`write_result`].
pub struct OutputReservation {
    destination: Option<PathBuf>,
    temporary: Option<PathBuf>,
    file: Option<File>,
}

impl std::fmt::Debug for OutputReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputReservation")
            .field("file_output", &self.destination.is_some())
            .finish_non_exhaustive()
    }
}

impl OutputReservation {
    /// Validate destination and reserve a writable sibling before remote work.
    ///
    /// Parent directories are caller-owned stable local directories, not a
    /// hostile concurrent-filesystem sandbox. Symlink parents are rejected.
    pub fn new(destination: Option<&Path>) -> Result<Self, WorkspaceReadError> {
        let Some(destination) = destination else {
            return Ok(Self {
                destination: None,
                temporary: None,
                file: None,
            });
        };
        match fs::symlink_metadata(destination) {
            Ok(_) => return Err(WorkspaceReadError::OutputExists),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(WorkspaceReadError::OutputFailed),
        }
        let absolute = if destination.is_absolute() {
            destination.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|_| WorkspaceReadError::OutputFailed)?
                .join(destination)
        };
        if absolute.file_name().is_none()
            || absolute
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(WorkspaceReadError::InvalidInput);
        }
        let parent = absolute.parent().ok_or(WorkspaceReadError::InvalidInput)?;
        let name = absolute
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(WorkspaceReadError::InvalidInput)?;
        fabric_deploy::definitions::validate_part_path(
            name,
            fabric_deploy::definitions::DefinitionLimits::default(),
        )
        .map_err(|_| WorkspaceReadError::InvalidInput)?;
        for ancestor in parent.ancestors() {
            let metadata =
                fs::symlink_metadata(ancestor).map_err(|_| WorkspaceReadError::OutputFailed)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(WorkspaceReadError::OutputFailed);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(WorkspaceReadError::OutputFailed);
                }
            }
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..32 {
            let temporary = parent.join(format!(
                ".fab-cli-{}-{}.tmp",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => {
                    return Ok(Self {
                        destination: Some(absolute),
                        temporary: Some(temporary),
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(WorkspaceReadError::OutputFailed),
            }
        }
        Err(WorkspaceReadError::OutputFailed)
    }

    fn discard(&mut self) -> io::Result<()> {
        self.file.take();
        if let Some(temporary) = self.temporary.take() {
            fs::remove_file(temporary)?;
        }
        Ok(())
    }
}

/// An envelope serialized once, under the caller's command deadline.
///
/// The immutable bytes are delivered without a second successful serialization.
pub struct PreparedWorkspaceResult {
    envelope: WorkspaceEnvelope,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for PreparedWorkspaceResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedWorkspaceResult")
            .field("status", &self.envelope.status)
            .finish_non_exhaustive()
    }
}

impl PreparedWorkspaceResult {
    /// Serialize the complete selected result without opening files or streams.
    pub fn new(envelope: WorkspaceEnvelope) -> Result<Self, WorkspaceReadError> {
        let mut bytes =
            serde_json::to_vec(&envelope).map_err(|_| WorkspaceReadError::OutputFailed)?;
        bytes.push(b'\n');
        Ok(Self { envelope, bytes })
    }
}

fn write_envelope(stream: &mut dyn Write, envelope: &WorkspaceEnvelope) -> io::Result<()> {
    serde_json::to_writer(&mut *stream, envelope).map_err(io::Error::other)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// Prepare and deliver an envelope for callers without asynchronous rendering.
///
/// Native callers use [`PreparedWorkspaceResult`] and [`write_prepared_result`]
/// to include serialization in their command deadline.
pub fn write_result(
    mut reservation: OutputReservation,
    envelope: &WorkspaceEnvelope,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    match PreparedWorkspaceResult::new(envelope.clone()) {
        Ok(prepared) => write_prepared_result(reservation, &prepared, stdout, stderr),
        Err(error) => {
            let cleaned = reservation.discard().is_ok();
            write_envelope(stderr, &delivery_failure(envelope, error, false, cleaned))?;
            Ok(1)
        }
    }
}

/// Deliver already-rendered bytes without resubmitting or reserializing success.
///
/// Failed reads emit only stderr; partial data remains labelled and nonzero.
/// Cleanup failure is explicit secondary evidence, never a lost read diagnostic
/// or a claim that a previously published output file does not exist.
pub fn write_prepared_result(
    mut reservation: OutputReservation,
    prepared: &PreparedWorkspaceResult,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    let envelope = &prepared.envelope;
    let write = |stream: &mut dyn Write| -> io::Result<()> {
        stream.write_all(&prepared.bytes)?;
        stream.flush()
    };
    if envelope.status == "failed" {
        if reservation.discard().is_err() {
            write_envelope(
                stderr,
                &delivery_failure(envelope, WorkspaceReadError::OutputFailed, false, false),
            )?;
            return Ok(1);
        }
        write(stderr)?;
        return Ok(envelope.exit_code());
    }
    if let Some(destination) = reservation.destination.as_ref() {
        let result = (|| {
            let file = reservation
                .file
                .as_mut()
                .ok_or_else(|| io::Error::other("The local output reservation is unavailable."))?;
            write(file)?;
            file.sync_all()?;
            fs::hard_link(
                reservation.temporary.as_ref().ok_or_else(|| {
                    io::Error::other("The local output reservation is unavailable.")
                })?,
                destination,
            )
        })();
        let cleaned = reservation.discard().is_ok();
        if let Err(error) = result {
            let code = if error.kind() == io::ErrorKind::AlreadyExists {
                WorkspaceReadError::OutputExists
            } else {
                WorkspaceReadError::OutputFailed
            };
            write_envelope(stderr, &delivery_failure(envelope, code, false, cleaned))?;
            return Ok(1);
        }
        if !cleaned {
            write_envelope(
                stderr,
                &delivery_failure(envelope, WorkspaceReadError::OutputFailed, true, false),
            )?;
            return Ok(1);
        }
    } else if write(stdout).is_err() {
        write_envelope(
            stderr,
            &delivery_failure(envelope, WorkspaceReadError::OutputFailed, false, true),
        )?;
        return Ok(1);
    }
    Ok(envelope.exit_code())
}

fn delivery_failure(
    envelope: &WorkspaceEnvelope,
    error: WorkspaceReadError,
    published: bool,
    cleanup_complete: bool,
) -> WorkspaceEnvelope {
    let mut failed = envelope.clone();
    failed.status = "failed";
    failed.meta.delivery = Some(ReadDelivery {
        published,
        cleanup_complete,
        prior_error: failed.error.take(),
    });
    failed.error = Some(diagnostic(error));
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_failure_retains_the_original_read_error_in_a_diagnostic_envelope() {
        let directory = std::env::temp_dir().join(format!(
            "fabric-workspace-cleanup-test-{}",
            std::process::id(),
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("missing.json");
        let reservation = OutputReservation {
            destination: None,
            temporary: Some(path),
            file: None,
        };
        let envelope = failure_envelope(None, WorkspaceReadError::AccessDenied, None);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_ne!(
            write_result(reservation, &envelope, &mut stdout, &mut stderr).unwrap(),
            0,
        );
        assert!(stdout.is_empty());
        let result: Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(result["error"]["code"], "OutputFailed");
        assert_eq!(
            result["meta"]["delivery"]["priorError"]["code"],
            "AccessDenied"
        );
        assert_eq!(result["meta"]["delivery"]["cleanupComplete"], false);
        assert_eq!(result["meta"]["delivery"]["published"], false);
        fs::remove_dir(directory).unwrap();
    }
}
