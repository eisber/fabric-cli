// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::batch_state::CommandOutcome;
use fabric_cli::auth_commands as commands;
use fabric_cli::parser::AuthCommand;
use fabric_cli_output::text::OutputFormat;
#[cfg(all(test, windows, feature = "windows-broker"))]
use serde_json::json;
use std::io;
#[cfg(any(all(windows, feature = "windows-broker"), feature = "interactive"))]
use std::io::Write;
#[cfg(all(windows, feature = "windows-broker"))]
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

#[cfg(all(windows, feature = "windows-broker"))]
#[derive(Default)]
struct AuthProgress(AtomicU8);

#[cfg(all(windows, feature = "windows-broker"))]
impl AuthProgress {
    fn selection(&self, stage: fabric_identity::windows_broker::AccountSelectionStage) {
        use fabric_identity::windows_broker::AccountSelectionStage;
        self.0.store(
            match stage {
                AccountSelectionStage::Provider => 1,
                AccountSelectionStage::Interactive => 2,
                AccountSelectionStage::Response => 3,
            },
            Ordering::Release,
        );
    }

    fn acquiring(&self) {
        self.0.store(0, Ordering::Release);
    }

    fn timeout(&self) -> commands::Failure {
        let stage = match self.0.load(Ordering::Acquire) {
            1 => "waiting for the Windows work-account provider",
            2 => "waiting for the Windows sign-in dialog",
            3 => "validating the selected Windows account",
            _ => "acquiring or validating the selected session",
        };
        (
            "timed_out",
            format!(
                "Authentication timed out while {stage}. Check the Windows sign-in window. To bypass account selection, run auth login --tenant <TENANT_GUID> --account <WORK_ACCOUNT_UPN>. Broker cleanup and cache changes may still be pending."
            ),
        )
    }
}

fn error(
    code: &'static str,
    message: &str,
    exit: u8,
    format: OutputFormat,
) -> io::Result<CommandOutcome> {
    fabric_cli::auth_commands::write_error_with_format(
        code,
        message,
        exit,
        format,
        io::stderr().lock(),
    )
    .map(CommandOutcome::Returned)
}

fn selected_format(
    command: &AuthCommand,
) -> Result<OutputFormat, fabric_cli::auth_commands::Failure> {
    if let Some(format) = &command.output_format {
        return OutputFormat::resolve(Some(format), None).map_err(|_| {
            (
                "invalid_configuration",
                "Invalid authentication output format.".into(),
            )
        });
    }
    let settings = crate::local_host::available_store()
        .and_then(|store| store.map(|store| store.read()).transpose())
        .map_err(|_| ("invalid_configuration", "Cannot read authentication output settings. Check the Fabric CLI configuration directory and config.json.".into()))?;
    let configured = settings.as_ref().and_then(|settings| {
        settings
            .get("output_format")
            .or_else(|| settings.get("fab_output_format"))
    });
    if configured.is_some_and(|value| !value.is_string()) {
        return Err((
            "invalid_configuration",
            "Invalid saved output_format setting.".into(),
        ));
    }
    OutputFormat::resolve(
        None,
        configured
            .and_then(serde_json::Value::as_str)
            .or(Some("text")),
    )
    .map_err(|_| {
        (
            "invalid_configuration",
            "Invalid saved output_format setting.".into(),
        )
    })
}

fn selected_method(
    explicit: Option<fabric_cli::parser::AuthMethod>,
    environment: Result<Option<String>, ()>,
    platform_windows: bool,
    broker_available: bool,
    device_available: bool,
) -> Result<fabric_cli::parser::AuthMethod, commands::Failure> {
    use fabric_cli::parser::AuthMethod;
    let method = match explicit {
        Some(method) => method,
        None => match environment {
            Ok(Some(mode)) if mode == "broker" => AuthMethod::Broker,
            Ok(Some(mode)) if mode == "device-code" => AuthMethod::DeviceCode,
            Ok(None) if platform_windows => AuthMethod::Broker,
            Ok(None) => AuthMethod::DeviceCode,
            _ => return Err((
                "invalid_configuration",
                "auth commands require an explicitly selected interactive method. Headless modes never start device-code authentication.".into(),
            )),
        },
    };
    match method {
        AuthMethod::Broker if !broker_available => Err((
            "unsupported",
            "Windows broker authentication requires Windows and the windows-broker feature. No provider fallback was attempted.".into(),
        )),
        AuthMethod::DeviceCode if !device_available => Err((
            "unsupported",
            "Device-code authentication requires an interactive feature build; no provider fallback was attempted.".into(),
        )),
        _ => Ok(method),
    }
}

