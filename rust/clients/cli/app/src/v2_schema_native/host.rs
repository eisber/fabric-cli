// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One-shot native schema binding; no v1 dispatcher or navigation/settings store.

use fabric_cli::v2_discovery::DataSchemaRequest;
use std::io::{self, Write};

pub(crate) fn run(
    request: &DataSchemaRequest,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    #[cfg(any(windows, feature = "headless"))]
    {
        let started = std::time::Instant::now();
        let deadline = started + fabric_sdk_http::ExecutionOptions::default().timeout;
        let result = run_selected(request, deadline);
        deliver(result, deadline, stdout, stderr)
    }
    #[cfg(not(any(windows, feature = "headless")))]
    {
        let _ = (request, stdout);
        fabric_cli::v2_discovery::write_failure(
            &fabric_cli::v2_discovery::DiscoveryFailure::NotConfigured,
            stderr,
        )
    }
}

#[cfg(any(windows, feature = "headless"))]
struct Buffered {
    exit: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[cfg(any(windows, feature = "headless"))]
fn run_selected(
    request: &DataSchemaRequest,
    deadline: std::time::Instant,
) -> Result<Buffered, super::Failure> {
    let runtime = super::native_runtime()?;
    let result = runtime.block_on(controlled(
        async {
            let endpoint = super::selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC"))?;
            if endpoint != fabric_sdk::EndpointBinding::public(fabric_sdk::ServiceOrigin::Fabric) {
                return Err(super::failure(
                    "NotSupported",
                    "Data schema requires the public Fabric/OneLake endpoint binding.",
                    1,
                ));
            }
            if std::env::var_os("FAB_RUST_AUTH_MODE").is_none() {
                return Err(super::failure(
                    "NotConfigured",
                    "Data schema requires an explicit FAB_RUST_AUTH_MODE.",
                    1,
                ));
            }
            let (credential, client, identity_client, timestamp) =
                tokio::task::spawn_blocking(|| {
                    let credential = super::selected_credential()?;
                    let (client, identity_client) = super::native_clients(&credential, false)?;
                    let timestamp = crate::local_host::timestamp().map_err(|_| {
                        super::failure("clock_failed", "The UTC clock is unavailable.", 1)
                    })?;
                    Ok::<_, super::Failure>((credential, client, identity_client, timestamp))
                })
                .await
                .map_err(|_| super::failure("host_failed", "Native schema setup failed.", 1))??;
            let (sdk, http) = super::connect_native(
                credential,
                client,
                identity_client,
                endpoint,
                Default::default(),
                None,
            )
            .await?;
            execute_authenticated(request, &sdk, &http, timestamp).await
        },
        async {
            tokio::select! {
                biased;
                signal = tokio::signal::ctrl_c() => match signal {
                    Ok(()) => super::failure("cancelled", "Data-schema read cancelled.", 1),
                    Err(_) => super::failure(
                        "signal_failed", "The cancellation handler could not start.", 1,
                    ),
                },
                _ = tokio::time::sleep_until(deadline.into()) => timed_out(),
            }
        },
    ));
    super::finish_runtime(runtime, result)
}

#[cfg(any(windows, feature = "headless"))]
async fn execute_authenticated(
    request: &DataSchemaRequest,
    sdk: &fabric_sdk::Client,
    http: &fabric_sdk_http::HttpExecutor,
    timestamp: fabric_cli_output::Timestamp,
) -> Result<Buffered, super::Failure> {
    use fabric_cli::{
        session::ReadSession, v2_discovery::DiscoveryHost, v2_schema_native::NativeSchemaProvider,
    };
    let reads = ReadSession {
        sdk,
        executor: http,
        tenant: fabric_cli_context::Element::tenant(
            sdk.selection().tenant(),
            sdk.selection().tenant(),
        ),
        current_path: "/",
        home: fabric_cli_context::HomeContext::Unspecified,
        timestamp,
        config: Default::default(),
    };
    let provider = NativeSchemaProvider {
        reads: &reads,
        http,
        limits: Default::default(),
    };
    let host = DiscoveryHost {
        data_schema: Some(&provider),
    };
    let mut output = Buffered {
        exit: 1,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    output.exit = host
        .run(
            ["fab-cli-discovery", "schema", request.path()],
            std::future::pending(),
            &mut output.stdout,
            &mut output.stderr,
        )
        .await
        .map_err(|_| super::failure("output_failed", "Schema buffering failed.", 1))?;
    Ok(output)
}

#[cfg(any(windows, feature = "headless"))]
async fn controlled<T>(
    work: impl std::future::Future<Output = Result<T, super::Failure>>,
    stopped: impl std::future::Future<Output = super::Failure>,
) -> Result<T, super::Failure> {
    use futures_util::FutureExt;
    let mut stopped = std::pin::pin!(stopped);
    let result = tokio::select! {
        biased;
        error = &mut stopped => return Err(error),
        result = work => result,
    };
    // Serialization is synchronous; repoll interruption before publishing bytes.
    match stopped.as_mut().now_or_never() {
        Some(error) => Err(error),
        None => result,
    }
}

#[cfg(any(windows, feature = "headless"))]
fn timed_out() -> super::Failure {
    super::failure(
        "timed_out",
        "The data-schema command deadline elapsed. Output may be incomplete; no successful result is available.",
        1,
    )
}

#[cfg(any(windows, feature = "headless"))]
fn deliver(
    result: Result<Buffered, super::Failure>,
    deadline: std::time::Instant,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<i32> {
    let result = if std::time::Instant::now() >= deadline {
        Err(timed_out())
    } else {
        result
    };
    match result {
        Ok(output) => {
            stdout.write_all(&output.stdout)?;
            stdout.flush()?;
            if output.stderr.is_empty() && std::time::Instant::now() >= deadline {
                return write_host_failure(timed_out(), stderr);
            }
            stderr.write_all(&output.stderr)?;
            stderr.flush()?;
            Ok(if std::time::Instant::now() >= deadline {
                1
            } else {
                output.exit
            })
        }
        Err(error) => write_host_failure(error, stderr),
    }
}

#[cfg(any(windows, feature = "headless"))]
fn write_host_failure((code, _, exit): super::Failure, stderr: &mut dyn Write) -> io::Result<i32> {
    serde_json::to_writer(
        &mut *stderr,
        &serde_json::json!({"error": {
            "code": code,
            "message": if code == "NotConfigured" {
                "Data schema requires native features and explicit native identity configuration (FAB_RUST_AUTH_MODE); no automatic login is performed."
            } else {
                "Native data-schema read could not complete; no successful result is available."
            }
        }}),
    )
    .map_err(io::Error::other)?;
    stderr.write_all(b"\n")?;
    stderr.flush()?;
    Ok(i32::from(exit))
}

#[cfg(all(test, any(windows, feature = "headless")))]
#[path = "host_tests.rs"]
mod tests;
