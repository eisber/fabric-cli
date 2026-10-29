// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

//! Bounded stream contracts for the executable's item-selection adapter.

use fabric_cli::console::{read_confirmation, read_selection, read_selection_with_prompt};
use std::io::{self, Cursor, Read, Write};

fn names() -> Vec<String> {
    vec![
        "First.Notebook".into(),
        "Second.Report".into(),
        "Third.DataPipeline".into(),
    ]
}

#[test]
fn selected_names_keep_display_order_and_duplicates_are_collapsed() {
    let mut output = Vec::new();
    let selected = read_selection(&mut Cursor::new("3, 1,3\r\n"), &mut output, &names()).unwrap();
    assert_eq!(selected, ["First.Notebook", "Third.DataPipeline"]);
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "Select items:\n  1. First.Notebook\n  2. Second.Report\n  3. Third.DataPipeline\n\
Enter comma-separated item numbers, 'all', or Enter for none: "
    );
}

#[test]
fn all_empty_and_no_choices_are_distinct_valid_selections() {
    for answer in ["all\n", " ALL \r\n"] {
        assert_eq!(
            read_selection(&mut Cursor::new(answer), &mut Vec::new(), &names()).unwrap(),
            names()
        );
    }
    assert!(
        read_selection(&mut Cursor::new("\n"), &mut Vec::new(), &names())
            .unwrap()
            .is_empty()
    );
    let mut output = Vec::new();
    let mut input = Cursor::new("untouched");
    assert!(
        read_selection(&mut input, &mut output, &[])
            .unwrap()
            .is_empty()
    );
    assert!(output.is_empty());
    assert_eq!(input.position(), 0);
}

#[test]
fn invalid_or_absent_input_never_selects_any_items() {
    for answer in [
        "",
        "1",
        "all",
        "0\n",
        "4\n",
        "-1\n",
        "+1\n",
        "1.5\n",
        "1,,2\n",
        "1,\n",
        "1 2\n",
        "unknown\n",
        "999999999999999999999999\n",
    ] {
        assert!(
            read_selection(&mut Cursor::new(answer), &mut Vec::new(), &names()).is_err(),
            "{answer:?}"
        );
    }
}

#[test]
fn selection_consumes_only_its_line_leaving_confirmation_to_the_caller() {
    let mut input = Cursor::new("2\nno\n");
    assert_eq!(
        read_selection(&mut input, &mut Vec::new(), &names()).unwrap(),
        ["Second.Report"]
    );
    assert!(!read_confirmation(&mut input, &mut Vec::new(), "Export?").unwrap());
}

#[test]
fn supplied_workspace_prompt_preserves_the_shared_selection_and_confirmation_contract() {
    let names = vec!["A.Workspace".into(), "B.Workspace".into()];
    let mut input = Cursor::new("2,1,2\nno\n");
    let mut output = Vec::new();
    assert_eq!(
        read_selection_with_prompt(&mut input, &mut output, "Select workspaces:", &names).unwrap(),
        names
    );
    assert!(
        String::from_utf8(output)
            .unwrap()
            .starts_with("Select workspaces:\n  1. A.Workspace\n  2. B.Workspace\n")
    );
    assert!(!read_confirmation(&mut input, &mut Vec::new(), "Are you sure?").unwrap());
}

#[test]
fn labels_escape_terminal_controls_but_selected_names_are_unchanged() {
    let names = vec![
        "Report\n\u{1b}[31m\u{7}.Report".into(),
        "Caf\u{e9}.Notebook".into(),
    ];
    let mut output = Vec::new();
    assert_eq!(
        read_selection(&mut Cursor::new("all\n"), &mut output, &names).unwrap(),
        names
    );
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains(r"Report\n\u{1b}[31m\u{7}.Report"));
    assert!(output.contains("Caf\u{e9}.Notebook"));
    assert!(!output.contains('\u{1b}'));
}

#[test]
fn oversized_input_is_bounded_and_not_a_selection() {
    let mut input = Cursor::new(format!("{}\n", "1,".repeat(40_000)));
    assert!(read_selection(&mut input, &mut Vec::new(), &names()).is_err());
    assert_eq!(input.position(), 65_537);
}

struct BrokenWriter {
    flush_only: bool,
}

impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.flush_only {
            Ok(bytes.len())
        } else {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed prompt"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed prompt"))
    }
}

#[test]
fn prompt_write_and_flush_failures_precede_input_consumption() {
    for flush_only in [false, true] {
        let mut input = Cursor::new("all\n");
        let error =
            read_selection(&mut input, &mut BrokenWriter { flush_only }, &names()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(input.position(), 0);
    }
}

struct BrokenReader;

impl Read for BrokenReader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("input unavailable"))
    }
}

impl io::BufRead for BrokenReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        Err(io::Error::other("input unavailable"))
    }

    fn consume(&mut self, _: usize) {}
}

#[test]
fn input_failures_are_reported_without_echoing_an_answer() {
    let error = read_selection(&mut BrokenReader, &mut Vec::new(), &names()).unwrap_err();
    assert_eq!(error.to_string(), "input unavailable");
    let error = read_selection(&mut Cursor::new(b"\xff\n"), &mut Vec::new(), &names()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}
