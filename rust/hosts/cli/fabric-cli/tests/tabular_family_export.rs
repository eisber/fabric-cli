// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit ACL, catalog, and table-schema exports through native family handlers.
//! Native family adapter contracts for shared tabular exports.

#![cfg(feature = "native-host")]
#![deny(unnameable_test_items)]

#[allow(dead_code, unused_imports)]
mod family_support;
#[allow(dead_code, unused_imports)]
mod folder_support;

use fabric_cli::{
    families::FamilySession,
    parser::{Invocation, parse_args},
    session::ReadSession,
    tabular_export::{self, ExecutionFailure, ExportDelivery, ExportRequest, ExportSink},
};
use fabric_cli_context::{Element, HomeContext, PathElement};
use fabric_cli_local::{ConfigStore, NavigationState};
use fabric_cli_mutations::{CommandFuture, MutationUi, NoticeKind};
use fabric_cli_output::Timestamp;
use fabric_sdk::{Audience, Client, CredentialSelection};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    future::pending,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

struct Fixture {
    root: PathBuf,
    http: Arc<family_support::Http>,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "tabular-family-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        ConfigStore::new(&root).initialize().unwrap();
        Self {
            root,
            http: Arc::new(family_support::Http {
                replies: Mutex::default(),
                requests: Mutex::default(),
            }),
        }
    }

    fn reply(&self, status: u16, body: Value) {
        self.http
            .replies
            .lock()
            .unwrap()
            .push_back((status, body, BTreeMap::new()));
    }

    fn metadata(&self, item: Option<&str>) {
        self.reply(
            200,
            json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}),
        );
        if let Some(kind) = item {
            self.reply(
                200,
                json!({"value":[{"id":"n","displayName":"Book","type":kind}]}),
            );
        }
    }

    async fn dispatch(&self, leaf: &[&str], format: &str, sink: &mut Sink) -> ExportDelivery {
        let Invocation::Tabular(export) =
            parse_args(["fab-rs"].into_iter().chain(leaf.iter().copied()).chain([
                "--format",
                format,
                "--output",
                "table.data",
            ]))
            .unwrap()
        else {
            panic!("expected explicit export")
        };
        let credential = Arc::new(family_support::Credential::default());
        let sdk = Client::new(
            credential,
            CredentialSelection::new("offline-account", "tenant", Audience::Fabric).unwrap(),
        );
        let executor = HttpExecutor::new(
            self.http.clone(),
            Arc::new(folder_support::Timer::default()),
            ExecutionOptions::default(),
        )
        .unwrap();
        let tenant = Element::tenant("Tenant", "tenant");
        let reads = ReadSession {
            sdk: &sdk,
            executor: &executor,
            tenant: tenant.clone(),
            current_path: "/",
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            config: Default::default(),
        };
        let store = ConfigStore::new(&self.root);
        let mut navigation = NavigationState::new(
            PathElement::from_fabric(tenant),
            HomeContext::Unspecified,
            self.root.to_string_lossy().into_owned(),
            37,
            true,
        )
        .unwrap();
        let mut local = family_support::Effects::default();
        let mut mutations = NoMutation;
        let ui = family_support::TransferUiCapture::default();
        let mut family = FamilySession {
            reads: &reads,
            http: &executor,
            store: &store,
            navigation: &mut navigation,
            local: &mut local,
            mutations: &mut mutations,
            confirmation: None,
            transfer_ui: &ui,
            transfer_files: None,
            resource_files: None,
        };
        tabular_export::dispatch(&export, sink, |invocation| async move {
            family
                .dispatch_tabular(invocation, pending())
                .await
                .map_err(|_| ExecutionFailure {
                    code: "adapter_failed".into(),
                    message: "Native adapter failed".into(),
                })
                .and_then(|output| {
                    if !matches!(
                        output.evidence,
                        fabric_cli::families::FamilyEvidence::CommandFailure(_)
                    ) {
                        assert!(
                            output.plan.records.is_empty(),
                            "export must consume native evidence before rendering"
                        );
                    }
                    tabular_export::family_rows(invocation, output)
                })
        })
        .await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct NoMutation;

impl MutationUi for NoMutation {
    fn notice(&mut self, _: NoticeKind, _: &str) {
        panic!("tabular reads cannot emit mutation notices")
    }
    fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
        Box::pin(async { panic!("tabular reads cannot request mutation confirmation") })
    }
}

#[derive(Default)]
struct Sink {
    reject_preflight: bool,
    publishes: usize,
    bytes: Vec<u8>,
}

impl ExportSink for Sink {
    fn preflight(&mut self, _: &ExportRequest) -> io::Result<()> {
        if self.reject_preflight {
            Err(io::Error::other("no clobber"))
        } else {
            Ok(())
        }
    }

    fn publish(&mut self, _: &ExportRequest, bytes: &[u8]) -> io::Result<()> {
        self.publishes += 1;
        self.bytes = bytes.to_vec();
        Ok(())
    }
}

