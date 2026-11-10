// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Experimental explicit resource execution head; the existing fab-rs binary is unchanged.

use fabric_cli::{
    get_backend::SdkGetBackend,
    get_command::{
        GetArgumentErrorKind, GetCommandError, GetInterruption, PreparedGet, get_command, parse_get,
    },
    get_target::GetTarget,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_onelake::OneLakeClient;
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_transport::{
    NativeHttpClient, ProxyPolicy, TokioTimer, TransportOptions, TrustPolicy,
};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(all(windows, feature = "windows-broker"))]
#[allow(
    dead_code,
    reason = "The v2 read host reuses only silent saved selection; it never logs in or writes bindings."
)]
mod auth_binding;
#[path = "headless.rs"]
mod headless;
#[cfg(all(windows, feature = "windows-broker"))]
#[allow(
    dead_code,
    reason = "The v2 read host reuses only the existing non-secret configuration root."
)]
mod runtime_config;
mod v2_credentials;
mod v2_exists_main;
#[cfg(feature = "v2-refresh")]
mod v2_refresh_main;
mod v2_search_main;

// Reuse the existing binary-private selector without importing v1 dispatch/state.
mod native_read {
    pub(crate) type Failure = (&'static str, String, u8);

    pub(crate) fn failure(code: &'static str, message: impl Into<String>, exit: u8) -> Failure {
        (code, message.into(), exit)
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().and_then(|value| value.to_str()) == Some("exists") {
        return std::process::ExitCode::from(finish(
            v2_exists_main::run(args),
            &mut io::stderr().lock(),
        ));
    }
    #[cfg(feature = "v2-refresh")]
    {
        if args.first().and_then(|value| value.to_str()) == Some("refresh") {
            return std::process::ExitCode::from(finish(
                v2_refresh_main::run(args),
                &mut io::stderr().lock(),
            ));
        }
    }
    std::process::ExitCode::from(finish(run().map(|()| 0), &mut io::stderr().lock()))
}

fn finish(result: Result<u8, Failure>, stderr: &mut dyn Write) -> u8 {
    match result {
        Ok(exit) => exit,
        Err((code, message, evidence, exit)) => {
            let result = serde_json::to_writer(
                stderr,
                &json!({"error":{"code":code,"message":message},"evidence":evidence}),
            );
            if result.is_err() {
                return 1;
            }
            exit
        }
    }
}

type Failure = (&'static str, String, Value, u8);

fn failed(code: &'static str, message: impl Into<String>, exit: u8) -> Failure {
    (code, message.into(), Value::Null, exit)
}

fn command_failure(error: GetCommandError) -> Failure {
    let code = match &error {
        GetCommandError::Interrupted(GetInterruption::Cancelled) => "Cancelled",
        GetCommandError::Interrupted(GetInterruption::DeadlineExceeded) => "TimedOut",
        GetCommandError::Incomplete { .. } => "Incomplete",
        _ => "GetFailed",
    };
    let evidence = match &error {
        GetCommandError::Incomplete { data } => data.clone(),
        GetCommandError::Backend { source, .. } => {
            if let Some(observation) = source.downcast_ref::<fabric_sdk::observe::ObserveError>() {
                let handle = match &observation.handle {
                    fabric_sdk::observe::ReadHandle::Operation { operation_id } => {
                        format!("/operations/{operation_id}")
                    }
                    fabric_sdk::observe::ReadHandle::Job {
                        workspace_id,
                        item_id,
                        job_id,
                    } => format!("/workspaces/{workspace_id}/items/{item_id}/runs/{job_id}"),
                };
                json!({
                    "handle":handle,
                    "requests":observation.requests,
                    "lastStatus":observation.last_observation.as_ref().map(|value| &value.raw_status),
                    "remoteCancelled":false,
                    "remoteReplayed":false,
                })
            } else {
                match source.downcast_ref::<fabric_sdk::dataagents::DataAgentError>() {
                    Some(fabric_sdk::dataagents::DataAgentError::Operation(error)) => {
                        dataagent_operation_evidence(error, None)
                    }
                    Some(fabric_sdk::dataagents::DataAgentError::Response(error)) => {
                        dataagent_operation_evidence(&error.error, Some(error.response.status))
                    }
                    _ => Value::Null,
                }
            }
        }
        GetCommandError::File(error) => json!({
            "pathCreated":error.path_created,
            "bytesWritten":error.bytes_written,
            "rollback":false
        }),
        _ => Value::Null,
    };
    (code, error.to_string(), evidence, 1)
}

fn dataagent_operation_evidence(
    error: &fabric_sdk_operations::OperationError,
    observed_status: Option<u16>,
) -> Value {
    use fabric_sdk_operations::OperationErrorKind;
    let (classification, status) = match &error.kind {
        OperationErrorKind::Http { status, .. } => ("http", Some(*status)),
        OperationErrorKind::Cancelled => ("cancelled", observed_status),
        OperationErrorKind::Limit => ("limit", observed_status),
        OperationErrorKind::Credential => ("credential", observed_status),
        OperationErrorKind::Execution(_) => ("execution", observed_status),
        OperationErrorKind::InvalidAudience => ("invalid-audience", observed_status),
        OperationErrorKind::InvalidInput => ("invalid-input", observed_status),
        OperationErrorKind::InvalidResponse => ("invalid-response", observed_status),
        OperationErrorKind::InvalidLink => ("invalid-link", observed_status),
        OperationErrorKind::OperationFailed { .. } => ("operation-failed", observed_status),
    };
    json!({
        "classification":classification,
        "httpStatus":status,
        "operationId":error.operation_id,
        "requestMayHaveReachedService":error.mutation_may_have_occurred,
        "remoteCancelled":false,
        "remoteReplayed":false
    })
}

fn run() -> Result<(), Failure> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.as_slice() == [std::ffi::OsStr::new("--version")] {
        writeln!(
            io::stdout().lock(),
            "fab-cli-v2 {}",
            env!("CARGO_PKG_VERSION")
        )
        .map_err(|_| failed("OutputFailed", "Version output failed.", 1))?;
        return Ok(());
    }
    if args.as_slice() == [std::ffi::OsStr::new("--help")] {
        writeln!(io::stdout().lock(), "fab-cli-v2: experimental explicit resource head\n\nUsage: fab-cli-v2 get <path> [OPTIONS]\n       fab-cli-v2 search <scope> [OPTIONS]\n       fab-cli-v2 exists <path> [OPTIONS]\n       fab-cli-v2 update <path> --input FILE|- --mode patch --plan\n\nRun a command with --help for its supported schema.\nUpdate plans are local; mutation execution requires a caller-injected approval host.\nReads require explicit FAB_RUST_AUTH_MODE; no identity fallback or interactive sign-in.")
            .map_err(|_| failed("OutputFailed", "Help output failed.", 1))?;
        return Ok(());
    }
    if args.first().and_then(|value| value.to_str()) == Some("search") {
        return v2_search_main::run(args);
    }
    if args
        .first()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value == "update" || value == "artifact")
        && !(args.get(1).and_then(|value| value.to_str()) == Some("data-agent")
            && args.get(2).and_then(|value| value.to_str()) == Some("get"))
    {
        return run_update(args);
    }
    let command = match parse_get(args) {
        Ok(command) => command,
        Err(error)
            if matches!(
                error.kind,
                GetArgumentErrorKind::Syntax(clap::error::ErrorKind::DisplayHelp)
            ) =>
        {
            get_command()
                .write_long_help(&mut io::stdout().lock())
                .map_err(|_| failed("OutputFailed", "Help output failed.", 1))?;
            return Ok(());
        }

        Err(error) => return Err(failed("InvalidArguments", error.to_string(), 2)),
    };
    GetTarget::parse(&command.path)
        .map_err(|error| failed("InvalidTarget", error.to_string(), 2))?;
    let started = Instant::now();
    let timeout = command.options.limits.timeout;
    let addressed_path = command.path.clone();
    let cwd = std::env::current_dir().map_err(|_| {
        failed(
            "InvalidWorkingDirectory",
            "The output working directory is unavailable.",
            2,
        )
    })?;
    let prepared = PreparedGet::preflight(command, &cwd).map_err(command_failure)?;
    let selection = v2_credentials::Selection::capture()?;
    let endpoint = match std::env::var("FAB_RUST_FABRIC_HOST") {
        Ok(host) => EndpointBinding::with_host(ServiceOrigin::Fabric, &host).map_err(|_| {
            failed(
                "InvalidConfiguration",
                "Invalid explicit Fabric endpoint.",
                2,
            )
        })?,
        Err(std::env::VarError::NotPresent) => EndpointBinding::public(ServiceOrigin::Fabric),
        Err(_) => {
            return Err(failed(
                "InvalidConfiguration",
                "Invalid explicit Fabric endpoint.",
                2,
            ));
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|_| failed("RuntimeFailed", "The get runtime could not be created.", 1))?;
    let result = runtime.block_on(async {
        let remaining = timeout
            .checked_sub(started.elapsed())
            .filter(|value| !value.is_zero())
            .ok_or_else(|| failed("TimedOut", "GET deadline elapsed before setup.", 1))?;
        let session = NativeSession::new(selection, endpoint, remaining).await?;
        let executor = session.executor;
        let storage_client = OneLakeClient::new(
            session.context.credential().clone(),
            session.storage_selection,
        );
        let client = Client::with_context(session.context);
        let storage_executor = OneLakeHttpExecutor::new(executor.clone());
        let backend = SdkGetBackend {
            client: &client,
            items: executor.as_ref(),
            workspaces: executor.as_ref(),
            operations: executor.as_ref(),
            storage: Some((&storage_client, &storage_executor)),
            documents: None,
        };
        let deadline = timeout.saturating_sub(started.elapsed());
        let interruption = async {
            tokio::select! {
                _ = tokio::time::sleep(deadline) => GetInterruption::DeadlineExceeded,
                _ = tokio::signal::ctrl_c() => GetInterruption::Cancelled,
            }
        };
        prepared
            .acquire(&backend, interruption)
            .await
            .map_err(command_failure)
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    let delivery = result.map_err(|(code, message, evidence, exit)| {
        if matches!(code, "Cancelled" | "TimedOut") {
            (
                code,
                message,
                json!({
                    "target":addressed_path,
                    "lastState":"unknown",
                    "detail":evidence,
                    "remoteCancelled":false,
                    "remoteReplayed":false
                }),
                exit,
            )
        } else {
            (code, message, evidence, exit)
        }
    })?;
    if started.elapsed() >= timeout {
        return Err(failed(
            "TimedOut",
            "The total get deadline elapsed; no delivery was started.",
            1,
        ));
    }
    if delivery.output_path().is_some() {
        delivery.commit_file().map_err(command_failure)?;
    } else {
        delivery
            .write_to(&mut io::stdout().lock())
            .map_err(command_failure)?;
    }
    Ok(())
}

struct NativeSession {
    context: RequestContext,
    storage_selection: CredentialSelection,
    executor: Arc<HttpExecutor>,
    service_http: Arc<dyn azure_core::http::HttpClient>,
}

impl NativeSession {
    async fn new(
        selection: v2_credentials::Selection,
        endpoint: EndpointBinding,
        timeout: Duration,
    ) -> Result<Self, Failure> {
        let transport = transport_options(timeout);
        let identity_http: Arc<dyn azure_core::http::HttpClient> = match selection
            .metadata_endpoint()
        {
            Some(url) => Arc::new(
                NativeHttpClient::for_managed_identity(transport.clone(), url).map_err(|_| {
                    failed(
                        "InvalidConfiguration",
                        "Managed identity transport configuration failed.",
                        2,
                    )
                })?,
            ),
            None => Arc::new(NativeHttpClient::new(transport.clone()).map_err(|_| {
                failed(
                    "InvalidConfiguration",
                    "Native transport configuration failed.",
                    2,
                )
            })?),
        };
        let fabric_selection =
            CredentialSelection::new(selection.account(), selection.tenant(), Audience::Fabric)
                .map_err(|_| {
                    failed(
                        "InvalidConfiguration",
                        "Invalid explicit identity selection.",
                        2,
                    )
                })?;
        let storage_selection =
            CredentialSelection::new(selection.account(), selection.tenant(), Audience::OneLake)
                .map_err(|_| {
                    failed(
                        "InvalidConfiguration",
                        "Invalid explicit storage identity selection.",
                        2,
                    )
                })?;
        let credential = tokio::select! {
            result = selection.credential(identity_http) => result?,
            () = tokio::time::sleep(timeout) => return Err(failed(
                "TimedOut", "Read setup deadline elapsed; broker cleanup may still be pending.", 1
            )),
            _ = tokio::signal::ctrl_c() => return Err(failed(
                "Cancelled", "Read setup cancelled; broker cleanup may still be pending.", 1
            )),
        };
        let service_http: Arc<dyn azure_core::http::HttpClient> =
            Arc::new(NativeHttpClient::new(transport).map_err(|_| {
                failed(
                    "InvalidConfiguration",
                    "Service transport configuration failed.",
                    2,
                )
            })?);
        let executor = Arc::new(
            HttpExecutor::new(
                service_http.clone(),
                Arc::new(TokioTimer),
                ExecutionOptions {
                    timeout,
                    max_delay: Duration::from_secs(60),
                },
            )
            .map_err(|_| failed("InvalidConfiguration", "Invalid read execution limits.", 2))?
            .with_endpoint(endpoint.clone()),
        );
        Ok(Self {
            context: RequestContext::with_endpoint(credential, fabric_selection, endpoint),
            storage_selection,
            executor,
            service_http,
        })
    }

    fn executor_for(
        &self,
        endpoint: EndpointBinding,
        timeout: Duration,
    ) -> Result<Arc<HttpExecutor>, Failure> {
        Ok(Arc::new(
            HttpExecutor::new(
                self.service_http.clone(),
                Arc::new(TokioTimer),
                ExecutionOptions {
                    timeout,
                    max_delay: Duration::from_secs(60),
                },
            )
            .map_err(|_| {
                failed(
                    "InvalidConfiguration",
                    "Invalid explicit service limits.",
                    2,
                )
            })?
            .with_endpoint(endpoint),
        ))
    }
}

fn transport_options(timeout: Duration) -> TransportOptions {
    TransportOptions {
        connect_timeout: timeout.min(Duration::from_secs(30)),
        read_timeout: timeout,
        write_timeout: timeout,
        pool_idle_timeout: Duration::from_secs(60),
        max_idle_per_host: NonZeroUsize::new(2).unwrap(),
        trust: TrustPolicy::Platform {
            additional_roots: Vec::new(),
        },
        proxy: ProxyPolicy::None,
    }
}

fn run_update(args: Vec<std::ffi::OsString>) -> Result<(), Failure> {
    use fabric_cli::update::{self, ArgumentError, PreparedUpdateCommand};
    let command =
        match update::parse(std::iter::once(std::ffi::OsString::from("fab-cli-v2")).chain(args)) {
            Ok(command) => command,
            Err(ArgumentError::Syntax(clap::error::ErrorKind::DisplayHelp)) => {
                update::command()
                    .write_long_help(&mut io::stdout().lock())
                    .map_err(|_| failed("OutputFailed", "Update help output failed.", 1))?;
                return Ok(());
            }
            Err(error) => return Err(failed("InvalidArguments", error.to_string(), 2)),
        };
    if !command.plan {
        return Err(failed(
            "ApprovalHostRequired",
            "Metadata mutation requires a caller-injected approval host; no automatic approval or credential fallback is installed.",
            1,
        ));
    }
    let cwd = std::env::current_dir().map_err(|_| {
        failed(
            "InvalidWorkingDirectory",
            "Update working directory is unavailable.",
            2,
        )
    })?;
    let prepared = PreparedUpdateCommand::preflight(command, &cwd, &mut io::stdin().lock())
        .map_err(|error| failed("UpdatePreflightFailed", error.to_string(), 1))?;
    prepared
        .deliver_plan(&mut io::stdout().lock())
        .map_err(|error| failed("OutputFailed", error.to_string(), 1))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use fabric_sdk::dataagents::{DataAgentError, ReadResponseError};
    use fabric_sdk_operations::{OperationError, OperationErrorKind, OperationResponse};

    #[test]
    fn dataagent_root_error_receipts_preserve_status_and_handle_without_content() {
        for response_limit in [false, true] {
            let operation = OperationError {
                kind: if response_limit {
                    OperationErrorKind::Limit
                } else {
                    OperationErrorKind::Http {
                        status: 403,
                        code: Some("private-service-code".into()),
                        error: None,
                    }
                },
                operation_id: Some("11111111-1111-1111-1111-111111111111".into()),
                mutation_may_have_occurred: true,
                raw_service_error: Some(Box::new(json!({"private":"service-content"}))),
            };
            let source = if response_limit {
                DataAgentError::Response(Box::new(ReadResponseError {
                    error: operation,
                    response: OperationResponse {
                        status: 200,
                        body: b"private-response-content".to_vec(),
                        headers: [("private-header".into(), "private-value".into())].into(),
                    },
                }))
            } else {
                DataAgentError::Operation(operation)
            };
            let failure = command_failure(GetCommandError::Backend {
                code: "DataAgentDefinitionFailed",
                source: Box::new(source),
            });
            let mut output = Vec::new();
            assert_eq!(finish(Err(failure), &mut output), 1);
            let text = std::str::from_utf8(&output).unwrap();
            assert!(!text.contains("private") && !text.contains("service-content"));
            let receipt: Value = serde_json::from_slice(&output).unwrap();
            let evidence = &receipt["evidence"];
            assert_eq!(
                evidence["httpStatus"],
                if response_limit { 200 } else { 403 }
            );
            assert_eq!(
                evidence["classification"],
                if response_limit { "limit" } else { "http" }
            );
            assert_eq!(
                evidence["operationId"],
                "11111111-1111-1111-1111-111111111111"
            );
            assert_eq!(evidence["requestMayHaveReachedService"], true);
            assert_eq!(evidence["remoteCancelled"], false);
            assert_eq!(evidence["remoteReplayed"], false);
        }
    }
}
