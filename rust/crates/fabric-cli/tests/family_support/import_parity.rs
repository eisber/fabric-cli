// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pinned Python import requests through the actual root/session/HTTP/files path.

#![cfg(feature = "native-read")]
#![deny(unnameable_test_items)]

use crate::family_support::{Harness, success};
use fabric_cli::families::FamilyEvidence;
use fabric_cli_output::CommandError;
use fabric_cli_transfer::{TransferEvent, TransferFuture, TransferUi};
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const CREATED: &str = "33333333-3333-3333-3333-333333333333";

#[derive(Default)]
struct Ui {
    events: Mutex<Vec<TransferEvent>>,
    questions: Mutex<Vec<String>>,
    answers: Mutex<std::collections::VecDeque<bool>>,
    cancel_on_progress: Option<Cancel>,
}

#[derive(Default)]
struct Signal {
    set: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

#[derive(Clone, Default)]
struct Cancel(Arc<Signal>);

impl Future for Cancel {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        *self.0.waker.lock().unwrap() = Some(cx.waker().clone());
        if self.0.set.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl TransferUi for Ui {
    fn emit(&self, event: TransferEvent) -> Result<(), CommandError> {
        if matches!(&event, TransferEvent::Grey(text) if text.contains("Checking again"))
            && let Some(cancel) = &self.cancel_on_progress
        {
            cancel.0.set.store(true, Ordering::Release);
            if let Some(waker) = cancel.0.waker.lock().unwrap().take() {
                waker.wake();
            }
        }
        self.events.lock().unwrap().push(event);
        Ok(())
    }

    fn confirm<'a>(&'a self, message: &'a str) -> TransferFuture<'a, Result<bool, CommandError>> {
        self.questions.lock().unwrap().push(message.into());
        Box::pin(async { Ok(self.answers.lock().unwrap().pop_front().unwrap()) })
    }

    fn select<'a>(
        &'a self,
        _: &'a [String],
    ) -> TransferFuture<'a, Result<Vec<String>, CommandError>> {
        panic!("import does not select export items")
    }
}

fn input(host: &Harness) {
    let root = host.root.join("input");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("Resources")).unwrap();
    std::fs::write(
        root.join("environment.yml"),
        b"dependencies:\r\n  - numpy\n",
    )
    .unwrap();
    std::fs::write(root.join("Resources").join("b.whl"), [0, 255, 13, 10]).unwrap();
    std::fs::write(root.join("Resources").join("a.jar"), b"jar").unwrap();
    std::fs::write(root.join("cache.abf"), b"excluded").unwrap();
}

