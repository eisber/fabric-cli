// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Existing-staging publication in the explicit v2 native head.

use super::{Failure, NativeSession, failed, headless};
use clap::{Arg, ArgAction};
use fabric_cli::{
    get_resolution::{ResolvedGetTarget, resolve},
    get_target::{GetRepresentation, GetTarget},
};
use fabric_sdk::{
    Client, EndpointBinding, ServiceOrigin,
    environment_publish::{
        self, NativeObservation, ObservationPolicy, PreparedPublish, PublishData, PublishError,
        PublishExecution, PublishSource, PublishState, PublishVersion,
    },
    runtime::{NativeDeadline, NativeFuture},
    v2::{
        ApprovalPolicy, ItemIdentity, Outcome, PreparedOperation, ResolvedTarget, Target,
        TargetKind,
    },
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_operations::{
    FabricOperation, OperationExecutor, OperationLimits, OperationSession,
    polling::{self, ObservationDriver, ObservationStep, ObserverFuture},
};
use fabric_sdk_transport::TokioTimer;
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    future::Future,
    io::{self, IsTerminal, Write},
    pin::{Pin, pin},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};

#[cfg(test)]
#[path = "v2_environment_publish_host_tests.rs"]
mod tests;

struct Command {
    path: String,
    target: GetTarget,
    wait: bool,
}

fn command() -> clap::Command {
    clap::Command::new("publish")
        .about("Publish the already-saved staging state of one existing Environment")
        .after_help("Target: /<workspace>.Workspace/<environment>.Environment. Publishes only its already-saved staging state; /staging is not a public path suffix. Names resolve uniquely to IDs before approval. Requires explicit native identity and terminal approval of the resolved target, endpoint, account, tenant and Fabric audience. Native write permission and tenant policy remain separately enforced. No input, save, upload, replacement, deployment, replay or remote cancellation. HTTP 202 is accepted, not published. --wait uses the existing finite host budget, including service Retry-After delays.")
        .arg(Arg::new("path").required(true))
        .arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue))
}

fn parse(args: Vec<OsString>) -> Result<Command, clap::Error> {
    let matches = command().try_get_matches_from(args)?;
    let path = matches
        .get_one::<String>("path")
        .cloned()
        .ok_or_else(invalid)?;
    let target = GetTarget::parse(&path).map_err(|_| invalid())?;
    if !matches!(
        &target,
        GetTarget::Item {
            item_type: Some(kind),
            representation: GetRepresentation::Metadata,
            ..
        } if kind == "Environment"
    ) {
        return Err(invalid());
    }
    Ok(Command {
        path,
        target,
        wait: matches.get_flag("wait"),
    })
}

fn invalid() -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::InvalidValue,
        "Publish requires one explicit typed existing Environment item path.",
    )
}

