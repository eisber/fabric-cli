// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in `fab-cli-discovery` preview, independent of the v1 parser and dispatcher.
//!
//! Operation descriptors come exclusively from the neutral SDK registry. A
//! lexical target match establishes neither existence nor authorization. This
//! head never executes a mutation, job, updater, REPL or batch command, and is
//! not an MCP server. Mutation crates remain in this package's dependency graph;
//! isolation here is a dispatch boundary, not a claim that those crates vanish.
//!
//! Bare `schema` means backend data metadata, not operation input or readable
//! `docs/schema`. Only typed Lakehouse `Tables` paths are admitted in this slice.
//! Embedders inject [`DataSchemaProvider`]; the standalone binary supplies none
//! and does not initialize settings, identity, transport or an async runtime.
//! [`DeltaDataSchemaProvider`] reuses the existing resolver and TABLE SDK.

use clap::{Arg, ArgAction, ArgMatches, Command, error::ErrorKind};
use fabric_cli_output::CommandError;
use fabric_cli_tables::TableResolver;
use fabric_sdk_discovery::help::TargetHelp;
use fabric_sdk_discovery::{
    DiscoveryError, MAX_TARGET_BYTES, Operation, OperationContract, TargetKind, TargetSelector,
    discover, discover_for_facade, registrations,
};
use fabric_sdk_onelake::{OneLakeError, OneLakeExecutionError};
use fabric_sdk_tables::{
    TableClient, TableError, TableErrorKind, TableExecutor, TableLimits,
    schema_document::DataSchemaDocument,
};
use serde::Serialize;
use std::{
    ffi::OsString,
    fmt,
    future::{Future, pending, poll_fn},
    io::{self, Write},
    num::NonZeroUsize,
    pin::{Pin, pin},
    task::Poll,
};

mod help;

/// Maximum serialized JSON bytes, including newline, for a local descriptor.
///
/// Data documents additionally use the provider's (possibly smaller) output
/// bound. Oversized documents fail before any success bytes are written;
/// neither descriptors nor data are clipped.
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

const BOUNDARIES: &str = "Discovery preview; no mutation execution; no MCP server; live data host not configured. \
    Operation --schema reads no payload, files, stdin, settings or credentials. \
    A known contract is not resolved identity or permission. Bare schema is backend data metadata, \
    not operation input or docs/schema. Mutation crates remain package dependencies but cannot dispatch in this head.";
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (discovery preview; no mutation execution; no MCP server; live data host not configured)"
);

/// A borrowed future running only on the embedding caller's runtime.
pub type DiscoveryFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Failure with original resolver/SDK evidence retained for embedding callers.
///
/// Display and the JSON diagnostic intentionally omit supplied arguments, paths,
/// service bodies and resolver messages. Inspect the typed variants explicitly
/// when a trusted embedding application needs their original classification.
#[derive(Debug)]
pub enum DiscoveryFailure {
    /// Invalid command syntax, including missing paths and unknown flags.
    InvalidArguments,
    /// A mutation spelling was used without the discovery-only `--schema` flag.
    ExecutionNotImplemented,
    /// No backend metadata provider was injected.
    NotConfigured,
    /// Backend, owner type, facet or resolved address is outside this slice.
    Unsupported,
    /// The caller cancelled local work; no rollback is implied.
    Cancelled,
    /// Serialization would exceed the finite output bound.
    OutputLimit,
    /// A document could not be serialized.
    Serialization,
    /// Original neutral registry failure.
    Registry(DiscoveryError),
    /// Exact local human-help capability failure, without a schema fallback.
    Help(DiscoveryError),
    /// Original context/resolution failure, not an empty schema.
    Resolver(CommandError),
    /// Original TABLE failure, including nested OneLake classifications.
    Table(Box<TableError>),
}

