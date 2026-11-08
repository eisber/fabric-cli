// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root/artifact metadata Update normalization over the same typed SDK request.
//!
//! This adapter does not select credentials, approve writes, install a runtime,
//! read files, or reserve output. Hosts supply those boundaries explicitly.

use crate::MutationError;
use clap::{Arg, ArgAction, Command, builder::OsStringValueParser, error::ErrorKind};
pub use fabric_sdk_items::update;
use serde_json::Value;
use std::{
    ffi::OsString,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use update::{
    PreparedUpdate, UpdateMode, UpdateOptions,
    contract::{ItemIdentity, ResolvedTarget, Target, TargetKind},
};

/// Explicit native input source; stdin is consumed only by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateInput {
    /// One bounded caller-owned stdin read.
    Stdin,
    /// One native file, without lossy Unicode conversion.
    File(PathBuf),
}

/// Parsed request, not permission or proof of resource existence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateCommand {
    /// One exact, UUID-addressed typed metadata target.
    pub target: ResolvedTarget,
    /// Required native JSON input.
    pub input: UpdateInput,
    /// Optional no-overwrite file; absent means stdout.
    pub output: Option<PathBuf>,
    /// Validated SDK options; no CLI-specific patch implementation exists.
    pub options: UpdateOptions,
    /// Local plan only; the host must never dispatch this invocation.
    pub plan: bool,
}

/// Common leaf schema, shared by root and advertised artifact facades.
pub fn command() -> Command {
    Command::new("update")
        .about("Patch exact existing Notebook or Lakehouse metadata")
        .infer_long_args(false)
        .args_override_self(false)
        .after_help("UUID-addressed metadata only. Null, unknown fields, replace, if-match and wait are unsupported. No definition replacement, pre-read/merge or optimistic locking. The host must authorize the exact unguarded write; --plan performs no remote write.")
        .arg(Arg::new("path").required(true).num_args(1))
        .arg(Arg::new("input").long("input").required(true).value_parser(OsStringValueParser::new()))
        .arg(Arg::new("mode").long("mode").required(true).value_parser(["patch","replace"]))
        .arg(Arg::new("if-match").long("if-match"))
        .arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue))
        .arg(Arg::new("plan").long("plan").action(ArgAction::SetTrue))
        .arg(Arg::new("timeout").long("timeout").value_parser(parse_timeout))
        .arg(Arg::new("format").long("format").value_parser(["json"]))
        .arg(Arg::new("output").long("output").short('o').value_parser(OsStringValueParser::new()))
        .arg(Arg::new("project").long("project").short('q'))
}

/// Standalone root and facade grammar; no v1 registrations are changed.
pub fn root_command() -> Command {
    Command::new("fab-cli-v2")
        .subcommand_required(true)
        .subcommand(command())
        .subcommand(
            Command::new("artifact")
                .subcommand_required(true)
                .subcommand(
                    Command::new("notebook")
                        .subcommand_required(true)
                        .subcommand(command()),
                )
                .subcommand(
                    Command::new("lakehouse")
                        .subcommand_required(true)
                        .subcommand(command()),
                ),
        )
}

/// Safe argument failure, retaining help versus invalid syntax classification.
#[derive(Debug)]
pub enum ArgumentError {
    /// Clap syntax kind; raw supplied values are never formatted.
    Syntax(ErrorKind),
    /// Invalid or unadvertised exact metadata target/options.
    Validation(MutationError),
}

impl std::fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid metadata update arguments; use update --help")
    }
}
impl std::error::Error for ArgumentError {}