pub(super) fn run(args: Vec<OsString>) -> Result<u8, Failure> {
    let command = match parse(args) {
        Ok(command) => command,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            command()
                .write_long_help(&mut io::stdout().lock())
                .map_err(|_| failed("OutputFailed", "Publish help delivery failed.", 1))?;
            return Ok(0);
        }
        Err(_) => {
            return Err(failed(
                "InvalidArguments",
                "Invalid publish arguments; run publish --help.",
                2,
            ));
        }
    };
    // This is the existing API host policy, not a new publish protocol default.
    let timeout = fabric_sdk_api::v2_policy::Limits::default().timeout;
    let deadline = NativeDeadline::new(timeout)
        .map_err(|_| failed("InvalidConfiguration", "Invalid host deadline.", 2))?;
    if !io::stderr().is_terminal() {
        return Err(failed(
            "ApprovalRequired",
            "An interactive terminal is required for Environment publication approval; no publication was submitted.",
            1,
        ));
    }
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
        .map_err(|_| failed("RuntimeFailed", "Publish runtime could not start.", 1))?;
    let result = runtime.block_on(async {
        let session = NativeSession::new(selection, endpoint, remaining(&deadline)?)?;
        let executor = HttpExecutor::new(
            session.service_http,
            Arc::new(TokioTimer),
            ExecutionOptions {
                timeout: remaining(&deadline)?,
                max_delay: timeout,
            },
        )
        .map_err(|_| {
            failed(
                "InvalidConfiguration",
                "Invalid publish execution limits.",
                2,
            )
        })?
        .with_endpoint(session.context.endpoint().clone());
        let client = Client::with_context(session.context);
        let signal_failed = AtomicBool::new(false);
        let result = dispatch(
            command,
            &client,
            &executor,
            &mut TerminalApproval,
            &deadline,
            async {
                if tokio::signal::ctrl_c().await.is_err() {
                    signal_failed.store(true, Ordering::SeqCst);
                }
            },
        )
        .await;
        if signal_failed.load(Ordering::SeqCst) {
            match result {
                Ok(mut delivery) => {
                    delivery.value["meta"]["localInterruption"] = json!("SignalRegistrationFailed");
                    delivery.exit = 1;
                    Ok(delivery)
                }
                Err(_) => Err(failed("SignalRegistrationFailed", "Publication preflight stopped because local interruption handling is unavailable.", 1)),
            }
        } else {
            result
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    Ok(result?.write_to(&mut io::stdout().lock(), &mut io::stderr().lock()))
}

fn remaining(deadline: &NativeDeadline) -> Result<Duration, Failure> {
    deadline.remaining().map_err(|_| {
        failed(
            "TimedOut",
            "The total publish deadline expired; no replay or remote cancellation was requested.",
            1,
        )
    })
}

trait ApprovalHost {
    fn approve<'a>(
        &'a mut self,
        prepared: &'a PreparedPublish,
    ) -> NativeFuture<'a, Result<bool, Failure>>;
}

struct TerminalApproval;

impl ApprovalHost for TerminalApproval {
    fn approve<'a>(
        &'a mut self,
        prepared: &'a PreparedPublish,
    ) -> NativeFuture<'a, Result<bool, Failure>> {
        let context = prepared.context();
        let question = format!(
            "Publish ONLY already-saved staging (remote write/publication, no revision guard)?\nTarget: {}\nIdentity: {:?}\nEndpoint: {}\nAccount: {}; tenant: {}; audience: Fabric\nApproval does not grant Environment write permission, delegated scope or tenant-policy authorization.",
            prepared
                .operation()
                .target()
                .canonical_path()
                .escape_debug(),
            prepared.operation().target().identity(),
            context.endpoint().host(),
            context.selection().account().escape_debug(),
            context.selection().tenant().escape_debug(),
        );
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                #[cfg(windows)]
                let console = std::fs::File::open("CONIN$");
                #[cfg(not(windows))]
                let console = std::fs::File::open("/dev/tty");
                let console = console.map_err(|_| {
                    failed(
                        "ApprovalRequired",
                        "Publication approval input is unavailable.",
                        1,
                    )
                })?;
                fabric_cli::console::read_explicit_confirmation(
                    &mut io::BufReader::new(console),
                    &mut io::stderr(),
                    &question,
                )
                .map_err(|_| {
                    failed(
                        "ApprovalRequired",
                        "Publication confirmation could not be read or delivered.",
                        1,
                    )
                })
            })
            .await
            .map_err(|_| failed("ApprovalRequired", "Publication approval worker failed.", 1))?
        })
    }
}

struct ImmediatePolicy<'a> {
    operation: &'a PreparedOperation,
    approved: bool,
}

impl ApprovalPolicy for ImmediatePolicy<'_> {
    type Error = Failure;

    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        if !self.approved || !std::ptr::eq(operation, self.operation) {
            return Err(failed(
                "ApprovalDenied",
                "This exact Environment publication was not approved; no publication was submitted.",
                1,
            ));
        }
        self.approved = false;
        Ok(())
    }
}

