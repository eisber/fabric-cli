// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{invalid, json_failure};
use clap::{Arg, ArgAction, Command};
use fabric_cli_command_contracts::CommandError;
use std::time::Duration;

/// Long-only approved API parser, with explicit per-command polling configuration.
/// Define the raw API arguments and options.
pub fn command() -> Command {
    Command::new("api")
        .about("Send a raw API request; recognized operations poll by default until completion or local cancellation")
        .disable_help_flag(true)
        .arg(Arg::new("help").long("help").action(ArgAction::Help))
        .arg(Arg::new("endpoint").required(true))
        .arg(
            Arg::new("polling-config")
                .long("polling-config")
                .value_name("JSON")
                .help("Optional polling JSON: poll_interval and timeout are positive numbers of seconds; both keys are optional and timeout has no default. Cannot be combined with --async. Backend work may continue after timeout"),
        )
        .arg(
            Arg::new("method")
                .long("method")
                .default_value("get")
                .value_parser(["get", "post", "delete", "put", "patch"]),
        )
        .arg(
            Arg::new("audience")
                .long("audience")
                .default_value("fabric")
                .value_parser(["fabric", "storage", "azure", "powerbi"]),
        )
        .arg(
            Arg::new("payload")
                .long("payload")
                .help("Inline JSON object, JSON file, or multipart file; cannot be used with --method delete"),
        )
        .arg(
            Arg::new("headers")
                .long("headers")
                .value_name("JSON")
                .help("Inline JSON object of string headers: Accept, Content-Type, If-Match, If-None-Match, Prefer, Range"),
        )
        .arg(
            Arg::new("query").long("query").help(
                "Local JMESPath on the response object: status_code, text, optional headers",
            ),
        )
        .arg(
            Arg::new("show-headers")
                .long("show-headers")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("async")
                .long("async")
                .action(ArgAction::SetTrue)
                .help("Send once and return the initial response without polling; cannot be combined with --polling-config"),
        )
}

/// Validate the shared per-command polling configuration.
pub fn parse_polling_config(text: Option<&str>) -> Result<fabric_sdk_api::LroConfig, CommandError> {
    let Some(text) = text else {
        return Ok(fabric_sdk_api::LroConfig::default());
    };
    if text.len() > 16 * 1024 {
        return Err(invalid("--polling-config exceeds the 16384-byte limit"));
    }
    let json = fabric_core_json::decode(text.as_bytes()).map_err(|_| {
        invalid(json_failure(
            "--polling-config is not valid JSON",
            text.as_bytes(),
        ))
    })?;
    let object = json
        .as_object()
        .ok_or_else(|| invalid("Polling configuration must be a JSON object"))?;
    if object.contains_key("polling_interval") {
        return Err(invalid(
            "Unknown polling configuration option 'polling_interval'; use 'poll_interval'",
        ));
    }
    if object
        .keys()
        .any(|key| key != "poll_interval" && key != "timeout")
    {
        return Err(invalid(
            "Unknown polling configuration option; supported keys are poll_interval and timeout",
        ));
    }
    let duration = |key: &str| -> Result<Option<Duration>, CommandError> {
        let Some(value) = object.get(key).filter(|value| !value.is_null()) else {
            return Ok(None);
        };
        let seconds = value
            .as_f64()
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| {
                CommandError::new(
                    "InvalidInput",
                    format!("Invalid {key}: expected a positive JSON number of seconds"),
                )
            })?;
        Duration::try_from_secs_f64(seconds).map(Some).map_err(|_| {
            CommandError::new(
                "InvalidInput",
                format!("Invalid {key}: duration exceeds supported range"),
            )
        })
    };
    fabric_sdk_api::LroConfig::new(duration("poll_interval")?, duration("timeout")?)
        .map_err(Into::into)
}
/// Explain the method-specific parse error without echoing the supplied value.
pub fn parse_failure_message(error: &clap::Error) -> Option<&'static str> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    (error.kind() == ErrorKind::InvalidValue
        && matches!(
            error.get(ContextKind::InvalidArg),
            Some(ContextValue::String(arg)) if arg == "--method <method>"
        ))
    .then_some("Invalid HTTP method for --method. Supported values are lowercase: get, post, delete, put, patch. For example, use post instead of POST.")
}
