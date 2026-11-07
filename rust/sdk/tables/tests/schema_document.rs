// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public-boundary contracts for additive native Delta schema documents.

#![deny(unnameable_test_items)]

#[path = "../../../tests/compatibility/jobs/support.rs"]
#[allow(
    dead_code,
    reason = "Shared HTTP fixtures also expose unused job-only helpers."
)]
mod support;

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::Method,
    time::OffsetDateTime,
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_onelake::{OneLakeError, OneLakeExecutionError};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_tables::{
    Audience, CredentialSelection, LoadOutcome, TableClient, TableError, TableErrorKind,
    TableExecutor, TableFuture, TableLimits, TablePath, TableRequest, TableResponse,
    TableWithOneLake,
    schema_document::{DataSchemaBackend, DataSchemaDocument, DataSchemaKind},
};
use serde_json::{Value, json};
use std::{
    future::{pending, ready},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};
use support::{ScriptedHttp, TestTimer, response};

#[derive(Debug, Default)]
struct Credential(Mutex<Vec<String>>);

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
            .extend(scopes.iter().map(|scope| scope.to_string()));
        Ok(AccessToken {
            token: "offline-schema-document-only".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

fn client(credential: Arc<Credential>) -> TableClient {
    TableClient::new(
        credential,
        &CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        &CredentialSelection::new("offline", "tenant", Audience::OneLake).unwrap(),
    )
    .unwrap()
}

struct NoLegacyExecution;

impl TableExecutor for NoLegacyExecution {
    fn read<'a>(
        &'a self,
        _: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async { panic!("selected OneLake SDK must not use legacy raw reads") })
    }

    fn load<'a>(
        &'a self,
        _: TableRequest<'a>,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async { panic!("schema discovery must not submit a load") })
    }
}

fn storage(network: Arc<ScriptedHttp>) -> OneLakeHttpExecutor {
    OneLakeHttpExecutor::new(Arc::new(
        HttpExecutor::new(
            network,
            Arc::new(TestTimer::default()),
            ExecutionOptions::default(),
        )
        .unwrap(),
    ))
}

fn table() -> TablePath {
    TablePath::new("ws", "item", "Tables/sales").unwrap()
}

fn listing() -> &'static str {
    r#"{"paths":[{"name":"item/Tables/sales/_delta_log/00001.json"}]}"#
}

fn metadata(schema: &str) -> String {
    json!({"metaData": {"schemaString": schema}}).to_string()
}

fn assert_invalid_response(result: Result<DataSchemaDocument, TableError>) {
    let error = result.unwrap_err();
    assert_eq!(error.kind, TableErrorKind::InvalidResponse);
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(error.operation_id, None);
}

