// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in exact classic Livy GET using the existing explicit Fabric session.

use super::{Failure, NativeSession, capture_fabric_selection, failed};
use fabric_cli::{
    get_target::is_id,
    runtime_v2::{
        OwnerKind, ParsedInvocation, ReadErrorKind, ReadFacet, ReadOptions, ResolvedTarget,
        command_schema,
        host::{
            ExplicitResolver, HostError, SetupError, acquire, prepare_destination, setup,
            write_stream,
        },
        livy_session_get, parse_args,
    },
};
use fabric_cli_output::OutputStream;
use fabric_sdk::{EndpointBinding, ServiceOrigin, livy::LivyService, runtime::NativeEndpoint};
use fabric_sdk_http::native_runtime::NativeFabricExecutor;
use std::{
    ffi::OsString,
    future::Future,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(30);
const HELP: &str = "Exact classic Livy session GET (opt-in runtime-v2)\n\
Usage: fab-cli-v2 get /<workspace-id>.Workspace/<lakehouse-id>.Lakehouse/sessions/<id> [OPTIONS]\n\
Options: --format json, --max-bytes N, --output FILE|- (-o FILE)\n\
This executable explicitly binds Lakehouse to Fabric JSON API 2023-12-01.\n\
Workspace and Lakehouse must be explicit GUIDs; session IDs are opaque SDK IDs.\n\
Select existing FAB_RUST_AUTH_MODE and optional FAB_RUST_FABRIC_HOST explicitly.\n\
The original 30-second deadline includes authentication, HTTP and output.\n\
No discovery, readiness claim, implicit session, creation, cancel or replay.\n\
Native .LivyService requires a separately authorized workload binding; no fallback.";

fn owns_path(path: &str) -> bool {
    parse_args(["fab-cli-v2", "get", path]).is_ok_and(|input| livy_session_get::claims(&input))
}

pub(super) fn claims(args: &[OsString]) -> bool {
    if args.first().and_then(|value| value.to_str()) != Some("get") {
        return false;
    }
    let matches = command_schema().ignore_errors(true).try_get_matches_from(
        std::iter::once(OsString::from("fab-cli-v2")).chain(args.iter().cloned()),
    );
    if let Ok(matches) = matches
        && let Some(("get", leaf)) = matches.subcommand()
        && let Some(path) = leaf.get_one::<String>("path")
    {
        return owns_path(path);
    }
    args.get(1)
        .and_then(|value| value.to_str())
        .is_some_and(owns_path)
}

fn host_failure(error: HostError) -> Failure {
    let code = match error.kind {
        ReadErrorKind::Timeout => "TimedOut",
        ReadErrorKind::Cancelled => "Cancelled",
        ReadErrorKind::Delivery => "OutputFailed",
        _ => "RuntimeReadFailed",
    };
    (
        code,
        "Exact Livy session read did not complete delivery.".into(),
        serde_json::json!({
            "kind": error.kind,
            "httpStatus": error.http_status,
            "readCompleted": error.read_completed,
            "fileCreated": error.file_created,
            "bytesWrittenAtObservation": error.bytes_written,
            "remoteCancelled": false,
            "remoteReplayed": false
        }),
        1,
    )
}

fn setup_failure(error: SetupError<Failure>) -> Failure {
    match error {
        SetupError::Host(error) => host_failure(error),
        SetupError::Work(error) => error,
    }
}

fn service(input: &ParsedInvocation, endpoint: &EndpointBinding) -> Result<LivyService, Failure> {
    if !livy_session_get::claims(input) {
        return Err(failed(
            "InvalidArguments",
            "Not an exact classic session GET.",
            2,
        ));
    }
    if input.path.owner.kind != OwnerKind::Lakehouse {
        return Err(failed(
            "UnsupportedCapability",
            "Native LivyService requires a separately authorized workload binding; no Fabric fallback.",
            1,
        ));
    }
    let workspace = input.path.owner.workspace.strip_suffix(".Workspace");
    let lakehouse = input.path.owner.artifact.strip_suffix(".Lakehouse");
    let (Some(workspace), Some(lakehouse)) = (workspace, lakehouse) else {
        return Err(failed("InvalidArguments", "Invalid typed Livy owner.", 2));
    };
    if !is_id(workspace) || !is_id(lakehouse) {
        return Err(failed(
            "ExplicitBindingRequired",
            "Executable Livy GET requires explicit workspace and Lakehouse GUIDs; names are not backend IDs.",
            2,
        ));
    }
    let service = LivyService::fabric_versioned(
        NativeEndpoint::new(endpoint.base_url())
            .map_err(|_| failed("InvalidConfiguration", "Invalid Fabric binding.", 2))?,
        workspace,
        lakehouse,
        "v1",
        "2023-12-01",
    )
    .map_err(|_| failed("InvalidConfiguration", "Invalid Fabric Livy binding.", 2))?;
    let ReadFacet::Session { id } = &input.path.facet else {
        return Err(failed(
            "InvalidArguments",
            "An exact session ID is required.",
            2,
        ));
    };
    service
        .session(id)
        .and_then(|session| session.get())
        .map_err(|_| {
            failed(
                "InvalidArguments",
                "Invalid exact classic Livy session ID.",
                2,
            )
        })?;
    Ok(service)
}

async fn execute(
    input: &ParsedInvocation,
    deadline: Instant,
    cancelled: impl Future<Output = ()>,
) -> Result<(), Failure> {
    tokio::pin!(cancelled);
    let (selected_endpoint, cwd) = setup(deadline, cancelled.as_mut(), || {
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
        let cwd = std::env::current_dir().map_err(|_| {
            failed(
                "InvalidWorkingDirectory",
                "Output working directory unavailable.",
                2,
            )
        })?;
        Ok((endpoint, cwd))
    })
    .await
    .map_err(setup_failure)?;
    // Validate the explicit owner and opaque SDK ID before credential selection.
    let service = service(input, &selected_endpoint)?;
    let resolver =
        ExplicitResolver::new(input.path.owner.clone(), ResolvedTarget::Lakehouse(service))
            .map_err(|_| {
                failed(
                    "InvalidConfiguration",
                    "Invalid exact Livy owner binding.",
                    2,
                )
            })?;
    let destination = prepare_destination(input.output.clone(), cwd, deadline, cancelled.as_mut())
        .await
        .map_err(host_failure)?;
    let session = setup(deadline, cancelled.as_mut(), move || {
        let (selection, endpoint) = capture_fabric_selection()?;
        if endpoint != selected_endpoint {
            return Err(failed(
                "InvalidConfiguration",
                "The explicit Fabric endpoint changed after preflight.",
                2,
            ));
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|value| !value.is_zero())
            .ok_or_else(|| failed("TimedOut", "Livy GET deadline elapsed before dispatch.", 1))?;
        NativeSession::new(selection, endpoint, remaining)
    })
    .await
    .map_err(setup_failure)?;
    let executor = NativeFabricExecutor::new(session.context, session.executor.as_ref().clone())
        .map_err(|_| {
            failed(
                "InvalidConfiguration",
                "Fabric executor binding was refused.",
                2,
            )
        })?;
    let read = acquire(
        input,
        &executor,
        &resolver,
        ReadOptions::default(),
        &destination,
        deadline,
        cancelled.as_mut(),
    )
    .await
    .map_err(host_failure)?;
    read.deliver(destination, deadline, cancelled.as_mut())
        .await
        .map_err(host_failure)?;
    Ok(())
}

pub(super) fn run(args: Vec<OsString>) -> u8 {
    let deadline = Instant::now() + TIMEOUT;
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
    else {
        // No unbounded synchronous stderr fallback when the runtime is unavailable.
        return 1;
    };
    let exit = runtime.block_on(async {
        if args.iter().any(|arg| arg == "--help" || arg == "-h") {
            return if write_stream(OutputStream::Stdout, HELP.as_bytes().to_vec(), deadline)
                .await
                .is_ok()
            {
                0
            } else {
                1
            };
        }
        let result = match parse_args(std::iter::once(OsString::from("fab-cli-v2")).chain(args)) {
            Ok(input) => {
                execute(&input, deadline, async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
            }
            Err(_) => Err(failed(
                "InvalidArguments",
                "Invalid exact Livy GET arguments.",
                2,
            )),
        };
        match result {
            Ok(()) => 0,
            Err((code, message, evidence, exit)) => {
                let bytes = serde_json::to_vec(&serde_json::json!({
                    "error": {"code": code, "message": message},
                    "evidence": evidence
                }));
                if let Ok(bytes) = bytes {
                    let _ = write_stream(OutputStream::Stderr, bytes, deadline).await;
                }
                exit
            }
        }
    });
    runtime.shutdown_timeout(Duration::ZERO);
    exit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn routing_is_exact_and_output_arguments_cannot_claim_a_foreign_path() {
        let path = "/Team.Workspace/Data.Lakehouse/sessions/opaque";
        assert!(claims(&args(&["get", path])));
        assert!(claims(&args(&["get", "--format", "json", path])));
        assert!(claims(&args(&["get", path, "--help"])));
        assert!(claims(&args(&["get", path, "--invalid"])));
        for values in [
            vec!["list", "/Team.Workspace/Data.Lakehouse/sessions"],
            vec!["get", "/Team.Workspace/Analysis.Notebook/sessions/opaque"],
            vec![
                "get",
                "/Team.Workspace/Data.Lakehouse/sessions/opaque/statements/1",
            ],
            vec!["get", "/Team.Workspace/Data.Lakehouse", "-o", path],
            vec!["artifact", "notebook", "get", path],
        ] {
            assert!(!claims(&args(&values)));
        }
    }

    #[test]
    fn fabric_binding_keeps_selected_endpoint_version_and_opaque_session_id() {
        let input = parse_args([
            "fab-cli-v2",
            "get",
            "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Lakehouse/sessions/opaque_~.7",
        ])
        .unwrap();
        let endpoint =
            EndpointBinding::with_host(ServiceOrigin::Fabric, "msitapi.fabric.microsoft.com")
                .unwrap();
        let service = service(&input, &endpoint).unwrap();
        let request = service.session("opaque_~.7").unwrap().get().unwrap();
        assert_eq!(
            request.native().url().as_str(),
            "https://msitapi.fabric.microsoft.com/v1/workspaces/11111111-1111-1111-1111-111111111111/lakehouses/22222222-2222-2222-2222-222222222222/livyApi/versions/2023-12-01/sessions/opaque_~.7"
        );
        assert_eq!(
            service.profile(),
            fabric_sdk::livy::ServiceProfile::FabricJson
        );
    }
}
