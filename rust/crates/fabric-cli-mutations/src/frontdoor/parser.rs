// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::MutationError;
use clap::{Arg, ArgAction, ArgMatches, Command};

/// Arguments shared by the source resource leaves, without ambient configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceArgs {
    /// Original path tokens for the caller's existing context resolver.
    pub path: Vec<String>,
    /// Explicit source output override; absence uses the caller's configuration.
    pub output_format: Option<String>,
    /// Hidden source skill selection, retained rather than silently ignored.
    pub skill: Option<String>,
}

/// Typed resource-family input, independent of SDK endpoint argument types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceCommand {
    /// Check the resolved context's ID without an additional metadata request.
    Exists(ResourceArgs),
    /// Create a workspace, item, or folder.
    Mkdir {
        /// Shared source arguments.
        args: ResourceArgs,
        /// Source `-P` tokens; an explicit empty list requests parameter help.
        params: Vec<String>,
    },
    /// Remove an item or folder after source confirmation.
    Remove {
        /// Shared source arguments.
        args: ResourceArgs,
        /// Bypass the confirmation prompt.
        force: bool,
        /// Permanently remove an item.
        hard: bool,
    },
    /// Update one metadata property using source input normalization.
    Set {
        /// Shared source arguments.
        args: ResourceArgs,
        /// Scalar query; literal outer quotes are retained by Python.
        query: String,
        /// Variadic input; outer quotes are stripped after joining.
        input: Vec<String>,
        /// Bypass the confirmation prompt.
        force: bool,
    },
    /// Create a Lakehouse shortcut.
    Link {
        /// Shared source arguments.
        args: ResourceArgs,
        /// Case-sensitive source shortcut target type.
        shortcut_type: String,
        /// Optional OneLake target tokens.
        target: Option<Vec<String>>,
        /// Optional inline JSON target tokens, not a local filename.
        input: Option<Vec<String>>,
        /// Bypass confirmation and use GenerateUniqueName.
        force: bool,
    },
    /// Start a mirrored database after reading its current state.
    Start {
        /// Shared source arguments.
        args: ResourceArgs,
        /// Bypass confirmation, not the state check.
        force: bool,
    },
    /// Stop a mirrored database after reading its current state.
    Stop {
        /// Shared source arguments.
        args: ResourceArgs,
        /// Bypass confirmation, not the state check.
        force: bool,
    },
    /// Assign a selected capacity to a workspace.
    Assign {
        /// Selected capacity path and shared source arguments.
        args: ResourceArgs,
        /// Original target workspace tokens.
        workspace: Vec<String>,
        /// Bypass confirmation.
        force: bool,
    },
    /// Unassign a selected capacity after checking the actual workspace linkage.
    Unassign {
        /// Selected capacity path and shared source arguments.
        args: ResourceArgs,
        /// Original target workspace tokens.
        workspace: Vec<String>,
        /// Bypass confirmation, not the linkage check.
        force: bool,
    },
    /// Copy a source to a separately resolved destination.
    Copy {
        /// Source path and shared source arguments.
        args: ResourceArgs,
        /// Destination tokens selected by Python's two-positional allocation.
        destination: Vec<String>,
        /// Source force selection; OneLake copy has no confirmation prompt.
        force: bool,
        /// Requested recursive resource copy.
        recursive: bool,
        /// Block resource-name collisions in a different folder.
        block_on_path_collision: bool,
    },
    /// Move a source to a separately resolved destination.
    Move {
        /// Source path and shared source arguments.
        args: ResourceArgs,
        /// Destination tokens selected by Python's two-positional allocation.
        destination: Vec<String>,
        /// Bypass the source confirmation.
        force: bool,
        /// Requested recursive resource move.
        recursive: bool,
    },
}

