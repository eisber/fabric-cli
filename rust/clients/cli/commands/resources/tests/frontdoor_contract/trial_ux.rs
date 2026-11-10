// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn assignment() -> Value {
    json!({
        "id":"assignment-a",
        "principal":{
            "id":"principal-a", "displayName":"Synthetic Reader", "type":"User",
            "userDetails":{"userPrincipalName":"reader@example.invalid"}
        },
        "role":"Viewer"
    })
}

fn streams(
    command: &AccessCommand,
    value: fabric_cli_resources::CommandValue,
    format: OutputFormat,
) -> String {
    let output = render(
        command,
        Ok(value),
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        &FamilyPresentation::new(format, command.surface()),
        &ascii_width,
    )
    .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(output.write_to(&mut stdout, &mut stderr).unwrap(), 0);
    assert!(stderr.is_empty());
    String::from_utf8(stdout).unwrap()
}

#[test]
fn acl_help_discovers_aliases_targets_context_and_workspace_discovery() {
    let mut acl = fabric_cli_resources::frontdoor::acl_subcommand();
    let help = acl.render_long_help().to_string();
    for spelling in ["grant", "revoke", "del", "set", "rm"] {
        assert!(help.contains(spelling), "{help}");
    }
    for leaf in ["ls", "get"] {
        let help = acl
            .find_subcommand_mut(leaf)
            .unwrap()
            .render_long_help()
            .to_string();
        for expected in [
            "Demo.Workspace",
            "fab-rs ls /",
            "Notebook",
            "current workspace",
            "workfolders",
            "not supported",
            "administrator",
        ] {
            assert!(help.contains(expected), "{leaf}: {help}");
        }
        assert!(help.len() < 2000, "{leaf}: help must remain concise");
    }
}

#[tokio::test]
async fn unqueried_text_acl_get_shows_full_values_without_changing_source_or_json() {
    for format in [OutputFormat::Text, OutputFormat::Json] {
        let (api, wire, credential) = setup(vec![reply(
            200,
            &json!({"value":[assignment()]}).to_string(),
            &[],
        )]);
        let targets = target(None);
        let command = parse(&["acl", "get", "Workspace.Workspace"]);
        let value = host(&api, &targets)
            .execute_for_output(&command, format, pending())
            .await
            .unwrap();
        let output = streams(&command, value, format);
        if format == OutputFormat::Text {
            assert_eq!(
                serde_json::from_str::<Value>(&output).unwrap(),
                assignment()
            );
            assert!(output.contains("Synthetic Reader"));
            assert!(output.contains("reader@example.invalid"));
            assert!(output.contains("Viewer"));
            assert!(!output.contains("[0].principal"));
        } else {
            let envelope: Value = serde_json::from_str(&output).unwrap();
            assert!(
                envelope["result"]["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|field| field == "[0].principal.displayName")
            );
        }
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    }
    let (api, _, _) = setup(vec![reply(
        200,
        &json!({"value":[assignment()]}).to_string(),
        &[],
    )]);
    let targets = target(None);
    let command = parse(&["acl", "get", "Workspace.Workspace"]);
    let original = host(&api, &targets)
        .execute(&command, pending())
        .await
        .unwrap();
    assert!(
        original
            .data
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "[0].role")
    );
}

#[tokio::test]
async fn explicit_acl_queries_preserve_discovery_projection_objects_arrays_and_errors() {
    for query in [
        "",
        ".",
        "[].principal",
        "[0].principal",
        "[].role",
        "length(@)",
        "bad[",
    ] {
        for format in [OutputFormat::Text, OutputFormat::Json] {
            let command = parse(&["acl", "get", "Workspace.Workspace", "--query", query]);
            let targets = target(None);
            let mut results = Vec::new();
            for presented in [false, true] {
                let (api, wire, _) = setup(vec![reply(
                    200,
                    &json!({"value":[assignment()]}).to_string(),
                    &[],
                )]);
                let result = if presented {
                    host(&api, &targets)
                        .execute_for_output(&command, format, pending())
                        .await
                } else {
                    host(&api, &targets).execute(&command, pending()).await
                };
                assert_eq!(wire.requests.lock().unwrap().len(), 1);
                results.push(result);
            }
            assert_eq!(results[0], results[1], "{format:?} query={query:?}");
        }
    }
}

#[tokio::test]
async fn acl_record_ux_retains_paging_empty_results_bounds_and_provider_failures() {
    let first = assignment();
    let mut second = assignment();
    second["id"] = json!("assignment-b");
    second["role"] = json!("Admin");
    let (api, wire, _) = setup(vec![
        reply(
            200,
            &json!({"value":[first.clone()],"continuationToken":"next"}).to_string(),
            &[],
        ),
        reply(200, &json!({"value":[second.clone()]}).to_string(), &[]),
    ]);
    let targets = target(None);
    let command = parse(&["acl", "get", "Workspace.Workspace"]);
    let value = host(&api, &targets)
        .execute_for_output(&command, OutputFormat::Text, pending())
        .await
        .unwrap();
    assert_eq!(value.data, Some(json!([first, second])));
    let output = streams(&command, value, OutputFormat::Text);
    assert!(output.contains("Viewer") && output.contains("Admin"));
    assert_eq!(wire.requests.lock().unwrap().len(), 2);

    for format in [OutputFormat::Text, OutputFormat::Json] {
        let (api, _, _) = setup(vec![
            reply(200, "{\"value\":[]}", &[]),
            reply(200, "{\"value\":[]}", &[]),
        ]);
        let value = host(&api, &targets)
            .execute_for_output(&command, format, pending())
            .await
            .unwrap();
        if format == OutputFormat::Text {
            assert!(value.data.is_none());
            assert!(streams(&command, value, format).is_empty());
        } else {
            let original = host(&api, &targets)
                .execute(&command, pending())
                .await
                .unwrap();
            assert_eq!(value, original);
            assert_eq!(value.data, Some(json!([""])));
        }
    }

    let (api, wire, credential) = setup(vec![reply(403, "{\"errorCode\":\"Forbidden\"}", &[])]);
    let error = host(&api, &targets)
        .execute_for_output(&command, OutputFormat::Text, pending())
        .await
        .unwrap_err();
    assert_eq!(error.code, "Forbidden");
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);

    let (api, wire, _) = setup(vec![reply(
        200,
        &json!({"value":[assignment()],"continuationToken":"next"}).to_string(),
        &[],
    )]);
    let mut limited = host(&api, &targets);
    limited.limits.max_pages = NonZeroUsize::new(1).unwrap();
    assert!(
        limited
            .execute_for_output(&command, OutputFormat::Text, pending())
            .await
            .is_err()
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn aliases_keep_item_mutations_unsupported_before_credentials_or_requests() {
    for leaf in ["grant", "revoke"] {
        let (api, wire, credential) = setup(vec![]);
        let targets = target(Some("Notebook"));
        let mut args = vec![
            "acl",
            leaf,
            "Workspace.Workspace/Item.Notebook",
            "-I",
            "principal",
            "-f",
        ];
        if leaf == "grant" {
            args.extend(["-R", "Viewer"]);
        }
        let error = host(&api, &targets)
            .execute(&parse(&args), pending())
            .await
            .unwrap_err();
        assert_eq!(error.code, "UnsupportedCommand");
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
        assert!(wire.requests.lock().unwrap().is_empty());
    }
}
