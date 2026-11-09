// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline parser, SDK/HTTP, projection, output and executable v2 read contracts.

#![cfg(feature = "workspace-read-v2")]
#![deny(unnameable_test_items)]

mod folder_support;

use fabric_cli::workspace_v2::{
    OutputReservation, ParsedWorkspaceInvocation, WorkspaceCommand, execute,
    execute_with_warehouse, failure_envelope, parse_args, write_result,
};
use fabric_sdk::{Audience, CredentialSelection};
use fabric_sdk_api::{ApiClient, workspace_reads::WorkspaceReadError};
use folder_support::{Credential, Http, Reply, Timer, complete, executor, reply};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    future::{pending, poll_fn, ready},
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::Poll,
};

const W: &str = "11111111-2222-3333-4444-555555555555";
const PATH: &str = "/11111111-2222-3333-4444-555555555555.Workspace";
const NOW: &str = "2026-11-05T20:00:00Z";

fn parsed(args: &[&str]) -> WorkspaceCommand {
    let args = std::iter::once(OsString::from("fab-cli-workspace"))
        .chain(args.iter().map(|arg| OsString::from(*arg)));
    let ParsedWorkspaceInvocation::Read(command) = parse_args(args).unwrap() else {
        panic!("a read is required");
    };
    command
}

fn client(replies: impl IntoIterator<Item = Reply>) -> (ApiClient, Arc<Http>, Arc<Credential>) {
    let http = Arc::new(Http::new(replies));
    let credential = Arc::new(Credential::default());
    let sdk = folder_support::sdk(credential.clone());
    let executor = Arc::new(executor(http.clone(), Arc::new(Timer::default())));
    let client = ApiClient::new(
        Arc::clone(sdk.credential()),
        sdk.selection().clone(),
        Arc::new(fabric_cli_resources::ResourceHttpExecutor(executor)),
    );
    (client, http, credential)
}

fn item(id: &str) -> Value {
    json!({"id":id,"displayName":id,"type":"Notebook","workspaceId":W})
}

fn value(envelope: &impl serde::Serialize) -> Value {
    serde_json::to_value(envelope).unwrap()
}

const WH: &str = "22222222-2222-3333-4444-555555555555";

fn warehouse(id: &str) -> Value {
    json!({"id":id,"workspaceId":W,"type":"Warehouse","displayName":"metadata only","future":false})
}

fn warehouse_client(
    replies: impl IntoIterator<Item = Reply>,
) -> (
    ApiClient,
    fabric_sdk::Client,
    fabric_sdk_http::HttpExecutor,
    Arc<Http>,
    Arc<Credential>,
) {
    let http = Arc::new(Http::new(replies));
    let credential = Arc::new(Credential::default());
    let sdk = folder_support::sdk(credential.clone());
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let api = ApiClient::new(
        Arc::clone(sdk.credential()),
        sdk.selection().clone(),
        Arc::new(fabric_cli_resources::ResourceHttpExecutor(Arc::new(
            executor.clone(),
        ))),
    );
    (api, sdk, executor, http, credential)
}

#[test]
fn warehouse_root_and_facade_get_and_list_share_actual_sdk_http_and_envelopes() {
    for (verb, suffix, body, expected_route, recursive) in [
        ("get", "", warehouse(WH), format!("/warehouses/{WH}"), false),
        (
            "get",
            "/settings/connection",
            json!({"connectionString":"inert.example","future":true}),
            format!("/warehouses/{WH}/connectionString"),
            false,
        ),
        (
            "get",
            "/restore-points/opaque %2F?#+key",
            json!({"id":"opaque %2F?#+key","creationMode":"Future"}),
            format!("/warehouses/{WH}/restorePoints/opaque%20%252F%3F%23+key"),
            false,
        ),
        (
            "list",
            "/restore-points",
            json!({"value":[{"id":"z"},{"id":"a"},{"id":"z"}]}),
            format!("/warehouses/{WH}/restorePoints"),
            false,
        ),
        (
            "list",
            "/warehouses",
            json!({"value":[warehouse(WH),warehouse(WH)]}),
            "/warehouses?recursive=false".into(),
            true,
        ),
    ] {
        let target = if suffix == "/warehouses" {
            format!("{PATH}{suffix}")
        } else {
            format!("{PATH}/{WH}.Warehouse{suffix}")
        };
        let mut results = Vec::new();
        for facade in [false, true] {
            let facade_path = if facade && suffix == "/warehouses" {
                PATH
            } else {
                &target
            };
            let mut args = if facade {
                vec!["artifact", "warehouse", verb, facade_path]
            } else {
                vec![verb, facade_path]
            };
            args.extend(["--max-bytes", "4096", "--timeout", "30s"]);
            if verb == "list" {
                args.extend(["--limit", "10"]);
            }
            if recursive {
                args.extend(["--recursive", "false"]);
            }
            let command = parsed(&args);
            let (api, sdk, executor, http, credential) = warehouse_client([reply(body.clone())]);
            let host = fabric_cli::warehouse_reads::WarehouseReadHost {
                client: &sdk,
                operations: &executor,
            };
            let result = complete(execute_with_warehouse(
                &api,
                Some(&host),
                &command,
                NOW,
                pending(),
                pending(),
            ));
            let envelope = value(&result);
            assert_eq!(result.exit_code(), 0, "{envelope}");
            assert_eq!(
                envelope["data"],
                if verb == "list" {
                    body["value"].clone()
                } else {
                    body.clone()
                }
            );
            assert_eq!(envelope["meta"]["target"]["type"], "Warehouse");
            assert_eq!(envelope["meta"]["coverage"]["complete"], true);
            assert_eq!(
                http.requests.lock().unwrap().as_slice(),
                &[format!(
                    "https://api.fabric.microsoft.com/v1/workspaces/{W}{expected_route}"
                )]
            );
            assert_eq!(credential.scopes.lock().unwrap().len(), 1);
            results.push(envelope);
        }
        assert_eq!(results[0], results[1]);
    }
}

