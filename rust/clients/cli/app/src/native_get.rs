// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native GET capabilities and one-shot delivery on the caller's runtime.

#![cfg_attr(
    not(any(windows, feature = "headless", feature = "interactive")),
    allow(dead_code)
)]

use super::{CommandOutcome, Failure, NativeTransferUi};
use fabric_cli::{
    parser::ReadCommand,
    session::{
        ReadSession,
        get_host::{
            ClockedGetHost, GetDispatch, GetErrorKind, GetEvidence, GetFailure, GetResourceConfig,
            GetWriteFailure,
            output::{GetOutputClock, LocalTimestamp},
        },
    },
};
use fabric_cli_output::{CommandError, OutputStream, RenderPlan, text::TextPlan};
use fabric_cli_transfer::{TransferFiles, TransferFuture, TransferUi};
use fabric_deploy::definitions::{
    DefinitionEntry, DefinitionFile, DefinitionFileSystem, DefinitionImportFileSystem,
    DefinitionLimits, GetOutputNewline,
};
use fabric_sdk_http::HttpExecutor;
use fabric_sdk_operations::{OperationError, OperationErrorKind, OperationExecutionError};
use serde_json::json;
use std::{
    fmt,
    future::Future,
    io::{self, Write},
    path::{Path, PathBuf},
    pin::Pin,
    time::Duration,
};
use tokio::runtime::Handle;

pub(super) const DRAIN_LIMIT: Duration = Duration::from_secs(2);
const LOCAL_WORK_LIMIT: Duration = Duration::from_secs(30);

pub(super) async fn controlled(
    start: impl AsyncFnOnce(
        &mut GetEvidence,
        Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Result<Delivery, Failure>,
    interrupted: impl Future<Output = Failure>,
) -> Result<Delivery, Failure> {
    let (signal, mut receiver) = tokio::sync::watch::channel(false);
    let cancelled = Box::pin(async move {
        while !*receiver.borrow() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    });
    let mut evidence = GetEvidence::default();
    let mut work = Box::pin(start(&mut evidence, cancelled));
    let failure = tokio::select! {
        biased;
        failure = interrupted => failure,
        result = work.as_mut() => return result,
    };
    let _ = signal.send(true);
    let drained = tokio::time::timeout(DRAIN_LIMIT, work.as_mut()).await;
    drop(work);
    match drained {
        Ok(Ok(delivery)) => Ok(delivery.interrupted(failure)),
        Ok(Err(_)) => Ok(Delivery::abandoned(evidence, failure)),
        Err(_) => {
            let (code, message, exit) = failure;
            Ok(Delivery::abandoned(
                evidence,
                (
                    code,
                    format!(
                        "{message} The bounded GET drain expired; other in-flight work may still complete."
                    ),
                    exit,
                ),
            ))
        }
    }
}

pub(super) struct Files<'a> {
    native: bool,
    injected: Option<&'a mut dyn TransferFiles>,
}

impl Files<'_> {
    pub(super) fn native() -> Self {
        Self {
            native: true,
            injected: None,
        }
    }
}

#[cfg(test)]
impl<'a> Files<'a> {
    fn injected(files: Option<&'a mut dyn TransferFiles>) -> Self {
        Self {
            native: false,
            injected: files,
        }
    }

    fn reborrow(&mut self) -> Files<'_> {
        Files {
            native: self.native,
            injected: self
                .injected
                .as_mut()
                .map(|files| &mut **files as &mut dyn TransferFiles),
        }
    }
}

pub(super) struct Inputs<'a> {
    pub(super) evidence: &'a mut GetEvidence,
    pub(super) cancelled: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    pub(super) clock: &'a dyn GetOutputClock,
    pub(super) ui: &'a dyn TransferUi,
    pub(super) files: Files<'a>,
    pub(super) resource_config: GetResourceConfig,
}

pub(super) struct NativeClock(pub(super) Handle);

impl GetOutputClock for NativeClock {
    fn local_timestamp(&self) -> TransferFuture<'_, Result<LocalTimestamp, CommandError>> {
        Box::pin(async move {
            let work = self.0.spawn_blocking(|| {
                std::panic::catch_unwind(|| {
                    chrono::Local::now().format("%Y%m%d_%H%M%S").to_string()
                })
            });
            let text = tokio::time::timeout(LOCAL_WORK_LIMIT, work)
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(Result::ok)
                .ok_or_else(|| {
                    CommandError::new(
                        Some("The local GET output clock did not complete".into()),
                        Some("ClockFailed".into()),
                    )
                })?;
            LocalTimestamp::parse(&text)
        })
    }
}

