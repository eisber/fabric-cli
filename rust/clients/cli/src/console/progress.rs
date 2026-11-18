// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Terminal presentation of engine polling events.

use fabric_core::{Clock, CoreError, ErrorKind, OperationContext, OperationKind};
use fabric_sdk_operations::PollProgress;
use std::{future::Future, io::Write, pin::Pin, time::Duration};

/// Interval before reporting that polling is still in progress.
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);

/// A typed update for the polling display.
pub enum ProgressUpdate {
    /// The service accepted the operation.
    Accepted(OperationKind),
    /// Polling is waiting until the next status check.
    Waiting(Duration),
    /// A status check is in flight.
    Polling(OperationKind),
    /// The service reported a new status.
    Status(PollProgress),
    /// Command projection completed.
    Finished,
}

/// Report a polling progress output failure.
pub fn progress_error() -> CoreError {
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
/// Caller-owned polling display state.
pub struct ProgressDisplay {
    started: Option<OperationKind>,
    latest: Option<PollProgress>,
    requesting: bool,
    waiting_reported: bool,
}

impl ProgressDisplay {
    /// Whether a heartbeat can add new information.
    pub fn needs_heartbeat(&self) -> bool {
        self.started.is_some() && !self.waiting_reported
    }

    /// Render and flush the supplied polling update.
    pub fn update(
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

    /// Render the waiting heartbeat once between updates.
    pub fn heartbeat(&mut self, color: bool, stderr: &mut dyn Write) -> Result<(), CoreError> {
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

    fn finish(&mut self, color: bool, stderr: &mut dyn Write) -> Result<(), CoreError> {
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

/// Sleep on the injected clock; remain pending until one has been configured.
pub fn progress_heartbeat<'a>(
    clock: Option<&'a dyn Clock>,
    interval: Duration,
    context: &'a OperationContext,
) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + 'a>> {
    match clock {
        Some(clock) => Box::pin(clock.sleep(interval, context)),
        None => Box::pin(std::future::pending()),
    }
}
