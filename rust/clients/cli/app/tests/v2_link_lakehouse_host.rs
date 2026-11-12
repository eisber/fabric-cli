// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline executable-host contracts; no Fabric credential or resource is used.
#![deny(unnameable_test_items)]

use fabric_cli::v2_link_lakehouse_host::{self as host, LocalFiles};
use fabric_cli_mutations::{
    CommandFuture, MutationError,
    v2_link::{LinkApproval, LinkEffects, LinkScope, PreparedLink, ResolvedLinkTarget},
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{self, Cursor, Read, Write},
};

const WS: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const SOURCE: &str = "33333333-3333-4333-8333-333333333333";
const CONNECTION: &str = "44444444-4444-4444-8444-444444444444";

fn target(parent: &str) -> String {
    format!("/{WS}.Workspace/{ITEM}.Lakehouse/{parent}/sales.Shortcut")
}

fn body(parent: &str) -> Value {
    json!({"path":parent,"name":"sales","target":{"oneLake":{
        "workspaceId":WS,"itemId":SOURCE,"path":"Tables/sales"
    }}})
}

fn prepare(
    path: &str,
    bytes: &[u8],
    facade: bool,
    wait: bool,
) -> Result<PreparedLink, MutationError> {
    let mut args = vec!["fab-cli-v2"];
    if facade {
        args.extend(["artifact", "lakehouse"]);
    }
    args.extend(["link", path, "--input", "-"]);
    if wait {
        args.push("--wait");
    }
    let command = host::parse(args).unwrap();
    host::prepare(
        &command,
        &mut LocalFiles {
            cwd: std::path::Path::new("."),
            stdin: &mut Cursor::new(bytes),
        },
    )
    .now_or_never()
    .unwrap()
}

fn prepared(parent: &str, facade: bool, wait: bool) -> PreparedLink {
    prepare(
        &target(parent),
        &serde_json::to_vec(&body(parent)).unwrap(),
        facade,
        wait,
    )
    .unwrap()
}

#[derive(Default)]
struct Approval {
    allow: bool,
    fail: bool,
    calls: Vec<(ResolvedLinkTarget, LinkEffects)>,
}

impl LinkApproval for Approval {
    fn approve<'a>(
        &'a mut self,
        target: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        self.calls.push((target.clone(), effects));
        Box::pin(std::future::ready(if self.fail {
            Err(MutationError::new(
                "ApprovalUnavailable",
                "Policy unavailable",
            ))
        } else {
            Ok(self.allow)
        }))
    }
}

#[derive(Default)]
struct Operations {
    calls: Vec<ResourceOperation>,
    replies: VecDeque<Result<ResourceOutcome, ResourceError>>,
}

impl ResourceOperations for Operations {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> CommandFuture<'_, Result<ResourceOutcome, ResourceError>> {
        self.calls.push(operation);
        Box::pin(std::future::ready(
            self.replies
                .pop_front()
                .expect("unexpected request or replay"),
        ))
    }
}

fn operations(outcome: Result<ResourceOutcome, ResourceError>) -> Operations {
    Operations {
        calls: vec![],
        replies: [
            Ok(ResourceOutcome::Completed {
                status: 200,
                data: Some(json!({"id":ITEM,"type":"Lakehouse"})),
            }),
            outcome,
        ]
        .into(),
    }
}

fn completed(parent: &str) -> ResourceOutcome {
    ResourceOutcome::Completed {
        status: 201,
        data: Some(
            json!({"path":parent,"name":"sales","target":{"privateSource":"not for receipt"}}),
        ),
    }
}