pub(super) enum Delivery {
    Plan {
        plan: TextPlan,
        outcome: CommandOutcome,
    },
    Get {
        dispatch: Box<GetDispatch>,
        outcome: CommandOutcome,
        prefix: usize,
    },
}

impl Delivery {
    pub(super) fn plan(plan: TextPlan, outcome: CommandOutcome) -> Self {
        Self::Plan { plan, outcome }
    }

    fn get(dispatch: GetDispatch) -> Self {
        let outcome = get_outcome(&dispatch);
        Self::Get {
            dispatch: Box::new(dispatch),
            outcome,
            prefix: 0,
        }
    }

    pub(super) fn prepend(&mut self, mut notice: TextPlan) {
        match self {
            Self::Plan { plan, .. } => {
                notice.records.append(&mut plan.records);
                plan.records = notice.records;
            }
            Self::Get {
                dispatch, prefix, ..
            } => {
                *prefix += notice.records.len();
                notice.records.append(&mut dispatch.plan.records);
                dispatch.plan.records = notice.records;
            }
        }
    }

    #[cfg(all(test, feature = "headless"))]
    pub(super) fn into_plan_parts(self) -> (TextPlan, CommandOutcome) {
        match self {
            Self::Plan { plan, outcome } => (plan, outcome),
            Self::Get { .. } => panic!("GET tests must retain the owned native delivery"),
        }
    }

    pub(super) fn write_to(
        self,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> io::Result<CommandOutcome> {
        match self {
            Self::Plan { plan, outcome } => {
                plan.write_to(stdout, stderr)?;
                Ok(outcome)
            }
            Self::Get {
                dispatch, outcome, ..
            } => (*dispatch)
                .write_to(stdout, stderr)
                .map(|_| outcome)
                .map_err(|failure| {
                    io::Error::new(failure.error.kind(), DeliveryWriteFailure(failure))
                }),
        }
    }

    pub(super) fn interrupted(mut self, failure: Failure) -> Self {
        let (code, message, exit) = failure;
        if let Self::Get {
            dispatch,
            outcome,
            prefix,
        } = &mut self
        {
            if !matches!(
                &dispatch.result,
                Err(error) if matches!(error.kind, GetErrorKind::Render { .. })
            ) {
                dispatch.plan.records.truncate(*prefix);
            }
            dispatch
                .plan
                .records
                .push(interruption_record(code, message, exit));
            dispatch.plan.exit_code = exit;
            *outcome = super::failure_outcome(code, exit);
        }
        self
    }

    pub(super) fn abandoned(evidence: GetEvidence, failure: Failure) -> Self {
        let (code, message, exit) = failure;
        let dispatch = GetDispatch {
            plan: TextPlan {
                records: vec![interruption_record(code, message, exit)],
                exit_code: exit,
            },
            result: Err(Box::new(GetFailure {
                kind: GetErrorKind::Command(CommandError::new(
                    Some("GET waiting stopped; retained effects may be incomplete".into()),
                    Some(code.into()),
                )),
                evidence,
            })),
        };
        Self::Get {
            dispatch: Box::new(dispatch),
            outcome: super::failure_outcome(code, exit),
            prefix: 0,
        }
    }
}

fn interruption_record(code: &str, message: String, exit: u8) -> RenderPlan {
    let mut payload = serde_json::to_vec(&json!({"error":{"code":code,"message":message}}))
        .expect("fixed JSON diagnostic shape");
    payload.push(b'\n');
    RenderPlan {
        stream: OutputStream::Stderr,
        exit_code: exit,
        payload,
    }
}

#[derive(Debug)]
pub(super) struct DeliveryWriteFailure(pub(super) Box<GetWriteFailure>);

impl fmt::Display for DeliveryWriteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Writing native GET output failed; inspect retained GET effects")
    }
}

impl std::error::Error for DeliveryWriteFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0.error)
    }
}

fn operation_cancelled(error: &OperationError) -> bool {
    matches!(
        &error.kind,
        OperationErrorKind::Cancelled
            | OperationErrorKind::Execution(OperationExecutionError::Cancelled)
    ) || matches!(
        &error.kind,
        OperationErrorKind::OperationFailed { status, .. } if status == "Cancelled"
    )
}

