// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Failure, NativeSession, failed};
use fabric_cli::v2_link_lakehouse_host::{self as host, LocalFiles};
use fabric_cli_mutations::{
    CommandFuture, MutationError,
    v2_link::{LinkApproval, LinkEffects, LinkScope, ResolvedLinkTarget},
};
use fabric_sdk::{EndpointBinding, ServiceOrigin};
use fabric_sdk_operations::{OperationLimits, OperationSession};
use futures_util::FutureExt;
use std::{
    ffi::OsString,
    io::{self, IsTerminal},
    time::Duration,
};

pub(super) fn run(args: Vec<OsString>) -> Result<(), Failure> {
    let command = match host::parse(std::iter::once(OsString::from("fab-cli-v2")).chain(args)) {
        Ok(command) => command,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            host::command()
                .write_long_help(&mut io::stdout().lock())
                .map_err(|_| failed("OutputFailed", "Link help output failed.", 1))?;
            return Ok(());
        }
        Err(_) => {
            return Err(failed(
                "InvalidArguments",
                "Invalid link arguments; use link --help.",
                2,
            ));
        }
    };
    let cwd = std::env::current_dir()
        .map_err(|_| failed("InputFailure", "Link working directory is unavailable.", 1))?;
    // This bounded preflight is immediately ready and precedes runtime/identity.
    let prepared = host::prepare(
        &command,
        &mut LocalFiles {
            cwd: &cwd,
            stdin: &mut io::stdin(),
        },
    )
    .now_or_never()
    .ok_or_else(|| failed("InputFailure", "Local link preflight did not complete.", 1))?
    .map_err(|error| failed("InvalidInput", error.to_string(), 2))?;
    let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|_| {
        failed(
            "NotConfigured",
            "Select an explicit FAB_RUST_AUTH_MODE; no credential chain is used.",
            1,
        )
    })?;
    let selection = super::headless::Selection::capture(&mode)
        .map_err(|(code, message, exit)| failed(code, message, exit))?;
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .map_err(|_| failed("RuntimeFailed", "The link runtime could not be created.", 1))?;
    let result = runtime.block_on(async {
        let timeout = Duration::from_secs(300);
        let deadline = tokio::time::Instant::now() + timeout;
        let session = NativeSession::new(selection, endpoint, timeout)?;
        let cancelled = async {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {},
                _ = tokio::signal::ctrl_c() => {},
            }
        };
        let mut operations = OperationSession::with_context(
            &session.context,
            session.executor.as_ref().clone(),
            OperationLimits::default(),
            cancelled,
        );
        let mut approval = ConsoleApproval { deadline };
        host::execute(prepared, &mut operations, Some(&mut approval))
            .await
            .map_err(|error| {
                (
                    "LinkFailed",
                    "Link failed or is incomplete; inspect retained evidence.".into(),
                    host::failure_metadata(&error),
                    1,
                )
            })
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    let receipt = result?;
    host::deliver(receipt, &mut io::stdout().lock()).map_err(|error| {
        (
            "OutputFailed",
            "Link output failed; do not resubmit to reproduce the receipt.".into(),
            host::receipt_metadata(&error.receipt),
            1,
        )
    })
}

struct ConsoleApproval {
    deadline: tokio::time::Instant,
}

impl LinkApproval for ConsoleApproval {
    fn approve<'a>(
        &'a mut self,
        target: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        Box::pin(async move {
            if !io::stderr().is_terminal() {
                return Err(MutationError::new(
                    "ApprovalRequired",
                    "Link requires an explicit terminal approval",
                ));
            }
            let LinkScope::LakehouseShortcut { path, name } = target.scope() else {
                return Err(MutationError::new(
                    "UnsupportedCapability",
                    "Only Lakehouse shortcut approval is supported",
                ));
            };
            let question = format!(
                "Create shortcut {}/{}/{}/{} with conflict policy Abort? Remote reference write: {}; update item sensitivity: {}. No copy or ACL grant.",
                target.workspace(),
                target.item(),
                path.escape_debug(),
                name.escape_debug(),
                effects.remote_write,
                effects.update_item_sensitivity,
            );
            let worker = tokio::task::spawn_blocking(move || {
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
                _ = tokio::time::sleep_until(self.deadline) =>
                    Err(MutationError::new("TimedOut", "Link approval deadline elapsed; no submission occurred")),
                _ = tokio::signal::ctrl_c() =>
                    Err(MutationError::new("Cancelled", "Link approval was interrupted; no submission occurred")),
                result = worker => result
                    .map_err(|_| MutationError::new("ApprovalRequired", "Link approval worker failed"))?
                    .map_err(|_| MutationError::new("ApprovalRequired", "Explicit link approval is unavailable")),
            }
        })
    }
}
