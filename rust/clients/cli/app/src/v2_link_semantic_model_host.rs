// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Semantic-model-only composition over the existing v2 session and link adapter.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::{
    get_resolution::{GetResolutionError, ResolvedGetTarget, resolve},
    get_target::{GetRepresentation, GetTarget},
};
use fabric_cli_mutations::{
    CommandFuture, FabricMutationBackend, MutationError,
    v2_link::{
        self, LinkApproval, LinkCommand, LinkEffects, LinkFiles, LinkOwner, LinkReceipt,
        LinkResolver, LinkScope, PreparedLink, ResolvedLinkTarget,
    },
};
use fabric_sdk::{Client, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_operations::OperationLimits;
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    future::pending,
    io::{self, IsTerminal, Read, Write},
    num::NonZeroUsize,
    time::{Duration, Instant},
};

const INPUT_LIMIT: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(240);

#[cfg(test)]
#[path = "v2_link_semantic_model_host_tests.rs"]
mod tests;

pub(super) fn handles(args: &[OsString]) -> bool {
    args.first().is_some_and(|arg| arg == "link")
        || (args.first().is_some_and(|arg| arg == "artifact")
            && args.get(1).is_some_and(|arg| arg == "semantic-model"))
}

fn parse(args: Vec<OsString>) -> Result<Option<LinkCommand>, Failure> {
    let argv: Vec<_> = std::iter::once(OsString::from("fab-cli-v2"))
        .chain(args)
        .collect();
    match v2_link::command().try_get_matches_from(&argv) {
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            write!(io::stdout().lock(), "{error}")
                .map_err(|_| failed("OutputFailed", "Link help delivery failed.", 1))?;
            Ok(None)
        }
        Err(_) => Err(failed(
            "InvalidArguments",
            "Use artifact semantic-model link <target> --input FILE|- [--wait].",
            2,
        )),
        Ok(_) => {
            let command = v2_link::parse_args(argv).map_err(local_failure)?;
            if command
                .facade
                .is_some_and(|owner| owner != LinkOwner::SemanticModel)
            {
                return Err(failed(
                    "UnsupportedCapability",
                    "This host supports semantic-model connection links only.",
                    2,
                ));
            }
            typed_target(&command.path).map_err(local_failure)?;
            Ok(Some(command))
        }
    }
}

fn typed_target(path: &str) -> Result<GetTarget, MutationError> {
    let owner = path.strip_suffix("/connections").ok_or_else(|| {
        MutationError::new(
            "InvalidTarget",
            "Link requires an explicit SemanticModel connections child",
        )
    })?;
    let target = GetTarget::parse(owner)
        .map_err(|_| MutationError::new("InvalidTarget", "Invalid typed connection target"))?;
    if !matches!(
        &target,
        GetTarget::Item {
            item_type: Some(kind),
            representation: GetRepresentation::Metadata,
            ..
        } if kind == "SemanticModel"
    ) {
        return Err(MutationError::new(
            "UnsupportedCapability",
            "Link requires a typed SemanticModel owner and its connections child",
        ));
    }
    Ok(target)
}

struct Input {
    selector: String,
    bytes: Vec<u8>,
}

impl Input {
    fn read(command: &LinkCommand, stdin: &mut dyn Read) -> Result<Self, MutationError> {
        let mut bytes = Vec::new();
        if command.input == "-" {
            stdin.take((INPUT_LIMIT + 1) as u64).read_to_end(&mut bytes)
        } else {
            let metadata = std::fs::symlink_metadata(&command.input)
                .map_err(|_| MutationError::new("InputFailed", "Link input is unavailable"))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(MutationError::new(
                    "InvalidInput",
                    "Link input requires a regular non-symlink file",
                ));
            }
            std::fs::File::open(&command.input)
                .map_err(|_| MutationError::new("InputFailed", "Link input is unavailable"))?
                .take((INPUT_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
        }
        .map_err(|_| MutationError::new("InputFailed", "Link input reading failed"))?;
        if bytes.len() > INPUT_LIMIT {
            return Err(MutationError::new(
                "BudgetExceeded",
                "Link input exceeds 1 MiB",
            ));
        }
        Ok(Self {
            selector: command.input.clone(),
            bytes,
        })
    }
}

impl LinkFiles for Input {
    fn read<'a>(
        &'a mut self,
        path: &'a str,
        max_bytes: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, MutationError>> {
        Box::pin(async move {
            if path != self.selector || self.bytes.len() > max_bytes.get() {
                return Err(MutationError::new(
                    "InvalidInput",
                    "Link input does not match the captured bounded input",
                ));
            }
            Ok(self.bytes.clone())
        })
    }
}

struct Resolver<'a> {
    client: &'a Client,
    http: &'a fabric_sdk_http::HttpExecutor,
}

