// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Reusable CLI v2 create arguments and pre-dispatch diagnostics.
//!
//! This module performs no I/O, authentication, resolution, or mutation and does
//! not mount an executable command. The host resolves the destination, supplies
//! target capabilities and its schema validator to the shared SDK planner,
//! arbitrates stdin, and passes the total timeout budget to its execution engine.
//! Collection names and types come from the input body, never from suffix guesses
//! or a `--name` flag. Diagnostic completeness hints do not select SDK actions.

use clap::{
    Arg, ArgAction, ArgMatches, Command,
    builder::{NonEmptyStringValueParser, OsStringValueParser, TypedValueParser},
    error::ErrorKind,
};
use fabric_sdk_items::create::{
    CreateError, CreateInputValidator, CreatePlan, CreateTarget, Preflight, plan_create,
};
use serde_json::Value;
use std::{
    ffi::{OsStr, OsString},
    fmt,
    path::PathBuf,
    time::Duration,
};

/// Shared SDK policy and capabilities; this surface defines no CLI equivalents.
pub use fabric_sdk_items::create::{ConflictPolicy, CreateCapabilities};

const MAX_TIMEOUT_MILLIS: u64 = 86_400_000;

/// The explicit source of advertised native content, including native bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputSource {
    /// The host must acquire stdin exclusively before reading.
    Stdin,
    /// A native path, retained without UTF-8 conversion or filesystem access.
    File(PathBuf),
}

/// Where the host should write result data and unprojected operation metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputDestination {
    /// Standard output, independent of whether input also uses stdin.
    Stdout,
    /// A native path; parsing neither opens nor creates it.
    File(PathBuf),
}

/// Result serialization, not the native content schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    /// JSON result serialization.
    Json,
}

/// A parsed request with no implicit reads, prompts, or remote operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateCommand {
    /// One exact destination or parent collection, for the typed resolver.
    pub destination: OsString,
    /// Explicit content source, if supplied.
    pub input: Option<InputSource>,
    /// Whether the host should wait for completion.
    pub wait: bool,
    /// Existing-target behavior, including the implicit error policy.
    pub policy: ConflictPolicy,
    /// Result serialization, independent of native content.
    pub format: OutputFormat,
    /// Result destination; defaults to stdout.
    pub output: OutputDestination,
    /// Projection expression for result data, evaluated by the host.
    ///
    /// IDs, effects, and operation handles must remain outside projection.
    pub project: Option<String>,
    /// Explicit named profile, selected by the host.
    pub profile: Option<String>,
    /// Total operation budget, not a per-request timeout or a running timer.
    pub timeout: Duration,
}

impl CreateCommand {
    /// Delegate action selection and input validation to the shared SDK planner.
    ///
    /// The host resolves this command's destination into the exact `target` and
    /// `preflight`, and supplies its trusted target/schema validator. `supplied`
    /// is the actual input for the SDK's JSON planning boundary, preserving
    /// omitted versus explicitly supplied content. This method neither reads a
    /// file nor converts advertised native bytes into arbitrary JSON.
    ///
    /// The SDK alone decides whether overwrite means creation or replacement,
    /// validates supplied input even for skip, and enforces completeness only
    /// when replacing an existing target. Stdin ownership remains a separate
    /// host check through [`Self::validate_stdin`].
    ///
    /// # Errors
    /// Returns [`plan_create`] errors unchanged, without inspecting provider text
    /// or claiming that any mutation was dispatched.
    pub fn plan(
        &self,
        target: CreateTarget,
        preflight: Preflight,
        capabilities: &CreateCapabilities,
        supplied: Option<Value>,
        validator: &dyn CreateInputValidator,
    ) -> Result<CreatePlan, CreateError> {
        plan_create(
            self.policy,
            target,
            preflight,
            capabilities,
            supplied,
            validator,
        )
    }

    /// Check stdin ownership without attempting to read it.
    ///
    /// `other_consumer` means another host component already requires stdin.
    /// Writing the result to stdout does not consume stdin.
    ///
    /// # Errors
    /// Returns a conflict only when this request and another consumer need stdin.
    pub fn validate_stdin(&self, other_consumer: bool) -> Result<(), DiagnosticFact> {
        if self.input == Some(InputSource::Stdin) && other_consumer {
            Err(DiagnosticFact::StdinConflict)
        } else {
            Ok(())
        }
    }
}

/// Host-established content hints used only to gate diagnostic suggestions.
///
/// These are not validation tokens and never affect [`CreateCommand::plan`].
/// The SDK planner and target-specific validator alone enforce input validity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputState {
    /// No native content is available.
    Missing,
    /// Content exists but does not contain all advertised replacement content.
    Incomplete,
    /// All advertised content is present; unrelated settings need not be present.
    Complete,
}

