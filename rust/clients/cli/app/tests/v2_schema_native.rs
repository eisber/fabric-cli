// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parser -> production TABLE resolver -> Delta provider -> shared HTTP, offline.

#![cfg(all(feature = "v2-discovery", feature = "native-host"))]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, Body, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::{
    session::ReadSession, v2_discovery::DiscoveryHost, v2_schema_native::NativeSchemaProvider,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_output::Timestamp;
use fabric_sdk::{Audience, Client, CredentialSelection};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_tables::TableLimits;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, ready},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

const TABLE: &str = "/Demo.Workspace/Book.Lakehouse/Tables/sales";

#[derive(Debug, Default)]
struct Credential(Mutex<Vec<Vec<String>>>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).into()).collect());
        Ok(AccessToken::new(
            "offline-schema-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    headers: Vec<(&'static str, &'static str)>,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
            headers: Vec::new(),
        }
    }

    fn directory(shortcut: bool) -> Self {
        Self {
            status: 200,
            body: Vec::new(),
            headers: vec![
                ("x-ms-resource-type", "directory"),
                (
                    "x-ms-onelake-shortcut-path",
                    if shortcut { "true" } else { "false" },
                ),
            ],
        }
    }
}

#[derive(Debug, Default)]
struct Http {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<(Method, String)>>,
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reply")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl HttpClient for Http {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert!(matches!(request.method(), Method::Get | Method::Head));
        assert!(matches!(request.body(), Body::Bytes(bytes) if bytes.is_empty()));
        assert!(request.headers().get_str(&"authorization".into()).is_ok());
        self.requests
            .lock()
            .unwrap()
            .push((request.method(), request.url().to_string()));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unplanned HTTP");
        let mut headers = Headers::new();
        for (name, value) in reply.headers {
            headers.insert(name, value);
        }
        Ok(AsyncRawResponse::from_bytes(
            StatusCode::from(reply.status),
            headers,
            reply.body,
        ))
    }
}

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(pending())
    }
}

struct Host {
    credential: Arc<Credential>,
    transport: Arc<Http>,
    sdk: Client,
    http: HttpExecutor,
}

impl Host {
    fn new() -> Self {
        let credential = Arc::new(Credential::default());
        let transport = Arc::new(Http::default());
        Self {
            sdk: Client::new(
                credential.clone(),
                CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
            ),
            http: HttpExecutor::new(
                transport.clone(),
                Arc::new(Timer),
                ExecutionOptions::default(),
            )
            .unwrap(),
            credential,
            transport,
        }
    }

    fn reply(&self, reply: Reply) {
        self.transport.replies.lock().unwrap().push_back(reply);
    }

    fn target(&self) {
        self.reply(Reply::json(json!({"value":[{
            "id":"w","displayName":"Demo","type":"Workspace"
        }]})));
        self.reply(Reply::json(json!({"value":[{
            "id":"n","displayName":"Book","type":"Lakehouse","workspaceId":"w"
        }]})));
        self.reply(Reply::directory(false));
    }

    fn delta(&self, local: &str, native: &Value) {
        self.reply(Reply::json(json!({"paths":[{
            "name":format!("n/{local}/_delta_log/00001.json")
        }]})));
        self.reply(Reply::json(
            json!({"metaData":{"schemaString":native.to_string()}}),
        ));
    }

