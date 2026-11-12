// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One-shot native schema binding; no v1 dispatcher or navigation/settings store.

#[cfg(any(windows, feature = "headless"))]
use crate::native_read as native;
use fabric_cli::v2_discovery::DataSchemaRequest;
use std::io::{self, Write};

pub(crate) fn run(request: &DataSchemaRequest, stderr: &mut dyn Write) -> io::Result<i32> {
    #[cfg(any(windows, feature = "headless"))]
    {
        let started = std::time::Instant::now();
        let deadline = started + fabric_sdk_http::ExecutionOptions::default().timeout;
        let result = run_selected(request, deadline);
        deliver(result, stderr)
    }
    #[cfg(not(any(windows, feature = "headless")))]
    {
        let _ = request;
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
) -> Result<Buffered, native::Failure> {
    let runtime = native::native_runtime()?;
    let result = runtime.block_on(async {
        let mut stopped = std::pin::pin!(async {
            tokio::select! {
                biased;
                signal = tokio::signal::ctrl_c() => match signal {
                    Ok(()) => native::failure("cancelled", "Data-schema read cancelled.", 1),
                    Err(_) => native::failure(
                        "signal_failed", "The cancellation handler could not start.", 1,
                    ),
                },
                _ = tokio::time::sleep_until(deadline.into()) => timed_out(),
            }
        });
        let (output, stdout) = controlled(async {
            let endpoint = native::selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC"))?;
            if endpoint != fabric_sdk::EndpointBinding::public(fabric_sdk::ServiceOrigin::Fabric) {
                return Err(native::failure(
                    "NotSupported",
                    "Data schema requires the public Fabric/OneLake endpoint binding.",
                    1,
                ));
            }
            if std::env::var_os("FAB_RUST_AUTH_MODE").is_none() {
                return Err(native::failure(
                    "NotConfigured",
                    "Data schema requires an explicit FAB_RUST_AUTH_MODE.",
                    1,
                ));
            }
            let stdout = native::execute::owned_stdout().map_err(|error| {
                if error.kind() == io::ErrorKind::Unsupported {
                    native::failure(
                        "UnsupportedCapability",
                        "Native schema output requires stdout redirected to a file or pipe on Windows.",
                        1,
                    )
                } else {
                    native::failure("output_failed", "The stdout handle could not be retained.", 1)
                }
            })?;
            let (credential, client, identity_client, timestamp) =
                tokio::task::spawn_blocking(|| {
                    let credential = native::selected_credential()?;
                    let (client, identity_client) = native::native_clients(&credential, false)?;
                    let timestamp = crate::local_host::timestamp().map_err(|_| {
                        native::failure("clock_failed", "The UTC clock is unavailable.", 1)
                    })?;
                    Ok::<_, native::Failure>((credential, client, identity_client, timestamp))
                })
                .await
                .map_err(|_| native::failure("host_failed", "Native schema setup failed.", 1))??;
            let (sdk, http) = native::connect_native(
                credential,
                client,
                identity_client,
                endpoint,
                Default::default(),
                None,
            )
            .await?;
            let output = execute_authenticated(request, &sdk, &http, timestamp).await?;
            Ok((output, stdout))
        }, stopped.as_mut()).await?;
        publish_stdout(output, stdout, deadline, stopped.as_mut()).await
    });
    native::finish_runtime(runtime, result)
}

#[cfg(any(windows, feature = "headless"))]
async fn execute_authenticated(
    request: &DataSchemaRequest,
    sdk: &fabric_sdk::Client,
    http: &fabric_sdk_http::HttpExecutor,
    timestamp: fabric_cli_output::Timestamp,
) -> Result<Buffered, native::Failure> {
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
        .map_err(|_| native::failure("output_failed", "Schema buffering failed.", 1))?;
    Ok(output)
}

#[cfg(any(windows, feature = "headless"))]
async fn controlled<T>(
    work: impl std::future::Future<Output = Result<T, native::Failure>>,
    stopped: impl std::future::Future<Output = native::Failure>,
) -> Result<T, native::Failure> {
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
fn timed_out() -> native::Failure {
    native::failure(
        "timed_out",
        "The data-schema command deadline elapsed. Output may be incomplete; no successful result is available.",
        1,
    )
}

#[cfg(any(windows, feature = "headless"))]
async fn publish_stdout(
    mut output: Buffered,
    mut stdout: impl Write + Send + 'static,
    deadline: std::time::Instant,
    cancelled: impl std::future::Future<Output = native::Failure>,
) -> Result<Buffered, native::Failure> {
    if output.stdout.is_empty() {
        return Ok(output);
    }
    if std::time::Instant::now() >= deadline {
        return Err(timed_out());
    }
    let bytes = std::mem::take(&mut output.stdout);
    controlled(
        async {
            let worker = tokio::task::spawn_blocking(move || {
                stdout.write_all(&bytes)?;
                stdout.flush()
            });
            worker
                .await
                .map_err(|_| native::failure("output_failed", "The output worker failed.", 1))?
                .map_err(|_| native::failure("output_failed", "Schema output failed.", 1))?;
            Ok(output)
        },
        async {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline.into()) => timed_out(),
                error = cancelled => error,
            }
        },
    )
    .await
}

#[cfg(any(windows, feature = "headless"))]
fn deliver(result: Result<Buffered, native::Failure>, stderr: &mut dyn Write) -> io::Result<i32> {
    match result {
        Ok(output) => {
            if !output.stdout.is_empty() {
                return Err(io::Error::other("Schema output was not supervised."));
            }
            stderr.write_all(&output.stderr)?;
            stderr.flush()?;
            Ok(output.exit)
        }
        Err(error) => write_host_failure(error, stderr),
    }
}

#[cfg(any(windows, feature = "headless"))]
fn write_host_failure((code, _, exit): native::Failure, stderr: &mut dyn Write) -> io::Result<i32> {
    serde_json::to_writer(
        &mut *stderr,
        &serde_json::json!({"error": {
            "code": code,
            "message": if code == "NotConfigured" {
                "Data schema requires native features and explicit native identity configuration (FAB_RUST_AUTH_MODE); no automatic login is performed."
            } else if code == "UnsupportedCapability" {
                "Native schema output requires stdout redirected to a file or pipe on Windows."
            } else if matches!(code, "cancelled" | "timed_out" | "signal_failed" | "output_failed") {
                "Native data-schema output is unconfirmed and may be partial; an in-flight write may still complete. Local waiting has stopped and no read is replayed."
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