impl LinkResolver for Resolver<'_> {
    fn resolve<'a>(
        &'a mut self,
        path: &'a str,
    ) -> CommandFuture<'a, Result<ResolvedLinkTarget, MutationError>> {
        Box::pin(async move {
            let target = typed_target(path)?;
            let resolved = resolve(
                target,
                self.client,
                self.http,
                self.http,
                NonZeroUsize::new(INPUT_LIMIT).unwrap(),
                pending(),
            )
            .await
            .map_err(resolution_failure)?;
            let ResolvedGetTarget::Item {
                workspace_id,
                item_id,
                item_type,
                ..
            } = resolved
            else {
                return Err(MutationError::new(
                    "InvalidTarget",
                    "Link owner is not an item",
                ));
            };
            if item_type.as_deref() != Some("SemanticModel") {
                return Err(MutationError::new(
                    "InvalidTarget",
                    "Link owner is not a SemanticModel",
                ));
            }
            ResolvedLinkTarget::new(
                &workspace_id,
                &item_id,
                LinkOwner::SemanticModel,
                LinkScope::SemanticModelConnections,
            )
        })
    }
}

fn resolution_failure(error: GetResolutionError) -> MutationError {
    use fabric_sdk::{items::ItemErrorKind, workspaces::WorkspaceListErrorKind};
    use fabric_sdk_workspaces::mutations::ResourceError;
    let message = error.to_string();
    let native = match error {
        GetResolutionError::NotFound => return MutationError::new("NotFound", message),
        GetResolutionError::AmbiguousTarget => {
            return MutationError::new("AmbiguousTarget", message);
        }
        GetResolutionError::Workspace(error) => match error.kind {
            WorkspaceListErrorKind::Http {
                status,
                code,
                service_error,
            } => Some((status, code, service_error)),
            _ => None,
        },
        GetResolutionError::Item(error) => match error.kind {
            ItemErrorKind::Http {
                status,
                code,
                service_error,
                ..
            } => Some((status, code, service_error)),
            _ => None,
        },
        _ => None,
    };
    if let Some((status, code, service_error)) = native {
        let mut error = ResourceError::input();
        error.code = code;
        error.status = Some(status);
        error.message = message;
        error.raw_service_error = service_error.as_ref().map(|value| Box::new(json!(value)));
        error.service_error = service_error;
        return error.into();
    }
    MutationError::new("TargetResolutionFailed", message)
}

struct ApprovalContext {
    target: ResolvedLinkTarget,
    effects: LinkEffects,
    request: Value,
    context: RequestContext,
}

trait Approval: Send {
    fn approve<'a>(
        &'a mut self,
        context: &'a ApprovalContext,
    ) -> CommandFuture<'a, Result<bool, MutationError>>;
}

struct BoundApproval<'a> {
    captured: ApprovalContext,
    policy: &'a mut dyn Approval,
    interrupted: Shared<BoxFuture<'a, ()>>,
}

