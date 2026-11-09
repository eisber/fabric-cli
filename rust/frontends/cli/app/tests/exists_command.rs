// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root/SDK agreement using the same offline metadata/evidence fixture.

#![deny(unnameable_test_items)]

#[path = "../../../../sdk/client/tests/exists_support/mod.rs"]
mod exists_support;
use exists_support::*;
use fabric_cli::exists_command::{ExistsArgumentError, parse_exists, write_exists};
use fabric_sdk::exists::ExistsLimits;
use serde_json::json;
use std::{
    ffi::OsString,
    io::{self, Write},
    sync::atomic::Ordering,
};

#[test]
fn metadata_paths_and_options_are_preflighted_without_a_host() {
    for path in [
        "/Sales.Workspace",
        "/Sales.Workspace/Note.Notebook",
        &format!("/workspaces/{WS}/items/{ITEM}"),
    ] {
        let command = parse_exists(["exists", path].into_iter().map(OsString::from)).unwrap();
        assert!(command.request(TENANT).is_ok());
        assert_eq!(command.max_bytes, ExistsLimits::default().max_output_bytes);
    }
    for args in [
        vec!["exists", "local.txt"],
        vec!["exists", "C:\\private"],
        vec!["exists", "/Sales.Workspace/Note.Notebook/Files/test"],
        vec!["exists", "/Sales.Workspace/Note.Notebook/src/definition"],
        vec!["exists", "/Sales.Workspace/Note.Notebook/docs/schema"],
        vec![
            "exists",
            "/Sales.Workspace/Note.Notebook/runs/00000000-0000-0000-0000-000000000000",
        ],
        vec![
            "exists",
            "/Sales.Workspace/Lake.Lakehouse/Tables/dbo/orders",
        ],
        vec!["exists", "/Sales.Workspace/Folder.Folder/Note.Notebook"],
        vec!["exists", "/Sales.Workspace/Folder.Folder"],
        vec!["exists", "/Sales.Workspace/*"],
        vec!["exists", "/Sales.Workspace", "--max-bytes", "0"],
        vec!["exists", "/Sales.Workspace", "--timeout", "0"],
        vec!["exists", "/Sales.Workspace", "--force"],
        vec![
            "exists",
            "/Sales.Workspace",
            "--max-bytes",
            "1",
            "--max-bytes",
            "2",
        ],
    ] {
        assert!(parse_exists(args.into_iter().map(OsString::from)).is_err());
    }
    assert_eq!(
        parse_exists(["exists", "--help"].into_iter().map(OsString::from)).unwrap_err(),
        ExistsArgumentError::Syntax(clap::error::ErrorKind::DisplayHelp)
    );
}

#[tokio::test]
async fn stdout_is_exact_sdk_json_and_null_is_a_nonzero_result() {
    for (status, data, exists, exit) in [
        (200, item_data(), Some(true), 0),
        (404, json!({"errorCode":"ItemNotFound"}), Some(false), 0),
        (404, json!(null), None, 1),
        (403, json!({"errorCode":"Forbidden"}), None, 1),
    ] {
        let fixture = Fixture::new(vec![reply(status, data)]);
        let report = fixture.run(item_target(), ExistsLimits::default()).await;
        let mut stdout = Vec::new();
        assert_eq!(
            report.exists(),
            exists,
            "HTTP {status}: {}",
            report.reason(),
        );
        assert_eq!(
            write_exists(&report, &mut stdout).unwrap(),
            exit,
            "HTTP {status}: {}",
            report.reason(),
        );
        assert_eq!(stdout, report.json_bytes());
        assert_eq!(report.exists(), exists);
        assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);
        assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 1);
    }
}

struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn writer_failure_is_explicit_and_does_not_replay_the_probe() {
    let fixture = Fixture::new(vec![reply(200, item_data())]);
    let report = fixture.run(item_target(), ExistsLimits::default()).await;
    assert_eq!(
        write_exists(&report, &mut Broken).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(fixture.executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cli_target_uses_the_same_sdk_scoped_name_operation() {
    let command = parse_exists(
        ["exists", &format!("/{WS}.Workspace/Note.Notebook")]
            .into_iter()
            .map(OsString::from),
    )
    .unwrap();
    let fixture = Fixture::new(vec![
        reply(200, json!({"value":[item_data()]})),
        reply(200, item_data()),
    ]);
    let report = fixture
        .run(command.target.clone(), command.limits().unwrap())
        .await;
    assert_eq!(report.exists(), Some(true));
    assert_eq!(fixture.executor.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn cli_workspace_and_name_miss_keep_the_same_sdk_evidence() {
    let command = parse_exists(
        ["exists", &format!("/workspaces/{WS}")]
            .into_iter()
            .map(OsString::from),
    )
    .unwrap();
    let fixture = Fixture::new(vec![reply(200, workspace_data())]);
    assert_eq!(
        fixture
            .run(command.target, ExistsLimits::default())
            .await
            .exists(),
        Some(true)
    );
    let mut wrong_type = item_data();
    wrong_type["id"] = OTHER.into();
    wrong_type["type"] = "Report".into();
    let fixture = Fixture::new(vec![reply(200, json!({"value":[wrong_type]}))]);
    assert_eq!(
        fixture
            .run(named_item(), ExistsLimits::default())
            .await
            .exists(),
        None
    );
}
