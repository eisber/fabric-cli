// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared local argument schema for common and facade entry points.

use crate::OntologyVerb;
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use std::{ffi::OsString, fmt, num::NonZeroUsize, path::PathBuf, time::Duration};

/// Explicit local input; no file is opened by argument parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputSource {
    /// One advertised stdin input.
    Stdin,
    /// A local path, distinct from the remote target.
    File(PathBuf),
}

/// Explicit local destination; parsing does not reserve or truncate a file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputDestination {
    /// Selected result stream on stdout; diagnostics belong on stderr.
    Stdout,
    /// A file that the host must atomically reserve before remote work.
    File(PathBuf),
}

/// Explicit update semantics, not an inferred upsert mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateMode {
    /// Change only declared fields, preserving omitted fields.
    Patch,
    /// Replace the complete advertised writable representation.
    Replace,
}

/// Locally parsed CLI arguments, not a resolved or approved SDK operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OntologyArguments {
    /// Common operation referenced by either entry point.
    pub verb: OntologyVerb,
    /// Opaque explicit remote path for the accepted shared resolver.
    pub path: String,
    /// Explicit local input; body/media/schema validation remains required.
    pub input: Option<InputSource>,
    /// Explicit update mode.
    pub mode: Option<UpdateMode>,
    /// Local result destination, requiring host no-clobber preflight.
    pub output: OutputDestination,
    /// Optional configured profile, never an identity fallback.
    pub profile: Option<String>,
    /// Optional data-only projection, to be validated before dispatch.
    pub project: Option<String>,
    /// Explicit total budget; no unresolved numeric default is installed.
    pub timeout: Option<Duration>,
    /// Explicit serialized-data bound for get/list, before projection.
    pub max_bytes: Option<NonZeroUsize>,
    /// Explicit collection bound for list.
    pub limit: Option<NonZeroUsize>,
    /// Opaque continuation, requiring target/identity/snapshot binding.
    pub continuation: Option<String>,
    /// Observe only a supported newly submitted/existing operation.
    pub wait: bool,
    /// Explicit permanent intent for deletion, not host approval.
    pub permanent: bool,
    /// Explicit bounded subtree intent, never permission for unrelated cascades.
    pub recursive: bool,
    /// Condition that must be supported by the resolved backend, never ignored.
    pub if_match: Option<String>,
}

/// Sanitized local argument failure, never raw parser text or supplied values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentError {
    /// Unknown command/option, missing value, conflict or invalid typed argument.
    InvalidSyntax,
    /// Empty or whitespace-only remote target.
    EmptyPath,
    /// The selected link/move parameter contract is not yet accepted.
    UnresolvedCommandContract,
}

impl fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidSyntax => "Invalid Ontology arguments; inspect the operation schema.",
            Self::EmptyPath => "An explicit nonempty remote target path is required.",
            Self::UnresolvedCommandContract => {
                "Ontology link/move input contracts require coordinator acceptance."
            }
        })
    }
}

impl std::error::Error for ArgumentError {}

/// Builds the schema used by either common registration or the facade alias.
///
/// This does not register a root command, operation or MCP tool. Native schema
/// validation, path resolution, approval and execution are separate gates.
pub fn operation_command(verb: OntologyVerb) -> Command {
    let mut command = Command::new(verb.common_name())
        .disable_help_flag(true)
        .arg(Arg::new("path").required(true))
        .arg(Arg::new("format").long("format").value_parser(["json"]))
        .arg(
            Arg::new("output")
                .long("output")
                .short('o')
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(Arg::new("profile").long("profile"))
        .arg(Arg::new("project").long("project").short('q'))
        .arg(
            Arg::new("timeout")
                .long("timeout")
                .value_parser(parse_duration),
        );

    if matches!(
        verb,
        OntologyVerb::Create | OntologyVerb::Update | OntologyVerb::Link | OntologyVerb::Move
    ) {
        command = command.arg(
            Arg::new("input")
                .long("input")
                .required(verb != OntologyVerb::Create)
                .value_parser(value_parser!(PathBuf)),
        );
    }
    if verb == OntologyVerb::Update {
        command = command.arg(
            Arg::new("mode")
                .long("mode")
                .required(true)
                .value_parser(["patch", "replace"]),
        );
    }
    if matches!(
        verb,
        OntologyVerb::Get | OntologyVerb::Create | OntologyVerb::Update | OntologyVerb::Delete
    ) {
        command = command.arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue));
    }
    if matches!(verb, OntologyVerb::Get | OntologyVerb::List) {
        command = command.arg(
            Arg::new("max-bytes")
                .long("max-bytes")
                .value_parser(value_parser!(NonZeroUsize)),
        );
    }
    if verb == OntologyVerb::List {
        command = command
            .arg(
                Arg::new("limit")
                    .long("limit")
                    .value_parser(value_parser!(NonZeroUsize)),
            )
            .arg(Arg::new("continuation").long("continuation"));
    }
    if verb == OntologyVerb::Delete {
        command = command
            .arg(
                Arg::new("permanent")
                    .long("permanent")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                Arg::new("recursive")
                    .long("recursive")
                    .action(ArgAction::SetTrue),
            );
    }
    if matches!(verb, OntologyVerb::Update | OntologyVerb::Delete) {
        command = command.arg(Arg::new("if-match").long("if-match"));
    }
    command
}