impl LinkApproval for BoundApproval<'_> {
    fn approve<'a>(
        &'a mut self,
        target: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        Box::pin(async move {
            if target != &self.captured.target || effects != self.captured.effects {
                return Err(MutationError::new(
                    "ApprovalRequired",
                    "Link approval context changed",
                ));
            }
            tokio::select! {
                biased;
                _ = self.interrupted.clone() => Err(MutationError::new(
                    "Cancelled", "Link approval interrupted; no bind was submitted",
                )),
                result = self.policy.approve(&self.captured) => result,
            }
        })
    }
}

struct TerminalApproval;

impl Approval for TerminalApproval {
    fn approve<'a>(
        &'a mut self,
        context: &'a ApprovalContext,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        Box::pin(async move {
            if !io::stderr().is_terminal() {
                return Err(MutationError::new(
                    "ApprovalRequired",
                    "An interactive terminal is required for immediate link approval",
                ));
            }
            let question = serde_json::to_string(&json!({
                "operation": v2_link::OPERATION_ID,
                "workspaceId": context.target.workspace(),
                "itemId": context.target.item(),
                "type": context.target.owner().item_type(),
                "scope": "connections",
                "remoteWrite": context.effects.remote_write,
                "account": context.context.selection().account(),
                "tenant": context.context.selection().tenant(),
                "endpoint": context.context.endpoint().base_url().as_str(),
                "request": context.request,
            }))
            .map_err(|_| MutationError::new("ApprovalFailed", "Link approval rendering failed"))?;
            tokio::task::spawn_blocking(move || {
                // Structured stdin cannot double as execution approval.
                #[cfg(windows)]
                let device = "CONIN$";
                #[cfg(not(windows))]
                let device = "/dev/tty";
                let input = std::fs::File::open(device)?;
                fabric_cli::console::read_explicit_confirmation(
                    &mut io::BufReader::new(input),
                    &mut io::stderr(),
                    &format!("Bind this existing connection? {question}"),
                )
            })
            .await
            .map_err(|_| MutationError::new("ApprovalFailed", "Link approval worker failed"))?
            .map_err(|_| {
                MutationError::new(
                    "ApprovalRequired",
                    "An immediate terminal approval is required",
                )
            })
        })
    }
}

async fn execute(
    prepared: PreparedLink,
    context: RequestContext,
    http: &fabric_sdk_http::HttpExecutor,
    approval: &mut dyn Approval,
    cancelled: impl std::future::Future<Output = ()> + Send,
) -> Result<LinkReceipt, Box<v2_link::LinkFailure>> {
    let captured = ApprovalContext {
        target: prepared.target().clone(),
        effects: prepared.effects(),
        request: prepared
            .operation()
            .body()
            .cloned()
            .expect("validated native bind includes its exact request body"),
        context: context.clone(),
    };
    let interrupted = cancelled.boxed().shared();
    let mut bound = BoundApproval {
        captured,
        policy: approval,
        interrupted: interrupted.clone(),
    };
    let mut operations = FabricMutationBackend::new(
        context.credential().clone(),
        context.selection().clone(),
        http,
        OperationLimits {
            max_requests: NonZeroUsize::new(100).unwrap(),
            max_response_bytes: NonZeroUsize::new(INPUT_LIMIT).unwrap(),
            max_poll_delay: Duration::from_secs(60),
        },
        interrupted,
    );
    v2_link::execute(prepared, &mut operations, Some(&mut bound)).await
}

fn local_failure(error: MutationError) -> Failure {
    let native = error.operation.as_deref();
    (
        "LinkPreflightFailed",
        error.to_string(),
        json!({
            "code":error.code,
            "httpStatus":native.and_then(|error| error.status),
            "serviceError":native.and_then(|error| error.raw_service_error.as_deref()),
            "mutationMayHaveOccurred":false,
            "remoteReplayed":false,
        }),
        2,
    )
}

