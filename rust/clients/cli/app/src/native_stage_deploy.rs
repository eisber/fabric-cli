// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exact stage deployment on the existing native identity, HTTP and output host.

use super::{Failure, failure, output};
use azure_core::http::Method;
use fabric_cli::stage_deploy::{
    StageDeployCommand, StageDeployError, StageDeployInvocation, execution_exit_code,
};
use fabric_cli_output::OutputStream;
use fabric_deploy::stage::{
    DeploymentApproval, DeploymentError, DeploymentErrorKind, DeploymentReceipt, DeploymentRequest,
    PreparedDeployment, StageReader,
};
use fabric_sdk::{RequestContext, ServiceOrigin, workspaces::WorkspaceResponse};
use fabric_sdk_http::{ExecutionError, ExecutionFuture, HttpExecutor, ResponseProgress};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use futures_util::{FutureExt, future::Shared};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    future::Future,
    io::{self, IsTerminal, Read, Write},
    num::NonZeroUsize,
    pin::Pin,
    sync::Mutex,
    time::{Duration, Instant},
};

#[cfg(test)]
#[path = "native_stage_deploy_tests.rs"]
mod tests;

#[derive(Clone, Copy)]
enum Stop {
    Deadline,
    Cancelled,
    SignalFailed,
}

type Stopping<'a> = Shared<Pin<Box<dyn Future<Output = Stop> + Send + 'a>>>;

fn stopping(
    deadline: Instant,
    signal: impl Future<Output = Stop> + Send + 'static,
) -> Stopping<'static> {
    let future: Pin<Box<dyn Future<Output = Stop> + Send>> = Box::pin(async move {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline.into()) => Stop::Deadline,
            stop = signal => stop,
        }
    });
    future.shared()
}

fn interrupted(stop: Stop) -> Failure {
    match stop {
        Stop::Deadline => failure(
            "TimedOut",
            "The total stage deployment deadline elapsed. Remote or filesystem effects may remain; do not replay.",
            1,
        ),
        Stop::Cancelled => failure(
            "Cancelled",
            "Local stage deployment waiting stopped. No remote cancellation or rollback was requested; do not replay.",
            2,
        ),
        Stop::SignalFailed => failure(
            "SignalFailed",
            "The cancellation handler failed. Remote effects may remain; do not replay.",
            1,
        ),
    }
}

async fn before<T>(
    work: impl Future<Output = Result<T, Failure>>,
    stop: Stopping<'_>,
) -> Result<T, Failure> {
    tokio::select! {
        biased;
        stop = stop => Err(interrupted(stop)),
        result = work => result,
    }
}

async fn blocking_before<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
    stop: Stopping<'_>,
) -> Result<T, Failure> {
    before(
        async {
            tokio::task::spawn_blocking(work).await.map_err(|_| {
                failure(
                    "HostWorkerFailed",
                    "A stage host preparation worker failed; no deployment was submitted.",
                    1,
                )
            })?
        },
        stop,
    )
    .await
}