pub(crate) fn run(command: &AuthCommand) -> io::Result<CommandOutcome> {
    let environment = if command.method.is_some() {
        Ok(None)
    } else {
        match std::env::var("FAB_RUST_AUTH_MODE") {
            Ok(mode) => Ok(Some(mode)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(()),
        }
    };
    let method = match selected_method(
        command.method,
        environment,
        cfg!(windows),
        cfg!(all(windows, feature = "windows-broker")),
        cfg!(feature = "interactive"),
    ) {
        Ok(method) => method,
        Err((code, message)) => {
            // Unsupported explicit providers fail before profile/configuration,
            // credential storage, HTTP or UI. Only argv selects error format.
            let format = OutputFormat::resolve(command.output_format.as_deref(), None)
                .unwrap_or(OutputFormat::Text);
            return error(
                code,
                &message,
                if code == "invalid_configuration" {
                    2
                } else {
                    1
                },
                format,
            );
        }
    };
    let format = match selected_format(command) {
        Ok(format) => format,
        Err((code, message)) => return error(code, &message, 2, OutputFormat::Text),
    };
    if method == fabric_cli::parser::AuthMethod::DeviceCode {
        #[cfg(feature = "interactive")]
        return crate::interactive::run(command, format);
        #[cfg(not(feature = "interactive"))]
        return error(
            "unsupported",
            "Device-code authentication requires an interactive feature build; no provider fallback was attempted.",
            1,
            format,
        );
    }
    #[cfg(all(windows, feature = "windows-broker"))]
    {
        use crate::auth_binding::{Hints, Store};
        use fabric_cli::parser::AuthAction;
        let inputs = (|| {
            let hints = Hints::capture(
                command.tenant.as_deref(),
                command.account.as_deref(),
                command
                    .client_id_explicit
                    .then_some(command.client_id.as_str()),
            )?;
            let store = Store::available()?;
            let previous = store.as_ref().map(Store::read).transpose()?.flatten();
            let hints = hints.resolve(previous.as_ref());
            if command.action == AuthAction::Login {
                if hints.complete()?.is_none() && !command.allow_interaction {
                    return Err((
                        "interaction_required",
                        "Silent login requires a saved selection or explicit tenant and account. Run auth login interactively to select an account.".into(),
                    ));
                }
                store.as_ref().ok_or_else(|| {
                    ("NotConfigured", "Login needs a writable non-secret selection directory. Set LOCALAPPDATA or FAB_RUST_CONFIG_ROOT to an absolute directory.".into())
                })?.prepare()?;
            }
            Ok((hints, store, previous))
        })();
        let (hints, store, previous) = match inputs {
            Ok(inputs) => inputs,
            Err((code, message)) => {
                return error(
                    code,
                    &message,
                    if code == "invalid_configuration" {
                        2
                    } else {
                        1
                    },
                    format,
                );
            }
        };
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
                    format,
                );
            }
        };
        let progress = Arc::new(AuthProgress::default());
        let result = runtime.block_on(async {
            run_until(
                execute_selected(command, hints, Arc::clone(&progress)),
                &progress,
                tokio::signal::ctrl_c(),
                tokio::time::sleep(std::time::Duration::from_secs(command.timeout_seconds)),
            )
            .await
        });
        let result = persist_result(command.action, store.as_ref(), previous.as_ref(), result);
        write_auth_outcome_with_format(result, format, io::stdout().lock(), io::stderr().lock())
    }
    #[cfg(not(all(windows, feature = "windows-broker")))]
    {
        let _ = command;
        error(
            "unsupported",
            "Windows broker authentication requires Windows.",
            1,
            format,
        )
    }
}

#[cfg(all(windows, feature = "windows-broker"))]
async fn run_until<T>(
    work: impl std::future::Future<Output = Result<T, commands::Failure>>,
    progress: &AuthProgress,
    cancelled: impl std::future::Future<Output = io::Result<()>>,
    deadline: impl std::future::Future<Output = ()>,
) -> Result<T, commands::Failure> {
    tokio::select! {
        result = work => result,
        signal = cancelled => {
            if signal.is_ok() {
                Err(("cancelled", "Authentication cancelled; broker cache changes are not rolled back.".into()))
            } else {
                Err(("signal_failed", "The cancellation handler could not start.".into()))
            }
        },
        () = deadline => Err(progress.timeout()),
    }
}