#[test]
fn columns_preserve_nullability_native_types_metadata_and_field_order() {
    let native = json!({
        "type": "struct",
        "fields": [
            {"name": "required", "type": "long", "nullable": false},
            {
                "name": "nested 名",
                "type": {
                    "type": "struct",
                    "fields": [{
                        "name": "items",
                        "type": {
                            "type": "array",
                            "elementType": {
                                "type": "map",
                                "keyType": "string",
                                "valueType": "decimal(38,18)",
                                "valueContainsNull": false,
                                "futureNativeProperty": [null, 7]
                            },
                            "containsNull": true
                        },
                        "nullable": false,
                        "metadata": {"comment": "unmodified"}
                    }]
                },
                "nullable": true,
                "metadata": {
                    "delta.columnMapping.id": u64::MAX,
                    "delta.columnMapping.physicalName": "col-native"
                },
                "futureFieldProperty": {"keep": true}
            },
            {"name": "missing-nullability", "type": "string"},
            {"name": "null-nullability", "type": "boolean", "nullable": null}
        ],
        "futureRootProperty": {"keep": [false, null, "original"]}
    });
    let document = DataSchemaDocument::from_native_schema(native.clone()).unwrap();
    assert_eq!(document.schema_version, 1);
    assert_eq!(document.schema_kind, DataSchemaKind::Data);
    assert_eq!(document.backend, DataSchemaBackend::DeltaJsonLog);
    assert_eq!(
        document
            .columns
            .iter()
            .map(|column| (column.name.as_str(), column.nullable))
            .collect::<Vec<_>>(),
        [
            ("required", Some(false)),
            ("nested 名", Some(true)),
            ("missing-nullability", None),
            ("null-nullability", None)
        ]
    );
    assert_eq!(document.columns[1].native_type, native["fields"][1]["type"]);
    assert_eq!(document.native_schema, native);
    assert_eq!(document.complete, None);
    assert!(document.coverage.json_commits_only);
    assert!(!document.coverage.checkpoints_reconstructed);
    assert!(!document.coverage.native_semantics_validated);
    assert_eq!(document.provenance.source_revision, None);
    assert!(!document.provenance.snapshot_consistency_established);
    let encoded = serde_json::to_value(&document).unwrap();
    assert_eq!(encoded["schema_version"], 1);
    assert_eq!(encoded["schema_kind"], "data");
    assert_eq!(encoded["backend"], "delta-json-log");
    assert_eq!(encoded["columns"][0]["nullable"], false);
    assert_eq!(encoded["columns"][1]["nullable"], true);
    assert_eq!(encoded["columns"][2].get("nullable"), Some(&Value::Null));
    assert_eq!(encoded["columns"][3].get("nullable"), Some(&Value::Null));
    assert_eq!(encoded["columns"][1]["type"], native["fields"][1]["type"]);
    assert!(encoded["columns"][1].get("native_type").is_none());
    assert_eq!(encoded.get("complete"), Some(&Value::Null));
    assert_eq!(
        encoded["provenance"].get("source_revision"),
        Some(&Value::Null)
    );
    assert_eq!(
        serde_json::from_value::<DataSchemaDocument>(encoded).unwrap(),
        document
    );
}

#[test]
fn empty_fields_are_valid_but_do_not_establish_completeness() {
    for native in [
        json!({"fields": []}),
        json!({"type": "struct", "fields": [], "metadata": {"unknown": 1}}),
    ] {
        let document = DataSchemaDocument::from_native_schema(native.clone()).unwrap();
        assert!(document.columns.is_empty());
        assert_eq!(document.native_schema, native);
        assert_eq!(document.complete, None);
        assert!(!document.provenance.snapshot_consistency_established);
    }
}

#[test]
fn native_type_names_are_not_semantically_validated_or_translated() {
    let native = json!({
        "fields": [
            {"name": "primitive", "type": "future-native-type(9)"},
            {"name": "complex", "type": {"type": "future-complex", "opaque": [1, null]}}
        ]
    });
    let document = DataSchemaDocument::from_native_schema(native.clone()).unwrap();
    assert_eq!(document.columns[0].native_type, "future-native-type(9)");
    assert_eq!(document.columns[1].native_type, native["fields"][1]["type"]);
    assert!(!document.coverage.native_semantics_validated);
}

#[test]
fn malformed_schema_fields_names_types_and_nullability_are_invalid_responses() {
    for native in [
        Value::Null,
        json!(false),
        json!(1),
        json!("schema"),
        json!([]),
        json!({}),
    ] {
        assert_invalid_response(DataSchemaDocument::from_native_schema(native));
    }
    for fields in [
        Value::Null,
        json!(false),
        json!(1),
        json!("fields"),
        json!({}),
    ] {
        assert_invalid_response(DataSchemaDocument::from_native_schema(
            json!({"fields": fields}),
        ));
    }
    for root_type in [
        Value::Null,
        json!(false),
        json!(1),
        json!([]),
        json!("array"),
    ] {
        assert_invalid_response(DataSchemaDocument::from_native_schema(
            json!({"type": root_type, "fields": []}),
        ));
    }
    for field in [
        Value::Null,
        json!(false),
        json!(1),
        json!("column"),
        json!([]),
        json!({}),
        json!({"name": "column"}),
        json!({"type": "string"}),
    ] {
        assert_invalid_response(DataSchemaDocument::from_native_schema(
            json!({"fields": [field]}),
        ));
    }
    for name in [
        Value::Null,
        json!(false),
        json!(1),
        json!([]),
        json!({}),
        json!(""),
    ] {
        assert_invalid_response(DataSchemaDocument::from_native_schema(
            json!({"fields": [{"name": name, "type": "string"}]}),
        ));
    }
    for native_type in [
        Value::Null,
        json!(false),
        json!(1),
        json!([]),
        json!(""),
        json!({}),
        json!({"type": null}),
        json!({"type": false}),
        json!({"type": 1}),
        json!({"type": []}),
        json!({"type": {}}),
        json!({"type": ""}),
    ] {
        assert_invalid_response(DataSchemaDocument::from_native_schema(
            json!({"fields": [{"name": "column", "type": native_type}]}),
        ));
    }
    for nullable in [json!("false"), json!(0), json!([]), json!({})] {
        assert_invalid_response(DataSchemaDocument::from_native_schema(
            json!({"fields": [{"name": "column", "type": "string", "nullable": nullable}]}),
        ));
    }
    assert_invalid_response(DataSchemaDocument::from_native_schema(json!({
        "fields": [
            {"name": "valid", "type": "string"},
            {"name": "invalid", "type": null}
        ]
    })));
}

