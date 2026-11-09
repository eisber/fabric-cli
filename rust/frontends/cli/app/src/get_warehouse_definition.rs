// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Warehouse source retrieval through the dedicated native SDK and shared observer.

use crate::{
    get_backend::{backend_error, byte_payload, json_payload, observer_options},
    get_command::{GetAcquireOptions, GetCommandError, GetPayload},
};
use fabric_sdk::{
    Client,
    observe::observe,
    sql_definitions::DefinitionLimits,
    warehouse::{
        NativeMediaType, WarehouseDefinition, WarehouseDefinitionFormat, WarehouseDefinitionRead,
        WarehouseRequests, WarehouseTarget, read_definition,
    },
};
use fabric_sdk_operations::OperationExecutor;
use serde_json::json;
use std::{future::pending, num::NonZeroUsize};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct DefinitionSelectionError(&'static str);

fn selection_error(message: &'static str) -> GetCommandError {
    backend_error("UnsupportedCapability", DefinitionSelectionError(message))
}

pub(crate) async fn retrieve(
    client: &Client,
    executor: &dyn OperationExecutor,
    target: &WarehouseTarget,
    native_format: Option<&str>,
    part: Option<&str>,
    options: GetAcquireOptions,
    remaining: NonZeroUsize,
) -> Result<GetPayload, GetCommandError> {
    let format = native_format
        .map(WarehouseDefinitionFormat::try_from)
        .transpose()
        .map_err(|error| backend_error("InvalidTarget", error))?;
    // Every structural ceiling is bounded by the existing source-byte allowance.
    let request = WarehouseRequests::get_definition(
        target,
        format,
        DefinitionLimits {
            max_wire_bytes: remaining,
            max_decoded_bytes: remaining,
            max_part_bytes: remaining,
            max_parts: remaining,
            max_path_bytes: remaining,
            max_depth: remaining,
        },
    )
    .map_err(|error| backend_error("InvalidTarget", error))?;
    let read = read_definition(client, executor, &request, pending())
        .await
        .map_err(|error| backend_error("DefinitionReadFailed", error))?;
    let definition = match read {
        WarehouseDefinitionRead::Completed(definition) => definition.data,
        accepted @ WarehouseDefinitionRead::Accepted { .. } => {
            let handle = accepted.accepted_handle();
            if !options.wait {
                let WarehouseDefinitionRead::Accepted { operation_id, .. } = accepted else {
                    unreachable!("acceptance matched above")
                };
                let mut payload = json_payload(json!({
                    "status":"accepted",
                    "handle":operation_id.map(|id| format!("/operations/{id}")),
                    "completed":false,
                }));
                payload.complete = false;
                payload.data["meta"]["outcome"] = json!("accepted");
                add_owner(&mut payload, target, request.format());
                return Ok(payload);
            }
            let handle = handle.ok_or_else(|| {
                selection_error("accepted Warehouse definition has no native observation handle")
            })?;
            let observed = observe(
                client,
                executor,
                &handle,
                observer_options(options, true, remaining),
                pending(),
            )
            .await
            .map_err(|error| backend_error("ObservationFailed", error))?;
            if !observed.work_succeeded {
                return Err(selection_error("Warehouse definition work did not succeed"));
            }
            let result = observed.result.ok_or_else(|| {
                selection_error("Warehouse definition operation did not return its native result")
            })?;
            request
                .decode_completed_result(&result.bytes)
                .map_err(|error| backend_error("DefinitionResultInvalid", error))?
                .data
        }
    };
    content_payload(&definition, target, part)
}

fn content_payload(
    definition: &WarehouseDefinition,
    target: &WarehouseTarget,
    part: Option<&str>,
) -> Result<GetPayload, GetCommandError> {
    let mut payload = if let Some(path) = part {
        let (index, selected) = definition
            .parts()
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.path == path)
            .ok_or_else(|| selection_error("Warehouse definition part was not returned"))?;
        let media = match &definition.media_types()[index] {
            NativeMediaType::Value(value) => Some(value.clone()),
            NativeMediaType::Missing | NativeMediaType::Null => None,
        };
        let mut payload = byte_payload(selected.bytes.clone(), media);
        payload.data["meta"]["part"] = json!(selected.path);
        payload.data["meta"]["mediaTypeEvidence"] = json!(match &definition.media_types()[index] {
            NativeMediaType::Missing => "missing",
            NativeMediaType::Null => "null",
            NativeMediaType::Value(_) => "declared",
        });
        payload.data["meta"]["coverage"] = json!({
            "kind":"selectedPart", "fullDefinition":false,
        });
        payload
    } else {
        let native =
            serde_json::from_slice(definition.response_bytes()).map_err(GetCommandError::Json)?;
        let mut payload = json_payload(native);
        payload.data["meta"]["coverage"] = json!({
            "kind":"nativeDefinition", "fullDefinition":true,
        });
        payload
    };
    add_owner(&mut payload, target, definition.format());
    payload.data["meta"]["hasUnknownNativeFields"] = json!(definition.has_unknown_fields());
    Ok(payload)
}

fn add_owner(
    payload: &mut GetPayload,
    target: &WarehouseTarget,
    format: WarehouseDefinitionFormat,
) {
    payload.data["meta"]["representation"] = json!("sourceDefinition");
    payload.data["meta"]["nativeFormat"] = json!(format.as_str());
    payload.data["meta"]["owner"] = json!({
        "workspaceId":target.workspace_id().to_string(),
        "itemId":target.warehouse_id().to_string(),
        "type":"Warehouse",
        "provenance":"selectedRequest",
    });
}
