// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Python-named arguments for the bounded read preview.

use clap::{Arg, ArgAction, Command, error::ErrorKind};
use std::ffi::OsString;

mod diagnostics;
pub use diagnostics::invalid_arguments_message;

pub mod batch;
pub mod delete;
pub use delete::DeleteCommand;

#[cfg(feature = "v2-cancel")]
mod cancel;
#[cfg(feature = "v2-cancel")]
pub use cancel::CancelCommand;

#[cfg(feature = "self-update")]
mod update;
#[cfg(feature = "self-update")]
pub use update::{UpdateAction, UpdateCommand};

mod jobs;
pub use jobs::{JobMutationCommand, JobMutationKind, JobReadCommand, JobReadKind, JobRunCommand};

#[cfg(any(feature = "windows-broker", feature = "interactive"))]
mod auth;
#[cfg(any(feature = "windows-broker", feature = "interactive"))]
pub use auth::{AuthAction, AuthCommand, AuthMethod, DEFAULT_BROKER_CLIENT_ID};

/// A supported read operation, not a new resource namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadKind {
    /// Tenant workspace listing or workspace item listing.
    List,
    /// Generic item metadata retrieval.
    Get,
}

/// Parsed Python read-command arguments; unsupported options are never ignored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadCommand {
    /// Operation selected by the command name.
    pub kind: ReadKind,
    /// Original spelling, including the Python `dir` alias.
    pub name: String,
    /// Path tokens, joined and quote-stripped by the existing context resolver.
    pub path: Vec<String>,
    /// Scalar LS expression or joined/quote-stripped GET tokens, when supplied.
    /// `Some("")` retains a present empty argument, separately from absence.
    pub query: Option<String>,
    /// Python `ls --long`.
    pub long: bool,
    /// Python `ls --all`.
    pub all: bool,
    /// Python `get --output`, recognized but not implemented.
    pub output: Option<String>,
    /// Python `get --verbose`.
    pub verbose: bool,
    /// Python `get --force`, recognized but not implemented.
    pub force: bool,
    /// Explicit global output override; the selected host owns format support.
    pub output_format: Option<String>,
}

/// A parse result with help/version separated from service execution.
#[derive(Clone, Debug)]
pub enum Invocation {
    /// Native serving operations on an exact owning MLModel item.
    MlEndpoint(crate::ml_endpoint::EndpointCommand),
    /// Real run/query/execute frontend with an explicitly injected backend boundary.
    NativeTabular(crate::native_tabular::NativeCommand),
    /// Explicit tabular file delivery around one declared production operation.
    Tabular(crate::tabular_export::ExportCommand),
    /// Help/version text, written to stdout without configuring a host.
    Display(String),
    /// Opt-in, ID-only Notebook run cancellation; never a terminal-state claim.
    #[cfg(feature = "v2-cancel")]
    Cancel(CancelCommand),
    /// Explicit v2 named Dataflow query execution, separate from item jobs.
    Execute(crate::execute::ExecuteCommand),
    /// Ordered source `-c` strings, parsed individually immediately before dispatch.
    Batch(batch::BatchInvocation),
    /// A command requiring explicitly supplied dependencies.
    Read(ReadCommand),
    /// V2 nonrecursive typed delete; never routed through the source `rm` handler.
    Delete(DeleteCommand),
    /// Read-only job leaf; never submission, cancellation or schedule mutation.
    JobRead(JobReadCommand),
    /// Submission or synchronous execution, using the same explicitly selected host.
    JobRun(JobRunCommand),
    /// Cancellation or schedule mutation through the same job session.
    JobMutation(JobMutationCommand),
    /// Local settings and navigation with caller-owned state and effects.
    Local(fabric_cli_local::LocalInvocation),
    /// Released ACL, label, catalog and raw API family.
    Access(fabric_cli_resources::frontdoor::AccessCommand),
    /// Released resource mutations and OneLake transfers.
    Resource(fabric_cli_mutations::frontdoor::ResourceCommand),
    /// Released definition export/import transfers.
    Transfer(fabric_cli_transfer::TransferCommand),
    /// Source config deployment, routed to the native deployment engine.
    Deploy(fabric_cli_transfer::TransferCommand),
    /// Released TABLE handlers over the existing metadata/storage/job session.
    Table(fabric_cli_tables::TableCommand),
    /// Opt-in, read-only analysis of an explicitly supplied local model.
    #[cfg(feature = "semantic-model")]
    SemanticModel(fabric_cli_semantic_model::AnalyzeCommand),
    /// Explicit opt-in broker command; parsing never signs in.
    #[cfg(any(feature = "windows-broker", feature = "interactive"))]
    Auth(AuthCommand),
    /// Explicit signed-release update, independent of Fabric authentication.
    #[cfg(feature = "self-update")]
    Update(UpdateCommand),
}

