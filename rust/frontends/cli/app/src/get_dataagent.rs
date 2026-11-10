// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! DataAgent-only adaptation over shared GET acquisition, observation and output.

use crate::{
    get_backend::{backend_error, byte_payload, json_payload, observer_options},
    get_budget::GetReadBudget,
    get_command::{GetAcquireOptions, GetCommandError, GetPayload},
    get_target::{DataAgentStage, GetRepresentation},
};
use fabric_sdk::{
    Client,
    dataagents::{DataAgentError, DataAgentRequests, DataAgentTarget, NativeFormat, Stage},
    observe::observe,
    representations::{DefinitionRead, RepresentationError, RepresentationLimits},
};
use serde_json::json;
use std::{future::pending, num::NonZeroUsize};

pub(super) fn validate_representation(
    representation: &GetRepresentation,
) -> Result<(), GetCommandError> {
    match representation {
        GetRepresentation::Metadata
        | GetRepresentation::DataAgentSettings
        | GetRepresentation::DataAgentDatasources
        | GetRepresentation::DataAgentSource { .. } => Ok(()),
        GetRepresentation::Definition { native_format, .. }
            if native_format
                .as_deref()
                .is_none_or(|format| format == "JSON") =>
        {
            Ok(())
        }
        _ => Err(backend_error(
            "UnsupportedDataAgentRepresentation",
            DataAgentError::InvalidInput,
        )),
    }
}

fn native_stage(stage: DataAgentStage) -> Stage {
    match stage {
        DataAgentStage::Draft => Stage::Staging,
        DataAgentStage::Published => Stage::Published,
    }
}

pub(super) async fn retrieve(
    client: &Client,
    budget: &GetReadBudget<'_>,
    workspace: &str,
    agent: &str,
    representation: &GetRepresentation,
    options: GetAcquireOptions,
) -> Result<GetPayload, GetCommandError> {
    validate_representation(representation)?;
    let target = DataAgentTarget::new(workspace, agent, "DataAgent")
        .map_err(|error| backend_error("InvalidDataAgentTarget", error))?;
    let cap = options.limits.max_bytes;
    let limits = || -> Result<RepresentationLimits, GetCommandError> {
        Ok(RepresentationLimits {
            max_response_bytes: NonZeroUsize::new(budget.remaining_bytes()).ok_or(
                GetCommandError::ByteLimit {
                    limit: cap.get(),
                    observed: cap.get().saturating_add(1),
                },
            )?,
            max_result_bytes: cap,
        })
    };
    let metadata = DataAgentRequests::metadata(&target)
        .map_err(|error| backend_error("InvalidDataAgentTarget", error))?
        .execute(client, budget, limits()?, pending())
        .await
        .map_err(|error| backend_error("DataAgentMetadataFailed", error))?;
    if matches!(representation, GetRepresentation::Metadata) {
        return Ok(json_payload(metadata.as_value().clone()));
    }
    if matches!(
        representation,
        GetRepresentation::DataAgentSettings | GetRepresentation::DataAgentDatasources
    ) {
        let stage = options
            .stage
            .ok_or_else(|| backend_error("DataAgentStageRequired", DataAgentError::InvalidInput))?;
        let mut payload = if matches!(representation, GetRepresentation::DataAgentSettings) {
            let settings = DataAgentRequests::settings(&target, native_stage(stage))
                .map_err(|error| backend_error("InvalidDataAgentTarget", error))?
                .execute(client, budget, limits()?, pending())
                .await
                .map_err(|error| backend_error("DataAgentSettingsFailed", error))?;
            json_payload(settings.as_value().clone())
        } else {
            let page = DataAgentRequests::datasources_page(&target, native_stage(stage), None)
                .map_err(|error| backend_error("InvalidDataAgentTarget", error))?
                .execute(client, budget, limits()?, pending())
                .await
                .map_err(|error| backend_error("DataAgentDatasourcesFailed", error))?;
            let mut payload =
                json_payload(serde_json::to_value(&page.page).map_err(GetCommandError::Json)?);
            payload.data["meta"]["collectionComplete"] = json!(page.is_complete());
            payload.data["meta"]["scope"] = json!("one-native-page");
            payload
        };
        payload.data["meta"]["stage"] = json!(stage.as_str());
        payload.data["meta"]["preview"] = json!(true);
        return Ok(payload);
    }
    let (format, part, stage) = match representation {
        GetRepresentation::Definition {
            native_format,
            part,
        } => (
            native_format.as_ref().map(|_| NativeFormat::Json),
            part.as_deref(),
            None,
        ),
        GetRepresentation::DataAgentSource { stage, part } => (None, part.as_deref(), Some(*stage)),
        _ => unreachable!("validated DataAgent reads handled above"),
    };
    let source_limits = limits()?;
    let mut read = DataAgentRequests::definition(&target, format)
        .map_err(|error| backend_error("InvalidDataAgentTarget", error))?
        .execute(client, budget, source_limits, pending())
        .await
        .map_err(|error| backend_error("DataAgentDefinitionFailed", error))?;
    if let DefinitionRead::Accepted { operation_id } = &read {
        if !options.wait {
            let mut receipt = json_payload(json!({
                "status":"accepted",
                "handle":format!("/operations/{operation_id}"),
                "completed":false
            }));
            receipt.data["meta"]["outcome"] = json!("accepted");
            if let Some(stage) = stage {
                receipt.data["meta"]["requestedStage"] = json!(stage.as_str());
                receipt.data["meta"]["stageVerified"] = json!(false);
            }
            receipt.complete = false;
            return Ok(receipt);
        }
        let handle = read.accepted_handle().expect("validated accepted handle");
        let observation = observe(
            client,
            budget,
            &handle,
            observer_options(options, true, limits()?.max_response_bytes),
            pending(),
        )
        .await
        .map_err(|error| backend_error("DataAgentObservationFailed", error))?;
        let result = observation.result.ok_or_else(|| {
            backend_error("DataAgentResultInvalid", DataAgentError::InvalidResponse)
        })?;
        read = DefinitionRead::from_completed_result(&result.bytes, source_limits)
            .map_err(|error| backend_error("DataAgentResultInvalid", error))?;
    }
    source_payload(read, stage, part)
}