fn read_request(command: &StageDeployCommand) -> Result<DeploymentRequest, Failure> {
    let invalid = || {
        failure(
            "InputFailure",
            "Unable to read a bounded regular native JSON input file; no deployment was submitted.",
            2,
        )
    };
    if !std::fs::metadata(command.input())
        .map_err(|_| invalid())?
        .is_file()
    {
        return Err(invalid());
    }
    let mut file = std::fs::File::open(command.input()).map_err(|_| invalid())?;
    if !file.metadata().map_err(|_| invalid())?.is_file() {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(
            u64::try_from(command.max_input_bytes())
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    command.request(&bytes).map_err(|error| {
        failure(
            "InvalidInput",
            format!(
                "Invalid native stage input ({:?}); no deployment was submitted.",
                error.kind
            ),
            2,
        )
    })
}

fn approval_question(
    prepared: &PreparedDeployment,
    context: &RequestContext,
    identity: &str,
) -> Result<String, DeploymentError> {
    let (source, target) = prepared.workspaces();
    // JSON quoting keeps input notes and selected identity from injecting terminal
    // controls or impersonating a confirmation question.
    let scope = json!({
        "identity": identity,
        "credentialSelection": {
            "account": context.selection().account(),
            "tenant": context.selection().tenant(),
            "audience": context.selection().audience().scope(),
        },
        "endpoint": context.endpoint().base_url().as_str(),
        "pipelineId": prepared.request().pipeline_id(),
        "sourceWorkspaceId": source,
        "targetWorkspaceId": target,
        "selection": format!("{:?}", prepared.request().intent()),
        "body": prepared.request().body(),
    });
    let scope = fabric_cli_output::json_dumps(&scope).map_err(|_| DeploymentError {
        kind: DeploymentErrorKind::ApprovalUnavailable,
        operation: None,
        receipt: None,
    })?;
    Ok(format!(
        "Approve this exact native stage deployment?\n{scope}\nFabric enforces Pipeline.Deploy, pipeline admin, workspace/item rights and identity support for every involved item; stage metadata is not authorization. This can create/overwrite paired items and propagate folders, including placement of unselected target items. Dependencies are not expanded. Concurrency is unguarded. Acceptance is not completion; no refresh, app publication, rollback or replay is implied."
    ))
}

async fn terminal_approval(question: String) -> io::Result<bool> {
    if !io::stderr().is_terminal() {
        return Err(io::Error::other("Immediate terminal approval is required"));
    }
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        let console = std::fs::File::open("CONIN$")?;
        #[cfg(not(windows))]
        let console = std::fs::File::open("/dev/tty")?;
        let mut prompt = output::owned_stream(OutputStream::Stderr, true)?;
        fabric_cli::console::read_explicit_confirmation(
            &mut io::BufReader::new(console),
            &mut prompt,
            &question,
        )
    })
    .await
    .map_err(io::Error::other)?
}

struct ImmediateApproval;
impl DeploymentApproval for ImmediateApproval {
    fn authorize(&mut self, _: &PreparedDeployment) -> Result<(), DeploymentError> {
        // Private, created only after the terminal approved this still-owned
        // PreparedDeployment; no saved/reusable approval or command flag exists.
        Ok(())
    }
}

#[derive(Default)]
struct SubmissionHeaders(Mutex<Option<OperationResponse>>);

impl ResponseProgress for SubmissionHeaders {
    fn record(&self, response: &WorkspaceResponse, _: bool) -> Result<(), ExecutionError> {
        let mut observed = self.0.lock().map_err(|_| ExecutionError::Transport)?;
        if observed.is_none() {
            *observed = Some(OperationResponse {
                status: response.status,
                body: Vec::new(),
                headers: response
                    .headers
                    .iter()
                    .filter(|(name, _)| {
                        name.eq_ignore_ascii_case("x-ms-operation-id")
                            || name.eq_ignore_ascii_case("deployment-id")
                    })
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect(),
            });
        }
        Ok(())
    }

    fn body_omitted(&self, response: &WorkspaceResponse) -> Result<(), ExecutionError> {
        self.record(response, false)
    }
}

fn response_id(response: &OperationResponse, name: &str) -> Option<String> {
    let mut values = response
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let (_, value) = values.next()?;
    (values.next().is_none()
        && value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        }))
    .then(|| value.clone())
}

struct StageHttp<'a> {
    http: &'a HttpExecutor,
    submission: SubmissionHeaders,
}

impl OperationExecutor for StageHttp<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> ExecutionFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        if request.method() != ResourceMethod::Post {
            return self.http.execute(request);
        }
        Box::pin(async move {
            if request.origin() != ServiceOrigin::Fabric
                || self.http.endpoint().origin() != ServiceOrigin::Fabric
            {
                return Err(OperationExecutionError::Failed);
            }
            self.http
                .execute_bound_with_progress(
                    ServiceOrigin::Fabric,
                    Method::Post,
                    request.url(),
                    request.token(),
                    request.body(),
                    &[],
                    request.max_response_bytes(),
                    Some(&self.submission),
                )
                .await
                .map(|response| OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(|error| match error {
                    ExecutionError::TimedOut => OperationExecutionError::TimedOut,
                    ExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
                    ExecutionError::RetriesExhausted => OperationExecutionError::RetriesExhausted,
                    ExecutionError::InvalidInput
                    | ExecutionError::Transport
                    | ExecutionError::RetryDelay => OperationExecutionError::Failed,
                })
        })
    }

    fn wait(&self, delay: Duration) -> ExecutionFuture<'_, Result<(), OperationExecutionError>> {
        self.http.wait(delay)
    }
}

