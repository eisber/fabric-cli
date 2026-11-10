// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive, experimental v2 GET grammar and runtime-neutral composition.
//!
//! This module does not register a root verb or change the v1 parser. Its explicit
//! preview bounds are 1 MiB and 30 seconds; not global CLI defaults. Omitted
//! format uses the resolved path's default (Markdown for docs, JSON for metadata).
//! The backend resolves the entire path, including representation selectors and
//! slash-containing names. Projection and output selection never choose a remote
//! operation. Native JSON data/envelopes are not relabeled as another schema.
//! Waiting requires an advertised observer, never an inferred kind or a replay.
//!
//! Call [`PreparedGet::preflight`] and delivery methods in a blocking context.
//! Only acquisition is async. The host supplies one interruption future covering
//! both caller cancellation and the **whole** command deadline, started before
//! preflight using [`GetLimits::timeout`]. No runtime, timer, ambient working
//! directory, identity, logger, stream or process is discovered here.
//! The host checks cancellation/deadline again before synchronous delivery;
//! delivery is a possibly-partial effect, not a cancellable atomic transaction.

use crate::get_target::{DataAgentStage, GetTarget};
use clap::{Arg, ArgAction, Command, error::ErrorKind};
use fabric_cli_storage_files::{ExactOutputError, ExactOutputFile, ExactOutputReceipt};
use futures_util::future::{Either, select};
use serde_json::Value;
use std::{
    error::Error,
    ffi::OsString,
    future::{Future, poll_fn},
    io::{self, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    pin::{Pin, pin},
    task::Poll,
    time::Duration,
};

/// Explicit experimental GET defaults, not inherited CLI settings.
pub const DEFAULT_MAX_BYTES: usize = 1024 * 1024;
/// Explicit experimental whole-command timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A path-advertised presentation, never a remote operation selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GetFormat {
    /// The backend's native structured data/envelope as JSON.
    Json,
    /// A native Markdown representation advertised by this resolved path.
    Markdown,
    /// The original bytes, without newline, encoding or filename changes.
    Raw,
}

/// The caller-selected delivery destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GetOutput {
    /// Omitted output or the exact `-` spelling; no filesystem preflight.
    Stdout,
    /// One exact local filename, resolved against the explicitly supplied cwd.
    File(PathBuf),
}

/// Bounds shared by resolution and acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GetLimits {
    /// Selected unprojected source/result and final output byte bound. Raw counts
    /// original binary bytes, not an unrelated base64/metadata JSON envelope.
    pub max_bytes: NonZeroUsize,
    /// Whole-command deadline duration; the caller provides its running timer.
    pub timeout: Duration,
}

/// Remote acquisition options, deliberately excluding presentation/projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GetAcquireOptions {
    /// Required for DataAgent configuration; source stages are addressed in paths.
    pub stage: Option<DataAgentStage>,
    /// Observe an advertised handle, including one returned by a single native
    /// read-effect acquisition; never replay acquisition in order to wait.
    pub wait: bool,
    /// Permit only the resolved target's supported bounded child traversal.
    pub recursive: bool,
    /// Child depth, with zero meaning the selected root only.
    pub depth: Option<u32>,
    /// The same bounds apply before any local projection.
    pub limits: GetLimits,
}

/// Parsed v2 GET input. The original path is passed unchanged to the resolver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GetCommand {
    /// One Unicode Fabric path, not slash-split or URL-to-identity rewritten.
    pub path: String,
    /// Exact local-file output or stdout.
    pub output: GetOutput,
    /// Explicit advertised presentation, or the resolved path default if omitted.
    pub format: Option<GetFormat>,
    /// One JMESPath expression; it never changes acquisition.
    pub project: Option<String>,
    /// Bounded operation options independent of projection and presentation.
    pub options: GetAcquireOptions,
}

/// Sanitized argument failure, never including supplied argument values.
#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
#[error("Invalid experimental get arguments; inspect kind or get_command() help.")]
pub struct GetArgumentError {
    /// Structured reason, without echoing paths, query text or parser messages.
    pub kind: GetArgumentErrorKind,
}