impl DiscoveryFailure {
    /// Sanitized machine classification, without raw provider codes or text.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidArguments => "invalid_arguments",
            Self::ExecutionNotImplemented => "discovery_preview_only",
            Self::NotConfigured => "NotConfigured",
            Self::Unsupported => "unsupported",
            Self::Cancelled => "cancelled",
            Self::OutputLimit => "output_limit_exceeded",
            Self::Serialization => "serialization_failed",
            Self::Registry(error) | Self::Help(error) => error.code(),
            Self::Resolver(_) => "resolution_failed",
            Self::Table(error) => table_error_code(&error.kind),
        }
    }

    /// Argument errors exit 2; incomplete, unsupported and host failures exit 1.
    pub fn exit_code(&self) -> i32 {
        if matches!(self, Self::InvalidArguments) {
            2
        } else {
            1
        }
    }
}

impl fmt::Display for DiscoveryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidArguments => {
                "Invalid arguments. Run fab-cli-discovery --help for discovery preview usage."
            }
            Self::ExecutionNotImplemented => {
                "Mutation execution is not implemented in this discovery preview. Use an operation with <path> --schema."
            }
            Self::NotConfigured => {
                "Backend data schema requires an injected host; live data host not configured."
            }
            Self::Unsupported => {
                "Unsupported schema backend, target type or facet in this discovery preview."
            }
            Self::Cancelled => "Discovery cancelled; no successful result is available.",
            Self::OutputLimit => "Discovery output exceeds the configured byte bound.",
            Self::Serialization => "Schema serialization failed.",
            Self::Registry(_) => "Operation schema is unavailable for this exact target and operation.",
            Self::Help(error) => {
                return write!(
                    formatter,
                    "{error} Run fab-cli-discovery help --help for supported selectors, or help <path> to list registered operations. Unregistered representations do not use a parent contract."
                );
            }
            Self::Resolver(_) => "Data-schema target resolution failed; no schema is available.",
            Self::Table(_) => "Backend schema read failed or is incomplete; no schema is available.",
        })
    }
}

impl std::error::Error for DiscoveryFailure {}

fn table_error_code(kind: &TableErrorKind) -> &'static str {
    match kind {
        TableErrorKind::InvalidInput => "invalid_target",
        TableErrorKind::InvalidDeltaTable => "invalid_delta_table",
        TableErrorKind::InvalidResponse => "invalid_response",
        TableErrorKind::Credential => "credential_failed",
        TableErrorKind::Execution => "execution_failed",
        TableErrorKind::Http { .. } => "service_error",
        TableErrorKind::Limit => "limit_exceeded",
        TableErrorKind::TimedOut => "timed_out",
        TableErrorKind::Cancelled => "cancelled",
        TableErrorKind::OneLake(error) => match error.as_ref() {
            OneLakeError::InvalidInput | OneLakeError::InvalidAudience => "invalid_target",
            OneLakeError::Credential => "credential_failed",
            OneLakeError::Execution(error) => match error {
                OneLakeExecutionError::TimedOut => "timed_out",
                OneLakeExecutionError::ResponseLimit => "limit_exceeded",
                OneLakeExecutionError::Cancelled => "cancelled",
                OneLakeExecutionError::Failed | OneLakeExecutionError::RetriesExhausted => {
                    "execution_failed"
                }
            },
            OneLakeError::Http { .. } => "service_error",
            OneLakeError::InvalidResponse => "invalid_response",
            OneLakeError::Limit => "limit_exceeded",
            OneLakeError::RepeatedContinuation => "repeated_continuation",
            OneLakeError::Cancelled => "cancelled",
        },
        TableErrorKind::OperationFailed
        | TableErrorKind::OperationCancelled
        | TableErrorKind::SharedOperation(_)
        | TableErrorKind::Maintenance { .. } => "unexpected_table_operation",
    }
}

/// Lexically validated backend-schema selector; not a resolved OneLake identity.
///
/// This initial slice accepts
/// `/<name>.Workspace/<name>.Lakehouse/Tables/<table>` or
/// `.../Tables/<schema>/<table>`, with an optional leading slash.
/// It deliberately does not accept `docs/schema`, native definitions, database
/// engines, globs, encoded selectors or OS paths. Literal quotes at either end
/// of the complete selector are unsupported because legacy token resolution
/// would strip them; internal quotes remain literal and are not normalized.
/// Legacy shortcut markers are unsupported because the existing resolver and
/// Delta reader remove them from otherwise literal table names.
#[derive(Debug)]
pub struct DataSchemaRequest {
    path: String,
}

