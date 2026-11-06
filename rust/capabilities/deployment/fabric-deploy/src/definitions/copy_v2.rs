// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded create-only definition copy over a caller-owned operation session.
//!
//! The caller selects a non-existing destination and owns discovery, collision
//! policy and sensitivity-label confirmation. The outgoing CREATE omits the
//! optional root `.platform` part so source Git identity and metadata are not
//! imposed on the new item. Raw source and all other parts remain unchanged;
//! dependencies are not remapped, and service labels or data are not copied.
//!
//! See the [optional Notebook platform part](https://learn.microsoft.com/rest/api/fabric/articles/item-management/definitions/notebook-definition#platform-part)
//! and [SemanticModel definition parts](https://learn.microsoft.com/rest/api/fabric/articles/item-management/definitions/semantic-model-definition).

use super::{
    DefinitionErrorKind, DefinitionLimits, DefinitionRead, copy_payload, validate_id,
    validate_part_path,
};
use fabric_core_json::json_dumps;
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutor, OperationResponse,
    OperationSession, OperationWarning,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::Value;
use std::{fmt, io};

/// Explicit non-existing destination selected by the caller, never an upsert.
#[derive(Clone, Copy, Debug)]
pub struct CopyDefinitionDestination<'a> {
    /// Validated destination workspace GUID.
    pub workspace_id: &'a str,
    /// Destination display name, not a local or Fabric path.
    pub display_name: &'a str,
    /// Exact `Notebook` or `SemanticModel` type, matching the source.
    pub item_type: &'a str,
    /// Optional destination folder GUID; absence is serialized as null.
    pub folder_id: Option<&'a str>,
}

/// Bounded copy failure classification, without definition or token content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefinitionCopyErrorKind {
    /// Source or destination type is outside the two supported definition types.
    UnsupportedType,
    /// Source metadata, source type and destination type disagree.
    TypeMismatch,
    /// A caller-provided identifier or destination name is invalid.
    InvalidInput,
    /// Source definition access was denied; metadata alone cannot be copied.
    DefinitionAccessDenied(u16),
    /// Definition or part structure is missing or malformed.
    InvalidDefinition,
    /// A finite request, part, path or depth bound was exceeded.
    Limit,
    /// The shared operation engine failed; inspect the original operation error.
    Operation,
    /// A returned status is neither created nor accepted.
    UnexpectedStatus(u16),
    /// The created response has missing, malformed or conflicting identity.
    InvalidResponse,
    /// Acceptance has no usable operation identity and is not completion.
    AcceptedWithoutOperation,
}

/// Original failure evidence; cancellation and errors never imply rollback.
///
/// The shared session does not expose raw responses on its own error paths.
/// `response` is therefore populated only when the session returned a response;
/// it is never reconstructed from parsed service error data.
pub struct DefinitionCopyError {
    /// Explicit validation, response or shared-operation classification.
    pub kind: DefinitionCopyErrorKind,
    /// Original response when returned by the shared session, including failures.
    pub response: Option<Box<OperationResponse>>,
    /// Original shared error, or session evidence for an invalid returned response.
    pub operation: Option<Box<OperationError>>,
    /// Shared polling-policy warnings observed before the failure.
    pub warnings: Vec<OperationWarning>,
}

impl fmt::Debug for DefinitionCopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DefinitionCopyError")
            .field("kind", &self.kind)
            .field("response_status", &self.response.as_ref().map(|r| r.status))
            .field("operation", &self.operation.as_ref().map(|_| "<evidence>"))
            .field("warnings", &self.warnings)
            .finish()
    }
}

impl fmt::Display for DefinitionCopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "Definition copy failed or is incomplete; inspect response and operation evidence",
        )
    }
}

impl std::error::Error for DefinitionCopyError {}

impl From<DefinitionCopyErrorKind> for DefinitionCopyError {
    fn from(kind: DefinitionCopyErrorKind) -> Self {
        Self {
            kind,
            response: None,
            operation: None,
            warnings: Vec::new(),
        }
    }
}

/// Actual create response, not an assertion that acceptance completed.
///
/// Status 201 has a validated `item_id`. Status 202 has an `operation_id` and no
/// completed item ID; the caller must reconcile it without replaying creation.
pub struct DefinitionCopyReceipt {
    /// Original bounded response, including exact bytes and headers.
    pub response: OperationResponse,
    /// Validated new item GUID only for a created response.
    pub item_id: Option<String>,
    /// Shared-session accepted operation GUID, when known.
    pub operation_id: Option<String>,
    /// Original shared operation-policy warnings.
    pub warnings: Vec<OperationWarning>,
    /// The outgoing CREATE intentionally omitted the source root `.platform`.
    ///
    /// This records request policy, not successful provisioning or dependency
    /// remapping. The raw source definition is never changed.
    pub platform_omitted: bool,
}