/// Construct the standalone create schema, also usable as a parent subcommand.
///
/// Long-option inference and self-overrides are disabled. `--force` conflicts
/// with every explicit `--if-exists`; the implicit default is applied only after
/// Clap validation so it cannot produce a false conflict.
pub fn command() -> Command {
    Command::new("create")
        .about("Create a target from its advertised native content")
        .long_about(
            "Create one destination or create within a parent collection. The typed \
             resolver determines the advertised schema or native media; collection \
             name and type come from the JSON body. Existing targets fail by default. Overwrite creates \
             a missing target when permitted; replacing an existing target requires \
             complete advertised content while retaining the ID and unrelated settings. \
             Policies are subject to resolved target capabilities.",
        )
        .infer_long_args(false)
        .infer_subcommands(false)
        .args_override_self(false)
        .arg(
            Arg::new("destination")
                .value_name("destination-or-parent-collection")
                .required(true)
                .num_args(1)
                .value_parser(OsStringValueParser::new().try_map(validate_destination))
                .help("Exact destination or parent collection; use -- before flag-like paths"),
        )
        .arg(
            Arg::new("input")
                .long("input")
                .value_name("FILE|-")
                .num_args(1)
                .value_parser(OsStringValueParser::new())
                .help("JSON or an advertised native file, or - for host-owned stdin"),
        )
        .arg(
            Arg::new("wait")
                .long("wait")
                .action(ArgAction::SetTrue)
                .help("Wait for completion within the total timeout budget"),
        )
        .arg(
            Arg::new("force")
                .long("force")
                .action(ArgAction::SetTrue)
                .conflicts_with("if-exists")
                .help("Use overwrite; replacement requires complete advertised native content"),
        )
        .arg(
            Arg::new("if-exists")
                .long("if-exists")
                .value_name("POLICY")
                .num_args(1)
                .value_parser(["error", "overwrite", "skip", "duplicate"])
                .help("Existing-target policy (default: error)"),
        )
        .arg(
            Arg::new("format")
                .long("format")
                .value_name("FORMAT")
                .num_args(1)
                .value_parser(["json"])
                .help("Result format: json (not the native content schema)"),
        )
        .arg(
            Arg::new("output")
                .long("output")
                .short('o')
                .value_name("FILE|-")
                .num_args(1)
                .value_parser(OsStringValueParser::new())
                .help("Result file, or - for stdout (default)"),
        )
        .arg(
            Arg::new("project")
                .long("project")
                .short('q')
                .value_name("EXPR")
                .num_args(1)
                .help("Project the result using the host's expression evaluator"),
        )
        .arg(
            Arg::new("profile")
                .long("profile")
                .value_name("NAME")
                .num_args(1)
                .value_parser(NonEmptyStringValueParser::new())
                .help("Select a named profile without authenticating during parsing"),
        )
        .arg(
            Arg::new("timeout")
                .long("timeout")
                .value_name("DURATION")
                .num_args(1)
                .value_parser(clap::builder::ValueParser::new(parse_timeout))
                .help("Total positive integer ms/s/m/h budget, at most 24h; default 30s or 5m with --wait"),
        )
        .after_help(
            "Examples:\n  fab-cli create /example/target --input notebook.py\n  \
             fab-cli create /example/target --input content.json --force\n  \
             fab-cli create /example/collection --input new-resource.json --if-exists duplicate",
        )
}

/// Parse argv including a leading command/program token, without I/O.
///
/// For example, `parse_args(["create", "/example/target", "--input", "-"])`.
/// Shell quoting is the caller's responsibility; each token is preserved.
///
/// # Errors
/// Returns Clap help or validation errors without printing or exiting.
pub fn parse_args<I, T>(args: I) -> Result<CreateCommand, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    from_matches(&command().try_get_matches_from(args)?)
}

