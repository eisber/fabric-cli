// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::batch_state::CommandOutcome;
use fabric_cli::feedback::{Configuration, Delivery, FeedbackCommand};
use serde_json::{Value, json};
use std::io::{self, Write};

pub(crate) fn run(command: &FeedbackCommand) -> io::Result<CommandOutcome> {
    if let Err(message) = command.validate() {
        return invalid(message);
    }

    let config = match Configuration::capture(command.send, |name| match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err("Feedback configuration must contain valid Unicode."),
    }) {
        Ok(config) => config,
        Err(message) => return invalid(message),
    };
    let command = match prepare(command, || {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| "The operating system could not generate a feedback correlation ID; nothing was sent.")?;
        Ok(bytes)
    }) {
        Ok(command) => command,
        Err(message) => return invalid(message),
    };
    let command = &command;
    let payload = match command.manifest(&config, chrono::Utc::now()) {
        Ok(payload) => payload,
        Err(message) => return invalid(message),
    };
    if !command.send {
        write_value(
            command,
            &json!({
                "delivery": "not_sent",
                "environment": config.environment.name(),
                "endpoint": config.environment.endpoint(),
                "app_id": config.environment.app_id(),
                "client_feedback_id": command.feedback_id,
                "message": "Offline preview only. Review caller-authored content and routing before --send; no identity or service access occurred.",
                "payload": payload,
            }),
            &mut io::stdout().lock(),
        )?;
        return Ok(CommandOutcome::Returned(0));
    }
    let result = send(command, &config)?;
    write_delivery(
        command,
        result,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

fn prepare(
    command: &FeedbackCommand,
    random: impl FnOnce() -> Result<[u8; 16], &'static str>,
) -> Result<FeedbackCommand, &'static str> {
    command.validate()?;
    let mut command = command.clone();
    if command.feedback_id.is_none() {
        command.feedback_id = Some(
            uuid::Builder::from_random_bytes(random()?)
                .into_uuid()
                .to_string(),
        );
    }
    Ok(command)
}

