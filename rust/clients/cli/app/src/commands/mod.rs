// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod api;
pub(crate) mod auth;

pub(super) use api::command as api_command;

use crate::{Host, progress::ConsoleProgress};
use fabric_cli_command_contracts::CommandError;
use fabric_core::CancellationToken;
use fabric_identity::Binding;
use serde_json::Value;
use std::sync::Arc;

fn selected(host: &dyn Host) -> Result<Binding, CommandError> {
    host.selection()?.ok_or_else(|| {
        CommandError::new(
            "Unauthorized",
            "No Rust identity selected. Run fab-rs auth login.",
        )
    })
}

pub(super) async fn execute(
    matches: &clap::ArgMatches,
    host: &dyn Host,
    cancellation: CancellationToken,
    observer: &Arc<ConsoleProgress>,
) -> Result<Value, CommandError> {
    match matches.subcommand() {
        Some(("auth", auth)) => auth::execute(auth, host, cancellation, observer).await,
        Some(("api", api)) => api::execute(api, host, cancellation, observer).await,
        _ => Err(CommandError::new(
            "InvalidInput",
            "A command is required; run fab-rs --help to list commands",
        )),
    }
}
