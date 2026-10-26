// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{Arg, ArgAction, ArgMatches, Command};
use fabric_cli_output::CommandError;

/// Canonical source transfer leaves; no aliases or new namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferKind {
    /// Single-item or selected workspace export.
    Export,
    /// Recursive workspace/folder bulk definition export.
    BulkExport,
    /// Create or update an item from local definition files.
    Import,
    /// Config-driven CICD deployment, explicitly unsupported by this adapter.
    Deploy,
}

impl TransferKind {
    /// Exact Python command spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Export => "export",
            Self::BulkExport => "bulk-export",
            Self::Import => "import",
            Self::Deploy => "deploy",
        }
    }
}

/// Source arguments, retaining token cardinality and absent versus empty values.
#[derive(Clone, Debug)]
pub struct TransferCommand {
    /// Selected root leaf.
    pub kind: TransferKind,
    /// Fabric path tokens, empty only for deploy.
    pub path: Vec<String>,
    /// Local export destination; bulk-export has exactly one token.
    pub output: Option<Vec<String>>,
    /// Local import directory.
    pub input: Option<Vec<String>>,
    /// Source definition format selector, not output format.
    pub format: Option<String>,
    /// Workspace export selects all supported items without a selection prompt.
    pub all: bool,
    /// Required by the bulk-export handler, not by its parser.
    pub recursive: bool,
    /// Bypass only the source confirmation prompts.
    pub force: bool,
    /// Source global output override, otherwise the caller supplies configuration.
    pub output_format: Option<String>,
    /// Deploy configuration path.
    pub config: Option<String>,
    /// Deploy environment; source default is `N/A`.
    pub target_env: String,
    /// Deploy override tokens, never interpreted as an import definition.
    pub params: Option<Vec<String>>,
    /// Deploy experimental bulk publication switch.
    pub bulk_publish: bool,
}

fn flag(name: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue)
}

fn tokens(name: &'static str, short: char) -> Arg {
    Arg::new(name).long(name).short(short).num_args(1..)
}

/// Compose these four root commands into the existing front door.
///
/// This installs no binary, registry, runtime, credentials, or service host.
/// `-tenv` is Python's multi-character short option; the central lexical adapter
/// must normalize it to `--target_env` before invoking Clap.
pub fn transfer_subcommands() -> Vec<Command> {
    [
        TransferKind::Export,
        TransferKind::BulkExport,
        TransferKind::Import,
        TransferKind::Deploy,
    ]
    .into_iter()
    .map(|kind| {
        let mut command = Command::new(kind.as_str())
            .args_override_self(true)
            .infer_long_args(true)
            .arg(flag("force").short('f'))
            .arg(
                Arg::new("output_format")
                    .long("output_format")
                    .value_parser(["json", "text"]),
            );
        if kind != TransferKind::Deploy {
            command = command.arg(Arg::new("path").required(true).num_args(1..));
        }
        match kind {
            TransferKind::Export => command
                .arg(tokens("output", 'o').required(true))
                .arg(flag("all").short('a'))
                .arg(Arg::new("format").long("format")),
            TransferKind::BulkExport => command
                .arg(Arg::new("output").long("output").short('o').required(true))
                .arg(flag("recursive")),
            TransferKind::Import => command
                .arg(tokens("input", 'i').required(true))
                .arg(Arg::new("format").long("format")),
            TransferKind::Deploy => command
                .arg(Arg::new("config").long("config").required(true))
                .arg(
                    Arg::new("target_env")
                        .long("target_env")
                        .default_value("N/A"),
                )
                .arg(tokens("params", 'P'))
                .arg(flag("bulk_publish")),
        }
    })
    .collect()
}

/// Convert root matches from the family's registration, without parsing again.
///
/// Unknown commands and mismatched schemas return explicit input errors.
pub fn command_from_matches(root: &ArgMatches) -> Result<TransferCommand, CommandError> {
    let (name, args) = root
        .subcommand()
        .ok_or_else(|| invalid("A transfer command is required"))?;
    let kind = match name {
        "export" => TransferKind::Export,
        "bulk-export" => TransferKind::BulkExport,
        "import" => TransferKind::Import,
        "deploy" => TransferKind::Deploy,
        _ => return Err(invalid("Unknown transfer command")),
    };
    let string = |id: &str| args.try_get_one::<String>(id).ok().flatten().cloned();
    let values = |id: &str| {
        args.try_get_many::<String>(id)
            .ok()
            .flatten()
            .map(|values| values.cloned().collect::<Vec<_>>())
    };
    let boolean = |id: &str| {
        args.try_get_one::<bool>(id)
            .ok()
            .flatten()
            .copied()
            .unwrap_or(false)
    };
    let path = if kind == TransferKind::Deploy {
        Vec::new()
    } else {
        values("path").ok_or_else(|| invalid("A Fabric path is required"))?
    };
    let command = TransferCommand {
        kind,
        path,
        output: values("output"),
        input: values("input"),
        format: string("format"),
        all: boolean("all"),
        recursive: boolean("recursive"),
        force: boolean("force"),
        output_format: string("output_format"),
        config: string("config"),
        target_env: string("target_env").unwrap_or_else(|| "N/A".into()),
        params: values("params"),
        bulk_publish: boolean("bulk_publish"),
    };
    if matches!(kind, TransferKind::Export | TransferKind::BulkExport) && command.output.is_none()
        || kind == TransferKind::Import && command.input.is_none()
        || kind == TransferKind::Deploy && command.config.is_none()
    {
        return Err(invalid("Required transfer arguments are missing"));
    }
    Ok(command)
}

/// Join source `nargs` values and strip only edge quote characters.
pub fn process_tokens(tokens: &[String]) -> String {
    tokens.join(" ").trim_matches(['"', '\'']).to_owned()
}

pub(crate) fn invalid(message: impl Into<String>) -> CommandError {
    command_error("InvalidInput", message)
}

pub(crate) fn command_error(code: &str, message: impl Into<String>) -> CommandError {
    CommandError::new(Some(message.into()), Some(code.into()))
}
