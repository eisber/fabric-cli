// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! KQL link composition in the existing explicit native resource head.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::{
    get_resolution::{self, ResolvedGetTarget},
    v2_link_kql_host::{self as host, KqlDestination, MAX_BYTES},
};
use fabric_cli_mutations::{
    CommandFuture, MutationError,
    v2_link::{self, LinkApproval, LinkEffects, LinkFiles, LinkResolver, ResolvedLinkTarget},
};
use fabric_sdk::{Client, EndpointBinding, ServiceOrigin};
use fabric_sdk_http::HttpExecutor;
use fabric_sdk_operations::{OperationLimits, OperationSession};
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Read},
    num::NonZeroUsize,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(test)]
#[path = "v2_link_kql_main_tests.rs"]
mod tests;

pub(super) fn handles(args: &[OsString]) -> bool {
    args.first().and_then(|value| value.to_str()) == Some("link")
        || (args.first().and_then(|value| value.to_str()) == Some("artifact")
            && args.get(1).and_then(|value| value.to_str()) == Some("kql-database")
            && (args.len() == 2
                || matches!(
                    args.get(2).and_then(|value| value.to_str()),
                    Some("link" | "--help" | "-h")
                )))
}

pub(super) fn run(args: Vec<OsString>) -> Result<u8, Failure> {
    let mut schema = v2_link::command();
    match schema.try_get_matches_from_mut(
        std::iter::once(OsString::from("fab-cli-v2")).chain(args.iter().cloned()),
    ) {
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            use std::io::Write;
            io::stdout()
                .lock()
                .write_all(error.to_string().as_bytes())
                .map_err(|_| failed("OutputFailed", "Link help delivery failed.", 1))?;
            return Ok(0);
        }
        Err(_) => {
            return Err(failed(
                "InvalidArguments",
                "Invalid link arguments; use link --help.",
                2,
            ));
        }
        Ok(_) => (),
    }
    let command =
        host::parse(args).map_err(|error| failed("InvalidArguments", error.to_string(), 2))?;
    let started = Instant::now();
    let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|_| {
        failed(
            "NotConfigured",
            "Select explicit FAB_RUST_AUTH_MODE; no credential fallback is used.",
            1,
        )
    })?;
    let selection = headless::Selection::capture(&mode)
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
        .max_blocking_threads(2)
        .build()
        .map_err(|_| failed("RuntimeFailed", "Link runtime could not start.", 1))?;
    let result = runtime.block_on(async {
        let session = NativeSession::new(selection, endpoint, TIMEOUT)?;
        let client = Client::with_context(session.context.clone());
        let mut resolver = Resolver { client: &client, executor: &session.executor };
        let mut files = Files;
        let remaining = TIMEOUT.saturating_sub(started.elapsed());
        let interrupted = async {
            tokio::select! {
                _ = tokio::time::sleep(remaining) => (),
                _ = tokio::signal::ctrl_c() => (),
            }
        };
        tokio::pin!(interrupted);
        let prepared = tokio::select! {
            biased;
            () = &mut interrupted => return Err(failed("Cancelled", "Link preflight interrupted; no mutation was submitted.", 1)),
            prepared = host::prepare(&command, &mut resolver, &mut files) => prepared
                .map_err(|error| failed("LinkPreflightFailed", error.to_string(), 1))?,
        };
        // Approval is part of the shared execute boundary. Race only pre-write
        // phases; shared OperationSession retains evidence during submission/polls.
        let mut approval = ConsoleApproval;
        let effects = prepared.effects();
        let approved = tokio::select! {
            biased;
            () = &mut interrupted => return Err(failed("Cancelled", "Link approval interrupted; no mutation was submitted.", 1)),
            result = approval.approve(prepared.target(), effects) => result
                .map_err(|error| failed("ApprovalRequired", error.to_string(), 1))?,
        };
        if !approved {
            return Err(failed("ApprovalDenied", "Link was not approved; no mutation was submitted.", 1));
        }
        let mut approved = CapturedApproval(prepared.target().clone(), effects);
        let mut operations = OperationSession::with_context(
            &session.context,
            (*session.executor).clone(),
            OperationLimits {
                max_requests: NonZeroUsize::new(32).unwrap(),
                max_response_bytes: NonZeroUsize::new(MAX_BYTES).unwrap(),
                max_poll_delay: Duration::from_secs(60),
            },
            &mut interrupted,
        );
        Ok(host::Delivery::execute(prepared, &mut operations, Some(&mut approved)).await)
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    Ok(result?.write_to(&mut io::stdout().lock(), &mut io::stderr().lock()))
}

struct Resolver<'a> {
    client: &'a Client,
    executor: &'a HttpExecutor,
}

impl LinkResolver for Resolver<'_> {
    fn resolve<'a>(
        &'a mut self,
        path: &'a str,
    ) -> CommandFuture<'a, Result<ResolvedLinkTarget, MutationError>> {
        Box::pin(async move {
            let destination = KqlDestination::parse(path)?;
            let resolved = get_resolution::resolve(
                destination.owner(),
                self.client,
                self.executor,
                self.executor,
                NonZeroUsize::new(MAX_BYTES).unwrap(),
                std::future::pending(),
            )
            .await
            .map_err(|error| MutationError::new("ResolutionFailed", &error.to_string()))?;
            let ResolvedGetTarget::Item {
                workspace_id,
                item_id,
                ..
            } = resolved
            else {
                return Err(MutationError::new(
                    "InvalidTarget",
                    "A KQL database owner is required",
                ));
            };
            destination.resolved(&workspace_id, &item_id)
        })
    }
}

struct Files;

impl LinkFiles for Files {
    fn read<'a>(
        &'a mut self,
        path: &'a str,
        max_bytes: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, MutationError>> {
        let path = path.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut input: Box<dyn Read> = if path == "-" {
                    Box::new(io::stdin())
                } else {
                    Box::new(std::fs::File::open(path).map_err(|_| {
                        MutationError::new("InputFailed", "Link input could not be opened")
                    })?)
                };
                let mut bytes = Vec::new();
                input
                    .by_ref()
                    .take(max_bytes.get() as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| {
                        MutationError::new("InputFailed", "Link input could not be read")
                    })?;
                if bytes.len() > max_bytes.get() {
                    return Err(MutationError::new(
                        "BudgetExceeded",
                        "Link input exceeds the byte limit",
                    ));
                }
                Ok(bytes)
            })
            .await
            .map_err(|_| MutationError::new("InputFailed", "Link input worker failed"))?
        })
    }
}

struct ConsoleApproval;

impl LinkApproval for ConsoleApproval {
    fn approve<'a>(
        &'a mut self,
        target: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        let question = host::approval_question(target, effects);
        Box::pin(async move {
            if !io::stderr().is_terminal() {
                return Err(MutationError::new(
                    "ApprovalRequired",
                    "An interactive terminal is required; stdin cannot approve a link",
                ));
            }
            tokio::task::spawn_blocking(move || {
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
            .map_err(|_| MutationError::new("ApprovalRequired", "Link approval worker failed"))?
            .map_err(|_| {
                MutationError::new(
                    "ApprovalRequired",
                    "Explicit link approval could not be read or delivered",
                )
            })
        })
    }
}

struct CapturedApproval(ResolvedLinkTarget, LinkEffects);

impl LinkApproval for CapturedApproval {
    fn approve<'a>(
        &'a mut self,
        target: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        Box::pin(async move {
            if &self.0 != target || self.1 != effects {
                return Err(MutationError::new(
                    "ApprovalRequired",
                    "The captured link approval does not match",
                ));
            }
            Ok(true)
        })
    }
}