fn error_outcome(kind: &GetErrorKind, exit: u8) -> CommandOutcome {
    use fabric_deploy::definitions::DefinitionErrorKind;
    use fabric_sdk::{
        items::{ItemErrorKind, ItemExecutionError},
        workspaces::{WorkspaceExecutionError, WorkspaceListErrorKind, WorkspaceMetadataErrorKind},
    };
    match kind {
        GetErrorKind::Command(error) => {
            let code = error.envelope(
                fabric_cli_output::Timestamp::parse("1970-01-01T00:00:00Z").unwrap(),
                None,
            );
            if matches!(
                code.result().error_code(),
                Some(
                    "LocalIOError"
                        | "OutputError"
                        | "ClockFailed"
                        | "ConfirmationInputFailed"
                        | "ConfirmationWorkerFailed"
                )
            ) {
                CommandOutcome::Stop(exit)
            } else {
                crate::batch_state::error_outcome(error, exit)
            }
        }
        GetErrorKind::Operation(error) if operation_cancelled(error) => {
            CommandOutcome::Cancelled(exit)
        }
        GetErrorKind::Api(error) if api_cancelled(error) => CommandOutcome::Cancelled(exit),
        GetErrorKind::Collection(error) if api_cancelled(&error.error) => {
            CommandOutcome::Cancelled(exit)
        }
        GetErrorKind::OneLake(fabric_sdk_onelake::OneLakeError::Cancelled) => {
            CommandOutcome::Cancelled(exit)
        }
        GetErrorKind::Resource(error)
            if matches!(
                error.code.as_str(),
                "Cancelled" | "LongRunningOperationCancelled"
            ) =>
        {
            CommandOutcome::Cancelled(exit)
        }
        GetErrorKind::Definition(error) => match &error.kind {
            DefinitionErrorKind::Operation(error) if operation_cancelled(error) => {
                CommandOutcome::Cancelled(exit)
            }
            DefinitionErrorKind::FileSystem(_) => CommandOutcome::Stop(exit),
            _ => CommandOutcome::Returned(exit),
        },
        GetErrorKind::Item(error)
            if matches!(
                error.kind,
                ItemErrorKind::Cancelled | ItemErrorKind::Execution(ItemExecutionError::Cancelled)
            ) =>
        {
            CommandOutcome::Cancelled(exit)
        }
        GetErrorKind::Workspace(error)
            if matches!(
                error.kind,
                WorkspaceMetadataErrorKind::Read(
                    WorkspaceListErrorKind::Cancelled
                        | WorkspaceListErrorKind::Execution(WorkspaceExecutionError::Cancelled)
                )
            ) =>
        {
            CommandOutcome::Cancelled(exit)
        }
        GetErrorKind::Render { .. } => CommandOutcome::Stop(exit),
        _ => CommandOutcome::Returned(exit),
    }
}

fn api_cancelled(error: &fabric_sdk_api::ApiError) -> bool {
    matches!(
        error,
        fabric_sdk_api::ApiError::Cancelled
            | fabric_sdk_api::ApiError::Execution(fabric_sdk_api::ExecutionError::Cancelled)
    ) || matches!(
        error,
        fabric_sdk_api::ApiError::Operation(error) if operation_cancelled(error)
    )
}

fn get_outcome(dispatch: &GetDispatch) -> CommandOutcome {
    match &dispatch.result {
        Ok(_) => CommandOutcome::Returned(dispatch.plan.exit_code),
        Err(failure) => error_outcome(&failure.kind, dispatch.plan.exit_code),
    }
}

pub(super) async fn dispatch(
    reads: &ReadSession<'_>,
    http: &HttpExecutor,
    command: &ReadCommand,
    inputs: Option<Inputs<'_>>,
) -> Delivery {
    if let Some(inputs) = inputs {
        return dispatch_selected(reads, http, command, inputs).await;
    }
    let clock = NativeClock(Handle::current());
    let mut evidence = GetEvidence::default();
    dispatch_selected(
        reads,
        http,
        command,
        Inputs {
            evidence: &mut evidence,
            cancelled: Box::pin(std::future::pending()),
            clock: &clock,
            ui: &NativeTransferUi,
            files: Files::native(),
            resource_config: GetResourceConfig::default(),
        },
    )
    .await
}

async fn dispatch_selected(
    reads: &ReadSession<'_>,
    http: &HttpExecutor,
    command: &ReadCommand,
    mut inputs: Inputs<'_>,
) -> Delivery {
    let mut native = command
        .output
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| LocalFiles::new(value.to_owned(), Handle::current()));
    let files = if inputs.files.native {
        native.as_mut().map(|files| files as &mut dyn TransferFiles)
    } else {
        inputs
            .files
            .injected
            .as_mut()
            .map(|files| &mut **files as &mut dyn TransferFiles)
    };
    let mut host = ClockedGetHost {
        operations: http,
        ui: inputs.ui,
        files,
        clock: inputs.clock,
        newline: if cfg!(windows) {
            GetOutputNewline::CrLf
        } else {
            GetOutputNewline::Lf
        },
        definition_limits: DefinitionLimits::default(),
        operation_limits: Default::default(),
    };
    Delivery::get(
        reads
            .dispatch_get_with_resource_storage(
                command,
                http,
                &mut host,
                inputs.evidence,
                &inputs.resource_config,
                inputs.cancelled,
            )
            .await,
    )
}