/// Parse root or artifact syntax into the identical SDK target/options.
///
/// # Errors
/// Refuses names requiring discovery, untyped IDs, child/source views, globs,
/// wrong-owner facades and unsupported condition/replace/wait options locally.
pub fn parse<I, T>(args: I) -> Result<UpdateCommand, ArgumentError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = root_command()
        .try_get_matches_from(args)
        .map_err(|error| ArgumentError::Syntax(error.kind()))?;
    let (facade, leaf) = match matches.subcommand() {
        Some(("update", leaf)) => (None, leaf),
        Some(("artifact", artifacts)) => {
            let (name, owner) = artifacts.subcommand().ok_or_else(invalid_args)?;
            let (_, leaf) = owner.subcommand().ok_or_else(invalid_args)?;
            (
                Some(if name == "notebook" {
                    "Notebook"
                } else {
                    "Lakehouse"
                }),
                leaf,
            )
        }
        _ => return Err(invalid_args()),
    };
    let path = leaf.get_one::<String>("path").ok_or_else(invalid_args)?;
    let target = parse_target(path).map_err(ArgumentError::Validation)?;
    if facade.is_some_and(|owner| owner != target.selection().owner_type) {
        return Err(ArgumentError::Validation(MutationError::new(
            "WrongOwnerType",
            "Artifact facade does not match the addressed item",
        )));
    }
    let mode = if leaf.get_one::<String>("mode").map(String::as_str) == Some("patch") {
        UpdateMode::Patch
    } else {
        UpdateMode::Replace
    };
    let mut options = UpdateOptions::patch();
    options.mode = mode;
    options.if_match = leaf.get_one::<String>("if-match").cloned();
    options.wait = leaf.get_flag("wait");
    if options.mode != UpdateMode::Patch || options.if_match.is_some() || options.wait {
        return Err(ArgumentError::Validation(MutationError::new(
            "UnsupportedCapability",
            "Replace, conditional writes and observation are not advertised for metadata PATCH",
        )));
    }
    options.timeout_millis = *leaf.get_one::<u64>("timeout").unwrap_or(&30_000);
    options.project = leaf.get_one::<String>("project").cloned();
    if options
        .project
        .as_ref()
        .is_some_and(|value| fabric_cli_query::validate_json_projection(value).is_err())
    {
        return Err(invalid_args());
    }
    let input = leaf.get_one::<OsString>("input").ok_or_else(invalid_args)?;
    if input.is_empty() {
        return Err(invalid_args());
    }
    let input = if input == "-" {
        UpdateInput::Stdin
    } else {
        UpdateInput::File(input.into())
    };
    let output = leaf
        .get_one::<OsString>("output")
        .filter(|value| *value != "-")
        .map(PathBuf::from);
    if output
        .as_ref()
        .is_some_and(|value| value.as_os_str().is_empty())
    {
        return Err(invalid_args());
    }
    Ok(UpdateCommand {
        target,
        input,
        output,
        options,
        plan: leaf.get_flag("plan"),
    })
}

fn invalid_args() -> ArgumentError {
    ArgumentError::Validation(MutationError::new(
        "InvalidInput",
        "Invalid metadata update arguments",
    ))
}

/// Validate an exact typed UUID path without HTTP or name resolution.
///
/// Supports `/workspace-guid.Workspace/item-guid.Notebook` (or Lakehouse), and
/// `/workspaces/{guid}/notebooks/{guid}` (or lakehouses). Canonical receipt path
/// preserves the supplied spelling. A suffix is constrained by the native typed
/// endpoint, not a permission grant.
///
/// # Errors
/// All other path grammars/facets fail UnsupportedCapability before execution.
pub fn parse_target(path: &str) -> Result<ResolvedTarget, MutationError> {
    let unsupported = || {
        MutationError::new(
            "UnsupportedCapability",
            "Update requires an exact typed UUID metadata path",
        )
    };
    if !path.starts_with('/')
        || path.len() > 32 * 1024
        || path.contains(['\\', '%', '?', '#'])
        || path.chars().any(char::is_control)
    {
        return Err(unsupported());
    }
    let parts: Vec<_> = path[1..].split('/').collect();
    let (workspace, item, owner) = match parts.as_slice() {
        [workspace, item] => {
            let workspace = workspace
                .strip_suffix(".Workspace")
                .ok_or_else(unsupported)?;
            let (item, owner) = item.rsplit_once('.').ok_or_else(unsupported)?;
            (workspace, item, owner)
        }
        ["workspaces", workspace, "notebooks", item] => (*workspace, *item, "Notebook"),
        ["workspaces", workspace, "lakehouses", item] => (*workspace, *item, "Lakehouse"),
        _ => return Err(unsupported()),
    };
    if !matches!(owner, "Notebook" | "Lakehouse") {
        return Err(unsupported());
    }
    let identity = ItemIdentity::new(workspace, item).map_err(|_| unsupported())?;
    ResolvedTarget::new(
        path,
        identity,
        Target {
            kind: TargetKind::Item,
            owner_type: owner.into(),
            facet: "metadata".into(),
            representation: "json".into(),
            generation: None,
        },
        None,
    )
    .map_err(|_| unsupported())
}