#[tokio::test]
async fn actual_acl_leaves_use_native_collections_for_each_shared_codec() {
    for leaf in ["ls", "dir", "get"] {
        for format in ["csv", "json", "parquet"] {
            let fixture = Fixture::new();
            fixture.metadata(None);
            fixture.reply(
                200,
                json!({"value":[{
                    "id":"assignment","role":"Admin","principal":{
                        "id":"person","type":"User","displayName":"Alice",
                        "userDetails":{"userPrincipalName":"alice@example.test"},
                    }
                }]}),
            );
            let mut args = vec!["acl", leaf, "Demo.Workspace"];
            if leaf == "get" {
                args.extend(["-q", "."]);
            }
            let mut sink = Sink::default();
            let output = fixture.dispatch(&args, format, &mut sink).await;
            assert_eq!(output.exit_code, 0, "{leaf} {format}: {:?}", output.receipt);
            assert_eq!(output.receipt["delivery"]["rows"], 1);
            assert_eq!(sink.publishes, 1);
            assert_eq!(fixture.http.requests.lock().unwrap().len(), 2);
            assert!(fixture.http.replies.lock().unwrap().is_empty());
            if format == "json" {
                let data: Value = serde_json::from_slice(&sink.bytes).unwrap();
                assert!(data.is_array() && data.get("result").is_none());
            } else if format == "parquet" {
                assert!(sink.bytes.starts_with(b"PAR1") && sink.bytes.ends_with(b"PAR1"));
            }
        }
    }
}

