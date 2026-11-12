// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Notebook collection list mount; all native policy belongs to the shared connector.

use super::{Failure, failed, v2_notebook_connect};
use fabric_cli::runtime_v2::{
    ReadOptions, TypedPath, command_schema, host, native_runtime_notebook_list as leaf, parse_args,
};
use fabric_cli_output::OutputStream;
use std::{
    ffi::OsString,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(30);

fn arguments(args: &[OsString]) -> Option<&[OsString]> {
    match args.first()?.to_str()? {
        "list" => Some(&args[1..]),
        "artifact" if args.get(1)?.to_str()? == "notebook" && args.get(2)?.to_str()? == "list" => {
            Some(&args[3..])
        }
        _ => None,
    }
}

pub(super) fn claims(args: &[OsString]) -> bool {
    let Some(tail) = arguments(args) else {
        return false;
    };
    if matches!(tail, [flag] if flag == "--help" || flag == "-h") {
        return true;
    }
    // Reuse the registered grammar to identify the positional path even when a
    // later option is invalid. No second argv grammar or foreign route fallback.
    let Ok(matches) = command_schema().ignore_errors(true).try_get_matches_from(
        std::iter::once(OsString::from("fab-cli-v2")).chain(args.iter().cloned()),
    ) else {
        return false;
    };
    let Some((name, mut command)) = matches.subcommand() else {
        return false;
    };
    if name == "artifact" {
        let Some((_, notebook)) = command.subcommand() else {
            return false;
        };
        let Some((_, list)) = notebook.subcommand() else {
            return false;
        };
        command = list;
    }
    command.get_one::<String>("path").is_some_and(|path| {
        TypedPath::parse(path).is_ok_and(|path| {
            path.owner.kind == fabric_cli::runtime_v2::OwnerKind::Notebook
                && path.facet == fabric_cli::runtime_v2::ReadFacet::Sessions
        })
    })
}

pub(super) fn run(args: Vec<OsString>) -> u8 {
    let deadline = Instant::now() + TIMEOUT;
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return 1,
    };
    let exit = runtime.block_on(async {
        match execute(args, deadline).await {
            Ok(()) => 0,
            Err((code, message, evidence, exit)) => {
                let bytes = serde_json::to_vec(
                    &serde_json::json!({"error":{"code":code,"message":message},"evidence":evidence}),
                );
                match bytes {
                    Ok(bytes) => {
                        if host::write_stream(OutputStream::Stderr, bytes, deadline).await.is_err() {
                            return 1;
                        }
                    }
                    Err(_) => return 1,
                }
                exit
            }
        }
    });
    runtime.shutdown_timeout(Duration::from_millis(100));
    exit
}

async fn execute(args: Vec<OsString>, deadline: Instant) -> Result<(), Failure> {
    let argv: Vec<_> = std::iter::once(OsString::from("fab-cli-v2"))
        .chain(args)
        .collect();
    if let Err(error) = command_schema().try_get_matches_from(argv.clone()) {
        if error.kind() == clap::error::ErrorKind::DisplayHelp {
            let mut schema = command_schema();
            let command = schema
                .find_subcommand_mut("list")
                .expect("registered list operation");
            let mut bytes = Vec::new();
            command.write_long_help(&mut bytes).map_err(|_| {
                failed(
                    "OutputFailed",
                    "Notebook list help could not be encoded.",
                    1,
                )
            })?;
            return host::write_stream(OutputStream::Stdout, bytes, deadline)
                .await
                .map_err(|_| {
                    failed(
                        "OutputFailed",
                        "Notebook list help could not be delivered.",
                        1,
                    )
                });
        }
        return Err(failed(
            "InvalidRuntimeArguments",
            "Invalid Notebook collection list arguments.",
            2,
        ));
    }
    let input = parse_args(argv)
        .map_err(|error| failed("InvalidRuntimeArguments", error.to_string(), 2))?;
    if !leaf::claims(&input) {
        return Err(failed(
            "InvalidRuntimeArguments",
            "This leaf only lists a typed Notebook session collection.",
            2,
        ));
    }
    let cancelled = async {
        // A failed signal listener is a local cancellation, never permission to
        // run without interruption handling.
        let _ = tokio::signal::ctrl_c().await;
    };
    tokio::pin!(cancelled);
    let options = ReadOptions::default();
    input
        .validate(options)
        .map_err(|error| failed("InvalidRuntimeArguments", error.to_string(), 2))?;
    let (connection, destination) =
        v2_notebook_connect::prepare(&input, options, deadline, &mut cancelled).await?;
    let observation = leaf::acquire(
        &input,
        &connection.executor,
        &connection.resolver,
        options,
        &destination,
        deadline,
        &mut cancelled,
    )
    .await
    .map_err(v2_notebook_connect::host_failure)?;
    observation
        .deliver(destination, deadline, &mut cancelled)
        .await
        .map_err(v2_notebook_connect::host_failure)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_is_only_list_of_notebook_collection_or_its_exact_help() {
        for args in [
            vec!["list", "/Team.Workspace/N.Notebook/sessions"],
            vec![
                "artifact",
                "notebook",
                "list",
                "/Team.Workspace/N.Notebook/sessions",
            ],
            vec![
                "list",
                "--limit",
                "2",
                "/Team.Workspace/N.Notebook/sessions",
            ],
            vec![
                "list",
                "/Team.Workspace/N.Notebook/sessions",
                "--timeout",
                "1",
            ],
            vec!["list", "--help"],
            vec!["artifact", "notebook", "list", "--help"],
        ] {
            assert!(claims(
                &args.into_iter().map(OsString::from).collect::<Vec<_>>()
            ));
        }
        for args in [
            vec!["get", "/Team.Workspace/N.Notebook/src/content"],
            vec![
                "artifact",
                "notebook",
                "get",
                "/Team.Workspace/N.Notebook/sessions/id",
            ],
            vec!["list", "/Team.Workspace/N.Notebook"],
            vec!["list", "/Team.Workspace/N.Notebook/sessions/id"],
            vec!["list", "/Team.Workspace/N.Notebook/sessions/active"],
            vec!["list", "/Team.Workspace/N.Notebook/sessions/livy-history"],
            vec!["list", "/Team.Workspace/N.Lakehouse/sessions"],
            vec!["list", "/Team.Workspace/N.LivyService/sessions"],
            vec![
                "artifact",
                "lakehouse",
                "list",
                "/Team.Workspace/N.Notebook/sessions",
            ],
            vec!["get", "--output", "/Team.Workspace/N.Notebook/sessions"],
            vec![
                "list",
                "/Team.Workspace/N.Lakehouse/sessions",
                "--output",
                "/Team.Workspace/N.Notebook/sessions",
            ],
        ] {
            assert!(!claims(
                &args.into_iter().map(OsString::from).collect::<Vec<_>>()
            ));
        }
    }
}
