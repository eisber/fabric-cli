// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

#[path = "../../../../engine/data-plane/aihub-query/examples/support/mod.rs"]
mod support;

use super::*;
use fabric_sdk::{CredentialSelection, RequestContext};
use serde_json::json;
use std::sync::{Arc, atomic::Ordering};

const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const AGENT: &str = "22222222-2222-4222-8222-222222222222";
const DATASOURCE: &str = "33333333-3333-4333-8333-333333333333";

fn settings() -> Map<String, Value> {
    let value = json!({"query_connection":{
        "protocol":"mcp",
        "service_binding":"dataagent-typed-query",
        "endpoint":"https://api.fabric.microsoft.com/caller-selected-mcp",
        "audience_scope":"https://api.fabric.microsoft.com/.default",
        "data_agents":[{
            "workspace_id":WORKSPACE,"agent_id":AGENT,"datasource_id":DATASOURCE,
            "stage":"published"
        }]
    }});
    value.as_object().unwrap().clone()
}

fn trusted() -> EndpointBinding {
    EndpointBinding::public(ServiceOrigin::Fabric)
}

fn owner() -> Artifact {
    Artifact {
        workspace: WORKSPACE.into(),
        item: AGENT.into(),
    }
}

#[test]
fn absent_connection_remains_absent_without_default_endpoint_or_settings_write() {
    assert!(
        QueryConnection::from_settings(None, &trusted())
            .unwrap()
            .is_none()
    );
    let settings = json!({"output_format":"json"}).as_object().unwrap().clone();
    let before = settings.clone();
    assert!(
        QueryConnection::from_settings(Some(&settings), &trusted())
            .unwrap()
            .is_none()
    );
    assert_eq!(settings, before);
}

#[test]
fn exact_trusted_endpoint_scope_and_selected_identity_are_retained_without_tokens() {
    let settings = settings();
    let before = settings.clone();
    let connection = QueryConnection::from_settings(Some(&settings), &trusted())
        .unwrap()
        .unwrap();
    let credential = Arc::new(support::Credential::default());
    let context = RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("selected-account", "selected-tenant", Audience::Fabric).unwrap(),
        trusted(),
    );
    let binding = connection.auth_binding(&context).unwrap();
    assert_eq!(
        binding.endpoint().as_str(),
        "https://api.fabric.microsoft.com/caller-selected-mcp"
    );
    assert_eq!(binding.scope(), Audience::Fabric.scope());
    assert_eq!(binding.account(), "selected-account");
    assert_eq!(binding.tenant(), "selected-tenant");
    assert_eq!(connection.agent(&owner()).unwrap().datasource(), DATASOURCE);
    assert_eq!(
        connection.agent(&owner()).unwrap().stage(),
        Stage::Production
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert_eq!(settings, before);
}

#[test]
fn arbitrary_https_or_unselected_service_origins_are_rejected_before_credentials() {
    for endpoint in [
        "https://untrusted.example.test/mcp",
        "https://api.fabric.microsoft.com.evil.example/mcp",
        "https://dailyapi.fabric.microsoft.com/mcp",
        "https://api.powerbi.com/mcp",
        "http://api.fabric.microsoft.com/mcp",
        "https://user@api.fabric.microsoft.com/mcp",
        "https://api.fabric.microsoft.com:8443/mcp",
        "https://api.fabric.microsoft.com/mcp?token=private",
        "https://api.fabric.microsoft.com/mcp#fragment",
        "https://api.fabric.microsoft.com/%2e/mcp",
        "https://api.fabric.microsoft.com\\mcp",
        " https://api.fabric.microsoft.com/mcp",
    ] {
        let mut settings = settings();
        settings["query_connection"]["endpoint"] = json!(endpoint);
        assert!(QueryConnection::from_settings(Some(&settings), &trusted()).is_err());
    }
}

