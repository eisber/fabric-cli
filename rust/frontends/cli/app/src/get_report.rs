// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Report-only v2 get binding over shared acquisition, observation and delivery.

use crate::{
    get_backend::{backend_error, byte_payload, json_payload, observer_options},
    get_budget::GetReadBudget,
    get_command::{GetAcquireOptions, GetCommandError, GetPayload},
    get_target::GetRepresentation,
};
use fabric_sdk::{
    Client,
    metadata::MetadataLimits,
    observe::observe,
    reports::{self, ReportDefinition, ReportDefinitionRead, ReportTarget},
    representations::{DefinitionRead, RepresentationLimits},
};
use serde_json::json;
use std::{future::pending, num::NonZeroUsize};

pub(super) async fn retrieve(
    client: &Client,
    budget: &GetReadBudget<'_>,
    workspace: &str,
    report: &str,
    representation: &GetRepresentation,
    options: GetAcquireOptions,
) -> Result<GetPayload, GetCommandError> {
    if let GetRepresentation::Definition { native_format, .. } = representation
        && native_format
            .as_deref()
            .is_some_and(|format| !matches!(format, "PBIR" | "PBIR-Legacy"))
    {
        return Err(backend_error(
            "UnsupportedReportFormat",
            reports::ReportError::UnsupportedFormat,
        ));
    }
    let cap = options.limits.max_bytes;
    let remaining = || {
        NonZeroUsize::new(budget.remaining_bytes()).ok_or(GetCommandError::ByteLimit {
            limit: cap.get(),
            observed: cap.get().saturating_add(1),
        })
    };
    let target = ReportTarget::new(workspace, report)
        .map_err(|error| backend_error("InvalidReportTarget", error))?;
    let metadata = reports::read_metadata(
        client,
        budget,
        &target,
        MetadataLimits {
            max_response_bytes: remaining()?,
            max_result_bytes: cap,
        },
        pending(),
    )
    .await
    .map_err(|error| backend_error("ReportMetadataFailed", error))?;
    let GetRepresentation::Definition {
        native_format,
        part,
    } = representation
    else {
        return Ok(json_payload(metadata.into_value()));
    };
    let limits = RepresentationLimits {
        max_response_bytes: remaining()?,
        max_result_bytes: cap,
    };
    let read = reports::read_definition(
        client,
        budget,
        &metadata,
        native_format.as_deref(),
        limits,
        pending(),
    )
    .await
    .map_err(|error| backend_error("ReportDefinitionFailed", error))?;
    let source = match read {
        ReportDefinitionRead::Completed(source) => source,
        ReportDefinitionRead::Accepted(read) => {
            let DefinitionRead::Accepted { operation_id } = &read else {
                unreachable!("Report acceptance retains shared acceptance")
            };
            if !options.wait {
                let mut payload = json_payload(json!({
                    "status":"accepted",
                    "handle":format!("/operations/{operation_id}"),
                    "completed":false
                }));
                payload.data["meta"]["outcome"] = json!("accepted");
                payload.complete = false;
                return Ok(payload);
            }
            let handle = read.accepted_handle().expect("validated accepted handle");
            let observation = observe(
                client,
                budget,
                &handle,
                observer_options(options, true, remaining()?),
                pending(),
            )
            .await
            .map_err(|error| backend_error("ReportObservationFailed", error))?;
            let result = observation.result.ok_or_else(|| {
                backend_error("ReportResultInvalid", reports::ReportError::InvalidResponse)
            })?;
            let read = DefinitionRead::from_completed_result(&result.bytes, limits)
                .map_err(|error| backend_error("ReportResultInvalid", error))?;
            ReportDefinition::from_read(read, native_format.as_deref())
                .map_err(|error| backend_error("ReportResultInvalid", error))?
        }
    };
    let mut payload = if let Some(path) = part {
        let bytes = source
            .part_bytes(path)
            .map_err(|error| backend_error("ReportPartFailed", error))?;
        let media = source
            .evidence()
            .parts
            .iter()
            .find(|part| &part.path == path)
            .and_then(|part| part.media_type.clone());
        byte_payload(bytes, media)
    } else {
        json_payload(source.data().clone())
    };
    payload.data["meta"]["reportSource"] = serde_json::to_value(source.evidence())
        .map_err(|error| backend_error("ReportEvidenceInvalid", error))?;
    if !source.complete() {
        payload.complete = false;
        payload.data["meta"]["outcome"] = json!("partial");
    }
    Ok(payload)
}