/// Convert matches produced by [`command`] into a typed request.
///
/// Pass the create subcommand's matches, not its parent's matches.
///
/// # Errors
/// Rejects incomplete or incompatible matches without panicking.
pub fn from_matches(matches: &ArgMatches) -> Result<CreateCommand, clap::Error> {
    let destination = value::<OsString>(matches, "destination")?.ok_or_else(invalid_matches)?;
    let destination = validate_destination(destination)
        .map_err(|_| clap::Error::raw(ErrorKind::InvalidValue, "Invalid create destination"))?;
    let force = value::<bool>(matches, "force")?.unwrap_or(false);
    let policy = value::<String>(matches, "if-exists")?;
    if force && policy.is_some() {
        return Err(clap::Error::raw(
            ErrorKind::ArgumentConflict,
            "--force conflicts with explicit --if-exists",
        ));
    }
    let policy = if force {
        ConflictPolicy::Overwrite
    } else {
        match policy.as_deref() {
            None => ConflictPolicy::default(),
            Some(policy) => ConflictPolicy::parse(policy).map_err(|_| invalid_matches())?,
        }
    };
    if !matches!(
        value::<String>(matches, "format")?.as_deref(),
        None | Some("json")
    ) {
        return Err(invalid_matches());
    }
    let wait = value::<bool>(matches, "wait")?.unwrap_or(false);
    Ok(CreateCommand {
        destination,
        input: value::<OsString>(matches, "input")?.map(|path| {
            if path == OsStr::new("-") {
                InputSource::Stdin
            } else {
                InputSource::File(PathBuf::from(path))
            }
        }),
        wait,
        policy,
        format: OutputFormat::Json,
        output: match value::<OsString>(matches, "output")? {
            None => OutputDestination::Stdout,
            Some(path) if path == OsStr::new("-") => OutputDestination::Stdout,
            Some(path) => OutputDestination::File(PathBuf::from(path)),
        },
        project: value(matches, "project")?,
        profile: value(matches, "profile")?,
        timeout: value(matches, "timeout")?
            .unwrap_or_else(|| Duration::from_secs(if wait { 300 } else { 30 })),
    })
}

fn value<T: Clone + Send + Sync + 'static>(
    matches: &ArgMatches,
    name: &str,
) -> Result<Option<T>, clap::Error> {
    matches
        .try_get_one::<T>(name)
        .map(|value| value.cloned())
        .map_err(|_| invalid_matches())
}

fn invalid_matches() -> clap::Error {
    clap::Error::raw(ErrorKind::InvalidValue, "Invalid create argument matches")
}

fn validate_destination(destination: OsString) -> Result<OsString, &'static str> {
    if destination.is_empty()
        || destination.to_string_lossy().chars().any(char::is_control)
        || destination
            .as_encoded_bytes()
            .iter()
            .any(u8::is_ascii_control)
    {
        Err("Destination must be nonempty and contain no control characters")
    } else {
        Ok(destination)
    }
}

/// A timeout must be a positive whole number of ms/s/m/h, at most 24 hours.
///
/// The error never contains the supplied value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeoutParseError;

impl fmt::Display for TimeoutParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("Timeout must be a positive integer followed by ms, s, m, or h, at most 24h")
    }
}

impl std::error::Error for TimeoutParseError {}

/// Parse a bounded total duration without fractional values or truncation.
///
/// # Errors
/// Rejects missing/unknown units, signs, whitespace, zero, fractions, overflow,
/// and durations above 24 hours.
pub fn parse_timeout(text: &str) -> Result<Duration, TimeoutParseError> {
    let (digits, multiplier) = if let Some(digits) = text.strip_suffix("ms") {
        (digits, 1)
    } else if let Some(digits) = text.strip_suffix('s') {
        (digits, 1_000)
    } else if let Some(digits) = text.strip_suffix('m') {
        (digits, 60_000)
    } else if let Some(digits) = text.strip_suffix('h') {
        (digits, 3_600_000)
    } else {
        return Err(TimeoutParseError);
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TimeoutParseError);
    }
    let millis = digits
        .parse::<u64>()
        .ok()
        .and_then(|amount| amount.checked_mul(multiplier))
        .filter(|millis| (1..=MAX_TIMEOUT_MILLIS).contains(millis))
        .ok_or(TimeoutParseError)?;
    Ok(Duration::from_millis(millis))
}

/// Typed facts, never inferred from provider text or error-message substrings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticFact {
    /// A named collision confirmed before dispatch; no mutation was attempted.
    AlreadyExists {
        /// The exact resolved target identity to identify in the human message.
        target: OsString,
    },
    /// Resolution produced multiple targets; force cannot disambiguate them.
    AmbiguousTarget {
        /// The unresolved target selector.
        target: OsString,
    },
    /// The resolved target does not advertise the effective policy.
    UnsupportedPolicy {
        /// Effective policy, whether explicit or defaulted.
        policy: ConflictPolicy,
    },
    /// The host requires content but has none.
    InputRequired,
    /// Overwrite content omits advertised replacement fields.
    IncompleteReplacement,
    /// Stdin was requested by another host component as well.
    StdinConflict,
}

/// Keep typed facts separate from untrusted, already-rendered provider text.
///
/// This deliberately has no `Debug` implementation. Provider text is neither
/// inspected nor copied into diagnostics and must not be used to manufacture a
/// pre-dispatch collision fact.
pub enum DiagnosticInput<'a> {
    /// A typed fact established by local validation or pre-dispatch resolution.
    Fact(&'a DiagnosticFact),
    /// Untrusted provider text, which may contain secrets or uncertain outcomes.
    ProviderText(&'a str),
}

/// A process invocation suggestion, never a shell command string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSuggestion {
    /// Executable name, not shell syntax.
    pub program: &'static str,
    /// Exact argv tokens; consumers must not join these for shell execution.
    pub args: Vec<OsString>,
}