/// Truthful fallback when no schema-controlled argument diagnostic is available.
pub const INVALID_ARGUMENTS: &str =
    "Invalid arguments. Run fab-rs --help for available commands and usage.";

/// Build the Clap schema/help without reading configuration or the environment.
///
/// Use [`parse_args`] for the source read-command lexical rules as well.
pub fn command() -> Command {
    let command = Command::new("fab-rs")
        .bin_name("fab-rs")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Experimental command composition for Microsoft Fabric")
        .after_help("Local commands use the platform user configuration directory; FAB_RUST_CONFIG_ROOT overrides it. Remote execution requires a caller-configured host; this default binary has no production authentication or HTTP transport. Config-driven deploy has a bounded native Notebook host; other deployment branches fail explicitly.")
        .arg_required_else_help(true)
        .args_override_self(true)
        .arg(
            Arg::new("batch_commands")
                .short('c')
                .long("command")
                .action(ArgAction::Append)
                .help("Run a command string in non-interactive mode; may be repeated"),
        )
        .arg(
            Arg::new("output_format")
                .long("output_format")
                .global(true)
                .value_parser(["json", "text"]),
        );
    #[cfg(any(feature = "windows-broker", feature = "interactive"))]
    let command = auth::register(command);
    #[cfg(feature = "self-update")]
    let command = update::register(command);
    #[cfg(feature = "v2-cancel")]
    let command = command.subcommand(cancel::command());
    #[cfg(all(feature = "native-read", not(feature = "headless")))]
    let command = command.after_help("Native remote commands reuse the Windows account selected by auth login; FAB_RUST_TENANT, FAB_RUST_ACCOUNT and FAB_RUST_CLIENT_ID provide explicit overrides. Windows broker authentication is opt-in; remote credentials are silent and never show sign-in UI. FAB_RUST_CONTEXT defaults to / or the explicitly selected persisted session; FAB_RUST_HOME is optional. Local/config and family commands share the platform user configuration directory; FAB_RUST_CONFIG_ROOT overrides it. Mutations change remote state; job run may cancel on timeout. Native confirmations require explicit --force or an injected UI host. No token input or credential fallback is available. Config-driven deploy supports bounded root-level Notebooks with explicit unpublish.skip; other branches fail before writes.");
    #[cfg(feature = "headless")]
    let command = command.after_help("Native remote commands support explicit FAB_RUST_AUTH_MODE: client-secret, client-assertion, workload-identity, azure-pipelines, managed-identity-vm, managed-identity-app-service, certificate-store-windows, certificate-store-pkcs11, certificate-store-macos, or device-code when compiled in. Select FAB_RUST_TENANT and the documented provider inputs; no mode is chosen from secret presence. Headless credentials are acquired lazily without auth login, UI, developer tools, raw access-token input or plaintext cache. FAB_RUST_MANAGED_IDENTITY must explicitly select system or client. Windows broker authentication is opt-in with native-read; broker credentials reuse auth login selection silently and never show sign-in UI. Default builds have no production authentication or HTTP transport. FAB_RUST_CONTEXT selects session context; local/config and remote families share the platform user configuration directory, overridden by FAB_RUST_CONFIG_ROOT. See the native host documentation for exact environment bindings, endpoint restrictions and experimental support limits.");
    #[cfg(all(feature = "interactive", not(feature = "headless")))]
    let command = command.after_help("Device-code authentication is opt-in: auth login --method device-code requires explicit public-cloud tenant and client GUIDs for first login. Native remote commands renew the saved session with FAB_RUST_AUTH_MODE=device-code; status and refresh never prompt. Refresh credentials stay in an OS protected store; macOS requires an explicitly pinned installed protected-store helper. Windows broker authentication is opt-in; Windows retains broker as its default method and fails explicitly if it is unavailable. Explicit method/provider configuration wins over platform defaults. No developer-tool fallback, raw access-token input or plaintext credential cache is available. Local and remote commands share the platform configuration directory, overridden by FAB_RUST_CONFIG_ROOT.");
    let command = fabric_cli_resources::frontdoor::register(command)
        .subcommands(fabric_cli_local::root_commands())
        .subcommands(fabric_cli_mutations::frontdoor::resource_subcommands())
        .subcommands(fabric_cli_transfer::transfer_subcommands());
    let command = delete::register(command)
        .subcommand(crate::execute::command())
        .subcommands(artifact_commands())
        .subcommand(fabric_cli_tables::command())
        .subcommand(jobs::register())
        .subcommand(
            Command::new("ls")
                .visible_alias("dir")
                .about("List workspaces or workspace items; a final 'prefix*' filters children")
                .args_override_self(true)
                .infer_long_args(true)
                .arg(Arg::new("path").num_args(0..))
                .arg(
                    Arg::new("long")
                        .short('l')
                        .long("long")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("all")
                        .short('a')
                        .long("all")
                        .action(ArgAction::SetTrue),
                )
                .arg(Arg::new("query").short('q').long("query")
                    .help("JMESPath expression over the listing array; use --long -q '[].id' for item IDs"))
                .after_help("Examples:\n  fab-rs ls 'marc*'\n  fab-rs ls 'Demo.Workspace/marc*'\n  fab-rs ls 'Demo.Workspace' --long -q '[].id'\n  fab-rs ls / -q \"[?name=='Demo.Workspace'].id\"\nA single trailing '*' filters children in one exact parent. Quote the path to prevent shell expansion. Only nonempty prefixes in the final segment are supported; other patterns and mutation wildcards are not supported.\nTo inspect the workspace itself, use: fab-rs get 'Demo.Workspace' -q id\nA bare field such as 'id' queries the whole listing array, not each item. Workspace item IDs are available only with --long."),
        )
        .subcommand(
            Command::new("get")
                .about("Get item properties (bounded generic metadata)")
                .args_override_self(true)
                .infer_long_args(true)
                .arg(Arg::new("path").num_args(1..).required(true))
                .arg(Arg::new("query").short('q').long("query").num_args(1..))
                .arg(Arg::new("output").short('o').long("output").num_args(1..))
                .arg(
                    Arg::new("verbose")
                        .short('v')
                        .long("verbose")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("force")
                        .short('f')
                        .long("force")
                        .action(ArgAction::SetTrue),
                ),
        );
    crate::tabular_export::register(crate::native_tabular::register(command))
}