impl UpdateCommand {
    /// Create the SDK plan after the host's bounded input read, without I/O.
    ///
    /// # Errors
    /// Rejects invalid input, nonabsolute cwd, unrepresentable output paths and
    /// SDK preparation failures. The host still must preflight/reserve output.
    pub fn prepare(&self, bytes: &[u8], cwd: &Path) -> Result<PreparedUpdate, MutationError> {
        if self
            .options
            .project
            .as_ref()
            .is_some_and(|value| fabric_cli_query::validate_json_projection(value).is_err())
        {
            return Err(MutationError::new(
                "InvalidInput",
                "Unsupported JSON data projection",
            ));
        }
        if !cwd.is_absolute() {
            return Err(MutationError::new(
                "InvalidInput",
                "Output working directory must be absolute",
            ));
        }
        let mut options = self.options.clone();
        if let Some(path) = &self.output {
            options.output_path = Some(
                cwd.join(path)
                    .to_str()
                    .ok_or_else(|| {
                        MutationError::new(
                            "InvalidInput",
                            "Output path cannot be represented in approval evidence",
                        )
                    })?
                    .into(),
            );
        }
        PreparedUpdate::new(self.target.clone(), bytes, options)
            .map_err(|error| MutationError::new("UpdatePreparationFailed", error.to_string()))
    }

    /// Caller-enforced total timeout, not a newly created timer.
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.options.timeout_millis)
    }

    /// Serialize and project only executed data, retaining all receipt metadata.
    ///
    /// # Errors
    /// The caller must retain `report` after any failure; output cannot roll back
    /// acceptance/completion. Failed/unknown results remain valid JSON receipts.
    pub fn write_report(
        &self,
        report: &update::UpdateReport,
        writer: &mut dyn Write,
    ) -> io::Result<()> {
        let mut document = serde_json::to_value(&report.receipt).map_err(io::Error::other)?;
        if let Some(expression) = &self.options.project
            && let Some(data) = document.get_mut("data")
        {
            *data = fabric_cli_query::project_json(data, expression).map_err(io::Error::other)?;
        }
        serde_json::to_writer(&mut *writer, &document).map_err(io::Error::other)?;
        writer.write_all(b"\n")
    }
}

fn parse_timeout(value: &str) -> Result<u64, &'static str> {
    let (digits, multiplier) = if let Some(value) = value.strip_suffix("ms") {
        (value, 1)
    } else if let Some(value) = value.strip_suffix('s') {
        (value, 1_000)
    } else if let Some(value) = value.strip_suffix('m') {
        (value, 60_000)
    } else if let Some(value) = value.strip_suffix('h') {
        (value, 3_600_000)
    } else {
        return Err("duration requires ms/s/m/h");
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("positive integer duration required");
    }
    digits
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(multiplier))
        .filter(|value| *value > 0 && *value <= 86_400_000)
        .ok_or("duration must be positive and at most 24h")
}