/// Human-safe guidance plus separately structured retry arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateDiagnostic {
    /// Stable error code.
    pub code: &'static str,
    /// Human message, with target identities debug-escaped.
    pub message: String,
    /// Generic guidance, containing no interpolated command or input content.
    pub guidance: Vec<&'static str>,
    /// Capability- and completeness-gated process invocation suggestions.
    ///
    /// These retain original native paths, queries, and profile selection.
    /// They are user data, not log entries or automatically executable actions.
    pub suggestions: Vec<CommandSuggestion>,
}

/// Render typed failures without shell interpolation or provider-text guessing.
///
/// Only a known pre-dispatch collision claims that no change was made. A force
/// suggestion requires both advertised overwrite support and complete content
/// from the original explicit input. No suggestion is emitted for ambiguity or
/// a provider failure, whose dispatch outcome may be uncertain.
pub fn render_diagnostic(
    input: DiagnosticInput<'_>,
    request: &CreateCommand,
    capabilities: &CreateCapabilities,
    content: InputState,
) -> CreateDiagnostic {
    let fact = match input {
        DiagnosticInput::Fact(fact) => fact,
        DiagnosticInput::ProviderText(_) => {
            return diagnostic(
                "ProviderError",
                "The provider reported a create failure. Use sanitized host details to reconcile the outcome.",
            );
        }
    };
    match fact {
        DiagnosticFact::AlreadyExists { target } => {
            let mut result = diagnostic(
                "AlreadyExists",
                format!("Target {target:?} already exists. No change was made."),
            );
            result
                .guidance
                .push("Consult update help for this target's supported modes and separate input contract.");
            if capabilities.replacement.is_some()
                && content == InputState::Complete
                && let Some(source) = &request.input
            {
                result.guidance.push(
                    "To replace complete advertised content while retaining the ID and unrelated settings, retry create with the same --input and --force.",
                );
                result.suggestions.push(force_suggestion(request, source));
            }
            result
        }
        DiagnosticFact::AmbiguousTarget { target } => diagnostic(
            "AmbiguousTarget",
            format!(
                "Target {target:?} is ambiguous. Select an unambiguous destination before retrying."
            ),
        ),
        DiagnosticFact::UnsupportedPolicy { policy } => diagnostic(
            "UnsupportedPolicy",
            format!(
                "The resolved target does not support the '{}' create policy.",
                policy.as_str()
            ),
        ),
        DiagnosticFact::InputRequired => diagnostic(
            "InputRequired",
            "Provide advertised native content with --input FILE or --input -. Create does not prompt for input.",
        ),
        DiagnosticFact::IncompleteReplacement => diagnostic(
            "IncompleteReplacement",
            "Replacing an existing target requires complete advertised native content. Supply the complete replacement with --input.",
        ),
        DiagnosticFact::StdinConflict => diagnostic(
            "StdinConflict",
            "Create input and another host component both require stdin. Use an input file or release the other stdin consumer.",
        ),
    }
}

fn diagnostic(code: &'static str, message: impl Into<String>) -> CreateDiagnostic {
    CreateDiagnostic {
        code,
        message: message.into(),
        guidance: Vec::new(),
        suggestions: Vec::new(),
    }
}

fn force_suggestion(request: &CreateCommand, source: &InputSource) -> CommandSuggestion {
    let input = match source {
        InputSource::Stdin => OsStr::new("-"),
        InputSource::File(path) => path.as_os_str(),
    };
    let output = match &request.output {
        OutputDestination::Stdout => OsStr::new("-"),
        OutputDestination::File(path) => path.as_os_str(),
    };
    let mut args = vec![
        OsString::from("create"),
        OsString::from("--force"),
        attached_value("--input", input),
        OsString::from("--format=json"),
        attached_value("--output", output),
        OsString::from(format!("--timeout={}ms", request.timeout.as_millis())),
    ];
    if request.wait {
        args.push(OsString::from("--wait"));
    }
    if let Some(project) = &request.project {
        args.push(attached_value("--project", OsStr::new(project)));
    }
    if let Some(profile) = &request.profile {
        args.push(attached_value("--profile", OsStr::new(profile)));
    }
    args.push(OsString::from("--"));
    args.push(request.destination.clone());
    CommandSuggestion {
        program: "fab-cli",
        args,
    }
}

fn attached_value(flag: &str, value: &OsStr) -> OsString {
    let mut argument = OsString::from(flag);
    argument.push("=");
    argument.push(value);
    argument
}
