// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct capability consumers must compose with unchanged aggregate imports.

#![deny(unnameable_test_items)]

use fabric_cli_resources as legacy;
use serde_json::json;
use std::any::TypeId;

fn same_type<A: 'static, B: 'static>() {
    assert_eq!(TypeId::of::<A>(), TypeId::of::<B>());
}

#[test]
fn shared_values_errors_confirmation_presentation_and_http_keep_one_identity() {
    same_type::<legacy::CommandError, fabric_cli_command_contracts::CommandError>();
    same_type::<legacy::CommandValue, fabric_cli_command_contracts::CommandValue>();
    same_type::<legacy::CommandError, fabric_cli_access::CommandError>();
    same_type::<legacy::CommandError, fabric_cli_catalog::CommandError>();
    same_type::<legacy::CommandError, fabric_cli_api::CommandError>();
    same_type::<legacy::CommandValue, fabric_cli_access::CommandValue>();
    same_type::<legacy::CommandValue, fabric_cli_catalog::CommandValue>();
    same_type::<legacy::CommandValue, fabric_cli_api::CommandValue>();
    same_type::<&'static dyn legacy::Confirmation, &'static dyn fabric_cli_access::Confirmation>();
    same_type::<&'static dyn legacy::Confirmation, &'static dyn fabric_cli_catalog::Confirmation>();
    same_type::<
        &'static dyn legacy::Confirmation,
        &'static dyn fabric_cli_command_contracts::Confirmation,
    >();
    same_type::<legacy::FamilySurface, fabric_cli_command_contracts::FamilySurface>();
    same_type::<
        legacy::FamilyPresentation<'static>,
        fabric_cli_command_contracts::FamilyPresentation<'static>,
    >();
    same_type::<legacy::ResourceHttpExecutor, fabric_cli_command_contracts::ResourceHttpExecutor>();
}

#[test]
fn capability_options_and_output_callbacks_are_reexports_not_new_types() {
    same_type::<legacy::AclRead, fabric_cli_access::AclRead>();
    same_type::<legacy::AclChange, fabric_cli_access::AclChange>();
    same_type::<legacy::LabelChange, fabric_cli_access::LabelChange>();
    same_type::<legacy::MutationOptions, fabric_cli_access::MutationOptions>();
    same_type::<legacy::NamedResource, fabric_cli_access::NamedResource>();
    same_type::<&'static dyn legacy::AclExportSink, &'static dyn fabric_cli_access::AclExportSink>(
    );
    same_type::<legacy::FindOptions, fabric_cli_catalog::FindOptions>();
    same_type::<legacy::HiddenNameKind, fabric_cli_catalog::HiddenNameKind>();
    same_type::<legacy::HiddenRead, fabric_cli_catalog::HiddenRead>();
    same_type::<&'static dyn legacy::FindOutput, &'static dyn fabric_cli_catalog::FindOutput>();
    same_type::<legacy::ApiInput, fabric_cli_api::ApiInput>();
    same_type::<legacy::ApiOptions, fabric_cli_api::ApiOptions>();
}

#[test]
fn frontdoor_is_an_alias_of_the_single_nominal_compatibility_composition() {
    same_type::<legacy::frontdoor::AccessCommand, legacy::compatibility::AccessCommand>();
    same_type::<legacy::frontdoor::AccessKind, legacy::compatibility::AccessKind>();
    same_type::<legacy::frontdoor::RawApiCommand, legacy::compatibility::RawApiCommand>();
    same_type::<legacy::frontdoor::AccessHost<'static>, legacy::compatibility::AccessHost<'static>>(
    );
    same_type::<legacy::frontdoor::ApiClients<'static>, legacy::compatibility::ApiClients<'static>>(
    );
    same_type::<legacy::frontdoor::AccessOutput, legacy::compatibility::AccessOutput>();
    same_type::<
        &'static dyn legacy::frontdoor::AccessTargets,
        &'static dyn legacy::compatibility::AccessTargets,
    >();
}

#[test]
fn canonical_access_and_catalog_results_use_the_legacy_value_and_error_contract() {
    let labels: legacy::CommandValue =
        fabric_cli_access::labels_from_bytes(br#"{"labels":[{"name":"Public","id":"offline"}]}"#)
            .unwrap();
    assert_eq!(labels.data, Some(json!([{"name":"Public","id":"offline"}])));
    assert_eq!(
        labels,
        legacy::labels_from_bytes(br#"{"labels":[{"name":"Public","id":"offline"}]}"#).unwrap()
    );
    let rows = [json!({"displayName":"item","type":"Notebook","workspaceName":"workspace"})];
    let options: legacy::FindOptions = fabric_cli_catalog::FindOptions::default();
    let projected = fabric_cli_catalog::project_search(&rows, &options).unwrap();
    assert_eq!(
        projected,
        vec![json!({"name":"item","type":"Notebook","workspace":"workspace"})]
    );
    assert_eq!(projected, legacy::project_search(&rows, &options).unwrap());
}

#[test]
fn canonical_api_request_and_input_builders_accept_legacy_options_without_conversion() {
    let input: Option<legacy::ApiInput> = fabric_cli_api::read_api_input(
        &[r#"{"value":1}"#.into()],
        "offline-boundary",
        std::num::NonZeroUsize::new(1024).unwrap(),
    )
    .unwrap();
    let options = legacy::ApiOptions {
        origin: fabric_sdk_api::Origin::Fabric,
        method: azure_core::http::Method::Post,
        endpoint: "workspaces".into(),
        params: None,
        headers: None,
        input,
        query: None,
        show_headers: false,
        bounds: fabric_sdk_api::RequestBounds::default(),
    };
    let direct = fabric_cli_api::build_api_request(&options).unwrap();
    let facade = legacy::build_api_request(&options).unwrap();
    assert_eq!(direct.method(), facade.method());
    assert_eq!(direct.url(), facade.url());
    assert_eq!(direct.body(), facade.body());
    assert_eq!(direct.body(), br#"{"value":1}"#);
}
