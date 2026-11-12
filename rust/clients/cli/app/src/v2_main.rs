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

#[path = "headless.rs"]
mod headless;
mod v2_exists_main;
#[cfg(feature = "v2-refresh")]
mod v2_refresh_main;
mod v2_search_main;
mod v2_update_lakehouse_host;

// Reuse the existing binary-private selector without importing v1 dispatch/state.
mod native_read {
    pub(crate) type Failure = (&'static str, String, u8);

    pub(crate) fn failure(code: &'static str, message: impl Into<String>, exit: u8) -> Failure {
        (code, message.into(), exit)
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(command) = v2_update_lakehouse_host::execution_command(&args) {
        return std::process::ExitCode::from(v2_update_lakehouse_host::run(command));
    }
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
                Value::Null
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
        writeln!(io::stdout().lock(), "fab-cli-v2: experimental explicit resource head\n\nUsage: fab-cli-v2 get <path> [OPTIONS]\n       fab-cli-v2 search <scope> [OPTIONS]\n       fab-cli-v2 exists <path> [OPTIONS]\n       fab-cli-v2 update <path> --input FILE|- --mode patch [--plan]\n       fab-cli-v2 artifact lakehouse update <path> --input FILE|- --mode patch [--plan]\n\nRun a command with --help for its supported schema.\nUpdate plans are local. Native Lakehouse metadata PATCH requires exact interactive approval; other updates require a caller-injected approval host.\nRemote commands require explicit FAB_RUST_AUTH_MODE; no identity fallback or interactive sign-in.")
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
    let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|_| {
        failed(
            "NotConfigured",
            "Select an explicit FAB_RUST_AUTH_MODE; no credential chain is used.",
            1,
        )
    })?;
    let selection = headless::Selection::capture(&mode)
        .map_err(|(code, message, exit)| failed(code, message, exit))?;
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
        let session = NativeSession::new(selection, endpoint, timeout)?;
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
    fn new(
        selection: headless::Selection,
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
            CredentialSelection::new(&selection.account, &selection.tenant, Audience::Fabric)
                .map_err(|_| {
                    failed(
                        "InvalidConfiguration",
                        "Invalid explicit identity selection.",
                        2,
                    )
                })?;
        let storage_selection =
            CredentialSelection::new(&selection.account, &selection.tenant, Audience::OneLake)
                .map_err(|_| {
                    failed(
                        "InvalidConfiguration",
                        "Invalid explicit storage identity selection.",
                        2,
                    )
                })?;
        let _explicit_provider_binding = (&selection.client, &selection.binding);
        let credential = selection
            .credential(identity_http)
            .map_err(|(code, message, exit)| failed(code, message, exit))?;
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
