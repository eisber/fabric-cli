// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pinned Python scalar-width behavior through the actual offline read session.
//!
//! Expected text was captured from hash-guarded Python definitions at
//! cf67fd52ea8b47e24c725e73d792de6f4a08e2e4, with CPython 3.13.15 / UCD 15.1.

#![deny(unnameable_test_items)]

#[allow(dead_code)]
mod folder_support;

use fabric_cli::{
    ReadConfig,
    parser::{Invocation, ReadCommand, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_output::{
    Timestamp,
    production_unicode::UnicodeLimits,
    text::{TextError, TextPlan, UnicodeResourceKind},
};
use folder_support::{Credential, Http, Timer, complete, executor, reply, sdk};
use serde_json::{Value, json};
use std::{future::pending, sync::Arc};

fn command(args: &[&str]) -> ReadCommand {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("read required")
    };
    command
}

fn dispatch(command: &ReadCommand, pages: Vec<Value>) -> Result<TextPlan, TextError> {
    let http = Arc::new(Http::new(pages.into_iter().map(reply)));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let sdk = sdk(Arc::new(Credential::default()));
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let result = complete(session.dispatch(command, pending()));
    http.assert_consumed();
    result
}

fn stdout(plan: &TextPlan) -> String {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    assert_eq!(plan.exit_code, 0);
    assert!(stderr.is_empty());
    String::from_utf8(stdout).unwrap()
}

#[test]
fn wide_and_combining_names_use_source_scalar_table_alignment() {
    let plan = dispatch(
        &command(&[
            "ls",
            "/",
            "-q",
            "[].{name:name,id:id}",
            "--output_format",
            "text",
        ]),
        vec![json!({"value":[
            {"id":"w","displayName":"界","type":"Workspace"},
            {"id":"e","displayName":"e\u{301}","type":"Workspace"},
            {"id":"a","displayName":"A","type":"Workspace"},
            {"id":"v","displayName":"\u{1fae9}","type":"Workspace"}
        ]})],
    )
    .unwrap();
    // U+0301 and U+1FAE9 are width 1 in the pinned source, unlike Rich/UCD17 cells.
    assert_eq!(
        stdout(&plan),
        "name           id\n-----------------\nA.Workspace    a\ne\u{301}.Workspace   e\n界.Workspace   w\n\u{1fae9}.Workspace    v\n"
    );
}

#[test]
fn single_column_wrap_retains_python_scalar_boundaries() {
    for (name, expected) in [
        (
            "界".repeat(41),
            format!("{}\n{}.Workspace\n", "界".repeat(26), "界".repeat(15)),
        ),
        (
            "e\u{301}".repeat(41),
            format!("{}\ne\u{301}.Workspace\n", "e\u{301}".repeat(40)),
        ),
    ] {
        let plan = dispatch(
            &command(&["ls", "/", "--output_format", "text"]),
            vec![json!({"value":[{"id":"w","displayName":name,"type":"Workspace"}]})],
        )
        .unwrap();
        assert_eq!(stdout(&plan), expected);
    }
}

#[test]
fn unicode_json_bytes_and_default_format_are_unchanged() {
    let page = json!({"value":[{"id":"w","displayName":"界e\u{301}","type":"Workspace"}]});
    let default = dispatch(&command(&["ls", "/"]), vec![page.clone()]).unwrap();
    let explicit = dispatch(
        &command(&["ls", "/", "--output_format", "json"]),
        vec![page],
    )
    .unwrap();
    assert_eq!(default, explicit);
    assert_eq!(
        stdout(&default),
        "{\n    \"timestamp\": \"2000-01-01T00:00:00Z\",\n    \"status\": \"Success\",\n    \"command\": \"ls\",\n    \"result\": {\n        \"data\": [\n            {\n                \"name\": \"\\u754ce\\u0301.Workspace\"\n            }\n        ]\n    }\n}\n"
    );
}

#[test]
fn unsupported_format_fails_before_discovery_without_ascii_success() {
    let mut command = command(&["ls", "/"]);
    command.output_format = Some("unsupported".into());
    let error = dispatch(&command, Vec::new()).unwrap_err();
    assert_eq!(error, TextError::UnsupportedFormat("unsupported".into()));
    assert_eq!(
        error
            .command_error()
            .envelope(Timestamp::parse("2000-01-01T00:00:00Z").unwrap(), None)
            .result()
            .error_code(),
        Some("NotSupported")
    );
    assert!(error.partial_output().is_none());
}

#[test]
fn unicode_formatter_limits_propagate_without_ascii_retry() {
    let error = dispatch(
        &command(&[
            "get",
            "Demo.Workspace/Book.Notebook",
            "-q",
            "join('', [description, description])",
            "--output_format",
            "text",
        ]),
        vec![
            json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
            json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]}),
            json!({"id":"n","displayName":"Book","description":
                "é".repeat(UnicodeLimits::default().input_bytes / 4 + 1)}),
        ],
    )
    .unwrap_err();
    assert_eq!(
        error,
        TextError::UnicodeResource(UnicodeResourceKind::InputBytes)
    );
    assert!(error.partial_output().is_none());
}
