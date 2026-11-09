// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]
//! Offline public-boundary discovery contracts; no runtime or service fixtures.

use fabric_sdk_discovery::{
    CONTRACT_VERSION, DiscoveryError, INPUT_SCHEMA_DIALECT, MAX_TARGET_BYTES, Operation,
    OperationContract, TargetKind, TargetSelector, discover, discover_for_facade, registrations,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::str::FromStr;

fn contract(operation: Operation, path: &str) -> OperationContract {
    discover(operation, &TargetSelector::parse(path).expect("typed path")).expect("known contract")
}

#[test]
fn operation_spelling_and_serialization_are_exact_and_stable() {
    for (operation, spelling) in Operation::ALL
        .into_iter()
        .zip(["create", "update", "run", "delete"])
    {
        assert_eq!(operation.as_str(), spelling);
        assert_eq!(Operation::from_str(spelling), Ok(operation));
        assert_eq!(
            serde_json::to_value(operation).expect("serialize"),
            spelling
        );
    }
    for spelling in ["", "Create", " run", "run ", "read", "schema", "invoke"] {
        assert_eq!(
            Operation::from_str(spelling),
            Err(DiscoveryError::InvalidOperation)
        );
    }
    assert_eq!(
        serde_json::to_value(TargetKind::Workspace).expect("serialize"),
        "Workspace"
    );
    assert_eq!(TargetKind::Notebook.as_str(), "Notebook");
    assert_eq!(
        TargetKind::ALL,
        [TargetKind::Workspace, TargetKind::Notebook]
    );
    assert_eq!(DiscoveryError::InvalidOperation.code(), "invalid_operation");
    assert_eq!(
        DiscoveryError::UnsupportedCapability {
            reason: "unregistered exact operation"
        }
        .code(),
        "unsupported_capability"
    );
}

#[test]
fn prospective_and_existing_selectors_are_lexical_and_preserve_names() {
    for (path, kind) in [
        ("/New.Workspace", TargetKind::Workspace),
        ("/Sales.Workspace/Analysis.Notebook", TargetKind::Notebook),
        (
            "/研究 space.Workspace/Analysis.v2.Notebook",
            TargetKind::Notebook,
        ),
    ] {
        let target = TargetSelector::parse(path).expect("literal typed path");
        assert_eq!(target.path(), path);
        assert_eq!(target.kind(), kind);
    }
    let prospective = contract(Operation::Create, "/Does not exist.Workspace");
    let descriptor = prospective.descriptor();
    assert_eq!(descriptor["target"]["intent"], "prospective_destination");
    assert_eq!(descriptor["target"]["resolution"], "unverified");
    assert_eq!(descriptor["target"]["authorization"], "not_evaluated");
    assert_eq!(
        descriptor["availability"]["v2MutationExecution"],
        "unconfigured"
    );
    assert_eq!(descriptor["effects"]["discovery"], "none");
}

#[test]
fn malformed_paths_are_rejected_without_echoing_supplied_values() {
    for path in [
        "",
        "/",
        "Sales.Workspace",
        "//Sales.Workspace",
        "/Sales.Workspace/",
        "/Sales.Workspace//Analysis.Notebook",
        "/Sales.Workspace/../Analysis.Notebook",
        "/./Sales.Workspace",
        "/Sales.Workspace/Analysis.Notebook\n",
        "/Sales.Workspace/Anal\0ysis.Notebook",
        "/Sales.Workspace/*",
        "/Sales.Workspace/[ab].Notebook",
        "/Sales.Workspace/{name}.Notebook",
        "/Sales.Workspace/?.Notebook",
        "/Sales.Workspace/Analysis.Notebook#src",
        "/Sales.Workspace/%2e%2e",
        "https://example.invalid/Sales.Workspace",
        "/https:example.Workspace",
        "/Sales.Workspace\\Analysis.Notebook",
        "/.Workspace",
        "/..Workspace",
        "/...Workspace",
        "/ Sales.Workspace",
        "/Sales .Workspace",
        "/Sales.Workspace.Notebook",
        "/Sales.Notebook",
    ] {
        let error = TargetSelector::parse(path).expect_err("malformed selector");
        assert!(
            matches!(error, DiscoveryError::InvalidTarget { .. }),
            "{path:?}"
        );
        assert_eq!(error.code(), "invalid_target");
        if !path.is_empty() {
            assert!(
                !error.to_string().contains(path),
                "selector must not be echoed"
            );
        }
    }
    let overlong = format!("/{}.Workspace", "n".repeat(MAX_TARGET_BYTES));
    assert!(matches!(
        TargetSelector::parse(&overlong),
        Err(DiscoveryError::InvalidTarget { .. })
    ));
    let exactly_bounded = format!("/{}.Workspace", "n".repeat(MAX_TARGET_BYTES - 11));
    assert_eq!(exactly_bounded.len(), MAX_TARGET_BYTES);
    assert!(TargetSelector::parse(&exactly_bounded).is_ok());
}

#[test]
fn unknown_kinds_facets_and_functions_never_use_parent_contracts() {
    for path in [
        "/Sales.workspace",
        "/Sales.Lakehouse",
        "/Sales",
        "/Sales.Workspace/Analysis.Unknown",
        "/Sales.Workspace/Other.Workspace",
        "/Sales.Workspace/Analysis.Notebook/src/definition",
        "/Sales.Workspace/Analysis.Notebook/functions/name",
        "/Sales.Workspace/src/definition",
        "/Sales.Workspace/functions/name",
        "/Sales.Workspace/src",
        "/Sales.Workspace/functions",
    ] {
        assert!(
            matches!(
                TargetSelector::parse(path),
                Err(DiscoveryError::UnsupportedCapability { .. })
            ),
            "{path}"
        );
    }
}

#[test]
fn exact_operation_kind_matrix_has_only_the_documented_denominator() {
    for operation in [
        Operation::Create,
        Operation::Update,
        Operation::Run,
        Operation::Delete,
    ] {
        let workspace = TargetSelector::parse("/Sales.Workspace").expect("workspace");
        let notebook =
            TargetSelector::parse("/Sales.Workspace/Analysis.Notebook").expect("notebook");
        assert_eq!(
            discover(operation, &workspace).is_ok(),
            operation != Operation::Run
        );
        assert_eq!(
            discover(operation, &notebook).is_ok(),
            operation == Operation::Run
        );
    }
}

#[test]
fn sdk_provenance_paths_remain_bound_to_their_original_commit_tree() {
    const REVISION: &str = "5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b";
    for entry in registrations() {
        let (target, source_path) = match entry.target_kind() {
            TargetKind::Workspace => (
                "/Sales.Workspace",
                "rust/capabilities/resources/fabric-sdk-workspaces/src/mutations.rs",
            ),
            TargetKind::Notebook => (
                "/Sales.Workspace/Analysis.Notebook",
                "rust/capabilities/jobs/fabric-sdk-jobs/src/lib.rs",
            ),
        };
        let descriptor = contract(entry.operation(), target).descriptor();
        let sdk_sources: Vec<_> = descriptor["sources"]
            .as_array()
            .expect("source evidence")
            .iter()
            .filter(|source| source["revision"] == REVISION)
            .collect();
        assert_eq!(sdk_sources.len(), 1);
        assert_eq!(
            sdk_sources[0]["url"],
            format!("https://github.com/eisber/fabric-cli/blob/{REVISION}/{source_path}")
        );
    }
}

#[test]
fn catalogue_is_unique_deterministic_and_has_versioned_source_evidence() {
    let catalogue = registrations();
    assert_eq!(catalogue.len(), 4);
    let ids: BTreeSet<_> = catalogue.iter().map(|entry| entry.canonical_id()).collect();
    assert_eq!(ids.len(), catalogue.len());
    assert_eq!(
        catalogue[0].canonical_id(),
        "fabric.core.workspace.create.v1"
    );
    for entry in catalogue {
        let path = match entry.target_kind() {
            TargetKind::Workspace => "/Sales.Workspace",
            TargetKind::Notebook => "/Sales.Workspace/Analysis.Notebook",
        };
        let value = contract(entry.operation(), path).descriptor();
        assert_eq!(value["contractVersion"], CONTRACT_VERSION);
        assert_eq!(value["schemaVersion"], "1.0");
        assert_eq!(value["schemaKind"], "operation_input");
        assert_eq!(value["operationId"], entry.canonical_id());
        assert_eq!(value["backend"]["apiVersion"], "v1");
        assert_eq!(value["coverage"]["complete"], false);
        assert_eq!(
            value["coverage"]["nativeSemanticValidation"],
            "not_performed"
        );
        assert_eq!(value["result"]["operationIdIsExecutionHandle"], false);
        assert_eq!(value["result"]["acceptedIsCompleted"], false);
        let sources = value["sources"].as_array().expect("source evidence");
        assert!(!sources.is_empty());
        for source in sources {
            let revision = source["revision"].as_str().expect("revision");
            assert_eq!(revision.len(), 40);
            assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert!(source["url"].as_str().expect("URL").starts_with("https://"));
            assert!(!source["scope"].as_str().expect("scope").is_empty());
        }
        assert!(
            value["examples"][0]["sourceUrl"]
                .as_str()
                .expect("example link")
                .ends_with("#examples")
        );
    }
    for operation in [Operation::Create, Operation::Update, Operation::Delete] {
        let value = contract(operation, "/Sales.Workspace").descriptor();
        assert_eq!(
            value["sources"][0]["revision"],
            "9feaea0cb4ae5c7942ca78399640061545c3df95"
        );
    }
}

#[test]
fn create_schema_and_validation_preserve_absent_null_and_empty() {
    let contract = contract(Operation::Create, "/New.Workspace");
    let schema = contract.input_schema();
    assert_eq!(schema["$schema"], INPUT_SCHEMA_DIALECT);
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], json!(["displayName"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["displayName"]["maxLength"], 256);
    assert_eq!(schema["properties"]["description"]["maxLength"], 4000);
    assert_eq!(schema["properties"]["capacityId"]["format"], "uuid");
    assert_eq!(schema["properties"]["domainId"]["format"], "uuid");
    assert!(contract.validate_input(None).is_err());
    for invalid in [Value::Null, json!({}), json!({"displayName": null})] {
        assert!(contract.validate_input(Some(&invalid)).is_err());
    }
    for valid in [
        json!({"displayName": "New"}),
        json!({"displayName": ""}),
        json!({"displayName": "New", "description": ""}),
    ] {
        let before = valid.clone();
        assert!(contract.validate_input(Some(&valid)).is_ok());
        assert_eq!(valid, before);
    }
    assert!(
        contract
            .validate_input(Some(&json!({"displayName": "New", "description": null})))
            .is_err()
    );
    assert_eq!(contract.descriptor()["input"]["required"], true);
    assert_eq!(contract.descriptor()["input"]["defaultsApplied"], false);
}

#[test]
fn string_maxima_count_unicode_scalars_not_utf8_bytes() {
    let contract = contract(Operation::Create, "/New.Workspace");
    for name in ["x".repeat(256), "📓".repeat(256)] {
        assert!(
            contract
                .validate_input(Some(
                    &json!({"displayName": name, "description": "d".repeat(4000)})
                ))
                .is_ok()
        );
    }
    for invalid in [
        json!({"displayName": "x".repeat(257)}),
        json!({"displayName": "📓".repeat(257)}),
        json!({"displayName": "New", "description": "d".repeat(4001)}),
    ] {
        assert!(contract.validate_input(Some(&invalid)).is_err());
    }
}

#[test]
fn uuid_constraints_validate_structure_not_boolean_pseudo_parsing() {
    let contract = contract(Operation::Create, "/New.Workspace");
    for valid in [
        "00000000-0000-0000-0000-000000000000",
        "01234567-89ab-cdef-0123-456789abcdef",
        "ABCDEF01-2345-6789-ABCD-EF0123456789",
    ] {
        for field in ["capacityId", "domainId"] {
            let mut input = json!({"displayName": "New"});
            input[field] = json!(valid);
            assert!(contract.validate_input(Some(&input)).is_ok());
        }
    }
    for invalid in [
        "true",
        "false",
        "",
        "0123456789abcdef0123456789abcdef",
        "01234567_89ab-cdef-0123-456789abcdef",
        "g1234567-89ab-cdef-0123-456789abcdef",
        "01234567-89ab-cdef-0123-456789abcde",
        "01234567-89ab-cdef-0123-456789abcdef ",
        "{01234567-89ab-cdef-0123-456789abcdef}",
    ] {
        for field in ["capacityId", "domainId"] {
            let mut input = json!({"displayName": "New"});
            input[field] = json!(invalid);
            assert!(contract.validate_input(Some(&input)).is_err(), "{field}");
        }
    }
}

#[test]
fn all_registered_fields_reject_wrong_types_and_unknown_fields() {
    let contract = contract(Operation::Create, "/New.Workspace");
    for field in ["displayName", "description", "capacityId", "domainId"] {
        for wrong in [Value::Null, json!(true), json!(1), json!([]), json!({})] {
            let mut input = json!({"displayName": "New"});
            input[field] = wrong;
            assert!(matches!(
                contract.validate_input(Some(&input)),
                Err(DiscoveryError::InvalidInput { .. })
            ));
        }
    }
    let error = contract
        .validate_input(Some(
            &json!({"displayName": "New", "private_supplied_field": "secret"}),
        ))
        .expect_err("unknown field");
    assert_eq!(error.code(), "invalid_input");
    assert!(!error.to_string().contains("private_supplied_field"));
    assert!(!error.to_string().contains("secret"));
    for wrong in [
        Value::Null,
        json!(true),
        json!(1),
        json!([]),
        json!("payload"),
    ] {
        assert!(contract.validate_input(Some(&wrong)).is_err());
    }
}

#[test]
fn update_is_optional_metadata_patch_not_generic_v2_replacement() {
    let contract = contract(Operation::Update, "/Sales.Workspace");
    let schema = contract.input_schema();
    assert_eq!(schema["required"], json!([]));
    assert_eq!(schema["properties"].as_object().expect("fields").len(), 2);
    assert!(schema.get("minProperties").is_none());
    for good in [
        json!({}),
        json!({"displayName": ""}),
        json!({"description": ""}),
        json!({"displayName": "Renamed", "description": "changed"}),
    ] {
        assert!(contract.validate_input(Some(&good)).is_ok());
    }
    assert!(contract.validate_input(None).is_err());
    for bad in [
        Value::Null,
        json!({"displayName": null}),
        json!({"description": null}),
        json!({"mode": "replace"}),
        json!({"capacityId": "00000000-0000-0000-0000-000000000000"}),
        json!({"domainId": "00000000-0000-0000-0000-000000000000"}),
        json!({"revision": 1}),
        json!({"displayName": "x".repeat(257)}),
        json!({"description": "x".repeat(4001)}),
    ] {
        assert!(contract.validate_input(Some(&bad)).is_err());
    }
    let descriptor = contract.descriptor();
    assert_eq!(descriptor["backend"]["nativeMethod"], "PATCH");
    assert_eq!(
        descriptor["coverage"]["nativeSemantics"],
        "metadata_patch_only"
    );
    assert_eq!(descriptor["coverage"]["supportedV2UpdateModes"], json!([]));
    assert_eq!(
        descriptor["coverage"]["v2UpdateModeSelection"],
        "unconfigured"
    );
    assert!(
        descriptor["coverage"]["unsupported"]
            .as_array()
            .expect("gaps")
            .contains(&json!("v2_update_mode_selection"))
    );
}

#[test]
fn deletion_accepts_absence_only_and_does_not_claim_recoverability() {
    let contract = contract(Operation::Delete, "/Sales.Workspace");
    assert_eq!(contract.input_schema(), Value::Bool(false));
    assert!(contract.validate_input(None).is_ok());
    for body in [
        Value::Null,
        json!({}),
        json!({"recursive": true}),
        json!({"permanent": true}),
        json!({"revision": "old"}),
    ] {
        assert!(matches!(
            contract.validate_input(Some(&body)),
            Err(DiscoveryError::InvalidInput { .. })
        ));
    }
    let value = contract.descriptor();
    assert_eq!(value["input"]["presence"], "absent_only");
    assert_eq!(value["input"]["mediaType"], Value::Null);
    for unsupported in [
        "recursive_option",
        "permanent_option",
        "recoverable_delete_guarantee",
    ] {
        assert!(
            value["coverage"]["unsupported"]
                .as_array()
                .expect("gaps")
                .contains(&json!(unsupported))
        );
    }
    assert!(
        value["effects"]["describedOperation"]
            .as_array()
            .expect("effects")
            .contains(&json!("would_delete_workspace_and_contained_items"))
    );
}

#[test]
fn notebook_run_is_no_body_only_and_parameters_are_explicit_capability_gaps() {
    let contract = contract(Operation::Run, "/Sales.Workspace/Analysis.Notebook");
    assert!(contract.validate_input(None).is_ok());
    assert_eq!(contract.input_schema(), Value::Bool(false));
    for body in [
        Value::Null,
        json!({}),
        json!({"executionData": {}}),
        json!({"parameters": []}),
        json!({"function": "name"}),
        json!("opaque"),
    ] {
        assert!(matches!(
            contract.validate_input(Some(&body)),
            Err(DiscoveryError::UnsupportedCapability { .. })
        ));
    }
    assert_eq!(
        contract.canonical_id(),
        "fabric.core.notebook.run-default.v1"
    );
    assert!(contract.help().contains("RunNotebook"));
    assert!(contract.help().contains("exact signature unavailable"));
    assert!(
        contract.descriptor()["result"]["execution"]
            .as_str()
            .expect("boundary")
            .contains("acceptance, not completion")
    );
}

#[test]
fn facade_discovery_is_the_identical_contract_after_exact_kind_narrowing() {
    for entry in registrations() {
        let path = match entry.target_kind() {
            TargetKind::Workspace => "/Sales.Workspace",
            TargetKind::Notebook => "/Sales.Workspace/Analysis.Notebook",
        };
        let target = TargetSelector::parse(path).expect("target");
        let direct = discover(entry.operation(), &target).expect("direct");
        let facade =
            discover_for_facade(entry.operation(), &target, entry.target_kind()).expect("facade");
        assert_eq!(direct.descriptor(), facade.descriptor());
        assert_eq!(direct.input_schema(), facade.input_schema());
        assert_eq!(direct.mcp_tool(), facade.mcp_tool());
        assert!(std::ptr::eq(direct.registration(), facade.registration()));
        let wrong_kind = match entry.target_kind() {
            TargetKind::Workspace => TargetKind::Notebook,
            TargetKind::Notebook => TargetKind::Workspace,
        };
        assert!(matches!(
            discover_for_facade(entry.operation(), &target, wrong_kind),
            Err(DiscoveryError::UnsupportedCapability { .. })
        ));
    }
}

#[test]
fn cli_sdk_mcp_and_serialization_project_one_registration_without_side_effects() {
    for entry in registrations() {
        let path = match entry.target_kind() {
            TargetKind::Workspace => "/Prospective unverified.Workspace",
            TargetKind::Notebook => "/Unverified.Workspace/Unverified.Notebook",
        };
        let contract = contract(entry.operation(), path);
        let before = contract.target().clone();
        let descriptor = contract.descriptor();
        let cli = entry.cli_descriptor();
        let mcp = contract.mcp_tool();
        let sdk_schema = contract.input_schema();
        assert_eq!(
            serde_json::to_value(&contract).expect("serialize"),
            descriptor
        );
        assert_eq!(descriptor["input"]["schema"], sdk_schema);
        assert_eq!(cli["input"], descriptor["input"]);
        assert_eq!(mcp["inputSchema"]["properties"]["input"], sdk_schema);
        assert_eq!(cli["operationId"], descriptor["operationId"]);
        assert_eq!(mcp["name"], descriptor["operationId"]);
        assert_eq!(mcp["inputSchema"]["additionalProperties"], false);
        let required = if descriptor["input"]["required"] == true {
            json!(["input"])
        } else {
            json!([])
        };
        assert_eq!(mcp["inputSchema"]["required"], required);
        assert_eq!(mcp["_meta"]["fabricDiscovery"]["projectionOnly"], true);
        assert_eq!(mcp["_meta"]["fabricDiscovery"]["execution"], "unconfigured");
        assert_eq!(
            mcp["_meta"]["fabricDiscovery"]["target"],
            descriptor["target"]
        );
        assert_eq!(cli["help"], contract.help());
        assert_eq!(contract.target(), &before);
        assert_eq!(contract.operation(), entry.operation());
        assert_eq!(contract.registration().summary(), entry.summary());
        assert!(contract.help().contains("resolution unverified"));
        assert!(contract.help().contains("authorization not evaluated"));
        assert!(contract.help().contains("execution unconfigured"));
        assert!(mcp.get("server").is_none());
    }
}
