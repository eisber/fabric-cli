// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli::{
    maps::{MapsCommand, execution::Failure},
    tabular_export::{ExportSink, FileSink},
};
use fabric_sdk::maps::{MAX_RESPONSE_BYTES, ProcessingResult};
use serde_json::json;
use std::io::{self, Read, Write};

pub(crate) fn run(command: &MapsCommand) -> io::Result<u8> {
    if command.offline_fixture.is_none() && !command.experimental_private_contract {
        return failed(
            Failure::new(
                "PrivateContractOptInRequired",
                "Maps is disabled by default. Online processing requires --experimental-private-contract and applicable private Shared client admission/entitlement; no credential or network access occurred.",
            ),
            2,
        );
    }
    let mut sink = FileSink::default();
    if let Some(export) = &command.export
        && sink.preflight(export).is_err()
    {
        return failed(
            Failure::new(
                "ExportPreflightFailed",
                "Maps export destination is unavailable or already exists; no processing started.",
            ),
            2,
        );
    }
    let result = if let Some(path) = &command.offline_fixture {
        let read = || -> Result<_, Failure> {
            let metadata = std::fs::symlink_metadata(path).map_err(|_| {
                Failure::new("FixtureUnavailable", "The offline fixture is unavailable.")
            })?;
            if !metadata.is_file() || metadata.len() > MAX_RESPONSE_BYTES as u64 {
                return Err(Failure::new(
                    "InvalidFixture",
                    "Offline fixture must be a bounded regular JSON file, not a symlink or device.",
                ));
            }
            let file = std::fs::File::open(path).map_err(|_| {
                Failure::new(
                    "FixtureUnavailable",
                    "The offline fixture could not be opened.",
                )
            })?;
            let mut bytes = Vec::new();
            file.take(MAX_RESPONSE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| {
                    Failure::new(
                        "FixtureReadFailed",
                        "The offline fixture could not be read.",
                    )
                })?;
            fabric_cli::maps::execution::offline_fixture(&command.request, &bytes)
        };
        read()
    } else {
        online(command)
    };
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let exit = if matches!(error.http_status, Some(401 | 403)) {
                4
            } else {
                1
            };
            return failed(error, exit);
        }
    };
    let offline = command.offline_fixture.is_some();
    if let Some(export) = &command.export {
        let table = match fabric_cli::maps::export_table(&result, offline) {
            Ok(table) => table,
            Err(message) => {
                return failed(Failure::new("InvalidExport", message), 1);
            }
        };
        let bytes = match fabric_cli_output::tabular::encode(&table, export.format) {
            Ok(bytes) => bytes,
            Err(_) => {
                return failed(
                    Failure::new(
                        "ExportEncodingFailed",
                        "Maps export exceeded its 8 MiB encoded-output budget or could not be encoded; no file was published. Select a smaller result or another format.",
                    ),
                    1,
                );
            }
        };
        if sink.publish(export, &bytes).is_err() {
            return failed(
                Failure::new(
                    "ExportDeliveryFailed",
                    "Maps processing finished but result-file delivery failed; processing was not replayed.",
                ),
                1,
            );
        }
        serde_json::to_writer(
            io::stdout().lock(),
            &json!({
                "operation":command.request.operation(),"complete":result.complete(),
                "validation":if offline {"offline-synthetic-fixture"} else {"live-unqualified-private-contract"},
                "delivery":{"status":"succeeded","format":export.format.name(),"rows":table.row_count(),"bytes":bytes.len(),"schema":table.columns()},
                "emptyResultMetadata":if table.row_count() == 0 {result.document().clone()} else {serde_json::Value::Null},
                "note":"This is a delivery receipt; the file contains real result rows, not invocation summaries."
            }),
        )?;
        writeln!(io::stdout().lock())?;
    } else {
        let bytes = match fabric_cli::maps::render(
            &result,
            command.query.as_deref(),
            command.output_format == "text",
            offline,
        ) {
            Ok(bytes) => bytes,
            Err(message) => {
                return failed(Failure::new("OutputFailed", message), 1);
            }
        };
        io::stdout().lock().write_all(&bytes)?;
    }
    if result.complete() {
        Ok(0)
    } else {
        failed(
            Failure::new(
                "PartialResult",
                "Maps returned null records, failed cells or missing matrix pairs. Inspect the result and per-pair statuses; HTTP 200 is not complete processing success.",
            ),
            3,
        )
    }
}

fn failed(error: Failure, exit: u8) -> io::Result<u8> {
    serde_json::to_writer(io::stderr().lock(), &json!({"error":error}))?;
    writeln!(io::stderr().lock())?;
    Ok(exit)
}

#[cfg(any(windows, feature = "headless", feature = "interactive"))]
fn online(command: &MapsCommand) -> Result<ProcessingResult, Failure> {
    let started = std::time::Instant::now();
    match std::env::var("FAB_RUST_AUTH_MODE") {
        Ok(mode) if mode != "broker" => {
            return Err(Failure::new(
                "UnsupportedIdentity",
                "Maps private Shared issuer admission for app-only, managed identity and device-code hosts is unqualified and disabled. No credential inputs were captured and no service call was attempted.",
            ));
        }
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(Failure::new(
                "InvalidConfiguration",
                "Invalid explicit native Maps credential mode.",
            ));
        }
        _ => {}
    }
    let selection = crate::native_read::selected_credential()
        .map_err(|_| Failure::new("IdentityUnavailable", "Select an existing explicitly configured native broker user identity; Maps never starts login or falls back to another client."))?;
    if !selection.is_broker() {
        return Err(Failure::new(
            "UnsupportedIdentity",
            "Maps private Shared issuer admission for app-only, managed identity and device-code hosts is unqualified and disabled. Use offline fixtures; no credential acquisition or service call was attempted.",
        ));
    }
    let (http, identity_http) =
        crate::native_read::native_clients(&selection, false).map_err(|_| {
            Failure::new(
                "TransportUnavailable",
                "The existing native transport could not be configured.",
            )
        })?;
    let runtime = crate::native_read::native_runtime().map_err(|_| {
        Failure::new(
            "RuntimeUnavailable",
            "The existing native runtime could not start.",
        )
    })?;
    let result = runtime.block_on(async {
        let remaining = || command.timeout.checked_sub(started.elapsed()).filter(|v| !v.is_zero())
            .ok_or_else(|| Failure::new("TimedOut", "Maps command deadline elapsed before processing; no provider request was sent."));
        let credential = tokio::time::timeout(remaining()?, selection.connect(identity_http)).await
            .map_err(|_| Failure::new("TimedOut", "Maps native identity deadline elapsed; no provider processing was sent."))?
            .map_err(|_| Failure::new("IdentityUnavailable", "The existing native broker session could not connect silently; no interactive login or identity fallback was attempted."))?;
        fabric_cli::maps::execution::execute(
            &command.request, command.experimental_private_contract, remaining()?, credential, http,
        ).await
    });
    crate::native_read::finish_runtime(runtime, result)
}

#[cfg(not(any(windows, feature = "headless", feature = "interactive")))]
fn online(_: &MapsCommand) -> Result<ProcessingResult, Failure> {
    Err(Failure::new(
        "UnsupportedIdentity",
        "This build has no supported native Maps user-identity host. Offline fixture processing remains available.",
    ))
}