#[test]
fn root_and_facade_preserve_exact_abort_body_and_completion_policy() {
    host::command().debug_assert();
    for parent in ["Files", "Files/landing/deep folder", "Tables"] {
        for facade in [false, true] {
            for wait in [false, true] {
                let prepared = prepared(parent, facade, wait);
                assert_eq!(prepared.operation().method(), ResourceMethod::Post);
                assert_eq!(
                    prepared.operation().path(),
                    format!(
                        "/v1/workspaces/{WS}/items/{ITEM}/shortcuts?shortcutConflictPolicy=Abort"
                    )
                );
                assert_eq!(prepared.operation().body(), Some(&body(parent)));
                assert_eq!(
                    prepared.operation().completion(),
                    if wait {
                        CompletionPolicy::WaitForResult
                    } else {
                        CompletionPolicy::ReturnAccepted
                    }
                );
                assert_eq!(
                    prepared.target().scope(),
                    &LinkScope::LakehouseShortcut {
                        path: parent.into(),
                        name: "sales".into()
                    }
                );
            }
        }
    }
}

struct Unread;
impl Read for Unread {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("invalid target must precede input");
    }
}

#[test]
fn wrong_owners_and_non_relationship_targets_fail_before_input() {
    for path in [
        target("Files").replace(".Lakehouse", ".Warehouse"),
        target("Files").replace(".Lakehouse", ".KQLDatabase"),
        target("Files").replace(".Shortcut", ""),
        target("Files").replace(".Shortcut", ".shortcut"),
        format!("{}/child", target("Files")),
        target("Files/../unsafe"),
        target("Files/source.Shortcut"),
        target("Files/source.shortcut"),
        target("Files//unsafe"),
        target("Files").replace(WS, "friendly-name"),
        target("Files").replace("Files", "docs"),
    ] {
        let command = host::parse(["fab-cli-v2", "link", &path, "--input", "-"]).unwrap();
        let error = host::prepare(
            &command,
            &mut LocalFiles {
                cwd: std::path::Path::new("."),
                stdin: &mut Unread,
            },
        )
        .now_or_never()
        .unwrap()
        .unwrap_err();
        assert_eq!(error.code, "InvalidTarget");
    }
}

#[test]
fn only_the_final_relationship_marker_is_removed() {
    let prepared = prepared("Files/archive.ShortcutArchive", false, false);
    assert_eq!(
        prepared.operation().body().unwrap()["path"],
        "Files/archive.ShortcutArchive"
    );
    let mut input = body("Files");
    input["name"] = "sales.Shortcut".into();
    let prepared = prepare(
        &target("Files").replace("sales.Shortcut", "sales.Shortcut.Shortcut"),
        &serde_json::to_vec(&input).unwrap(),
        false,
        false,
    )
    .unwrap();
    assert_eq!(
        prepared.operation().body().unwrap()["name"],
        "sales.Shortcut"
    );
}