#[test]
fn deserialization_rejects_unrecognized_envelope_versions_and_discriminators() {
    let encoded = serde_json::to_value(
        DataSchemaDocument::from_native_schema(json!({"fields": []})).unwrap(),
    )
    .unwrap();
    for version in [json!(0), json!(2), json!("1"), json!(null)] {
        let mut invalid = encoded.clone();
        invalid["schema_version"] = version;
        assert!(serde_json::from_value::<DataSchemaDocument>(invalid).is_err());
    }
    for (field, value) in [
        ("schema_kind", json!("operation-input")),
        ("backend", json!("sql")),
    ] {
        let mut invalid = encoded.clone();
        invalid[field] = value;
        assert!(serde_json::from_value::<DataSchemaDocument>(invalid).is_err());
    }
}

#[tokio::test]
async fn reader_uses_all_onelake_pages_and_newest_metadata_without_extra_requests() {
    let native = json!({
        "type": "struct",
        "fields": [{"name": "new", "type": "long", "nullable": false}],
        "unknown": {"preserved": true}
    });
    let network = ScriptedHttp::new([
        response(200, listing(), &[("x-ms-continuation", "next+/=")]),
        response(
            200,
            r#"{"paths":[{"name":"item/Tables/sales/_delta_log/00002.json"}]}"#,
            &[],
        ),
        response(
            200,
            &format!(
                "{}\n{}\n",
                json!({"add": {"path": "part.parquet"}}),
                metadata(&native.to_string())
            ),
            &[],
        ),
    ]);
    let storage = storage(network.clone());
    let executor = TableWithOneLake {
        tables: &NoLegacyExecution,
        storage: &storage,
    };
    let credential = Arc::new(Credential::default());
    let document = client(credential.clone())
        .schema_document(&executor, &table(), TableLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(
        document,
        DataSchemaDocument::from_native_schema(native).unwrap()
    );
    let requests = network.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request.method() == Method::Get)
    );
    assert_eq!(
        requests[1]
            .url()
            .query_pairs()
            .find(|(key, _)| key == "continuationToken")
            .unwrap()
            .1,
        "next+/="
    );
    assert_eq!(
        requests[2].url().path(),
        "/ws/item/Tables/sales/_delta_log/00002.json"
    );
    assert_eq!(
        *credential.0.lock().unwrap(),
        [Audience::OneLake.scope(); 3]
    );
}