/// Stable semantic failures plus Clap's structured syntax classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GetArgumentErrorKind {
    /// The first argument must be exactly `get`, not an executable name.
    MissingCommand,
    /// A required value, known option, exact value count or value parser failed.
    Syntax(ErrorKind),
    /// The Fabric path must not be empty.
    EmptyPath,
    /// A present JMESPath expression must not be empty.
    EmptyProjection,
    /// Byte bounds and timeout durations must be positive.
    InvalidLimit,
    /// Raw bytes cannot be projected as JSON.
    RawProjection,
    /// Depth has meaning only with explicit recursive traversal.
    DepthWithoutRecursive,
    /// Missing, conflicting or inapplicable explicit DataAgent stage/owner selection.
    InvalidStageTarget,
}

/// Build the isolated v2 GET schema without registering or executing it.
pub fn get_command() -> Command {
    Command::new("get")
        .args_override_self(false)
        .infer_long_args(false)
        .about("Experimental bounded GET over a caller-configured native backend")
        .after_help("Preview bounds: --max-bytes 1048576 and --timeout 30s. Omitted --format uses the resolved path default: Markdown for docs, JSON for metadata. Formats are path-advertised; metadata supports JSON only. Accepted/incomplete acquisitions can deliver only an unprojected native JSON receipt to stdout, never partial content or an output file. --output - selects stdout; filenames are exact and never overwritten. --project/-q takes one JMESPath expression and does not change acquisition. This schema does not replace the v1 command.")
        .arg(Arg::new("path").required(true).num_args(1))
        .arg(
            Arg::new("stage")
                .long("stage")
                .num_args(1)
                .value_parser(["draft", "published"])
                .help("Explicit DataAgent settings/datasources stage; draft uses native staging"),
        )
        .arg(
            Arg::new("output")
                .short('o')
                .long("output")
                .num_args(1)
                .value_name("FILE|-")
                .value_parser(clap::builder::OsStringValueParser::new()),
        )
        .arg(
            Arg::new("format")
                .long("format")
                .num_args(1)
                .value_parser(["json", "markdown", "raw"]),
        )
        .arg(
            Arg::new("wait")
                .long("wait")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("recursive")
                .long("recursive")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("depth")
                .long("depth")
                .num_args(1)
                .value_parser(clap::value_parser!(u32)),
        )
        .arg(
            Arg::new("max-bytes")
                .long("max-bytes")
                .num_args(1)
                .default_value("1048576")
                .value_parser(clap::value_parser!(NonZeroUsize)),
        )
        .arg(
            Arg::new("timeout")
                .long("timeout")
                .num_args(1)
                .default_value("30s")
                .value_parser(parse_duration),
        )
        .arg(
            Arg::new("project")
                .long("project")
                .short('q')
                .num_args(1)
                .value_name("JMESPath"),
        )
}