fn source_payload(
    read: DefinitionRead,
    stage: Option<DataAgentStage>,
    part: Option<&str>,
) -> Result<GetPayload, GetCommandError> {
    let DefinitionRead::Completed { definition } = &read else {
        unreachable!("acceptance returned above")
    };
    let mut draft = Vec::new();
    let mut published = Vec::new();
    let mut shared = Vec::new();
    let mut unclassified = Vec::new();
    let mut inventory = Vec::new();
    for (index, part) in definition["definition"]["parts"]
        .as_array()
        .expect("shared definition validation")
        .iter()
        .enumerate()
    {
        let path = part["path"].as_str().expect("validated native part path");
        let (group, indices) = if path.starts_with("Files/Config/draft/") {
            ("draft", &mut draft)
        } else if path.starts_with("Files/Config/published/") {
            ("published", &mut published)
        } else if matches!(
            path,
            "Files/Config/data_agent.json" | "Files/Config/publish_info.json" | ".platform"
        ) {
            ("shared", &mut shared)
        } else {
            ("unclassified", &mut unclassified)
        };
        indices.push(index);
        inventory.push(json!({"index":index,"path":path,"scope":group}));
    }
    let selected = stage.map(|stage| match stage {
        DataAgentStage::Draft => &draft,
        DataAgentStage::Published => &published,
    });
    if selected.is_some_and(Vec::is_empty) {
        return Err(backend_error(
            "DataAgentStageUnavailable",
            RepresentationError::NotFound,
        ));
    }
    let exact_path = part.map(|path| {
        stage.map_or_else(
            || path.to_owned(),
            |stage| format!("Files/Config/{}/{path}", stage.as_str()),
        )
    });
    let mut payload = if let Some(path) = &exact_path {
        byte_payload(
            read.part_bytes(path)
                .map_err(|error| backend_error("DataAgentPartFailed", error))?,
            None,
        )
    } else if let Some(indices) = selected {
        json_payload(json!({
            "nativeDefinition":definition,
            "selectedPartIndices":indices
        }))
    } else {
        json_payload(definition.clone())
    };
    payload.data["meta"]["dataAgentSource"] = json!({
        "stage":stage.map(DataAgentStage::as_str),
        "scope":if part.is_some() {"exact-part"} else if stage.is_some() {"stage-projection"} else {"full-definition"},
        "fullSourceRoundtrip":stage.is_none() && part.is_none(),
        "nativeDefinitionIncluded":part.is_none(),
        "selectedNativePath":exact_path,
        "draftPartIndices":draft,
        "publishedPartIndices":published,
        "sharedPartIndices":shared,
        "unclassifiedPartIndices":unclassified,
        "parts":inventory
    });
    Ok(payload)
}