#[tokio::test]
async fn actual_find_uses_catalog_rows_without_metadata_prefetch_or_diagnostic_rows() {
    for format in ["csv", "json", "parquet"] {
        let fixture = Fixture::new();
        fixture.reply(
            200,
            json!({"value":[{
                "id":"item","displayName":"Book","type":"Notebook",
                "hierarchy":{"workspace":{"id":"w","displayName":"Demo"}},
            }]}),
        );
        let mut sink = Sink::default();
        let output = fixture.dispatch(&["find", "Book"], format, &mut sink).await;
        assert_eq!(output.exit_code, 0, "{:?}", output.receipt);
        assert_eq!(output.receipt["delivery"]["rows"], 1);
        let requests = fixture.http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["method"], "POST");
        assert!(
            requests[0]["url"]
                .as_str()
                .unwrap()
                .ends_with("/catalog/search")
        );
        assert!(fixture.http.replies.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn actual_schema_uses_storage_metadata_not_progress_messages_as_rows() {
    for format in ["csv", "json", "parquet"] {
        let fixture = Fixture::new();
        fixture.metadata(Some("Lakehouse"));
        fixture.http.replies.lock().unwrap().push_back((
            200,
            Value::Null,
            BTreeMap::from([("x-ms-resource-type".into(), "directory".into())]),
        ));
        fixture.reply(
            200,
            json!({"paths":[{"name":"n/Tables/sales/_delta_log/00001.json"}]}),
        );
        fixture.reply(200, json!({"metaData":{"schemaString":"{\"fields\":[{\"name\":\"id\",\"type\":\"long\",\"nullable\":false,\"metadata\":{\"comment\":\"native\"}},{\"name\":\"names\",\"type\":{\"type\":\"array\",\"elementType\":\"string\"},\"nullable\":true,\"metadata\":{}}]}"}}));
        let mut sink = Sink::default();
        let output = fixture
            .dispatch(
                &[
                    "table",
                    "schema",
                    "Demo.Workspace/Book.Lakehouse/Tables/sales",
                ],
                format,
                &mut sink,
            )
            .await;
        assert_eq!(output.exit_code, 0, "{:?}", output.receipt);
        assert_eq!(output.receipt["delivery"]["rows"], 2);
        assert_eq!(fixture.http.requests.lock().unwrap().len(), 5);
        assert!(fixture.http.replies.lock().unwrap().is_empty());
        if format == "json" {
            let data: Value = serde_json::from_slice(&sink.bytes).unwrap();
            assert_eq!(data[0]["metadata"]["comment"], "native");
        }
    }
}

#[tokio::test]
async fn family_preflight_precedes_send_and_native_failure_never_exports_partial_data() {
    for leaf in [
        &["acl", "ls", "Demo.Workspace"][..],
        &["acl", "dir", "Demo.Workspace"][..],
        &["acl", "get", "Demo.Workspace", "-q", "."][..],
        &["find", "Book"][..],
        &[
            "table",
            "schema",
            "Demo.Workspace/Book.Lakehouse/Tables/sales",
        ][..],
    ] {
        let fixture = Fixture::new();
        let mut sink = Sink {
            reject_preflight: true,
            ..Default::default()
        };
        let output = fixture.dispatch(leaf, "json", &mut sink).await;
        assert_eq!(output.receipt["execution"]["status"], "not_started");
        assert!(fixture.http.requests.lock().unwrap().is_empty());
        assert_eq!(sink.publishes, 0);
    }
    let fixture = Fixture::new();
    fixture.reply(
        403,
        json!({"errorCode":"Forbidden","message":"Offline denial"}),
    );
    let mut sink = Sink::default();
    let output = fixture
        .dispatch(&["acl", "ls", "Demo.Workspace"], "json", &mut sink)
        .await;
    assert_ne!(output.exit_code, 0);
    assert_eq!(output.receipt["execution"]["status"], "failed");
    assert_eq!(fixture.http.requests.lock().unwrap().len(), 1);
    assert_eq!(sink.publishes, 0);
}

#[tokio::test]
async fn actual_empty_acl_lists_and_raw_collections_have_real_endpoint_schemas() {
    for (leaf, item, names) in [
        (
            &["acl", "ls", "Demo.Workspace"][..],
            false,
            &["acl", "identity", "type"][..],
        ),
        (
            &["acl", "dir", "Demo.Workspace", "--long"][..],
            false,
            &["acl", "identity", "type", "objectId", "name"][..],
        ),
        (
            &["acl", "get", "Demo.Workspace", "-q", "."][..],
            false,
            &["id", "principal", "role"][..],
        ),
        (
            &["acl", "ls", "Demo.Workspace/Book.Notebook"][..],
            true,
            &["acl", "identity", "type"][..],
        ),
        (
            &["acl", "dir", "Demo.Workspace/Book.Notebook", "--long"][..],
            true,
            &["id", "name", "acl", "identity", "type"][..],
        ),
        (
            &["acl", "get", "Demo.Workspace/Book.Notebook", "-q", "."][..],
            true,
            &["principal", "itemAccessDetails"][..],
        ),
    ] {
        for format in ["json", "csv", "parquet"] {
            let fixture = Fixture::new();
            fixture.metadata(item.then_some("Notebook"));
            fixture.reply(
                200,
                if item {
                    json!({"accessDetails":[]})
                } else {
                    json!({"value":[]})
                },
            );
            let mut sink = Sink::default();
            let output = fixture.dispatch(leaf, format, &mut sink).await;
            assert_empty(&output, &sink, format, names);
            assert_eq!(
                fixture.http.requests.lock().unwrap().len(),
                if item { 3 } else { 2 }
            );
            assert!(fixture.http.replies.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn actual_empty_find_and_table_schema_export_all_formats_without_fake_rows() {
    for long in [false, true] {
        for format in ["json", "csv", "parquet"] {
            let fixture = Fixture::new();
            fixture.reply(200, json!({"value":[]}));
            let mut leaf = vec!["find", "missing"];
            if long {
                leaf.push("--long");
            }
            let mut sink = Sink::default();
            let output = fixture.dispatch(&leaf, format, &mut sink).await;
            let names = if long {
                &["name", "id", "type", "workspace", "workspace_id"][..]
            } else {
                &["name", "type", "workspace"][..]
            };
            assert_empty(&output, &sink, format, names);
            assert_eq!(fixture.http.requests.lock().unwrap().len(), 1);
        }
    }
    for format in ["json", "csv", "parquet"] {
        let fixture = Fixture::new();
        fixture.metadata(Some("Lakehouse"));
        fixture.http.replies.lock().unwrap().push_back((
            200,
            Value::Null,
            BTreeMap::from([("x-ms-resource-type".into(), "directory".into())]),
        ));
        fixture.reply(
            200,
            json!({"paths":[{"name":"n/Tables/sales/_delta_log/00001.json"}]}),
        );
        fixture.reply(200, json!({"metaData":{"schemaString":"{\"fields\":[]}"}}));
        let mut sink = Sink::default();
        let output = fixture
            .dispatch(
                &[
                    "table",
                    "schema",
                    "Demo.Workspace/Book.Lakehouse/Tables/sales",
                ],
                format,
                &mut sink,
            )
            .await;
        assert_empty(
            &output,
            &sink,
            format,
            &["name", "type", "nullable", "metadata"],
        );
        assert_eq!(output.receipt["delivery"]["schema"][1]["type"], "json");
        assert_eq!(output.receipt["delivery"]["schema"][2]["type"], "boolean");
        assert_eq!(output.receipt["delivery"]["schema"][3]["type"], "json");
        assert_eq!(fixture.http.requests.lock().unwrap().len(), 5);
        assert!(fixture.http.replies.lock().unwrap().is_empty());
    }
}

fn assert_empty(output: &ExportDelivery, sink: &Sink, format: &str, names: &[&str]) {
    assert_eq!(output.exit_code, 0, "{:?}", output.receipt);
    assert_eq!(output.receipt["delivery"]["rows"], 0);
    assert_eq!(sink.publishes, 1);
    let schema = output.receipt["delivery"]["schema"].as_array().unwrap();
    assert_eq!(
        schema
            .iter()
            .map(|column| column["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        names
    );
    match format {
        "json" => assert_eq!(
            serde_json::from_slice::<Value>(&sink.bytes).unwrap(),
            json!([])
        ),
        "csv" => assert_eq!(sink.bytes, format!("{}\r\n", names.join(",")).into_bytes()),
        "parquet" => assert!(sink.bytes.starts_with(b"PAR1") && sink.bytes.ends_with(b"PAR1")),
        _ => unreachable!("fixed format list"),
    }
}