impl DataSchemaRequest {
    /// Validate a typed exact Lakehouse Tables selector without any I/O.
    ///
    /// # Errors
    /// Returns an explicit unsupported failure for unsafe or unmapped selectors.
    pub fn parse(path: &str) -> Result<Self, DiscoveryFailure> {
        // The token resolver strips outer quotes; reject rather than select another table.
        if path.len() > MAX_TARGET_BYTES || path.trim_matches(['\'', '"']) != path {
            return Err(DiscoveryFailure::Unsupported);
        }
        let parts: Vec<_> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
        if !matches!(parts.len(), 4 | 5)
            || parts.iter().any(|part| !exact_segment(part))
            || !named_suffix(parts[0], ".Workspace")
            || !named_suffix(parts[1], ".Lakehouse")
            || parts[2] != "Tables"
        {
            return Err(DiscoveryFailure::Unsupported);
        }
        Ok(Self {
            path: path.to_owned(),
        })
    }

    /// Original lexical selector; no canonical identity or existence is implied.
    pub fn path(&self) -> &str {
        &self.path
    }
}

fn exact_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !matches!(segment, "." | "..")
        && segment.trim() == segment
        && !segment.contains(['\\', '*', '?', '[', ']', '{', '}', '%', ':', '#'])
        && !segment.contains(".Shortcut")
        && !segment.contains(".shortcut")
        && !segment.chars().any(char::is_control)
}

fn named_suffix(segment: &str, suffix: &str) -> bool {
    segment
        .strip_suffix(suffix)
        .is_some_and(|name| !name.is_empty() && !matches!(name, "." | ".."))
}

/// Parsed requests contain no payload, input file, stdin or execution arguments.
#[derive(Debug)]
pub enum DiscoveryRequest {
    /// A neutral SDK operation contract, optionally constrained by an artifact alias.
    Operation {
        /// Canonical SDK operation.
        operation: Operation,
        /// Unverified lexical target.
        target: TargetSelector,
        /// Optional owner narrowing, checked by the same SDK registry.
        facade: Option<TargetKind>,
    },
    /// Real backend metadata, requiring an injected provider.
    DataSchema(DataSchemaRequest),
}

/// Local help/version rendering or an effect-free parsed request.
#[derive(Debug)]
pub enum ParsedDiscovery {
    /// Human help or version text from this preview's own parser.
    Text(String),
    /// Exact-target human help, retaining the SDK contracts instead of parsing text.
    Help(TargetHelp),
    /// Schema request ready for [`DiscoveryHost::execute`].
    Request(DiscoveryRequest),
}

/// Build only the preview grammar; never reuse execution-required v1 builders.
///
/// Operation roots and artifact aliases enumerate the neutral SDK types. The
/// registry, not these command spellings, owns supported operation/type pairs
/// and all input constraints. Unsupported alias pairs remain explicit failures.
pub fn command() -> Command {
    let mut root = Command::new("fab-cli-discovery")
        .version(VERSION)
        .about("Opt-in Fabric schema/discovery preview")
        .after_help(BOUNDARIES)
        .disable_help_subcommand(true)
        .subcommand_required(true)
        .arg_required_else_help(false);
    for operation in Operation::ALL {
        root = root.subcommand(operation_command(operation, None));
    }
    let mut artifact = Command::new("artifact")
        .about("Owner-constrained aliases to the same neutral operation registry")
        .after_help(BOUNDARIES)
        .subcommand_required(true);
    for kind in TargetKind::ALL {
        let mut facade = Command::new(facade_name(kind))
            .about("Narrow operation discovery by owner type; does not resolve or authorize")
            .after_help(BOUNDARIES)
            .subcommand_required(true);
        for operation in Operation::ALL {
            facade = facade.subcommand(operation_command(operation, Some(kind)));
        }
        artifact = artifact.subcommand(facade);
    }
    root.subcommand(help::command())
        .subcommand(artifact)
        .subcommand(
        Command::new("schema")
            .about(
                "Backend data schema, not operation input or docs/schema; injected host required",
            )
            .after_help(BOUNDARIES)
            .arg(path_argument()),
    )
}