async fn dispatch(
    command: Command,
    client: &Client,
    executor: &HttpExecutor,
    host: &mut impl ApprovalHost,
    deadline: &NativeDeadline,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<Delivery, Failure> {
    let budget = remaining(deadline)?;
    let interruption = AtomicU8::new(0);
    let mut interrupted = pin!(async {
        tokio::select! {
            _ = tokio::time::sleep(budget) => interruption.store(1, Ordering::SeqCst),
            _ = cancelled => interruption.store(2, Ordering::SeqCst),
        }
    });
    let limits = OperationLimits {
        max_poll_delay: fabric_sdk_api::v2_policy::Limits::default().timeout,
        ..OperationLimits::default()
    };
    let resolved = resolve(
        command.target,
        client,
        executor,
        executor,
        limits.max_response_bytes,
        interrupted.as_mut(),
    )
    .await
    .map_err(|_| {
        failed(
            "TargetResolutionFailed",
            "The exact Environment target could not be resolved completely and uniquely.",
            1,
        )
    })?;
    let ResolvedGetTarget::Item {
        workspace_id,
        item_id,
        ..
    } = resolved
    else {
        return Err(failed(
            "InvalidTarget",
            "An existing Environment is required.",
            2,
        ));
    };
    let target = ResolvedTarget::new(
        &command.path,
        ItemIdentity::new(&workspace_id, &item_id)
            .map_err(|_| failed("InvalidTarget", "Invalid resolved Environment identity.", 1))?,
        Target {
            kind: TargetKind::Item,
            owner_type: "Environment".into(),
            facet: "staging".into(),
            representation: "environment".into(),
            generation: None,
        },
        None,
    )
    .map_err(|_| failed("InvalidTarget", "Invalid resolved Environment target.", 1))?;
    let mut session = OperationSession::with_context(
        client.context(),
        executor.clone(),
        limits,
        interrupted.as_mut(),
    );
    let get = FabricOperation::new(
        ResourceMethod::Get,
        &format!("/v1/workspaces/{workspace_id}/environments/{item_id}"),
        Vec::new(),
        CompletionPolicy::ReturnAccepted,
    )
    .map_err(|_| {
        failed(
            "InvalidTarget",
            "Invalid Environment verification request.",
            1,
        )
    })?;
    let response = session.execute(get).await.map_err(|_| {
        failed(
            "TargetVerificationFailed",
            "Existing Environment verification failed; no publication was submitted.",
            1,
        )
    })?;
    drop(session);
    let value: Value = serde_json::from_slice(&response.body).map_err(|_| {
        failed(
            "TargetVerificationFailed",
            "Malformed Environment verification; no publication was submitted.",
            1,
        )
    })?;
    if response.status != 200
        || value["type"] != "Environment"
        || !value["id"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(&item_id))
        || !value["workspaceId"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(&workspace_id))
    {
        return Err(failed(
            "TargetMismatch",
            "The service did not verify this exact existing Environment; no publication was submitted.",
            1,
        ));
    }
    let prepared = environment_publish::prepare(
        client,
        target.clone(),
        PublishSource::ExistingStaging,
        if command.wait {
            ObservationPolicy::WaitForOperation
        } else {
            ObservationPolicy::ReturnAccepted
        },
    )
    .map_err(|_| {
        failed(
            "InvalidPublication",
            "Environment publication preparation failed.",
            1,
        )
    })?;
    let approved = tokio::select! {
        biased;
        () = interrupted.as_mut() => return Err(failed("Interrupted", "Publication preflight stopped; no publication was submitted.", 1)),
        approval = host.approve(&prepared) => approval?,
    };
    remaining(deadline)?;
    let grant = prepared.authorize(&mut ImmediatePolicy {
        operation: prepared.operation(),
        approved,
    })?;
    let execution = environment_publish::publish(
        client,
        executor.clone(),
        grant,
        limits,
        interrupted.as_mut(),
    )
    .await;
    let execution = match execution {
        Ok(execution) => execution,
        Err(error) => return receipt_error(error),
    };
    let mut delivery = Delivery::new(&execution)?;
    if command.wait
        && let Outcome::Partial {
            data: PublishData::Native(publication),
            ..
        } = execution.receipt.outcome()
        && matches!(
            publication.state,
            PublishState::Waiting | PublishState::Running | PublishState::Cancelling
        )
        && let Some(version) = &publication.target_version
    {
        let initial_delay = execution
            .response
            .as_ref()
            .ok_or_else(|| {
                failed(
                    "InvalidPublication",
                    "Native publication response was not retained.",
                    1,
                )
            })
            .and_then(|response| {
                polling::retry_after(&response.headers, Duration::from_secs(1), false).map_err(
                    |_| {
                        failed(
                            "InvalidRetryAfter",
                            "Native publication advertised an invalid observation delay.",
                            1,
                        )
                    },
                )
            });
        let observed = match initial_delay {
            Ok(delay) => {
                let mut driver = NativeObserver {
                    client,
                    executor,
                    target: &target,
                    version,
                    limits,
                    cancelled: interrupted.as_mut(),
                    requests: 0,
                    last: None,
                };
                let result = polling::drive(&mut driver, Some(delay)).await;
                delivery.value["meta"]["lastNativeObservation"] = json!(driver.last);
                result
            }
            Err(error) => Err(error),
        };
        delivery.observe(observed);
    }
    match interruption.load(Ordering::SeqCst) {
        1 => delivery.value["meta"]["localInterruption"] = json!("TimedOut"),
        2 => delivery.value["meta"]["localInterruption"] = json!("Cancelled"),
        _ => (),
    }
    Ok(delivery)
}

struct NativeObserver<'a> {
    client: &'a Client,
    executor: &'a HttpExecutor,
    target: &'a ResolvedTarget,
    version: &'a PublishVersion,
    limits: OperationLimits,
    cancelled: Pin<&'a mut (dyn Future<Output = ()> + Send + 'a)>,
    requests: usize,
    last: Option<environment_publish::NativePublication>,
}

