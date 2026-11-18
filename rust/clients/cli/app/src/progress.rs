// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::diagnostics;
use async_trait::async_trait;
use fabric_cli_command_contracts::CommandError;
use fabric_core::{
    CancellationToken, Clock, CoreError, ErrorKind, OperationContext, OperationKind, RecoveryInfo,
    diagnostics::{
        DiagnosticDuration, DiagnosticEvent, DiagnosticObserver, DiagnosticOutcome,
        DiagnosticPhase, DiagnosticStage,
    },
};
use fabric_sdk_api::{PollObserver, PollProgress};
use fabric_sdk_http::bounded_with_clock;
use std::{
    future::Future,
    io::Write,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{mpsc, oneshot};

pub(super) const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);

pub(super) struct ProgressEvent {
    pub(super) progress: ConsoleUpdate,
    pub(super) written: oneshot::Sender<Result<(), CoreError>>,
}

pub(super) enum ConsoleUpdate {
    Progress(ProgressUpdate),
    Diagnostic(Box<DiagnosticEvent>, SystemTime),
}

pub(super) enum ProgressUpdate {
    Accepted(OperationKind),
    Waiting(Duration),
    Polling(OperationKind),
    Status(PollProgress),
    Finished,
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
    pub(super) async fn send(&self, progress: ProgressUpdate) -> Result<(), CoreError> {
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

    pub(super) fn elapsed(&self) -> Duration {
        self.timing
            .as_ref()
            .map_or(Duration::ZERO, |(clock, start)| {
                clock.now().saturating_duration_since(*start)
            })
    }

    async fn emit(&self, event: DiagnosticEvent) -> Result<(), CoreError> {
        let Some((clock, _)) = &self.timing else {
            return Ok(());
        };
        self.send_update(ConsoleUpdate::Diagnostic(Box::new(event), clock.wall_now()))
            .await
    }

    pub(super) async fn stage(
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

    pub(super) async fn start(&self, stage: DiagnosticStage) -> Result<Duration, CommandError> {
        let started = self.elapsed();
        self.stage(stage, DiagnosticPhase::Started, Duration::ZERO)
            .await?;
        Ok(started)
    }

    pub(super) async fn finish<T>(
        &self,
        stage: DiagnosticStage,
        started: Duration,
        result: Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        let phase = result.as_ref().map_or_else(
            |error| DiagnosticPhase::Failed(error.kind()),
            |_| DiagnosticPhase::Completed,
        );
        self.stage(stage, phase, self.elapsed().saturating_sub(started))
            .await?;
        result
    }

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
            .and_then(|value| serde_json::to_value(value).ok())
            .map(Box::new);
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

pub(super) fn progress_error() -> CoreError {
    CoreError::new(
        ErrorKind::Transport,
        "Cannot write polling progress to stderr; the output stream was closed or failed",
    )
}

fn progress_line(
    message: &str,
    color: bool,
    tone: &str,
    stderr: &mut dyn Write,
) -> Result<(), CoreError> {
    let result = if color {
        writeln!(stderr, "\x1b[{tone}m{message}\x1b[0m")
    } else {
        writeln!(stderr, "{message}")
    };
    let result = result
        .and_then(|()| stderr.flush())
        .map_err(|_| progress_error());
    if result.is_err() && color {
        // A partial colored write must not leave the user's terminal styled.
        let _ = write!(stderr, "\x1b[0m").and_then(|()| stderr.flush());
    }
    result
}

fn progress_label(kind: OperationKind) -> &'static str {
    if kind == OperationKind::FabricJob {
        "job"
    } else {
        "operation"
    }
}

fn status_text(progress: PollProgress) -> String {
    if let Some(percentage) = progress.percentage_complete() {
        format!("{} ({percentage}%)", progress.status())
    } else {
        progress.status().into()
    }
}

#[derive(Default)]
pub(super) struct ProgressDisplay {
    pub(super) started: Option<OperationKind>,
    latest: Option<PollProgress>,
    requesting: bool,
    pub(super) waiting_reported: bool,
}

impl ProgressDisplay {
    pub(super) fn update(
        &mut self,
        update: ProgressUpdate,
        color: bool,
        stderr: &mut dyn Write,
    ) -> Result<(), CoreError> {
        if matches!(update, ProgressUpdate::Finished) {
            return self.finish(color, stderr);
        }
        if let ProgressUpdate::Waiting(delay) = update {
            self.requesting = false;
            self.waiting_reported = true;
            let label = progress_label(self.started.ok_or_else(progress_error)?);
            return progress_line(
                &format!(
                    "Waiting for {label} completion; next status check in {}s...",
                    delay.as_secs_f64()
                ),
                color,
                "90",
                stderr,
            );
        }
        let (message, tone) = match update {
            ProgressUpdate::Accepted(kind) => {
                self.started = Some(kind);
                let label = if kind == OperationKind::FabricJob {
                    "Job"
                } else {
                    "Operation"
                };
                (format!("{label} accepted. Polling for completion..."), "36")
            }
            ProgressUpdate::Polling(kind) => {
                self.requesting = true;
                self.waiting_reported = false;
                (format!("Checking {} status...", progress_label(kind)), "36")
            }
            ProgressUpdate::Status(progress) => {
                self.latest = Some(progress);
                self.requesting = false;
                self.waiting_reported = false;
                let label = if progress.is_job() {
                    "Job instance status"
                } else {
                    "Operation status"
                };
                let tone = match progress.status() {
                    "Succeeded" | "Completed" | "Deduped" => "32",
                    "Failed" => "31",
                    "Cancelled" => "33",
                    _ => "36",
                };
                (format!("{label}: {}", status_text(progress)), tone)
            }
            ProgressUpdate::Waiting(..) => {
                unreachable!("handled before rendering permanent output")
            }
            ProgressUpdate::Finished => unreachable!("handled before rendering permanent output"),
        };
        progress_line(&message, color, tone, stderr)
    }

    pub(super) fn heartbeat(
        &mut self,
        color: bool,
        stderr: &mut dyn Write,
    ) -> Result<(), CoreError> {
        if self.waiting_reported {
            return Ok(());
        }
        let kind = self.started.ok_or_else(progress_error)?;
        let label = progress_label(kind);
        let waiting = if self.latest.is_some_and(|progress| {
            matches!(progress.status(), "Succeeded" | "Completed" | "Deduped")
        }) {
            format!("Waiting for {label} result")
        } else if self.requesting {
            format!("Waiting for {label} status response")
        } else {
            format!("Waiting for {label} completion")
        };
        let last = self.latest.map_or_else(String::new, |progress| {
            format!("; Last reported status: {}", status_text(progress))
        });
        self.waiting_reported = true;
        progress_line(&format!("{waiting}{last}..."), color, "90", stderr)
    }

    pub(super) fn finish(&mut self, color: bool, stderr: &mut dyn Write) -> Result<(), CoreError> {
        if let Some(kind) = self.started {
            let label = if kind == OperationKind::FabricJob {
                "Job"
            } else {
                "Operation"
            };
            let message = if self.latest.is_some_and(|p| p.status() == "Deduped") {
                format!("{label} finished with status: Deduped.")
            } else {
                format!("{label} completed.")
            };
            progress_line(&message, color, "32", stderr)?;
        }
        Ok(())
    }
}

pub(super) fn progress_heartbeat<'a>(
    clock: Option<&'a dyn Clock>,
    interval: Duration,
    context: &'a OperationContext,
) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + 'a>> {
    match clock {
        Some(clock) => Box::pin(clock.sleep(interval, context)),
        None => Box::pin(std::future::pending()),
    }
}