#[test]
fn strict_native_input_and_body_conflicts_precede_all_effects() {
    let valid = body("Files");
    for input in [
        br#"{"path":"Files","name":"sales","target":{},"unknown":true}"#.to_vec(),
        br#"{"path":"Files","path":"Files","name":"sales","target":{}}"#.to_vec(),
        format!(r#"{{"path":"Files","name":"sales","target":{{"oneLake":{{"workspaceId":"{WS}","workspaceId":"{WS}","itemId":"{SOURCE}","path":"Files/a"}}}}}}"#).into_bytes(),
        b"{}".to_vec(), b"[]".to_vec(), b"null".to_vec(), vec![0xff],
        format!("{valid} {valid}").into_bytes(),
        vec![b' ';host::MAX_INPUT_BYTES+1],
        serde_json::to_vec(&body("Files/other")).unwrap(),
    ] {
        assert!(prepare(&target("Files"), &input, false, false).is_err());
    }
    for parent in [
        "Tables/schema",
        "Tables/schema/table",
        "TablesWithSuffix",
        "Files/..",
    ] {
        assert!(
            prepare(
                &target(parent),
                &serde_json::to_vec(&body(parent)).unwrap(),
                false,
                false
            )
            .is_err()
        );
    }
    for option in [
        "--force",
        "--overwrite",
        "--bulk",
        "--copy",
        "--dereference",
    ] {
        assert!(
            host::parse([
                "fab-cli-v2",
                "link",
                &target("Files"),
                "--input",
                "-",
                option
            ])
            .is_err()
        );
    }
    assert!(
        host::parse([
            "fab-cli-v2",
            "artifact",
            "semantic-model",
            "link",
            &target("Files"),
            "--input",
            "-"
        ])
        .is_err()
    );
}

#[test]
fn all_native_target_kinds_keep_provider_validation_and_sensitivity_effect() {
    for native_target in [
        json!({"oneLake":{"workspaceId":WS,"itemId":SOURCE,"path":"Files/source"}}),
        json!({"adlsGen2":{"connectionId":CONNECTION,"location":"https://storage.dfs.core.windows.net","subpath":"/container/folder"}}),
        json!({"amazonS3":{"connectionId":CONNECTION,"location":"https://bucket.s3.us-west-2.amazonaws.com","subpath":"/folder"}}),
        json!({"googleCloudStorage":{"connectionId":CONNECTION,"location":"https://bucket.storage.googleapis.com","subpath":"/folder"}}),
        json!({"s3Compatible":{"connectionId":CONNECTION,"location":"https://s3.example.invalid","bucket":"bucket","subpath":"/folder"}}),
        json!({"azureBlobStorage":{"connectionId":CONNECTION,"location":"https://storage.blob.core.windows.net","subpath":"container/folder"}}),
        json!({"dataverse":{"connectionId":CONNECTION,"environmentDomain":"https://org.crm4.dynamics.com","tableName":"account","deltaLakeFolder":"folder/account"}}),
        json!({"oneDriveSharePoint":{"connectionId":CONNECTION,"location":"https://contoso.sharepoint.com","subpath":"/Shared Documents/folder","updateFabricItemSensitivity":true}}),
    ] {
        let mut input = body("Files/landing");
        input["target"] = native_target;
        let prepared = prepare(
            &target("Files/landing"),
            &serde_json::to_vec(&input).unwrap(),
            true,
            false,
        )
        .unwrap();
        assert_eq!(
            prepared.effects().update_item_sensitivity,
            input
                .pointer("/target/oneDriveSharePoint/updateFabricItemSensitivity")
                .is_some()
        );
        assert!(!prepared.effects().query_acceleration);
        assert_eq!(prepared.operation().body(), Some(&input));
        input["target"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap()["unknown"] = true.into();
        assert!(
            prepare(
                &target("Files/landing"),
                &serde_json::to_vec(&input).unwrap(),
                true,
                false
            )
            .is_err()
        );
    }
    for native_target in [
        json!({"externalDataShare":{}}),
        json!({"futureTarget":{}}),
        json!({"oneLake":{"workspaceId":WS,"itemId":SOURCE,"path":"/Files/source"}}),
        json!({"adlsGen2":{"connectionId":CONNECTION,"location":"https://untrusted.invalid","subpath":"container"}}),
    ] {
        let mut input = body("Files");
        input["target"] = native_target;
        assert!(
            prepare(
                &target("Files"),
                &serde_json::to_vec(&input).unwrap(),
                false,
                false
            )
            .is_err()
        );
    }
}

#[test]
fn approval_missing_declined_or_failed_makes_no_request() {
    for approval in [
        None,
        Some(Approval::default()),
        Some(Approval {
            fail: true,
            ..Approval::default()
        }),
    ] {
        let mut approval = approval;
        let mut operations = Operations::default();
        let error = host::execute(
            prepared("Files", false, false),
            &mut operations,
            approval
                .as_mut()
                .map(|value| value as &mut dyn LinkApproval),
        )
        .now_or_never()
        .unwrap()
        .unwrap_err();
        assert!(operations.calls.is_empty());
        assert_eq!(
            host::failure_metadata(&error)["mutationMayHaveOccurred"],
            false
        );
    }
}

#[test]
fn sensitivity_update_is_explicit_in_the_exact_approval_and_denial_is_effect_free() {
    let mut input = body("Files");
    input["target"] = json!({"oneDriveSharePoint":{
        "connectionId":CONNECTION,"location":"https://contoso.sharepoint.com",
        "subpath":"/Shared Documents/folder","updateFabricItemSensitivity":true
    }});
    let prepared = prepare(
        &target("Files"),
        &serde_json::to_vec(&input).unwrap(),
        false,
        false,
    )
    .unwrap();
    let mut approval = Approval::default();
    let mut operations = Operations::default();
    host::execute(prepared, &mut operations, Some(&mut approval))
        .now_or_never()
        .unwrap()
        .unwrap_err();
    assert!(operations.calls.is_empty());
    assert_eq!(approval.calls.len(), 1);
    assert!(approval.calls[0].1.remote_write);
    assert!(approval.calls[0].1.update_item_sensitivity);
    assert!(!approval.calls[0].1.query_acceleration);
    assert_eq!(
        approval.calls[0].0.scope(),
        &LinkScope::LakehouseShortcut {
            path: "Files".into(),
            name: "sales".into()
        }
    );
}

#[test]
fn direct_call_cannot_submit_another_owner_through_this_host() {
    use fabric_cli_mutations::v2_link::{self, LinkOwner};
    let target = ResolvedLinkTarget::new(
        WS,
        ITEM,
        LinkOwner::KqlDatabase,
        LinkScope::KqlTableShortcut {
            name: "sales".into(),
        },
    )
    .unwrap();
    let input =
        json!({"name":"sales","enableQueryAcceleration":false,"target":body("Files")["target"]});
    let prepared = v2_link::prepare(target, None, &input, false).unwrap();
    let mut operations = Operations::default();
    let mut approval = Approval {
        allow: true,
        ..Approval::default()
    };
    let failure = host::execute(prepared, &mut operations, Some(&mut approval))
        .now_or_never()
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.error.code, "UnsupportedCapability");
    assert!(operations.calls.is_empty());
    assert!(approval.calls.is_empty());
}

#[test]
fn actual_owner_denial_or_malformed_metadata_never_mutates() {
    let mut denied = ResourceError::input();
    denied.code = "Forbidden".into();
    denied.status = Some(403);
    for outcome in [
        Err(denied),
        Ok(ResourceOutcome::Completed {
            status: 200,
            data: Some(json!({"id":ITEM,"type":"Warehouse"})),
        }),
        Ok(ResourceOutcome::Completed {
            status: 200,
            data: Some(json!({"id":SOURCE,"type":"Lakehouse"})),
        }),
        Ok(ResourceOutcome::Completed {
            status: 200,
            data: None,
        }),
        Ok(ResourceOutcome::Accepted {
            operation_id: Some("read-operation".into()),
        }),
    ] {
        let mut operations = Operations {
            calls: vec![],
            replies: [outcome].into(),
        };
        let mut approval = Approval {
            allow: true,
            ..Approval::default()
        };
        let error = host::execute(
            prepared("Files", false, false),
            &mut operations,
            Some(&mut approval),
        )
        .now_or_never()
        .unwrap()
        .unwrap_err();
        assert_eq!(operations.calls.len(), 1);
        assert_eq!(operations.calls[0].method(), ResourceMethod::Get);
        assert!(!error.error.operation.unwrap().mutation_may_have_occurred);
    }
}

#[test]
fn accepted_completed_and_malformed_outcomes_are_real_and_never_replayed() {
    for outcome in [
        ResourceOutcome::Accepted {
            operation_id: Some("native-operation".into()),
        },
        completed("Files"),
        ResourceOutcome::Completed {
            status: 201,
            data: Some(json!({"path":"Files","name":"wrong"})),
        },
        ResourceOutcome::Completed {
            status: 200,
            data: None,
        },
    ] {
        let malformed = matches!(&outcome,ResourceOutcome::Completed { data,.. } if data.as_ref().and_then(|value|value.get("name")).and_then(Value::as_str) != Some("sales"));
        let accepted = matches!(outcome, ResourceOutcome::Accepted { .. });
        let mut operations = operations(Ok(outcome));
        let mut approval = Approval {
            allow: true,
            ..Approval::default()
        };
        let result = host::execute(
            prepared("Files", true, false),
            &mut operations,
            Some(&mut approval),
        )
        .now_or_never()
        .unwrap();
        if malformed {
            let error = result.unwrap_err();
            assert!(error.observed.is_some());
            assert_eq!(
                host::failure_metadata(&error)["mutationMayHaveOccurred"],
                true
            );
        } else {
            let receipt = result.unwrap();
            assert_eq!(
                receipt.metadata()["status"],
                if accepted { "accepted" } else { "succeeded" }
            );
            assert!(!receipt.metadata().to_string().contains("privateSource"));
        }
        assert_eq!(approval.calls.len(), 1);
        assert_eq!(operations.calls.len(), 2);
        assert_eq!(
            operations
                .calls
                .iter()
                .filter(|op| op.method() == ResourceMethod::Post)
                .count(),
            1
        );
    }
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("broken writer"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("broken flush"))
    }
}

