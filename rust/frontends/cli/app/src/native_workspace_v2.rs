// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private workspace preview over the shared native credential and transport composition.

use fabric_cli::workspace_v2::{
    OutputReservation, PreparedWorkspaceResult, WorkspaceCommand, WorkspaceEnvelope,
    failure_envelope, write_prepared_result, write_result,
};
use fabric_sdk_api::workspace_reads::WorkspaceReadError;
use std::{
    io::{self, Write},
    time::Instant,
};

pub(super) fn run(command: &WorkspaceCommand) -> io::Result<u8> {
    let (reservation, (deadline, result)) = match with_reservation(command, |started| {
        (
            started.checked_add(command.timeout()),
            run_reserved(command, started),
        )
    }) {
        Ok(prepared) => prepared,
        Err(error) => {
            let fallback = OutputReservation::new(None)
                .map_err(|_| io::Error::other("Workspace output could not be initialized."))?;
            return write_result(
                fallback,
                &failure_envelope(Some(command), error, None),
                &mut io::stdout().lock(),
                &mut io::stderr().lock(),
            );
        }
    };
    deliver(
        command,
        reservation,
        result,
        deadline,
        Instant::now,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

pub(super) fn with_reservation<T>(
    command: &WorkspaceCommand,
    run: impl FnOnce(Instant) -> T,
) -> Result<(OutputReservation, T), WorkspaceReadError> {
    let reservation = OutputReservation::new(command.output())?;
    // No selector, settings, navigation or credential access precedes reservation.
    let started = Instant::now();
    Ok((reservation, run(started)))
}

pub(super) fn deliver(
    command: &WorkspaceCommand,
    reservation: OutputReservation,
    result: Result<PreparedWorkspaceResult, Box<WorkspaceEnvelope>>,
    deadline: Option<Instant>,
    mut now: impl FnMut() -> Instant,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    let prepared = match result {
        Ok(prepared) => prepared,
        Err(envelope) => return write_result(reservation, &envelope, stdout, stderr),
    };
    if deadline.is_some_and(|deadline| now() >= deadline) {
        return write_result(
            reservation,
            &failure_envelope(Some(command), WorkspaceReadError::Timeout, None),
            stdout,
            stderr,
        );
    }
    let mut diagnostic = Vec::new();
    let result = write_prepared_result(reservation, &prepared, stdout, &mut diagnostic);
    if deadline.is_some_and(|deadline| now() >= deadline) {
        return write_late_diagnostic(command, &diagnostic, result.is_err(), stderr);
    }
    if !diagnostic.is_empty() {
        let written = stderr.write_all(&diagnostic).and_then(|()| stderr.flush());
        written?;
        if deadline.is_some_and(|deadline| now() >= deadline) {
            // A diagnostic was already delivered; appending another JSON
            // object would corrupt the single-envelope stderr contract.
            return Ok(1);
        }
    }
    result
}

fn write_late_diagnostic(
    command: &WorkspaceCommand,
    prior_diagnostic: &[u8],
    io_failed: bool,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    // Retain any primary/cleanup diagnostic and its known publication evidence.
    let mut failed = if prior_diagnostic.is_empty() {
        serde_json::to_value(failure_envelope(
            Some(command),
            WorkspaceReadError::Timeout,
            None,
        ))
    } else {
        serde_json::from_slice(prior_diagnostic)
    }
    .map_err(io::Error::other)?;
    let hints = failed
        .pointer_mut("/error/hints")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| io::Error::other("The workspace failure diagnostic is unavailable."))?;
    hints.push(serde_json::json!(
        "The deadline elapsed during blocking output. Output may already have been delivered; delivery is uncertain and no rollback is implied."
    ));
    if io_failed {
        hints.push(serde_json::json!(
            "Output I/O also reported a failure; previously delivered bytes or file publication are not undone."
        ));
    }
    serde_json::to_writer(&mut *stderr, &failed).map_err(io::Error::other)?;
    stderr.write_all(b"\n")?;
    stderr.flush()?;
    Ok(1)
}

#[cfg(not(any(windows, feature = "headless")))]
pub(super) fn run_reserved(
    command: &WorkspaceCommand,
    _: Instant,
) -> Result<PreparedWorkspaceResult, Box<WorkspaceEnvelope>> {
    Err(Box::new(failure_envelope(
        Some(command),
        WorkspaceReadError::UnsupportedCapability,
        None,
    )))
}

#[cfg(any(windows, feature = "headless"))]
fn run_reserved(
    command: &WorkspaceCommand,
    started: Instant,
) -> Result<PreparedWorkspaceResult, Box<WorkspaceEnvelope>> {
    use std::time::Duration;

    let fail = |error| Box::new(failure_envelope(Some(command), error, None));
    let timeout = command.timeout();
    let Some(deadline) = started.checked_add(timeout) else {
        return Err(fail(WorkspaceReadError::InvalidInput));
    };
    if timeout.is_zero() || timeout > Duration::from_secs(24 * 60 * 60) {
        return Err(fail(WorkspaceReadError::InvalidInput));
    }
    let endpoint = match workspace_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC")) {
        Ok(endpoint) => endpoint,
        Err(error) => return Err(fail(error)),
    };
    let timestamp = match crate::local_host::timestamp() {
        Ok(timestamp) => timestamp,
        Err(_) => return Err(fail(WorkspaceReadError::OperationFailed)),
    };
    let observed_at = timestamp.as_str();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return Err(fail(WorkspaceReadError::OperationFailed)),
    };
    let result = runtime.block_on(controlled(
        async {
            let (credential, client, identity_client) = tokio::task::spawn_blocking(move || {
                let credential = super::selected_credential()?;
                let (client, identity_client) = {
                    let mut transport = super::native_transport_options(false);
                    transport.connect_timeout = timeout;
                    transport.read_timeout = timeout;
                    transport.write_timeout = timeout;
                    super::native_clients_with_options(&credential, transport)?
                };
                Ok::<_, super::Failure>((credential, client, identity_client))
            })
            .await
            .map_err(|_| WorkspaceReadError::OperationFailed)?
            .map_err(native_error)?;
            let result = execute_injected(
                command,
                (credential, (client, identity_client)),
                endpoint,
                observed_at,
            )
            .await?;
            // Render off the runtime thread, then repoll interruption before publication.
            tokio::task::spawn_blocking(move || PreparedWorkspaceResult::new(result))
                .await
                .map_err(|_| WorkspaceReadError::OutputFailed)?
        },
        tokio::signal::ctrl_c(),
        async {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
        },
    ));
    // Detached blocking setup/rendering may finish locally, but cannot publish a result.
    runtime.shutdown_background();
    result.map_err(|error| Box::new(failure_envelope(Some(command), error, Some(observed_at))))
}