fn retain_submission(
    result: &mut Result<DeploymentReceipt, DeploymentError>,
    submission: &SubmissionHeaders,
) {
    if let Err(error) = result {
        match submission.0.lock() {
            Ok(observed) => {
                if let Some(response) = observed.as_ref()
                    && let Some(receipt) = error.receipt.as_mut()
                    && receipt.http_status.is_none()
                {
                    // Header progress is not a completed body or native success.
                    receipt.http_status = Some(response.status);
                    receipt.operation_id = response_id(response, "x-ms-operation-id");
                    receipt.deployment_id = response_id(response, "deployment-id");
                    receipt.mutation_may_have_occurred = true;
                    if response.status == 202
                        && (receipt.operation_id.is_none() || receipt.deployment_id.is_none())
                    {
                        error.kind = DeploymentErrorKind::InvalidResponse;
                    }
                }
            }
            Err(_) => error.kind = DeploymentErrorKind::Operation,
        }
    }
}

async fn execute<F: Future<Output = io::Result<bool>>>(
    command: &StageDeployCommand,
    request: DeploymentRequest,
    context: &RequestContext,
    executor: &HttpExecutor,
    identity: &str,
    approve: impl FnOnce(String) -> F,
    stop: Stopping<'_>,
) -> Result<DeploymentReceipt, DeploymentError> {
    let executor = StageHttp {
        http: executor,
        submission: SubmissionHeaders::default(),
    };
    let result = async {
        let mut limits = OperationLimits::default();
        let reader = StageReader {
            context,
            executor: &executor,
            limits: OperationLimits {
                max_requests: NonZeroUsize::new(1).expect("one stage read"),
                ..limits
            },
        };
        let source = reader
            .read_stage(request.pipeline_id(), request.source_stage_id(), async {
                stop.clone().await;
            })
            .await?;
        let target = reader
            .read_stage(request.pipeline_id(), request.target_stage_id(), async {
                stop.clone().await;
            })
            .await?;
        let prepared = PreparedDeployment::new(request, source, target)?;
        prepared.require_context(context)?;
        let question = approval_question(&prepared, context, identity)?;
        let approved = tokio::select! {
            biased;
            reason = stop.clone() => return Err(DeploymentError {
                kind: match reason {
                    Stop::Deadline => DeploymentErrorKind::TimedOut,
                    Stop::Cancelled => DeploymentErrorKind::Cancelled,
                    Stop::SignalFailed => DeploymentErrorKind::Operation,
                },
                operation: None, receipt: None,
            }),
            answer = approve(question) => answer.map_err(|_| DeploymentError {
                kind: DeploymentErrorKind::ApprovalUnavailable, operation: None, receipt: None,
            })?,
        };
        if !approved {
            return Err(DeploymentError {
                kind: DeploymentErrorKind::ApprovalDenied,
                operation: None,
                receipt: None,
            });
        }
        let approved = prepared.authorize(&mut ImmediateApproval)?;
        limits.max_requests =
            NonZeroUsize::new(limits.max_requests.get() - 2).expect("two preflight reads debited");
        approved
            .execute(context, &executor, limits, command.wait(), async {
                stop.clone().await;
            })
            .await
    }
    .await;
    let mut result = result;
    retain_submission(&mut result, &executor.submission);
    if let Err(error) = &mut result
        && let Some(Stop::Deadline) = stop.now_or_never()
    {
        error.kind = DeploymentErrorKind::TimedOut;
    }
    result
}

