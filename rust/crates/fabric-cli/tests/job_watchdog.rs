// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[allow(dead_code)]
#[path = "../src/native_settings.rs"]
mod settings;

use fabric_cli_jobs::{JobConfig, command_from_matches, job_subcommand};
use std::time::Duration;

fn selected(args: &[&str], cancel: bool) -> Option<Duration> {
    let matches = job_subcommand().try_get_matches_from(args).unwrap();
    settings::command_watchdog(
        Some(&command_from_matches(&matches).unwrap()),
        JobConfig {
            cancel_on_timeout: cancel,
        },
    )
    .unwrap()
}

#[test]
fn omitted_runtime_negative_runtime_and_cancel_wait_have_no_arbitrary_watchdog() {
    for cancel in [false, true] {
        for args in [
            vec!["job", "run", "Book.Notebook"],
            vec!["job", "run", "Book.Notebook", "--timeout=-1"],
            vec!["job", "run-cancel", "Book.Notebook", "--id=run", "--wait"],
        ] {
            assert_eq!(selected(&args, cancel), None);
        }
    }
}

#[test]
fn finite_runtime_retains_elapsed_cleanup_budget_and_other_commands_retain_cap() {
    for args in [
        vec!["job", "start", "Book.Notebook"],
        vec!["job", "run-list", "Book.Notebook"],
        vec!["job", "run-status", "Book.Notebook", "--id=run"],
        vec!["job", "run-cancel", "Book.Notebook", "--id=run"],
        vec!["job", "run-rm", "Book.Notebook", "--id=schedule", "--force"],
    ] {
        assert_eq!(selected(&args, true), Some(Duration::from_secs(240)));
    }
    assert_eq!(
        selected(&["job", "run", "Book.Notebook", "--timeout=600"], false),
        Some(Duration::from_secs(240 + 1200 + 240 + 60)),
    );
    assert_eq!(
        selected(&["job", "run", "Book.Notebook", "--timeout=600"], true),
        Some(Duration::from_secs(240 + 1200 + 240 + 60 + 240 + 100 * 300)),
    );
}