#[cfg(feature = "semantic-model")]
fn artifact_commands() -> Vec<Command> {
    vec![fabric_cli_semantic_model::command().subcommand(crate::ml_endpoint::command())]
}

#[cfg(not(feature = "semantic-model"))]
fn artifact_commands() -> Vec<Command> {
    vec![
        Command::new("artifact")
            .about("Artifact capabilities")
            .arg_required_else_help(true)
            .subcommand(crate::ml_endpoint::command()),
    ]
}

/// Parse arguments including argv[0], without printing or exiting.
///
/// Invalid-input errors are retained for programmatic classification. Executable
/// callers must use [`invalid_arguments_message`] rather than echoing raw errors.
pub fn parse_args<I, T>(args: I) -> Result<Invocation, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let mut args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let root = root_argument(&args);
    if root.and_then(|index| args[index].to_str()) == Some("delete") {
        args = delete::normalize(&args, root.expect("selected delete root"));
    }
    if root.and_then(|index| args[index].to_str()) == Some("deploy") {
        for token in &mut args[root.expect("selected root") + 1..] {
            if token == "-tenv" {
                *token = "--target_env".into();
            } else if let Some(value) = token.to_str().and_then(|v| v.strip_prefix("-tenv=")) {
                *token = format!("--target_env={value}").into();
            }
        }
    }
    let mut parser = command();
    if root.and_then(|index| args[index].to_str()) == Some("execute") {
        // V1's inherited last-value-wins policy must not discard v2 selectors.
        parser = parser.args_override_self(false);
    }
    if batch::has_root_option(&args) {
        // Python stores the root version flag but executes a supplied command
        // list first. Keep the existing non-batch Clap version precedence.
        parser = parser.disable_version_flag(true).arg(
            Arg::new("batch_version")
                .short('v')
                .visible_short_alias('V')
                .long("version")
                .action(ArgAction::SetTrue),
        );
    }
    parser.build();
    if args
        .iter()
        .any(|arg| matches!(arg.to_str(), Some("--help" | "-h" | "--version" | "-V")))
        && let Err(error) = parser.clone().try_get_matches_from(&args)
        && matches!(
            error.kind(),
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
        )
    {
        return Ok(Invocation::Display(error.to_string()));
    }
    #[cfg(any(feature = "windows-broker", feature = "interactive"))]
    if let Ok(matches) = parser.clone().try_get_matches_from(&args)
        && matches.subcommand_name() == Some("auth")
    {
        return auth::parse(&matches).map(Invocation::Auth);
    }
    let source_args = if matches!(
        root.and_then(|index| args[index].to_str()),
        Some("ls" | "dir" | "get" | "job" | "table")
    ) {
        source_read_arguments(&parser, &args)?
    } else {
        args.clone()
    };
    let matches = match parser.try_get_matches_from_mut(source_args) {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return Ok(Invocation::Display(error.to_string()));
        }
        Err(error) if error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            if let Some(group) = root
                .and_then(|index| args[index].to_str())
                .and_then(|name| parser.find_subcommand_mut(name))
                .filter(|group| group.has_subcommands())
            {
                return Ok(Invocation::Display(group.render_help().to_string()));
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let Some((name, values)) = matches.subcommand() else {
        if let Some(commands) = matches.get_many::<String>("batch_commands") {
            return Ok(Invocation::Batch(batch::BatchInvocation {
                commands: commands.cloned().collect(),
                output_format: matches.get_one::<String>("output_format").cloned(),
            }));
        }
        return Err(clap::Error::raw(
            ErrorKind::MissingSubcommand,
            INVALID_ARGUMENTS,
        ));
    };
    if matches!(name, "run" | "query") {
        return crate::native_tabular::from_matches(name, values).map(Invocation::NativeTabular);
    }
    let export = crate::tabular_export::request(&matches);
    let wrap = |mut invocation: Invocation| {
        if let Some(request) = export {
            if let Invocation::Access(command) = &mut invocation
                && let fabric_cli_resources::frontdoor::AccessKind::AclGet { output, .. } =
                    &mut command.kind
            {
                *output = None;
            }
            Invocation::Tabular(crate::tabular_export::ExportCommand {
                command: Box::new(invocation),
                request,
            })
        } else {
            invocation
        }
    };
    if values.subcommand_name().is_none()
        && let Some(group) = parser.find_subcommand_mut(name).filter(|group| {
            group.has_subcommands() && (group.is_arg_required_else_help_set() || name == "config")
        })
    {
        return Ok(Invocation::Display(group.render_help().to_string()));
    }
    #[cfg(feature = "v2-cancel")]
    if name == "cancel" {
        if matches.contains_id("batch_commands") {
            return Err(invalid_arguments());
        }
        return cancel::parse(&matches, values).map(Invocation::Cancel);
    }
    if name == "job" {
        let mut invocation = jobs::parse(values)?;
        let output = match &mut invocation {
            Invocation::JobRead(command) => &mut command.output_format,
            Invocation::JobRun(command) => &mut command.output_format,
            Invocation::JobMutation(command) => &mut command.output_format,
            _ => return Err(invalid_arguments()),
        };
        *output = output
            .take()
            .or_else(|| matches.get_one::<String>("output_format").cloned());
        return Ok(wrap(invocation));
    }
    if name == "delete" {
        return DeleteCommand::from_matches(values).map(Invocation::Delete);
    }
    if name == "execute" {
        if matches.contains_id("output_format") {
            return Err(invalid_arguments());
        }
        return crate::execute::from_matches(values).map(Invocation::Execute);
    }
    if !matches!(name, "ls" | "get") {
        return family_invocation(&matches).map(wrap);
    }
    let list = name == "ls";
    let spelling = root.and_then(|index| args.get(index));
    let name = if list && spelling.is_some_and(|arg| arg == "dir") {
        "dir"
    } else {
        name
    };
    let joined = |key: &str| {
        values.get_many::<String>(key).map(|values| {
            let tokens: Vec<_> = values.map(String::as_str).collect();
            fabric_cli_query::join_query_args(&tokens)
        })
    };
    Ok(wrap(Invocation::Read(ReadCommand {
        kind: if list { ReadKind::List } else { ReadKind::Get },
        name: name.into(),
        path: values
            .get_many::<String>("path")
            .map(|values| values.cloned().collect())
            .unwrap_or_default(),
        query: if list {
            values.get_one::<String>("query").cloned()
        } else {
            joined("query")
        },
        long: list && values.get_flag("long"),
        all: list && values.get_flag("all"),
        output: if list { None } else { joined("output") },
        verbose: !list && values.get_flag("verbose"),
        force: !list && values.get_flag("force"),
        output_format: matches.get_one::<String>("output_format").cloned(),
    })))
}