#[cfg(all(windows, feature = "windows-broker"))]
fn persist_result(
    action: fabric_cli::parser::AuthAction,
    store: Option<&crate::auth_binding::Store>,
    previous: Option<&crate::auth_binding::Binding>,
    result: Result<(serde_json::Value, fabric_identity::LoginOptions), commands::Failure>,
) -> Result<serde_json::Value, commands::Failure> {
    use fabric_cli::parser::AuthAction;
    let (value, options) = result?;
    if action == AuthAction::Login {
        store
            .ok_or_else(|| {
                (
                    "auth_state_failed",
                    "The broker selection directory is unavailable.".into(),
                )
            })?
            .save(&options)
            .map_err(|(code, message)| {
                (
                    code,
                    format!("Authentication completed, but its selection was not saved. {message}"),
                )
            })?;
    } else if action == AuthAction::Logout
        && let Some(store) = store
    {
        store.clear_if_unchanged(previous.filter(|binding| binding.matches(&options)))
            .map_err(|(code, message)| {
                (code, format!("Broker sign-out completed, but its saved selection could not be cleared. {message}"))
            })?;
    }
    Ok(value)
}

#[cfg(all(windows, feature = "windows-broker"))]
async fn execute_selected(
    command: &AuthCommand,
    hints: crate::auth_binding::Hints,
    progress: Arc<AuthProgress>,
) -> Result<(serde_json::Value, fabric_identity::LoginOptions), commands::Failure> {
    let options = match hints.complete()? {
        Some(options) => options,
        None if command.action == fabric_cli::parser::AuthAction::Login
            && command.allow_interaction =>
        {
            let selection_progress = Arc::clone(&progress);
            fabric_identity::windows_broker::select_account_with_progress(
                hints.client.ok_or_else(|| {
                    (
                        "invalid_configuration",
                        "A broker client ID is required.".into(),
                    )
                })?,
                hints.tenant,
                hints.account,
                fabric_identity::windows_broker::Interaction::Allowed,
                move |stage| selection_progress.selection(stage),
            )
            .await
            .map_err(|error| {
                let (code, message) = broker_error(error);
                (code, format!("Windows account selection failed: {message}"))
            })?
        }
        None => {
            return Err((
                "NotConfigured",
                "No Windows broker account is selected. Run fab-rs auth login first, or supply --tenant and --account.".into(),
            ));
        }
    };
    progress.acquiring();
    let broker = WindowsBroker {
        options: options.clone(),
    };
    let value = fabric_cli::session::run_auth(
        command,
        &broker,
        std::future::pending(),
        std::future::pending(),
    )
    .await
    .map_err(|(code, message)| {
        (
            code,
            format!("Selected-session authentication failed: {message}"),
        )
    })?;
    Ok((value, options))
}

#[cfg(all(test, windows, feature = "windows-broker"))]
fn write_auth_outcome(
    result: Result<serde_json::Value, commands::Failure>,
    stdout: impl Write,
    stderr: impl Write,
) -> io::Result<CommandOutcome> {
    write_auth_outcome_with_format(result, OutputFormat::Json, stdout, stderr)
}

#[cfg(any(all(windows, feature = "windows-broker"), feature = "interactive"))]
pub(crate) fn write_auth_outcome_with_format(
    result: Result<serde_json::Value, commands::Failure>,
    format: OutputFormat,
    stdout: impl Write,
    stderr: impl Write,
) -> io::Result<CommandOutcome> {
    let cancelled = matches!(&result, Err(("cancelled", _)));
    let outcome = commands::write_outcome_with_format(result, format, stdout, stderr)?.into();
    Ok(match (cancelled, outcome) {
        (true, CommandOutcome::Stop(exit) | CommandOutcome::Returned(exit)) => {
            CommandOutcome::Cancelled(exit)
        }
        (_, outcome) => outcome,
    })
}

