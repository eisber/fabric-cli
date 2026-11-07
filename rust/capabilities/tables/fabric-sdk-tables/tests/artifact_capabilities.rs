// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parent-owned discovery contracts without service execution.

#![deny(unnameable_test_items)]

use fabric_sdk_tables::capabilities::{
    BindingSupport, ParentArtifact, TableOperation, TableTransport, describe, describe_json,
};

#[test]
fn common_load_intent_keeps_three_native_bindings() {
    let cases = [
        (ParentArtifact::Lakehouse, TableTransport::FabricLoad),
        (ParentArtifact::KqlDatabase, TableTransport::KustoManagement),
        (ParentArtifact::Warehouse, TableTransport::Tds),
    ];
    for (parent, transport) in cases {
        let binding = describe(parent, TableOperation::Load);
        assert_eq!(binding.parent, parent);
        assert_eq!(binding.transport, Some(transport));
        assert_eq!(binding.support, BindingSupport::Adapter);
        let json = describe_json(parent, TableOperation::Load);
        assert_eq!(json["verb"], "load");
        assert_eq!(json["parentType"], parent.name());
        assert_eq!(json["requiresInjectedExecutor"], true);
        assert!(json.get("configured").is_none());
    }
}

#[test]
fn read_representations_are_get_not_new_verbs() {
    for operation in [TableOperation::GetSchema, TableOperation::GetStatistics] {
        assert_eq!(operation.verb(), "get");
        assert!(operation.representation().is_some());
    }
    assert_eq!(
        describe(ParentArtifact::Lakehouse, TableOperation::GetStatistics).support,
        BindingSupport::AdditionalBindingRequired
    );
    assert_eq!(
        describe(ParentArtifact::Lakehouse, TableOperation::Query).support,
        BindingSupport::AdditionalBindingRequired
    );
}

#[test]
fn lakehouse_maintenance_is_not_generic_retention_or_optimization() {
    for operation in [TableOperation::Optimize, TableOperation::Vacuum] {
        assert_eq!(
            describe(ParentArtifact::Lakehouse, operation).transport,
            Some(TableTransport::FabricJob)
        );
        for parent in [ParentArtifact::KqlDatabase, ParentArtifact::Warehouse] {
            let binding = describe(parent, operation);
            assert_eq!(binding.support, BindingSupport::Unsupported);
            assert_eq!(binding.transport, None);
        }
    }
}

#[test]
fn copy_cannot_silently_become_a_file_copy() {
    for parent in [
        ParentArtifact::Lakehouse,
        ParentArtifact::KqlDatabase,
        ParentArtifact::Warehouse,
    ] {
        let json = describe_json(parent, TableOperation::Copy);
        assert_eq!(json["support"], "unsupported");
        assert!(json["transport"].is_null());
        assert_eq!(json["requiresInjectedExecutor"], false);
    }
}
