// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual stream contracts for explicit terminal confirmation.

use fabric_cli::console::read_confirmation;
use std::io::{self, Cursor, Write};

#[test]
fn yes_no_and_entered_default_match_the_source_confirmation() {
    for (answer, expected) in [
        ("y\n", true),
        ("YES\r\n", true),
        ("\n", true),
        ("no\n", false),
        ("N\n", false),
    ] {
        let mut output = Vec::new();
        let confirmed =
            read_confirmation(&mut Cursor::new(answer), &mut output, "Overwrite?").unwrap();
        assert_eq!(confirmed, expected);
        assert_eq!(output, b"Overwrite? [Y/n] ");
    }
}

#[test]
fn absent_unterminated_invalid_and_oversized_answers_never_grant_consent() {
    for answer in ["", "yes", "sometimes\n", &format!("{}\n", "y".repeat(129))] {
        assert!(read_confirmation(&mut Cursor::new(answer), &mut Vec::new(), "Delete?").is_err());
    }
}

#[test]
fn a_confirmation_consumes_only_its_answer_line() {
    let mut input = Cursor::new(b"n\ny\n");
    assert!(!read_confirmation(&mut input, &mut Vec::new(), "First?").unwrap());
    assert!(read_confirmation(&mut input, &mut Vec::new(), "Second?").unwrap());
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed prompt"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed prompt"))
    }
}

#[test]
fn prompt_delivery_failure_precedes_input_consumption() {
    let mut input = Cursor::new(b"yes\n");
    let error = read_confirmation(&mut input, &mut BrokenWriter, "Continue?").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(input.position(), 0);
}
