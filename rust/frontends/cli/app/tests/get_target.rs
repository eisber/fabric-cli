// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure target/representation selection without remote discovery.

#![deny(unnameable_test_items)]

use fabric_cli::get_target::{DataAgentStage, GetPathError, GetRepresentation, GetTarget};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";

#[test]
fn metadata_and_source_are_different_typed_requests() {
    let metadata = GetTarget::parse("/Sales.Workspace/Forecast.Notebook").unwrap();
    assert_eq!(
        metadata,
        GetTarget::Item {
            workspace: "Sales".into(),
            item: "Forecast".into(),
            item_type: Some("Notebook".into()),
            representation: GetRepresentation::Metadata,
        }
    );
    assert!(matches!(
        GetTarget::parse("/Sales.Workspace/Forecast.Notebook/src/definition").unwrap(),
        GetTarget::Item {
            representation: GetRepresentation::Definition { part: None, .. },
            ..
        }
    ));
    assert!(matches!(
        GetTarget::parse("/Sales.Workspace/Model.SemanticModel/src/definition/formats/TMDL/parts/definition/model.tmdl").unwrap(),
        GetTarget::Item {
            representation: GetRepresentation::Definition { native_format: Some(format), part: Some(part) }, ..
        } if format == "TMDL" && part == "definition/model.tmdl"
    ));
}

#[test]
fn operation_and_job_handles_preserve_exact_scoped_identity() {
    assert_eq!(
        GetTarget::parse(&format!("/operations/{ITEM}/result")).unwrap(),
        GetTarget::Operation {
            operation_id: ITEM.into(),
            result: true,
        }
    );
    assert!(matches!(
        GetTarget::parse(&format!("/workspaces/{WS}/items/{ITEM}/runs/{ITEM}")).unwrap(),
        GetTarget::Item { representation:GetRepresentation::Job { job_id }, .. } if job_id == ITEM
    ));
}

#[test]
fn docs_and_files_select_only_the_addressed_representation() {
    assert!(matches!(
        GetTarget::parse("/Sales.Workspace/Model.SemanticModel/docs/schema").unwrap(),
        GetTarget::Item { representation:GetRepresentation::Docs(view), .. } if view == "schema"
    ));
    assert!(matches!(
        GetTarget::parse("/Sales.Workspace/Bronze.Lakehouse/Files/a b/empty.bin").unwrap(),
        GetTarget::Item { representation:GetRepresentation::File(path), .. } if path == "Files/a b/empty.bin"
    ));
}

#[test]
fn invalid_paths_and_unadvertised_facets_do_not_become_metadata() {
    for path in [
        "",
        "/",
        "Sales.Workspace",
        "https://api.fabric.microsoft.com/v1/items/id",
        "/Sales.Workspace//Report.Report",
        "/Sales.Workspace/Item.Notebook/../src/definition",
        "/Sales.Workspace/Item.Notebook/%2e%2e",
        "/Sales.Workspace/Item.Notebook?format=TMDL",
        "/operations/not-a-guid",
        "/workspaces/not-a-guid/items/other",
    ] {
        assert!(GetTarget::parse(path).is_err(), "{path}");
    }

    for path in [
        "/Sales.Workspace/Item.Notebook/settings/secret",
        "/Sales.Workspace/Item.Notebook/src/context/topics/a",
        "/Sales.Workspace/Item.Notebook/src/owl",
        "/Sales.Workspace/Item.Notebook/src/definition/guessed.json",
    ] {
        assert_eq!(
            GetTarget::parse(path),
            Err(GetPathError::UnsupportedRepresentation),
            "{path}"
        );
    }
}

#[test]
fn dataagent_stages_are_typed_explicit_and_never_inferred_from_machine_ids() {
    let target =
        GetTarget::parse("/Sales.Workspace/Agent.DataAgent/src/draft/stage_config.json").unwrap();
    assert!(matches!(
        target,
        GetTarget::Item {
            representation: GetRepresentation::DataAgentSource {
                stage: DataAgentStage::Draft, part: Some(ref part)
            }, ..
        } if part == "stage_config.json"
    ));
    assert!(target.validate_stage(None).is_ok());
    assert!(target.validate_stage(Some(DataAgentStage::Draft)).is_err());
    let settings = GetTarget::parse("/Sales.Workspace/Agent.DataAgent/settings").unwrap();
    assert!(settings.validate_stage(None).is_err());
    assert!(
        settings
            .validate_stage(Some(DataAgentStage::Published))
            .is_ok()
    );
    for suffix in [
        "/settings",
        "/datasources",
        "/src/draft",
        "/src/published/a.json",
    ] {
        for owner in [
            format!("/workspaces/{WS}/items/{ITEM}"),
            "/Sales.Workspace/Agent.Notebook".into(),
        ] {
            assert!(GetTarget::parse(&format!("{owner}{suffix}")).is_err());
        }
    }
    for path in [
        "/Sales.Workspace/Agent.DataAgent/src/staging",
        "/Sales.Workspace/Agent.DataAgent/src/published/../draft/a",
    ] {
        assert!(GetTarget::parse(path).is_err());
    }
}
