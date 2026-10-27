// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[cfg(windows)]
#[path = "auth_commands.rs"]
mod commands;

#[cfg(windows)]
use fabric_cli::parser;
use fabric_cli::parser::AuthCommand;
use serde_json::json;
use std::{
    io::{self, Write},
    process::ExitCode,
};

fn error(code: &'static str, message: &str, exit: u8) -> io::Result<ExitCode> {
    let mut stderr = io::stderr().lock();
    serde_json::to_writer(
        &mut stderr,
        &json!({"error": {"code": code, "message": message}}),
    )
    .map_err(io::Error::other)?;
    writeln!(stderr)?;
    Ok(ExitCode::from(exit))
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
        let broker = WindowsBroker { options };
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
            commands::run(
                command.action,
                command.allow_interaction,
                &broker,
                tokio::signal::ctrl_c(),
                tokio::time::sleep(std::time::Duration::from_secs(command.timeout_seconds)),
            )
            .await
        });
        commands::write_result(result, io::stdout().lock(), io::stderr().lock()).map(ExitCode::from)
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
struct WindowsBroker {
    options: fabric_identity::LoginOptions,
}

#[cfg(windows)]
impl commands::Broker for WindowsBroker {
    fn tenant(&self) -> &str {
        self.options.tenant()
    }

    fn account(&self) -> &str {
        self.options.account()
    }

    async fn login(
        &self,
        allow_interaction: bool,
    ) -> Result<Vec<commands::TokenMetadata>, commands::Failure> {
        let credential =
            fabric_identity::WindowsCredential::login(self.options.clone(), allow_interaction)
                .await
                .map_err(broker_error)?;
        Ok(credential
            .metadata()
            .iter()
            .map(|metadata| commands::TokenMetadata {
                scope: metadata.scope,
                expires_on: metadata.expires_on,
            })
            .collect())
    }

    async fn logout(&self) -> Result<(), commands::Failure> {
        fabric_identity::sign_out(self.options.clone())
            .await
            .map_err(broker_error)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use fabric_identity::windows_broker::{BrokerError, NativeStage};

    #[test]
    fn existing_broker_error_classes_and_numeric_diagnostics_are_preserved() {
        for (error, expected) in [
            (BrokerError::InvalidConfiguration, "invalid_configuration"),
            (BrokerError::InteractionRequired, "interaction_required"),
            (BrokerError::Cancelled, "cancelled"),
            (BrokerError::IdentityMismatch, "identity_mismatch"),
            (BrokerError::AmbiguousAccount, "identity_mismatch"),
            (BrokerError::InvalidResponse, "invalid_broker_response"),
            (BrokerError::ProviderUnavailable, "broker_unavailable"),
            (
                BrokerError::CompletionUnconfirmed { code: -1 },
                "completion_unconfirmed",
            ),
            (
                BrokerError::AccountsUnavailable { status: 99 },
                "authentication_failed",
            ),
            (
                BrokerError::Provider {
                    status: 5,
                    code: 1234,
                },
                "authentication_failed",
            ),
            (BrokerError::ThreadStart, "authentication_failed"),
            (
                BrokerError::Native {
                    stage: NativeStage::Request,
                    code: -1,
                },
                "authentication_failed",
            ),
        ] {
            let (code, message) = broker_error(error);
            assert_eq!(code, expected);
            assert_eq!(message, error.to_string());
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            assert_eq!(
                commands::write_result(Err((code, message.clone())), &mut stdout, &mut stderr)
                    .unwrap(),
                1
            );
            assert!(stdout.is_empty());
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&stderr).unwrap(),
                json!({"error": {"code": expected, "message": message}})
            );
        }
    }

    #[test]
    fn production_adapter_keeps_validated_explicit_selection_without_native_access() {
        let broker = WindowsBroker {
            options: fabric_identity::LoginOptions::new(
                "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
                "11111111-2222-3333-4444-555555555555",
                "selected@example.test",
            )
            .unwrap(),
        };
        assert_eq!(
            commands::Broker::tenant(&broker),
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        );
        assert_eq!(
            broker.options.client(),
            "11111111-2222-3333-4444-555555555555"
        );
        assert_eq!(commands::Broker::account(&broker), "selected@example.test");
    }
}