#[cfg(all(windows, feature = "windows-broker"))]
pub(crate) fn broker_error(
    error: fabric_identity::windows_broker::BrokerError,
) -> (&'static str, String) {
    use fabric_identity::windows_broker::BrokerError;
    let code = match error {
        BrokerError::InvalidConfiguration => "invalid_configuration",
        BrokerError::InteractionRequired => "interaction_required",
        BrokerError::Cancelled => "cancelled",
        BrokerError::IdentityMismatch | BrokerError::AmbiguousAccount => "identity_mismatch",
        BrokerError::InvalidResponse | BrokerError::ResponseMetadata { .. } => {
            "invalid_broker_response"
        }
        BrokerError::ProviderUnavailable => "broker_unavailable",
        BrokerError::InteractiveWindowUnavailable => "interactive_window_unavailable",
        BrokerError::CompletionUnconfirmed { .. } => "completion_unconfirmed",
        BrokerError::IdentityCapacityExhausted { .. } => "identity_capacity_exhausted",
        BrokerError::CapacityExhausted { .. } => "broker_capacity_exhausted",
        BrokerError::OperationInProgress => "authentication_busy",
        BrokerError::SessionInvalidated => "session_invalidated",
        BrokerError::CleanupPending { .. } => "cleanup_pending",
        BrokerError::TimedOut => "timed_out",
        BrokerError::CoordinationFailed => "coordination_failed",
        _ => "authentication_failed",
    };
    (code, error.to_string())
}

#[cfg(all(windows, feature = "windows-broker"))]
struct WindowsBroker {
    options: fabric_identity::LoginOptions,
}

#[cfg(all(windows, feature = "windows-broker"))]
fn token_metadata(credential: &fabric_identity::WindowsCredential) -> Vec<commands::TokenMetadata> {
    credential
        .metadata()
        .iter()
        .map(|metadata| commands::TokenMetadata {
            scope: metadata.scope,
            expires_on: metadata.expires_on,
        })
        .collect()
}

#[cfg(all(windows, feature = "windows-broker"))]
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
        Ok(token_metadata(&credential))
    }

    async fn status(&self) -> Result<Vec<commands::TokenMetadata>, commands::Failure> {
        let credential = fabric_identity::WindowsCredential::status(self.options.clone())
            .await
            .map_err(broker_error)?;
        Ok(token_metadata(&credential))
    }

    async fn logout(&self) -> Result<(), commands::Failure> {
        fabric_identity::sign_out(self.options.clone())
            .await
            .map_err(broker_error)
    }
}

#[cfg(test)]
mod method_tests {
    #![deny(unnameable_test_items)]
    use super::*;
    use fabric_cli::parser::AuthMethod;

    #[test]
    fn argv_method_overrides_every_environment_mode() {
        for platform_windows in [false, true] {
            for method in [AuthMethod::Broker, AuthMethod::DeviceCode] {
                for mode in ["broker", "device-code", "client-secret", ""] {
                    assert_eq!(
                        selected_method(
                            Some(method),
                            Ok(Some(mode.into())),
                            platform_windows,
                            true,
                            true,
                        )
                        .unwrap(),
                        method,
                    );
                }
                assert_eq!(
                    selected_method(Some(method), Err(()), platform_windows, true, true).unwrap(),
                    method
                );
            }
        }
    }

    #[test]
    fn explicit_broker_never_falls_back_to_device_code_on_an_unsupported_host() {
        for platform_windows in [false, true] {
            for selected in [None, Some(AuthMethod::Broker)] {
                assert_eq!(
                    selected_method(
                        selected,
                        Ok(Some("broker".into())),
                        platform_windows,
                        false,
                        true
                    )
                    .unwrap_err()
                    .0,
                    "unsupported",
                );
            }
        }
    }

    #[test]
    fn only_absent_method_and_absent_mode_use_the_platform_default() {
        for platform_windows in [false, true] {
            for broker_available in [false, true] {
                for device_available in [false, true] {
                    let result = selected_method(
                        None,
                        Ok(None),
                        platform_windows,
                        broker_available,
                        device_available,
                    );
                    let available = if platform_windows {
                        broker_available
                    } else {
                        device_available
                    };
                    if available {
                        assert_eq!(
                            result.unwrap(),
                            if platform_windows {
                                AuthMethod::Broker
                            } else {
                                AuthMethod::DeviceCode
                            },
                        );
                    } else {
                        assert_eq!(result.unwrap_err().0, "unsupported");
                    }
                }
            }
            for (mode, method) in [
                ("broker", AuthMethod::Broker),
                ("device-code", AuthMethod::DeviceCode),
            ] {
                assert_eq!(
                    selected_method(None, Ok(Some(mode.into())), platform_windows, true, true)
                        .unwrap(),
                    method,
                );
            }
        }
    }

