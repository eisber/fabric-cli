// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit destructive confirmation without changing the v1 prompt default.

#![deny(unnameable_test_items)]

use fabric_cli::console::{read_confirmation, read_explicit_confirmation};
use std::io::{self, Cursor, Write};

#[test]
fn v2_empty_answer_declines_without_changing_v1_default() {
    let mut output = Vec::new();
    assert!(!read_explicit_confirmation(&mut Cursor::new(b"\n"), &mut output, "Delete?").unwrap());
    assert_eq!(output, b"Delete? [y/N] ");
    assert!(read_confirmation(&mut Cursor::new(b"\n"), &mut Vec::new(), "Delete?").unwrap());
}

#[test]
fn only_entered_affirmative_answers_approve() {
    for (answer, approved) in [
        ("y\n", true),
        ("YES\r\n", true),
        ("n\n", false),
        ("no\n", false),
    ] {
        assert_eq!(
            read_explicit_confirmation(
                &mut Cursor::new(answer.as_bytes()),
                &mut Vec::new(),
                "Delete?"
            )
            .unwrap(),
            approved
        );
    }
    for answer in ["", "yes", "approve\n", "maybe\n"] {
        assert!(
            read_explicit_confirmation(
                &mut Cursor::new(answer.as_bytes()),
                &mut Vec::new(),
                "Delete?"
            )
            .is_err()
        );
    }
}

struct BrokenPrompt;

impl Write for BrokenPrompt {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic prompt failure"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("synthetic flush failure"))
    }
}

#[test]
fn undelivered_prompt_cannot_be_approved() {
    assert!(
        read_explicit_confirmation(&mut Cursor::new(b"yes\n"), &mut BrokenPrompt, "Delete?")
            .is_err()
    );
}
