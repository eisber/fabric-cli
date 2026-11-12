// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared executable connector, not a Notebook content/session command engine.

use super::{Failure, NativeSession, capture_fabric_selection, failed};
use fabric_cli::runtime_v2::{
    ParsedInvocation, ReadOptions,
    host::{Destination, SetupError, prepare_destination, setup},
    notebook::{BINDING_VARIABLE, NotebookBinding, NotebookConnection},
};
use fabric_sdk::{Audience, CredentialSelection};
use std::{future::Future, path::PathBuf, time::Instant};

/// Exact local preflight and native connector; no auth/network in blocking setup.
pub(crate) async fn prepare(
    input: &ParsedInvocation,
    options: ReadOptions,
    deadline: Instant,
    cancelled: impl Future<Output = ()>,
) -> Result<(NotebookConnection, Destination), Failure> {
    let mut cancelled = std::pin::pin!(cancelled);
    input
        .validate(options)
        .map_err(|error| failed("InvalidRuntimeArguments", error.to_string(), 2))?;
    let path = input.path.clone();
    let (binding, cwd) = setup(deadline, cancelled.as_mut(), move || {
        let binding_path = std::env::var_os(BINDING_VARIABLE).ok_or_else(|| {
            failed("NotebookBindingRequired", "Select an explicit non-secret FAB_RUST_NOTEBOOK_BINDING file; no binding discovery is performed.", 1)
        })?;
        let binding = NotebookBinding::read(&PathBuf::from(binding_path), &path).map_err(|_| {
            failed("InvalidNotebookBinding", "The explicit Notebook binding is invalid or does not match the typed owner.", 2)
        })?;
        let cwd = std::env::current_dir().map_err(|_| {
            failed("InvalidWorkingDirectory", "The output working directory is unavailable.", 2)
        })?;
        Ok((binding, cwd))
    }).await.map_err(setup_failure)?;
    let destination = prepare_destination(input.output.clone(), cwd, deadline, cancelled.as_mut())
        .await
        .map_err(host_failure)?;
    let connection = setup(deadline, cancelled.as_mut(), move || {
        let (selection, endpoint) = capture_fabric_selection()?;
        let owner =
            CredentialSelection::new(&selection.account, &selection.tenant, Audience::Fabric)
                .map_err(|_| {
                    failed(
                        "InvalidNotebookIdentity",
                        "Invalid explicit Notebook identity.",
                        2,
                    )
                })?;
        binding.check_identity(&owner, &endpoint).map_err(|_| {
            failed(
                "NotebookIdentityMismatch",
                "The binding does not match the selected account, tenant and Fabric catalog.",
                2,
            )
        })?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                failed(
                    "TimedOut",
                    "The original Notebook read deadline elapsed.",
                    1,
                )
            })?;
        let session = NativeSession::with_provider_policy(selection, endpoint, remaining, true)?;
        binding
            .connect(
                session.context,
                session.executor.as_ref().clone(),
                options,
                deadline,
            )
            .map_err(|_| {
                failed(
                    "InvalidNotebookConnection",
                    "The explicit Notebook context or original deadline is invalid.",
                    2,
                )
            })
    })
    .await
    .map_err(setup_failure)?;
    Ok((connection, destination))
}

fn setup_failure(error: SetupError<Failure>) -> Failure {
    match error {
        SetupError::Work(error) => error,
        SetupError::Host(error) => host_failure(error),
    }
}

pub(crate) fn host_failure(error: fabric_cli::runtime_v2::host::HostError) -> Failure {
    use fabric_cli::runtime_v2::ReadErrorKind;
    let code = match error.kind {
        ReadErrorKind::Cancelled => "Cancelled",
        ReadErrorKind::Timeout => "TimedOut",
        ReadErrorKind::OutputUnavailable => "OutputUnavailable",
        ReadErrorKind::Delivery => "OutputDeliveryFailed",
        _ => "RuntimeReadFailed",
    };
    (
        code,
        error.to_string(),
        serde_json::json!({
            "httpStatus":error.http_status,
            "readCompleted":error.read_completed,
            "fileCreated":error.file_created,
            "bytesWrittenAtObservation":error.bytes_written,
            "remoteCancelled":false,"remoteReplayed":false,
        }),
        1,
    )
}