impl fmt::Debug for DefinitionCopyReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DefinitionCopyReceipt")
            .field("response_status", &self.response.status)
            .field("item_id", &self.item_id)
            .field("operation_id", &self.operation_id)
            .field("warnings", &self.warnings)
            .field("platform_omitted", &self.platform_omitted)
            .finish()
    }
}

/// Validated create-only request consumed once, with no update/delete fallback.
pub struct PreparedDefinitionCopy {
    path: String,
    body: Vec<u8>,
    workspace_id: String,
    item_type: String,
    platform_omitted: bool,
}

impl fmt::Debug for PreparedDefinitionCopy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedDefinitionCopy")
            .field("body", &"<definition content>")
            .field("platform_omitted", &self.platform_omitted)
            .finish_non_exhaustive()
    }
}

impl PreparedDefinitionCopy {
    /// Dispatch exactly one create using the caller's fresh destination session.
    ///
    /// The source `getDefinition` POST consumes its session's mutation allowance.
    /// Supply a fresh session with the same endpoint, context and latched
    /// cancellation. This method never polls, updates, deletes or retries.
    ///
    /// # Errors
    /// Preserves the shared error and any returned response. Bare acceptance
    /// without an operation ID and malformed created identities remain uncertain
    /// failures, never completion or authorization to retry.
    pub async fn execute<E: OperationExecutor>(
        self,
        session: &mut OperationSession<'_, E>,
    ) -> Result<DefinitionCopyReceipt, DefinitionCopyError> {
        let operation = FabricOperation::new(
            ResourceMethod::Post,
            &self.path,
            self.body,
            CompletionPolicy::ReturnAccepted,
        )
        .map_err(|kind| DefinitionCopyError {
            kind: DefinitionCopyErrorKind::Operation,
            response: None,
            operation: Some(Box::new(session.evidence(kind))),
            warnings: session.warnings().to_vec(),
        })?;
        let response =
            session
                .execute(operation)
                .await
                .map_err(|operation| DefinitionCopyError {
                    kind: DefinitionCopyErrorKind::Operation,
                    response: None,
                    operation: Some(Box::new(operation)),
                    warnings: session.warnings().to_vec(),
                })?;
        let operation_id = session.operation_id().map(str::to_owned);
        let item_id = match response.status {
            201 => match created_id(&response.body, &self.workspace_id, &self.item_type) {
                Some(id) => Some(id),
                None => {
                    return Err(response_error(
                        DefinitionCopyErrorKind::InvalidResponse,
                        response,
                        session,
                    ));
                }
            },
            202 if operation_id.is_some() => None,
            202 => {
                return Err(response_error(
                    DefinitionCopyErrorKind::AcceptedWithoutOperation,
                    response,
                    session,
                ));
            }
            status => {
                return Err(response_error(
                    DefinitionCopyErrorKind::UnexpectedStatus(status),
                    response,
                    session,
                ));
            }
        };
        Ok(DefinitionCopyReceipt {
            response,
            item_id,
            operation_id,
            warnings: session.warnings().to_vec(),
            platform_omitted: self.platform_omitted,
        })
    }
}

