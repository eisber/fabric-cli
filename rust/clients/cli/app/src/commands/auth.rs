// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::selected;
use crate::{Host, progress::ConsoleProgress};
use azure_core::credentials::TokenRequestOptions;
use clap::{Arg, ArgAction, Command};
use fabric_cli_command_contracts::{AuthOutput, CommandError, CommandOutput};
use fabric_core::{Audience, CancellationToken, OperationContext, diagnostics::DiagnosticStage};
use fabric_sdk_http::bounded_with_clock;
use std::{sync::Arc, time::Duration};

pub(crate) fn command() -> Command {
    let help_arg = || Arg::new("help").long("help").action(ArgAction::Help);
    let auth_leaf = |name| Command::new(name).disable_help_flag(true).arg(help_arg());
    Command::new("auth")
        .about(executable_text!(
            "Bind ",
            " to an existing Azure CLI identity."
        ))
        .after_help(executable_text!(
            "",
            " does not sign in to or sign out of Azure CLI; run az login separately."
        ))
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .arg(help_arg())
        .subcommand_required(true)
        .subcommand(
            auth_leaf("login").about("Validate and bind the current Azure CLI tenant/principal"),
        )
        .subcommand(auth_leaf("status").about("Validate the selected Azure CLI identity"))
        .subcommand(
            auth_leaf("logout").about("Clear Rust selection only; leave Azure CLI signed in"),
        )
}

pub(super) async fn execute(
    auth: &clap::ArgMatches,
    host: &dyn Host,
    cancellation: CancellationToken,
    observer: &Arc<ConsoleProgress>,
) -> Result<CommandOutput, CommandError> {
    match auth.subcommand_name() {
        Some("logout") => {
            let started = observer.start(DiagnosticStage::Selection).await?;
            observer
                .finish(DiagnosticStage::Selection, started, host.clear_selection())
                .await?;
            Ok(CommandOutput::Auth(AuthOutput::Logout))
        }
        Some("login") => {
            let started = observer.start(DiagnosticStage::Authentication).await?;
            let context =
                OperationContext::new(host.clock().now(), Duration::from_secs(240), cancellation)?;
            let binding = observer
                .finish(
                    DiagnosticStage::Authentication,
                    started,
                    host.login(&context).await,
                )
                .await?;
            let started = observer.start(DiagnosticStage::Selection).await?;
            observer
                .finish(
                    DiagnosticStage::Selection,
                    started,
                    host.save_selection(&binding),
                )
                .await?;
            Ok(CommandOutput::Auth(AuthOutput::Login {
                tenant_id: binding.tenant,
                principal_id: binding.principal,
            }))
        }
        Some("status") => {
            let started = observer.start(DiagnosticStage::Selection).await?;
            let binding = observer
                .finish(DiagnosticStage::Selection, started, selected(host))
                .await?;
            let started = observer.start(DiagnosticStage::Authentication).await?;
            let credential = host.credential(binding.clone())?;
            let clock = host.clock();
            let context = OperationContext {
                deadline: Some(clock.now() + Duration::from_secs(240)),
                cancellation,
            };
            let result = bounded_with_clock(
                clock.as_ref(),
                &context,
                credential.get_token(
                    &[Audience::Fabric.scope()],
                    Some(TokenRequestOptions::default()),
                ),
            )
            .await?
            .map_err(|_| {
                CommandError::new(
                    "Unauthorized",
                    format!("Credential acquisition failed; run {} auth login to validate the selected session", crate::EXECUTABLE_NAME),
                )
            });
            observer
                .finish(DiagnosticStage::Authentication, started, result)
                .await?;
            Ok(CommandOutput::Auth(AuthOutput::Status {
                tenant_id: binding.tenant,
                principal_id: binding.principal,
            }))
        }
        _ => Err(CommandError::new(
            "InvalidInput",
            format!(
                "An auth action is required; use {} auth login, logout, or status",
                crate::EXECUTABLE_NAME
            ),
        )),
    }
}
