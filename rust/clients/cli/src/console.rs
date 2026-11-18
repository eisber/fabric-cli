// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use fabric_cli_api::ApiReporting;
use fabric_cli_command_contracts::{CommandDiagnostics, CommandError};
use fabric_cli_logging as diagnostics;
use fabric_cli_polling::{ProgressUpdate, progress_error};
use fabric_core::{
    CancellationToken, Clock, CoreError, ErrorKind, OperationContext, OperationKind, RecoveryInfo,
    bounded_with_clock,
    diagnostics::{
        DiagnosticDuration, DiagnosticEvent, DiagnosticObserver, DiagnosticOutcome,
        DiagnosticPhase, DiagnosticStage,
    },
};
use fabric_sdk_operations::{PollObserver, PollProgress};
use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{mpsc, oneshot};

pub(super) struct ProgressEvent {
    pub(super) progress: ConsoleUpdate,
    pub(super) written: oneshot::Sender<Result<(), CoreError>>,
}

pub(super) enum ConsoleUpdate {
    Progress(ProgressUpdate),
    Diagnostic(Box<DiagnosticEvent>, SystemTime),
}

pub(super) struct ConsoleProgress {
    pub(super) events: mpsc::Sender<ProgressEvent>,
    pub(super) failed: CancellationToken,
    pub(super) broken: AtomicBool,
    pub(super) timing: Option<(Arc<dyn Clock>, Instant)>,
    pub(super) cancellation: CancellationToken,
    pub(super) heartbeat_clock: OnceLock<Arc<dyn Clock>>,
    pub(super) reporting: Mutex<ReportingState>,
}

pub(super) struct ReportingState {
    pub(super) outcome: DiagnosticOutcome,
    pub(super) recovery: Option<RecoveryInfo>,
    pub(super) completed: bool,
}
impl ConsoleProgress {
    async fn send(&self, progress: ProgressUpdate) -> Result<(), CoreError> {
        self.send_update(ConsoleUpdate::Progress(progress)).await
    }

    async fn send_update(&self, progress: ConsoleUpdate) -> Result<(), CoreError> {
        let diagnostic = matches!(&progress, ConsoleUpdate::Diagnostic(..));
        let error = || {
            if diagnostic {
                diagnostics::error()
            } else {
                progress_error()
            }
        };
        if self.broken.load(Ordering::Relaxed) {
            return Err(error());
        }
        let (written, received) = oneshot::channel();
        self.events
            .send(ProgressEvent { progress, written })
            .await
            .map_err(|_| error())?;
        received.await.map_err(|_| error())?
    }

    async fn emit(&self, event: DiagnosticEvent) -> Result<(), CoreError> {
        let Some((clock, _)) = &self.timing else {
            return Ok(());
        };
        self.send_update(ConsoleUpdate::Diagnostic(Box::new(event), clock.wall_now()))
            .await
    }
}

impl CommandDiagnostics for ConsoleProgress {
    fn elapsed(&self) -> Duration {
        self.timing
            .as_ref()
            .map_or(Duration::ZERO, |(clock, start)| {
                clock.now().saturating_duration_since(*start)
            })
    }

    async fn stage(
        &self,
        stage: DiagnosticStage,
        phase: DiagnosticPhase,
        duration: Duration,
    ) -> Result<(), CommandError> {
        let Some((clock, _)) = &self.timing else {
            return Ok(());
        };
        let context = OperationContext {
            deadline: None,
            cancellation: self.cancellation.clone(),
        };
        bounded_with_clock(
            clock.as_ref(),
            &context,
            self.emit(DiagnosticEvent::Stage {
                stage,
                phase,
                duration: DiagnosticDuration::from_duration(duration),
            }),
        )
        .await?
        .map_err(|error| {
            if error.kind == ErrorKind::Diagnostic {
                self.reporting_error()
            } else {
                error.into()
            }
        })
    }
}

impl ApiReporting for ConsoleProgress {
    fn set_poll_clock(&self, clock: Arc<dyn Clock>) -> Result<(), CoreError> {
        self.heartbeat_clock
            .set(clock)
            .map_err(|_| CoreError::new(ErrorKind::Protocol, "Progress clock already set"))
    }

    fn diagnostics_enabled(&self) -> bool {
        self.timing.is_some()
    }

    fn set_recovery(&self, recovery: Option<RecoveryInfo>) {
        self.reporting
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recovery = recovery;
    }

    fn recovery(&self) -> Option<RecoveryInfo> {
        self.reporting
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recovery
            .clone()
    }

    async fn progress_failure(&self) -> CoreError {
        self.failed.cancelled().await;
        progress_error()
    }

    async fn finish_progress(&self) -> Result<(), CoreError> {
        self.send(ProgressUpdate::Finished).await
    }
}

impl ConsoleProgress {
    pub(super) fn remember(&self, event: &DiagnosticEvent, written: bool) {
        let mut state = self
            .reporting
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match event {
            DiagnosticEvent::HttpSend { .. } if written => {
                if !matches!(state.outcome, DiagnosticOutcome::Terminal(_)) {
                    state.outcome = DiagnosticOutcome::Uncertain;
                }
            }
            DiagnosticEvent::HttpResponse {
                status: Some(status),
                ..
            }
            | DiagnosticEvent::HttpFailure {
                status: Some(status),
                ..
            } => {
                if !matches!(state.outcome, DiagnosticOutcome::Terminal(_)) {
                    state.outcome = DiagnosticOutcome::Received(*status);
                }
            }
            DiagnosticEvent::PollCompleted { outcome } => {
                state.outcome = DiagnosticOutcome::Terminal(*outcome);
                if let Some(recovery) = &mut state.recovery {
                    recovery.outcome_uncertain = false;
                }
            }
            _ => {}
        }
    }

    pub(super) fn reporting_error(&self) -> CommandError {
        let state = self
            .reporting
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut error: CommandError = CoreError::diagnostic_failure(state.outcome).into();
        if state.completed {
            error.message = format!("Command work completed. {}", error.message);
        }
        error.recovery = state
            .recovery
            .as_ref()
            .and_then(|value| serde_json::to_value(value).ok());
        error
    }
}

#[async_trait]
impl DiagnosticObserver for ConsoleProgress {
    async fn observe(&self, event: DiagnosticEvent) -> Result<(), CoreError> {
        self.emit(event).await
    }
}

#[async_trait]
impl PollObserver for ConsoleProgress {
    async fn accepted(&self, kind: OperationKind, _: u16) -> Result<(), CoreError> {
        self.send(ProgressUpdate::Accepted(kind)).await
    }

    async fn waiting(&self, _: OperationKind, delay: Duration) -> Result<(), CoreError> {
        self.send(ProgressUpdate::Waiting(delay)).await
    }

    async fn polling(&self, kind: OperationKind) -> Result<(), CoreError> {
        self.send(ProgressUpdate::Polling(kind)).await
    }

    async fn observe(&self, progress: PollProgress) -> Result<(), CoreError> {
        self.send(ProgressUpdate::Status(progress)).await
    }

    async fn diagnostic(&self, event: DiagnosticEvent) -> Result<(), CoreError> {
        self.remember(&event, false);
        self.emit(event).await
    }
}
