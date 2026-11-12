// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native file/resolver/approval adapters, without ambient credentials or effects.
#![deny(unnameable_test_items)]

use super::super::v2_test_support as support;
use super::*;
use fabric_sdk::{Audience, CredentialSelection, RequestContext};
use serde_json::{Value, json};
use std::sync::Arc;
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const WS: &str = "11111111-1111-4111-8111-111111111111";
const DB: &str = "22222222-2222-4222-8222-222222222222";
const TARGET: &str = "/11111111-1111-4111-8111-111111111111.Workspace/22222222-2222-4222-8222-222222222222.KQLDatabase/Tables/sales_ref.Shortcut";

fn input() -> Value {
    json!({"name":"sales_ref","enableQueryAcceleration":false,"target":{"oneLake":{"workspaceId":WS,"itemId":DB,"path":"Tables/sales"}}})
}

fn client() -> Client {
    Client::with_context(RequestContext::new(
        Arc::new(TestCredential::default()),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    ))
}

fn executor(http: Arc<ScriptedHttp>) -> HttpExecutor {
    HttpExecutor::new(
        http,
        Arc::new(TestTimer::default()),
        fabric_sdk_http::ExecutionOptions::default(),
    )
    .unwrap()
}

#[test]
fn target_specific_dispatch_precedes_update_without_annexing_other_facades() {
    for args in [
        vec!["link", TARGET, "--input", "-"],
        vec!["artifact", "kql-database", "link", TARGET, "--input", "-"],
        vec!["artifact", "kql-database", "--help"],
    ] {
        assert!(handles(
            &args.into_iter().map(OsString::from).collect::<Vec<_>>()
        ));
    }
    for args in [
        vec!["artifact", "notebook", "update"],
        vec!["artifact", "lakehouse", "link"],
        vec!["artifact", "data-agent", "link"],
        vec!["update"],
    ] {
        assert!(!handles(
            &args.into_iter().map(OsString::from).collect::<Vec<_>>()
        ));
    }
}

#[tokio::test]
async fn actual_native_file_and_resolver_adapters_prepare_the_captured_native_operation() {
    let file = tempfile::NamedTempFile::new().unwrap();
    serde_json::to_writer(&file, &input()).unwrap();
    let args = [
        "artifact",
        "kql-database",
        "link",
        TARGET,
        "--input",
        file.path().to_str().unwrap(),
    ];
    let command = host::parse(args.into_iter().map(OsString::from)).unwrap();
    let http = ScriptedHttp::new([]);
    let client = client();
    let executor = executor(http.clone());
    let mut resolver = Resolver {
        client: &client,
        executor: &executor,
    };
    let prepared = host::prepare(&command, &mut resolver, &mut Files)
        .await
        .unwrap();
    assert_eq!(prepared.operation().body(), Some(&input()));
    assert_eq!(
        prepared.operation().path(),
        format!("/v1/workspaces/{WS}/kqlDatabases/{DB}/shortcuts")
    );
    assert_eq!(prepared.target().workspace(), WS);
    assert_eq!(prepared.target().item(), DB);
    assert!(!prepared.effects().query_acceleration);
    assert!(
        http.requests.lock().unwrap().is_empty(),
        "GUID owners need no discovery"
    );
    let mut approval = CapturedApproval(prepared.target().clone(), prepared.effects());
    let mut operations = OperationSession::with_context(
        client.context(),
        executor.clone(),
        OperationLimits::default(),
        std::future::pending(),
    );
    // Missing approval sends no request even with fully configured native adapters.
    let delivery = host::Delivery::execute(prepared.clone(), &mut operations, None).await;
    assert!(delivery.result().is_err());
    assert!(http.requests.lock().unwrap().is_empty());
    let different = KqlDestination::parse(&TARGET.replace("sales_ref", "other"))
        .unwrap()
        .resolved(WS, DB)
        .unwrap();
    assert!(
        approval
            .approve(&different, prepared.effects())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_resolver_names_are_bounded_typed_unique_and_never_permission_fallback() {
    for (status, items, success) in [
        (
            200,
            json!([{"id":DB,"displayName":"db","type":"KQLDatabase"}]),
            true,
        ),
        (
            200,
            json!([{"id":DB,"displayName":"db","type":"Lakehouse"}]),
            false,
        ),
        (
            200,
            json!([{"id":DB,"displayName":"db","type":"KQLDatabase"},{"id":WS,"displayName":"db","type":"KQLDatabase"}]),
            false,
        ),
        (403, json!([]), false),
    ] {
        let http = ScriptedHttp::new([response(status, &json!({"value":items}).to_string(), &[])]);
        let client = client();
        let executor = executor(http.clone());
        let mut resolver = Resolver {
            client: &client,
            executor: &executor,
        };
        let path = format!("/{WS}.Workspace/db.KQLDatabase/Tables/sales_ref.Shortcut");
        assert_eq!(resolver.resolve(&path).await.is_ok(), success);
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert_eq!(
            http.requests.lock().unwrap()[0].method(),
            azure_core::http::Method::Get
        );
    }
}

#[tokio::test]
async fn native_file_bounds_and_missing_inputs_fail_explicitly() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"12345").unwrap();
    assert!(
        Files
            .read(file.path().to_str().unwrap(), NonZeroUsize::new(4).unwrap())
            .await
            .is_err()
    );
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.json");
    assert!(
        Files
            .read(missing.to_str().unwrap(), NonZeroUsize::new(4).unwrap())
            .await
            .is_err()
    );
}
