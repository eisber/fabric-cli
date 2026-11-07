// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{Arg, ArgAction, ArgMatches, Command};

/// Registered public client used when no explicit or saved broker client is selected.
pub const DEFAULT_BROKER_CLIENT_ID: &str = "5814bfb4-2705-4994-b8d6-39aabeb5eaeb";

/// Explicit authentication action; no action runs during parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthAction {
    /// Silent acquisition, allowing user-initiated broker UI when required.
    Login,
    /// Silent-only acquisition of the selected identity's three audiences.
    Status,
    /// Explicit app-scoped sign-out on this device.
    Logout,
}

/// Explicit interactive provider; absence preserves the platform's default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthMethod {
    /// Experimental Windows WAM, never a cross-platform fallback.
    Broker,
    /// Explicit RFC 8628 device code using native protected refresh persistence.
    DeviceCode,
}

/// Parsed non-secret authentication inputs, independent of read-command arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthCommand {
    /// Operation to execute only through the opt-in authentication host.
    pub action: AuthAction,
    /// Explicit provider selection; no failed provider selects another method.
    pub method: Option<AuthMethod>,
    /// Optional target tenant GUID; otherwise use the saved selection or login picker.
    pub tenant: Option<String>,
    /// Optional work UPN; never selected from the first cached account.
    pub account: Option<String>,
    /// Public application registration GUID.
    pub client_id: String,
    /// Whether argv explicitly overrides the saved/environment application selection.
    pub client_id_explicit: bool,
    /// Explicit presentation override, otherwise selected from the caller's settings.
    pub output_format: Option<String>,
    /// Maximum CLI wait in seconds (1-600).
    pub timeout_seconds: u64,
    /// Whether login may initiate interactive broker/MFA UI.
    pub allow_interaction: bool,
}

pub(super) fn register(command: Command) -> Command {
    let mut auth = Command::new("auth")
        .about("Explicit Entra authentication using opt-in providers")
        .subcommand_required(true);
    for (name, about) in [
        (
            "login",
            "Acquire Fabric, OneLake and ARM tokens using the selected method",
        ),
        (
            "status",
            "Validate all three audiences silently; never open sign-in UI",
        ),
        (
            "logout",
            "Sign out only this application's selected session on this device",
        ),
    ] {
        let mut subcommand = Command::new(name)
            .about(about)
            .arg(
                Arg::new("method")
                    .long("method")
                    .value_parser(["broker", "device-code"])
                    .help("Explicit authentication method; device-code requires the interactive feature"),
            )
            .arg(
                Arg::new("tenant")
                    .long("tenant")
                    .help("Target tenant GUID; required for device-code login, otherwise saved selection or broker sign-in")
                    .value_name("TENANT_GUID"),
            )
            .arg(
                Arg::new("account")
                    .long("account")
                    .help("Work UPN hint; optional for device-code, otherwise saved selection or broker picker")
                    .value_name("WORK_ACCOUNT_UPN"),
            )
            .arg(
                Arg::new("client-id")
                    .long("client-id")
                    .default_value(DEFAULT_BROKER_CLIENT_ID)
                    .value_name("CLIENT_GUID"),
            )
            .arg(
                Arg::new("timeout-seconds")
                    .long("timeout-seconds")
                    .default_value("120")
                    .value_parser(clap::value_parser!(u64).range(1..=600)),
            );
        if name == "login" {
            subcommand = subcommand.arg(
                Arg::new("no-interactive")
                    .long("no-interactive")
                    .action(ArgAction::SetTrue)
                    .help("Never show UI; fail if interaction is required"),
            );
        }
        auth = auth.subcommand(subcommand);
    }
    let command = command.after_help("Windows broker authentication is opt-in. Login remembers only the selected account, tenant and client ID; tokens stay in WAM. Remote commands require a native-read build.");
    #[cfg(feature = "interactive")]
    let command = command.after_help("Device-code authentication is opt-in with --method device-code (the default only off Windows). Login requires explicit --tenant and --client-id GUIDs, or FAB_RUST_TENANT and FAB_RUST_CLIENT_ID. --account is an optional matching hint. Only refresh credentials are persisted, in Windows Credential Manager, macOS Keychain, or Linux Secret Service; a locked/unavailable store fails without plaintext fallback. Status and SDK renewal never start sign-in. Windows WAM remains the default when compiled in. Remote commands can explicitly select FAB_RUST_AUTH_MODE=device-code; headless modes never prompt.");
    command.subcommand(auth)
}

pub(super) fn parse(matches: &ArgMatches) -> Result<AuthCommand, clap::Error> {
    let auth = matches
        .subcommand_matches("auth")
        .expect("matched auth command");
    let (name, args) = auth.subcommand().expect("required auth action");
    let action = match name {
        "login" => AuthAction::Login,
        "status" => AuthAction::Status,
        "logout" => AuthAction::Logout,
        _ => unreachable!("auth schema validates the action"),
    };
    Ok(AuthCommand {
        action,
        method: args
            .get_one::<String>("method")
            .map(|method| match method.as_str() {
                "device-code" => AuthMethod::DeviceCode,
                _ => AuthMethod::Broker,
            }),
        tenant: args.get_one::<String>("tenant").cloned(),
        account: args.get_one::<String>("account").cloned(),
        client_id: args
            .get_one::<String>("client-id")
            .expect("default client")
            .clone(),
        client_id_explicit: args.value_source("client-id")
            == Some(clap::parser::ValueSource::CommandLine),
        output_format: args
            .get_one::<String>("output_format")
            .cloned()
            .or_else(|| matches.get_one::<String>("output_format").cloned()),
        timeout_seconds: *args
            .get_one::<u64>("timeout-seconds")
            .expect("default timeout"),
        allow_interaction: action == AuthAction::Login && !args.get_flag("no-interactive"),
    })
}