#[cfg(any(windows, feature = "headless"))]
pub(super) async fn execute_injected(
    command: &WorkspaceCommand,
    (credential, (client, identity_client)): (super::NativeCredential, super::NativeClients),
    endpoint: super::EndpointBinding,
    observed_at: &str,
) -> Result<WorkspaceEnvelope, WorkspaceReadError> {
    if endpoint != super::EndpointBinding::public(super::ServiceOrigin::Fabric) {
        return Err(WorkspaceReadError::UnsupportedCapability);
    }
    let (sdk, executor) = super::connect_native(
        credential,
        client,
        identity_client,
        endpoint,
        fabric_sdk_http::ExecutionOptions {
            timeout: command.timeout(),
            max_delay: command.timeout(),
        },
        None,
    )
    .await
    .map_err(native_error)?;
    execute_authenticated(command, sdk, executor, observed_at).await
}

#[cfg(any(windows, feature = "headless"))]
pub(super) async fn execute_authenticated(
    command: &WorkspaceCommand,
    sdk: fabric_sdk::Client,
    executor: fabric_sdk_http::HttpExecutor,
    observed_at: &str,
) -> Result<WorkspaceEnvelope, WorkspaceReadError> {
    use std::sync::Arc;

    let public = super::EndpointBinding::public(super::ServiceOrigin::Fabric);
    if sdk.context().endpoint() != &public || executor.endpoint() != &public {
        return Err(WorkspaceReadError::UnsupportedCapability);
    }
    let api = fabric_sdk_api::ApiClient::new(
        Arc::clone(sdk.credential()),
        sdk.selection().clone(),
        Arc::new(fabric_cli_resources::ResourceHttpExecutor(Arc::new(
            executor.clone(),
        ))),
    );
    let warehouse = fabric_cli::warehouse_reads::WarehouseReadHost {
        client: &sdk,
        operations: &executor,
    };
    Ok(fabric_cli::workspace_v2::execute_with_warehouse(
        &api,
        Some(&warehouse),
        command,
        observed_at,
        std::future::pending(),
        std::future::pending(),
    )
    .await)
}

#[cfg(any(windows, feature = "headless"))]
pub(super) fn workspace_endpoint(
    value: Option<std::ffi::OsString>,
) -> Result<super::EndpointBinding, WorkspaceReadError> {
    let endpoint = super::selected_endpoint(value).map_err(native_error)?;
    if endpoint != super::EndpointBinding::public(super::ServiceOrigin::Fabric) {
        return Err(WorkspaceReadError::UnsupportedCapability);
    }
    Ok(endpoint)
}

#[cfg(any(windows, feature = "headless"))]
pub(super) async fn controlled<T>(
    work: impl std::future::Future<Output = Result<T, WorkspaceReadError>>,
    cancelled: impl std::future::Future<Output = io::Result<()>>,
    deadline: impl std::future::Future<Output = ()>,
) -> Result<T, WorkspaceReadError> {
    let mut cancelled = std::pin::pin!(cancelled);
    let mut deadline = std::pin::pin!(deadline);
    let signal_error = |signal: io::Result<()>| match signal {
        Ok(()) => WorkspaceReadError::Cancelled,
        Err(_) => WorkspaceReadError::OperationFailed,
    };
    let result = tokio::select! {
        biased;
        signal = cancelled.as_mut() => return Err(signal_error(signal)),
        () = deadline.as_mut() => return Err(WorkspaceReadError::Timeout),
        result = work => result,
    };
    tokio::select! {
        biased;
        signal = cancelled => Err(signal_error(signal)),
        () = deadline => Err(WorkspaceReadError::Timeout),
        result = std::future::ready(result) => result,
    }
}

#[cfg(any(windows, feature = "headless"))]
pub(super) fn native_error((code, _, _): super::Failure) -> WorkspaceReadError {
    match code {
        "invalid_configuration" | "invalid_arguments" => WorkspaceReadError::InvalidInput,
        "NotConfigured" => WorkspaceReadError::NotConfigured,
        "NotSupported" | "unsupported" => WorkspaceReadError::UnsupportedCapability,
        "cancelled" => WorkspaceReadError::Cancelled,
        "timed_out" => WorkspaceReadError::Timeout,
        "interaction_required"
        | "identity_mismatch"
        | "invalid_broker_response"
        | "broker_unavailable"
        | "interactive_window_unavailable"
        | "completion_unconfirmed"
        | "identity_capacity_exhausted"
        | "broker_capacity_exhausted"
        | "authentication_busy"
        | "session_invalidated"
        | "cleanup_pending"
        | "coordination_failed"
        | "authentication_failed" => WorkspaceReadError::Unauthenticated,
        _ => WorkspaceReadError::OperationFailed,
    }
}