    async fn run(
        &self,
        args: &[&str],
        limits: TableLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> (i32, Vec<u8>, Vec<u8>) {
        let reads = ReadSession {
            sdk: &self.sdk,
            executor: &self.http,
            tenant: Element::tenant("Tenant", "tenant"),
            current_path: "/",
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2026-11-11T00:00:00Z").unwrap(),
            config: Default::default(),
        };
        let provider = NativeSchemaProvider {
            reads: &reads,
            http: &self.http,
            limits,
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = DiscoveryHost {
            data_schema: Some(&provider),
        }
        .run(
            std::iter::once("fab-cli-discovery").chain(args.iter().copied()),
            cancelled,
            &mut stdout,
            &mut stderr,
        )
        .await
        .unwrap();
        (exit, stdout, stderr)
    }

    fn requests(&self) -> Vec<(Method, String)> {
        self.transport.requests.lock().unwrap().clone()
    }
}

fn error(output: (i32, Vec<u8>, Vec<u8>)) -> String {
    assert_eq!(output.0, 1);
    assert!(output.1.is_empty());
    let value: Value = serde_json::from_slice(&output.2).unwrap();
    assert_eq!(output.2.last(), Some(&b'\n'));
    value["error"]["code"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn parsed_native_host_preserves_exact_types_provenance_and_both_audiences() {
    for path in [TABLE, "Demo.Workspace/Book.Lakehouse/Tables/dbo/sales"] {
        let host = Host::new();
        host.target();
        if path.contains("/dbo/") {
            host.reply(Reply::directory(false));
        }
        let native = json!({
            "type":"struct",
            "fields":[
                {"name":"id","type":"long","nullable":false,"metadata":{"id":u64::MAX}},
                {"name":"nested","type":{"type":"array","elementType":"decimal(38,7)"}},
                {"name":"unknown","type":"future","nullable":null}
            ]
        });
        let tail = if path.contains("/dbo/") {
            "Tables/dbo/sales"
        } else {
            "Tables/sales"
        };
        host.delta(tail, &native);
        let (exit, out, err) = host
            .run(&["schema", path], Default::default(), pending())
            .await;
        assert_eq!(exit, 0, "{}", String::from_utf8_lossy(&err));
        assert!(err.is_empty());
        assert_eq!(out.last(), Some(&b'\n'));
        let document: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(document["native_schema"], native);
        assert_eq!(document["columns"][0]["nullable"], false);
        assert!(document["columns"][1]["nullable"].is_null());
        assert!(document["columns"][2]["nullable"].is_null());
        assert_eq!(document["schema_kind"], "data");
        assert_eq!(document["backend"], "delta-json-log");
        assert_eq!(document["complete"], Value::Null);
        assert_eq!(document["provenance"]["source_revision"], Value::Null);
        assert_eq!(
            document["provenance"]["snapshot_consistency_established"],
            false
        );
        assert_eq!(document["coverage"]["json_commits_only"], true);
        assert_eq!(document["coverage"]["checkpoints_reconstructed"], false);
        let requests = host.requests();
        assert_eq!(requests.len(), if path.contains("/dbo/") { 6 } else { 5 });
        assert!(
            requests
                .iter()
                .all(|(method, _)| matches!(method, Method::Get | Method::Head))
        );
        assert!(requests[2].1.ends_with(if path.contains("/dbo/") {
            "/w/n/Tables/dbo"
        } else {
            "/w/n/Tables/sales"
        }));
        assert!(
            requests
                .last()
                .unwrap()
                .1
                .ends_with(&format!("/w/n/{tail}/_delta_log/00001.json"))
        );
        assert!(host.transport.replies.lock().unwrap().is_empty());
        let scopes = host.credential.0.lock().unwrap();
        assert!(
            scopes
                .iter()
                .any(|scope| scope == &[Audience::Fabric.scope()])
        );
        assert!(
            scopes
                .iter()
                .any(|scope| scope == &[Audience::OneLake.scope()])
        );
    }
}

#[test]
fn native_binary_with_no_identity_is_unconfigured_without_creating_a_profile() {
    let profile = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_fab-cli-discovery"));
    command
        .env_clear()
        .env("FAB_RUST_CONFIG_ROOT", profile.path());
    for key in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let output = command.args(["schema", TABLE]).output().unwrap();
    assert_eq!(
        error((output.status.code().unwrap(), output.stdout, output.stderr)),
        "NotConfigured"
    );
    assert_eq!(std::fs::read_dir(profile.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn local_help_version_contracts_invalid_and_lexical_shortcuts_never_touch_native_provider() {
    let host = Host::new();
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["schema", "--help"],
        vec!["help", "/Demo.Workspace"],
        vec!["create", "/New.Workspace", "--schema"],
        vec![
            "schema",
            "/Demo.Workspace/Book.Lakehouse/Tables/sales.Shortcut",
        ],
        vec![
            "schema",
            "/Demo.Workspace/Book.Lakehouse/Tables/dbo.shortcut/sales",
        ],
        vec!["schema", TABLE, "--input", "private-input"],
    ] {
        let (exit, _, _) = host.run(&args, Default::default(), pending()).await;
        assert_eq!(
            exit,
            if args.contains(&"--input") {
                2
            } else if args
                .iter()
                .any(|arg| arg.contains(".Shortcut") || arg.contains(".shortcut"))
            {
                1
            } else {
                0
            }
        );
    }
    assert!(host.requests().is_empty());
    assert!(host.credential.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn resolved_shortcut_table_and_schema_fail_before_delta_access() {
    for path in [TABLE, "/Demo.Workspace/Book.Lakehouse/Tables/dbo/sales"] {
        let host = Host::new();
        host.target();
        host.transport
            .replies
            .lock()
            .unwrap()
            .back_mut()
            .unwrap()
            .headers
            .push(("x-ms-onelake-shortcut-path", "true"));
        if path.contains("/dbo/") {
            host.reply(Reply::directory(false));
        }
        assert_eq!(
            error(
                host.run(&["schema", path], Default::default(), pending())
                    .await
            ),
            "resolution_failed"
        );
        assert!(
            host.requests()
                .iter()
                .all(|(_, url)| !url.contains("_delta_log"))
        );
        assert!(host.transport.replies.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn resolved_file_is_not_a_physical_table_and_metadata_failures_are_explicit() {
    for scenario in ["file", "missing", "malformed-metadata"] {
        let host = Host::new();
        host.target();
        {
            let mut replies = host.transport.replies.lock().unwrap();
            match scenario {
                "file" => {
                    replies.back_mut().unwrap().headers = vec![("x-ms-resource-type", "file")];
                }
                "missing" => {
                    replies.back_mut().unwrap().status = 404;
                }
                _ => {
                    replies.clear();
                    replies.push_back(Reply::json(json!({"value":"not-a-list"})));
                }
            }
        }
        assert_eq!(
            error(
                host.run(&["schema", TABLE], Default::default(), pending())
                    .await
            ),
            "resolution_failed"
        );
        assert!(
            host.requests()
                .iter()
                .all(|(_, url)| !url.contains("_delta_log"))
        );
        assert!(host.transport.replies.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn malformed_partial_and_failed_list_pages_never_publish_schema() {
    for scenario in ["malformed", "partial", "list-page"] {
        let host = Host::new();
        host.target();
        if scenario == "malformed" {
            host.reply(Reply::json(
                json!({"paths":[{"name":"n/Tables/sales/_delta_log/1.json"}]}),
            ));
            host.reply(Reply::json(json!({"metaData":{"schemaString":"{"}})));
        } else {
            let mut first =
                Reply::json(json!({"paths":[{"name":"n/Tables/sales/_delta_log/1.json"}]}));
            first.headers.push(("x-ms-continuation", "next-page"));
            host.reply(first);
            let mut failed = Reply::json(json!({"error":{"code":"private-service-message"}}));
            failed.status = if scenario == "partial" { 206 } else { 403 };
            host.reply(failed);
        }
        let (exit, out, err) = host
            .run(&["schema", TABLE], Default::default(), pending())
            .await;
        let text = String::from_utf8_lossy(&err);
        assert_eq!(exit, 1, "{scenario}: {text}");
        assert!(out.is_empty());
        assert!(!text.contains("private-"));
        assert!(host.transport.replies.lock().unwrap().is_empty());
        if scenario != "malformed" {
            assert!(
                host.requests()
                    .last()
                    .unwrap()
                    .1
                    .contains("continuation=next-page")
            );
            assert!(
                host.requests()
                    .last()
                    .unwrap()
                    .1
                    .contains("resource=filesystem")
            );
        }
    }
}

#[tokio::test]
async fn cancellation_and_sdk_request_limits_are_not_empty_success() {
    let host = Host::new();
    assert_eq!(
        error(
            host.run(&["schema", TABLE], Default::default(), ready(()))
                .await
        ),
        "cancelled"
    );
    assert!(host.requests().is_empty());
    assert!(host.credential.0.lock().unwrap().is_empty());
    host.target();
    let limits = TableLimits {
        max_requests: NonZeroUsize::new(1).unwrap(),
        ..Default::default()
    };
    host.reply(Reply::json(
        json!({"paths":[{"name":"n/Tables/sales/_delta_log/1.json"}]}),
    ));
    assert_eq!(
        error(host.run(&["schema", TABLE], limits, pending()).await),
        "limit_exceeded"
    );
    assert_eq!(host.requests().len(), 4);
}