#[tokio::test]
async fn pinned_environment_lifecycles_match_exact_requests_parts_effects_and_ui() {
    let source: Value = serde_json::from_str(include_str!(
        "../../../fabric-deploy/tests/definition_import_lifecycle_source.json"
    ))
    .unwrap();
    assert_eq!(source["source"], "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4");
    for case in source["cases"].as_array().unwrap() {
        let mut host = Harness::new();
        input(&host);
        let existing = case["existing"].as_bool().unwrap();
        host.metadata_with_id(
            WORKSPACE,
            if existing {
                json!([{"id":ITEM,"displayName":"Synthetic","type":"Environment"}])
            } else {
                json!([])
            },
        );
        let mut states = case["states"].as_array().unwrap().iter();
        let requests = case["requests"].as_array().unwrap();
        for request in requests {
            if request["method"] == "GET" {
                host.reply(
                    200,
                    json!({"properties":{"publishDetails":{"state": states.next().unwrap()}}}),
                );
            } else if request["path"].as_str().unwrap().ends_with("/items") {
                host.reply(201, json!({"id":CREATED}));
            } else {
                host.reply(200, Value::Null);
            }
        }
        let ui = Ui::default();
        let output = host
            .execute_with_capabilities(
                &[
                    "import",
                    "Demo.Workspace/Synthetic.Environment",
                    "-i",
                    "input",
                    "-f",
                ],
                pending(),
                &ui,
            )
            .await;
        if case["completed"] == true {
            success(&output);
            assert!(matches!(
                &output.evidence,
                FamilyEvidence::Transfer(Ok(result))
                    if result.effects.len() == 1
                    && result.effects[0].item_id == if existing {ITEM} else {CREATED}
                    && result.excluded_paths == ["cache.abf"]
            ));
        } else {
            assert!(matches!(
                &output.evidence,
                FamilyEvidence::Transfer(Err(error))
                    if error.effects.len() == 1 && error.effects[0].item_id == CREATED
            ));
            assert_ne!(output.plan.exit_code, 0);
        }
        let actual = host.requests();
        assert_eq!(actual.len(), requests.len() + 2, "{case}");
        for (actual, expected) in actual[2..].iter().zip(requests) {
            assert_eq!(actual["method"], expected["method"]);
            assert_eq!(
                actual["url"],
                format!(
                    "https://api.fabric.microsoft.com{}",
                    expected["path"].as_str().unwrap()
                )
            );
            assert_eq!(actual["raw_body"], expected["body"]);
        }
        let expected = case["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event.get("info").or_else(|| event.get("progress")))
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let actual = ui
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                TransferEvent::Grey(text) if text.contains("Environment publish") => {
                    Some(text.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
}

#[tokio::test]
async fn import_confirmation_and_declined_overwrite_retain_source_cache_semantics() {
    for answers in [vec![false], vec![true, false], vec![true, true]] {
        let mut host = Harness::new();
        input(&host);
        host.metadata_with_id(
            WORKSPACE,
            json!([{"id":ITEM,"displayName":"Synthetic","type":"DataPipeline"}]),
        );
        if answers == [true, true] {
            host.reply(200, Value::Null);
        }
        let ui = Ui {
            answers: Mutex::new(answers.clone().into()),
            ..Ui::default()
        };
        let output = host
            .execute_with_capabilities(
                &[
                    "import",
                    "Demo.Workspace/Synthetic.DataPipeline",
                    "-i",
                    "input",
                ],
                pending(),
                &ui,
            )
            .await;
        success(&output);
        assert_eq!(
            ui.questions.lock().unwrap().as_slice(),
            if answers.len() == 1 {
                vec!["Are you sure?"]
            } else {
                vec!["Are you sure?", "Overwrite?"]
            }
        );
        let FamilyEvidence::Transfer(Ok(result)) = &output.evidence else {
            panic!("{:?}", output.evidence)
        };
        assert_eq!(result.effects.len(), usize::from(answers[0]));
        assert_eq!(
            host.requests().len(),
            if answers == [true, true] { 3 } else { 2 }
        );
    }
}

#[tokio::test]
async fn report_connection_is_raw_and_bypath_is_source_invalid_payload_before_cache_exclusion() {
    for (name, bytes, valid) in [
        (
            "definition.pbir",
            br#"{"datasetReference":{"byConnection":{"id":"model"}}}"#.as_slice(),
            true,
        ),
        (
            "definition.pbir",
            br#"{"datasetReference":{"byPath":null}}"#.as_slice(),
            true,
        ),
        (
            "cache.abf.definition.pbir",
            br#"{"datasetReference":{"byPath":{"path":"../Model"}}}"#.as_slice(),
            false,
        ),
        (
            "definition.pbir",
            br#"{"datasetReference":null}"#.as_slice(),
            false,
        ),
        ("definition.pbir", b"[]".as_slice(), false),
    ] {
        let mut host = Harness::new();
        let root = host.root.join("input");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join(name), bytes).unwrap();
        host.metadata_with_id(WORKSPACE, json!([]));
        if valid {
            host.reply(201, json!({"id":CREATED}));
        }
        let output = host
            .execute(&[
                "import",
                "Demo.Workspace/Synthetic.Report",
                "-i",
                "input",
                "-f",
            ])
            .await;
        if valid {
            success(&output);
            let requests = host.requests();
            let encoded = requests[2]["body"]["definition"]["parts"][0]["payload"]
                .as_str()
                .unwrap();
            assert_eq!(
                encoded,
                if bytes.windows(12).any(|window| window == b"byConnection") {
                    "eyJkYXRhc2V0UmVmZXJlbmNlIjp7ImJ5Q29ubmVjdGlvbiI6eyJpZCI6Im1vZGVsIn19fQ=="
                } else {
                    "eyJkYXRhc2V0UmVmZXJlbmNlIjp7ImJ5UGF0aCI6bnVsbH19"
                }
            );
        } else {
            assert_ne!(output.plan.exit_code, 0);
            assert_eq!(host.requests().len(), 2);
            assert!(
                matches!(&output.evidence, FamilyEvidence::Transfer(Err(error)) if error.effects.is_empty())
            );
        }
    }
}

#[tokio::test]
async fn environment_create_and_publish_use_shared_lro_results_without_replaying_posts() {
    const CREATE_OPERATION: &str = "44444444-4444-4444-4444-444444444444";
    const PUBLISH_OPERATION: &str = "55555555-5555-5555-5555-555555555555";
    let mut host = Harness::new();
    input(&host);
    host.metadata_with_id(WORKSPACE, json!([]));
    for (operation, result) in [
        (CREATE_OPERATION, json!({"id":CREATED})),
        (PUBLISH_OPERATION, json!({})),
    ] {
        let location = format!("https://api.fabric.microsoft.com/v1/operations/{operation}");
        let result_location = format!("{location}/result");
        host.headers(
            202,
            Value::Null,
            &[
                ("Location", &location),
                ("x-ms-operation-id", operation),
                ("Retry-After", "0"),
            ],
        );
        host.headers(
            200,
            json!({"status":"Succeeded"}),
            &[("Location", &result_location)],
        );
        host.reply(200, result);
    }
    host.reply(
        200,
        json!({"properties":{"publishDetails":{"state":"success"}}}),
    );
    let output = host
        .execute(&[
            "import",
            "Demo.Workspace/Synthetic.Environment",
            "-i",
            "input",
            "-f",
        ])
        .await;
    success(&output);
    assert!(matches!(
        &output.evidence,
        FamilyEvidence::Transfer(Ok(result)) if result.effects[0].item_id == CREATED
    ));
    let requests = host.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "POST")
            .count(),
        2
    );
    assert_eq!(requests.len(), 9);
    assert!(
        requests[5]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/environments/{CREATED}/staging/publish"))
    );
}

#[tokio::test]
async fn environment_local_cancel_during_publish_backoff_keeps_create_effect_and_uncertainty() {
    let mut host = Harness::new();
    input(&host);
    host.metadata_with_id(WORKSPACE, json!([]));
    host.reply(201, json!({"id":CREATED}));
    host.reply(200, Value::Null);
    host.reply(
        200,
        json!({"properties":{"publishDetails":{"state":"running"}}}),
    );
    let cancel = Cancel::default();
    let ui = Ui {
        cancel_on_progress: Some(cancel.clone()),
        ..Ui::default()
    };
    let output = host
        .execute_with_capabilities(
            &[
                "import",
                "Demo.Workspace/Synthetic.Environment",
                "-i",
                "input",
                "-f",
            ],
            cancel,
            &ui,
        )
        .await;
    let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
        panic!("cancelled publication must not succeed")
    };
    assert_eq!(error.effects[0].item_id, CREATED);
    let fabric_cli_transfer::TransferFailure::Definition(error) = &error.failure else {
        panic!("original shared operation evidence must survive")
    };
    let fabric_deploy::definitions::DefinitionErrorKind::Operation(error) = &error.kind else {
        panic!("original shared operation evidence must survive")
    };
    assert_eq!(
        error.kind,
        fabric_sdk_operations::OperationErrorKind::Cancelled
    );
    assert!(error.mutation_may_have_occurred);
    assert_eq!(host.requests().len(), 5);
}

