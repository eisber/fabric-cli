// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Feature-enabled adapter rejection with child-process streams explicitly piped.

#![deny(unnameable_test_items)]
#![cfg(feature = "reedline-editor")]

use std::process::{Command, Stdio};

use fabric_cli_repl::{
    Options,
    editor::{EditorError, ReedlineEditor},
};

#[test]
fn redirected_constructor_child() {
    if std::env::var_os("FAB_REPL_REDIRECTED_TEST_CHILD").is_none() {
        return;
    }
    assert!(matches!(
        ReedlineEditor::new(Options::default()),
        Err(EditorError::UnsupportedTerminal)
    ));
    println!("redirected-terminal-rejected");
}

#[test]
fn redirected_stdin_stdout_are_rejected_without_raw_mode_or_fallback() {
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "redirected_constructor_child", "--nocapture"])
        .env("FAB_REPL_REDIRECTED_TEST_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("redirected-terminal-rejected")
    );
}
