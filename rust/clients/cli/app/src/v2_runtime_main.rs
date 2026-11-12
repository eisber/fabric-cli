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
Production reads require a native PBI-audience credential with verified tenant
and object identity, plus an explicitly bound Notebook MwcToken adapter.
The existing headless provider rejects that PBI scope. Fabric /v1 cannot
authorize Notebook MWC routes; its bearer is not an MwcToken.
This build reports NotebookWorkloadBindingRequired before identity or HTTP.";

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
                "Production Notebook reads require an approved workload authorization binding, not Fabric /v1."
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
        let _ = write_stream(OutputStream::Stderr, br#"{"error":{"code":"NotebookWorkloadBindingRequired","message":"Native Notebook reads require a PBI-audience credential with verified tenant/object identity and an explicitly selected MwcToken workload adapter. The current headless provider rejects the required PBI scope; a Fabric /v1 bearer is not a substitute."},"remoteCancelled":false,"remoteReplayed":false}"#.to_vec(), deadline).await;
        1
    });
    runtime.shutdown_timeout(Duration::from_millis(100));
    exit
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