#[tokio::test]
async fn environment_readiness_permission_and_malformed_publish_state_are_not_success() {
    for existing in [false, true] {
        let mut host = Harness::new();
        input(&host);
        host.metadata_with_id(
            WORKSPACE,
            if existing {
                json!([{"id":ITEM,"displayName":"Synthetic","type":"Environment"}])
            } else {
                json!([])
            },
        );
        if existing {
            host.reply(403, json!({"errorCode":"Denied","message":"synthetic"}));
        } else {
            host.reply(201, json!({"id":CREATED}));
            host.reply(200, Value::Null);
            host.reply(200, json!({"properties":{"publishDetails":{"state":null}}}));
        }
        let output = host
            .execute(&[
                "import",
                "Demo.Workspace/Synthetic.Environment",
                "-i",
                "input",
                "-f",
            ])
            .await;
        assert_ne!(output.plan.exit_code, 0);
        let FamilyEvidence::Transfer(Err(error)) = &output.evidence else {
            panic!("real permission and malformed response errors must not succeed")
        };
        assert_eq!(error.effects.len(), usize::from(!existing));
        assert_eq!(host.requests().len(), if existing { 3 } else { 5 });
        if existing {
            assert!(
                host.requests()
                    .iter()
                    .all(|request| request["method"] != "POST")
            );
        }
    }
}
