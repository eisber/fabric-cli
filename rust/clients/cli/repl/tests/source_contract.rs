// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent source-captured lexical and raw-alias contract.

#![deny(unnameable_test_items)]

use fabric_cli_repl::{LineAction, Options, ShellNotice, classify_line, tokenize};
use serde_json::Value;

#[test]
fn pinned_source_cases_match_without_an_editor_or_application_dependency() {
    let contract: Value = serde_json::from_str(include_str!("contract.json")).unwrap();
    for case in contract["cases"].as_array().unwrap() {
        let line = case["line"].as_str().unwrap();
        if let Some(expected) = case["error"].as_str() {
            assert_eq!(
                tokenize(line, Options::default()).unwrap_err().to_string(),
                expected,
                "{line:?}"
            );
            assert!(classify_line(line, Options::default()).is_err());
            continue;
        }
        let tokens: Vec<String> = case["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            tokenize(line, Options::default()).unwrap(),
            tokens,
            "{line:?}"
        );
        let expected = match case["action"].as_str().unwrap() {
            "empty" => LineAction::Empty,
            "command" => LineAction::Command(tokens),
            "exit" => LineAction::Notice(ShellNotice::Exit),
            "help" => LineAction::Notice(ShellNotice::Help),
            "version" => LineAction::Notice(ShellNotice::Version),
            "prefix-hint" => LineAction::Notice(ShellNotice::PrefixHint),
            action => panic!("Unknown contract action: {action}"),
        };
        assert_eq!(
            classify_line(line, Options::default()).unwrap(),
            expected,
            "{line:?}"
        );
    }
}
