// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{Arg, ArgAction, ArgMatches, Command};

/// Typed TABLE family registration for the separately owned front door.
///
/// The parent composes this as a subcommand; this crate does not change the
/// central parser or register another executable namespace.
pub fn command() -> Command {
    let path = || Arg::new("path").required(true).num_args(1..);
    let output = || {
        Arg::new("output_format")
            .long("output_format")
            .value_parser(["json", "text"])
    };
    Command::new("table")
        .about("Load, optimize, inspect or vacuum Delta tables")
        .subcommand(
            Command::new("schema")
                .args_override_self(true)
                .infer_long_args(true)
                .arg(path())
                .arg(output()),
        )
        .subcommand(
            Command::new("load")
                .args_override_self(true)
                .infer_long_args(true)
                .arg(path())
                .arg(output())
                .arg(Arg::new("file").long("file").required(true).num_args(1..))
                .arg(Arg::new("extension").long("extension"))
                .arg(
                    Arg::new("mode")
                        .long("mode")
                        .default_value("overwrite")
                        .value_parser(["append", "overwrite"]),
                )
                .arg(Arg::new("format").long("format")),
        )
        .subcommand(
            Command::new("vacuum")
                .args_override_self(true)
                .infer_long_args(true)
                .arg(path())
                .arg(output())
                .arg(
                    Arg::new("retain_n_hours")
                        .long("retain_n_hours")
                        .default_value("168")
                        .allow_negative_numbers(true),
                ),
        )
        .subcommand(
            Command::new("optimize")
                .args_override_self(true)
                .infer_long_args(true)
                .arg(path())
                .arg(output())
                .arg(Arg::new("vorder").long("vorder").action(ArgAction::SetTrue))
                .arg(Arg::new("zorder").long("zorder")),
        )
}

/// One canonical Python leaf and its raw source option values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TableOperation {
    /// Read Delta schema.
    Schema,
    /// Load from an explicitly resolved OneLake file or folder.
    Load {
        /// Source path tokens (`nargs='+'`).
        file: Vec<String>,
        /// Raw parser-validated append/overwrite choice.
        mode: String,
        /// Python's comma-separated option string.
        format: Option<String>,
        /// Exact optional file extension.
        extension: Option<String>,
    },
    /// Vacuum retention, parsed by the handler after context resolution.
    Vacuum {
        /// Raw integer-hour spelling.
        retain_n_hours: String,
    },
    /// Optimize settings.
    Optimize {
        /// Enable V-Order.
        vorder: bool,
        /// Raw comma-separated Z-Order columns.
        zorder: Option<String>,
    },
}

/// Argument values without ambient configuration, credentials or context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableCommand {
    /// Canonical TABLE operation.
    pub operation: TableOperation,
    /// Destination path tokens, joined by the shared context boundary.
    pub path: Vec<String>,
    /// Explicit output override; absent uses the host's configuration snapshot.
    pub output_format: Option<String>,
}

impl TableCommand {
    /// Convert matches produced by [`command`]; `None` is the group-help invocation.
    pub fn from_matches(matches: &ArgMatches) -> Option<Self> {
        let (name, args) = matches.subcommand()?;
        let value = |key: &str| args.get_one::<String>(key).cloned();
        let operation = match name {
            "schema" => TableOperation::Schema,
            "load" => TableOperation::Load {
                file: args.get_many::<String>("file")?.cloned().collect(),
                mode: value("mode")?,
                format: value("format"),
                extension: value("extension"),
            },
            "vacuum" => TableOperation::Vacuum {
                retain_n_hours: value("retain_n_hours")?,
            },
            "optimize" => TableOperation::Optimize {
                vorder: args.get_flag("vorder"),
                zorder: value("zorder"),
            },
            _ => return None,
        };
        Some(Self {
            operation,
            path: args.get_many::<String>("path")?.cloned().collect(),
            output_format: value("output_format"),
        })
    }

    /// Python output uses the group name, not a new service namespace.
    pub fn leaf_name(&self) -> &'static str {
        match &self.operation {
            TableOperation::Schema => "table schema",
            TableOperation::Load { .. } => "table load",
            TableOperation::Vacuum { .. } => "table vacuum",
            TableOperation::Optimize { .. } => "table optimize",
        }
    }
}
