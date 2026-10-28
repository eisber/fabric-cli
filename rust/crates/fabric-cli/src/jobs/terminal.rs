// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! CLI-owned schedule-removal confirmation; SDK consumers never open a terminal.

use fabric_cli_output::CommandError;
use std::io::{BufRead, Write};

fn io_error(error: std::io::Error) -> CommandError {
    CommandError::new(Some(error.to_string()), Some("LocalIOError".into()))
}

/// Ask the source's default-no question using explicitly supplied streams.
///
/// The prompt is flushed before input is read. EOF, an empty line and every
/// answer except `y`/`yes` decline deletion. Read, write and flush failures must
/// reach the handler before it can submit a DELETE.
pub fn confirm_with(
    input: &mut impl BufRead,
    prompt: &mut impl Write,
) -> Result<bool, CommandError> {
    prompt
        .write_all(b"Are you sure? [y/N] ")
        .and_then(|()| prompt.flush())
        .map_err(io_error)?;
    let mut answer = String::new();
    input.read_line(&mut answer).map_err(io_error)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Ask on the native CLI terminal without blocking its asynchronous runtime.
///
/// The caller races this future with local interruption before mutation. A
/// cancelled terminal read may finish later, but can never perform a DELETE:
/// only the still-running job handler owns that continuation. `--force` never
/// calls this function. The prompt uses stderr, leaving JSON stdout untouched.
#[cfg(feature = "native-read")]
pub async fn confirm_terminal() -> Result<bool, CommandError> {
    tokio::task::spawn_blocking(|| {
        confirm_with(&mut std::io::stdin().lock(), &mut std::io::stderr().lock())
    })
    .await
    .map_err(|_| {
        CommandError::new(
            Some("The terminal confirmation reader failed".into()),
            Some("LocalIOError".into()),
        )
    })?
}
