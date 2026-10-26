// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fabric_cli_resources::{
    HiddenNameKind, find_hidden_name, read_hidden_metadata, resolve_hidden_name,
};
use fabric_sdk_catalog::HiddenResource;

#[tokio::test]
async fn original_hidden_get_handlers_match_metadata_queries_request_order_and_suppressed_diagnostics()
 {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/catalog/bindings_cases.json"
    ))
    .unwrap();
    for case in fixtures["metadata"].as_array().unwrap() {
        let replies = case["replies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|reply_value| {
                reply(
                    reply_value["status"].as_u64().unwrap() as u16,
                    &reply_value["body"].to_string(),
                    &[],
                )
            })
            .collect();
        let (api, wire, _) = setup(replies);
        let resource = match case["resource"].as_str().unwrap() {
            "domain" => HiddenResource::Domain("resource".into()),
            "connection" => HiddenResource::Connection("resource".into()),
            "gateway" => HiddenResource::Gateway("resource".into()),
            _ => panic!("unknown fixture"),
        };
        let result = read_hidden_metadata(
            &CatalogClient::new(api),
            &resource,
            case["query"].as_str(),
            case["verbose"].as_bool().unwrap(),
            Limits::default(),
            pending(),
        )
        .await;
        let expected = &case["expected"];
        if expected["error"].is_null() {
            let result = result.unwrap_or_else(|error| panic!("{}: {error}", case["name"]));
            assert_eq!(result.metadata.value, expected["value"], "{}", case["name"]);
            assert_eq!(
                result.output.data.as_ref().unwrap_or(&Value::Null),
                &expected["data"],
                "{}",
                case["name"]
            );
            assert_eq!(
                json!(result.output.warnings),
                expected["warnings"],
                "{}",
                case["name"]
            );
            if case["name"] == "membership-denied" || case["name"] == "membership-malformed" {
                assert_eq!(result.metadata.omissions.len(), 1);
                assert_eq!(result.metadata.omissions[0].field, "domainWorkspaces");
                assert_eq!(result.metadata.omissions[0].failure.partial.pages, 0);
            } else {
                assert!(result.metadata.omissions.is_empty());
            }
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
            assert_eq!(json!(error.warnings), expected["warnings"]);
        }
        let actual:Vec<_>=wire.requests.lock().unwrap().iter().map(|request|
            json!({"method":request.method().to_string(),"url":request.url().as_str()})).collect();
        assert_eq!(json!(actual), expected["requests"], "{}", case["name"]);
    }
}

#[tokio::test]
async fn source_exact_name_first_match_and_complete_paged_domain_resolution_share_the_existing_read_routes()
 {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/catalog/bindings_cases.json"
    ))
    .unwrap();
    for case in fixtures["names"].as_array().unwrap() {
        let kind = match case["kind"].as_str().unwrap() {
            "Domain" => HiddenNameKind::Domain,
            "Connection" => HiddenNameKind::Connection,
            "Gateway" => HiddenNameKind::Gateway,
            _ => panic!("unknown kind"),
        };
        let result = find_hidden_name(
            case["rows"].as_array().unwrap(),
            kind,
            case["name"].as_str().unwrap(),
        );
        if case["error"].is_null() {
            assert_eq!(result.unwrap(), case["id"].as_str().unwrap());
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code, case["error"]["code"].as_str().unwrap());
            assert_eq!(error.message, case["error"]["message"].as_str().unwrap());
        }
    }
    let (api, wire, _) = setup(vec![
        reply(
            200,
            r#"{"domains":[{"id":"first","displayName":"Other"}],"continuationToken":"next"}"#,
            &[],
        ),
        reply(
            200,
            r#"{"domains":[{"id":"parent","displayName":"Parent","parentDomainId":"root"}]}"#,
            &[],
        ),
    ]);
    let id = resolve_hidden_name(
        &CatalogClient::new(api),
        HiddenNameKind::Domain,
        "/Parent.Domain/",
        Limits::default(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(id, "parent");
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url().path(), "/v1/admin/domains");
    assert_eq!(requests[1].url().query(), Some("continuationToken=next"));
}

#[tokio::test]
async fn gateway_specific_supported_types_keep_query_on_every_page_and_original_field_byte_bound() {
    let body = r#"{"value":[{"type":"SQL","supportedCredentialTypes":["Basic"]}],"continuationToken":" *~"}"#;
    let (api, wire, _) = setup(vec![
        reply(200, body, &[]),
        reply(200, r#"{"value":[]}"#, &[]),
    ]);
    let result = CatalogClient::new(api)
        .supported_connection_types(
            Some("gateway +/"),
            Limits {
                max_response_bytes: NonZeroUsize::new(body.len()).unwrap(),
                ..Limits::default()
            },
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(
        requests[0].url().path(),
        "/v1/connections/supportedConnectionTypes"
    );
    assert_eq!(requests[0].url().query(), Some("gatewayId=gateway+%2B%2F"));
    assert_eq!(
        requests[1].url().query(),
        Some("gatewayId=gateway+%2B%2F&continuationToken=+%2A~")
    );
}

#[test]
fn domain_id_to_name_preserves_first_match_and_missing_defaults_without_ignoring_malformed_rows() {
    let rows = json!([{"id":"same","displayName":"First"},{"id":"same","displayName":"Second"}]);
    let rows = rows.as_array().unwrap();
    assert_eq!(
        fabric_cli_resources::domain_name_by_id(rows, Some("same")).unwrap(),
        "First"
    );
    assert_eq!(
        fabric_cli_resources::domain_name_by_id(rows, Some("missing")).unwrap(),
        ""
    );
    assert_eq!(
        fabric_cli_resources::domain_name_by_id(rows, None).unwrap(),
        ""
    );
    assert_eq!(
        fabric_cli_resources::domain_name_by_id(&[json!({})], Some("same"))
            .unwrap_err()
            .code,
        "InvalidEntriesFormat"
    );
}

#[tokio::test]
async fn domain_enrichment_does_not_swallow_shared_safety_limits_or_primary_failure() {
    let primary = r#"{"id":"resource","displayName":"Domain"}"#;
    let (api, wire, _) = setup(vec![
        reply(200, primary, &[]),
        reply(
            200,
            r#"{"value":[{"id":"a"}],"continuationToken":"next"}"#,
            &[],
        ),
    ]);
    let error = CatalogClient::new(api)
        .get_metadata(
            &HiddenResource::Domain("resource".into()),
            Limits {
                max_pages: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error, ApiError::Limit);
    assert_eq!(wire.requests.lock().unwrap().len(), 2);
    let (api, wire, credential) = setup(Vec::new());
    let error = CatalogClient::new(api)
        .get_metadata(
            &HiddenResource::Domain("resource".into()),
            Limits::default(),
            ready(()),
        )
        .await
        .unwrap_err();
    assert_eq!(error, ApiError::Cancelled);
    assert!(wire.requests.lock().unwrap().is_empty());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
}
