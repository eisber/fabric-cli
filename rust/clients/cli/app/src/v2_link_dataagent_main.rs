// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! DataAgent-only executable adapter over the existing v2 native session.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::v2_link_dataagent_host as host;
use fabric_cli_mutations::{
    CommandFuture, MutationError,
    v2_link::{LinkApproval, LinkEffects, PreparedLink, ResolvedLinkTarget},
};
use fabric_sdk::{EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_operations::{OperationLimits, OperationSession};
use futures_util::FutureExt;
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(300);

pub(super) fn run(args: Vec<OsString>) -> Result<u8, Failure> {
    let command = match host::parse(args) {
        Ok(command) => command,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            write!(io::stdout().lock(), "{error}")
                .map_err(|_| failed("OutputFailed", "Link help delivery failed.", 1))?;
            return Ok(0);
        }
        Err(_) => {
            return Err(failed(
                "InvalidArguments",
                "Invalid DataAgent link arguments; run artifact data-agent link --help.",
                2,
            ));
        }
    };
    let started = Instant::now();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .map_err(|_| failed("RuntimeFailed", "Link runtime could not start.", 1))?;
    let result: Result<host::Dispatch, Failure> = runtime.block_on(async {
        let preparation = tokio::task::spawn_blocking(move || {
            host::prepare(&command, &mut host::NativeFiles::new(&mut io::stdin()))
                .now_or_never()
                .ok_or_else(|| failed("InputFailed", "Local link preparation did not complete.", 1))?
                .map_err(|error| (
                    "LinkPreflightFailed",
                    error.message,
                    serde_json::json!({"code":error.code,"mutationMayHaveOccurred":false}),
                    2,
                ))
        });
        let prepared = tokio::select! {
            biased;
            _ = tokio::time::sleep(TIMEOUT.saturating_sub(started.elapsed())) =>
                return Err(failed("TimedOut", "Link input deadline elapsed; no request was submitted.", 1)),
            signal = tokio::signal::ctrl_c() =>
                return Err(signal_failure(signal, "Link input interrupted; no request was submitted.")),
            result = preparation => result
                .map_err(|_| failed("InputFailed", "Link input worker failed; no request was submitted.", 1))??,
        };
        if !io::stderr().is_terminal() {
            return Err(failed(
                "ApprovalRequired",
                "Link requires an interactive approval terminal; no submission occurred.",
                1,
            ));
        }
        let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|_| failed(
            "NotConfigured", "Select explicit FAB_RUST_AUTH_MODE; no identity fallback is used.", 1,
        ))?;
        let selection = headless::Selection::capture(&mode)
            .map_err(|(code, message, exit)| failed(code, message, exit))?;
        let endpoint = match std::env::var("FAB_RUST_FABRIC_HOST") {
            Ok(host) => EndpointBinding::with_host(ServiceOrigin::Fabric, &host)
                .map_err(|_| failed("InvalidConfiguration", "Invalid explicit Fabric endpoint.", 2))?,
            Err(std::env::VarError::NotPresent) => EndpointBinding::public(ServiceOrigin::Fabric),
            Err(_) => return Err(failed("InvalidConfiguration", "Invalid explicit Fabric endpoint.", 2)),
        };
        let remaining = TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(failed("TimedOut", "Link deadline elapsed before transport setup; no request was submitted.", 1));
        }
        let session = NativeSession::new(selection, endpoint, remaining)?;
        let mut approval = Approval {
            prepared: prepared.clone(),
            question: approval_question(&prepared, &session.context),
            deadline: started + TIMEOUT,
        };
        let interrupted = async {
            tokio::select! {
                _ = tokio::time::sleep(TIMEOUT.saturating_sub(started.elapsed())) => (),
                _ = tokio::signal::ctrl_c() => (),
            }
        };
        let mut operations = OperationSession::with_context(
            &session.context,
            session.executor.as_ref().clone(),
            OperationLimits::default(),
            interrupted,
        );
        Ok(host::dispatch(prepared, &mut operations, Some(&mut approval)).await)
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    let dispatch = result?;
    let exit = dispatch.exit_code();
    match dispatch.write_to(&mut io::stdout().lock()) {
        Ok(_) => Ok(exit),
        Err(error) => Err((
            "OutputFailed",
            "Link output failed; retain execution evidence and do not replay the mutation.".into(),
            error.evidence(),
            1,
        )),
    }
}

fn signal_failure(result: io::Result<()>, message: &'static str) -> Failure {
    if result.is_ok() {
        failed("Cancelled", message, 1)
    } else {
        failed(
            "SignalFailed",
            "Link interruption handler could not start; no request was submitted.",
            1,
        )
    }
}

fn approval_question(prepared: &PreparedLink, context: &RequestContext) -> String {
    format!(
        "Create exactly this staging DataAgent datasource reference?\nContext: {}\nPOST {}\nInput: {}\nEffects: remote reference write only; no publication, query, source creation, table selection or ACL grant.",
        serde_json::json!({
            "tenant":context.selection().tenant(),
            "account":context.selection().account(),
            "endpoint":context.endpoint().host(),
        }),
        prepared.operation().path(),
        prepared
            .operation()
            .body()
            .unwrap_or(&serde_json::Value::Null),
    )
}

struct Approval {
    prepared: PreparedLink,
    question: String,
    deadline: Instant,
}

impl LinkApproval for Approval {
    fn approve<'a>(
        &'a mut self,
        target: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        Box::pin(async move {
            if target != self.prepared.target() || effects != self.prepared.effects() {
                return Err(MutationError::new(
                    "ApprovalRequired",
                    "Link approval target or effects changed",
                ));
            }
            let question = self.question.clone();
            let confirmation = tokio::task::spawn_blocking(move || {
                #[cfg(windows)]
                let console = std::fs::File::open("CONIN$")?;
                #[cfg(not(windows))]
                let console = std::fs::File::open("/dev/tty")?;
                fabric_cli::console::read_explicit_confirmation(
                    &mut io::BufReader::new(console),
                    &mut io::stderr(),
                    &question,
                )
            });
            tokio::select! {
                biased;
                _ = tokio::time::sleep(self.deadline.saturating_duration_since(Instant::now())) =>
                    Err(MutationError::new("TimedOut", "Link approval deadline elapsed; no request was submitted")),
                signal = tokio::signal::ctrl_c() => {
                    let (code,message,_,_) = signal_failure(signal, "Link approval interrupted; no request was submitted.");
                    Err(MutationError::new(code, message))
                },
                result = confirmation => result
                    .map_err(|_| MutationError::new("ApprovalFailed", "Link approval worker failed"))?
                    .map_err(|_| MutationError::new("ApprovalFailed", "Link approval could not be obtained")),
            }
        })
    }
}