fn facade_name(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Workspace => "workspace",
        TargetKind::Notebook => "notebook",
    }
}

fn path_argument() -> Arg {
    Arg::new("path")
        .value_name("path")
        .help("Exact typed target path; lexical match is unverified and authorization unevaluated")
        .required(true)
        .num_args(1)
}

fn operation_command(operation: Operation, facade: Option<TargetKind>) -> Command {
    let mut help = registrations()
        .iter()
        .filter(|entry| {
            entry.operation() == operation && facade.is_none_or(|kind| entry.target_kind() == kind)
        })
        .map(|entry| entry.help())
        .collect::<Vec<_>>()
        .join("\n\n");
    help.push_str("\n\n");
    help.push_str(BOUNDARIES);
    Command::new(operation.as_str())
        .about("Inspect this operation's SDK contract; mutation execution is not implemented")
        .after_help(help)
        .arg(path_argument())
        .arg(
            Arg::new("schema")
                .long("schema")
                .action(ArgAction::SetTrue)
                .help("Print the exact registered operation descriptor JSON; never execute"),
        )
}

/// Parse argv, including its executable element, without ambient input or I/O.
///
/// # Errors
/// Raw Clap errors are never returned or printed: they may contain supplied
/// secrets. Help/version use only static registered command information.
pub fn parse_from<I, T>(args: I) -> Result<ParsedDiscovery, DiscoveryFailure>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = match command().try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return Ok(ParsedDiscovery::Text(error.to_string()));
        }
        Err(_) => return Err(DiscoveryFailure::InvalidArguments),
    };
    let (name, values) = matches
        .subcommand()
        .ok_or(DiscoveryFailure::InvalidArguments)?;
    if name == "help" {
        return help::parse(values).map(ParsedDiscovery::Help);
    }
    if name == "schema" {
        let path = values
            .get_one::<String>("path")
            .ok_or(DiscoveryFailure::InvalidArguments)?;
        return Ok(ParsedDiscovery::Request(DiscoveryRequest::DataSchema(
            DataSchemaRequest::parse(path)?,
        )));
    }
    let (name, values, facade) = if name == "artifact" {
        let (kind_name, owner) = values
            .subcommand()
            .ok_or(DiscoveryFailure::InvalidArguments)?;
        let kind = TargetKind::ALL
            .into_iter()
            .find(|kind| facade_name(*kind) == kind_name)
            .ok_or(DiscoveryFailure::InvalidArguments)?;
        let (operation, values) = owner
            .subcommand()
            .ok_or(DiscoveryFailure::InvalidArguments)?;
        (operation, values, Some(kind))
    } else {
        (name, values, None)
    };
    operation_request(name, values, facade).map(ParsedDiscovery::Request)
}

fn operation_request(
    name: &str,
    values: &ArgMatches,
    facade: Option<TargetKind>,
) -> Result<DiscoveryRequest, DiscoveryFailure> {
    if !values.get_flag("schema") {
        return Err(DiscoveryFailure::ExecutionNotImplemented);
    }
    let operation = name
        .parse::<Operation>()
        .map_err(|_| DiscoveryFailure::InvalidArguments)?;
    let path = values
        .get_one::<String>("path")
        .ok_or(DiscoveryFailure::InvalidArguments)?;
    let target = TargetSelector::parse(path).map_err(DiscoveryFailure::Registry)?;
    Ok(DiscoveryRequest::Operation {
        operation,
        target,
        facade,
    })
}

/// Runtime-neutral backend metadata provider. Never fall back to query/export.
///
/// Implementations must honor cancellation before I/O, drop local resources on
/// cancellation and return errors for partial/failed reads, not empty success.
/// A successful empty native fields array is distinct from an absent provider.
pub trait DataSchemaProvider: Send + Sync {
    /// Finite serialized document bound, including the output newline.
    ///
    /// The host also enforces [`MAX_DOCUMENT_BYTES`]. The Delta adapter uses
    /// [`TableLimits::max_response_bytes`] for this bound as well as SDK reads.
    fn max_output_bytes(&self) -> NonZeroUsize;

