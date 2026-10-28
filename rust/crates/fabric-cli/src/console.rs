// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit user-input adapters owned by the executable, not SDK libraries.

#[cfg(feature = "native-read")]
use fabric_cli_output::CommandError;
use std::io::{self, BufRead, Write};

const ANSWER_LIMIT: u64 = 128;

/// Read an explicit confirmation, preserving the source prompt's yes default.
///
/// An entered empty line selects yes. EOF is not confirmation, and malformed
/// answers, oversized input and writer failures are errors rather than consent.
/// The caller owns the input/output streams and decides when prompting is allowed.
pub fn read_confirmation(
    input: &mut impl BufRead,
    output: &mut impl Write,
    question: &str,
) -> io::Result<bool> {
    output.write_all(question.as_bytes())?;
    output.write_all(b" [Y/n] ")?;
    output.flush()?;
    let mut answer = String::new();
    let count = input.take(ANSWER_LIMIT + 1).read_line(&mut answer)?;
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Confirmation input ended without an answer",
        ));
    }
    if count as u64 > ANSWER_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Confirmation answer exceeds the input limit",
        ));
    }
    if !answer.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Confirmation answer requires an entered line",
        ));
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "" | "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Confirmation requires yes or no",
        )),
    }
}

/// Prompt only when an explicitly invoked command requests confirmation.
///
/// Stdin and stderr are confined to a blocking worker so input cannot block the
/// current-thread async runtime. Dropping this future does not interrupt an OS
/// read; the native host's bounded runtime shutdown still applies.
#[cfg(feature = "native-read")]
pub async fn confirm(question: &str) -> Result<bool, CommandError> {
    let question = question.to_owned();
    tokio::task::spawn_blocking(move || {
        read_confirmation(&mut io::stdin().lock(), &mut io::stderr().lock(), &question)
    })
    .await
    .map_err(|_| {
        CommandError::new(
            Some("The confirmation input worker did not complete".into()),
            Some("ConfirmationWorkerFailed".into()),
        )
    })?
    .map_err(|error| {
        CommandError::new(
            Some(error.to_string()),
            Some("ConfirmationInputFailed".into()),
        )
    })
}
