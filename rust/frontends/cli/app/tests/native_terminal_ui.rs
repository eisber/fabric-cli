// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline tests of the exact host adapter with scripted streams and child pipes.

#![deny(unnameable_test_items)]

#[path = "../src/native_terminal_ui.rs"]
mod terminal_ui;

use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
use fabric_cli_mutations::{
    CommandFuture, MutationError, MutationHost, MutationMetadata, MutationUi, NoticeKind, RemovalUi,
};
use fabric_cli_resources::Confirmation;
use fabric_sdk_workspaces::mutations::{
    ResourceError, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use serde_json::Value;
use std::{
    io::{self, Cursor, Write},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use terminal_ui::{NativeAccessConfirmation, NativeMutationUi};

#[test]
fn pending_default_confirmation_releases_diagnostic_output_before_input() {
    const CHILD: &str = "FAB_TEST_PENDING_CONFIRMATION_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut confirmation = NativeAccessConfirmation::default();
        let result = runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_millis(250),
                confirmation.confirm("Pending confirmation"),
            )
            .await
        });
        assert!(result.is_err(), "empty open stdin must remain pending");
        runtime.shutdown_timeout(Duration::from_millis(100));
        io::stderr()
            .write_all(b"\nconfirmation timeout diagnostic\n")
            .unwrap();
        return;
    }

    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "pending_default_confirmation_releases_diagnostic_output_before_input",
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let mut child = command.spawn().unwrap();
    let input = child.stdin.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let completed = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        completed,
        "pending input retained the diagnostic stream after bounded shutdown"
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("Pending confirmation [Y/n] "));
    assert!(diagnostic.contains("confirmation timeout diagnostic"));
}

#[test]
fn pending_default_selection_releases_diagnostic_output_before_input() {
    const CHILD: &str = "FAB_TEST_PENDING_SELECTION_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut ui = NativeMutationUi::default();
        let names = vec!["Demo.Workspace".into()];
        let result = runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_millis(250),
                ui.select_result("Select workspaces:", &names),
            )
            .await
        });
        assert!(result.is_err(), "empty open stdin must remain pending");
        runtime.shutdown_timeout(Duration::from_millis(100));
        io::stderr()
            .write_all(b"\nselection timeout diagnostic\n")
            .unwrap();
        return;
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "pending_default_selection_releases_diagnostic_output_before_input",
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let mut child = command.spawn().unwrap();
    let input = child.stdin.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let completed = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        completed,
        "pending selection retained stderr after bounded shutdown"
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("Select workspaces:\n"));
    assert!(diagnostic.contains("selection timeout diagnostic"));
}

#[tokio::test(flavor = "current_thread")]
async fn access_questions_and_entered_answers_reach_the_existing_source_reader() {
    let input = Arc::new(Mutex::new(Cursor::new(b"YES\r\nno\n\n")));
    let output = Arc::new(Mutex::new(Vec::new()));
    let mut prompt = NativeAccessConfirmation::with_reader({
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
        let mut prompt = NativeAccessConfirmation::with_reader(move |question| {
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
        let mut prompt = NativeAccessConfirmation::with_reader({
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
    let mut interrupted = NativeAccessConfirmation::with_reader(|_| {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "input cancelled",
        ))
    });
    let error = interrupted.confirm("Are you sure?").await.unwrap_err();
    assert_eq!(error.code, "ConfirmationInputFailed");
    assert_eq!(error.message, "input cancelled");

    let mut panicking =
        NativeAccessConfirmation::with_reader(|_| panic!("synthetic worker failure"));
    let error = panicking.confirm("Are you sure?").await.unwrap_err();
    assert_eq!(error.code, "ConfirmationWorkerFailed");
    assert_eq!(
        error.message,
        "The confirmation input worker did not complete"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_pending_prompt_does_not_block_the_runtime_or_turn_cancellation_into_consent() {
    let (release, wait) = mpsc::channel();
    let wait = Mutex::new(wait);
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let (finished, completion) = tokio::sync::oneshot::channel();
    let finished = Mutex::new(Some(finished));
    let mut prompt = NativeAccessConfirmation::with_reader(move |_| {
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
    let mut prompt = NativeMutationUi::with_io(
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
        let mut prompt = NativeMutationUi::with_io(
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
        let mut prompt = NativeMutationUi::with_io(
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

#[tokio::test(flavor = "current_thread")]
async fn removal_selection_flushes_notices_and_preserves_input_worker_and_delivery_errors() {
    let names = vec!["Demo.Workspace".into()];
    let transcript = Arc::new(Mutex::new(Vec::new()));
    let mut ui = NativeMutationUi::with_selection_io(
        |_| panic!("selection must not ask for confirmation"),
        {
            let transcript = Arc::clone(&transcript);
            move |prompt, names| {
                fabric_cli::console::read_selection_with_prompt(
                    &mut Cursor::new(b"1\n"),
                    &mut *transcript.lock().unwrap(),
                    prompt,
                    names,
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
    ui.notice(NoticeKind::Progress, "Existing notice");
    assert_eq!(
        ui.select_result("Select workspaces:", &names)
            .await
            .unwrap(),
        names
    );
    let transcript = String::from_utf8(transcript.lock().unwrap().clone()).unwrap();
    assert!(transcript.starts_with("Existing notice\nSelect workspaces:\n"));
    assert!(ui.records.is_empty());

    for panicking in [false, true] {
        let reads = Arc::new(AtomicUsize::new(0));
        let mut ui = NativeMutationUi::with_selection_io(
            |_| panic!("failed selection must not confirm"),
            {
                let reads = Arc::clone(&reads);
                move |_, _| {
                    reads.fetch_add(1, Ordering::SeqCst);
                    Ok(Vec::new())
                }
            },
            move |_| {
                assert!(!panicking, "synthetic output worker failure");
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "notice failed"))
            },
        );
        ui.notice(NoticeKind::Warning, "Prior warning");
        let error = ui.select_result("Select items:", &names).await.unwrap_err();
        assert_eq!(
            error.code,
            if panicking {
                "SelectionWorkerFailed"
            } else {
                "SelectionOutputFailed"
            }
        );
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert!(ui.records.is_empty(), "failed output must not replay");
    }
    let mut ui = NativeMutationUi::with_selection_io(
        |_| panic!("failed selection must not confirm"),
        |_, _| panic!("synthetic input worker failure"),
        |_| Ok(()),
    );
    assert_eq!(
        ui.select_result("Select items:", &names)
            .await
            .unwrap_err()
            .code,
        "SelectionWorkerFailed"
    );
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
        let mut ui = NativeMutationUi::with_io(
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
    let mut ui = NativeMutationUi::with_io(
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
