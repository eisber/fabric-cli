// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Narrow runtime content routing; never substitute public item definitions.

use fabric_cli::runtime_v2::{
    OwnerKind, ReadFacet, ReadOperation, TypedPath, command_schema, host::write_stream, parse_args,
};
use fabric_cli_output::OutputStream;
use std::{
    ffi::OsString,
    time::{Duration, Instant},
};

const HELP: &str = "\
Experimental Notebook runtime content read

Usage: fab-cli-v2 get /<workspace>.Workspace/<notebook>.Notebook/src/content [OPTIONS]
       fab-cli-v2 artifact notebook get <same-path> [OPTIONS]

Options: --format json  --output FILE|-  -o FILE  --max-bytes N
The full persisted IPYNB includes saved outputs, not live execution results.
The host ceiling is 4 MiB wire, 1 MiB data, 2 MiB complete receipt.
No metadata/definition fallback, overwrite, discovery, retry or remote cancel.
Enable notebook-workload for native PBI/verified-principal/MwcToken execution.
Select a non-secret absolute FAB_RUST_NOTEBOOK_BINDING JSON file with exact
owner/catalog/cluster/workload/capacity/workspace/notebook/account/tenant.
Keep existing FAB_RUST_AUTH_MODE and FAB_RUST_FABRIC_HOST selections.
Returned bindings confirm selections only. No live call is made by help.
Without that feature this build reports NotebookWorkloadBindingRequired.";

/// Claim only this leaf or its own facade help, never all `artifact` invocations.
pub(crate) fn claims(args: &[OsString]) -> bool {
    if args == [OsString::from("--help")] || args == [OsString::from("--version")] {
        return true;
    }
    let text = |index: usize| args.get(index).and_then(|value| value.to_str());
    let facade = (text(0), text(1), text(2)) == (Some("artifact"), Some("notebook"), Some("get"));
    if facade {
        if matches!(text(3), Some("--help" | "-h")) {
            return true;
        }
    } else if text(0) != Some("get") {
        return false;
    }
    let argv = |values: Vec<OsString>| std::iter::once(OsString::from("fab-cli-v2")).chain(values);
    let matches = match command_schema().try_get_matches_from(argv(args.to_vec())) {
        Ok(matches) => matches,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            let without_help = args
                .iter()
                .filter(|value| *value != "--help" && *value != "-h")
                .cloned()
                .collect();
            match command_schema().try_get_matches_from(argv(without_help)) {
                Ok(matches) => matches,
                Err(_) => return false,
            }
        }
        Err(_) => return false,
    };
    let leaf = if facade {
        matches
            .subcommand_matches("artifact")
            .and_then(|value| value.subcommand_matches("notebook"))
            .and_then(|value| value.subcommand_matches("get"))
    } else {
        matches.subcommand_matches("get")
    };
    leaf.and_then(|value| value.get_one::<String>("path"))
        .is_some_and(|path| {
            TypedPath::parse(path).is_ok_and(|parsed| {
                parsed.owner.kind == OwnerKind::Notebook
                    && parsed.facet == ReadFacet::NotebookContent
            })
        })
}