fn execution_value(result: &Result<DeploymentReceipt, DeploymentError>) -> (Value, u8) {
    match result {
        Ok(receipt) => (receipt.to_value(), execution_exit_code(receipt)),
        Err(error) => {
            let mut value = error.receipt.as_ref().map_or_else(
                || json!({"operation":"deploy","status":"failed"}),
                |receipt| receipt.to_value(),
            );
            value["error"] = json!({
                "code": format!("{:?}", error.kind),
                "message": "Stage deployment failed or is incomplete; retain native evidence and do not replay."
            });
            (
                value,
                if error.kind == DeploymentErrorKind::Cancelled {
                    2
                } else {
                    1
                },
            )
        }
    }
}

fn publication_stop(stop: Stopping<'static>, deadline: Instant) -> Stopping<'static> {
    if matches!(
        stop.clone().now_or_never(),
        Some(Stop::Cancelled | Stop::SignalFailed)
    ) {
        // The observed signal stops execution, not delivery of its retained
        // receipt. Final delivery still has only the original time remaining.
        stopping(deadline, std::future::pending())
    } else {
        stop
    }
}

async fn publish(
    value: Value,
    exit: u8,
    stream: OutputStream,
    deadline: Instant,
    stop: Stopping<'_>,
) -> io::Result<u8> {
    let writer = output::owned_stream(stream, true)?;
    publish_to(writer, value, exit, deadline, stop).await
}

async fn publish_to(
    writer: impl Write + Send + 'static,
    value: Value,
    exit: u8,
    deadline: Instant,
    stop: Stopping<'_>,
) -> io::Result<u8> {
    let mut bytes = fabric_cli_output::json_dumps(&value)
        .map_err(io::Error::other)?
        .into_bytes();
    bytes.push(b'\n');
    let delivered = output::write_owned(
        writer,
        deadline,
        async { interrupted(stop.await) },
        move |writer| writer.write_all(&bytes).and_then(|()| writer.flush()),
    )
    .await;
    match delivered {
        output::OwnedWrite::Complete(Ok(())) => Ok(exit),
        output::OwnedWrite::Complete(Err(error)) => Err(error),
        output::OwnedWrite::Deadline => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Stage output deadline elapsed",
        )),
        output::OwnedWrite::Cancelled(_) => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Stage output interrupted",
        )),
        output::OwnedWrite::WorkerFailed => Err(io::Error::other("Stage output worker failed")),
    }
}

fn delivery_failure(
    execution: Result<Result<DeploymentReceipt, DeploymentError>, Failure>,
    cause: io::Error,
) -> io::Error {
    io::Error::other(match execution {
        Ok(result) => StageDeployError::Delivery {
            cause,
            execution: Box::new(result),
        },
        Err(_) => StageDeployError::Preflight(cause),
    })
}

pub(super) fn run(args: impl IntoIterator<Item = OsString>) -> io::Result<u8> {
    let deadline = Instant::now() + fabric_sdk_http::ExecutionOptions::default().timeout;
    run_before(args, deadline)
}