fn execution_failure(error: &v2_link::LinkFailure) -> Failure {
    let operation = error.error.operation.as_deref();
    (
        "LinkFailed",
        error.to_string(),
        json!({
            "target": {"workspaceId":error.target.workspace(),"itemId":error.target.item(),"type":"SemanticModel"},
            "code":error.error.code,
            "httpStatus":operation.and_then(|error| error.status),
            "operationId":operation.and_then(|error| error.operation_id.as_deref()),
            "serviceError":operation.and_then(|error| error.raw_service_error.as_deref()),
            "mutationMayHaveOccurred":operation.is_some_and(|error| error.mutation_may_have_occurred),
            "remoteCancelled":false,
            "remoteReplayed":false,
        }),
        1,
    )
}

fn deliver(receipt: &LinkReceipt, writer: &mut dyn Write) -> Result<(), Failure> {
    let metadata = receipt.metadata();
    let result = serde_json::to_writer(&mut *writer, &metadata)
        .map_err(io::Error::other)
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush());
    result.map_err(|_| {
        (
            "OutputFailed",
            "Link receipt delivery failed; do not replay the bind.".into(),
            metadata,
            1,
        )
    })
}

pub(super) fn run(args: Vec<OsString>) -> Result<(), Failure> {
    let Some(command) = parse(args)? else {
        return Ok(());
    };
    let started = Instant::now();
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
    if endpoint != EndpointBinding::public(ServiceOrigin::Fabric) {
        return Err(failed(
            "UnsupportedCapability",
            "Link requires the public Fabric endpoint.",
            2,
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .map_err(|_| failed("RuntimeFailed", "Link runtime could not start.", 1))?;
    let result = runtime.block_on(async {
        let captured = command.clone();
        let input = tokio::select! {
            biased;
            _ = tokio::time::sleep(TIMEOUT) => return Err(failed("TimedOut", "Link input expired; no bind was submitted.", 1)),
            _ = tokio::signal::ctrl_c() => return Err(failed("Cancelled", "Link input cancelled; no bind was submitted.", 1)),
            result = tokio::task::spawn_blocking(move || Input::read(&captured, &mut io::stdin().lock())) =>
                result.map_err(|_| failed("InputFailed", "Link input worker failed.", 1))?.map_err(local_failure)?,
        };
        let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|_| failed(
            "NotConfigured", "Select explicit FAB_RUST_AUTH_MODE; no identity fallback is used.", 1,
        ))?;
        let selection = headless::Selection::capture(&mode)
            .map_err(|(code, message, exit)| failed(code, message, exit))?;
        let session = NativeSession::new(selection, endpoint, TIMEOUT)?;
        let client = Client::with_context(session.context.clone());
        let mut resolver = Resolver { client: &client, http: session.executor.as_ref() };
        let mut input = input;
        let preflight = v2_link::prepare_command(
            &command, &mut resolver, &mut input, NonZeroUsize::new(INPUT_LIMIT).unwrap(),
        );
        let remaining = TIMEOUT.saturating_sub(started.elapsed());
        let prepared = tokio::select! {
            biased;
            _ = tokio::time::sleep(remaining) => return Err(failed("TimedOut", "Link preflight expired; no bind was submitted.", 1)),
            _ = tokio::signal::ctrl_c() => return Err(failed("Cancelled", "Link preflight cancelled; no bind was submitted.", 1)),
            result = preflight => result.map_err(local_failure)?,
        };
        let remaining = TIMEOUT.saturating_sub(started.elapsed());
        let interruption = std::sync::atomic::AtomicU8::new(0);
        let interrupted = async {
            tokio::select! {
                _ = tokio::time::sleep(remaining) => interruption.store(1, std::sync::atomic::Ordering::SeqCst),
                _ = tokio::signal::ctrl_c() => interruption.store(2, std::sync::atomic::Ordering::SeqCst),
            }
        };
        execute(prepared, session.context, session.executor.as_ref(), &mut TerminalApproval, interrupted)
            .await.map_err(|error| {
                let mut failure = execution_failure(&error);
                failure.2["localInterruption"] = match interruption.load(std::sync::atomic::Ordering::SeqCst) {
                    1 => json!("TimedOut"),
                    2 => json!("Cancelled"),
                    _ => Value::Null,
                };
                failure
            })
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    deliver(&result?, &mut io::stdout().lock())
}
