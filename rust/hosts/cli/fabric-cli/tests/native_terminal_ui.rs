// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline tests of the exact host adapter; no terminal or credential access.

#![deny(unnameable_test_items)]

#[path = "../src/native_terminal_ui.rs"]
mod terminal_ui;

use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
use fabric_cli_mutations::{
    CommandFuture, MutationError, MutationHost, MutationMetadata, MutationUi, NoticeKind,
};
use fabric_cli_resources::Confirmation;
use fabric_sdk_workspaces::mutations::{
    ResourceError, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use serde_json::Value;
use std::{
    io::{self, Cursor, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use terminal_ui::{NativeAccessConfirmation, NativeMutationUi};

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

#[tokio::test(flavor = "current_thread")]
async fn resource_confirmation_delivers_prior_notices_before_reading_input_once() {
    let transcript = Arc::new(Mutex::new(Vec::new()));
    let mut prompt = NativeMutationUi::scripted(
        {
            let transcript = Arc::clone(&transcript);
            move |question| {
                fabric_cli::console::read_confirmation(
                    &mut Cursor::new(b"no\n"),
                    &mut *transcript.lock().unwrap(),
                    question,
                )
            }
        },
        {
            let transcript = Arc::clone(&transcript);
            move |records| {
                for record in records {
                    record.write_to(&mut Vec::new(), &mut *transcript.lock().unwrap())?;
                }
                Ok(())
            }
        },
    );
    prompt.notice(
        NoticeKind::Warning,
        "Modifying properties may lead to unintended consequences",
    );
    let prior = prompt
        .records
        .iter()
        .flat_map(|record| record.payload.clone())
        .collect::<Vec<_>>();
    assert!(!prompt.confirm_result("Are you sure?").await.unwrap());
    assert!(prompt.records.is_empty());
    let expected = [prior.as_slice(), b"Are you sure? [Y/n] "].concat();
    assert_eq!(*transcript.lock().unwrap(), expected);
}

#[tokio::test(flavor = "current_thread")]
async fn resource_prompt_failure_preserves_its_error_class_instead_of_declining() {
    for (answer, expected) in [
        (Some("yes\n"), Some(true)),
        (Some("n\n"), Some(false)),
        (None, None),
    ] {
        let mut prompt = NativeMutationUi::scripted(
            move |question| {
                fabric_cli::console::read_confirmation(
                    &mut Cursor::new(answer.unwrap_or_default()),
                    &mut Vec::new(),
                    question,
                )
            },
            |_| Ok(()),
        );
        let result = prompt.confirm_result("Are you sure?").await;
        match expected {
            Some(answer) => assert_eq!(result.unwrap(), answer),
            None => {
                let error = result.unwrap_err();
                assert_eq!(error.code, "ConfirmationInputFailed");
                assert!(error.operation.is_none());
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn notice_delivery_failure_never_reaches_resource_input_or_replays_the_notice() {
    for panicking in [false, true] {
        let inputs = Arc::new(AtomicUsize::new(0));
        let deliveries = Arc::new(AtomicUsize::new(0));
        let mut prompt = NativeMutationUi::scripted(
            {
                let inputs = Arc::clone(&inputs);
                move |_| {
                    inputs.fetch_add(1, Ordering::SeqCst);
                    Ok(true)
                }
            },
            {
                let deliveries = Arc::clone(&deliveries);
                move |_| {
                    deliveries.fetch_add(1, Ordering::SeqCst);
                    assert!(!panicking, "synthetic output worker failure");
                    Err(io::Error::new(io::ErrorKind::BrokenPipe, "notice failed"))
                }
            },
        );
        prompt.notice(NoticeKind::Warning, "Resource warning");
        let error = prompt.confirm_result("Are you sure?").await.unwrap_err();
        assert_eq!(
            error.code,
            if panicking {
                "ConfirmationWorkerFailed"
            } else {
                "ConfirmationOutputFailed"
            }
        );
        assert_eq!(inputs.load(Ordering::SeqCst), 0);
        assert_eq!(deliveries.load(Ordering::SeqCst), 1);
        assert!(prompt.records.is_empty());
    }
}

struct Operations(usize);
impl ResourceOperations for Operations {
    fn execute(
        &mut self,
        _: ResourceOperation,
    ) -> CommandFuture<'_, Result<ResourceOutcome, ResourceError>> {
        self.0 += 1;
        Box::pin(async {
            Ok(ResourceOutcome::Completed {
                status: 200,
                data: None,
            })
        })
    }
}

struct Metadata;
impl MutationMetadata for Metadata {
    fn read<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Value, MutationError>> {
        panic!("delete must not read SET metadata")
    }
}

fn delete_target() -> Arc<Element> {
    let workspace = Element::child(
        "Team",
        Some("ws".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("Tenant", "tenant"),
    )
    .unwrap();
    Element::child(
        "Book",
        Some("item".into()),
        NodeKind::Item(ItemType::parse("Notebook").unwrap()),
        workspace,
    )
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn native_resource_delete_requires_explicit_consent_unless_force_is_selected() {
    let target = delete_target();
    for (answer, force, expected_writes) in [
        ("yes\n", false, 1),
        ("no\n", false, 0),
        ("", false, 0),
        ("", true, 1),
    ] {
        let mut ui = NativeMutationUi::scripted(
            move |question| {
                fabric_cli::console::read_confirmation(
                    &mut Cursor::new(answer),
                    &mut Vec::new(),
                    question,
                )
            },
            |_| Ok(()),
        );
        let mut operations = Operations(0);
        let mut metadata = Metadata;
        let result = MutationHost {
            operations: &mut operations,
            ui: &mut ui,
            metadata: &mut metadata,
        }
        .remove(&target, force)
        .await;
        assert_eq!(operations.0, expected_writes);
        if !force && answer.is_empty() {
            assert_eq!(result.unwrap_err().code, "ConfirmationInputFailed");
        } else {
            let result = result.unwrap();
            assert_eq!(result.declined, expected_writes == 0);
            assert_eq!(result.outcome.is_some(), expected_writes == 1);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_native_resource_input_cannot_dispatch_after_a_late_yes() {
    let (release, wait) = mpsc::channel();
    let wait = Mutex::new(wait);
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let (finished, completion) = tokio::sync::oneshot::channel();
    let finished = Mutex::new(Some(finished));
    let mut ui = NativeMutationUi::scripted(
        move |_| {
            started.send(()).unwrap();
            wait.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .map_err(io::Error::other)?;
            finished.lock().unwrap().take().unwrap().send(()).unwrap();
            Ok(true)
        },
        |_| Ok(()),
    );
    let target = delete_target();
    let mut operations = Operations(0);
    let mut metadata = Metadata;
    let mut host = MutationHost {
        operations: &mut operations,
        ui: &mut ui,
        metadata: &mut metadata,
    };
    let mut pending = Box::pin(host.remove(&target, false));
    tokio::select! {
        result = &mut pending => panic!("Delete completed without input: {result:?}"),
        result = observed.recv() => assert_eq!(result, Some(())),
    }
    tokio::select! {
        result = &mut pending => panic!("Delete completed without input: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(1)) => {},
    }
    drop(pending);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), completion)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(operations.0, 0);
}
