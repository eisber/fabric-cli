// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use azure_core::http::Url;
use fabric_sdk_query::{
    EngineBinding, QueryCapability, QueryColumn, QueryDataset, QueryEvidence, QueryResult,
};
use fabric_v2_contracts::ItemIdentity;
use serde_json::{Value, json};

/// Constructs only synthetic, offline native execution evidence.
pub fn result(columns: &[(&str, &str)], rows: Vec<Vec<Value>>) -> QueryResult {
    QueryResult {
        datasets: vec![QueryDataset {
            ordinal: 0,
            table_id: 17,
            name: "primary".into(),
            columns: columns
                .iter()
                .map(|(name, native_type)| QueryColumn {
                    name: (*name).into(),
                    native_type: (*native_type).into(),
                })
                .collect(),
            rows,
        }],
        metadata: vec![json!({"service":"synthetic-offline"})],
        diagnostics: vec![],
        completion: json!({
            "FrameType":"DataSetCompletion", "HasErrors":false,
            "Cancelled":false, "OneApiErrors":[]
        }),
        evidence: QueryEvidence {
            binding: EngineBinding::kusto(
                "offline-kusto",
                "1",
                ItemIdentity::new(
                    "11111111-1111-1111-1111-111111111111",
                    "22222222-2222-2222-2222-222222222222",
                )
                .unwrap(),
                Url::parse("https://offline.kusto.fabric.microsoft.com/").unwrap(),
                "offline-database",
                "https://api.kusto.windows.net/.default",
                "offline-account",
                "offline-tenant",
            )
            .unwrap(),
            language: "kql",
            transport: "offline-fixture",
            protocol_version: "v2.0",
            read_policy: "offline-fixture",
            adapter_capability: QueryCapability::ScriptedOffline,
            production_verified: false,
            http_eof: true,
            http_status: Some(200),
            content_type: Some("application/json".into()),
            client_request_id: "offline-request".into(),
            activity_id: Some("offline-activity".into()),
            completed: true,
            locally_truncated: false,
        },
    }
}
