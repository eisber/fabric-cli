// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use fabric_sdk_dataagent_ontology::{
    ValidationError,
    inputs::*,
    models::*,
    scope::*,
};
use serde_json::json;

fn guid(number: u8) -> Guid {
    Guid::new(format!("11111111-2222-3333-4444-{number:012}")).unwrap()
}

fn attachment(stage: Stage, agent: u8, ontology: u8) -> AttachmentScope {
    AttachmentScope {
        agent: AgentScope { workspace_id: guid(1), agent_id: guid(agent) },
        stage,
        ontology_id: guid(ontology),
    }
}

#[test]
fn identity_is_not_a_display_name_path_or_zero_guid() {
    for invalid in ["", "../ontologies", "00000000-0000-0000-0000-000000000000"] {
        assert_eq!(Guid::new(invalid), Err(ValidationError::InvalidGuid));
    }
    assert_eq!(
        Guid::new("AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE").unwrap(),
        Guid::new("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap()
    );
    assert_ne!(attachment(Stage::Staging, 2, 3), attachment(Stage::Staging, 4, 3));
    assert_ne!(attachment(Stage::Staging, 2, 3), attachment(Stage::Published, 2, 3));
}

#[test]
fn attach_body_uses_referenced_workspace_and_native_discriminator() {
    let input = AttachOntology::new(ItemReference::by_id(guid(3), guid(4)), "Ontology").unwrap();
    assert_eq!(
        serde_json::to_value(input).unwrap(),
        json!({
            "ontologyReference": {
                "referenceType": "ById",
                "itemId": guid(3),
                "workspaceId": guid(4)
            },
            "displayName": "Ontology"
        })
    );
    for name in ["", "..", "a/b", "a\\b", "bad\nname"] {
        assert!(AttachOntology::new(ItemReference::by_id(guid(3), guid(4)), name).is_err());
    }
}

#[test]
fn native_selection_distinguishes_false_all_empty_and_named() {
    assert_eq!(serde_json::to_value(SelectEntities::all(false)).unwrap(), json!({"isSelected": false}));
    assert!(SelectEntities::named(vec![], true).is_err());
    assert!(SelectEntities::named(vec!["opaque".into(); 501], true).is_err());
    assert!(SelectEntities::named(vec!["x".repeat(8193)], true).is_err());
    let ids = vec!["a+/= opaque".into(), "~v1:entity".into()];
    assert_eq!(
        serde_json::to_value(SelectEntities::named(ids.clone(), true).unwrap()).unwrap(),
        json!({"elementIds": ids, "isSelected": true})
    );
}

#[test]
fn authored_patch_does_not_replace_or_regenerate_omitted_authority() {
    assert_eq!(
        serde_json::to_value(InstructionsPatch::new(Some("authored".into()), None).unwrap()).unwrap(),
        json!({"instructions": "authored"})
    );
    assert_eq!(
        serde_json::to_value(InstructionsReplace::new(None, Some("description".into())).unwrap()).unwrap(),
        json!({"instructions": null, "description": "description"})
    );
    assert_eq!(
        serde_json::to_value(FewShotPatch::new(None, Some("query source".into())).unwrap()).unwrap(),
        json!({"query": "query source"})
    );
}

#[test]
fn text_limits_match_dotnet_utf16_not_utf8_or_unicode_scalar_count() {
    let pair = String::from_utf8(vec![0xf0, 0x9f, 0x98, 0x80]).unwrap();
    assert!(InstructionsPatch::new(Some("a".repeat(15_000)), None).is_ok());
    assert!(InstructionsPatch::new(Some(pair.repeat(7500)), None).is_ok());
    assert!(InstructionsPatch::new(Some(pair.repeat(7501)), None).is_err());
    assert!(FewShotCreate::new("a".repeat(1000), Some("q".repeat(5000))).is_ok());
    assert!(FewShotCreate::new(String::new(), Some("draft".into())).is_ok());
    assert!(FewShotCreate::new("a".repeat(1001), None).is_err());
    assert!(FewShotCreate::new("q".into(), Some("a".repeat(5001))).is_err());
    assert!(Publish::new(Some("a".repeat(500_001))).is_err());
}

#[test]
fn validators_never_cross_caller_stage_agent_ontology_or_source_boundaries() {
    let caller = CallerScope::new("account", "tenant").unwrap();
    let scope = SourceScope { attachment: attachment(Stage::Staging, 2, 3), source_id: guid(4) };
    let resource = Resource::Instructions(scope.clone());
    let tag = Validator::new(caller.clone(), resource.clone(), "\"native-etag\"").unwrap();
    assert_eq!(tag.for_resource(&caller, &resource).unwrap(), "\"native-etag\"");
    let other_caller = CallerScope::new("other", "tenant").unwrap();
    assert!(tag.for_resource(&other_caller, &resource).is_err());
    for changed in [
        SourceScope { attachment: attachment(Stage::Published, 2, 3), ..scope.clone() },
        SourceScope { attachment: attachment(Stage::Staging, 5, 3), ..scope.clone() },
        SourceScope { attachment: attachment(Stage::Staging, 2, 6), ..scope.clone() },
        SourceScope { source_id: guid(7), ..scope.clone() },
    ] {
        assert_eq!(
            tag.for_resource(&caller, &Resource::Instructions(changed)),
            Err(ValidationError::ValidatorScope)
        );
    }
    assert!(tag.for_resource(&caller, &Resource::FewShots(scope)).is_err());
    for bad in ["*", "", "etag\r\ninjected: value"] {
        assert!(Validator::new(caller.clone(), resource.clone(), bad).is_err());
    }
    assert!(!format!("{tag:?}").contains("native-etag"));
}

#[test]
fn malformed_pages_and_rows_are_not_empty_success() {
    for invalid in [json!({}), json!({"value": null}), json!({"value": [{}]})] {
        assert!(serde_json::from_value::<Page<OntologyAttachment>>(invalid).is_err());
    }
    assert!(serde_json::from_value::<Page<Element>>(json!({"value": [{"id": "only"}]})).is_err());
    assert!(serde_json::from_value::<SourceSchema>(json!({
        "dataSourceId": guid(4),
        "cacheLastUpdatedTime": "2026-11-05T00:00:00Z",
        "elements": [null]
    })).is_err());
    assert!(serde_json::from_value::<SourceSchema>(json!({
        "dataSourceId": guid(4),
        "cacheLastUpdatedTime": "2026-11-05T00:00:00Z",
        "elements": [{}]
    })).is_err());
}

#[test]
fn cached_schema_uses_native_snake_case_not_management_element_shape() {
    let schema: SourceSchema = serde_json::from_value(json!({
        "dataSourceId": guid(4), "cacheLastUpdatedTime": "2026-11-05T00:00:00Z",
        "elements": [{
            "display_name": "table", "is_selected": true, "type": 2, "children": [],
            "sql_definition": "source catalog data"
        }]
    })).unwrap();
    assert_eq!(schema.elements[0].display_name, "table");
    assert_eq!(schema.elements[0].additional["sql_definition"], "source catalog data");
}

#[test]
fn source_tombstones_and_physical_identity_remain_distinct() {
    let source: OntologySource = serde_json::from_value(json!({
        "id": guid(4),
        "type": "Eventhouse",
        "itemReference": {
            "referenceType": "ById", "workspaceId": guid(1), "itemId": guid(5)
        },
        "instructions": "authoritative",
        "state": "PendingConversion",
        "eTag": "\"setup\""
    })).unwrap();
    assert_ne!(source.id, source.item_reference.unwrap().item_id);
    assert_eq!(source.instructions.as_deref(), Some("authoritative"));
    let unavailable: OntologySource = serde_json::from_value(json!({
        "id": guid(4), "state": "NotAvailable"
    })).unwrap();
    assert!(unavailable.workspace_id.is_none());
    assert_eq!(unavailable.state.as_deref(), Some("NotAvailable"));
}
