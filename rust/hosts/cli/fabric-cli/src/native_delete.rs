// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{batch_state::CommandOutcome, delete_host};
use fabric_cli::delete::{PreparedDelete, ReservedDeleteOutput};
use std::io;

#[cfg(not(any(windows, feature = "headless")))]
pub(super) fn run(
    prepared: PreparedDelete,
    _output: ReservedDeleteOutput,
    _deadline: std::time::Instant,
) -> io::Result<CommandOutcome> {
    delete_host::host_failure(
        &prepared,
        "UnsupportedCapability",
        "Native deletion is unavailable on this host.",
    )
}

#[cfg(any(windows, feature = "headless"))]
pub(super) fn run(
    prepared: PreparedDelete,
    output: ReservedDeleteOutput,
    deadline: std::time::Instant,
) -> io::Result<CommandOutcome> {
    use std::io::IsTerminal;

    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return delete_host::host_failure(
            &prepared,
            "ApprovalRequired",
            "Native permanent deletion requires interactive approval; no unattended bypass is available.",
        );
    }
    let endpoint = match super::selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC")) {
        Ok(endpoint) => endpoint,
        Err((code, message, _)) => {
            return delete_host::host_failure(&prepared, code, &message);
        }
    };
    let credential = match super::selected_credential() {
        Ok(credential) => credential,
        Err((code, message, _)) => {
            return delete_host::host_failure(&prepared, code, &message);
        }
    };
    let selection = match fabric_sdk::CredentialSelection::new(
        credential.account(),
        credential.tenant(),
        fabric_sdk::Audience::Fabric,
    ) {
        Ok(selection) => selection,
        Err(_) => {
            return delete_host::host_failure(
                &prepared,
                "InvalidInput",
                "The deletion credential context is invalid.",
            );
        }
    };
    let prepared = match prepared.with_selection(&selection, &endpoint) {
        Ok(prepared) => prepared,
        Err(error) => {
            return delete_host::write_failure(&error.envelope(None), error.exit_code());
        }
    };
    if std::time::Instant::now() >= deadline {
        return delete_host::host_failure(
            &prepared,
            "Timeout",
            "The total deletion deadline elapsed before approval.",
        );
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            return delete_host::host_failure(
                &prepared,
                "RuntimeFailed",
                "The deletion runtime could not start.",
            );
        }
    };
    let result = runtime.block_on(execute(&prepared, output, credential, endpoint, deadline));
    let result = super::finish_runtime(runtime, result);
    match result {
        Ok(dispatch) if dispatch.result.is_err() => {
            delete_host::write_failure(dispatch.envelope(), dispatch.exit_code())
        }
        Ok(dispatch) => {
            match fabric_cli::delete::write_dispatch(dispatch, &mut io::stdout().lock()) {
                Ok(dispatch) => Ok(CommandOutcome::Returned(dispatch.exit_code())),
                Err(error) => delete_host::write_failure(&error.envelope(), error.exit_code()),
            }
        }
        Err((code, message)) => delete_host::host_failure(&prepared, code, &message),
    }
}

#[cfg(any(windows, feature = "headless"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Interruption {
    Timeout,
    Cancelled,
    SignalFailed,
}

#[cfg(any(windows, feature = "headless"))]
impl Interruption {
    fn failure(self) -> (&'static str, String) {
        match self {
            Self::Timeout => (
                "Timeout",
                "The total deletion deadline elapsed; no rollback or replay is implied.".into(),
            ),
            Self::Cancelled => (
                "Cancelled",
                "Local deletion waiting stopped; no rollback or replay is implied.".into(),
            ),
            Self::SignalFailed => (
                "SignalFailed",
                "The deletion interruption handler could not start.".into(),
            ),
        }
    }
}

