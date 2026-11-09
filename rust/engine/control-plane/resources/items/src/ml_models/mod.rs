// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native MLModel item and serving contracts over caller-owned execution.
//!
//! Input files, schema-directed tabular adaptation, output encoders and delivery
//! belong to the shared hosts. This SDK does not invoke a local model, infer a
//! registry binding, acquire credentials, or install a transport or runtime.

mod payload;
mod requests;

pub use payload::{
    CreateMlModelRequest, DefaultVersionAssignmentBehavior, FormatType, Orientation, ScaleRule,
    ScoreDataRequest, ScoreDataResponse, ScoreOutcome, SensitivityLabelApplyStrategy,
    SensitivityLabelSettings, UpdateEndpointRequest, UpdateEndpointVersionRequest,
    UpdateMlModelRequest,
};
pub use requests::{
    MlModelDeleteMode, MlModelEffect, MlModelIdentity, MlModelIdentityPolicy, MlModelRequest,
    MlModelRequests, MlModelResultKind, MlModelTarget,
};

fn validate_uuid(value: &str) -> Result<(), fabric_sdk_workspaces::mutations::ResourceError> {
    if value.len() != 36
        || !value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(fabric_sdk_workspaces::mutations::ResourceError::input());
    }
    Ok(())
}