fn root_argument(args: &[OsString]) -> Option<usize> {
    let mut index = 1;
    while index < args.len() {
        let value = args[index].to_str()?;
        match value {
            "--output_format" | "-c" | "--command" => index += 2,
            "--version" | "-v" | "-V" => index += 1,
            value
                if value.starts_with("--output_format=")
                    || value.starts_with("--command=")
                    || value.starts_with("-c") =>
            {
                index += 1
            }
            _ => return Some(index),
        }
    }
    None
}

fn family_invocation(matches: &clap::ArgMatches) -> Result<Invocation, clap::Error> {
    if matches.subcommand().is_some_and(|(name, artifact)| {
        name == "artifact" && artifact.subcommand_name() == Some("ml-endpoint")
    }) {
        return crate::ml_endpoint::from_matches(matches).map(Invocation::MlEndpoint);
    }
    #[cfg(feature = "semantic-model")]
    if matches.subcommand_name() == Some("artifact") {
        return fabric_cli_semantic_model::AnalyzeCommand::from_matches(matches)
            .map(Invocation::SemanticModel)
            .ok_or_else(invalid_arguments);
    }
    #[cfg(feature = "self-update")]
    if matches.subcommand_name() == Some("update") {
        return update::parse(matches).map(Invocation::Update);
    }
    if let Some(("table", group)) = matches.subcommand() {
        return fabric_cli_tables::TableCommand::from_matches(group)
            .map(Invocation::Table)
            .ok_or_else(invalid_arguments);
    }
    if let Some(command) =
        fabric_cli_local::from_matches(matches).map_err(|_| invalid_arguments())?
    {
        return Ok(Invocation::Local(command));
    }
    if let Some(command) = fabric_cli_resources::frontdoor::AccessCommand::from_matches(matches)
        .map_err(|_| invalid_arguments())?
    {
        return Ok(Invocation::Access(command));
    }
    match matches.subcommand_name() {
        Some("deploy") => fabric_cli_transfer::command_from_matches(matches)
            .map(Invocation::Deploy)
            .map_err(|_| invalid_arguments()),
        Some("export" | "bulk-export" | "import") => {
            fabric_cli_transfer::command_from_matches(matches)
                .map(Invocation::Transfer)
                .map_err(|_| invalid_arguments())
        }
        _ => fabric_cli_mutations::frontdoor::command_from_matches(matches)
            .map(Invocation::Resource)
            .map_err(|_| invalid_arguments()),
    }
}

