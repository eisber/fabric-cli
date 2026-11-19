// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! AIHub DataAgent dispatch contracts for the query command capability.

#![deny(unnameable_test_items)]

#[path = "../../../../../engine/data-plane/aihub-query/examples/support/mod.rs"]
mod support;

use fabric_cli_query_commands::{
    Artifact, DataAgentRoute, InputReader, InputSource, PreparedAiHub, QueryInput, QueryTarget,
    Stage, TextSource, prepare_input,
};
use fabric_sdk_aihub_query::{Completeness, Limits, NativeValue, QueryErrorKind, QueryOutcome};
use serde_json::json;
use std::{
    future::{pending, ready},
    io,
    sync::{Arc, atomic::Ordering},
};
use support::*;

struct NoInput;

impl InputReader for NoInput {
    fn read(&mut self, _: InputSource<'_>, _: usize) -> io::Result<Vec<u8>> {
        panic!("inline input must not open another reader")
    }
}

fn prepared(wire: Arc<Wire>, credential: Arc<Credential>, stage: Stage) -> PreparedAiHub {
    let workspace = "55555555-5555-4555-8555-555555555555";
    let agent = Artifact {
        workspace: workspace.into(),
        item: AGENT.into(),
    };
    let artifact = Artifact {
        workspace: workspace.into(),
        item: "66666666-6666-4666-8666-666666666666".into(),
    };
    let route = QueryTarget {
        requested_artifact: agent.clone(),
        execution_artifact: artifact.clone(),
        data_agent: Some(DataAgentRoute {
            engine: Some("advertised-agent".into()),
            agent,
            datasource: SOURCE.into(),
            bound_artifact: artifact,
            stage,
        }),
        native_kql: Vec::new(),
    }
    .resolve(None)
    .unwrap();
    let input = prepare_input(
        QueryInput {
            text: TextSource::Inline("opaque synthetic query\n".into()),
            parameters: None,
        },
        &route,
        &mut NoInput,
    )
    .unwrap();
    PreparedAiHub::new(
        route,
        input,
        client(
            wire,
            credential,
            Arc::new(Timer::default()),
            Limits::default(),
        ),
        500,
    )
    .unwrap()
}

#[tokio::test]
async fn real_sdk_typed_tokens_target_scope_and_opaque_input_are_wired() {
    let wire = Wire::new(setup().into_iter().chain([rpc(2, success())]));
    let credential = Arc::new(Credential::default());
    let result = prepared(wire.clone(), credential.clone(), Stage::Production)
        .execute(&deadline(), pending())
        .await
        .unwrap();
    let QueryOutcome::Complete(result) = result else {
        panic!("completed native typed result expected");
    };
    assert_eq!(result.completeness, Completeness::Complete);
    assert_eq!(
        result.resultsets[0].rows[0][0],
        NativeValue::Integer("18446744073709551615".into())
    );
    assert_eq!(result.resultsets[0].columns[0].name, "value");
    assert_eq!(result.resultsets[0].columns[1].name, "value");
    assert_eq!(credential.scopes.lock().unwrap().as_slice(), [SCOPE]);
    let messages = messages(&wire);
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages
            .iter()
            .map(|message| message["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["initialize", "notifications/initialized", "tools/call"]
    );
    assert_eq!(messages[2]["id"], 2);
    assert_eq!(messages[2]["params"]["name"], "execute_query");
    for message in &messages {
        assert_eq!(
            message["params"]["_meta"]["variants"],
            "Fabric.Routing.Documents.DataAgent,Fabric.Routing.Documents.DataAgent.TypedQueryResults,Fabric.Routing.Documents.SqlReadOnlyEnforce"
        );
    }
    let arguments = &messages[2]["params"]["arguments"];
    assert_eq!(arguments["artifactId"], format!("{AGENT}/{SOURCE}"));
    assert_eq!(arguments["query"], "opaque synthetic query\n");
    assert_eq!(arguments["max_rows"], 500);
    assert!(arguments.get("language").is_none());
    assert!(arguments.get("filename").is_none());
}

#[tokio::test]
async fn sandbox_stage_is_explicit_and_independent_of_typed_result_opt_in() {
    let wire = Wire::new(setup().into_iter().chain([rpc(2, success())]));
    prepared(
        wire.clone(),
        Arc::new(Credential::default()),
        Stage::Sandbox,
    )
    .execute(&deadline(), pending())
    .await
    .unwrap();
    for message in messages(&wire) {
        assert_eq!(
            message["params"]["_meta"]["variants"],
            "Fabric.Routing.Documents.DataAgent,Fabric.Routing.Documents.DataAgent.TypedQueryResults,Fabric.Routing.Documents.SqlReadOnlyEnforce,Fabric.Routing.Documents.DataAgent.Sandbox"
        );
    }
}

#[tokio::test]
async fn missing_typed_resource_has_no_csv_fallback_or_second_query() {
    let wire = Wire::new(setup().into_iter().chain([rpc(
        2,
        json!({"content":[{"type":"resource","resource":{
            "uri":"file:///legacy.csv","mimeType":"text/csv","text":"n\r\n1\r\n"}}]}),
    )]));
    let error = prepared(
        wire.clone(),
        Arc::new(Credential::default()),
        Stage::Production,
    )
    .execute(&deadline(), pending())
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::MissingTypedJson);
    assert!(error.query_dispatched);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn actual_authentication_failure_never_dispatches_query_or_switches_provider() {
    let wire = Wire::new([]);
    let credential = Arc::new(Credential {
        fail: true,
        ..Credential::default()
    });
    let error = prepared(wire.clone(), credential.clone(), Stage::Production)
        .execute(&deadline(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Authentication);
    assert!(!error.query_dispatched);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn local_cancellation_before_credentials_is_not_remote_cancellation() {
    let wire = Wire::new([]);
    let credential = Arc::new(Credential::default());
    let error = prepared(wire.clone(), credential.clone(), Stage::Production)
        .execute(&deadline(), ready(()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Canceled);
    assert!(!error.query_dispatched);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn fixed_tool_remote_missing_tool_or_argument_errors_keep_dispatch_evidence_without_replay() {
    for code in [-32601, -32602] {
        let error = serde_json::to_vec(&json!({
            "jsonrpc":"2.0","id":2,
            "error":{"code":code,"message":"synthetic-private-provider-text"}
        }))
        .unwrap();
        let wire = Wire::new(setup().into_iter().chain([response(
            200,
            error,
            "application/json",
            None,
        )]));
        let error = prepared(
            wire.clone(),
            Arc::new(Credential::default()),
            Stage::Production,
        )
        .execute(&deadline(), pending())
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::Rpc(code));
        assert!(error.query_dispatched);
        assert!(
            !error
                .to_string()
                .contains("synthetic-private-provider-text")
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
        assert!(
            messages(&wire)
                .iter()
                .all(|message| message["method"] != "tools/list")
        );
        assert_eq!(messages(&wire)[2]["params"]["name"], "execute_query");
    }
}