#[test]
fn writer_failure_and_uncertain_provider_failure_preserve_evidence_without_replay() {
    let mut operations = operations(Ok(completed("Files")));
    let mut approval = Approval {
        allow: true,
        ..Approval::default()
    };
    let receipt = host::execute(
        prepared("Files", false, false),
        &mut operations,
        Some(&mut approval),
    )
    .now_or_never()
    .unwrap()
    .unwrap();
    let error = host::deliver(receipt, &mut BrokenWriter).unwrap_err();
    assert_eq!(error.receipt.metadata()["httpStatus"], 201);
    assert_eq!(operations.calls.len(), 2);
    let mut failure = ResourceError::input();
    failure.code = "TimedOut".into();
    failure.operation_id = Some("native-handle".into());
    failure.status = Some(202);
    failure.mutation_may_have_occurred = true;
    let mut operations = self::operations(Err(failure.clone()));
    let error = host::execute(
        prepared("Files", false, true),
        &mut operations,
        Some(&mut approval),
    )
    .now_or_never()
    .unwrap()
    .unwrap_err();
    assert_eq!(error.error.operation.as_deref(), Some(&failure));
    assert_eq!(
        host::failure_metadata(&error)["operationId"],
        "native-handle"
    );
    assert_eq!(
        host::failure_metadata(&error)["mutationMayHaveOccurred"],
        true
    );
    assert_eq!(operations.calls.len(), 2);
}

