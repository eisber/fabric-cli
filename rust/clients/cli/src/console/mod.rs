// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod diagnostics;
mod progress;

use crate::Host;
use async_trait::async_trait;
use fabric_cli_command_contracts::{CommandDiagnostics, CommandError};
use fabric_core::{
    CancellationToken, Clock, CoreError, ErrorKind, OperationContext, OperationKind, RecoveryInfo,
    bounded_with_clock,
    diagnostics::{
        DiagnosticDuration, DiagnosticEvent, DiagnosticObserver, DiagnosticOutcome,
        DiagnosticPhase, DiagnosticStage,
    },
};
use fabric_sdk_operations::{PollObserver, PollProgress};
use progress::{
    PROGRESS_INTERVAL, ProgressDisplay, ProgressUpdate, progress_error, progress_heartbeat,
};
use std::{
    io::Write,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{mpsc, oneshot};

pub(super) struct ProgressEvent {
    progress: ConsoleUpdate,
    written: oneshot::Sender<Result<(), CoreError>>,
}

enum ConsoleUpdate {
    Progress(ProgressUpdate),
    Diagnostic(Box<DiagnosticEvent>, SystemTime),
}

pub(super) struct ConsoleProgress {
    events: mpsc::Sender<ProgressEvent>,
    failed: CancellationToken,
    broken: AtomicBool,
    timing: Option<(Arc<dyn Clock>, Instant)>,
    cancellation: CancellationToken,
    heartbeat_clock: OnceLock<Arc<dyn Clock>>,
    reporting: Mutex<ReportingState>,
}

struct ReportingState {
    outcome: DiagnosticOutcome,
    recovery: Option<RecoveryInfo>,
    completed: bool,
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

impl ConsoleProgress {
    pub(super) fn set_poll_clock(&self, clock: Arc<dyn Clock>) -> Result<(), CoreError> {
        self.heartbeat_clock
            .set(clock)
            .map_err(|_| CoreError::new(ErrorKind::Protocol, "Progress clock already set"))
    }

    pub(super) fn diagnostics_enabled(&self) -> bool {
        self.timing.is_some()
    }

    pub(super) fn set_recovery(&self, recovery: Option<RecoveryInfo>) {
        self.reporting
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recovery = recovery;
    }

    pub(super) fn recovery(&self) -> Option<RecoveryInfo> {
        self.reporting
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recovery
            .clone()
    }

    pub(super) async fn progress_failure(&self) -> CoreError {
        self.failed.cancelled().await;
        progress_error()
    }

    pub(super) async fn finish_progress(&self) -> Result<(), CoreError> {
        self.send(ProgressUpdate::Finished).await
    }
}

impl ConsoleProgress {
    fn remember(&self, event: &DiagnosticEvent, written: bool) {
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

    fn reporting_error(&self) -> CommandError {
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

pub(super) fn observe(
    host: &dyn Host,
    verbose: bool,
    cancellation: CancellationToken,
) -> (Arc<ConsoleProgress>, mpsc::Receiver<ProgressEvent>) {
    let (sender, events) = mpsc::channel(1);
    let observer = Arc::new(ConsoleProgress {
        events: sender,
        failed: CancellationToken::new(),
        broken: AtomicBool::new(false),
        timing: verbose.then(|| {
            let clock = host.clock();
            let start = clock.now();
            (clock, start)
        }),
        cancellation,
        heartbeat_clock: OnceLock::new(),
        reporting: Mutex::new(ReportingState {
            outcome: DiagnosticOutcome::BeforeSend,
            recovery: None,
            completed: false,
        }),
    });
    (observer, events)
}

pub(super) async fn drive<T>(
    host: &dyn Host,
    stderr: &mut dyn Write,
    observer: &ConsoleProgress,
    mut events: mpsc::Receiver<ProgressEvent>,
    execution: impl Future<Output = Result<T, CommandError>>,
) -> (Result<T, CommandError>, bool) {
    let mut result = {
        let interval = PROGRESS_INTERVAL;
        let heartbeat_context = OperationContext::with_deadline(None, CancellationToken::new());
        let mut heartbeat = progress_heartbeat(
            observer.heartbeat_clock.get().map(Arc::as_ref),
            interval,
            &heartbeat_context,
        );
        let mut display = ProgressDisplay::default();
        tokio::pin!(execution);
        loop {
            tokio::select! {
                biased;
                result = &mut execution => {
                    break result;
                }
                Some(event) = events.recv() => {
                    let result = match event.progress {
                        ConsoleUpdate::Progress(update) => {
                            let result = display.update(update, host.progress_color(), stderr);
                            heartbeat = progress_heartbeat(
                                observer.heartbeat_clock.get().map(Arc::as_ref),
                                interval,
                                &heartbeat_context,
                            );
                            result
                        }
                        ConsoleUpdate::Diagnostic(event, timestamp) => {
                            observer.remember(&event, false);
                            let result = diagnostics::write(&event, timestamp, host.progress_color(), stderr);
                            if result.is_ok() {
                                observer.remember(&event, true);
                            }
                            result
                        }
                    };
                    if result.is_err() && observer.timing.is_some() {
                        observer.broken.store(true, Ordering::Relaxed);
                    }
                    // Cancellation can drop the observer while its event is queued.
                    let _ = event.written.send(result);
                }
                result = &mut heartbeat, if display.needs_heartbeat() && !observer.failed.is_cancelled() && !observer.broken.load(Ordering::Relaxed) => {
                    if let Err(error) = result {
                        break Err(error.into());
                    }
                    if display.heartbeat(host.progress_color(), stderr).is_err() {
                        if observer.timing.is_some() {
                            observer.broken.store(true, Ordering::Relaxed);
                        }
                        observer.failed.cancel();
                    }
                    heartbeat = progress_heartbeat(
                        observer.heartbeat_clock.get().map(Arc::as_ref),
                        interval,
                        &heartbeat_context,
                    );
                }
            }
        }
    };
    if let Some((clock, _)) = &observer.timing
        && !observer.broken.load(Ordering::Relaxed)
    {
        let phase = match &result {
            Ok(_) => {
                observer
                    .reporting
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .completed = true;
                DiagnosticPhase::Completed
            }
            Err(error) => DiagnosticPhase::Failed(error.kind()),
        };
        let elapsed = observer.elapsed();
        let event = DiagnosticEvent::Stage {
            stage: DiagnosticStage::Command,
            phase,
            duration: DiagnosticDuration::from_duration(elapsed),
        };
        if diagnostics::write(&event, clock.wall_now(), host.progress_color(), stderr).is_err() {
            observer.broken.store(true, Ordering::Relaxed);
            let mut error = observer.reporting_error();
            if let Err(original) = &result {
                if error.recovery.is_none() {
                    error.recovery = original.recovery.clone();
                }
                error.message.push_str(&format!(
                    " Command failed ({}).",
                    diagnostics::category(original.kind())
                ));
            }
            result = Err(error);
        }
    }
    for diagnostic in host.take_diagnostics() {
        if !observer.broken.load(Ordering::Relaxed) {
            let _ = writeln!(stderr, "{diagnostic}");
        }
    }
    (result, observer.broken.load(Ordering::Relaxed))
}