impl ObservationDriver for NativeObserver<'_> {
    type Output = NativeObservation;
    type Error = Failure;

    fn step(&mut self) -> ObserverFuture<'_, Result<ObservationStep<Self::Output>, Failure>> {
        Box::pin(async move {
            if self.requests >= self.limits.max_requests.get() {
                return Err(failed(
                    "ObservationLimit",
                    "Environment observation request bound reached.",
                    1,
                ));
            }
            self.requests += 1;
            let observation = environment_publish::observe_version(
                self.client, self.executor.clone(), self.target, self.version, self.limits, self.cancelled.as_mut(),
            )
            .await
            .map_err(|error| {
                let mut failure = failed("ObservationIncomplete", "Environment observation failed or its original target version changed; publication may still be active.", 1);
                failure.2 = json!({
                    "responseStatus":error.response.as_ref().map(|response| response.status),
                    "observedVersion":error.response.as_ref().and_then(|response| serde_json::from_slice::<Value>(&response.body).ok()).and_then(|value| value["properties"]["publishDetails"]["targetVersion"].as_str().map(str::to_owned)),
                });
                failure
            })?;
            self.last = Some(observation.publication.clone());
            if observation.succeeded
                || matches!(
                    observation.publication.state,
                    PublishState::Failed | PublishState::Cancelled
                )
            {
                return Ok(ObservationStep::Complete(observation));
            }
            if observation.publication.state == PublishState::Success {
                return Err(failed(
                    "ObservationIncomplete",
                    "Environment component states do not establish complete publication.",
                    1,
                ));
            }
            let delay =
                polling::retry_after(&observation.response.headers, Duration::from_secs(1), false)
                    .map_err(|_| {
                        failed(
                            "InvalidRetryAfter",
                            "Environment observation advertised an invalid delay.",
                            1,
                        )
                    })?;
            Ok(ObservationStep::Continue(delay))
        })
    }

    fn wait(&mut self, delay: Duration) -> ObserverFuture<'_, Result<(), Failure>> {
        Box::pin(async move {
            tokio::select! {
                biased;
                () = self.cancelled.as_mut() => Err(failed("Interrupted", "Local Environment observation stopped; remote work was not cancelled or replayed.", 1)),
                result = self.executor.wait(delay) => result.map_err(|_| failed("ObservationIncomplete", "Environment observation delay exceeded the finite host budget.", 1)),
            }
        })
    }
}