fn write_delivery(
    command: &FeedbackCommand,
    result: Delivery,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<CommandOutcome> {
    let accepted = result.accepted();
    let value = serde_json::to_value(result).map_err(io::Error::other)?;
    if accepted {
        write_value(command, &value, stdout)?;
        Ok(CommandOutcome::Returned(0))
    } else {
        write_value(command, &value, stderr)?;
        Ok(CommandOutcome::Stop(1))
    }
}

fn invalid(message: &'static str) -> io::Result<CommandOutcome> {
    serde_json::to_writer(
        io::stderr().lock(),
        &json!({"error":{"code":"invalid_configuration","message":message}}),
    )
    .map_err(io::Error::other)?;
    writeln!(io::stderr().lock())?;
    Ok(CommandOutcome::Stop(2))
}

fn write_value(command: &FeedbackCommand, value: &Value, out: &mut impl Write) -> io::Result<()> {
    if command.output_format.as_deref() == Some("text") {
        writeln!(
            out,
            "{}: environment={}, app={}, feedback={}",
            value["delivery"].as_str().unwrap_or("unknown"),
            value["environment"].as_str().unwrap_or("unknown"),
            value["app_id"],
            value["client_feedback_id"].as_str().unwrap_or("unknown"),
        )?;
        if let Some(status) = value.get("http_status").and_then(Value::as_u64) {
            writeln!(out, "HTTP {status}")?;
        }
        writeln!(out, "{}", value["message"].as_str().unwrap_or(""))?;
        if let Some(payload) = value.get("payload") {
            serde_json::to_writer_pretty(&mut *out, payload).map_err(io::Error::other)?;
            writeln!(out)?;
        }
    } else {
        serde_json::to_writer_pretty(&mut *out, value).map_err(io::Error::other)?;
        writeln!(out)?;
    }
    Ok(())
}

#[cfg(not(all(windows, feature = "native-host", feature = "windows-broker")))]
fn send(command: &FeedbackCommand, config: &Configuration) -> io::Result<Delivery> {
    Ok(Delivery::new(
        command,
        config,
        "not_sent",
        "Sending feedback requires Windows and a fab-rs build with --features native-read. Preview remains available on all platforms.",
    ))
}

#[cfg(all(windows, feature = "native-host", feature = "windows-broker"))]
fn send(command: &FeedbackCommand, config: &Configuration) -> io::Result<Delivery> {
    use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
    use fabric_sdk_transport::{
        NativeHttpClient, ProxyPolicy, TokioTimer, TransportOptions, TrustPolicy,
    };
    use std::{num::NonZeroUsize, sync::Arc, time::Duration};

    let not_sent = |message| Delivery::new(command, config, "not_sent", message);
    match std::env::var("FAB_RUST_AUTH_MODE") {
        Ok(mode) if mode == "broker" => {}
        Err(std::env::VarError::NotPresent) => {}
        _ => {
            return Ok(not_sent(
                "Feedback supports only explicit Windows broker identity. Unset FAB_RUST_AUTH_MODE or set it to broker; other providers are not silently replaced.",
            ));
        }
    }
    let selection = match crate::auth_binding::selected() {
        Ok(selection) => selection,
        Err(_) => {
            return Ok(not_sent(
                "Select a broker account with auth login, or set FAB_RUST_TENANT, FAB_RUST_ACCOUNT and FAB_RUST_CLIENT_ID. No identity fallback occurred.",
            ));
        }
    };
    let client = match NativeHttpClient::new(TransportOptions {
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(30),
        write_timeout: Duration::from_secs(30),
        pool_idle_timeout: Duration::from_secs(60),
        max_idle_per_host: NonZeroUsize::new(1).expect("nonzero constant"),
        trust: TrustPolicy::Platform {
            additional_roots: Vec::new(),
        },
        proxy: ProxyPolicy::None,
    }) {
        Ok(client) => client,
        Err(_) => {
            return Ok(not_sent(
                "Feedback HTTPS transport initialization failed; nothing was sent.",
            ));
        }
    };
    let executor = match HttpExecutor::new(
        Arc::new(client),
        Arc::new(TokioTimer),
        ExecutionOptions {
            timeout: Duration::from_secs(60),
            ..ExecutionOptions::default()
        },
    ) {
        Ok(executor) => executor,
        Err(_) => {
            return Ok(not_sent(
                "Feedback HTTP executor initialization failed; nothing was sent.",
            ));
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            return Ok(not_sent(
                "Feedback runtime initialization failed; nothing was sent.",
            ));
        }
    };
    let result = runtime.block_on(async {
        let operation = async {
            let audience = match config.environment {
                fabric_cli::feedback::Environment::Production => fabric_identity::windows_broker::TokenAudience::Ocv,
                fabric_cli::feedback::Environment::Ppe => fabric_identity::windows_broker::TokenAudience::OcvPpe,
            };
            let credential = match fabric_identity::WindowsCredential::connect_for_audience(selection, audience).await {
                Ok(credential) => credential,
                Err(_) => return not_sent("OCV broker authentication failed for the selected environment. Check feedback preauthorization or explicitly run auth login if interaction is required; no feedback request was sent."),
            };
            fabric_cli::feedback::submit(command, config, chrono::Utc::now(), credential.as_ref(), &executor).await
        };
        attempt(
            command, config, operation, tokio::signal::ctrl_c(),
            tokio::time::sleep(Duration::from_secs(120)), &mut io::stderr().lock(),
        ).await
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

#[cfg(all(windows, feature = "native-host", feature = "windows-broker"))]
async fn attempt(
    command: &FeedbackCommand,
    config: &Configuration,
    operation: impl std::future::Future<Output = Delivery>,
    signal: impl std::future::Future<Output = io::Result<()>>,
    deadline: impl std::future::Future<Output = ()>,
    stderr: &mut impl Write,
) -> io::Result<Delivery> {
    // Capture correlation before polling identity or transport: failed output
    // must not invite replay after an unreported remote acceptance.
    write_value(
        command,
        &json!({
            "delivery": "pending",
            "environment": config.environment.name(),
            "app_id": config.environment.app_id(),
            "client_feedback_id": command.feedback_id,
            "message": "One submission attempt is starting. Retain this correlation ID. Lost output or interruption does not mean ingestion failed; do not automatically repeat.",
        }),
        stderr,
    )?;
    Ok(tokio::select! {
        biased;
        signal = signal => Delivery::new(command, config, "unknown", if signal.is_ok() {
            "Local waiting was cancelled; broker cleanup or remote ingestion may still complete. Retain this correlation ID and reconcile before submitting again."
        } else {
            "The cancellation signal handler failed. Local waiting stopped; the submission is unconfirmed. Reconcile this correlation ID before submitting again."
        }),
        () = deadline => Delivery::new(command, config, "unknown",
            "Feedback timed out. Broker cleanup or remote ingestion may still complete; reconcile this correlation ID before submitting again."),
        result = operation => result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_cli::feedback::Environment;

    fn input() -> (FeedbackCommand, Configuration) {
        (
            FeedbackCommand {
                title: "title".into(),
                message: "content".into(),
                feedback_id: Some("11111111-2222-4333-8444-555555555555".into()),
                send: true,
                output_format: None,
            },
            Configuration {
                environment: Environment::Ppe,
                feature_area: None,
            },
        )
    }

    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn preparation_generates_one_v4_id_or_preserves_override_without_entropy() {
        let (mut command, _) = input();
        command.feedback_id = None;
        let prepared = prepare(&command, || Ok([0; 16])).unwrap();
        assert_eq!(
            prepared.feedback_id.as_deref(),
            Some("00000000-0000-4000-8000-000000000000")
        );
        assert!(command.feedback_id.is_none());
        let prepared =
            prepare(&prepared, || panic!("resolved ID must not be regenerated")).unwrap();
        assert_eq!(
            prepared.feedback_id.as_deref(),
            Some("00000000-0000-4000-8000-000000000000")
        );
        let (explicit, _) = input();
        let preserved = prepare(&explicit, || panic!("explicit ID needs no randomness")).unwrap();
        assert_eq!(preserved.feedback_id, explicit.feedback_id);
        assert!(prepare(&command, || Err("Synthetic RNG failure")).is_err());
        command.title.clear();
        assert!(
            prepare(&command, || panic!(
                "invalid input must not consume entropy"
            ))
            .is_err()
        );
    }
    #[test]
    fn accepted_output_failure_is_an_error_and_uncertain_outcomes_stop_batches() {
        let (command, config) = input();
        let result = Delivery::new(&command, &config, "accepted", "accepted");
        assert!(write_delivery(&command, result, &mut Closed, &mut Vec::new()).is_err());
        let mut result = Delivery::new(&command, &config, "accepted", "accepted");
        result.http_status = Some(200);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert!(matches!(
            write_delivery(&command, result, &mut stdout, &mut stderr).unwrap(),
            CommandOutcome::Returned(0)
        ));
        let receipt: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(receipt["delivery"], "accepted");
        assert_eq!(receipt["http_status"], 200);
        assert!(stderr.is_empty());
        for delivery in ["unknown", "rejected", "not_sent"] {
            let result = Delivery::new(&command, &config, delivery, "not confirmed");
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            assert!(matches!(
                write_delivery(&command, result, &mut stdout, &mut stderr).unwrap(),
                CommandOutcome::Stop(1)
            ));
            assert!(stdout.is_empty());
            assert_eq!(
                serde_json::from_slice::<Value>(&stderr).unwrap()["delivery"],
                delivery
            );
        }
    }

    #[cfg(all(windows, feature = "native-host", feature = "windows-broker"))]
    #[tokio::test]
    async fn successful_attempt_preserves_pending_correlation_before_completion() {
        use std::future::{pending, ready};
        let (mut command, config) = input();
        command.feedback_id = None;
        let command = prepare(&command, || Ok([0; 16])).unwrap();
        let payload = command.manifest(&config, chrono::Utc::now()).unwrap();
        let result = Delivery::new(&command, &config, "accepted", "accepted");
        let mut stderr = Vec::new();
        let result = attempt(
            &command,
            &config,
            ready(result),
            pending(),
            pending(),
            &mut stderr,
        )
        .await
        .unwrap();
        assert!(result.accepted());
        assert_eq!(
            payload["clientFeedbackId"],
            serde_json::json!(result.client_feedback_id)
        );
        let record: Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(record["delivery"], "pending");
        assert_eq!(
            record["client_feedback_id"],
            serde_json::json!(command.feedback_id)
        );
    }

    #[cfg(all(windows, feature = "native-host", feature = "windows-broker"))]
    #[tokio::test]
    async fn pending_output_failure_prevents_operation_polling() {
        use std::{cell::Cell, future::pending};
        let (command, config) = input();
        let polled = Cell::new(false);
        let operation = async {
            polled.set(true);
            Delivery::new(&command, &config, "accepted", "accepted")
        };
        assert!(
            attempt(
                &command,
                &config,
                operation,
                pending(),
                pending(),
                &mut Closed
            )
            .await
            .is_err()
        );
        assert!(!polled.get());
    }

    #[cfg(all(windows, feature = "native-host", feature = "windows-broker"))]
    struct Dropped(std::sync::Arc<std::sync::atomic::AtomicBool>);

    #[cfg(all(windows, feature = "native-host", feature = "windows-broker"))]
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[cfg(all(windows, feature = "native-host", feature = "windows-broker"))]
    #[tokio::test]
    async fn cancellation_deadline_and_signal_errors_drop_work_and_retain_uncertainty() {
        use std::{
            future::{pending, poll_fn},
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
            task::Poll,
        };
        let (command, config) = input();
        for mode in 0..3 {
            let dropped = Arc::new(AtomicBool::new(false));
            let polled = AtomicBool::new(false);
            let guard = Dropped(dropped.clone());
            let operation = async {
                let _guard = guard;
                polled.store(true, Ordering::SeqCst);
                pending::<Delivery>().await
            };
            let signal = poll_fn(|_| {
                if polled.load(Ordering::SeqCst) && mode != 1 {
                    Poll::Ready(if mode == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::other("synthetic private signal error"))
                    })
                } else {
                    Poll::Pending
                }
            });
            let deadline = poll_fn(|cx| {
                cx.waker().wake_by_ref();
                if polled.load(Ordering::SeqCst) && mode == 1 {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            });
            let mut stderr = Vec::new();
            let result = attempt(&command, &config, operation, signal, deadline, &mut stderr)
                .await
                .unwrap();
            assert_eq!(result.delivery, "unknown");
            assert_eq!(result.client_feedback_id, command.feedback_id);
            assert!(dropped.load(Ordering::SeqCst));
            assert!(
                !serde_json::to_string(&result)
                    .unwrap()
                    .contains("synthetic private")
            );
            let pending: Value = serde_json::from_slice(&stderr).unwrap();
            assert_eq!(pending["delivery"], "pending");
            assert_eq!(
                pending["client_feedback_id"],
                serde_json::json!(command.feedback_id)
            );
        }
    }
}
