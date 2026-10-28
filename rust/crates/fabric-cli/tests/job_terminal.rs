// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli::jobs::terminal::confirm_with;
use std::io::{self, Cursor, Write};

#[test]
fn actual_terminal_question_is_default_no_and_only_explicit_yes_accepts() {
    for (answer, expected) in [
        ("y\n", true),
        ("YES\r\n", true),
        ("n\n", false),
        ("\n", false),
        ("", false),
        ("anything\n", false),
    ] {
        let mut prompt = Vec::new();
        assert_eq!(
            confirm_with(&mut Cursor::new(answer), &mut prompt).unwrap(),
            expected
        );
        assert_eq!(prompt, b"Are you sure? [y/N] ");
    }
}

struct FailedFlush;

impl Write for FailedFlush {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("synthetic flush failure"))
    }
}

#[test]
fn flush_failure_is_typed_and_does_not_consume_consent() {
    let mut input = Cursor::new("yes\n");
    let error = confirm_with(&mut input, &mut FailedFlush).unwrap_err();
    assert_eq!(error.error_code(), Some("LocalIOError"));
    assert_eq!(input.position(), 0);
}