#[test]
fn warehouse_list_bounded_page_is_honest_partial_not_failure_or_full_collection_success() {
    let target = format!("{PATH}/warehouses");
    let command = parsed(&[
        "list",
        &target,
        "--max-bytes",
        "4096",
        "--limit",
        "10",
        "--recursive",
        "false",
        "--timeout",
        "30s",
        "-q",
        "length(@)",
    ]);
    let (api, sdk, executor, http, _) = warehouse_client([reply(json!({
        "value":[warehouse(WH)],"continuationToken":"native-next"
    }))]);
    let host = fabric_cli::warehouse_reads::WarehouseReadHost {
        client: &sdk,
        operations: &executor,
    };
    let result = complete(execute_with_warehouse(
        &api,
        Some(&host),
        &command,
        NOW,
        pending(),
        pending(),
    ));
    let envelope = value(&result);
    assert_eq!(result.exit_code(), 1);
    assert_eq!(envelope["status"], "partial");
    assert!(envelope["error"].is_null());
    assert_eq!(envelope["data"], 1);
    assert_eq!(envelope["meta"]["coverage"]["complete"], false);
    assert_eq!(envelope["meta"]["coverage"]["resumable"], false);
    assert_eq!(
        envelope["meta"]["nativeRead"]["coverage"]["resumeKind"],
        "nativeInformational"
    );
    assert_eq!(
        envelope["meta"]["nativeRead"]["nativePage"]["continuationToken"],
        "native-next"
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn warehouse_actual_head_preflight_rejects_wrong_facade_facets_and_missing_limits() {
    for (verb, path, extra) in [
        (
            "get",
            format!("{PATH}/{WH}.Warehouse/src/definition"),
            vec![],
        ),
        ("get", format!("{PATH}/{WH}.Warehouse/docs"), vec![]),
        (
            "get",
            format!("{PATH}/{WH}.Warehouse/restore-points/../escape"),
            vec![],
        ),
        (
            "get",
            format!("{PATH}/{WH}.SQLDatabase/settings/connection"),
            vec![],
        ),
        ("list", format!("{PATH}/warehouses"), vec!["--limit", "10"]),
        (
            "list",
            format!("{PATH}/{WH}.Warehouse/restore-points"),
            vec!["--limit", "10", "--recursive", "false"],
        ),
    ] {
        let mut args = vec![
            verb,
            path.as_str(),
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
        ];
        args.extend(extra);
        assert!(
            parse_args(
                std::iter::once(OsString::from("fab-cli-workspace"))
                    .chain(args.iter().map(OsString::from))
            )
            .is_err()
        );
    }
    let path = format!("{PATH}/{WH}.Warehouse");
    assert!(
        parse_args(
            [
                "fab-cli-workspace",
                "artifact",
                "workspace",
                "get",
                &path,
                "--max-bytes",
                "4096",
                "--timeout",
                "30s"
            ]
            .map(OsString::from)
        )
        .is_err()
    );
    let collection_path = format!("{PATH}/warehouses");
    for args in [
        vec![
            "get",
            path.as_str(),
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
        ],
        vec![
            "list",
            collection_path.as_str(),
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
            "--limit",
            "10",
            "--recursive",
            "false",
        ],
    ] {
        let output = process(&args)
            .env("FAB_RUST_AUTH_MODE", "invalid-explicit-provider")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if cfg!(feature = "native-host") { 2 } else { 1 })
        );
        assert!(output.stdout.is_empty());
        let failed: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            failed["error"]["code"],
            if !cfg!(feature = "native-host") {
                "NotConfigured"
            } else if cfg!(any(windows, feature = "headless")) {
                "InvalidInput"
            } else {
                "UnsupportedCapability"
            }
        );
        assert_ne!(failed["status"], "succeeded");
        assert_eq!(failed["meta"]["target"]["type"], "Warehouse");
    }
}

