// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The selected six-leaf local group, separate from the Rust v1 grammar.

use std::{ffi::OsString, path::PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command, error::ErrorKind};

use crate::{
    auth::{AuthAction, AuthRequest},
    report::{Destination, ReportRequest},
    upgrade::UpgradeRequest,
};

/// Caller-selected result presentation; no setting is read during parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    /// One structured local-operation envelope.
    Json,
    /// Shared key/value text rendering.
    Text,
}

/// An executable local operation, not a remote Fabric command alias.
#[derive(Debug)]
pub enum Operation {
    /// Write one supported local preference.
    Configure {
        /// Explicit typed configuration target.
        target: String,
        /// Validated by the preference adapter before I/O.
        value: String,
    },
    /// Acquire or sign out the explicitly selected identity context.
    Auth {
        /// Login or scoped logout.
        action: AuthAction,
        /// Non-secret explicit provider selection.
        request: AuthRequest,
    },
    /// Resolve a supported ID-addressed target and launch or hand off its URI.
    Show {
        /// Exact resource path, never a current-directory default.
        path: String,
        /// Explicit experience, independent of authentication.
        experience: String,
        /// Whether argv overrides the saved presentation preference.
        experience_explicit: bool,
        /// Return a manual URI handoff without launching a viewer.
        uri_only: bool,
        /// Total bounded local-launch budget.
        timeout_seconds: u64,
    },
    /// Prepare a feedback draft or an explicit manual submission handoff.
    Report(ReportRequest),
    /// Apply a signed update through the selected installation host.
    Upgrade(UpgradeRequest),
}

impl Operation {
    /// Canonical selected operation path for result and error evidence.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Configure { .. } => "cli configure",
            Self::Auth {
                action: AuthAction::Login,
                ..
            } => "cli login",
            Self::Auth {
                action: AuthAction::Logout,
                ..
            } => "cli logout",
            Self::Show { .. } => "cli show",
            Self::Report(_) => "cli report",
            Self::Upgrade(_) => "cli upgrade",
        }
    }
}

/// Parsed execution with all ambient input selection left to the outer host.
#[derive(Debug)]
pub struct Invocation {
    /// Selected operation and its explicit inputs.
    pub operation: Operation,
    /// Result presentation, JSON by default for this additive head.
    pub format: Format,
    /// Whether argv overrides the saved presentation preference.
    pub format_explicit: bool,
    /// Explicit isolated preference root; absent means the host's v2 default.
    pub config_root: Option<PathBuf>,
}

/// Help and version bypass all execution and configuration.
#[derive(Debug)]
pub enum Parsed {
    /// Schema-controlled text for stdout.
    Display(String),
    /// A validated selected operation.
    Execute(Invocation),
}

fn target() -> Arg {
    Arg::new("target")
        .required(true)
        .value_name("PATH")
        .help("One explicit typed target; no implicit current context")
}

fn timeout(default: &'static str) -> Arg {
    Arg::new("timeout-seconds")
        .long("timeout-seconds")
        .value_name("SECONDS")
        .default_value(default)
        .value_parser(clap::value_parser!(u64).range(1..=600))
}

fn authentication(name: &'static str, about: &'static str) -> Command {
    let command = Command::new(name)
        .about(about)
        .arg(target())
        .arg(
            Arg::new("tenant")
                .long("tenant")
                .required(true)
                .value_name("TENANT_GUID"),
        )
        .arg(
            Arg::new("account")
                .long("account")
                .required(true)
                .value_name("WORK_ACCOUNT_UPN"),
        )
        .arg(
            Arg::new("client-id")
                .long("client-id")
                .default_value("5814bfb4-2705-4994-b8d6-39aabeb5eaeb")
                .value_name("CLIENT_GUID"),
        )
        .arg(timeout("120"));
    if name == "login" {
        command.arg(
            Arg::new("no-interactive")
                .long("no-interactive")
                .action(ArgAction::SetTrue)
                .help("Forbid broker UI; do not fall back to another provider"),
        )
    } else {
        command
    }
}

