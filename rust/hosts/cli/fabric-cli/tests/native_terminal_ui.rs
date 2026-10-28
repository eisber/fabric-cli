// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline tests of the exact host adapter; no terminal or credential access.

#![deny(unnameable_test_items)]

#[path = "../src/native_terminal_ui.rs"]
mod terminal_ui;

use fabric_cli_resources::Confirmation;
use std::{
    io::{self, Cursor, Write},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use terminal_ui::NativeAccessConfirmation;

#[tokio::test(flavor = "current_thread")]
async fn access_questions_and_entered_answers_reach_the_existing_source_reader() {
    let input = Arc::new(Mutex::new(Cursor::new(b"YES\r\nno\n\n")));
    let output = Arc::new(Mutex::new(Vec::new()));
    let mut prompt = NativeAccessConfirmation::scripted({
        let output = Arc::clone(&output);
        move |question| {
            fabric_cli::console::read_confirmation(
                &mut *input.lock().unwrap(),
                &mut *output.lock().unwrap(),
                question,
            )
        }
    });
    assert!(prompt.confirm("Are you sure?").await.unwrap());
    assert!(!prompt.confirm("Overwrite?").await.unwrap());
    assert!(prompt.confirm("Are you sure?").await.unwrap());
    assert_eq!(
        *output.lock().unwrap(),
        b"Are you sure? [Y/n] Overwrite? [Y/n] Are you sure? [Y/n] "
    );
}

#[tokio::test(flavor = "current_thread")]
async fn eof_unentered_invalid_and_oversized_input_are_errors_not_declines() {
    for answer in ["", "yes", "maybe\n", &format!("{}\n", "y".repeat(129))] {
        let answer = answer.to_owned();
        let mut prompt = NativeAccessConfirmation::scripted(move |question| {
            fabric_cli::console::read_confirmation(
                &mut Cursor::new(answer.as_bytes()),
                &mut Vec::new(),
                question,
            )
        });
        let error = prompt.confirm("Are you sure?").await.unwrap_err();
        assert_eq!(error.code, "ConfirmationInputFailed");
        assert!(!error.mutation_may_have_occurred);
    }
}

struct BrokenWriter {
    fail_flush: bool,
}

impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_flush {
            Ok(bytes.len())
        } else {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed prompt"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed prompt"))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prompt_write_and_flush_errors_do_not_consume_or_confirm_input() {
    for fail_flush in [false, true] {
        let input = Arc::new(Mutex::new(Cursor::new(b"yes\n")));
        let mut prompt = NativeAccessConfirmation::scripted({
            let input = Arc::clone(&input);
            move |question| {
                fabric_cli::console::read_confirmation(
                    &mut *input.lock().unwrap(),
                    &mut BrokenWriter { fail_flush },
                    question,
                )
            }
        });
        let error = prompt.confirm("Overwrite?").await.unwrap_err();
        assert_eq!(error.code, "ConfirmationInputFailed");
        assert_eq!(error.message, "closed prompt");
        assert_eq!(input.lock().unwrap().position(), 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn interrupted_reads_and_worker_failures_remain_distinct_errors() {
    let mut interrupted = NativeAccessConfirmation::scripted(|_| {
        Err(io::Error::new(io::ErrorKind::Interrupted, "input cancelled"))
    });
    let error = interrupted.confirm("Are you sure?").await.unwrap_err();
    assert_eq!(error.code, "ConfirmationInputFailed");
    assert_eq!(error.message, "input cancelled");

    let mut panicking = NativeAccessConfirmation::scripted(|_| panic!("synthetic worker failure"));
    let error = panicking.confirm("Are you sure?").await.unwrap_err();
    assert_eq!(error.code, "ConfirmationWorkerFailed");
    assert_eq!(error.message, "The confirmation input worker did not complete");
}

#[tokio::test(flavor = "current_thread")]
async fn a_pending_prompt_does_not_block_the_runtime_or_turn_cancellation_into_consent() {
    let (release, wait) = mpsc::channel();
    let wait = Mutex::new(wait);
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let (finished, completion) = tokio::sync::oneshot::channel();
    let finished = Mutex::new(Some(finished));
    let mut prompt = NativeAccessConfirmation::scripted(move |_| {
        started.send(()).unwrap();
        wait.lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .map_err(io::Error::other)?;
        finished.lock().unwrap().take().unwrap().send(()).unwrap();
        Ok(true)
    });
    let mut pending = prompt.confirm("Are you sure?");
    tokio::select! {
        result = &mut pending => panic!("Prompt completed without input: {result:?}"),
        result = observed.recv() => assert_eq!(result, Some(())),
    }
    tokio::select! {
        result = &mut pending => panic!("Prompt completed without input: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(1)) => {},
    }
    drop(pending);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), completion)
        .await
        .unwrap()
        .unwrap();
}
