// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Metadata-only import seam, using actual selected SDK/HTTP and source receipts.

use super::*;

async fn validate(
    host: &Harness,
    input: &str,
    cancelled: impl std::future::Future<Output = ()> + Send,
) -> Result<(), CommandError> {
    let endpoint = fabric_sdk::EndpointBinding::with_host(
        fabric_sdk::ServiceOrigin::Fabric,
        "msitapi.fabric.microsoft.com",
    )
    .unwrap();
    let sdk = fabric_sdk::Client::with_context(fabric_sdk::RequestContext::with_endpoint(
        host.credential.clone(),
        fabric_sdk::CredentialSelection::new(
            "offline-account",
            "tenant",
            fabric_sdk::Audience::Fabric,
        )
        .unwrap(),
        endpoint.clone(),
    ));
    let http =
        fabric_sdk_http::HttpExecutor::new(host.http.clone(), Arc::new(Timer), Default::default())
            .unwrap()
            .with_endpoint(endpoint);
    let reads = fabric_cli::session::ReadSession {
        sdk: &sdk,
        executor: &http,
        tenant: fabric_cli_context::Element::tenant("Tenant", "tenant"),
        current_path: "/",
        home: fabric_cli_context::HomeContext::Unspecified,
        timestamp: fabric_cli_output::Timestamp::parse("2026-10-28T13:10:00Z").unwrap(),
        config: Default::default(),
    };
    let result =
        fabric_cli::families::validate_local_import_input(&reads, &http, input, cancelled).await;
    assert!(host.http.replies.lock().unwrap().is_empty());
    result
}

#[tokio::test]
async fn actual_source_import_input_classification_uses_metadata_only_and_selected_origins() {
    let receipt: Value = serde_json::from_str(include_str!(
        "../../../fabric-deploy/tests/definition_transfer_input_source.json"
    ))
    .unwrap();
    for case in receipt["cases"].as_array().unwrap() {
        let host = host();
        let name = case["name"].as_str().unwrap();
        if name == "fabric-folder" {
            host.reply(
                200,
                json!({"value":[{"id":WS,"displayName":"Demo","type":"Workspace"}]}),
            );
            host.reply(
                200,
                json!({"value":[{"id":NOTEBOOK,"displayName":"Parent"}]}),
            );
        } else if name.starts_with("lakehouse-") || name.starts_with("warehouse-") {
            let kind = if name.starts_with("warehouse-") {
                "Warehouse"
            } else {
                "Lakehouse"
            };
            host.metadata_with_id(WS, json!([{"id":LAKE,"displayName":"Lake","type":kind}]));
            if name.ends_with("-child") {
                host.headers(200, Value::Null, &[("x-ms-resource-type", "directory")]);
            }
        }
        let result = validate(&host, case["input"].as_str().unwrap(), pending()).await;
        if case["error"].is_null() {
            assert!(result.is_ok(), "{}", case["name"]);
            assert!(
                host.requests().is_empty(),
                "local existence/kind belongs to the file worker"
            );
        } else {
            let error = result.unwrap_err().envelope(
                fabric_cli_output::Timestamp::parse("2026-10-28T13:10:00Z").unwrap(),
                None,
            );
            assert_eq!(error.result().error_code(), case["error"]["code"].as_str());
            assert_eq!(error.result().message(), case["error"]["message"].as_str());
        }
        for request in host.requests() {
            let url = request["url"].as_str().unwrap();
            if request["method"] == "HEAD" {
                assert!(url.starts_with("https://onelake.dfs.fabric.microsoft.com/"));
            } else {
                assert_eq!(request["method"], "GET");
                assert!(url.starts_with("https://msitapi.fabric.microsoft.com/"));
            }
        }
        assert!(
            fs::read_dir(host.root.join("output"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}

#[tokio::test]
async fn import_input_auth_failure_cannot_become_local_candidate_or_download() {
    let host = host();
    host.metadata_with_id(
        WS,
        json!([{"id":LAKE,"displayName":"Lake","type":"Lakehouse"}]),
    );
    host.reply(403, Value::Null);
    assert!(
        validate(
            &host,
            "Demo.Workspace/Lake.Lakehouse/Files/input",
            pending()
        )
        .await
        .is_err()
    );
    assert_eq!(host.requests().len(), 3);
    assert_eq!(host.requests()[2]["method"], "HEAD");
    assert!(host.requests().iter().all(|request| {
        request["method"] == "HEAD"
            || request["url"]
                .as_str()
                .unwrap()
                .starts_with("https://msitapi.fabric.microsoft.com/")
    }));
}

#[tokio::test]
async fn import_input_cancellation_does_not_acquire_token_or_probe_files() {
    let host = host();
    assert!(
        validate(&host, "input", std::future::ready(()))
            .await
            .is_err()
    );
    assert!(host.requests().is_empty());
    assert!(host.credential.0.lock().unwrap().is_empty());
}
