// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit user-input adapters owned by the executable, not SDK libraries.

#[cfg(feature = "native-read")]
use fabric_cli_output::CommandError;
use std::io::{self, BufRead, Write};

const ANSWER_LIMIT: u64 = 128;
const SELECTION_LIMIT: u64 = 65_536;

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
    let count = io::Read::take(&mut *input, ANSWER_LIMIT + 1).read_line(&mut answer)?;
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

/// Choose item numbers from caller-supplied names in their display order.
///
/// Comma-separated numbers select a subset, `all` selects every item, and an
/// entered empty line selects none. Duplicate numbers are collapsed and names
/// are returned unchanged in display order. EOF, invalid input and stream
/// failures are errors, never an implicit selection.
pub fn read_selection(
    input: &mut impl BufRead,
    output: &mut impl Write,
    names: &[String],
) -> io::Result<Vec<String>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    writeln!(output, "Select items:")?;
    for (index, name) in names.iter().enumerate() {
        writeln!(output, "  {}. {}", index + 1, name.escape_debug())?;
    }
    write!(
        output,
        "Enter comma-separated item numbers, 'all', or Enter for none: "
    )?;
    output.flush()?;
    let mut answer = String::new();
    let count = io::Read::take(&mut *input, SELECTION_LIMIT + 1).read_line(&mut answer)?;
    if count as u64 > SELECTION_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Item selection exceeds the input limit",
        ));
    }
    if count == 0 || !answer.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Item selection requires an entered line",
        ));
    }
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(Vec::new());
    }
    if answer.eq_ignore_ascii_case("all") {
        return Ok(names.to_vec());
    }
    let mut selected = vec![false; names.len()];
    for number in answer.split(',') {
        let number = number.trim();
        let index = number
            .parse::<usize>()
            .ok()
            .filter(|index| {
                (1..=names.len()).contains(index)
                    && number.bytes().all(|byte| byte.is_ascii_digit())
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Select listed item numbers separated by commas, 'all', or Enter for none",
                )
            })?;
        selected[index - 1] = true;
    }
    Ok(names
        .iter()
        .zip(selected)
        .filter(|(_, selected)| *selected)
        .map(|(name, _)| name.clone())
        .collect())
}

/// Prompt only when an explicitly invoked command requests confirmation.
///
/// Stdin and stderr are confined to a blocking worker so input cannot block the
/// current-thread async runtime. Dropping this future does not interrupt an OS
/// read; the native host's bounded runtime shutdown still applies.
#[cfg(feature = "native-read")]
pub async fn confirm(question: &str) -> Result<bool, CommandError> {
    let question = question.to_owned();
    prompt(
        move || read_confirmation(&mut io::stdin().lock(), &mut io::stderr(), &question),
        "ConfirmationWorkerFailed",
        "ConfirmationInputFailed",
    )
    .await
}

/// Select export items using stdin and stderr on the existing runtime's worker.
///
/// The caller controls prompt eligibility and cancellation. Dropping this
/// future does not interrupt an OS read; the native host uses bounded shutdown.
#[cfg(feature = "native-read")]
pub async fn select_items(names: &[String]) -> Result<Vec<String>, CommandError> {
    let names = names.to_vec();
    prompt(
        move || read_selection(&mut io::stdin().lock(), &mut io::stderr(), &names),
        "SelectionWorkerFailed",
        "SelectionInputFailed",
    )
    .await
}

#[cfg(feature = "native-read")]
async fn prompt<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
    worker_code: &'static str,
    input_code: &'static str,
) -> Result<T, CommandError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| {
            CommandError::new(
                Some("The terminal input worker did not complete".into()),
                Some(worker_code.into()),
            )
        })?
        .map_err(|error| CommandError::new(Some(error.to_string()), Some(input_code.into())))
}

#[cfg(all(test, feature = "native-read"))]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[tokio::test(flavor = "current_thread")]
    async fn pending_input_does_not_block_the_callers_runtime() {
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = mpsc::channel();
        let task = tokio::spawn(prompt(
            move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5))
                    .map_err(io::Error::other)?;
                Ok(true)
            },
            "WorkerFailed",
            "InputFailed",
        ));
        let ready = tokio::time::timeout(Duration::from_secs(1), ready).await;
        release.send(()).unwrap();
        assert!(ready.unwrap().is_ok());
        assert!(task.await.unwrap().unwrap());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_stops_waiting_but_does_not_claim_to_interrupt_the_worker() {
        let (started, ready) = tokio::sync::oneshot::channel();
        let (finished, done) = tokio::sync::oneshot::channel();
        let (release, wait) = mpsc::channel();
        let task = tokio::spawn(prompt(
            move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5))
                    .map_err(io::Error::other)?;
                finished.send(()).unwrap();
                Ok(true)
            },
            "WorkerFailed",
            "InputFailed",
        ));
        let ready = tokio::time::timeout(Duration::from_secs(1), ready).await;
        task.abort();
        let cancelled = task.await.unwrap_err();
        release.send(()).unwrap();
        assert!(ready.unwrap().is_ok());
        assert!(cancelled.is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), done)
            .await
            .unwrap()
            .unwrap();
    }
}