/// Builds only the Ontology group, sharing each common operation schema.
pub fn ontology_command() -> Command {
    OntologyVerb::ALL.into_iter().fold(
        Command::new("ontology")
            .disable_help_flag(true)
            .disable_help_subcommand(true)
            .subcommand_required(true),
        |command, verb| command.subcommand(operation_command(verb)),
    )
}

/// Parses tokens beginning with the common verb's program name.
///
/// # Errors
/// Returns sanitized syntax/contract failures without echoing argument values.
pub fn parse_common_args<I, T>(
    verb: OntologyVerb,
    args: I,
) -> Result<OntologyArguments, ArgumentError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = operation_command(verb)
        .try_get_matches_from(args)
        .map_err(|_| ArgumentError::InvalidSyntax)?;
    from_matches(verb, &matches)
}

/// Parses tokens beginning with `ontology`, then one of its seven aliases.
///
/// # Errors
/// Uses exactly the same validation as the common-operation entry point.
pub fn parse_facade_args<I, T>(args: I) -> Result<OntologyArguments, ArgumentError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = ontology_command()
        .try_get_matches_from(args)
        .map_err(|_| ArgumentError::InvalidSyntax)?;
    let (name, matches) = matches.subcommand().ok_or(ArgumentError::InvalidSyntax)?;
    let verb = OntologyVerb::from_name(name).ok_or(ArgumentError::InvalidSyntax)?;
    from_matches(verb, matches)
}

fn from_matches(
    verb: OntologyVerb,
    matches: &ArgMatches,
) -> Result<OntologyArguments, ArgumentError> {
    let path = matches
        .get_one::<String>("path")
        .ok_or(ArgumentError::InvalidSyntax)?;
    if path.trim().is_empty() {
        return Err(ArgumentError::EmptyPath);
    }
    let input_path = optional::<PathBuf>(matches, "input");
    let output_path = optional::<PathBuf>(matches, "output");
    if input_path
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
        || output_path
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
    {
        return Err(ArgumentError::InvalidSyntax);
    }
    let input = input_path.map(|path| {
        if path.as_os_str() == "-" {
            InputSource::Stdin
        } else {
            InputSource::File(path)
        }
    });
    let output = match output_path {
        Some(path) if path.as_os_str() != "-" => OutputDestination::File(path),
        _ => OutputDestination::Stdout,
    };
    let mode = match optional::<String>(matches, "mode").as_deref() {
        Some("patch") => Some(UpdateMode::Patch),
        Some("replace") => Some(UpdateMode::Replace),
        None => None,
        _ => return Err(ArgumentError::InvalidSyntax),
    };
    let profile = optional::<String>(matches, "profile");
    let project = optional::<String>(matches, "project");
    let continuation = optional::<String>(matches, "continuation");
    let if_match = optional::<String>(matches, "if-match");
    if [&profile, &project, &continuation, &if_match]
        .into_iter()
        .any(|value| value.as_ref().is_some_and(|value| value.trim().is_empty()))
    {
        return Err(ArgumentError::InvalidSyntax);
    }
    Ok(OntologyArguments {
        verb,
        path: path.clone(),
        input,
        mode,
        output,
        profile,
        project,
        timeout: optional(matches, "timeout"),
        max_bytes: optional(matches, "max-bytes"),
        limit: optional(matches, "limit"),
        continuation,
        wait: flag(matches, "wait"),
        permanent: flag(matches, "permanent"),
        recursive: flag(matches, "recursive"),
        if_match,
    })
}

fn optional<T: Clone + Send + Sync + 'static>(matches: &ArgMatches, id: &str) -> Option<T> {
    if matches.ids().any(|present| present.as_str() == id) {
        matches.get_one::<T>(id).cloned()
    } else {
        None
    }
}

fn flag(matches: &ArgMatches, id: &str) -> bool {
    optional::<bool>(matches, id).unwrap_or(false)
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let (number, factor) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3_600_000)
    } else {
        return Err("A positive integer duration with ms/s/m/h is required.".into());
    };
    let milliseconds = (!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| number.parse::<u64>().ok())
        .flatten()
        .and_then(|number| number.checked_mul(factor))
        .filter(|value| (1..=86_400_000).contains(value))
        .ok_or_else(|| "Duration must be positive and no greater than 24 hours.".to_owned())?;
    Ok(Duration::from_millis(milliseconds))
}
