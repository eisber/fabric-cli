// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive regressions against independently executed, frozen Python captures.

use fabric_cli_output::{
    CommandError, Timestamp,
    text::{self, OutputFormat, Presentation, TextOptions},
};
use serde_json::Value;

#[test]
fn failed_text_envelopes_match_frozen_python_error_adapter() {
    let captures: Value = serde_json::from_str(include_str!(
        "../../../reviews/20261027/python-output-captures.json"
    ))
    .unwrap();
    for (name, command, code, message) in [
        ("get-text-error", "get", "Unauthorized", "Denied."),
        (
            "ls-text-query-error",
            "ls",
            "InvalidInput",
            "Invalid jmespath query (https://jmespath.org)",
        ),
        (
            "read-text-cancel",
            "get",
            "LongRunningOperationCancelled",
            "Operation cancelled",
        ),
        ("job-text-error", "job run-list", "Unauthorized", "Denied."),
    ] {
        let expected = captures["observations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|capture| capture["name"] == name)
            .unwrap();
        let envelope = CommandError::new(Some(message.into()), Some(code.into())).envelope(
            Timestamp::parse(captures["timestamp"].as_str().unwrap()).unwrap(),
            Some(command.into()),
        );
        let plan = text::render(
            &envelope,
            OutputFormat::Text,
            &TextOptions::default(),
            &text::ascii_width,
            Presentation::default(),
        )
        .unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        plan.write_to(&mut out, &mut err).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            expected["stdout"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            expected["stderr"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(u64::from(plan.exit_code), expected["exit"], "{name}");
    }
}
