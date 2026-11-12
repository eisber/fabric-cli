// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed create over the existing explicit native identity/HTTP host.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::create_command::{
    CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateBinding, CreateCommandError,
    CreateExecution, CreateTransportEvidence, PreparedCreateCommand, command_for_binding,
    v2_create,
};
use fabric_sdk::{Client, EndpointBinding, ServiceOrigin};
use fabric_sdk_http::ExecutionTrace;
use serde_json::json;
use std::{
    ffi::OsString,
    future::Future,
    io::{self, IsTerminal, Write},
    pin::Pin,
    time::{Duration, Instant},
};

pub(super) fn run(args: Vec<OsString>, binding: &'static dyn CreateBinding) -> Result<u8, Failure> {
    let command = match v2_create::parse_args(args) {
        Ok(command) => command,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            command_for_binding(binding)
                .write_long_help(&mut io::stdout().lock())
                .and_then(|()| io::stdout().flush())
                .map_err(|_| failed("OutputFailed", "Create help delivery failed.", 1))?;
            return Ok(0);
        }
        Err(_) => {
            return Err(failed(
                "InvalidArguments",
                "Invalid create arguments; run create --help.",
                2,
            ));
        }
    };
    let deadline = Instant::now()
        .checked_add(command.timeout)
        .ok_or_else(|| failed("InvalidArguments", "Invalid create deadline.", 2))?;
    let cwd = std::env::current_dir().map_err(|_| {
        failed(
            "InvalidWorkingDirectory",
            "Create working directory is unavailable.",
            2,
        )
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|_| failed("RuntimeFailed", "Create runtime could not start.", 1))?;
    let result = runtime.block_on(async {
        let preparation = tokio::task::spawn_blocking(move || {
            PreparedCreateCommand::preflight(command, binding, &cwd, &mut io::stdin(), false)
        });
        let prepared = tokio::select! {
            biased;
            _ = tokio::time::sleep(deadline.saturating_duration_since(Instant::now())) =>
                return Err(failed("TimedOut", "Create expired during local preflight; no mutation occurred.", 1)),
            _ = tokio::signal::ctrl_c() =>
                return Err(failed("Cancelled", "Create interrupted during preflight; no mutation occurred.", 1)),
            result = preparation => result
                .map_err(|_| failed("InputFailed", "Create input worker failed.", 1))?
                .map_err(command_failure)?,
        };
        if !io::stderr().is_terminal() {
            return Err(failed("ApprovalRequired", "Create requires explicit terminal approval; no credential or provider access occurred.", 1));
        }
        let mode = std::env::var("FAB_RUST_AUTH_MODE")
            .map_err(|_| failed("NotConfigured", "Select explicit FAB_RUST_AUTH_MODE; no identity fallback is used.", 1))?;
        let selection = headless::Selection::capture(&mode)
            .map_err(|(code, message, exit)| failed(code, message, exit))?;
        let endpoint = match std::env::var("FAB_RUST_FABRIC_HOST") {
            Ok(host) => EndpointBinding::with_host(ServiceOrigin::Fabric, &host)
                .map_err(|_| failed("InvalidConfiguration", "Invalid explicit Fabric endpoint.", 2))?,
            Err(std::env::VarError::NotPresent) => EndpointBinding::public(ServiceOrigin::Fabric),
            Err(_) => return Err(failed("InvalidConfiguration", "Invalid explicit Fabric endpoint.", 2)),
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(failed("TimedOut", "Create expired before identity setup.", 1));
        }
        let session = NativeSession::new(selection, endpoint, remaining)?;
        let client = Client::with_context(session.context);
        let trace = ExecutionTrace::default();
        let observe = || {
            let evidence = trace.snapshot();
            CreateTransportEvidence {
                http_status: evidence.status,
                operation_id: evidence.operation_id,
            }
        };
        let interrupted = async {
            tokio::select! {
                _ = tokio::time::sleep(deadline.saturating_duration_since(Instant::now())) => (),
                _ = tokio::signal::ctrl_c() => (),
            }
        };
        prepared.execute(
            binding,
            &client,
            session.executor.as_ref(),
            CreateExecution {
                executor: (*session.executor).clone().with_trace(trace.clone()),
                observe: &observe,
            },
            &mut TerminalApproval,
            interrupted,
        ).await.map_err(command_failure)
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    let delivery = result?;
    if Instant::now() >= deadline {
        return Err((
            "TimedOut", "Create deadline expired before receipt delivery; reconcile the retained evidence, do not replay.".into(),
            delivery.report().value().clone(), 1,
        ));
    }
    let exit = delivery.report().exit_code();
    delivery
        .deliver(&mut io::stdout().lock())
        .map_err(command_failure)?;
    Ok(exit)
}

fn command_failure(error: CreateCommandError) -> Failure {
    let (code, evidence, exit) = match &error {
        CreateCommandError::Policy(error) => (
            "CreatePolicyRejected",
            json!({"reason":error.to_string(),"mutationMayHaveOccurred":false}),
            2,
        ),
        CreateCommandError::Invalid => {
            ("InvalidInput", json!({"mutationMayHaveOccurred":false}), 2)
        }
        CreateCommandError::Approval(CreateApprovalError::Denied) => (
            "ApprovalDenied",
            json!({"mutationMayHaveOccurred":false}),
            1,
        ),
        CreateCommandError::Approval(_) => (
            "ApprovalRequired",
            json!({"mutationMayHaveOccurred":false}),
            1,
        ),
        CreateCommandError::Interrupted => {
            ("Interrupted", json!({"mutationMayHaveOccurred":false}), 1)
        }
        CreateCommandError::Delivery { report, .. } => ("OutputFailed", report.value().clone(), 1),
        _ => ("CreateFailed", json!({"mutationMayHaveOccurred":false}), 1),
    };
    (code, error.to_string(), evidence, exit)
}

struct TerminalApproval;
impl CreateApproval for TerminalApproval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        let question = format!(
            "Approve {} create policy {} for {:?} in workspace {} using account {:?}, tenant {:?}, endpoint {:?}, remoteWrite={}, input={}, output={:?}, wait={}, timeout={:?}? Acceptance may not be completion. Local cancellation/output failure is not rollback; do not automatically replay.",
            request.plan().target().item_type(),
            request.plan().policy().as_str(),
            request.plan().target().name(),
            request.plan().target().parent_id().as_str(),
            request.context().selection().account(),
            request.context().selection().tenant(),
            request.context().endpoint(),
            request.remote_write(),
            match request.plan().action() {
                fabric_sdk::items::create::CreateAction::Create { input } =>
                    input.supplied().map_or_else(
                        || "<omitted: displayName from exact destination>".into(),
                        serde_json::Value::to_string
                    ),
                _ => request.raw_input().map_or_else(
                    || "<omitted>".into(),
                    |raw| String::from_utf8_lossy(raw).escape_debug().to_string()
                ),
            },
            request.command().output,
            request.command().wait,
            request.command().timeout,
        );
        Box::pin(async move {
            let approved = tokio::task::spawn_blocking(move || {
                #[cfg(windows)]
                let console = std::fs::File::open("CONIN$")?;
                #[cfg(not(windows))]
                let console = std::fs::File::open("/dev/tty")?;
                fabric_cli::console::read_explicit_confirmation(
                    &mut io::BufReader::new(console),
                    &mut io::stderr(),
                    &question,
                )
            })
            .await
            .map_err(|_| CreateApprovalError::Unavailable)?
            .map_err(|_| CreateApprovalError::Unavailable)?;
            if approved {
                Ok(())
            } else {
                Err(CreateApprovalError::Denied)
            }
        })
    }
}
