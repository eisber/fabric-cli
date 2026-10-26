// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[cfg(windows)]
use fabric_cli::parser::AuthAction;
use fabric_cli::parser::AuthCommand;
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    process::ExitCode,
};

fn error(code: &'static str, message: &str, exit: u8) -> io::Result<ExitCode> {
    write_json(
        io::stderr().lock(),
        &json!({"error": {"code": code, "message": message}}),
    )?;
    Ok(ExitCode::from(exit))
}

fn write_json(mut stream: impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut stream, value).map_err(io::Error::other)?;
    writeln!(stream)
}

pub(crate) fn run(command: &AuthCommand) -> io::Result<ExitCode> {
    #[cfg(windows)]
    {
        let options = match fabric_identity::LoginOptions::new(
            &command.tenant,
            &command.client_id,
            &command.account,
        ) {
            Ok(options) => options,
            Err(error_value) => return error("invalid_configuration", &error_value.to_string(), 2),
        };
        let timeout = command.timeout_seconds;
        let allow_interaction = command.allow_interaction;
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                return error(
                    "runtime_failed",
                    "The authentication runtime could not start.",
                    1,
                );
            }
        };
        let result = runtime.block_on(async {
            tokio::select! {
                result = execute(command.action, options, allow_interaction) => result,
                signal = tokio::signal::ctrl_c() => {
                    if signal.is_ok() { Err(("cancelled", "Authentication cancelled; broker cache changes are not rolled back.".to_owned())) }
                    else { Err(("signal_failed", "The cancellation handler could not start.".to_owned())) }
                },
                _ = tokio::time::sleep(std::time::Duration::from_secs(timeout)) => {
                    Err(("timed_out", "Authentication timed out; broker cleanup and cache changes may still be pending.".to_owned()))
                },
            }
        });
        match result {
            Ok(data) => {
                write_json(io::stdout().lock(), &data)?;
                Ok(ExitCode::SUCCESS)
            }
            Err((code, message)) => error(code, &message, 1),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = command;
        error(
            "unsupported",
            "Windows broker authentication requires Windows.",
            1,
        )
    }
}

#[cfg(windows)]
pub(crate) fn broker_error(
    error: fabric_identity::windows_broker::BrokerError,
) -> (&'static str, String) {
    use fabric_identity::windows_broker::BrokerError;
    let code = match error {
        BrokerError::InvalidConfiguration => "invalid_configuration",
        BrokerError::InteractionRequired => "interaction_required",
        BrokerError::Cancelled => "cancelled",
        BrokerError::IdentityMismatch | BrokerError::AmbiguousAccount => "identity_mismatch",
        BrokerError::InvalidResponse => "invalid_broker_response",
        BrokerError::ProviderUnavailable => "broker_unavailable",
        BrokerError::CompletionUnconfirmed { .. } => "completion_unconfirmed",
        _ => "authentication_failed",
    };
    (code, error.to_string())
}

#[cfg(windows)]
async fn execute(
    action: AuthAction,
    options: fabric_identity::LoginOptions,
    allow_interaction: bool,
) -> Result<Value, (&'static str, String)> {
    use fabric_identity::{WindowsCredential, sign_out};
    if action == AuthAction::Logout {
        sign_out(options).await.map_err(broker_error)?;
        return Ok(
            json!({"logged_in": false, "signed_out": true, "scope": "application_on_this_device"}),
        );
    }
    let tenant = options.tenant().to_owned();
    let account = options.account().to_owned();
    let credential = WindowsCredential::login(options, allow_interaction)
        .await
        .map_err(broker_error)?;
    let tokens: Vec<Value> = credential
        .metadata()
        .iter()
        .map(|metadata| {
            json!({
                "scope": metadata.scope, "expires_on": metadata.expires_on,
            })
        })
        .collect();
    Ok(json!({
        "logged_in": true, "authentication_mode": "Windows broker",
        "tenant_id": tenant, "account": account, "tokens": tokens,
        "mfa_evidence": "not_checked", "service_access_verified": false,
    }))
}
