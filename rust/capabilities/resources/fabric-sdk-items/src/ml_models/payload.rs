// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native MLModel JSON schemas, without host input or output conversion.
//!
//! The caller's shared parameter adapter supplies these types; this module never
//! reads files or wraps data in notebook or function execution envelopes.

use fabric_sdk_workspaces::mutations::{ResourceError, ResourceOutcome};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fmt};

/// Native behavior when applying a sensitivity label during item creation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SensitivityLabelApplyStrategy {
    /// Fail creation if the requested label cannot be applied.
    ApplyOrFail,
    /// Ignore a failure to apply the requested label.
    Ignore,
}

/// Native sensitivity label settings, not the generic Fabio label spelling.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SensitivityLabelSettings {
    /// Canonical hyphenated label GUID.
    pub label_id: String,
    /// Explicit strategy; omission leaves the service's default unchanged.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_option"
    )]
    pub sensitivity_label_apply_strategy: Option<SensitivityLabelApplyStrategy>,
}

impl SensitivityLabelSettings {
    /// Validate the label identifier without contacting the service.
    ///
    /// # Errors
    /// Returns a sanitized local input error for an invalid label GUID.
    pub fn validate(&self) -> Result<(), ResourceError> {
        super::validate_uuid(&self.label_id)
    }
}

/// Native MLModel metadata creation; definitions and model weights are excluded.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateMlModelRequest {
    /// Required, nonblank item display name, retained without normalization.
    pub display_name: String,
    /// Optional description with at most 256 Unicode scalar values.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_option"
    )]
    pub description: Option<String>,
    /// Native folder selection: omitted, explicit null, or a hyphenated GUID.
    ///
    /// `None` omits `folderId`, `Some(None)` sends JSON null, and
    /// `Some(Some(id))` sends the supplied GUID. The native create contract uses
    /// the workspace as the folder for both omission and null; the SDK preserves
    /// the caller's choice rather than injecting that service default.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_nullable"
    )]
    pub folder_id: Option<Option<String>>,
    /// Optional native label settings; explicit JSON null is not a clear action.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_option"
    )]
    pub sensitivity_label_settings: Option<SensitivityLabelSettings>,
}

impl CreateMlModelRequest {
    /// Validate metadata before a caller dispatches the creation request.
    ///
    /// # Errors
    /// Rejects blank display names, descriptions over 256 characters and invalid
    /// folder or label GUIDs, without including their contents in the error.
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.display_name.trim().is_empty() {
            return Err(ResourceError::input());
        }
        if let Some(description) = &self.description {
            validate_description(description)?;
        }
        if let Some(Some(folder_id)) = &self.folder_id {
            super::validate_uuid(folder_id)?;
        }
        if let Some(settings) = &self.sensitivity_label_settings {
            settings.validate()?;
        }
        Ok(())
    }
}

/// The native MLModel item patch supports description only, not rename or upload.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateMlModelRequest {
    /// Required description, at most 256 characters; an empty string is valid.
    pub description: String,
}

impl UpdateMlModelRequest {
    /// Validate the native description-only patch.
    ///
    /// # Errors
    /// Rejects descriptions longer than 256 Unicode scalar values.
    pub fn validate(&self) -> Result<(), ResourceError> {
        validate_description(&self.description)
    }
}

/// Native endpoint default-version assignment behavior.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DefaultVersionAssignmentBehavior {
    /// The endpoint uses an explicitly configured default version.
    StaticallyConfigured,
    /// No default version assignment is configured.
    NotConfigured,
}

/// Native endpoint settings patch, independent of registry aliases or stages.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateEndpointRequest {
    /// Optional explicit assignment behavior, with no injected client default.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_option"
    )]
    pub default_version_assignment_behavior: Option<DefaultVersionAssignmentBehavior>,
    /// Optional native version name, retained without cross-field inference.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_option"
    )]
    pub default_version_name: Option<String>,
}

impl UpdateEndpointRequest {
    /// Require at least one documented endpoint property.
    ///
    /// No undocumented relationship between the two properties is imposed;
    /// version existence and endpoint state remain service decisions.
    ///
    /// # Errors
    /// Rejects a patch with both properties omitted.
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.default_version_assignment_behavior.is_none() && self.default_version_name.is_none()
        {
            return Err(ResourceError::input());
        }
        Ok(())
    }
}

/// Native serving-version scaling policy, which can affect capacity use.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ScaleRule {
    /// Keep serving resources running.
    AlwaysOn,
    /// Allow idle serving resources to scale to zero.
    AllowScaleToZero,
}

/// Native serving-version settings patch, not a trained-model replacement.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateEndpointVersionRequest {
    /// Required native scale rule.
    pub scale_rule: ScaleRule,
}

impl UpdateEndpointVersionRequest {
    /// Validate the finite native scale-rule schema without inferring readiness.
    ///
    /// # Errors
    /// The typed enum has no invalid local state; service validation can still
    /// reject updates to a version that is not enabled.
    pub fn validate(&self) -> Result<(), ResourceError> {
        Ok(())
    }
}

/// Native scoring input representation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatType {
    /// A native dataframe representation.
    Dataframe,
}

/// Native dataframe orientation; `record` is deliberately singular.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    /// Split representation.
    Split,
    /// Values representation.
    Values,
    /// Record representation.
    Record,
    /// Index representation.
    Index,
    /// Table representation.
    Table,
}