struct LocalFiles {
    input: String,
    runtime: Handle,
    selected: Option<fabric_cli::families::transfer_files::TokioTransferFiles>,
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "GET requires its selected asynchronous file capability",
    )
}

async fn bounded<T>(work: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    tokio::time::timeout(LOCAL_WORK_LIMIT, work)
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "Local GET output work timed out; effects may be incomplete",
            )
        })?
}

impl LocalFiles {
    fn new(input: String, runtime: Handle) -> Self {
        Self {
            input,
            runtime,
            selected: None,
        }
    }

    fn selected(
        &mut self,
    ) -> io::Result<&mut fabric_cli::families::transfer_files::TokioTransferFiles> {
        self.selected.as_mut().ok_or_else(unavailable)
    }
}

fn local_candidate(input: &str, current: &Path) -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        Ok(super::transfer_paths::candidate(
            input,
            current,
            super::transfer_paths::home_from_environment().as_deref(),
            std::env::var_os("USERNAME").as_deref(),
        ))
    }
    #[cfg(not(windows))]
    {
        if input == "~" || input.starts_with("~/") {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "Local GET output requires an absolute HOME for tilde expansion",
                    )
                })?;
            return Ok(home.join(input[1..].trim_start_matches('/')));
        }
        if input.starts_with('~') {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Named-user tilde output is unsupported on this platform",
            ));
        }
        let path = PathBuf::from(input);
        Ok(if path.is_absolute() {
            path
        } else {
            current.join(path)
        })
    }
}

impl DefinitionFileSystem for LocalFiles {
    fn preflight(&mut self, _: &Path, _: &[DefinitionFile]) -> io::Result<()> {
        Err(unavailable())
    }
    fn create_directory(&mut self, _: &Path, _: &str) -> io::Result<()> {
        Err(unavailable())
    }
    fn write(&mut self, _: &Path, _: &DefinitionFile) -> io::Result<()> {
        Err(unavailable())
    }
}

impl DefinitionImportFileSystem for LocalFiles {
    fn entries(&mut self, _: &Path, _: &str) -> io::Result<Vec<DefinitionEntry>> {
        Err(unavailable())
    }
    fn read(&mut self, _: &Path, _: &str, _: usize) -> io::Result<Vec<u8>> {
        Err(unavailable())
    }
}

impl TransferFiles for LocalFiles {
    fn resolve_directory(&mut self, _: &str) -> io::Result<PathBuf> {
        Err(unavailable())
    }
    fn is_empty(&mut self, _: &Path) -> io::Result<bool> {
        Err(unavailable())
    }

    fn resolve_export_directory_async<'a>(
        &'a mut self,
        input: &'a str,
    ) -> TransferFuture<'a, io::Result<PathBuf>> {
        Box::pin(async move {
            if input != self.input {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "GET output selection changed",
                ));
            }
            if self.selected.is_none() {
                let current = bounded(async {
                    self.runtime
                        .spawn_blocking(std::env::current_dir)
                        .await
                        .map_err(io::Error::other)?
                })
                .await?;
                let candidate = local_candidate(input, &current)?;
                self.selected = Some(
                    fabric_cli::families::transfer_files::TokioTransferFiles::defer_export_root(
                        candidate,
                        input.into(),
                        DefinitionLimits::default(),
                        self.runtime.clone(),
                    ),
                );
            }
            bounded(self.selected()?.resolve_export_directory_async(input)).await
        })
    }

    fn preflight_async<'a>(
        &'a mut self,
        root: &'a Path,
        files: Vec<DefinitionFile>,
    ) -> TransferFuture<'a, io::Result<Vec<DefinitionFile>>> {
        Box::pin(async move { bounded(self.selected()?.preflight_async(root, files)).await })
    }
    fn create_directory_async<'a>(
        &'a mut self,
        root: &'a Path,
        name: &'a str,
    ) -> TransferFuture<'a, io::Result<()>> {
        Box::pin(async move { bounded(self.selected()?.create_directory_async(root, name)).await })
    }
    fn write_async<'a>(
        &'a mut self,
        root: &'a Path,
        file: DefinitionFile,
    ) -> TransferFuture<'a, io::Result<()>> {
        Box::pin(async move { bounded(self.selected()?.write_async(root, file)).await })
    }
}

#[cfg(test)]
#[path = "native_get_tests.rs"]
mod tests;
