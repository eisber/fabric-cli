// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fabric_sdk_catalog::HiddenResource;

#[tokio::test]
async fn every_hidden_singleton_route_uses_actual_sdk_http_metadata_not_collection_fallback() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/catalog/metadata_cases.json"
    ))
    .unwrap();
    for case in fixtures["cases"].as_array().unwrap() {
        let target = match case["name"].as_str().unwrap() {
            "connection" => HiddenResource::Connection("resource".into()),
            "gateway" => HiddenResource::Gateway("resource".into()),
            "domain" => HiddenResource::Domain("resource".into()),
            "spark-pool" => HiddenResource::SparkPool {
                workspace: "workspace".into(),
                id: "resource".into(),
            },
            "private-endpoint" => HiddenResource::ManagedPrivateEndpoint {
                workspace: "workspace".into(),
                id: "resource".into(),
            },
            "share" => HiddenResource::ExternalDataShare {
                workspace: "workspace".into(),
                item: "item".into(),
                id: "resource".into(),
            },
            _ => panic!("fixture kind"),
        };
        let (api, wire, _) = setup(vec![reply(
            200,
            r#"{"id":"resource","metadata":{"opaque":true}}"#,
            &[],
        )]);
        let result = CatalogClient::new(api)
            .get_hidden(&target, NonZeroUsize::new(4096).unwrap(), pending())
            .await
            .unwrap();
        assert_eq!(result, json!({"id":"resource","metadata":{"opaque":true}}));
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            json!({"method":requests[0].method().to_string(),"url":requests[0].url().as_str()}),
            case["request"]
        );
    }
}
