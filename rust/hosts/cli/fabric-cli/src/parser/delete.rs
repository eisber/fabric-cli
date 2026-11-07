// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The additive v2 delete grammar; the source `rm`/`del` leaves are unchanged.

use clap::{Arg, ArgAction, ArgMatches, Command, error::ErrorKind};
use std::{ffi::OsString, time::Duration};

/// One bounded v2 delete invocation, without credentials or ambient state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteCommand {
    /// One exact absolute typed GUID path, never a name or collection.
    pub path: String,
    /// Explicit consent to an irreversible provider delete; defaults to false.
    pub permanent: bool,
    /// Recognized for explicit capability rejection before any write.
    pub recursive: bool,
    /// Recognized optimistic-concurrency requirement; native Notebook rejects it.
    pub if_match: Option<String>,
    /// Request reconciliation rather than returning provider acceptance.
    pub wait: bool,
    /// Whole-command budget, enforced by the caller, including preflight/approval.
    pub timeout: Duration,
    /// JSON-only envelope format.
    pub format: String,
    /// JMESPath over `data` only, never status, metadata or errors.
    pub project: Option<String>,
    /// `-` for stdout, otherwise exactly one local file to reserve without clobbering.
    pub output: String,
    /// Optional assertion about the profile already selected by the caller.
    pub profile: Option<String>,
    /// Explicit legacy global override; incompatible text must be rejected.
    pub output_format: Option<String>,
}

impl DeleteCommand {
    /// Decode this leaf's matches; performs no filesystem or service access.
    ///
    /// # Errors
    /// Rejects missing values and nonpositive, excessive or malformed timeouts.
    /// Semantic path/capability/query checks belong to [`crate::delete::preflight`].
    pub fn from_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let invalid = || clap::Error::raw(ErrorKind::InvalidValue, super::INVALID_ARGUMENTS);
        let wait = *matches.get_one::<bool>("wait").ok_or_else(invalid)?;
        Ok(Self {
            path: matches
                .get_one::<String>("path")
                .cloned()
                .ok_or_else(invalid)?,
            permanent: *matches.get_one::<bool>("permanent").ok_or_else(invalid)?,
            recursive: *matches.get_one::<bool>("recursive").ok_or_else(invalid)?,
            if_match: matches.get_one::<String>("if_match").cloned(),
            wait,
            timeout: matches
                .get_one::<String>("timeout")
                .map(|value| parse_timeout(value).map_err(|_| invalid()))
                .transpose()?
                .unwrap_or(Duration::from_secs(if wait { 300 } else { 30 })),
            format: matches
                .get_one::<String>("format")
                .cloned()
                .ok_or_else(invalid)?,
            project: matches.get_one::<String>("project").cloned(),
            output: matches
                .get_one::<String>("output")
                .cloned()
                .ok_or_else(invalid)?,
            profile: matches.get_one::<String>("profile").cloned(),
            output_format: matches.get_one::<String>("output_format").cloned(),
        })
    }
}

/// Decode leaf matches using the same API as [`DeleteCommand::from_matches`].
///
/// # Errors
/// Retains invalid argument classification without printing supplied values.
pub fn from_matches(matches: &ArgMatches) -> Result<DeleteCommand, clap::Error> {
    DeleteCommand::from_matches(matches)
}

pub(super) fn normalize(args: &[OsString], root: usize) -> Vec<OsString> {
    let mut result = args[..=root].to_vec();
    let mut index = root + 1;
    while let Some(value) = args.get(index) {
        if value == "--" {
            result.extend_from_slice(&args[index..]);
            break;
        }
        if matches!(
            value.to_str(),
            Some("--permanent" | "--recursive" | "--wait")
        ) && let Some(boolean) = args.get(index + 1).and_then(|value| value.to_str())
            && matches!(boolean, "true" | "false")
        {
            result.push(format!("{}={boolean}", value.to_string_lossy()).into());
            index += 2;
        } else {
            result.push(value.clone());
            index += 1;
        }
    }
    result
}

/// Register only the new delete leaf on the caller's root command.
pub fn register(command: Command) -> Command {
    command.subcommand(
        Command::new("delete")
            .about("Delete one exact Notebook with explicit host approval (v2 preview)")
            .args_override_self(false)
            .infer_long_args(false)
            .arg(Arg::new("path").required(true).num_args(1))
            .arg(boolean("permanent").help("Explicit irreversible-delete opt-in (default false)"))
            .arg(boolean("recursive").help("Recognized but true is unsupported"))
            .arg(Arg::new("if_match").long("if-match").num_args(1))
            .arg(boolean("wait").help("Wait for provider completion (default false)"))
            .arg(
                Arg::new("timeout")
                    .long("timeout")
                    .value_parser(timeout_value)
                    .help("Positive integer with ms/s/m/h suffix, at most 24h; default 30s or 5m with --wait"),
            )
            .arg(Arg::new("format").long("format").value_parser(["json"]).default_value("json"))
            .arg(Arg::new("project").long("project").short('q').num_args(1))
            .arg(Arg::new("output").long("output").short('o').num_args(1).default_value("-"))
            .arg(Arg::new("profile").long("profile").num_args(1))
            .after_help("Initial admitted target grammar:\n  /<workspace-guid>.Workspace/<item-guid>.Notebook\nBoth GUIDs must use the exact 8-4-4-4-12 hexadecimal spelling. The provider enforces the Notebook type through the /notebooks/{id} endpoint, not through the caller's suffix alone. Names, root, wildcards, cells, docs/src paths, other types and multiple targets are rejected.\nThe current native capability requires --permanent and Workspace Admin. Default recoverable deletion is unsupported until an authoritative recovery contract is available. Permanent requests do not guarantee physical erasure. --permanent does not replace explicit host approval. Defensive 202 handling does not claim that this endpoint advertises long-running operations.\nBoolean options accept true/false values or a bare true flag. Only JSON is supported; --output_format text is incompatible. --project applies only to data. Local output uses create-new/no-clobber reservation before mutation."),
    )
}

fn boolean(name: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .action(ArgAction::Set)
        .value_parser(clap::value_parser!(bool))
        .num_args(0..=1)
        .require_equals(true)
        .default_missing_value("true")
        .default_value("false")
}

fn timeout_value(value: &str) -> Result<String, String> {
    parse_timeout(value).map(|_| value.to_owned())
}

/// Parse a bounded whole-command timeout without accessing a clock.
///
/// # Errors
/// Requires an unsigned positive integer and `ms`, `s`, `m` or `h`, at most 24h.
pub fn parse_timeout(value: &str) -> Result<Duration, String> {
    let invalid =
        || "Timeout requires a positive integer with ms/s/m/h suffix, at most 24h".to_owned();
    let (number, factor) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3_600_000)
    } else {
        return Err(invalid());
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let milliseconds = number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(factor))
        .filter(|value| *value > 0 && *value <= 86_400_000)
        .ok_or_else(invalid)?;
    Ok(Duration::from_millis(milliseconds))
}