#[tokio::test]
async fn reader_accepts_empty_fields_but_rejects_malformed_inner_json_or_shapes() {
    for (schema, valid) in [
        (r#"{"fields":[]}"#, true),
        ("not JSON", false),
        ("null", false),
        ("[]", false),
        ("{}", false),
        (r#"{"fields":null}"#, false),
        (r#"{"fields":[{"name":"x","type":null}]}"#, false),
        (
            r#"{"fields":[{"name":"x","type":"long","nullable":0}]}"#,
            false,
        ),
    ] {
        let network = ScriptedHttp::new([
            response(200, listing(), &[]),
            response(200, &metadata(schema), &[]),
        ]);
        let storage = storage(network.clone());
        let executor = TableWithOneLake {
            tables: &NoLegacyExecution,
            storage: &storage,
        };
        let result = client(Arc::new(Credential::default()))
            .schema_document(&executor, &table(), TableLimits::default(), pending())
            .await;
        if valid {
            let document = result.unwrap();
            assert!(document.columns.is_empty());
            assert_eq!(document.complete, None);
        } else {
            assert_invalid_response(result);
        }
        assert_eq!(network.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn reader_rejects_oversized_positive_and_negative_integer_metadata() {
    for integer in [
        "18446744073709551616",
        "18446744073709551617",
        "-9223372036854775809",
        "-18446744073709551617",
    ] {
        for template in [
            r#"{"fields":[],"metadata":{"integer":NUMBER}}"#,
            r#"{"fields":[{"name":"x","type":"long","metadata":{"integer":NUMBER}}]}"#,
            r#"{"fields":[{"name":"x","type":{"type":"future","metadata":{"integer":NUMBER}}}]}"#,
        ] {
            let schema = template.replace("NUMBER", integer);
            let network = ScriptedHttp::new([
                response(200, listing(), &[]),
                response(200, &metadata(&schema), &[]),
            ]);
            let storage = storage(network.clone());
            let executor = TableWithOneLake {
                tables: &NoLegacyExecution,
                storage: &storage,
            };
            let result = client(Arc::new(Credential::default()))
                .schema_document(&executor, &table(), TableLimits::default(), pending())
                .await;
            assert_invalid_response(result);
            assert_eq!(network.requests.lock().unwrap().len(), 2);
        }
    }
}

#[tokio::test]
async fn reader_preserves_integer_limits_floats_and_numeric_looking_strings() {
    let schema = r#"{
        "fields": [{
            "name": "x",
            "type": "long",
            "metadata": {
                "i64_min": -9223372036854775808,
                "i64_max": 9223372036854775807,
                "u64_max": 18446744073709551615,
                "zero": 0,
                "fraction": 0.1,
                "negative_fraction": -1.5,
                "exponent": 1e20,
                "negative_exponent": 1E-20,
                "large_float": 18446744073709551617.0,
                "negative_zero": -0.0,
                "positive_string": "18446744073709551617",
                "negative_string": "-9223372036854775809",
                "quoted_string": "\"18446744073709551617\", -9223372036854775809",
                "backslash_string": "\\18446744073709551617",
                "18446744073709551617": "numeric-looking key"
            }
        }]
    }"#;
    let network = ScriptedHttp::new([
        response(200, listing(), &[]),
        response(200, &metadata(schema), &[]),
    ]);
    let storage = storage(network.clone());
    let executor = TableWithOneLake {
        tables: &NoLegacyExecution,
        storage: &storage,
    };
    let document = client(Arc::new(Credential::default()))
        .schema_document(&executor, &table(), TableLimits::default(), pending())
        .await
        .unwrap();
    let metadata = &document.native_schema["fields"][0]["metadata"];
    assert_eq!(metadata["i64_min"].as_i64(), Some(i64::MIN));
    assert_eq!(metadata["i64_max"].as_i64(), Some(i64::MAX));
    assert_eq!(metadata["u64_max"].as_u64(), Some(u64::MAX));
    assert_eq!(metadata["zero"].as_u64(), Some(0));
    for (name, expected) in [
        ("fraction", 0.1),
        ("negative_fraction", -1.5),
        ("exponent", 1e20),
        ("negative_exponent", 1e-20),
        ("large_float", 2.0_f64.powi(64)),
    ] {
        assert!(metadata[name].as_number().unwrap().is_f64());
        assert_eq!(metadata[name].as_f64(), Some(expected));
    }
    assert!(
        metadata["negative_zero"]
            .as_f64()
            .unwrap()
            .is_sign_negative()
    );
    assert_eq!(metadata["positive_string"], "18446744073709551617");
    assert_eq!(metadata["negative_string"], "-9223372036854775809");
    assert_eq!(
        metadata["quoted_string"],
        "\"18446744073709551617\", -9223372036854775809"
    );
    assert_eq!(metadata["backslash_string"], "\\18446744073709551617");
    assert_eq!(metadata["18446744073709551617"], "numeric-looking key");
    assert_eq!(
        serde_json::to_value(&document).unwrap()["native_schema"]["fields"][0]["metadata"]
            ["u64_max"]
            .as_u64(),
        Some(u64::MAX)
    );
    assert_eq!(network.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn pre_cancelled_reader_makes_no_http_or_credential_calls() {
    let network = ScriptedHttp::new([]);
    let storage = storage(network.clone());
    let executor = TableWithOneLake {
        tables: &NoLegacyExecution,
        storage: &storage,
    };
    let credential = Arc::new(Credential::default());
    let error = client(credential.clone())
        .schema_document(&executor, &table(), TableLimits::default(), ready(()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, TableErrorKind::Cancelled);
    assert!(!error.mutation_may_have_occurred);
    assert!(network.requests.lock().unwrap().is_empty());
    assert!(credential.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn later_page_denial_and_repeated_continuation_never_yield_partial_success() {
    for denied in [false, true] {
        let second = if denied {
            response(403, "", &[("x-ms-error-code", "AuthorizationFailure")])
        } else {
            response(200, r#"{"paths":[]}"#, &[("x-ms-continuation", "next")])
        };
        let network = ScriptedHttp::new([
            response(200, listing(), &[("x-ms-continuation", "next")]),
            second,
        ]);
        let storage = storage(network.clone());
        let executor = TableWithOneLake {
            tables: &NoLegacyExecution,
            storage: &storage,
        };
        let error = client(Arc::new(Credential::default()))
            .schema_document(&executor, &table(), TableLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            TableErrorKind::OneLake(Box::new(if denied {
                OneLakeError::Http {
                    status: 403,
                    code: Some("AuthorizationFailure".into()),
                    raw_service_error: None,
                }
            } else {
                OneLakeError::RepeatedContinuation
            }))
        );
        assert!(!error.mutation_may_have_occurred);
        assert_eq!(network.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn request_log_and_byte_limits_remain_errors_not_empty_documents() {
    let two_logs = r#"{"paths":[{"name":"item/Tables/sales/_delta_log/00001.json"},{"name":"item/Tables/sales/_delta_log/00002.json"}]}"#;
    for (body, limits, expected) in [
        (
            listing(),
            TableLimits {
                max_requests: NonZeroUsize::new(1).unwrap(),
                ..TableLimits::default()
            },
            TableErrorKind::Limit,
        ),
        (
            two_logs,
            TableLimits {
                max_logs: NonZeroUsize::new(1).unwrap(),
                ..TableLimits::default()
            },
            TableErrorKind::OneLake(Box::new(OneLakeError::Limit)),
        ),
        (
            listing(),
            TableLimits {
                max_response_bytes: NonZeroUsize::new(1).unwrap(),
                ..TableLimits::default()
            },
            TableErrorKind::OneLake(Box::new(OneLakeError::Execution(
                OneLakeExecutionError::ResponseLimit,
            ))),
        ),
    ] {
        let network = ScriptedHttp::new([response(200, body, &[])]);
        let storage = storage(network.clone());
        let executor = TableWithOneLake {
            tables: &NoLegacyExecution,
            storage: &storage,
        };
        let error = client(Arc::new(Credential::default()))
            .schema_document(&executor, &table(), limits, pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(!error.mutation_may_have_occurred);
        assert_eq!(network.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn checkpoint_only_or_missing_metadata_is_not_a_valid_empty_schema() {
    for (listing, log) in [
        (
            r#"{"paths":[{"name":"item/Tables/sales/_delta_log/00001.checkpoint.parquet"}]}"#,
            None,
        ),
        (r#"{"paths":[]}"#, None),
        (listing(), Some(r#"{"add":{"path":"part.parquet"}}"#)),
    ] {
        let mut replies = vec![response(200, listing, &[])];
        if let Some(log) = log {
            replies.push(response(200, log, &[]));
        }
        let count = replies.len();
        let network = ScriptedHttp::new(replies);
        let storage = storage(network.clone());
        let executor = TableWithOneLake {
            tables: &NoLegacyExecution,
            storage: &storage,
        };
        let error = client(Arc::new(Credential::default()))
            .schema_document(&executor, &table(), TableLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, TableErrorKind::InvalidDeltaTable);
        assert!(!error.mutation_may_have_occurred);
        assert_eq!(network.requests.lock().unwrap().len(), count);
    }
}