/// Parse `get` followed by its arguments, without an initial executable name.
///
/// Every option takes exactly its documented value count. Duplicate options,
/// aliases of a repeated projection, abbreviations and global YAML/native format
/// aliases are rejected. `--help` returns a syntax error with `DisplayHelp` kind;
/// an embedding host can render [`get_command`] help on its own stream.
///
/// # Errors
/// Returns sanitized syntax/semantic failures without performing any I/O.
pub fn parse_get(args: impl IntoIterator<Item = OsString>) -> Result<GetCommand, GetArgumentError> {
    let args: Vec<_> = args.into_iter().collect();
    let facade = args.first().and_then(|value| value.to_str()) == Some("artifact");
    let mut args = args.into_iter();
    if facade
        && (args.next().as_deref() != Some(std::ffi::OsStr::new("artifact"))
            || args.next().as_deref() != Some(std::ffi::OsStr::new("data-agent")))
    {
        return Err(argument_error(GetArgumentErrorKind::MissingCommand));
    }
    if args.next().as_deref() != Some(std::ffi::OsStr::new("get")) {
        return Err(argument_error(GetArgumentErrorKind::MissingCommand));
    }
    let matches = get_command()
        .no_binary_name(true)
        .try_get_matches_from(args)
        .map_err(|error| argument_error(GetArgumentErrorKind::Syntax(error.kind())))?;
    let command = GetCommand {
        path: matches
            .get_one::<String>("path")
            .expect("required path")
            .clone(),
        output: match matches.get_one::<OsString>("output") {
            None => GetOutput::Stdout,
            Some(value) if value == "-" => GetOutput::Stdout,
            Some(value) => GetOutput::File(value.into()),
        },
        format: matches
            .get_one::<String>("format")
            .map(|format| match format.as_str() {
                "markdown" => GetFormat::Markdown,
                "raw" => GetFormat::Raw,
                _ => GetFormat::Json,
            }),
        project: matches.get_one::<String>("project").cloned(),
        options: GetAcquireOptions {
            stage: matches.get_one::<String>("stage").map(|stage| {
                if stage == "draft" {
                    DataAgentStage::Draft
                } else {
                    DataAgentStage::Published
                }
            }),
            wait: matches.get_flag("wait"),
            recursive: matches.get_flag("recursive"),
            depth: matches.get_one::<u32>("depth").copied(),
            limits: GetLimits {
                max_bytes: *matches
                    .get_one::<NonZeroUsize>("max-bytes")
                    .expect("explicit experimental byte default"),
                timeout: *matches
                    .get_one::<Duration>("timeout")
                    .expect("explicit experimental timeout default"),
            },
        },
    };
    validate_command(&command)?;
    if let Ok(GetTarget::Item {
        item_type: Some(kind),
        representation,
        ..
    }) = GetTarget::parse(&command.path)
        && kind == "DataAgent"
    {
        crate::get_dataagent::validate_representation(&representation)
            .map_err(|_| argument_error(GetArgumentErrorKind::InvalidStageTarget))?;
    }
    if facade
        && !matches!(
            GetTarget::parse(&command.path),
            Ok(GetTarget::Item { item_type: Some(kind), .. }) if kind == "DataAgent"
        )
    {
        return Err(argument_error(GetArgumentErrorKind::InvalidStageTarget));
    }
    Ok(command)
}

fn argument_error(kind: GetArgumentErrorKind) -> GetArgumentError {
    GetArgumentError { kind }
}

fn validate_command(command: &GetCommand) -> Result<(), GetArgumentError> {
    let invalid = if command.path.is_empty() {
        Some(GetArgumentErrorKind::EmptyPath)
    } else if command.project.as_deref().is_some_and(str::is_empty) {
        Some(GetArgumentErrorKind::EmptyProjection)
    } else if command.options.limits.timeout.is_zero() {
        Some(GetArgumentErrorKind::InvalidLimit)
    } else if command.format == Some(GetFormat::Raw) && command.project.is_some() {
        Some(GetArgumentErrorKind::RawProjection)
    } else if command.options.depth.is_some() && !command.options.recursive {
        Some(GetArgumentErrorKind::DepthWithoutRecursive)
    } else if GetTarget::parse(&command.path)
        .is_ok_and(|target| target.validate_stage(command.options.stage).is_err())
        || (command.options.stage.is_some() && GetTarget::parse(&command.path).is_err())
    {
        Some(GetArgumentErrorKind::InvalidStageTarget)
    } else {
        None
    };
    invalid.map_or(Ok(()), |kind| Err(argument_error(kind)))
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let invalid = || "Expected a finite positive duration with ms, s or m suffix".to_owned();
    let (number, factor) = if let Some(number) = value.strip_suffix("ms") {
        (number, 0.001)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60.0)
    } else {
        return Err(invalid());
    };
    let mut pieces = number.split('.');
    let integer = pieces.next().unwrap_or("");
    let fractional = pieces.next();
    if integer.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || fractional
            .is_some_and(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        || pieces.next().is_some()
    {
        return Err(invalid());
    }
    let seconds = number.parse::<f64>().map_err(|_| invalid())? * factor;
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| invalid())?;
    if duration.is_zero() {
        return Err(invalid());
    }
    Ok(duration)
}