struct Delivery {
    value: Value,
    exit: u8,
}

impl Delivery {
    fn new(execution: &PublishExecution) -> Result<Self, Failure> {
        let submission = serde_json::to_value(&execution.receipt).map_err(|_| {
            failed(
                "OutputFailed",
                "Publication evidence could not be serialized; do not replay publication.",
                1,
            )
        })?;
        let mut value = submission.clone();
        value["meta"]["submission"] = submission;
        value["meta"]["initialHttpStatus"] = json!(
            execution
                .initial_response
                .as_ref()
                .map(|response| response.status)
        );
        value["meta"]["remoteCancelled"] = json!(false);
        value["meta"]["remoteReplayed"] = json!(false);
        value["meta"]["rollback"] = json!(false);
        let exit = if matches!(
            execution.receipt.outcome(),
            Outcome::Succeeded { .. } | Outcome::Accepted
        ) {
            0
        } else {
            1
        };
        Ok(Self { value, exit })
    }

    fn observe(&mut self, result: Result<NativeObservation, Failure>) {
        match result {
            Ok(observation) => {
                self.value["meta"]["evidence"]["coverage"] = json!({"kind":"complete"});
                self.value["meta"]["sources"] = json!([{
                    "source":"environment.staging.publish.observation",
                    "httpStatus":observation.response.status,
                    "coverage":{"kind":"complete"},
                    "error":if observation.succeeded { Value::Null } else { json!({"code":"NativePublicationFailed","message":"The exact observed publication failed or was cancelled."}) },
                    "details":{"nativeState":observation.publication.state,"targetVersion":observation.publication.target_version}
                }]);
                self.value["status"] = json!(if observation.succeeded {
                    "succeeded"
                } else {
                    "failed"
                });
                self.value["data"] = json!(PublishData::Native(observation.publication));
                self.value["error"] = if observation.succeeded {
                    Value::Null
                } else {
                    json!({"code":"NativePublicationFailed","message":"The exact native publication failed or was cancelled; rollback is not established."})
                };
                self.value["meta"]["observationCoverage"] = json!({"kind":"complete"});
                self.exit = u8::from(!observation.succeeded);
            }
            Err((code, message, evidence, _)) => {
                self.value["status"] = json!("unknown");
                self.value["data"] = Value::Null;
                self.value["error"] = json!({"code":code,"message":message});
                self.value["meta"]["observationEvidence"] = evidence;
                self.exit = 1;
            }
        }
    }

    fn write_to(&self, stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
        let write = |stream: &mut dyn Write, value: &Value| -> io::Result<()> {
            serde_json::to_writer(&mut *stream, value).map_err(io::Error::other)?;
            writeln!(stream)?;
            stream.flush()
        };
        let delivered = if self.exit == 0 {
            write(stdout, &self.value)
        } else {
            write(stderr, &self.value)
        };
        if delivered.is_ok() {
            return self.exit;
        }
        let mut retained = self.value.clone();
        retained["meta"]["delivery"] = json!({"code":"OutputFailure","message":"Publication receipt delivery failed. Retain the original outcome and handles; do not replay publication."});
        let _fallback = write(stderr, &retained);
        1
    }
}

fn receipt_error(error: PublishError) -> Result<Delivery, Failure> {
    let evidence = match error {
        PublishError::Input(_) => {
            return Err(failed(
                "InvalidPublication",
                "Publication context or approval changed; no publication was submitted.",
                1,
            ));
        }
        PublishError::Receipt(error) => json!({"outcome":error.outcome,"evidence":error.evidence}),
        PublishError::Source(error) => json!({"receipt":error.receipt,"sources":error.sources}),
    };
    Ok(Delivery {
        value: json!({"schemaVersion":"0.1","status":"unknown","data":null,"meta":{"retained":evidence,"remoteCancelled":false,"remoteReplayed":false,"rollback":false},"error":{"code":"InvalidPublicationReceipt","message":"Publication evidence could not be validated; do not replay publication."}}),
        exit: 1,
    })
}