/// Native scoring parameters supplied by the caller's shared parameter adapter.
///
/// Each input remains arbitrary JSON. Model-specific signatures and service
/// acceptance are separate from this schema; no client defaults, table columns,
/// function parameters or notebook `executionData` are invented here.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScoreDataRequest {
    /// Required native array, retaining null, false, zero and nested JSON values.
    pub inputs: Vec<Value>,
    /// Optional advertised representation; omission is not `dataframe` injection.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_option"
    )]
    pub format_type: Option<FormatType>,
    /// Optional advertised orientation, omitted rather than defaulted or cleared.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_option"
    )]
    pub orientation: Option<Orientation>,
}

impl ScoreDataRequest {
    /// Validate the native parameter shape without guessing a model signature.
    ///
    /// # Errors
    /// All representable values satisfy this local schema. No undocumented
    /// minimum input count, row limit or cross-property requirement is imposed.
    pub fn validate(&self) -> Result<(), ResourceError> {
        Ok(())
    }
}

/// A native score result, preserving predictions and open representation metadata.
///
/// Metadata uses JSON values rather than closed request enums so future service
/// values and explicit null remain observable. This preserves parsed JSON data,
/// not original whitespace, object-key order or numeric token spelling.
///
/// These predictions are not automatically tabular. A host must establish an
/// explicit, schema-bearing representation before using shared tabular output;
/// metadata or an array alone does not authorize heuristic column conversion.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreDataResponse {
    /// Required native predictions, including false, zero, null and empty arrays.
    pub predictions: Vec<Value>,
    /// Native representation metadata; `None` means absent, `Some(Null)` present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub format_type: Option<Value>,
    /// Native orientation metadata, retaining future values and explicit null.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub orientation: Option<Value>,
    /// Future native fields retained as data, never implicitly logged.
    #[serde(flatten)]
    pub additional_fields: BTreeMap<String, Value>,
}

/// Scoring completion versus service acceptance, not interchangeable success.
///
/// Scoring can have remote effects and consume capacity. Invalid completed
/// responses retain mutation uncertainty; they must never trigger replay.
#[derive(Clone, PartialEq)]
pub enum ScoreOutcome {
    /// HTTP 200 with a validated native predictions envelope.
    Completed(ScoreDataResponse),
    /// Acceptance only; predictions must be retrieved by the shared reconciler.
    Accepted {
        /// Shared executor's operation identifier, preserved without invention.
        operation_id: Option<String>,
    },
}

impl ScoreOutcome {
    /// Interpret one shared executor outcome without transport or reconciliation.
    ///
    /// Accepted work never becomes predictions. Only an HTTP 200 completed
    /// response with the required predictions array is completed scoring success.
    /// Local output adaptation or delivery is a subsequent host-owned gate, not
    /// a reason to replay a successfully completed remote score.
    ///
    /// # Errors
    /// Returns sanitized `InvalidResponse` for malformed HTTP 200 score bodies,
    /// or `OperationFailed` for unexpected completed statuses. Both retain the
    /// observed status and `mutation_may_have_occurred = true`, without storing
    /// raw response data in the diagnostic.
    pub fn from_resource(outcome: ResourceOutcome) -> Result<Self, ResourceError> {
        match outcome {
            ResourceOutcome::Accepted { operation_id } => Ok(Self::Accepted { operation_id }),
            ResourceOutcome::Completed { status: 200, data } => {
                let response = data
                    .ok_or_else(|| score_response_error("InvalidResponse", 200))
                    .and_then(|data| {
                        serde_json::from_value(data)
                            .map_err(|_| score_response_error("InvalidResponse", 200))
                    })?;
                Ok(Self::Completed(response))
            }
            ResourceOutcome::Completed { status, .. } => {
                Err(score_response_error("OperationFailed", status))
            }
        }
    }
}

fn score_response_error(code: &str, status: u16) -> ResourceError {
    ResourceError {
        code: code.into(),
        message: "Fabric scoring did not return a valid completed native score response".into(),
        status: Some(status),
        service_error: None,
        raw_service_error: None,
        operation_id: None,
        mutation_may_have_occurred: true,
    }
}

fn non_null_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn present_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn present_value<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

fn validate_description(description: &str) -> Result<(), ResourceError> {
    if description.chars().count() > 256 {
        return Err(ResourceError::input());
    }
    Ok(())
}

macro_rules! redacted_debug {
    ($($kind:ty),+ $(,)?) => {
        $(
            impl fmt::Debug for $kind {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.debug_struct(stringify!($kind)).finish_non_exhaustive()
                }
            }
        )+
    };
}

redacted_debug!(
    SensitivityLabelSettings,
    CreateMlModelRequest,
    UpdateMlModelRequest,
    UpdateEndpointRequest,
    UpdateEndpointVersionRequest,
    ScoreDataRequest,
    ScoreDataResponse,
);

impl fmt::Debug for ScoreOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed(_) => f.debug_tuple("Completed").field(&"<native score>").finish(),
            Self::Accepted { operation_id } => f
                .debug_struct("Accepted")
                .field(
                    "operation_id",
                    &operation_id.as_ref().map(|_| "<operation>"),
                )
                .finish(),
        }
    }
}