/// A backend-selected target category, resolved before content acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GetTargetKind {
    /// Native metadata; only JSON is permitted even if misadvertised otherwise.
    Metadata,
    /// An explicit representation path, not an inferred export.
    Representation,
    /// A previously returned operation/job handle; wait is separately advertised.
    ReturnedHandle,
}

/// Backend-native target identity with its explicit read/presentation contract.
#[derive(Debug)]
pub struct ResolvedGet<T> {
    /// Typed operation identity owned by the backend, not fabricated by the CLI.
    pub target: T,
    /// Distinguishes metadata, representations and already-returned handles.
    pub kind: GetTargetKind,
    /// Formats advertised by this exact resolved path.
    pub formats: Vec<GetFormat>,
    /// Omitted-format presentation advertised by this path, never a global alias.
    pub default_format: GetFormat,
    /// This exact path advertises observation of an existing handle or one
    /// returned by its single native read-effect acquisition. Metadata must
    /// advertise false; a target kind alone never grants observation.
    pub wait: bool,
    /// This path supports bounded child traversal.
    pub recursive: bool,
}

/// Native data/receipt and optional native byte/text representations.
///
/// `data` retains the backend's target, evidence and coverage envelope if it has
/// one. The CLI never invents or changes that schema. Raw bytes are not embedded
/// into or serialized through JSON. The backend must enforce acquisition bounds,
/// including intermediate pages/decoding, before constructing this payload.
#[derive(Debug)]
pub struct GetPayload {
    /// Native unprojected structured data, including native coverage evidence.
    pub data: Value,
    /// Original bytes, only when this resolved representation supports raw.
    pub raw: Option<Vec<u8>>,
    /// Native Markdown, only when the resolved representation supplies it.
    pub markdown: Option<String>,
    /// Native raw media type; JSON/Markdown delivery uses their actual media type.
    pub media_type: String,
    /// Whether source content/coverage is complete, not merely receipt encoding.
    /// Accepted operations and incomplete graphs must set false even when their
    /// JSON receipt is complete. `data` must retain native status/coverage and
    /// handles. Only that whole, unprojected JSON receipt can reach stdout; no
    /// file/raw/Markdown delivery or accepted-to-completed conversion occurs.
    pub complete: bool,
}

/// Sendable future returned by a caller-injected operation backend.
pub type GetFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, GetCommandError>> + Send + 'a>>;

/// A runtime-neutral adapter over frontend-neutral native operations.
///
/// Object-safe with an explicit associated target, for example
/// `&dyn GetBackend<Target = NativeTarget>`. No method may discover credentials,
/// streams, cwd or a runtime. Dropping resolve/retrieve futures must cancel their
/// work; detached acquisition would defeat the whole-command interruption bound.
pub trait GetBackend: Send + Sync {
    /// Native resolved operation identity, shared with the host's SDK adapter.
    type Target: Send + Sync;

