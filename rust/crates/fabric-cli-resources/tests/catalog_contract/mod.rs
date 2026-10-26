// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod bindings;
mod interactive;
mod metadata;
mod query_dispatch;
use super::*;
use fabric_cli_resources::{FindOptions, find};
use fabric_sdk_catalog::{CatalogClient, HiddenCollection};

#[tokio::test]
async fn actual_catalog_and_find_consumer_match_independent_python_filters_paging_projection_and_errors()
 {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/catalog/cases.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let replies = case["responses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                reply(
                    r["status"].as_u64().unwrap() as u16,
                    &r["body"].to_string(),
                    &[],
                )
            })
            .collect();
        let (api, wire, _) = setup(replies);
        let result = find(
            &CatalogClient::new(api),
            &FindOptions {
                search: "needle".into(),
                params: case["params"].as_str().map(str::to_owned),
                long: case["long"].as_bool().unwrap_or(false),
                query: case["query"].as_str().map(str::to_owned),
            },
            Limits::default(),
            pending(),
        )
        .await;
        let expected = &case["expected"];
        if expected["error"].is_null() {
            let result = result.unwrap_or_else(|e| panic!("{}: {e}", case["name"]));
            assert_eq!(
                result.data.as_ref().unwrap_or(&Value::Null),
                &expected["data"],
                "{}",
                case["name"]
            );
            assert_eq!(
                json!(result.diagnostics),
                expected["diagnostics"],
                "{}",
                case["name"]
            );
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                error.code,
                expected["error"]["code"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                error.message,
                expected["error"]["message"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                json!(error.diagnostics),
                expected["diagnostics"],
                "{}",
                case["name"]
            );
        }
        let actual:Vec<_>=wire.requests.lock().unwrap().iter().map(|r|json!({
            "method":r.method().to_string(),"url":r.url().as_str(),
            "body":serde_json::from_slice::<Value>(&azure_core::Bytes::from(r.body())).unwrap()
        })).collect();
        assert_eq!(json!(actual), expected["requests"], "{}", case["name"]);
    }
}

#[tokio::test]
async fn hidden_catalog_routes_use_real_collection_fields_and_shared_executor() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/catalog/cases.json"
    ))
    .unwrap();
    for case in fixture["hidden"].as_array().unwrap() {
        let collection = match case["name"].as_str().unwrap() {
            "connections" => HiddenCollection::Connections,
            "gateways" => HiddenCollection::Gateways,
            "domains" => HiddenCollection::Domains,
            "connection-types" => HiddenCollection::ConnectionTypes,
            "spark-pools" => HiddenCollection::SparkPools("workspace".into()),
            "private-endpoints" => HiddenCollection::ManagedPrivateEndpoints("workspace".into()),
            "shares" => HiddenCollection::ItemExternalDataShares {
                workspace: "workspace".into(),
                item: "item".into(),
            },
            "domain-workspaces" => HiddenCollection::DomainWorkspaces("domain".into()),
            _ => panic!("unknown fixture"),
        };
        let field = if matches!(collection, HiddenCollection::Domains) {
            "domains"
        } else {
            "value"
        };
        let (api, wire, _) = setup(vec![reply(
            200,
            &json!({field:[{"id":"row"}]}).to_string(),
            &[],
        )]);
        let listed = CatalogClient::new(api)
            .list_hidden(&collection, Limits::default(), pending())
            .await
            .unwrap();
        assert_eq!(
            listed.rows,
            json!([{"id":"row"}]).as_array().unwrap().clone()
        );
        let request = &wire.requests.lock().unwrap()[0];
        assert_eq!(
            json!({"method":request.method().to_string(),"url":request.url().as_str()}),
            case["request"]
        );
    }
}