    #[test]
    fn unavailable_device_and_explicit_headless_modes_do_not_choose_a_fallback() {
        assert_eq!(
            selected_method(None, Ok(Some("device-code".into())), true, true, false)
                .unwrap_err()
                .0,
            "unsupported",
        );
        for environment in [
            Ok(Some("client-secret".into())),
            Ok(Some(String::new())),
            Err(()),
        ] {
            assert_eq!(
                selected_method(None, environment, true, true, true)
                    .unwrap_err()
                    .0,
                "invalid_configuration"
            );
        }
    }
}

#[cfg(all(test, windows, feature = "windows-broker"))]
mod tests {
    use super::*;
    use fabric_identity::windows_broker::{BrokerError, NativeStage};

    #[tokio::test]
    async fn one_deadline_cancels_pending_selection_and_reports_its_actual_phase() {
        use fabric_identity::windows_broker::AccountSelectionStage;
        use std::{cell::Cell, future::pending, time::Duration};
        struct Dropped<'a>(&'a Cell<bool>);
        impl Drop for Dropped<'_> {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        for stage in [
            AccountSelectionStage::Provider,
            AccountSelectionStage::Interactive,
            AccountSelectionStage::Response,
        ] {
            let progress = AuthProgress::default();
            let dropped = Cell::new(false);
            let entered = Cell::new(false);
            let work = async {
                let _guard = Dropped(&dropped);
                entered.set(true);
                progress.selection(stage);
                pending::<Result<(), commands::Failure>>().await
            };
            let result = run_until(
                work,
                &progress,
                pending(),
                tokio::time::sleep(Duration::from_millis(5)),
            )
            .await
            .unwrap_err();
            assert!(entered.get());
            assert!(dropped.get());
            assert_eq!(result, progress.timeout());
            assert!(result.1.contains(match stage {
                AccountSelectionStage::Provider => "work-account provider",
                AccountSelectionStage::Interactive => "sign-in dialog",
                AccountSelectionStage::Response => "validating the selected Windows account",
            }));
        }
    }

    #[tokio::test]
    async fn cancellation_and_signal_failure_remain_distinct_from_picker_timeout() {
        use std::future::{pending, ready};
        let progress = AuthProgress::default();
        let result = run_until::<()>(pending(), &progress, ready(Ok(())), pending())
            .await
            .unwrap_err();
        assert_eq!(result.0, "cancelled");
        let result = run_until::<()>(
            pending(),
            &progress,
            ready(Err(io::Error::other("synthetic"))),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(result.0, "signal_failed");
        progress.acquiring();
        assert!(
            progress
                .timeout()
                .1
                .contains("acquiring or validating the selected session")
        );
    }

    #[test]
    fn only_a_completed_login_persists_selection_and_status_cannot_write_it() {
        use crate::auth_binding::Store;
        use fabric_cli::parser::AuthAction;
        use std::{
            path::PathBuf,
            sync::atomic::{AtomicU64, Ordering},
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = std::env::var_os("FAB_IDENTITY_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = parent.join(format!(
            "auth-result-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Store::new(root.clone()).unwrap();
        let options = fabric_identity::LoginOptions::new(
            "11111111-2222-3333-4444-555555555555",
            fabric_cli::parser::DEFAULT_BROKER_CLIENT_ID,
            "selected@example.test",
        )
        .unwrap();
        for action in [AuthAction::Login, AuthAction::Status, AuthAction::Logout] {
            let result = persist_result(
                action,
                Some(&store),
                None,
                Err(("cancelled", "Synthetic cancellation".into())),
            );
            assert!(result.is_err());
            assert!(!root.exists());
        }
        let value = serde_json::json!({"logged_in":true});
        assert_eq!(
            persist_result(
                AuthAction::Status,
                Some(&store),
                None,
                Ok((value.clone(), options.clone()))
            )
            .unwrap(),
            value
        );
        assert!(!root.exists());
        persist_result(
            AuthAction::Login,
            Some(&store),
            None,
            Ok((value.clone(), options.clone())),
        )
        .unwrap();
        let saved = store.read().unwrap().unwrap();
        assert!(saved.matches(&options));
        let another = fabric_identity::LoginOptions::new(
            options.tenant(),
            options.client(),
            "other@example.test",
        )
        .unwrap();
        persist_result(
            AuthAction::Logout,
            Some(&store),
            Some(&saved),
            Ok((serde_json::json!({"signed_out":true}), another)),
        )
        .unwrap();
        assert!(store.read().unwrap().is_some());
        persist_result(
            AuthAction::Logout,
            Some(&store),
            Some(&saved),
            Ok((serde_json::json!({"signed_out":true}), options.clone())),
        )
        .unwrap();
        assert!(store.read().unwrap().is_none());
        std::fs::write(root.join("broker-session.json"), b"broken-state").unwrap();
        let result = persist_result(AuthAction::Login, Some(&store), None, Ok((value, options)));
        let (code, message) = result.unwrap_err();
        assert_eq!(code, "invalid_configuration");
        assert!(message.contains("selection was not saved"));
        assert_eq!(
            std::fs::read(root.join("broker-session.json")).unwrap(),
            b"broken-state"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn auth_cancellation_retains_its_reason_without_confusing_timeouts() {
        for (code, expected) in [
            ("cancelled", CommandOutcome::Cancelled(1)),
            ("timed_out", CommandOutcome::Stop(1)),
            ("cleanup_pending", CommandOutcome::Stop(1)),
            ("completion_unconfirmed", CommandOutcome::Stop(1)),
            ("signal_failed", CommandOutcome::Stop(1)),
            ("authentication_failed", CommandOutcome::Returned(1)),
        ] {
            let mut error = Vec::new();
            let actual = write_auth_outcome(
                Err((code, "Synthetic failure".into())),
                Vec::new(),
                &mut error,
            )
            .unwrap();
            assert_eq!(actual, expected);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&error).unwrap()["error"]["code"],
                code
            );
        }
    }

    #[test]
    fn existing_broker_error_classes_and_numeric_diagnostics_are_preserved() {
        for (error, expected) in [
            (BrokerError::InvalidConfiguration, "invalid_configuration"),
            (BrokerError::InteractionRequired, "interaction_required"),
            (BrokerError::Cancelled, "cancelled"),
            (BrokerError::IdentityMismatch, "identity_mismatch"),
            (BrokerError::AmbiguousAccount, "identity_mismatch"),
            (BrokerError::InvalidResponse, "invalid_broker_response"),
            (
                BrokerError::ResponseMetadata {
                    field: fabric_identity::windows_broker::ResponseField::Authority,
                },
                "invalid_broker_response",
            ),
            (BrokerError::ProviderUnavailable, "broker_unavailable"),
            (
                BrokerError::CompletionUnconfirmed { code: -1 },
                "completion_unconfirmed",
            ),
            (
                BrokerError::IdentityCapacityExhausted { limit: 64 },
                "identity_capacity_exhausted",
            ),
            (
                BrokerError::CapacityExhausted {
                    active: 4,
                    limit: 4,
                },
                "broker_capacity_exhausted",
            ),
            (BrokerError::OperationInProgress, "authentication_busy"),
            (BrokerError::SessionInvalidated, "session_invalidated"),
            (
                BrokerError::CleanupPending {
                    active: 4,
                    limit: 4,
                },
                "cleanup_pending",
            ),
            (BrokerError::TimedOut, "timed_out"),
            (BrokerError::CoordinationFailed, "coordination_failed"),
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
    fn response_metadata_errors_keep_the_public_code_and_only_fixed_field_labels() {
        use fabric_identity::windows_broker::ResponseField;
        for field in [
            ResponseField::RecordCount,
            ResponseField::Account,
            ResponseField::AccountId,
            ResponseField::AccountName,
            ResponseField::Properties,
            ResponseField::Authority,
            ResponseField::TenantId,
            ResponseField::Expiry,
            ResponseField::Token,
        ] {
            let (code, message) = broker_error(BrokerError::ResponseMetadata { field });
            assert_eq!(code, "invalid_broker_response");
            assert_eq!(
                message,
                format!("the broker response has missing or invalid {field} metadata")
            );
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            assert_eq!(
                write_auth_outcome(Err((code, message.clone())), &mut stdout, &mut stderr).unwrap(),
                CommandOutcome::Returned(1)
            );
            assert!(stdout.is_empty());
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&stderr).unwrap(),
                json!({"error":{"code":"invalid_broker_response","message":message}})
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