    /// Obtain actual metadata, on the caller's runtime, through injected services.
    fn schema<'a>(
        &'a self,
        request: &'a DataSchemaRequest,
        cancelled: DiscoveryFuture<'a, ()>,
    ) -> DiscoveryFuture<'a, Result<DataSchemaDocument, DiscoveryFailure>>;
}

/// Real physical Delta adapter, with no identity, runtime or transport creation.
pub struct DeltaDataSchemaProvider<'a> {
    /// Existing TABLE SDK, with credentials explicitly supplied by its caller.
    pub sdk: &'a TableClient,
    /// Existing bounded execution adapter; never constructed by this host.
    pub executor: &'a dyn TableExecutor,
    /// Existing OneLake context resolver; final target existence is required.
    pub resolver: &'a dyn TableResolver,
    /// Existing finite SDK bounds, also bounding serialized output.
    pub limits: TableLimits,
}

impl DataSchemaProvider for DeltaDataSchemaProvider<'_> {
    fn max_output_bytes(&self) -> NonZeroUsize {
        self.limits.max_response_bytes
    }

    fn schema<'a>(
        &'a self,
        request: &'a DataSchemaRequest,
        cancelled: DiscoveryFuture<'a, ()>,
    ) -> DiscoveryFuture<'a, Result<DataSchemaDocument, DiscoveryFailure>> {
        Box::pin(cancel_first(cancelled, async move {
            let tokens = [request.path().to_owned()];
            let resolved = self
                .resolver
                .resolve(&tokens, true, Box::pin(pending()))
                .await
                .map_err(DiscoveryFailure::Resolver)?;
            let parts: Vec<_> = resolved.path.local_path().split('/').collect();
            if resolved.item_type.as_str() != "Lakehouse"
                || !matches!(parts.len(), 2 | 3)
                || parts[0] != "Tables"
                || parts.iter().any(|part| !exact_segment(part))
            {
                return Err(DiscoveryFailure::Unsupported);
            }
            self.sdk
                .schema_document(self.executor, &resolved.path, self.limits, pending())
                .await
                .map_err(|error| DiscoveryFailure::Table(Box::new(error)))
        }))
    }
}

/// Untagged success projection: exactly the SDK descriptor or data document.
///
/// There is no host-owned copy of an operation contract or native schema.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum DiscoveryDocument {
    /// Versioned neutral SDK operation contract.
    Operation(OperationContract),
    /// Versioned TABLE SDK native metadata document, with unknown completeness.
    Data(DataSchemaDocument),
}

/// Reusable immutable injected host. Construction has no side effects.
#[derive(Default)]
pub struct DiscoveryHost<'a> {
    /// Absent in the standalone binary. Operation discovery does not consult it.
    pub data_schema: Option<&'a dyn DataSchemaProvider>,
}

