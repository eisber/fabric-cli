// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exact Notebook session GET mount; all native policy belongs to the shared connector.

use super::{Failure, failed, v2_notebook_connect};
use fabric_cli::runtime_v2::{
    OwnerKind, ReadFacet, ReadOperation, ReadOptions, TypedPath, command_schema,
    host::{acquire, write_stream_cancelled},
    parse_args,
};
use fabric_cli_output::OutputStream;
use futures_util::FutureExt;
use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(30);
const HELP: &str = "Experimental exact Notebook Jupyter session GET.\n\
    get /<name>.Workspace/<name>.Notebook/sessions/<explicitJupyterSessionId>\n\
    artifact notebook get accepts the identical session path and options.\n\
    Options: --format json, --max-bytes N, --output FILE|- / -o FILE|-.\n\
    Requires FAB_RUST_NOTEBOOK_BINDING and the existing explicit native identity.\n\
    JSON receipt only; no active/latest lookup, Spark readiness, metadata/content fallback or lifecycle action.\n\
    One original 30-second deadline covers setup, authentication, HTTP and delivery.\n\
    --timeout, --wait, --project and recursive options are unsupported.";

pub(super) fn claims(args: &[OsString]) -> bool {
    let matches = command_schema().ignore_errors(true).try_get_matches_from(
        std::iter::once(OsString::from("fab-cli-v2")).chain(
            args.iter()
                .filter(|arg| *arg != "--help" && *arg != "-h")
                .cloned(),
        ),
    );
    let Ok(matches) = matches else {
        return false;
    };
    let Some((name, mut leaf)) = matches.subcommand() else {
        return false;
    };
    let name = if name == "artifact" {
        let Some(("notebook", notebook)) = leaf.subcommand() else {
            return false;
        };
        let Some((name, operation)) = notebook.subcommand() else {
            return false;
        };
        leaf = operation;
        name
    } else {
        name
    };
    if name != "get" {
        return false;
    }
    let Some(path) = leaf.get_one::<String>("path") else {
        return false;
    };
    matches!(
        TypedPath::parse(path),
        Ok(TypedPath {
            owner: fabric_cli::runtime_v2::OwnerPath {
                kind: OwnerKind::Notebook,
                ..
            },
            facet: ReadFacet::Session { .. },
        })
    )
}

pub(super) fn run(args: Vec<OsString>) -> u8 {
    let started = Instant::now();
    let Some(deadline) = started.checked_add(TIMEOUT) else {
        return 1;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
    else {
        // No synchronous stream fallback: even diagnostics may be a blocked pipe.
        return 1;
    };
    let exit = runtime.block_on(run_before(args, deadline));
    runtime.shutdown_timeout(
        Duration::from_secs(1).min(deadline.saturating_duration_since(Instant::now())),
    );
    exit
}

async fn run_before(args: Vec<OsString>, deadline: Instant) -> u8 {
    let signal_failed = Arc::new(AtomicBool::new(false));
    let signal_error = Arc::clone(&signal_failed);
    let cancelled = async move {
        if tokio::signal::ctrl_c().await.is_err() {
            signal_error.store(true, Ordering::SeqCst);
        }
    }
    .shared();
    let result = if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        write_stream_cancelled(
            OutputStream::Stdout,
            HELP.as_bytes().to_vec(),
            deadline,
            cancelled.clone(),
        )
        .await
        .map_err(|_| failed("OutputFailed", "Notebook session help delivery failed.", 1))
    } else {
        execute(args, deadline, cancelled.clone()).await
    };
    let result = if signal_failed.load(Ordering::SeqCst) {
        Err(failed(
            "SignalFailed",
            "The Notebook read cancellation handler could not start.",
            1,
        ))
    } else {
        result
    };
    match result {
        Ok(()) => 0,
        Err((code, message, evidence, exit)) => {
            let Ok(bytes) = serde_json::to_vec(&serde_json::json!({
                "error":{"code":code,"message":message},"evidence":evidence,
            })) else {
                return 1;
            };
            // The same signal/deadline bounds diagnostics; no blocking fallback or replay.
            match write_stream_cancelled(OutputStream::Stderr, bytes, deadline, cancelled).await {
                Ok(()) => exit,
                Err(_) if code == "Cancelled" => 2,
                Err(_) => 1,
            }
        }
    }
}

async fn execute(
    args: Vec<OsString>,
    deadline: Instant,
    cancelled: impl std::future::Future<Output = ()>,
) -> Result<(), Failure> {
    let input = parse_args(std::iter::once(OsString::from("fab-cli-v2")).chain(args))
        .map_err(|error| failed("InvalidRuntimeArguments", error.to_string(), 2))?;
    if input.operation != ReadOperation::Get
        || input.path.owner.kind != OwnerKind::Notebook
        || !matches!(input.path.facet, ReadFacet::Session { .. })
    {
        return Err(failed(
            "UnsupportedRuntimeRead",
            "This mount accepts only an explicit Notebook individual-session GET.",
            2,
        ));
    }
    let options = ReadOptions::default();
    let mut cancelled = std::pin::pin!(cancelled);
    let (connection, destination) =
        v2_notebook_connect::prepare(&input, options, deadline, cancelled.as_mut()).await?;
    let read = acquire(
        &input,
        &connection.executor,
        &connection.resolver,
        options,
        &destination,
        deadline,
        cancelled.as_mut(),
    )
    .await
    .map_err(v2_notebook_connect::host_failure)?;
    read.deliver(destination, deadline, cancelled.as_mut())
        .await
        .map_err(v2_notebook_connect::host_failure)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(args: &[&str]) -> bool {
        claims(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn claims_only_exact_notebook_session_get_with_both_spellings() {
        let path = "/Team.Workspace/Analysis.Notebook/sessions/jupyter-1";
        assert!(selected(&["get", path]));
        assert!(selected(&[
            "get",
            "--format",
            "json",
            path,
            "--max-bytes",
            "4096"
        ]));
        assert!(selected(&["artifact", "notebook", "get", path]));
        assert!(selected(&["get", path, "--help"]));
        assert!(selected(&["get", path, "--timeout", "30s"]));
        for args in [
            vec!["get", "/Team.Workspace/Analysis.Notebook"],
            vec!["get", "/Team.Workspace/Analysis.Notebook/src/content"],
            vec!["list", "/Team.Workspace/Analysis.Notebook/sessions"],
            vec!["get", "/Team.Workspace/Analysis.Notebook/sessions/active"],
            vec![
                "get",
                "/Team.Workspace/Analysis.Notebook/sessions/livy-history",
            ],
            vec!["get", "/Team.Workspace/Data.Lakehouse/sessions/1"],
            vec!["get", "/Team.Workspace/Native.LivyService/sessions/1"],
            vec!["artifact", "notebook", "list", path],
            vec!["artifact", "lakehouse", "get", path],
            vec!["artifact", "notebook", "update", path],
        ] {
            assert!(!selected(&args), "{args:?}");
        }
    }
}
