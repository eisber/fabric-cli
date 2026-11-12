// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Statement-only executable adapter over the released runtime read host.

use super::{NativeSession, headless};
use clap::{Arg, ArgAction, ArgMatches};
use fabric_cli::{
    get_target::is_id,
    runtime_v2::{
        OwnerKind, ParsedInvocation, ReadErrorKind, ReadFacet, ReadOptions, ResolvedTarget,
        command_schema,
        host::{
            ExplicitResolver, HostError, acquire, diagnostic, prepare_destination, write_stream,
        },
        parse_args,
    },
};
use fabric_cli_output::OutputStream;
use fabric_sdk::{
    EndpointBinding, ServiceOrigin,
    livy::LivyService,
    runtime::{NativeEndpoint, NativeError},
};
use fabric_sdk_http::native_runtime::NativeFabricExecutor;
use std::{
    ffi::OsString,
    future::Future,
    pin::pin,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(30);
const HELP: &str = concat!(
    "Experimental existing Livy statement JSON read\n\n",
    "get /<workspace-guid>.Workspace/<lakehouse-guid>.Lakehouse/sessions/<session>/statements/<id>[/result]\n",
    "Options: --format json, --max-bytes N, --output FILE (or -o FILE).\n",
    "The result alias returns the same full JSON envelope, not all rows or byte pages.\n",
    "Select FAB_RUST_FABRIC_HOST and existing FAB_RUST_AUTH_MODE explicitly.\n",
    "The Lakehouse owner selects Fabric profile v1 / Livy API 2023-12-01.\n",
    "Native .LivyService authorization is unsupported; no endpoint/audience fallback.\n",
    "One 30-second total deadline; no submit, remote cancellation or output replay."
);

fn matches(args: &[OsString]) -> Option<ArgMatches> {
    command_schema()
        .mut_subcommand("get", |command| {
            command.disable_help_flag(true).arg(
                Arg::new("statement-help")
                    .long("help")
                    .short('h')
                    .action(ArgAction::SetTrue),
            )
        })
        .try_get_matches_from(
            std::iter::once(OsString::from("fab-cli-v2")).chain(args.iter().cloned()),
        )
        .ok()
}

pub(super) fn claims(args: &[OsString]) -> bool {
    let Some(matches) = matches(args) else {
        return false;
    };
    let Some(("get", get)) = matches.subcommand() else {
        return false;
    };
    let Some(path) = get.get_one::<String>("path") else {
        return false;
    };
    let Some(path) = path.strip_prefix('/') else {
        return false;
    };
    let parts: Vec<_> = path.split('/').collect();
    matches!(
        parts.as_slice(),
        [workspace, owner, "sessions", _, "statements", _]
        | [workspace, owner, "sessions", _, "statements", _, "result"]
            if workspace.ends_with(".Workspace")
                && (owner.ends_with(".Lakehouse") || owner.ends_with(".LivyService"))
    )
}

fn error(kind: ReadErrorKind) -> HostError {
    HostError {
        kind,
        http_status: None,
        read_completed: false,
        file_created: false,
        bytes_written: 0,
    }
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
        match run_until(args, deadline).await {
            Ok(()) => 0,
            Err(error) => {
                let exit = if matches!(
                    error.kind,
                    ReadErrorKind::InvalidInput | ReadErrorKind::WrongOwner
                ) {
                    2
                } else {
                    1
                };
                let _delivery = diagnostic(&error, deadline).await;
                exit
            }
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    exit
}

async fn run_until(args: Vec<OsString>, deadline: Instant) -> Result<(), HostError> {
    if matches(&args)
        .and_then(|matches| {
            matches
                .subcommand_matches("get")
                .map(|get| get.get_flag("statement-help"))
        })
        .unwrap_or(false)
    {
        return write_stream(OutputStream::Stdout, HELP.as_bytes().to_vec(), deadline)
            .await
            .map_err(|_| error(ReadErrorKind::Delivery));
    }
    let invocation = parse_args(std::iter::once(OsString::from("fab-cli-v2")).chain(args))
        .map_err(HostError::from)?;
    let endpoint = endpoint()?;
    let resolver = binding(&invocation, &endpoint)?;
    let cancelled = async {
        // A failed signal listener fails closed instead of silently disabling cancellation.
        let _signal = tokio::signal::ctrl_c().await;
    };
    let mut cancelled = pin!(cancelled);
    let cwd = std::env::current_dir().map_err(|_| error(ReadErrorKind::OutputUnavailable))?;
    let destination =
        prepare_destination(invocation.output.clone(), cwd, deadline, cancelled.as_mut()).await?;
    let executor = selected_executor(endpoint, deadline)?;
    execute(
        &invocation,
        &executor,
        &resolver,
        destination,
        deadline,
        cancelled.as_mut(),
    )
    .await
}

fn binding(
    invocation: &ParsedInvocation,
    endpoint: &EndpointBinding,
) -> Result<ExplicitResolver, HostError> {
    if !matches!(invocation.path.facet, ReadFacet::Statement { .. }) {
        return Err(error(ReadErrorKind::UnsupportedCapability));
    }
    if invocation.path.owner.kind != OwnerKind::Lakehouse {
        return Err(error(ReadErrorKind::UnsupportedCapability));
    }
    let workspace = invocation
        .path
        .owner
        .workspace
        .strip_suffix(".Workspace")
        .filter(|value| is_id(value))
        .ok_or_else(|| error(ReadErrorKind::InvalidInput))?;
    let lakehouse = invocation
        .path
        .owner
        .artifact
        .strip_suffix(".Lakehouse")
        .filter(|value| is_id(value))
        .ok_or_else(|| error(ReadErrorKind::InvalidInput))?;
    let service = LivyService::fabric_versioned(
        NativeEndpoint::new(endpoint.base_url()).map_err(native_error)?,
        workspace,
        lakehouse,
        "v1",
        "2023-12-01",
    )
    .map_err(native_error)?;
    ExplicitResolver::new(
        invocation.path.owner.clone(),
        ResolvedTarget::Lakehouse(service),
    )
    .map_err(HostError::from)
}

fn endpoint() -> Result<EndpointBinding, HostError> {
    let host =
        std::env::var("FAB_RUST_FABRIC_HOST").map_err(|_| error(ReadErrorKind::InvalidInput))?;
    EndpointBinding::with_host(ServiceOrigin::Fabric, &host)
        .map_err(|_| error(ReadErrorKind::InvalidInput))
}

fn selected_executor(
    endpoint: EndpointBinding,
    deadline: Instant,
) -> Result<NativeFabricExecutor, HostError> {
    let mode = std::env::var("FAB_RUST_AUTH_MODE")
        .map_err(|_| error(ReadErrorKind::UnsupportedCapability))?;
    let selection =
        headless::Selection::capture(&mode).map_err(|_| error(ReadErrorKind::InvalidInput))?;
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .ok_or_else(|| error(ReadErrorKind::Timeout))?;
    let session = NativeSession::new(selection, endpoint, remaining)
        .map_err(|_| error(ReadErrorKind::Execution))?;
    NativeFabricExecutor::new(session.context, session.executor.as_ref().clone())
        .map_err(native_error)
}

fn native_error(value: NativeError) -> HostError {
    fabric_cli::runtime_v2::ReadError::from(value).into()
}

async fn execute(
    invocation: &ParsedInvocation,
    executor: &dyn fabric_sdk::runtime::NativeExecutor,
    resolver: &ExplicitResolver,
    destination: fabric_cli::runtime_v2::host::Destination,
    deadline: Instant,
    cancelled: impl Future<Output = ()>,
) -> Result<(), HostError> {
    let mut cancelled = pin!(cancelled);
    let observation = acquire(
        invocation,
        executor,
        resolver,
        ReadOptions::default(),
        &destination,
        deadline,
        cancelled.as_mut(),
    )
    .await?;
    observation
        .deliver(destination, deadline, cancelled.as_mut())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    const PATH: &str = "/Team.Workspace/Data.Lakehouse/sessions/0/statements/2";

    #[test]
    fn exact_claim_includes_result_and_invalid_ids_but_not_sibling_leaves_or_option_values() {
        for values in [
            vec!["get", PATH],
            vec!["get", "-o", "result.json", PATH],
            vec!["get", PATH, "--help"],
            vec!["get", "/W.Workspace/L.LivyService/sessions/0/statements/-1"],
            vec![
                "get",
                "/W.Workspace/L.Lakehouse/sessions/0/statements/9223372036854775808/result",
            ],
        ] {
            assert!(claims(&args(&values)), "{values:?}");
        }
        for values in [
            vec!["list", "/W.Workspace/L.Lakehouse/sessions"],
            vec!["get", "/W.Workspace/L.Lakehouse/sessions/0"],
            vec!["get", "/W.Workspace/N.Notebook/sessions/0"],
            vec!["get", "/W.Workspace/L.Lakehouse/sessions/0/statements"],
            vec![
                "get",
                "/W.Workspace/L.Lakehouse/sessions/0/statements/2/cancel",
            ],
            vec!["get", "/W.Workspace/L.Lakehouse", "-o", PATH],
            vec!["artifact", "notebook", "get", PATH],
            vec!["get", "--help"],
        ] {
            assert!(!claims(&args(&values)), "{values:?}");
        }
    }

    #[test]
    fn executable_binding_requires_exact_guid_owners_and_never_uses_native_profile_fallback() {
        let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
        for path in [
            "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Lakehouse/sessions/0/statements/0",
            "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Lakehouse/sessions/session-7/statements/9223372036854775807/result",
        ] {
            let input = parse_args(["fab-cli-v2", "get", path]).unwrap();
            assert!(binding(&input, &endpoint).is_ok());
        }
        for path in [
            PATH,
            "/11111111-1111-1111-1111-111111111111.Workspace/Data.Lakehouse/sessions/0/statements/2",
            "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.LivyService/sessions/0/statements/2",
        ] {
            let input = parse_args(["fab-cli-v2", "get", path]).unwrap();
            assert!(binding(&input, &endpoint).is_err());
        }
    }
}