    /// Resolve the entire path without retrieving remote content.
    ///
    /// Bounded lookup metadata is allowed. Do not infer identity by splitting
    /// names at `/`, treat URL input as identity without the SDK resolver, or use
    /// a projection/format to choose another remote operation.
    fn resolve<'a>(
        &'a self,
        path: &'a str,
        limits: GetLimits,
    ) -> GetFuture<'a, ResolvedGet<Self::Target>>;

    /// Resolve stage-aware capabilities without changing existing backend contracts.
    fn resolve_with_options<'a>(
        &'a self,
        path: &'a str,
        options: GetAcquireOptions,
    ) -> GetFuture<'a, ResolvedGet<Self::Target>> {
        if options.stage.is_some() {
            return Box::pin(async {
                Err(argument_error(GetArgumentErrorKind::InvalidStageTarget).into())
            });
        }
        self.resolve(path, options.limits)
    }

    /// Retrieve complete bounded content for the already validated target.
    ///
    /// `wait` requires `resolved.wait` and only observes the exact handle already
    /// resolved or returned by this one native read-effect acquisition. Never
    /// replay a read-effect POST or submit a new operation in order to wait.
    /// Recursive/depth never imply an unbounded listing. Enforce max_bytes on
    /// unprojected source data and honor the host's shared whole deadline.
    /// There is deliberately no projection, format or output-path argument.
    fn retrieve<'a>(
        &'a self,
        resolved: &'a ResolvedGet<Self::Target>,
        options: GetAcquireOptions,
    ) -> GetFuture<'a, GetPayload>;

    /// Validate typed local projection syntax before any target discovery.
    fn validate_projection(&self, expression: &str) -> Result<(), GetCommandError> {
        fabric_cli_query::validate_projection(expression).map_err(GetCommandError::from)
    }

    /// Project retrieved data locally without changing native acquisition.
    ///
    /// Adapters may preserve their own evidence envelope outside projected data.
    fn project(&self, data: &Value, expression: &str) -> Result<Value, GetCommandError> {
        fabric_cli_query::project_value(data, expression).map_err(GetCommandError::from)
    }

    /// Render projected native data as Markdown without any remote acquisition.
    ///
    /// Unprojected Markdown uses `GetPayload::markdown` unchanged. Adapters only
    /// implement this if that same path advertises a typed Markdown renderer.
    fn render_markdown(
        &self,
        _data: &Value,
        _max_bytes: NonZeroUsize,
    ) -> Result<String, GetCommandError> {
        Err(GetCommandError::MissingRepresentation(GetFormat::Markdown))
    }
}

/// A host-provided interruption covering cancellation and the whole deadline.
#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub enum GetInterruption {
    /// The caller cancelled this command; cancellation is not rollback.
    #[error("GET was cancelled")]
    Cancelled,
    /// The one whole-command deadline elapsed.
    #[error("GET whole-command deadline elapsed")]
    DeadlineExceeded,
}