#[test]
fn arbitrary_audiences_and_wrong_identity_audience_cannot_use_selected_credential() {
    for scope in [
        "https://other.example.test/.default",
        "https://management.azure.com/.default",
        "https://analysis.windows.net/powerbi/api/.default",
        "https://api.fabric.microsoft.com/user_impersonation",
    ] {
        let mut settings = settings();
        settings["query_connection"]["audience_scope"] = json!(scope);
        assert_eq!(
            QueryConnection::from_settings(Some(&settings), &trusted())
                .err()
                .unwrap(),
            QueryConnectionError::UnsupportedAudience
        );
    }
    let connection = QueryConnection::from_settings(Some(&settings()), &trusted())
        .unwrap()
        .unwrap();
    let credential = Arc::new(support::Credential::default());
    let context = RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("selected-account", "selected-tenant", Audience::Arm).unwrap(),
        trusted(),
    );
    assert_eq!(
        connection.auth_binding(&context).err().unwrap(),
        QueryConnectionError::UnsupportedAudience
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn unknown_fields_and_secret_or_identity_overrides_are_not_ignored() {
    for field in [
        "token",
        "client_secret",
        "account",
        "tenant",
        "allow_unverified",
        "engine",
    ] {
        let mut settings = settings();
        settings["query_connection"][field] = json!("private-sentinel");
        let error = QueryConnection::from_settings(Some(&settings), &trusted())
            .err()
            .unwrap();
        assert_eq!(error, QueryConnectionError::Invalid);
        assert!(!error.to_string().contains("private-sentinel"));
    }
}

#[test]
fn datasource_stage_is_explicit_and_duplicate_agent_choices_fail_closed() {
    let mut configured = settings();
    configured["query_connection"]["data_agents"][0]["stage"] = json!("staging");
    let connection = QueryConnection::from_settings(Some(&configured), &trusted())
        .unwrap()
        .unwrap();
    assert_eq!(connection.agent(&owner()).unwrap().stage(), Stage::Sandbox);
    for stage in [json!("draft"), json!("unknown"), Value::Null] {
        let mut configured = settings();
        configured["query_connection"]["data_agents"][0]["stage"] = stage;
        assert!(QueryConnection::from_settings(Some(&configured), &trusted()).is_err());
    }
    let mut configured = settings();
    let duplicate = configured["query_connection"]["data_agents"][0].clone();
    configured["query_connection"]["data_agents"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert_eq!(
        QueryConnection::from_settings(Some(&configured), &trusted())
            .err()
            .unwrap(),
        QueryConnectionError::AmbiguousAgent
    );
}

#[test]
fn no_implicit_first_agent_or_binding_after_selected_context_changes() {
    let connection = QueryConnection::from_settings(Some(&settings()), &trusted())
        .unwrap()
        .unwrap();
    let different = Artifact {
        workspace: WORKSPACE.into(),
        item: DATASOURCE.into(),
    };
    assert_eq!(
        connection.agent(&different).err().unwrap(),
        QueryConnectionError::MissingAgent
    );
    let context = RequestContext::with_endpoint(
        Arc::new(support::Credential::default()),
        CredentialSelection::new("selected-account", "selected-tenant", Audience::Fabric).unwrap(),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap(),
    );
    assert_eq!(
        connection.auth_binding(&context).err().unwrap(),
        QueryConnectionError::UntrustedEndpoint
    );
}

#[test]
fn malformed_null_or_oversized_connection_never_becomes_an_absent_connection() {
    for value in [
        Value::Null,
        json!("not-an-object"),
        json!({}),
        json!({"protocol":"x".repeat(MAX_CONNECTION_BYTES+1)}),
    ] {
        let configured = Map::from_iter([("query_connection".into(), value)]);
        assert_eq!(
            QueryConnection::from_settings(Some(&configured), &trusted())
                .err()
                .unwrap(),
            QueryConnectionError::Invalid
        );
    }
}