pub(crate) fn run(args: Vec<OsString>) -> u8 {
    // This deadline starts before parsing/runtime construction and is never reset.
    let deadline = Instant::now() + Duration::from_secs(30);
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return 1, // No synchronous stream fallback.
    };
    let exit = runtime.block_on(async move {
        if args == [OsString::from("--version")] {
            return if write_stream(OutputStream::Stdout, format!("fab-cli-v2 {}", env!("CARGO_PKG_VERSION")).into_bytes(), deadline).await.is_ok() { 0 } else { 1 };
        }
        if args == [OsString::from("--help")] {
            let help = concat!(
                "fab-cli-v2: experimental explicit resource head\n\n",
                "Usage: fab-cli-v2 get <path> [OPTIONS]\n",
                "       fab-cli-v2 search <scope> [OPTIONS]\n",
                "       fab-cli-v2 exists <path> [OPTIONS]\n",
                "       fab-cli-v2 update <path> --input FILE|- --mode patch --plan\n\n",
                "Run a command with --help for its supported schema.\n",
                "Update plans are local; mutation execution requires a caller-injected approval host.\n",
                "Reads require explicit FAB_RUST_AUTH_MODE; no identity fallback or interactive sign-in.\n\n",
                "Experimental Notebook content: get /<workspace>.Workspace/<notebook>.Notebook/src/content\n",
                "or artifact notebook get <same-path>. Run artifact notebook get --help.\n",
                "Native Notebook reads require notebook-workload and an explicit FAB_RUST_NOTEBOOK_BINDING."
            );
            return if write_stream(OutputStream::Stdout, help.as_bytes().to_vec(), deadline).await.is_ok() { 0 } else { 1 };
        }
        let argv = std::iter::once(OsString::from("fab-cli-v2")).chain(args.clone());
        let help_requested = command_schema()
            .try_get_matches_from(argv)
            .is_err_and(|error| error.kind() == clap::error::ErrorKind::DisplayHelp);
        if help_requested {
            return if write_stream(OutputStream::Stdout, HELP.as_bytes().to_vec(), deadline).await.is_ok() { 0 } else { 1 };
        }
        let input = match parse_args(std::iter::once(OsString::from("fab-cli-v2")).chain(args)) {
            Ok(input) if input.operation == ReadOperation::Get
                && input.path.owner.kind == OwnerKind::Notebook
                && input.path.facet == ReadFacet::NotebookContent => input,
            _ => {
                let _ = write_stream(OutputStream::Stderr, br#"{"error":{"code":"InvalidRuntimeArguments","message":"Invalid Notebook content arguments; run artifact notebook get --help."}}"#.to_vec(), deadline).await;
                return 2;
            }
        };
        // Validate host ceilings before reporting the unavailable authorization
        // capability. Do not construct credentials, HTTP or a guessed MWC context.
        if input.max_bytes > fabric_cli::runtime_v2::ReadOptions::default().max_data_bytes {
            let _ = write_stream(OutputStream::Stderr, br#"{"error":{"code":"BudgetExceeded","message":"Selected data budget exceeds the native host ceiling."}}"#.to_vec(), deadline).await;
            return 2;
        }
        #[cfg(feature = "notebook-workload")]
        {
            execute(input, deadline).await
        }
        #[cfg(not(feature = "notebook-workload"))]
        {
            let _ = write_stream(OutputStream::Stderr, br#"{"error":{"code":"NotebookWorkloadBindingRequired","message":"Enable notebook-workload and select an explicit non-secret FAB_RUST_NOTEBOOK_BINDING file. A Fabric /v1 bearer is not a substitute for Notebook MwcToken authorization."},"remoteCancelled":false,"remoteReplayed":false}"#.to_vec(), deadline).await;
            1
        }
    });
    runtime.shutdown_timeout(Duration::from_millis(100));
    exit
}

#[cfg(feature = "notebook-workload")]
async fn execute(input: fabric_cli::runtime_v2::ParsedInvocation, deadline: Instant) -> u8 {
    use fabric_cli::runtime_v2::{ReadOptions, host::acquire};
    use std::sync::atomic::{AtomicBool, Ordering};
    let signal_failed = AtomicBool::new(false);
    let signal = async {
        if tokio::signal::ctrl_c().await.is_err() {
            signal_failed.store(true, Ordering::SeqCst);
        }
    };
    let mut signal = std::pin::pin!(signal);
    let options = ReadOptions::default();
    let work = async {
        let (connection, destination) =
            super::v2_notebook_connect::prepare(&input, options, deadline, signal.as_mut()).await?;
        let pending = acquire(
            &input,
            &connection.executor,
            &connection.resolver,
            options,
            &destination,
            deadline,
            signal.as_mut(),
        )
        .await
        .map_err(super::v2_notebook_connect::host_failure)?;
        pending
            .deliver(destination, deadline, signal.as_mut())
            .await
            .map_err(super::v2_notebook_connect::host_failure)?;
        Ok::<(), super::Failure>(())
    }
    .await;
    match work {
        Ok(()) => 0,
        Err((code, message, evidence, exit)) => {
            let (code, message) = if signal_failed.load(Ordering::SeqCst) {
                (
                    "SignalFailed",
                    "The local cancellation handler could not start.".to_owned(),
                )
            } else {
                (code, message)
            };
            let bytes = serde_json::to_vec(&serde_json::json!({
                "error":{"code":code,"message":message},"evidence":evidence,
                "remoteCancelled":false,"remoteReplayed":false,
            }))
            .expect("bounded static diagnostic is serializable");
            let _ = write_stream(OutputStream::Stderr, bytes, deadline).await;
            exit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn claims_only_notebook_content_and_own_facade_help() {
        for values in [
            vec!["get", "/W.Workspace/N.Notebook/src/content"],
            vec![
                "artifact",
                "notebook",
                "get",
                "/W.Workspace/N.Notebook/src/content",
            ],
            vec!["artifact", "notebook", "get", "--help"],
        ] {
            assert!(claims(&args(&values)));
        }
        for values in [
            vec!["get", "/W.Workspace/N.Notebook"],
            vec!["get", "/W.Workspace/N.Notebook/sessions/id"],
            vec!["list", "/W.Workspace/N.Notebook/sessions"],
            vec!["get", "/W.Workspace/L.Lakehouse/sessions/id"],
            vec![
                "artifact",
                "notebook",
                "update",
                "/W.Workspace/N.Notebook",
                "--help",
            ],
            vec![
                "artifact",
                "report",
                "get",
                "/W.Workspace/N.Notebook/src/content",
            ],
            vec!["search", "/W.Workspace"],
        ] {
            assert!(!claims(&args(&values)));
        }
    }

    #[tokio::test]
    async fn existing_native_session_can_use_bounded_setup_without_identity_or_http() {
        use fabric_cli::runtime_v2::host::{SetupError, setup};
        let result = setup(
            Instant::now() + Duration::from_secs(5),
            std::future::pending(),
            || {
                let selection = super::super::headless::Selection::from_inputs(
                    "unsupported-offline-mode",
                    |_| None,
                )
                .map_err(|(code, message, exit)| super::super::failed(code, message, exit))?;
                super::super::NativeSession::new(
                    selection,
                    fabric_sdk::EndpointBinding::public(fabric_sdk::ServiceOrigin::Fabric),
                    Duration::from_secs(1),
                )
            },
        )
        .await;
        match result {
            Err(SetupError::Work((code, _, _, _))) => assert_eq!(code, "invalid_configuration"),
            _ => panic!("expected the original typed local configuration failure"),
        }
    }
}
