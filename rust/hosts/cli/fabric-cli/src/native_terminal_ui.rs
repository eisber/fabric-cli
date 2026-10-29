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

use fabric_cli_mutations::{CommandFuture, MutationError, MutationUi, NoticeKind, RemovalUi};
use fabric_cli_resources::{CommandError, Confirmation};
use std::{future::Future, io, pin::Pin, sync::Arc};

type ConfirmationReader = dyn Fn(&str) -> io::Result<bool> + Send + Sync;

fn read_native_confirmation(question: &str) -> io::Result<bool> {
    fabric_cli::console::read_confirmation(&mut io::stdin().lock(), &mut io::stderr(), question)
}

pub(crate) struct NativeAccessConfirmation {
    reader: Arc<ConfirmationReader>,
}

impl Default for NativeAccessConfirmation {
    fn default() -> Self {
        Self::with_reader(read_native_confirmation)
    }
}

impl NativeAccessConfirmation {
    pub(crate) fn with_reader(
        reader: impl Fn(&str) -> io::Result<bool> + Send + Sync + 'static,
    ) -> Self {
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
type SelectionReader = dyn Fn(&str, &[String]) -> io::Result<Vec<String>> + Send + Sync;

fn read_native_selection(prompt: &str, names: &[String]) -> io::Result<Vec<String>> {
    fabric_cli::console::read_selection_with_prompt(
        &mut io::stdin().lock(),
        &mut io::stderr(),
        prompt,
        names,
    )
}

pub(crate) struct NativeMutationUi {
    pub(crate) records: Vec<fabric_cli_output::RenderPlan>,
    confirmation: NativeAccessConfirmation,
    selection: Arc<SelectionReader>,
    writer: Arc<NoticeWriter>,
}

impl Default for NativeMutationUi {
    fn default() -> Self {
        Self::with_io(read_native_confirmation, |records| {
            use io::Write;
            let mut stdout = io::stdout().lock();
            let mut stderr = io::stderr().lock();
            for record in records {
                record.write_to(&mut stdout, &mut stderr)?;
            }
            stdout.flush()?;
            stderr.flush()
        })
    }
}

impl NativeMutationUi {
    pub(crate) fn with_io(
        reader: impl Fn(&str) -> io::Result<bool> + Send + Sync + 'static,
        writer: impl Fn(&[fabric_cli_output::RenderPlan]) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self::with_selection_io(reader, read_native_selection, writer)
    }

    pub(crate) fn with_selection_io(
        reader: impl Fn(&str) -> io::Result<bool> + Send + Sync + 'static,
        selection: impl Fn(&str, &[String]) -> io::Result<Vec<String>> + Send + Sync + 'static,
        writer: impl Fn(&[fabric_cli_output::RenderPlan]) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            records: Vec::new(),
            confirmation: NativeAccessConfirmation::with_reader(reader),
            selection: Arc::new(selection),
            writer: Arc::new(writer),
        }
    }

    async fn flush_notices(
        &mut self,
        worker_code: &str,
        output_code: &str,
        worker_message: &str,
    ) -> Result<(), MutationError> {
        let records = std::mem::take(&mut self.records);
        if !records.is_empty() {
            let writer = Arc::clone(&self.writer);
            tokio::task::spawn_blocking(move || writer(&records))
                .await
                .map_err(|_| MutationError::new(worker_code, worker_message))?
                .map_err(|error| MutationError::new(output_code, error.to_string()))?;
        }
        Ok(())
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
            self.flush_notices(
                "ConfirmationWorkerFailed",
                "ConfirmationOutputFailed",
                "The confirmation output worker did not complete",
            )
            .await?;
            self.confirmation
                .confirm(message)
                .await
                .map_err(|error| MutationError::new(&error.code, error.message))
        })
    }
}

impl RemovalUi for NativeMutationUi {
    fn select<'a>(&'a mut self, _: &'a str, _: &'a [String]) -> CommandFuture<'a, Vec<String>> {
        // Native input is only read at the fallible production boundary.
        Box::pin(async { Vec::new() })
    }

    fn select_result<'a>(
        &'a mut self,
        prompt: &'a str,
        names: &'a [String],
    ) -> CommandFuture<'a, Result<Vec<String>, MutationError>> {
        Box::pin(async move {
            self.flush_notices(
                "SelectionWorkerFailed",
                "SelectionOutputFailed",
                "The selection output worker did not complete",
            )
            .await?;
            let reader = Arc::clone(&self.selection);
            let prompt = prompt.to_owned();
            let names = names.to_vec();
            tokio::task::spawn_blocking(move || reader(&prompt, &names))
                .await
                .map_err(|_| {
                    MutationError::new(
                        "SelectionWorkerFailed",
                        "The selection input worker did not complete",
                    )
                })?
                .map_err(|error| MutationError::new("SelectionInputFailed", error.to_string()))
        })
    }
}