#[cfg(any(windows, feature = "headless"))]
async fn execute(
    prepared: &PreparedDelete,
    output: ReservedDeleteOutput,
    credential: super::NativeCredential,
    endpoint: fabric_sdk::EndpointBinding,
    deadline: std::time::Instant,
) -> Result<fabric_cli::delete::DeleteDispatch, (&'static str, String)> {
    use fabric_cli_resources::Confirmation;
    use fabric_sdk::{Audience, Client, CredentialSelection, RequestContext};
    use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
    use fabric_sdk_operations::{OperationLimits, OperationSession};
    use fabric_sdk_transport::{
        NativeHttpClient, ProxyPolicy, TokioTimer, TransportOptions, TrustPolicy,
    };
    use std::{num::NonZeroUsize, sync::Arc, time::Duration};

    let mut stop = std::pin::pin!(async {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Interruption::Timeout,
            result = tokio::signal::ctrl_c() => match result {
                Ok(()) => Interruption::Cancelled,
                Err(_) => Interruption::SignalFailed,
            },
        }
    });
    let selection =
        CredentialSelection::new(credential.account(), credential.tenant(), Audience::Fabric)
            .map_err(|_| {
                (
                    "InvalidInput",
                    "The deletion credential context is invalid.".into(),
                )
            })?;
    let question = format!(
        "Permanently delete Notebook {} in workspace {}?\nContext: {}\nEffects: owning Notebook lifecycle and content only. No immediate physical-erasure guarantee.",
        prepared.assessment().target().item_id(),
        prepared.assessment().target().workspace_id(),
        serde_json::json!({
            "tenant": selection.tenant(),
            "account": selection.account(),
            "endpoint": endpoint.host(),
        }),
    );
    let mut confirmation = super::terminal_ui::NativeAccessConfirmation::with_reader(|question| {
        fabric_cli::console::read_explicit_confirmation(
            &mut io::stdin().lock(),
            &mut io::stderr(),
            question,
        )
    });
    let approved = tokio::select! {
        biased;
        reason = stop.as_mut() => return Err(reason.failure()),
        result = confirmation.confirm(&question) => result.map_err(|_| (
            "ApprovalFailed", "Explicit deletion approval could not be obtained.".into()
        ))?,
    };
    if !approved {
        return Err((
            "ApprovalRequired",
            "Permanent deletion was not approved; no mutation was submitted.".into(),
        ));
    }
    let approval = fabric_sdk::items::delete::DeleteApproval::approve(prepared.assessment());
    let options = TransportOptions {
        connect_timeout: prepared.timeout(),
        read_timeout: prepared.timeout(),
        write_timeout: prepared.timeout(),
        pool_idle_timeout: Duration::from_secs(60),
        max_idle_per_host: NonZeroUsize::new(1).expect("nonzero constant"),
        trust: TrustPolicy::Platform {
            additional_roots: Vec::new(),
        },
        proxy: ProxyPolicy::None,
    };
    let identity_http = credential
        .metadata_endpoint()
        .map(|endpoint| {
            NativeHttpClient::for_managed_identity(options.clone(), endpoint)
                .map(|client| Arc::new(client) as Arc<dyn azure_core::http::HttpClient>)
        })
        .transpose()
        .map_err(|_| {
            (
                "InvalidInput",
                "The identity transport binding is invalid.".into(),
            )
        })?;
    let client = Arc::new(NativeHttpClient::new(options).map_err(|_| {
        (
            "TransportFailed",
            "The deletion transport could not start.".into(),
        )
    })?);
    let credential = tokio::select! {
        biased;
        reason = stop.as_mut() => return Err(reason.failure()),
        result = credential.connect(identity_http.unwrap_or_else(|| client.clone())) =>
            result.map_err(|(code, message, _)| (code, message))?,
    };
    let sdk = Client::with_context(RequestContext::with_endpoint(
        credential,
        selection,
        endpoint.clone(),
    ));
    let http = HttpExecutor::new(client, Arc::new(TokioTimer), ExecutionOptions::default())
        .map_err(|_| {
            (
                "TransportFailed",
                "The deletion HTTP executor could not start.".into(),
            )
        })?
        .with_endpoint(endpoint);
    let reason = std::sync::atomic::AtomicU8::new(0);
    let cancelled = async {
        let interruption = stop.as_mut().await;
        reason.store(
            match interruption {
                Interruption::Timeout => 1,
                Interruption::Cancelled => 2,
                Interruption::SignalFailed => 3,
            },
            std::sync::atomic::Ordering::SeqCst,
        );
    };
    let mut operations = OperationSession::with_context(
        sdk.context(),
        http.clone(),
        OperationLimits::default(),
        cancelled,
    );
    let mut operations = InterruptibleOperations {
        inner: &mut operations,
        reason: &reason,
    };
    fabric_cli::delete::dispatch(&mut operations, prepared, Some(&approval), output)
        .await
        .map_err(|error| {
            (
                "DeleteDispatchFailed",
                format!("{}: {}", error.code(), error.message()),
            )
        })
}

#[cfg(any(windows, feature = "headless"))]
struct InterruptibleOperations<'a> {
    inner: &'a mut dyn fabric_sdk_workspaces::mutations::ResourceOperations,
    reason: &'a std::sync::atomic::AtomicU8,
}

#[cfg(any(windows, feature = "headless"))]
impl fabric_sdk_workspaces::mutations::ResourceOperations for InterruptibleOperations<'_> {
    fn execute(
        &mut self,
        operation: fabric_sdk_workspaces::mutations::ResourceOperation,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        fabric_sdk_workspaces::mutations::ResourceOutcome,
                        fabric_sdk_workspaces::mutations::ResourceError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            self.inner.execute(operation).await.map_err(|mut error| {
                use std::sync::atomic::Ordering;
                let interruption = match self.reason.load(Ordering::SeqCst) {
                    1 => Some(Interruption::Timeout),
                    2 => Some(Interruption::Cancelled),
                    3 => Some(Interruption::SignalFailed),
                    _ => None,
                };
                if let Some(reason) = interruption {
                    let (code, message) = reason.failure();
                    error.code = code.into();
                    error.message = message;
                }
                error
            })
        })
    }
}