fn invalid_arguments() -> clap::Error {
    clap::Error::raw(ErrorKind::UnknownArgument, INVALID_ARGUMENTS)
}

// Adapt only the bounded read argv lexical rules. Clap still owns the option
// schema, cardinality, validation, defaults, aliases, help and preview errors.
// Attached values consume one token in argparse; separate nargs+ values consume
// through the next option. Its variadic path is consumed once, never resumed.
fn source_read_arguments(
    parser: &Command,
    args: &[OsString],
) -> Result<Vec<OsString>, clap::Error> {
    let Some(start) = root_argument(args).filter(|index| {
        matches!(
            args[*index].to_str(),
            Some("ls" | "dir" | "get" | "job" | "table")
        )
    }) else {
        return Ok(args.to_vec());
    };
    let name = args[start].to_str().expect("matched command");
    let schema = parser
        .find_subcommand(if name == "dir" { "ls" } else { name })
        .expect("bounded read schema");
    let (schema, start) = if matches!(name, "job" | "table") {
        let Some(leaf) = args.get(start + 1).and_then(|arg| arg.to_str()) else {
            return Ok(args.to_vec());
        };
        let Some(schema) = schema.find_subcommand(leaf) else {
            return Ok(args.to_vec());
        };
        (schema, start + 1)
    } else {
        (schema, start)
    };
    let mut normalized = args[..=start].to_vec();
    let mut path = Vec::new();
    let mut path_closed = false;
    let mut index = start + 1;
    while index < args.len() {
        let Some(token) = args[index].to_str() else {
            return Ok(args.to_vec());
        };
        index += 1;
        if token == "--" {
            if path_closed {
                return Err(invalid_arguments());
            }
            path.extend_from_slice(&args[index..]);
            break;
        }
        if !source_option(token) {
            if path_closed {
                return Err(invalid_arguments());
            }
            path.push(args[index - 1].clone());
            continue;
        }
        path_closed |= !path.is_empty();
        let mut options = Vec::new();
        if let Some(long) = token.strip_prefix("--") {
            let (name, attached) = long
                .split_once('=')
                .map_or((long, None), |(name, value)| (name, Some(value)));
            let option = if let Some(exact) = schema
                .get_arguments()
                .find(|arg| arg.get_long() == Some(name))
            {
                exact
            } else {
                let mut candidates = schema
                    .get_arguments()
                    .filter(|arg| arg.get_long().is_some_and(|long| long.starts_with(name)));
                let Some(option) = candidates.next() else {
                    return Ok(args.to_vec());
                };
                if candidates.next().is_some() {
                    return Ok(args.to_vec());
                }
                option
            };
            options.push((option, attached));
        } else {
            for (offset, short) in token[1..].char_indices() {
                let Some(option) = schema
                    .get_arguments()
                    .find(|arg| arg.get_short() == Some(short))
                else {
                    return Ok(args.to_vec());
                };
                let remaining = &token[1 + offset + short.len_utf8()..];
                if option.get_action().takes_values() && !remaining.is_empty() {
                    options.push((
                        option,
                        Some(remaining.strip_prefix('=').unwrap_or(remaining)),
                    ));
                    break;
                }
                options.push((option, None));
            }
        }
        for (option, attached) in options {
            if matches!(option.get_id().as_str(), "help" | "version") {
                return Ok(args.to_vec());
            }
            let key = format!("--{}", option.get_long().expect("bounded option long name"));
            if !option.get_action().takes_values() {
                if attached.is_some() {
                    return Err(invalid_arguments());
                }
                normalized.push(key.into());
                continue;
            }
            let value = if let Some(value) = attached {
                value.to_owned()
            } else {
                let max = option
                    .get_num_args()
                    .expect("built option arity")
                    .max_values();
                let first = index;
                while index < args.len()
                    && index - first < max
                    && args[index]
                        .to_str()
                        .is_some_and(|value| !source_option(value))
                {
                    index += 1;
                }
                if first == index {
                    return Err(diagnostics::missing_value(option));
                }
                let tokens: Vec<_> = args[first..index]
                    .iter()
                    .map(|value| value.to_str().expect("checked UTF-8"))
                    .collect();
                if max == 1 {
                    tokens[0].to_owned()
                } else if name == "job" {
                    tokens.join(" ")
                } else {
                    fabric_cli_query::join_query_args(&tokens)
                }
            };
            normalized.push(format!("{key}={value}").into());
        }
    }
    if !path.is_empty() {
        normalized.push("--".into());
        normalized.extend(path);
    }
    Ok(normalized)
}

fn source_option(value: &str) -> bool {
    value.len() > 1 && value.starts_with('-') && !source_negative_number(value)
}

fn source_negative_number(value: &str) -> bool {
    let Some(value) = value.strip_prefix('-') else {
        return false;
    };
    if let Some((integer, fraction)) = value.split_once('.') {
        integer.bytes().all(|byte| byte.is_ascii_digit())
            && !fraction.is_empty()
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
    } else {
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
    }
}
