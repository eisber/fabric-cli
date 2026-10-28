// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit native host input, never an implicit library terminal dependency.
//!
//! Confirmation follows `fab_ui.prompt_confirm` at Python source
//! cf67fd52ea8b47e24c725e73d792de6f4a08e2e4. The existing stream reader owns
//! the entered-line/default contract; this adapter owns scheduling and errors.
//! EOF, invalid input and failed prompt delivery are errors, never consent.
//! Only the experimental Windows native host selects real stdin/stderr.
//! Cancellation drops the answer future without cancelling an OS read; the
//! native runtime's bounded shutdown still applies. No credentials are selected.

use fabric_cli_mutations::{CommandFuture, MutationError, MutationUi, NoticeKind};
use fabric_cli_resources::{CommandError, Confirmation};
use std::{future::Future, io, pin::Pin, sync::Arc};

type ConfirmationReader = dyn Fn(&str) -> io::Result<bool> + Send + Sync;

pub(crate) struct NativeAccessConfirmation {
    reader: Arc<ConfirmationReader>,
}

impl Default for NativeAccessConfirmation {
    fn default() -> Self {
        Self {
            reader: Arc::new(|question| {
                fabric_cli::console::read_confirmation(
                    &mut io::stdin().lock(),
                    &mut io::stderr().lock(),
                    question,
                )
            }),
        }
    }
}

impl NativeAccessConfirmation {
    #[cfg(test)]
    pub(crate) fn scripted(reader: impl Fn(&str) -> io::Result<bool> + Send + Sync + 'static) -> Self {
        Self {
            reader: Arc::new(reader),
        }
    }
}

impl Confirmation for NativeAccessConfirmation {
    fn confirm<'a>(
        &'a mut self,
        question: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, CommandError>> + Send + 'a>> {
        let reader = Arc::clone(&self.reader);
        let question = question.to_owned();
        Box::pin(async move {
            // Dropping the future stops waiting, not an already-running OS read.
            tokio::task::spawn_blocking(move || reader(&question))
                .await
                .map_err(|_| {
                    CommandError::new(
                        "ConfirmationWorkerFailed",
                        "The confirmation input worker did not complete",
                    )
                })?
                .map_err(|error| CommandError::new("ConfirmationInputFailed", error.to_string()))
        })
    }
}

type NoticeWriter = dyn Fn(&[fabric_cli_output::RenderPlan]) -> io::Result<()> + Send + Sync;

pub(crate) struct NativeMutationUi {
    pub(crate) records: Vec<fabric_cli_output::RenderPlan>,
    confirmation: NativeAccessConfirmation,
    writer: Arc<NoticeWriter>,
}

impl Default for NativeMutationUi {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            confirmation: NativeAccessConfirmation::default(),
            writer: Arc::new(|records| {
                use io::Write;
                let mut stdout = io::stdout().lock();
                let mut stderr = io::stderr().lock();
                for record in records {
                    record.write_to(&mut stdout, &mut stderr)?;
                }
                stdout.flush()?;
                stderr.flush()
            }),
        }
    }
}

impl NativeMutationUi {
    #[cfg(test)]
    pub(crate) fn scripted(
        reader: impl Fn(&str) -> io::Result<bool> + Send + Sync + 'static,
        writer: impl Fn(&[fabric_cli_output::RenderPlan]) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            records: Vec::new(),
            confirmation: NativeAccessConfirmation::scripted(reader),
            writer: Arc::new(writer),
        }
    }
}

impl MutationUi for NativeMutationUi {
    fn notice(&mut self, kind: NoticeKind, message: &str) {
        use fabric_cli_output::{
            OutputStream,
            text::{self, Presentation, Style},
        };
        let plan = match kind {
            NoticeKind::Warning => text::render_notice(
                text::Notice::Warning(message),
                None,
                Presentation::default(),
            ),
            _ => text::render_line(
                message,
                Style::Grey,
                OutputStream::Stderr,
                Presentation::default(),
            ),
        };
        self.records.extend(plan.records);
    }

    fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
        // The legacy infallible boundary never reads native input. All resource
        // consumers use confirm_result, where prompt failures remain observable.
        Box::pin(async { false })
    }

    fn confirm_result<'a>(
        &'a mut self,
        message: &'a str,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        Box::pin(async move {
            let records = std::mem::take(&mut self.records);
            if !records.is_empty() {
                let writer = Arc::clone(&self.writer);
                tokio::task::spawn_blocking(move || writer(&records))
                    .await
                    .map_err(|_| {
                        MutationError::new(
                            "ConfirmationWorkerFailed",
                            "The confirmation output worker did not complete",
                        )
                    })?
                    .map_err(|error| {
                        MutationError::new("ConfirmationOutputFailed", error.to_string())
                    })?;
            }
            self.confirmation
                .confirm(message)
                .await
                .map_err(|error| MutationError::new(&error.code, error.message))
        })
    }
}
