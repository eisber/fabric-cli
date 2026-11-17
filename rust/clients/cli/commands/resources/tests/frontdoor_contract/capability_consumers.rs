// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public consumers bypass the compatibility facade's parser and dispatcher.

use super::*;
use fabric_cli_access::{host::AccessHost as FocusedHost, parser::AccessKind as FocusedKind};

#[test]
fn focused_parsers_retain_aliases_empty_queries_and_shared_type_identity() {
    let matches = fabric_cli_access::parser::register(clap::Command::new("direct"))
        .try_get_matches_from([
            "direct",
            "acl",
            "grant",
            "Demo.Workspace",
            "-I",
            "person",
            "-R",
            "Viewer",
        ])
        .unwrap();
    let command = fabric_cli_access::parser::from_matches(&matches)
        .unwrap()
        .unwrap();
    assert!(
        matches!(command.kind, FocusedKind::AclChange { change: fabric_cli_access::AclChange::Set { ref role, .. }, .. } if role == "Viewer")
    );
    let compatible: AccessCommand = command.into();
    assert!(matches!(compatible.kind, AccessKind::AclChange { .. }));

    let matches = fabric_cli_access::parser::register(clap::Command::new("direct"))
        .try_get_matches_from(["direct", "acl", "get", "Demo.Workspace", "-q", ""])
        .unwrap();
    let command = fabric_cli_access::parser::from_matches(&matches)
        .unwrap()
        .unwrap();
    assert!(
        matches!(command.kind, FocusedKind::AclGet { query: Some(ref value), .. } if value.is_empty())
    );
    assert_eq!(
        AccessCommand::from(command).access().unwrap().output_format,
        None
    );

    let value: fabric_cli_resources::CommandValue = fabric_cli_api::CommandValue::default();
    let value: fabric_cli_catalog::CommandValue = value;
    let _: fabric_cli_access::CommandValue = value;
    let error: fabric_cli_resources::CommandError =
        fabric_cli_api::CommandError::new("InvalidInput", "same type");
    let error: fabric_cli_catalog::CommandError = error;
    let _: fabric_cli_access::CommandError = error;
}

#[tokio::test]
async fn focused_access_dispatch_keeps_v2_text_get_and_explicit_empty_query_distinct() {
    for (query, records) in [(None, true), (Some(""), false)] {
        let assignment =
            json!({"id":"assignment","principal":{"id":"principal","type":"User"},"role":"Viewer"});
        let (api, wire, _) = setup(vec![reply(
            200,
            &json!({"value":[assignment.clone()]}).to_string(),
            &[],
        )]);
        let targets = target(None);
        let mut args = vec!["direct", "acl", "get", "Demo.Workspace"];
        if let Some(query) = query {
            args.extend(["-q", query]);
        }
        let matches = fabric_cli_access::parser::register(clap::Command::new("direct"))
            .try_get_matches_from(args)
            .unwrap();
        let command = fabric_cli_access::parser::from_matches(&matches)
            .unwrap()
            .unwrap();
        let result = FocusedHost {
            client: &api,
            targets: &targets,
            label_definitions: None,
            confirmation: None,
            limits: Limits::default(),
        }
        .execute_for_output(&command, OutputFormat::Text, pending())
        .await
        .unwrap();
        if records {
            assert_eq!(result.data, Some(json!([assignment])));
        } else {
            assert!(
                result
                    .data
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .contains(&json!("[0].principal.id"))
            );
        }
        assert_eq!(targets.calls.load(Ordering::SeqCst), 1);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn focused_raw_api_dispatch_prepares_body_and_preserves_audience_before_file_error() {
    let matches = fabric_cli_api::parser::register(clap::Command::new("direct"))
        .try_get_matches_from([
            "direct",
            "api",
            "items",
            "-X",
            "post",
            "-i",
            "{\"name\":",
            "\"find\"}",
            "-q",
            "text",
        ])
        .unwrap();
    let command = fabric_cli_api::parser::from_matches(&matches)
        .unwrap()
        .unwrap();
    let (api, wire, credential) = setup(vec![reply(201, r#"{"id":"created"}"#, &[])]);
    let clients = fabric_cli_api::ApiClients {
        fabric: &api,
        onelake: None,
        arm: None,
        powerbi: None,
    };
    let result = clients
        .execute(
            &command.kind,
            "offline-boundary",
            Limits::default(),
            RequestBounds::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.data, Some(json!({"id":"created"})));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    {
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests[0].method().to_string(), "POST");
        assert_eq!(
            serde_json::from_slice::<Value>(&azure_core::Bytes::from(requests[0].body())).unwrap(),
            json!({"name":"find"})
        );
    }

    let matches = fabric_cli_api::parser::register(clap::Command::new("direct"))
        .try_get_matches_from([
            "direct",
            "api",
            "items",
            "-A",
            "storage",
            "-i",
            "missing-input.invalid",
        ])
        .unwrap();
    let command = fabric_cli_api::parser::from_matches(&matches)
        .unwrap()
        .unwrap();
    let error = clients
        .execute(
            &command.kind,
            "offline-boundary",
            Limits::default(),
            RequestBounds::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "NotConfigured");
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn focused_catalog_parser_dispatch_matches_the_existing_independent_source_case() {
    let source: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/compatibility/catalog/cases.json"
    ))
    .unwrap();
    let case = &source["cases"][0];
    let (api, wire, _) = setup(vec![reply(
        200,
        &case["responses"][0]["body"].to_string(),
        &[],
    )]);
    let matches = fabric_cli_catalog::parser::register(clap::Command::new("direct"))
        .try_get_matches_from(["direct", "find", "needle"])
        .unwrap();
    let command = fabric_cli_catalog::parser::from_matches(&matches)
        .unwrap()
        .unwrap();
    let result = fabric_cli_catalog::execute(&api, &command.kind, Limits::default(), pending())
        .await
        .unwrap();
    assert_eq!(result.data.as_ref().unwrap(), &case["expected"]["data"]);
    assert_eq!(json!(result.diagnostics), case["expected"]["diagnostics"]);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url().as_str(),
        case["expected"]["requests"][0]["url"].as_str().unwrap()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&azure_core::Bytes::from(requests[0].body())).unwrap(),
        case["expected"]["requests"][0]["body"]
    );
}