struct FlushFailure(Vec<u8>);
impl Write for FlushFailure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush failed"))
    }
}

#[test]
fn complete_output_then_failed_flush_still_retains_the_original_accepted_receipt() {
    let mut operations = operations(Ok(ResourceOutcome::Accepted {
        operation_id: Some("accepted-handle".into()),
    }));
    let mut approval = Approval {
        allow: true,
        ..Approval::default()
    };
    let receipt = host::execute(
        prepared("Files", false, false),
        &mut operations,
        Some(&mut approval),
    )
    .now_or_never()
    .unwrap()
    .unwrap();
    let mut writer = FlushFailure(Vec::new());
    let failure = host::deliver(receipt, &mut writer).unwrap_err();
    let output: Value = serde_json::from_slice(&writer.0).unwrap();
    assert_eq!(output["status"], "accepted");
    assert_eq!(
        output["relationship"],
        json!({"path":"Files","name":"sales"})
    );
    assert_eq!(host::receipt_metadata(&failure.receipt), output);
    assert_eq!(operations.calls.len(), 2);
}

#[test]
fn file_and_stdin_inputs_have_identical_native_requests() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("input.json"),
        serde_json::to_vec(&body("Files")).unwrap(),
    )
    .unwrap();
    let path = target("Files");
    let command = host::parse([
        "fab-cli-v2",
        "artifact",
        "lakehouse",
        "link",
        &path,
        "--input",
        "input.json",
    ])
    .unwrap();
    let file = host::prepare(
        &command,
        &mut LocalFiles {
            cwd: root.path(),
            stdin: &mut Unread,
        },
    )
    .now_or_never()
    .unwrap()
    .unwrap();
    assert_eq!(
        file.operation().body(),
        prepared("Files", true, false).operation().body()
    );
    std::fs::create_dir(root.path().join("directory")).unwrap();
    let mut invalid = command;
    invalid.input = "directory".into();
    assert!(
        host::prepare(
            &invalid,
            &mut LocalFiles {
                cwd: root.path(),
                stdin: &mut Unread
            }
        )
        .now_or_never()
        .unwrap()
        .is_err()
    );
}