/// Construct the six-leaf schema without reading configuration or credentials.
pub fn command() -> Command {
    Command::new("fab-cli")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Additive Fabric CLI v2 local-host operations")
        .arg_required_else_help(true)
        .subcommand_required(true)
        .arg(
            Arg::new("format")
                .long("format")
                .global(true)
                .default_value("json")
                .value_parser(["json", "text"]),
        )
        .arg(
            Arg::new("config-root")
                .long("config-root")
                .global(true)
                .value_name("ABSOLUTE_DIRECTORY")
                .help("Explicit isolated v2 preference root; only configure accesses it"),
        )
        .subcommand(
            Command::new("cli")
                .about("Explicit tool, identity, viewer, feedback and installation operations")
                .arg_required_else_help(true)
                .subcommand_required(true)
                .subcommand(
                    Command::new("configure")
                        .about("Set a supported local preference without remote effects")
                        .arg(target())
                        .arg(
                            Arg::new("value")
                                .long("value")
                                .required(true)
                                .value_name("VALUE"),
                        ),
                )
                .subcommand(authentication(
                    "login",
                    "Acquire the explicitly selected Windows broker identity",
                ))
                .subcommand(authentication(
                    "logout",
                    "Sign out only the selected app account on this device",
                ))
                .subcommand(
                    Command::new("report")
                        .about("Prepare feedback or a manual handoff; never sends or uploads")
                        .arg(target())
                        .arg(
                            Arg::new("destination")
                                .long("destination")
                                .required(true)
                                .value_name("github:OWNER/REPO|ideas|security"),
                        )
                        .arg(Arg::new("title").long("title").required(true))
                        .arg(Arg::new("message").long("message").required(true))
                        .arg(
                            Arg::new("handoff")
                                .long("handoff")
                                .action(ArgAction::SetTrue)
                                .help(
                                    "Return manual submission instructions, not a delivery receipt",
                                ),
                        ),
                )
                .subcommand(
                    Command::new("show")
                        .about("Open an exact supported ID-addressed Fabric viewer")
                        .arg(target())
                        .arg(
                            Arg::new("experience")
                                .long("experience")
                                .default_value("fabric")
                                .value_parser(["fabric", "powerbi"]),
                        )
                        .arg(
                            Arg::new("uri-only")
                                .long("uri-only")
                                .action(ArgAction::SetTrue)
                                .help("Return the viewer URI as a handoff without launching"),
                        )
                        .arg(timeout("30")),
                )
                .subcommand(
                    Command::new("upgrade")
                        .about("Apply a signed same-major update to this standalone installation")
                        .arg(
                            Arg::new("target-version")
                                .long("target-version")
                                .value_name("SEMVER"),
                        ),
                ),
        )
}

fn value(matches: &ArgMatches, name: &str) -> String {
    matches
        .get_one::<String>(name)
        .expect("required/defaulted schema value")
        .clone()
}

fn invalid(message: &'static str) -> clap::Error {
    clap::Error::raw(ErrorKind::InvalidValue, message)
}

fn auth_request(args: &ArgMatches, action: AuthAction) -> Result<AuthRequest, clap::Error> {
    if !matches!(value(args, "target").as_str(), "/.auth" | "/.auth/") {
        return Err(invalid(
            "Authentication requires the explicit /.auth target.",
        ));
    }
    Ok(AuthRequest {
        account: value(args, "account"),
        tenant: value(args, "tenant"),
        client_id: value(args, "client-id"),
        allow_interaction: action == AuthAction::Login && !args.get_flag("no-interactive"),
        timeout_seconds: *args
            .get_one::<u64>("timeout-seconds")
            .expect("default timeout"),
    })
}

/// Parse argv including its program name without effects, printing, or exit.
pub fn parse_args<I, T>(args: I) -> Result<Parsed, clap::Error>
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
            return Ok(Parsed::Display(error.to_string()));
        }
        Err(error) => return Err(error),
    };
    let cli = matches
        .subcommand_matches("cli")
        .expect("selected cli group");
    let (name, args) = cli.subcommand().expect("required selected leaf");
    let operation = match name {
        "configure" => Operation::Configure {
            target: value(args, "target"),
            value: value(args, "value"),
        },
        "login" => Operation::Auth {
            action: AuthAction::Login,
            request: auth_request(args, AuthAction::Login)?,
        },
        "logout" => Operation::Auth {
            action: AuthAction::Logout,
            request: auth_request(args, AuthAction::Logout)?,
        },
        "show" => Operation::Show {
            path: value(args, "target"),
            experience: value(args, "experience"),
            experience_explicit: args.value_source("experience")
                == Some(clap::parser::ValueSource::CommandLine),
            uri_only: args.get_flag("uri-only"),
            timeout_seconds: *args
                .get_one::<u64>("timeout-seconds")
                .expect("default timeout"),
        },
        "report" => {
            if value(args, "target") != "/.support/feedback" {
                return Err(invalid(
                    "Feedback requires the explicit /.support/feedback target.",
                ));
            }
            Operation::Report(ReportRequest {
                destination: Destination::parse(&value(args, "destination"))
                    .map_err(|_| invalid("Select an explicitly supported report destination."))?,
                title: value(args, "title"),
                message: value(args, "message"),
                handoff: args.get_flag("handoff"),
            })
        }
        "upgrade" => Operation::Upgrade(UpgradeRequest {
            target_version: args.get_one::<String>("target-version").cloned(),
        }),
        _ => unreachable!("the schema admits only selected leaves"),
    };
    Ok(Parsed::Execute(Invocation {
        operation,
        format: if value(&matches, "format") == "text" {
            Format::Text
        } else {
            Format::Json
        },
        format_explicit: matches.value_source("format")
            == Some(clap::parser::ValueSource::CommandLine),
        config_root: matches.get_one::<String>("config-root").map(PathBuf::from),
    }))
}
