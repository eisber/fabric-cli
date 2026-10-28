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