fn run_before(args: impl IntoIterator<Item = OsString>, deadline: Instant) -> io::Result<u8> {
    let invocation = fabric_cli::stage_deploy::parse_args(args);
    let runtime =
        super::native_runtime().map_err(|_| io::Error::other("Stage runtime unavailable"))?;
    let result = runtime.block_on(async {
        let stop = stopping(deadline, async {
            if tokio::signal::ctrl_c().await.is_ok() { Stop::Cancelled } else { Stop::SignalFailed }
        });
        let command = match invocation {
            Ok(StageDeployInvocation::Display(text)) => {
                let writer = output::owned_stream(OutputStream::Stdout, true)?;
                return match output::write_owned(writer, deadline, async { interrupted(stop.await) }, move |writer| {
                    writer.write_all(text.as_bytes()).and_then(|()| writer.flush())
                }).await {
                    output::OwnedWrite::Complete(result) => result.map(|()| 0),
                    _ => Err(io::Error::other("Stage help delivery failed or was interrupted")),
                };
            }
            Ok(StageDeployInvocation::Deploy(command)) => command,
            Err(_) => return publish(json!({"error":{"code":"invalid_arguments","message":"Invalid native stage deploy arguments. Run fab-rs --v2-deploy --help."}}), 2, OutputStream::Stderr, deadline, stop).await,
        };
        let execution = async {
            let input_command = command.clone();
            let request = blocking_before(move || read_request(&input_command), stop.clone()).await?;
            let endpoint = super::selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC"))?;
            let credential = blocking_before(super::selected_credential, stop.clone()).await?;
            let identity = match &credential {
                super::NativeCredential::Broker { tenant, account, client } =>
                    json!({"provider":"broker","tenant":tenant,"account":account,"client":client}),
                #[cfg(feature = "headless")]
                super::NativeCredential::Headless(selection) =>
                    json!({"provider":selection.binding,"tenant":selection.tenant,"account":selection.account,"client":selection.client}),
                #[cfg(feature = "interactive")]
                super::NativeCredential::DeviceCode(selection) =>
                    json!({"provider":"device-code","tenant":selection.tenant,"account":selection.account,"client":selection.client}),
            }.to_string();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { return Err(interrupted(Stop::Deadline)); }
            let mut transport = super::native_transport_options(false);
            transport.connect_timeout = transport.connect_timeout.min(remaining);
            transport.read_timeout = transport.read_timeout.min(remaining);
            transport.write_timeout = transport.write_timeout.min(remaining);
            let (credential, client, identity_client) = blocking_before(move || {
                super::native_clients_with_options(&credential, transport)
                    .map(|(client, identity_client)| (credential, client, identity_client))
            }, stop.clone()).await?;
            let (sdk, executor) = before(super::connect_native(
                credential, client, identity_client, endpoint,
                fabric_sdk_http::ExecutionOptions { timeout: remaining, ..Default::default() }, Some(deadline),
            ), stop.clone()).await?;
            // Acquire both output handles before approval/submission, without writing.
            output::owned_stream(OutputStream::Stdout, true)
                .and_then(|_| output::owned_stream(OutputStream::Stderr, true))
                .map_err(|_| failure("OutputFailure", "Stage output is unavailable; no deployment was submitted.", 1))?;
            Ok(execute(&command, request, sdk.context(), &executor, &identity,
                terminal_approval, stop.clone()).await)
        }.await;
        let (mut value, mut exit) = match &execution {
            Ok(result) => execution_value(result),
            Err((code, message, exit)) => (json!({"error":{"code":code,"message":message}}), *exit),
        };
        if let Some(reason @ (Stop::Cancelled | Stop::SignalFailed)) = stop.clone().now_or_never() {
            let (code, message, interrupted_exit) = interrupted(reason);
            value["error"] = json!({"code":code,"message":message});
            exit = interrupted_exit;
        }
        let output_stop = publication_stop(stop, deadline);
        let stream = if exit == 0 { OutputStream::Stdout } else { OutputStream::Stderr };
        let delivered = publish(value, exit, stream, deadline, output_stop.clone()).await;
        match delivered {
            Ok(exit) => Ok(exit),
            Err(cause) => {
                // Retain original handles/outcomes; never replace failed delivery
                // with a fresh execution or take a global stderr lock.
                let (mut value, _) = match &execution {
                    Ok(result) => execution_value(result),
                    Err((code, message, _)) => (json!({"error":{"code":code,"message":message}}), 1),
                };
                value["error"] = json!({"code":"OutputFailure","message":"Stage receipt delivery failed or may be partial; retain native evidence and do not replay."});
                let fallback = if stream == OutputStream::Stdout {
                    publish(value, 1, OutputStream::Stderr, deadline, output_stop).await
                } else {
                    Err(cause)
                };
                match fallback {
                    Ok(exit) => Ok(exit),
                    Err(cause) => Err(delivery_failure(execution, cause)),
                }
            }
        }
    });
    super::finish_runtime(runtime, result).map_err(|cause| {
        if cause
            .get_ref()
            .is_some_and(|error| error.is::<StageDeployError>())
        {
            cause
        } else {
            io::Error::other(StageDeployError::Preflight(cause))
        }
    })
}