/// Typed command failures; provider text is never parsed or used for dispatch.
#[derive(Debug, thiserror::Error)]
pub enum GetCommandError {
    /// Invalid direct construction or parsed argument combination.
    #[error(transparent)]
    Arguments(#[from] GetArgumentError),
    /// Relative local output needs an explicit absolute native working directory.
    #[error("GET file output requires a caller-supplied absolute working directory")]
    InvalidWorkingDirectory,
    /// This path does not advertise an observer for its exact returned handle.
    #[error("--wait requires an advertised GET observer")]
    WaitRequiresHandle,
    /// This target does not advertise bounded child traversal.
    #[error("The resolved GET target does not support --recursive")]
    RecursiveUnsupported,
    /// The selected explicit or path-default format was not advertised here.
    #[error("The resolved GET target does not support the selected format")]
    UnsupportedFormat(GetFormat),
    /// Native typed JMESPath failure, without legacy scalar stringification.
    #[error(transparent)]
    Projection(#[from] fabric_cli_query::QueryError),
    /// Advertised native bytes/text were not provided by acquisition.
    #[error("GET acquisition did not provide the selected native representation")]
    MissingRepresentation(GetFormat),
    /// Incomplete native coverage cannot satisfy the requested delivery.
    #[error("GET acquisition is incomplete; no output was delivered")]
    Incomplete {
        /// Native structured evidence, retained without printing or rewrapping it.
        data: Value,
    },
    /// Complete source or final rendered output exceeded its byte bound.
    #[error("GET data exceeds the selected byte bound")]
    ByteLimit {
        /// Maximum bytes selected by the caller.
        limit: usize,
        /// Observed bytes, possibly the first serialized prefix exceeding limit.
        observed: usize,
    },
    /// Native JSON encoding failure, without interpreting provider text.
    #[error("GET JSON encoding failed")]
    Json(#[source] serde_json::Error),
    /// A native backend failure with a stable caller-owned code and source.
    #[error("GET backend failed ({code})")]
    Backend {
        /// Stable structured code, not a parsed provider message.
        code: &'static str,
        /// Original typed SDK/operation error for callers to inspect.
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    /// Caller cancellation or its shared whole-command deadline.
    #[error(transparent)]
    Interrupted(#[from] GetInterruption),
    /// Original local-file error including path/create/partial-write evidence.
    #[error(transparent)]
    File(#[from] ExactOutputError),
    /// Original stream error including confirmed bytes already written.
    #[error(transparent)]
    Stdout(#[from] GetStdoutError),
    /// The caller selected the wrong synchronous delivery method.
    #[error("GET delivery method does not match the preflighted destination")]
    DeliveryMismatch,
}

#[derive(Debug)]
enum PreparedOutput {
    Stdout,
    File(ExactOutputFile),
}

/// Preflighted command with no remote work or created file.
#[derive(Debug)]
pub struct PreparedGet {
    command: GetCommand,
    output: PreparedOutput,
}

impl PreparedGet {
    /// Preflight exact local output in the caller's blocking context.
    ///
    /// Relative filenames are joined to `absolute_cwd`, never ambient cwd. The
    /// exact-output policy rejects unresolved `.`/`..`, remote/device paths,
    /// missing parents and existing entries; it never creates parents or probes
    /// writability by creating a file. Stdout performs no filesystem operation.
    ///
    /// # Errors
    /// Preserves typed argument and exact-output preflight failures. A failure
    /// cannot reach either backend method.
    pub fn preflight(command: GetCommand, absolute_cwd: &Path) -> Result<Self, GetCommandError> {
        validate_command(&command)?;
        let output = match &command.output {
            GetOutput::Stdout => PreparedOutput::Stdout,
            GetOutput::File(path) if path == Path::new("-") => PreparedOutput::Stdout,
            GetOutput::File(path) => {
                if !absolute_cwd.is_absolute() {
                    return Err(GetCommandError::InvalidWorkingDirectory);
                }
                let path = if path.is_absolute() {
                    path.clone()
                } else {
                    absolute_cwd.join(path)
                };
                PreparedOutput::File(ExactOutputFile::preflight(
                    &path,
                    command.options.limits.max_bytes.get(),
                )?)
            }
        };
        Ok(Self { command, output })
    }

    /// Acquire, validate and fully buffer output without filesystem writes.
    ///
    /// `interrupted` must combine caller cancellation with the single running
    /// timer for `command.options.limits.timeout`, started before preflight. It
    /// is polled first, so an already-interrupted call never reaches the backend.
    /// Dropping acquisition cancels in-flight backend futures, without rollback.
    /// No blocking filesystem operation occurs on this async boundary.
    ///
    /// # Errors
    /// Preserves resolution/acquisition failures, interruption, native coverage,
    /// format and byte-bound failures. No failure writes a stdout/file prefix.
    pub async fn acquire<B: GetBackend + ?Sized>(
        self,
        backend: &B,
        interrupted: impl Future<Output = GetInterruption> + Send,
    ) -> Result<GetDelivery, GetCommandError> {
        let interruption = pin!(interrupted);
        let acquisition = pin!(self.acquire_complete(backend));
        match select(interruption, acquisition).await {
            Either::Left((reason, _)) => Err(reason.into()),
            Either::Right((result, mut interruption)) => {
                let reason = poll_fn(|cx| {
                    Poll::Ready(match interruption.as_mut().poll(cx) {
                        Poll::Ready(reason) => Some(reason),
                        Poll::Pending => None,
                    })
                })
                .await;
                reason.map_or(result, |reason| Err(reason.into()))
            }
        }
    }

    async fn acquire_complete<B: GetBackend + ?Sized>(
        self,
        backend: &B,
    ) -> Result<GetDelivery, GetCommandError> {
        let command = self.command;
        if let Some(expression) = &command.project {
            backend.validate_projection(expression)?;
        }
        let resolved = backend
            .resolve_with_options(&command.path, command.options)
            .await?;
        let mut format = validate_resolved(&command, &resolved)?;
        let mut payload = backend.retrieve(&resolved, command.options).await?;
        if !payload.complete {
            if matches!(&self.output, PreparedOutput::File(_))
                || command.project.is_some()
                || (command.format.is_some() && format != GetFormat::Json)
            {
                return Err(GetCommandError::Incomplete { data: payload.data });
            }
            format = GetFormat::Json;
        }
        let limit = command.options.limits.max_bytes.get();
        let original_json = if format == GetFormat::Raw {
            Vec::new()
        } else {
            bounded_json(&payload.data, limit)?
        };
        if format == GetFormat::Raw
            && let Some(raw) = &payload.raw
        {
            check_bytes(raw.len(), limit)?;
        }
        if format == GetFormat::Markdown
            && let Some(markdown) = &payload.markdown
        {
            check_bytes(markdown.len(), limit)?;
        }
        let projected = command
            .project
            .as_deref()
            .map(|expression| backend.project(&payload.data, expression))
            .transpose()?;
        let (bytes, media_type) = match format {
            GetFormat::Json => (
                match &projected {
                    Some(data) => bounded_json(data, limit)?,
                    None => original_json,
                },
                "application/json".to_owned(),
            ),
            GetFormat::Markdown => {
                let markdown = match &projected {
                    Some(data) => {
                        backend.render_markdown(data, command.options.limits.max_bytes)?
                    }
                    None => match payload.markdown.take() {
                        Some(markdown) => markdown,
                        None => backend
                            .render_markdown(&payload.data, command.options.limits.max_bytes)?,
                    },
                };
                check_bytes(markdown.len(), limit)?;
                (markdown.into_bytes(), "text/markdown".to_owned())
            }
            GetFormat::Raw => (
                payload
                    .raw
                    .take()
                    .ok_or(GetCommandError::MissingRepresentation(format))?,
                payload.media_type,
            ),
        };
        Ok(GetDelivery {
            output: self.output,
            bytes,
            format,
            media_type,
            data: payload.data,
            complete: payload.complete,
        })
    }
}

fn validate_resolved<T>(
    command: &GetCommand,
    resolved: &ResolvedGet<T>,
) -> Result<GetFormat, GetCommandError> {
    if command.options.wait && !resolved.wait {
        return Err(GetCommandError::WaitRequiresHandle);
    }
    if command.options.recursive && !resolved.recursive {
        return Err(GetCommandError::RecursiveUnsupported);
    }
    let format = command.format.unwrap_or(resolved.default_format);
    if !resolved.formats.contains(&format)
        || (resolved.kind == GetTargetKind::Metadata && format != GetFormat::Json)
    {
        return Err(GetCommandError::UnsupportedFormat(format));
    }
    if format == GetFormat::Raw && command.project.is_some() {
        return Err(argument_error(GetArgumentErrorKind::RawProjection).into());
    }
    Ok(format)
}

fn check_bytes(observed: usize, limit: usize) -> Result<(), GetCommandError> {
    if observed > limit {
        return Err(GetCommandError::ByteLimit { limit, observed });
    }
    Ok(())
}

pub(crate) fn bounded_json(data: &Value, limit: usize) -> Result<Vec<u8>, GetCommandError> {
    struct BoundedJson {
        bytes: Vec<u8>,
        limit: usize,
        exceeded: Option<usize>,
    }
    impl Write for BoundedJson {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let observed = self.bytes.len().saturating_add(bytes.len());
            if observed > self.limit {
                self.exceeded = Some(observed);
                return Err(io::Error::from(io::ErrorKind::FileTooLarge));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = BoundedJson {
        bytes: Vec::new(),
        limit,
        exceeded: None,
    };
    if let Err(error) = serde_json::to_writer(&mut writer, data) {
        return Err(match writer.exceeded {
            Some(observed) => GetCommandError::ByteLimit { limit, observed },
            None => GetCommandError::Json(error),
        });
    }
    Ok(writer.bytes)
}

/// Fully buffered content or native JSON receipt awaiting blocking delivery.
///
/// Acquisition never writes a prefix. A caller cancelled after acquisition must
/// not start delivery; the same applies if its whole-command deadline elapsed.
/// Once a stream/file write starts, cancellation and I/O errors cannot promise
/// rollback; exact confirmed effects remain inspectable.
#[derive(Debug)]
pub struct GetDelivery {
    output: PreparedOutput,
    bytes: Vec<u8>,
    format: GetFormat,
    media_type: String,
    data: Value,
    complete: bool,
}

impl GetDelivery {
    /// Inspect complete encoded bytes before choosing to perform local effects.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Selected advertised presentation.
    pub fn format(&self) -> GetFormat {
        self.format
    }

    /// Actual delivered media type, not a renamed native schema identifier.
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    /// Native unprojected data/evidence retained even after local projection.
    pub fn data(&self) -> &Value {
        &self.data
    }

    /// Whether this is complete content rather than an incomplete JSON receipt.
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Exact resolved filename, or `None` for stdout.
    pub fn output_path(&self) -> Option<&Path> {
        match &self.output {
            PreparedOutput::Stdout => None,
            PreparedOutput::File(output) => Some(output.path()),
        }
    }

    /// Write and flush exact bytes on the caller's stdout in blocking context.
    ///
    /// Adds no newline or logger output and never redirects a file request.
    ///
    /// # Errors
    /// Retains original I/O error, failure stage and confirmed bytes written.
    pub fn write_to(self, stdout: &mut (impl Write + ?Sized)) -> Result<usize, GetCommandError> {
        if !matches!(self.output, PreparedOutput::Stdout) {
            return Err(GetCommandError::DeliveryMismatch);
        }
        let mut written = 0;
        while written < self.bytes.len() {
            match stdout.write(&self.bytes[written..]) {
                Ok(0) => {
                    return Err(GetStdoutError {
                        stage: GetStdoutStage::Write,
                        bytes_written: written,
                        io_error: io::Error::from(io::ErrorKind::WriteZero),
                    }
                    .into());
                }
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(io_error) => {
                    return Err(GetStdoutError {
                        stage: GetStdoutStage::Write,
                        bytes_written: written,
                        io_error,
                    }
                    .into());
                }
            }
        }
        stdout.flush().map_err(|io_error| GetStdoutError {
            stage: GetStdoutStage::Flush,
            bytes_written: written,
            io_error,
        })?;
        Ok(written)
    }

    /// Write exact stdout bytes using the same blocking boundary as [`Self::write_to`].
    ///
    /// # Errors
    /// Preserves the same destination check and original partial-write evidence.
    pub fn write_stdout(
        self,
        stdout: &mut (impl Write + ?Sized),
    ) -> Result<usize, GetCommandError> {
        self.write_to(stdout)
    }

    /// Commit the exact preflighted file in the caller's blocking context.
    ///
    /// No directory, temporary file, inferred extension or stdout is produced.
    /// Creation is exclusive, including when another writer races preflight.
    ///
    /// # Errors
    /// Preserves native creation/write/flush and possibly-partial file effects.
    pub fn commit_file(self) -> Result<ExactOutputReceipt, GetCommandError> {
        match self.output {
            PreparedOutput::File(output) => Ok(output.commit(&self.bytes)?),
            PreparedOutput::Stdout => Err(GetCommandError::DeliveryMismatch),
        }
    }
}

/// Boundary where an explicit stdout delivery failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GetStdoutStage {
    /// A write call failed or accepted zero bytes.
    Write,
    /// All writes completed but the caller's flush failed.
    Flush,
}

/// Stdout failure with precise partial-write evidence and original native cause.
#[derive(Debug, thiserror::Error)]
#[error("GET stdout delivery failed; inspect stage and bytes_written")]
pub struct GetStdoutError {
    /// Write or flush boundary that failed.
    pub stage: GetStdoutStage,
    /// Bytes accepted by completed write calls before failure.
    pub bytes_written: usize,
    /// Original caller-stream error; never parsed for provider classification.
    #[source]
    pub io_error: io::Error,
}