impl ResourceCommand {
    /// Canonical Python command name; aliases do not count as additional leaves.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Exists(_) => "exists",
            Self::Mkdir { .. } => "mkdir",
            Self::Remove { .. } => "rm",
            Self::Set { .. } => "set",
            Self::Link { .. } => "ln",
            Self::Start { .. } => "start",
            Self::Stop { .. } => "stop",
            Self::Assign { .. } => "assign",
            Self::Unassign { .. } => "unassign",
            Self::Copy { .. } => "cp",
            Self::Move { .. } => "mv",
        }
    }

    /// Shared arguments used by context and output hosts.
    pub fn args(&self) -> &ResourceArgs {
        match self {
            Self::Exists(args)
            | Self::Mkdir { args, .. }
            | Self::Remove { args, .. }
            | Self::Set { args, .. }
            | Self::Link { args, .. }
            | Self::Start { args, .. }
            | Self::Stop { args, .. }
            | Self::Assign { args, .. }
            | Self::Unassign { args, .. }
            | Self::Copy { args, .. }
            | Self::Move { args, .. } => args,
        }
    }
}

fn flag(name: &'static str, short: Option<char>) -> Arg {
    let arg = Arg::new(name).long(name).action(ArgAction::SetTrue);
    match short {
        Some(short) => arg.short(short),
        None => arg,
    }
}

fn tokens(name: &'static str) -> Arg {
    Arg::new(name).long(name).num_args(1..)
}

fn leaf(name: &'static str) -> Command {
    common(name).arg(Arg::new("path").required(true).num_args(1..))
}

fn common(name: &'static str) -> Command {
    Command::new(name)
        .args_override_self(true)
        .infer_long_args(true)
        .arg(
            Arg::new("output_format")
                .long("output_format")
                .value_parser(["json", "text"]),
        )
        .arg(Arg::new("skill").long("skill").hide(true))
}

/// Compose these root leaves directly into the single existing front door.
///
/// This installs no new namespace, runtime, credential provider, or service host.
pub fn resource_subcommands() -> Vec<Command> {
    vec![
        leaf("exists"),
        leaf("mkdir").visible_alias("create").arg(
            Arg::new("params")
                .long("params")
                .short('P')
                .num_args(0..)
                .default_value("run=true"),
        ),
        leaf("rm")
            .visible_alias("del")
            .arg(flag("force", Some('f')))
            .arg(flag("hard", None)),
        leaf("set")
            .arg(Arg::new("query").long("query").short('q').required(true))
            .arg(tokens("input").short('i').required(true))
            .arg(flag("force", Some('f'))),
        leaf("ln")
            .visible_alias("mklink")
            .arg(Arg::new("type").long("type").required(true).value_parser([
                "adlsGen2",
                "amazonS3",
                "dataverse",
                "googleCloudStorage",
                "oneLake",
                "s3Compatible",
            ]))
            .arg(tokens("target"))
            .arg(tokens("input").short('i'))
            .arg(flag("force", Some('f'))),
        leaf("start").arg(flag("force", Some('f'))),
        leaf("stop").arg(flag("force", Some('f'))),
        leaf("assign")
            .arg(tokens("workspace").short('W').required(true))
            .arg(flag("force", Some('f'))),
        leaf("unassign")
            .arg(tokens("workspace").short('W').required(true))
            .arg(flag("force", Some('f'))),
        common("cp")
            .visible_alias("copy")
            .arg(Arg::new("paths").required(true).num_args(2..))
            .arg(flag("force", Some('f')))
            .arg(flag("recursive", Some('r')))
            .arg(flag("block_on_path_collision", None)),
        common("mv")
            .visible_alias("move")
            .arg(Arg::new("paths").required(true).num_args(2..))
            .arg(flag("force", Some('f')))
            .arg(flag("recursive", Some('r'))),
    ]
}