/// Prepare one raw Notebook or SemanticModel definition for creation only.
///
/// Original input is validated and bounded before omitting the optional root
/// `.platform` from the outgoing CREATE. Source metadata must not impose its Git
/// logical identity or override the selected destination name. The raw source,
/// other definition fields and every other part remain unchanged; no bytes are
/// decoded, cleaned or dependency-remapped. The host must have selected a
/// non-existing destination and obtained any required warning confirmation.
/// This API performs no discovery or service label propagation.
///
/// # Errors
/// Rejects unsupported/mismatched types, invalid GUIDs/names, missing or denied
/// definitions, malformed parts and finite bounds before any mutation or token
/// acquisition. Source metadata IDs are validated when supplied.
pub fn prepare_definition_copy(
    source: &DefinitionRead,
    source_type: &str,
    destination: CopyDefinitionDestination<'_>,
    limits: DefinitionLimits,
) -> Result<PreparedDefinitionCopy, DefinitionCopyError> {
    use DefinitionCopyErrorKind as Kind;

    for item_type in [source_type, destination.item_type] {
        if !matches!(item_type, "Notebook" | "SemanticModel") {
            return Err(Kind::UnsupportedType.into());
        }
    }
    if source_type != destination.item_type {
        return Err(Kind::TypeMismatch.into());
    }
    validate_id(destination.workspace_id).map_err(|_| Kind::InvalidInput)?;
    if let Some(folder) = destination.folder_id {
        validate_id(folder).map_err(|_| Kind::InvalidInput)?;
    }
    let name = destination.display_name;
    if name.trim().is_empty() || name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return Err(Kind::InvalidInput.into());
    }
    if name.len() > limits.max_path_bytes.get() {
        return Err(Kind::Limit.into());
    }
    if let Some(status) = source.definition_access_denied {
        return Err(Kind::DefinitionAccessDenied(status).into());
    }
    let metadata = source.item.as_object().ok_or(Kind::InvalidDefinition)?;
    if let Some(item_type) = metadata.get("type")
        && item_type.as_str() != Some(source_type)
    {
        return Err(Kind::TypeMismatch.into());
    }
    for field in ["id", "workspaceId"] {
        if let Some(value) = metadata.get(field) {
            let id = value.as_str().ok_or(Kind::InvalidInput)?;
            validate_id(id).map_err(|_| Kind::InvalidInput)?;
        }
    }
    let definition = metadata
        .get("definition")
        .and_then(Value::as_object)
        .ok_or(Kind::InvalidDefinition)?;
    if let Some(format) = definition.get("format")
        && !format.is_string()
    {
        return Err(Kind::InvalidDefinition.into());
    }
    let parts = definition
        .get("parts")
        .and_then(Value::as_array)
        .ok_or(Kind::InvalidDefinition)?;
    if parts.is_empty() {
        return Err(Kind::InvalidDefinition.into());
    }
    if parts.len() > limits.max_files.get() {
        return Err(Kind::Limit.into());
    }
    for part in parts {
        let part = part.as_object().ok_or(Kind::InvalidDefinition)?;
        let path = part
            .get("path")
            .and_then(Value::as_str)
            .ok_or(Kind::InvalidDefinition)?;
        if path.len() > limits.max_path_bytes.get() {
            return Err(Kind::Limit.into());
        }
        validate_part_path(path, limits).map_err(|kind| match kind {
            DefinitionErrorKind::Limit => Kind::Limit,
            _ => Kind::InvalidDefinition,
        })?;
        if !part.get("payload").is_some_and(Value::is_string)
            || !part
                .get("payloadType")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        {
            return Err(Kind::InvalidDefinition.into());
        }
    }
    if let Some(description) = metadata.get("description")
        && !matches!(description, Value::String(_) | Value::Null)
    {
        return Err(Kind::InvalidDefinition.into());
    }
    // Bound the input before copy_payload clones it or the ASCII encoder expands it.
    let mut budget = ByteBudget(limits.max_bytes.get());
    serde_json::to_writer(&mut budget, &source.item["definition"]).map_err(|_| Kind::Limit)?;
    if let Some(description) = metadata.get("description") {
        serde_json::to_writer(&mut budget, description).map_err(|_| Kind::Limit)?;
    }
    let mut payload = copy_payload(source, destination.item_type, name, destination.folder_id)
        .map_err(|_| Kind::InvalidDefinition)?;
    let parts = payload
        .pointer_mut("/definition/parts")
        .and_then(Value::as_array_mut)
        .ok_or(Kind::InvalidDefinition)?;
    let original_count = parts.len();
    parts.retain(|part| part.get("path").and_then(Value::as_str) != Some(".platform"));
    let platform_omitted = parts.len() != original_count;
    if parts.is_empty() {
        return Err(Kind::InvalidDefinition.into());
    }
    let body = json_dumps(&payload).into_bytes();
    if body.len() > limits.max_bytes.get() {
        return Err(Kind::Limit.into());
    }
    Ok(PreparedDefinitionCopy {
        path: format!("/v1/workspaces/{}/items", destination.workspace_id),
        body,
        workspace_id: destination.workspace_id.to_owned(),
        item_type: destination.item_type.to_owned(),
        platform_omitted,
    })
}

fn created_id(body: &[u8], workspace_id: &str, item_type: &str) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object()?;
    let id = object.get("id")?.as_str()?;
    validate_id(id).ok()?;
    if let Some(actual) = object.get("type")
        && actual.as_str() != Some(item_type)
    {
        return None;
    }
    if let Some(actual) = object.get("workspaceId") {
        let actual = actual.as_str()?;
        validate_id(actual).ok()?;
        if !actual.eq_ignore_ascii_case(workspace_id) {
            return None;
        }
    }
    Some(id.to_owned())
}

fn response_error<E: OperationExecutor>(
    kind: DefinitionCopyErrorKind,
    response: OperationResponse,
    session: &OperationSession<'_, E>,
) -> DefinitionCopyError {
    DefinitionCopyError {
        kind,
        response: Some(Box::new(response)),
        operation: Some(Box::new(
            session.evidence(OperationErrorKind::InvalidResponse),
        )),
        warnings: session.warnings().to_vec(),
    }
}

struct ByteBudget(usize);

impl io::Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("definition copy byte bound"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
