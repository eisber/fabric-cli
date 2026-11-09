// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root parser/delivery agreement with the exact SDK offline catalog fixture.

#![deny(unnameable_test_items)]
#[path = "../../../../engine/sdk/tests/exists_catalog_support/mod.rs"]
mod exists_catalog_support;
#[path = "../../../../engine/sdk/tests/exists_support/mod.rs"]
mod exists_support;
use exists_catalog_support::*;
use exists_support::{ITEM, OTHER, TENANT, WS};
use fabric_cli::exists_command::{parse_exists, write_exists};
use fabric_sdk::exists::{ExistsLimits, ExistsTarget, MetadataSelector};
use serde_json::{Value, json};
use std::{ffi::OsString, sync::atomic::Ordering};

#[tokio::test]
async fn catalog_registration_preserves_existing_metadata_targets_and_name_ambiguity() {
    for (selected, replies) in [
        (
            ExistsTarget::Workspace {
                workspace: MetadataSelector::id(WS).unwrap(),
            },
            vec![exists_support::reply(200, exists_support::workspace_data())],
        ),
        (
            exists_support::item_target(),
            vec![exists_support::reply(200, exists_support::item_data())],
        ),
        (
            exists_support::named_item(),
            vec![
                exists_support::reply(200, json!({"value":[exists_support::item_data()]})),
                exists_support::reply(200, exists_support::item_data()),
            ],
        ),
    ] {
        let fixture = exists_support::Fixture::new(replies);
        assert_eq!(
            fixture
                .run(selected, ExistsLimits::default())
                .await
                .exists(),
            Some(true)
        );
    }
    let fixture = exists_support::Fixture::new(vec![exists_support::reply(
        200,
        json!({"value":[
            {"id":ITEM,"displayName":"Note","type":"Notebook"},
            {"id":OTHER,"displayName":"Note","type":"Notebook"}
        ]}),
    )]);
    let report = fixture
        .run(exists_support::named_item(), ExistsLimits::default())
        .await;
    assert_eq!(report.exists(), None);
    assert_eq!(report.reason(), "AmbiguousTarget");
}

#[test]
fn id_bound_catalog_selectors_are_distinct_from_physical_tables_paths() {
    let namespace = format!("/workspaces/{WS}/items/{ITEM}/catalog/namespaces/dbo");
    let table = format!("{namespace}/tables/Orders");
    for (path, is_table) in [(&namespace, false), (&table, true)] {
        let command = parse_exists(
            ["exists", path, "--timeout", "7"]
                .into_iter()
                .map(OsString::from),
        )
        .unwrap();
        assert_eq!(command.target, target(is_table));
        assert_eq!(command.timeout.as_secs(), 7);
        assert!(command.request(TENANT).unwrap().target().is_catalog());
    }
    for path in [
        format!("/workspaces/{WS}/items/{ITEM}/Tables/dbo/Orders"),
        format!("/workspaces/{WS}/items/{ITEM}/catalog/namespaces/dbo/tables/Orders/data"),
        format!("/workspaces/{WS}/items/{ITEM}/catalog/namespaces/a/b"),
        format!("/workspaces/{WS}/items/{ITEM}/catalog/namespaces/"),
        format!("/workspaces/{WS}/items/{ITEM}/catalog/namespaces/../tables/Orders"),
        format!("/workspaces/{WS}/items/{ITEM}/catalog/namespaces/dbo/tables/"),
        format!("/workspaces/{WS}/items/Lake.Lakehouse/catalog/namespaces/dbo"),
        "/Sales.Workspace/Lake.Lakehouse/Tables/dbo/Orders".into(),
    ] {
        assert!(
            parse_exists(["exists", &path].into_iter().map(OsString::from)).is_err(),
            "{path}"
        );
    }
    assert!(matches!(
        target(false),
        ExistsTarget::CatalogNamespace { .. }
    ));
    assert!(matches!(target(true), ExistsTarget::CatalogTable { .. }));
}

#[tokio::test]
async fn catalog_sdk_and_root_share_boolean_null_evidence_bytes_and_exit_contract() {
    for table in [false, true] {
        for (status, exists, exit) in [
            (204, Some(true), 0),
            (404, Some(false), 0),
            (403, None, 1),
            (405, None, 1),
            (200, None, 1),
        ] {
            let path = format!(
                "/workspaces/{WS}/items/{ITEM}/catalog/namespaces/dbo{}",
                if table { "/tables/Orders" } else { "" }
            );
            let command = parse_exists(["exists", &path].into_iter().map(OsString::from)).unwrap();
            let fixture = CatalogFixture::new(vec![
                configuration(config(PREFIX)),
                response(status, vec![]),
            ]);
            let limits = command.limits().unwrap();
            let report = fixture.run(command.target, limits).await;
            assert_eq!(report.exists(), exists);
            let mut stdout = Vec::new();
            assert_eq!(write_exists(&report, &mut stdout).unwrap(), exit);
            assert_eq!(stdout, report.json_bytes());
            let data: Value = serde_json::from_slice(&stdout).unwrap();
            assert_eq!(data["exists"], json!(exists));
            assert_eq!(data["evidence"]["scope"], "selectedCatalogTarget");
            assert_eq!(data["evidence"]["responses"][2]["httpStatus"], status);
            assert_eq!(fixture.executor.requests.lock().unwrap().len(), 2);
            assert_eq!(fixture.metadata.credential.0.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.storage.0.load(Ordering::SeqCst), 2);
        }
    }
}

#[tokio::test]
async fn catalog_byte_cap_is_applied_to_the_shared_native_and_output_budget() {
    let path = format!("/workspaces/{WS}/items/{ITEM}/catalog/namespaces/dbo");
    let command = parse_exists(
        ["exists", &path, "--max-bytes", "4096"]
            .into_iter()
            .map(OsString::from),
    )
    .unwrap();
    let limits = command.limits().unwrap();
    assert_eq!(limits.retention.max_retained_bytes(), 4096);
    assert_eq!(limits.retention.max_response_bytes(), 4096);
    let fixture = CatalogFixture::new(vec![configuration(config(PREFIX)), response(204, vec![])]);
    let report = fixture.run(command.target, limits).await;
    assert_eq!(report.exists(), Some(true));
    assert!(report.json_bytes().len() <= 4096);
    assert!(
        fixture
            .executor
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, _, limit)| *limit <= 4096)
    );
    assert_eq!(
        ExistsLimits::default().max_output_bytes,
        parse_exists(["exists", &path].into_iter().map(OsString::from))
            .unwrap()
            .max_bytes
    );
}