/// Convert matches from the root parser containing [`resource_subcommands`].
///
/// The caller handles Clap syntax errors using its existing stream/exit policy.
/// Unknown commands and mismatched registration schemas fail explicitly.
pub fn command_from_matches(root: &ArgMatches) -> Result<ResourceCommand, MutationError> {
    let (name, matches) = root
        .subcommand()
        .ok_or_else(|| invalid("A resource command is required"))?;
    let (path, destination) = if matches!(name, "cp" | "copy" | "mv" | "move") {
        let mut paths = values(matches, "paths")?
            .ok_or_else(|| invalid("Source and destination paths are required"))?;
        if paths.len() < 2 {
            return Err(invalid("Source and destination paths are required"));
        }
        let destination = paths.pop().ok_or_else(|| invalid("Missing destination"))?;
        (paths, Some(vec![destination]))
    } else {
        (
            values(matches, "path")?
                .filter(|tokens| !tokens.is_empty())
                .ok_or_else(|| invalid("A resource path is required"))?,
            None,
        )
    };
    let args = ResourceArgs {
        path,
        output_format: string(matches, "output_format")?,
        skill: string(matches, "skill")?,
    };
    match name {
        "exists" => Ok(ResourceCommand::Exists(args)),
        "mkdir" | "create" => Ok(ResourceCommand::Mkdir {
            args,
            params: values(matches, "params")?.unwrap_or_default(),
        }),
        "rm" | "del" => Ok(ResourceCommand::Remove {
            args,
            force: boolean(matches, "force")?,
            hard: boolean(matches, "hard")?,
        }),
        "set" => Ok(ResourceCommand::Set {
            args,
            query: string(matches, "query")?.ok_or_else(|| invalid("Missing SET query"))?,
            input: values(matches, "input")?.ok_or_else(|| invalid("Missing SET input"))?,
            force: boolean(matches, "force")?,
        }),
        "ln" | "mklink" => Ok(ResourceCommand::Link {
            args,
            shortcut_type: string(matches, "type")?
                .ok_or_else(|| invalid("Missing shortcut type"))?,
            target: values(matches, "target")?,
            input: values(matches, "input")?,
            force: boolean(matches, "force")?,
        }),
        "start" => Ok(ResourceCommand::Start {
            args,
            force: boolean(matches, "force")?,
        }),
        "stop" => Ok(ResourceCommand::Stop {
            args,
            force: boolean(matches, "force")?,
        }),
        "assign" => Ok(ResourceCommand::Assign {
            args,
            workspace: values(matches, "workspace")?
                .ok_or_else(|| invalid("Missing assignment workspace"))?,
            force: boolean(matches, "force")?,
        }),
        "unassign" => Ok(ResourceCommand::Unassign {
            args,
            workspace: values(matches, "workspace")?
                .ok_or_else(|| invalid("Missing unassignment workspace"))?,
            force: boolean(matches, "force")?,
        }),
        "cp" | "copy" => Ok(ResourceCommand::Copy {
            args,
            destination: destination.ok_or_else(|| invalid("Missing copy destination"))?,
            force: boolean(matches, "force")?,
            recursive: boolean(matches, "recursive")?,
            block_on_path_collision: boolean(matches, "block_on_path_collision")?,
        }),
        "mv" | "move" => Ok(ResourceCommand::Move {
            args,
            destination: destination.ok_or_else(|| invalid("Missing move destination"))?,
            force: boolean(matches, "force")?,
            recursive: boolean(matches, "recursive")?,
        }),
        _ => Err(invalid("Unknown resource command")),
    }
}

fn string(matches: &ArgMatches, name: &str) -> Result<Option<String>, MutationError> {
    matches
        .try_get_one::<String>(name)
        .map(|value| value.cloned())
        .map_err(|_| invalid("Resource parser schema mismatch"))
}

fn values(matches: &ArgMatches, name: &str) -> Result<Option<Vec<String>>, MutationError> {
    matches
        .try_get_many::<String>(name)
        .map(|values| values.map(|values| values.cloned().collect()))
        .map_err(|_| invalid("Resource parser schema mismatch"))
}

fn boolean(matches: &ArgMatches, name: &str) -> Result<bool, MutationError> {
    matches
        .try_get_one::<bool>(name)
        .map(|value| value.copied().unwrap_or(false))
        .map_err(|_| invalid("Resource parser schema mismatch"))
}

pub(super) fn invalid(message: impl Into<String>) -> MutationError {
    MutationError::new("InvalidInput", message)
}
