// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod api;
pub(crate) mod auth;

use crate::{Host, progress::ConsoleProgress};
use fabric_cli_command_contracts::{CommandError, CommandOutput};
use fabric_core::CancellationToken;
use fabric_identity::Binding;
use std::sync::Arc;

fn selected(host: &dyn Host) -> Result<Binding, CommandError> {
    host.selection()?.ok_or_else(|| {
        CommandError::new(
            "Unauthorized",
            format!(
                "No Rust identity selected. Run {} auth login.",
                crate::EXECUTABLE_NAME
            ),
        )
    })
}

pub(super) async fn execute(
    matches: &clap::ArgMatches,
    host: &dyn Host,
    cancellation: CancellationToken,
    observer: &Arc<ConsoleProgress>,
) -> Result<CommandOutput, CommandError> {
    match matches.subcommand() {
        Some(("auth", auth)) => auth::execute(auth, host, cancellation, observer).await,
        Some(("api", api)) => api::execute(api, host, cancellation, observer).await,
        _ => Err(CommandError::new(
            "InvalidInput",
            format!(
                "A command is required; run {} --help to list commands",
                crate::EXECUTABLE_NAME
            ),
        )),
    }
}