#[test]
fn warehouse_injected_host_absence_and_original_status_failures_are_explicit() {
    let path = format!("{PATH}/{WH}.Warehouse");
    let command = parsed(&["get", &path, "--max-bytes", "4096", "--timeout", "30s"]);
    let (api, _, _) = client([]);
    assert_eq!(
        value(&complete(execute(
            &api,
            &command,
            NOW,
            pending(),
            pending()
        )))["error"]["code"],
        "NotConfigured"
    );
    for (status, code) in [
        (401, "Unauthenticated"),
        (403, "AccessDenied"),
        (404, "NotFound"),
        (202, "OperationFailed"),
        (206, "InvalidResponse"),
    ] {
        let (api, sdk, executor, http, _) = warehouse_client([Reply {
            status,
            body: json!({"error":{"code":"NativeFailure","message":"private"}}),
            headers: [("x-ms-operation-id".into(), WH.into())]
                .into_iter()
                .collect(),
        }]);
        let host = fabric_cli::warehouse_reads::WarehouseReadHost {
            client: &sdk,
            operations: &executor,
        };
        let result = complete(execute_with_warehouse(
            &api,
            Some(&host),
            &command,
            NOW,
            pending(),
            pending(),
        ));
        let failed = value(&result);
        assert_eq!(failed["error"]["code"], code, "{failed}");
        assert!(failed["data"].is_null());
        assert_eq!(failed["meta"]["target"]["id"], WH);
        if status == 202 {
            assert_eq!(failed["meta"]["nativeRead"]["operationId"], WH);
        }
        assert_eq!(http.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn warehouse_existing_output_race_and_stdout_failure_reuse_common_delivery() {
    let path = format!("{PATH}/{WH}.Warehouse/settings/connection");
    let directory = TestDirectory::new();
    let file = directory.0.join("connection.json");
    let command = parsed(&["get", &path, "--max-bytes", "4096", "--timeout", "30s"]);
    let (api, sdk, executor, http, _) =
        warehouse_client([reply(json!({"connectionString":"inert.example"}))]);
    let host = fabric_cli::warehouse_reads::WarehouseReadHost {
        client: &sdk,
        operations: &executor,
    };
    let result = complete(execute_with_warehouse(
        &api,
        Some(&host),
        &command,
        NOW,
        pending(),
        pending(),
    ));
    let reservation = OutputReservation::new(Some(&file)).unwrap();
    fs::write(&file, b"racing-owner").unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        write_result(reservation, &result, &mut out, &mut err).unwrap(),
        1
    );
    assert!(out.is_empty());
    assert_eq!(fs::read(&file).unwrap(), b"racing-owner");
    let failed: Value = serde_json::from_slice(&err).unwrap();
    assert_eq!(failed["error"]["code"], "OutputExists");
    assert_eq!(failed["meta"]["target"]["id"], WH);
    let mut err = Vec::new();
    assert_eq!(
        write_result(
            OutputReservation::new(None).unwrap(),
            &result,
            &mut BrokenWriter,
            &mut err
        )
        .unwrap(),
        1
    );
    let failed: Value = serde_json::from_slice(&err).unwrap();
    assert_eq!(failed["meta"]["nativeRead"]["requestOwner"]["itemId"], WH);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn primary_common_and_facade_contracts_are_equal_and_never_enrich() {
    let metadata = json!({"id":W,"displayName":"Viewer workspace","unknown":{"null":null,"n":1.0}});
    let mut envelopes = Vec::new();
    for args in [
        vec!["get", PATH, "--max-bytes", "4096", "--timeout", "30s"],
        vec![
            "artifact",
            "workspace",
            "get",
            PATH,
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
        ],
    ] {
        let command = parsed(&args);
        let (client, http, credential) = client([reply(metadata.clone())]);
        let result = complete(execute(&client, &command, NOW, pending(), pending()));
        assert_eq!(result.exit_code(), 0);
        let envelope = value(&result);
        assert_eq!(envelope["data"], metadata);
        assert_eq!(envelope["meta"]["coverage"]["complete"], true);
        assert_eq!(envelope["meta"]["context"]["tenant"], "offline-tenant");
        assert_eq!(
            *http.requests.lock().unwrap(),
            [format!(
                "https://api.fabric.microsoft.com/v1/workspaces/{W}"
            )],
        );
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        http.assert_consumed();
        envelopes.push(envelope);
    }
    assert_eq!(envelopes[0], envelopes[1]);
}

#[test]
fn list_aliases_preserve_empty_singleton_and_many_arrays() {
    for rows in [vec![], vec![item("a")], vec![item("a"), item("b")]] {
        let mut envelopes = Vec::new();
        for args in [
            vec![
                "list",
                PATH,
                "--max-bytes",
                "4096",
                "--limit",
                "3",
                "--recursive",
                "false",
                "--timeout",
                "1m",
            ],
            vec![
                "artifact",
                "workspace",
                "list",
                PATH,
                "--max-bytes",
                "4096",
                "--limit",
                "3",
                "--recursive",
                "false",
                "--timeout",
                "1m",
            ],
        ] {
            let command = parsed(&args);
            let (client, http, _) = client([reply(json!({"value": rows}))]);
            let result = complete(execute(&client, &command, NOW, pending(), pending()));
            let envelope = value(&result);
            assert_eq!(result.exit_code(), 0);
            assert_eq!(envelope["data"], json!(rows));
            assert_eq!(
                *http.requests.lock().unwrap(),
                [format!(
                    "https://api.fabric.microsoft.com/v1/workspaces/{W}/items?recursive=false",
                )],
            );
            envelopes.push(envelope);
        }
        assert_eq!(envelopes[0], envelopes[1]);
    }
}

#[test]
fn record_truncation_is_explicit_nonresumable_partial_not_a_skipping_cursor() {
    let command = parsed(&[
        "list",
        PATH,
        "--max-bytes",
        "4096",
        "--limit",
        "2",
        "--recursive",
        "true",
        "--timeout",
        "30s",
    ]);
    let (client, http, _) = client([reply(json!({
        "value":[item("a"),item("b"),item("c")],
        "continuationToken":"provider-token-never-emit"
    }))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let envelope = value(&result);
    assert_eq!(result.exit_code(), 1);
    assert_eq!(envelope["status"], "partial");
    assert_eq!(envelope["data"], json!([item("a"), item("b")]));
    assert_eq!(envelope["meta"]["coverage"]["complete"], false);
    assert_eq!(envelope["meta"]["coverage"]["truncated"], true);
    assert_eq!(envelope["meta"]["coverage"]["continuation"], Value::Null);
    assert_eq!(envelope["meta"]["coverage"]["resumable"], false);
    assert!(!envelope.to_string().contains("provider-token-never-emit"));
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn projection_changes_only_data_and_preserves_json_scalar_types() {
    let command = parsed(&[
        "list",
        PATH,
        "--max-bytes",
        "4096",
        "--limit",
        "2",
        "--recursive",
        "false",
        "--timeout",
        "30s",
        "--project",
        "length(@)",
    ]);
    let (client, _, _) = client([reply(json!({"value":[item("a")]}))]);
    let envelope = value(&complete(execute(
        &client,
        &command,
        NOW,
        pending(),
        pending(),
    )));
    assert_eq!(envelope["data"], json!(1));
    assert_eq!(envelope["meta"]["target"]["id"], W);
    assert_eq!(envelope["meta"]["coverage"]["complete"], true);
}

#[test]
fn projection_cannot_rescue_over_budget_primary_metadata() {
    let command = parsed(&[
        "get",
        PATH,
        "--max-bytes",
        "2",
        "--timeout",
        "30s",
        "--project",
        "id",
    ]);
    let (client, _, _) = client([reply(json!({"id":W,"displayName":"large"}))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let envelope = value(&result);
    assert_ne!(result.exit_code(), 0);
    assert_eq!(envelope["data"], Value::Null);
    assert_eq!(envelope["error"]["code"], "BudgetExceeded");
}

#[test]
fn service_denials_are_not_retried_as_admin_or_another_identity() {
    for (status, code) in [
        (401, "Unauthenticated"),
        (403, "AccessDenied"),
        (404, "NotFound"),
    ] {
        let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
        let mut response = reply(json!({"errorCode":"Denied","message":"secret provider text"}));
        response.status = status;
        let (client, http, credential) = client([response]);
        let envelope = value(&complete(execute(
            &client,
            &command,
            NOW,
            pending(),
            pending(),
        )));
        assert_eq!(envelope["error"]["code"], code);
        assert!(!envelope.to_string().contains("secret provider text"));
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
}

#[test]
fn ready_cancellation_and_deadline_win_before_credentials() {
    for cancel in [true, false] {
        let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
        let (client, http, credential) = client([]);
        let result = if cancel {
            complete(execute(&client, &command, NOW, ready(()), pending()))
        } else {
            complete(execute(&client, &command, NOW, pending(), ready(())))
        };
        assert_eq!(
            value(&result)["error"]["code"],
            if cancel { "Cancelled" } else { "Timeout" },
        );
        assert!(http.requests.lock().unwrap().is_empty());
        assert!(credential.scopes.lock().unwrap().is_empty());
    }
}

#[test]
fn root_deadline_and_cancellation_win_before_or_during_native_page() {
    for cancel in [true, false] {
        for in_flight in [true, false] {
            let command = parsed(&[
                "list",
                "/",
                "--limit",
                "1",
                "--max-bytes",
                "4096",
                "--timeout",
                "30s",
            ]);
            let mut transport = Http::new([]);
            transport.hold_after = Some(1);
            let http = Arc::new(transport);
            let credential = Arc::new(Credential::default());
            let client = ApiClient::new(
                credential.clone(),
                CredentialSelection::new("selected-account", "selected-tenant", Audience::Fabric)
                    .unwrap(),
                Arc::new(fabric_cli_resources::ResourceHttpExecutor(Arc::new(
                    executor(http.clone(), Arc::new(Timer::default())),
                ))),
            );
            let interrupt = poll_fn(|cx| {
                if in_flight && http.requests.lock().unwrap().is_empty() {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            });
            let result = if cancel {
                complete(execute(&client, &command, NOW, interrupt, pending()))
            } else {
                complete(execute(&client, &command, NOW, pending(), interrupt))
            };
            let envelope = value(&result);
            assert_eq!(
                envelope["error"]["code"],
                if cancel { "Cancelled" } else { "Timeout" }
            );
            assert_eq!(envelope["meta"]["coverage"]["complete"], Value::Null);
            assert_eq!(envelope["meta"]["context"]["account"], "selected-account");
            assert_eq!(http.requests.lock().unwrap().len(), usize::from(in_flight));
            assert_eq!(
                credential.scopes.lock().unwrap().len(),
                usize::from(in_flight)
            );
        }
    }
}

#[test]
fn in_flight_cancellation_and_deadline_drop_read_without_reissue() {
    for cancel in [true, false] {
        let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
        let mut transport = Http::new([]);
        transport.hold_after = Some(1);
        let http = Arc::new(transport);
        let credential = Arc::new(Credential::default());
        let client = ApiClient::new(
            credential.clone(),
            CredentialSelection::new("selected-account", "selected-tenant", Audience::Fabric)
                .unwrap(),
            Arc::new(fabric_cli_resources::ResourceHttpExecutor(Arc::new(
                executor(http.clone(), Arc::new(Timer::default())),
            ))),
        );
        let interrupt = poll_fn(|cx| {
            if http.requests.lock().unwrap().is_empty() {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        });
        let result = if cancel {
            complete(execute(&client, &command, NOW, interrupt, pending()))
        } else {
            complete(execute(&client, &command, NOW, pending(), interrupt))
        };
        assert_eq!(
            value(&result)["error"]["code"],
            if cancel { "Cancelled" } else { "Timeout" },
        );
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
}

#[test]
fn oversized_projection_is_an_input_error_before_remote_work() {
    let expression = "a".repeat(4097);
    let error = parse_args(
        [
            "fab-cli-workspace",
            "get",
            PATH,
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
            "--project",
            &expression,
        ]
        .map(OsString::from),
    )
    .unwrap_err();
    assert_eq!(error, WorkspaceReadError::InvalidInput);
    assert_eq!(failure_envelope(None, error, None).exit_code(), 2);
}

#[test]
fn parser_requires_explicit_budgets_and_rejects_unadvertised_surfaces() {
    let invalid = [
        vec!["get", PATH],
        vec!["get", PATH, "--max-bytes", "0", "--timeout", "30s"],
        vec!["get", PATH, "--max-bytes", "1", "--timeout", "0s"],
        vec!["get", PATH, "--max-bytes", "1", "--timeout", "25h"],
        vec!["get", PATH, "--max-bytes", "1", "--timeout", "1.5s"],
        vec![
            "get",
            PATH,
            "--max-bytes",
            "1",
            "--timeout",
            "30s",
            "--project",
            "[?",
        ],
        vec![
            "get",
            PATH,
            "--max-bytes",
            "1",
            "--timeout",
            "30s",
            "--profile",
            "other",
        ],
        vec![
            "list",
            PATH,
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
            "--limit",
            "2",
        ],
        vec!["run", PATH, "--timeout", "30s"],
        vec![
            "get",
            "/name.Workspace",
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
        ],
        vec![
            "get",
            "/../x.Workspace",
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
        ],
        vec![
            "get",
            "/x.Lakehouse",
            "--max-bytes",
            "4096",
            "--timeout",
            "30s",
        ],
    ];
    for args in invalid {
        assert!(
            parse_args(
                std::iter::once(OsString::from("fab-cli-workspace"))
                    .chain(args.iter().map(|arg| OsString::from(*arg))),
            )
            .is_err(),
            "{args:?}",
        );
    }
    let uppercase = format!("/{}.Workspace", W.to_uppercase());
    let command = parsed(&["get", &uppercase, "--max-bytes", "4096", "--timeout", "24h"]);
    assert_eq!(command.request().workspace_id(), Some(W));
}

#[test]
fn root_list_is_access_filtered_not_admin_and_never_invents_a_workspace_id() {
    let rows = vec![
        json!({"id":W,"displayName":"Same","type":"FutureWorkspace"}),
        json!({"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","displayName":"Same","type":"Personal"}),
    ];
    let command = parsed(&[
        "list",
        "/",
        "--max-bytes",
        "4096",
        "--limit",
        "2",
        "--timeout",
        "30s",
    ]);
    assert_eq!(command.request().workspace_id(), None);
    let (client, http, credential) = client([reply(json!({"value":rows}))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let envelope = value(&result);
    assert_eq!(result.exit_code(), 0);
    assert_eq!(envelope["data"], json!(rows));
    assert_eq!(envelope["meta"]["target"]["path"], "/");
    assert_eq!(envelope["meta"]["target"]["id"], Value::Null);
    assert_eq!(envelope["meta"]["target"]["type"], "Workspaces");
    assert_eq!(envelope["meta"]["target"]["scope"], "accessible-workspaces");
    assert_eq!(envelope["meta"]["context"]["account"], "offline-account");
    assert_eq!(envelope["meta"]["page"]["recordsObserved"], 2);
    assert_eq!(envelope["meta"]["page"]["recordsReturned"], 2);
    assert_eq!(
        *http.requests.lock().unwrap(),
        ["https://api.fabric.microsoft.com/v1/workspaces"]
    );
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    http.assert_consumed();
}

#[test]
fn root_projection_preserves_native_paging_coverage_without_exposing_a_token() {
    for rows in [
        vec![],
        vec![json!({"id":W,"displayName":"Name","type":"Workspace"})],
    ] {
        let command = parsed(&[
            "list",
            "/",
            "--max-bytes",
            "4096",
            "--limit",
            "1",
            "--timeout",
            "30s",
            "--project",
            "length(@)",
        ]);
        let (client, http, _) = client([reply(
            json!({"value":rows,"continuationToken":"native-page-two"}),
        )]);
        let result = complete(execute(&client, &command, NOW, pending(), pending()));
        let envelope = value(&result);
        assert_eq!(result.exit_code(), 1);
        assert_eq!(envelope["status"], "partial");
        assert_eq!(envelope["data"], json!(rows.len()));
        assert_eq!(envelope["meta"]["coverage"]["complete"], false);
        assert_eq!(envelope["meta"]["coverage"]["continuation"], Value::Null);
        assert_eq!(envelope["meta"]["coverage"]["resumable"], false);
        assert_eq!(envelope["meta"]["page"]["nativeContinuation"], Value::Null);
        assert_eq!(envelope["meta"]["page"]["hasMore"], true);
        assert_eq!(envelope["meta"]["page"]["outputTruncated"], false);
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert!(!format!("{result:?}").contains("native-page-two"));
        assert!(!envelope.to_string().contains("native-page-two"));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            write_result(
                OutputReservation::new(None).unwrap(),
                &result,
                &mut stdout,
                &mut stderr,
            )
            .unwrap(),
            1,
        );
        assert!(stderr.is_empty());
        assert!(
            !String::from_utf8(stdout)
                .unwrap()
                .contains("native-page-two")
        );
    }
}

#[test]
fn item_and_root_clipping_withhold_next_token_and_keep_preprojection_counts() {
    for (path, recursive, rows) in [
        (
            PATH,
            vec!["--recursive", "false"],
            vec![item("one"), item("two")],
        ),
        (
            "/",
            vec![],
            vec![
                json!({"id":W,"displayName":"One","type":"Workspace"}),
                json!({"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","displayName":"Two","type":"Workspace"}),
            ],
        ),
    ] {
        for (records, bytes) in [
            ("1", "4096".to_owned()),
            (
                "2",
                serde_json::to_vec(&vec![rows[0].clone()])
                    .unwrap()
                    .len()
                    .to_string(),
            ),
        ] {
            let mut args = vec![
                "list",
                path,
                "--limit",
                records,
                "--max-bytes",
                bytes.as_str(),
                "--timeout",
                "30s",
                "--project",
                "`false`",
            ];
            args.extend_from_slice(&recursive);
            let command = parsed(&args);
            let (client, http, _) = client([reply(
                json!({"value":rows,"continuationToken":"must-not-skip"}),
            )]);
            let result = complete(execute(&client, &command, NOW, pending(), pending()));
            let envelope = value(&result);
            assert_eq!(envelope["data"], false);
            assert_eq!(result.exit_code(), 1);
            assert_eq!(envelope["meta"]["page"]["recordsObserved"], 2);
            assert_eq!(envelope["meta"]["page"]["recordsReturned"], 1);
            assert_eq!(envelope["meta"]["page"]["outputTruncated"], true);
            assert_eq!(envelope["meta"]["page"]["nativeContinuation"], Value::Null);
            assert!(!envelope.to_string().contains("must-not-skip"));
            assert_eq!(http.requests.lock().unwrap().len(), 1);
        }
    }
}

#[test]
fn root_list_rejects_facade_recursion_admin_and_resume_options_before_remote_work() {
    for prefix in [
        vec!["list", "/", "--recursive", "false"],
        vec!["artifact", "workspace", "list", "/"],
        vec!["list", "/admin/workspaces"],
        vec!["list", "/", "--continuation", "unbound-token"],
        vec!["list", "/", "--all"],
    ] {
        let mut args = prefix;
        args.extend(["--limit", "1", "--max-bytes", "4096", "--timeout", "30s"]);
        assert!(
            parse_args(
                std::iter::once(OsString::from("fab-cli-workspace"))
                    .chain(args.iter().map(OsString::from))
            )
            .is_err()
        );
    }
}

#[test]
fn explicit_items_collection_has_the_same_canonical_contract() {
    let path = format!("{PATH}/items");
    let command = parsed(&[
        "list",
        &path,
        "--max-bytes",
        "4096",
        "--limit",
        "3",
        "--recursive",
        "false",
        "--timeout",
        "1ms",
    ]);
    let envelope = failure_envelope(Some(&command), WorkspaceReadError::Cancelled, Some(NOW));
    assert_eq!(value(&envelope)["meta"]["target"]["path"], path);
    assert!(matches!(
        parse_args(["fab-cli-workspace", "--help"].map(OsString::from)).unwrap(),
        ParsedWorkspaceInvocation::Display(_),
    ));
    assert!(matches!(
        parse_args(["fab-cli-workspace", "--version"].map(OsString::from)).unwrap(),
        ParsedWorkspaceInvocation::Display(_),
    ));
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fabric-workspace-v2-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        // Resolve system temp aliases once; product output rejects symlink parents.
        Self(fs::canonicalize(path).unwrap())
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        for entry in fs::read_dir(&self.0).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(&self.0).unwrap();
    }
}

#[test]
fn no_clobber_preflight_and_atomic_publication_preserve_existing_output() {
    let directory = TestDirectory::new();
    let path = directory.0.join("result.json");
    fs::write(&path, "original").unwrap();
    assert!(matches!(
        OutputReservation::new(Some(&path)),
        Err(WorkspaceReadError::OutputExists),
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    fs::remove_file(&path).unwrap();
    let reservation = OutputReservation::new(Some(&path)).unwrap();
    assert!(!path.exists());
    let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
    let (client, http, _) = client([reply(json!({"id":W,"displayName":"workspace"}))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        write_result(reservation, &result, &mut stdout, &mut stderr).unwrap(),
        0,
    );
    assert!(stdout.is_empty() && stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
        value(&result),
    );
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn output_created_after_preflight_is_never_overwritten_or_retried() {
    let directory = TestDirectory::new();
    let path = directory.0.join("result.json");
    let reservation = OutputReservation::new(Some(&path)).unwrap();
    fs::write(&path, "racing writer").unwrap();
    let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
    let (client, http, _) = client([reply(json!({"id":W,"displayName":"workspace"}))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        write_result(reservation, &result, &mut stdout, &mut stderr).unwrap(),
        1,
    );
    assert!(stdout.is_empty());
    assert_eq!(fs::read_to_string(&path).unwrap(), "racing writer");
    assert_eq!(
        serde_json::from_slice::<Value>(&stderr).unwrap()["error"]["code"],
        "OutputExists",
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn failed_read_cleans_reservation_without_publishing_a_success_file() {
    let directory = TestDirectory::new();
    let path = directory.0.join("result.json");
    let reservation = OutputReservation::new(Some(&path)).unwrap();
    let failed = failure_envelope(None, WorkspaceReadError::AccessDenied, None);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        write_result(reservation, &failed, &mut stdout, &mut stderr).unwrap(),
        1,
    );
    assert!(stdout.is_empty());
    assert!(!stderr.is_empty());
    assert!(!path.exists());
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
}

struct BrokenWriter;

#[test]
fn output_failure_retains_native_counts_but_does_not_log_opaque_continuation() {
    let command = parsed(&[
        "list",
        "/",
        "--limit",
        "1",
        "--max-bytes",
        "4096",
        "--timeout",
        "30s",
    ]);
    let (client, http, _) = client([reply(json!({
        "value": [{"id":W,"displayName":"Workspace","type":"Workspace"}],
        "continuationToken":"private-next-page"
    }))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    assert_eq!(
        value(&result)["meta"]["page"]["nativeContinuation"],
        Value::Null
    );
    assert!(!value(&result).to_string().contains("private-next-page"));
    let mut stderr = Vec::new();
    assert_eq!(
        write_result(
            OutputReservation::new(None).unwrap(),
            &result,
            &mut BrokenWriter,
            &mut stderr
        )
        .unwrap(),
        1
    );
    let failed: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(failed["error"]["code"], "OutputFailed");
    assert_eq!(failed["meta"]["page"]["recordsObserved"], 1);
    assert_eq!(failed["meta"]["page"]["hasMore"], true);
    assert_eq!(failed["meta"]["page"]["nativeContinuation"], Value::Null);
    assert!(
        !String::from_utf8(stderr)
            .unwrap()
            .contains("private-next-page")
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("injected local stream failure"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn stdout_failure_retains_target_without_reissuing_read() {
    let command = parsed(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"]);
    let (client, http, _) = client([reply(json!({"id":W,"displayName":"workspace"}))]);
    let result = complete(execute(&client, &command, NOW, pending(), pending()));
    let mut stderr = Vec::new();
    assert_eq!(
        write_result(
            OutputReservation::new(None).unwrap(),
            &result,
            &mut BrokenWriter,
            &mut stderr,
        )
        .unwrap(),
        1,
    );
    let failed: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(failed["error"]["code"], "OutputFailed");
    assert_eq!(failed["meta"]["target"]["id"], W);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

fn process(args: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_fab-cli-workspace"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.args(args);
    command
}

#[test]
fn executable_help_and_version_do_not_enter_native_or_filesystem_setup() {
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["artifact", "workspace", "--help"],
    ] {
        let output = process(&args)
            .env("FAB_RUST_CONFIG_ROOT", "must-not-read-this-relative-config")
            .env("FAB_RUST_AUTH_MODE", "must-not-select-this-mode")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("fab-cli-workspace"));
        assert!(!text.contains("must-not-"));
    }
}

#[test]
fn executable_invalid_input_is_a_sanitized_v2_stderr_envelope() {
    let output = process(&["get", PATH, "--unadvertised-secret", "must-not-echo"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not-echo"));
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["schemaVersion"], "0.1");
    assert_eq!(envelope["status"], "failed");
    assert_eq!(envelope["error"]["code"], "InvalidInput");
}

#[cfg(not(feature = "native-host"))]
#[test]
fn executable_without_host_returns_not_configured_instead_of_success() {
    let output = process(&["get", PATH, "--max-bytes", "4096", "--timeout", "30s"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["error"]["code"], "NotConfigured");
    assert_eq!(envelope["meta"]["target"]["id"], W);
}

#[test]
fn executable_output_exists_precedes_invalid_native_identity_configuration() {
    let directory = TestDirectory::new();
    let path = directory.0.join("existing.json");
    fs::write(&path, "original").unwrap();
    let output = process(&[
        "get",
        PATH,
        "--max-bytes",
        "4096",
        "--timeout",
        "30s",
        "--output",
        path.to_str().unwrap(),
    ])
    .env("FAB_RUST_AUTH_MODE", "must-not-select-this-mode")
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["error"]["code"], "OutputExists");
    assert_eq!(fs::read_to_string(path).unwrap(), "original");
}
