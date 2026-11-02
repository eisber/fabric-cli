// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Terminal answers through root export dispatch, scripted HTTP and real files.

use crate::family_support::{Harness, success};
use fabric_cli::{
    console::{read_confirmation, read_selection},
    families::FamilyEvidence,
};
use fabric_cli_output::{CommandError, Timestamp};
use fabric_cli_transfer::{TransferEvent, TransferFailure, TransferFuture, TransferUi};
use serde_json::json;
use std::{
    future::pending,
    io::{self, Cursor},
    sync::Mutex,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ZULU: &str = "22222222-2222-2222-2222-222222222222";
const ALPHA: &str = "33333333-3333-3333-3333-333333333333";
const BETA: &str = "44444444-4444-4444-4444-444444444444";

struct Streams {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}

struct Ui(Mutex<Streams>);

impl Ui {
    fn new(answers: &str) -> Self {
        Self(Mutex::new(Streams {
            input: Cursor::new(answers.as_bytes().to_vec()),
            output: Vec::new(),
        }))
    }

    fn output(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().output.clone()).unwrap()
    }
}

fn input_error(error: io::Error, code: &str) -> CommandError {
    CommandError::new(Some(error.to_string()), Some(code.into()))
}

impl TransferUi for Ui {
    fn emit(&self, _: TransferEvent) -> Result<(), CommandError> {
        Ok(())
    }

    fn confirm<'a>(&'a self, question: &'a str) -> TransferFuture<'a, Result<bool, CommandError>> {
        Box::pin(async move {
            let Streams { input, output } = &mut *self.0.lock().unwrap();
            read_confirmation(input, output, question)
                .map_err(|error| input_error(error, "ConfirmationInputFailed"))
        })
    }

    fn select<'a>(
        &'a self,
        names: &'a [String],
    ) -> TransferFuture<'a, Result<Vec<String>, CommandError>> {
        Box::pin(async move {
            let Streams { input, output } = &mut *self.0.lock().unwrap();
            read_selection(input, output, names)
                .map_err(|error| input_error(error, "SelectionInputFailed"))
        })
    }
}

fn workspace(host: &Harness) {
    std::fs::create_dir(host.root.join("output")).unwrap();
    host.metadata_with_id(
        WORKSPACE,
        json!([
            {"id":ZULU,"displayName":"Zulu","type":"DataPipeline"},
            {"id":ALPHA,"displayName":"Alpha","type":"DataPipeline"},
            {"id":BETA,"displayName":"Beta","type":"DataPipeline"},
        ]),
    );
}

fn definition(host: &Harness) {
    host.reply(200, json!({}));
    host.reply(
        200,
        json!({"definition":{"parts":[{
            "path":"pipeline-content.json","payload":"e30=","payloadType":"InlineBase64",
        }]}}),
    );
}

#[tokio::test]
async fn terminal_subset_exports_only_selected_items_in_discovery_order() {
    let mut host = Harness::new();
    workspace(&host);
    definition(&host);
    definition(&host);
    let ui = Ui::new("3,1\n");
    let output = host
        .execute_with_capabilities(
            &["export", "Demo.Workspace", "-o", "output", "--force"],
            pending(),
            &ui,
        )
        .await;
    success(&output);
    assert!(matches!(
        &output.evidence,
        FamilyEvidence::Transfer(Ok(result))
            if result.completed_paths == [
                "Zulu.DataPipeline/pipeline-content.json",
                "Alpha.DataPipeline/pipeline-content.json",
            ]
    ));
    let file = host
        .root
        .join("output")
        .join("Alpha.DataPipeline")
        .join("pipeline-content.json");
    let contents: serde_json::Value =
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(contents, json!({}));
    assert!(!host.root.join("output").join("Beta.DataPipeline").exists());
    let prompts = ui.output();
    assert!(
        prompts.contains("1. Alpha.DataPipeline\n  2. Beta.DataPipeline\n  3. Zulu.DataPipeline")
    );
    assert!(!prompts.contains("[Y/n]"));
    let requests = host.requests();
    assert!(
        requests[2]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/items/{ZULU}"))
    );
    assert!(
        requests[4]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/items/{ALPHA}"))
    );
}

#[tokio::test]
async fn empty_or_invalid_selection_never_reads_definitions_or_writes_files() {
    for (answer, code) in [
        ("\n", None),
        ("", Some("SelectionInputFailed")),
        ("4\n", Some("SelectionInputFailed")),
    ] {
        let mut host = Harness::new();
        workspace(&host);
        let ui = Ui::new(answer);
        let output = host
            .execute_with_capabilities(
                &["export", "Demo.Workspace", "-o", "output", "-f"],
                pending(),
                &ui,
            )
            .await;
        if let Some(code) = code {
            let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
                panic!("failed selection must remain an error")
            };
            let TransferFailure::Command(error) = &error.failure else {
                panic!("selection failure must retain its command error")
            };
            assert_eq!(
                error
                    .envelope(Timestamp::parse("2026-10-28T12:00:00Z").unwrap(), None)
                    .result()
                    .error_code(),
                Some(code)
            );
        } else {
            assert!(success(&output).contains("No items selected"));
        }
        assert_eq!(host.requests().len(), 2);
        assert_eq!(
            std::fs::read_dir(host.root.join("output")).unwrap().count(),
            0
        );
    }
}

#[tokio::test]
async fn all_skips_selection_but_only_force_skips_confirmation() {
    for (force, answer, accepted) in [
        (false, "no\n", false),
        (false, "yes\n", true),
        (true, "", true),
    ] {
        let mut host = Harness::new();
        workspace(&host);
        if accepted {
            for _ in 0..3 {
                definition(&host);
            }
        }
        let ui = Ui::new(answer);
        let mut args = vec!["export", "Demo.Workspace", "-o", "output", "--all"];
        if force {
            args.push("--force");
        }
        let output = host.execute_with_capabilities(&args, pending(), &ui).await;
        success(&output);
        assert!(!ui.output().contains("Select items:"));
        assert_eq!(ui.output().contains("[Y/n]"), !force);
        assert!(matches!(
            &output.evidence,
            FamilyEvidence::Transfer(Ok(result))
                if result.declined == !accepted
                    && result.completed_paths.len() == if accepted { 3 } else { 0 }
        ));
        assert_eq!(
            std::fs::read_dir(host.root.join("output")).unwrap().count(),
            if accepted { 3 } else { 0 }
        );
    }
}

#[tokio::test]
async fn cli_workspace_guard_still_precedes_any_prompt_or_write() {
    let mut host = Harness::new();
    std::fs::create_dir(host.root.join("output")).unwrap();
    host.reply(
        200,
        json!({"value":[{"id":WORKSPACE,"displayName":"Demo","type":"Workspace"}]}),
    );
    let ui = Ui::new("all\nyes\n");
    let output = host
        .execute_with_capabilities(
            &["export", "Demo.Workspace", "-o", "output"],
            pending(),
            &ui,
        )
        .await;
    assert!(matches!(&output.evidence, FamilyEvidence::Transfer(Err(_))));
    assert!(ui.output().is_empty());
    assert_eq!(ui.0.lock().unwrap().input.position(), 0);
    let requests = host.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]["url"]
            .as_str()
            .unwrap()
            .ends_with("/v1/workspaces")
    );
    assert_eq!(
        std::fs::read_dir(host.root.join("output")).unwrap().count(),
        0
    );
}
