// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{Host, console::ConsoleProgress};
use fabric_cli_command_contracts::{CommandError, ValidatedInvocation};
use fabric_core::CancellationToken;
use serde_json::Value;
use std::sync::Arc;

pub(super) async fn execute(
    invocation: &ValidatedInvocation,
    host: &dyn Host,
    cancellation: CancellationToken,
    observer: &Arc<ConsoleProgress>,
) -> Result<Value, CommandError> {
    match invocation.matches().subcommand_name() {
        Some("auth") => {
            fabric_cli_auth::execute(invocation, host, cancellation, observer.as_ref()).await
        }
        Some("api") => fabric_cli_api::execute(invocation, host, cancellation, observer).await,
        _ => Err(CommandError::new(
            "InvalidInput",
            "A command is required; run fab-rs --help to list commands",
        )),
    }
}