impl DiscoveryHost<'_> {
    /// Execute a parsed discovery request on the caller's runtime.
    ///
    /// Cancellation wins when ready concurrently with work. No partial data
    /// document is returned on errors, and no data provider is consulted for an
    /// operation descriptor. Dropping cancellation-raced work cancels local
    /// resolution/reads, not a remote operation or rollback.
    ///
    /// # Errors
    /// Unsupported contracts, unconfigured backend hosts, cancellation and all
    /// resolver/SDK failures remain explicit typed failures.
    pub async fn execute(
        &self,
        request: &DiscoveryRequest,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<DiscoveryDocument, DiscoveryFailure> {
        cancel_first(cancelled, async {
            match request {
                DiscoveryRequest::Operation {
                    operation,
                    target,
                    facade,
                } => {
                    let contract = match facade {
                        Some(kind) => discover_for_facade(*operation, target, *kind),
                        None => discover(*operation, target),
                    }
                    .map_err(DiscoveryFailure::Registry)?;
                    Ok(DiscoveryDocument::Operation(contract))
                }
                DiscoveryRequest::DataSchema(request) => {
                    let provider = self.data_schema.ok_or(DiscoveryFailure::NotConfigured)?;
                    provider
                        .schema(request, Box::pin(pending()))
                        .await
                        .map(DiscoveryDocument::Data)
                }
            }
        })
        .await
    }

    /// Parse, execute and write one invocation; no batch or REPL semantics.
    ///
    /// Success JSON is UTF-8 and newline terminated on stdout. Sanitized failures
    /// are newline terminated JSON on stderr. Serialization/limit checks finish
    /// before writing success bytes. Stream failures propagate as `io::Error`;
    /// after a partial writer failure callers must not treat bytes as a result.
    ///
    /// # Errors
    /// Returns all stdout/stderr write and flush failures to the embedding caller.
    pub async fn run<I, T>(
        &self,
        args: I,
        cancelled: impl Future<Output = ()> + Send,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> io::Result<i32>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let request = match parse_from(args) {
            Ok(ParsedDiscovery::Text(text)) => {
                stdout.write_all(text.as_bytes())?;
                stdout.flush()?;
                return Ok(0);
            }
            Ok(ParsedDiscovery::Help(help)) => {
                let text = match cancel_first(cancelled, async { Ok(help.render()) }).await {
                    Ok(text) => text,
                    Err(error) => return write_failure(&error, stderr),
                };
                if text.len() > MAX_DOCUMENT_BYTES {
                    return write_failure(&DiscoveryFailure::OutputLimit, stderr);
                }
                stdout.write_all(text.as_bytes())?;
                stdout.flush()?;
                return Ok(0);
            }
            Ok(ParsedDiscovery::Request(request)) => request,
            Err(error) => return write_failure(&error, stderr),
        };
        let result = self.execute(&request, cancelled).await;
        let limit = match &request {
            DiscoveryRequest::DataSchema(_) => self
                .data_schema
                .map_or(MAX_DOCUMENT_BYTES, |provider| {
                    provider.max_output_bytes().get()
                })
                .min(MAX_DOCUMENT_BYTES),
            DiscoveryRequest::Operation { .. } => MAX_DOCUMENT_BYTES,
        };
        let bytes = match result.and_then(|document| encode_bounded(&document, limit)) {
            Ok(bytes) => bytes,
            Err(error) => return write_failure(&error, stderr),
        };
        stdout.write_all(&bytes)?;
        stdout.flush()?;
        Ok(0)
    }
}

async fn cancel_first<T>(
    cancelled: impl Future<Output = ()>,
    work: impl Future<Output = Result<T, DiscoveryFailure>>,
) -> Result<T, DiscoveryFailure> {
    let mut cancelled = pin!(cancelled);
    let mut work = pin!(work);
    poll_fn(|context| {
        if cancelled.as_mut().poll(context).is_ready() {
            Poll::Ready(Err(DiscoveryFailure::Cancelled))
        } else {
            work.as_mut().poll(context)
        }
    })
    .await
}

/// Write a sanitized structured failure and propagate stream errors.
///
/// # Errors
/// Returns write/flush failures; raw argument or provider messages are not used.
pub fn write_failure(error: &DiscoveryFailure, stderr: &mut dyn Write) -> io::Result<i32> {
    #[derive(Serialize)]
    struct ErrorBody<'a> {
        code: &'a str,
        message: String,
    }
    #[derive(Serialize)]
    struct Envelope<'a> {
        error: ErrorBody<'a>,
    }
    let bytes = encode_bounded(
        &Envelope {
            error: ErrorBody {
                code: error.code(),
                message: error.to_string(),
            },
        },
        MAX_DOCUMENT_BYTES,
    )
    .map_err(|_| io::Error::other("discovery error serialization failed"))?;
    stderr.write_all(&bytes)?;
    stderr.flush()?;
    Ok(error.exit_code())
}

fn encode_bounded(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, DiscoveryFailure> {
    let mut output = BoundedBuffer {
        bytes: Vec::new(),
        limit,
        exceeded: false,
    };
    if serde_json::to_writer(&mut output, value).is_err() {
        return Err(if output.exceeded {
            DiscoveryFailure::OutputLimit
        } else {
            DiscoveryFailure::Serialization
        });
    }
    output
        .write_all(b"\n")
        .map_err(|_| DiscoveryFailure::OutputLimit)?;
    Ok(output.bytes)
}

struct BoundedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("discovery output byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
