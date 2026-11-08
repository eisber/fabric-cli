// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    ExistsLimits, ExistsObservation, ExistsReport, ExistsRequest, ExistsTarget, MetadataSelector,
    evidence::Evidence,
};
use crate::{
    Client,
    items::{ItemErrorKind, ItemExecutionError, ItemListError, ItemListOptions},
    metadata::{MetadataError, MetadataHost, MetadataLimits, MetadataTarget},
    workspaces::{
        WorkspaceExecutionError, WorkspaceListError, WorkspaceListErrorKind,
        WorkspaceMetadataErrorKind,
    },
};
use fabric_sdk_api::v2_outcome::Outcome;
use fabric_v2_contracts::WorkspaceIdentity;
use serde_json::Value;
use std::{collections::BTreeSet, fmt, future::Future, pin::pin};

#[derive(Debug)]
pub(super) enum ProbeError {
    Metadata(MetadataError),
    Workspace(WorkspaceListError),
    Item(ItemListError),
    InvalidResponse,
    Changed,
    TypeMismatch,
    Cancelled,
    TimedOut,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("metadata existence observation failed or changed")
    }
}
impl std::error::Error for ProbeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Metadata(error) => Some(error),
            Self::Workspace(error) => Some(error),
            Self::Item(error) => Some(error),
            _ => None,
        }
    }
}

pub(super) struct Conclusion {
    state: ExistsObservation,
    reason: &'static str,
    workspace_id: Option<String>,
    item_id: Option<String>,
    item_type: Option<String>,
}

fn conclusion(state: ExistsObservation, reason: &'static str) -> Conclusion {
    Conclusion {
        state,
        reason,
        workspace_id: None,
        item_id: None,
        item_type: None,
    }
}

fn valid_id(value: &str) -> Result<(), ProbeError> {
    WorkspaceIdentity::new(value)
        .map(|_| ())
        .map_err(|_| ProbeError::InvalidResponse)
}

fn unique(ids: BTreeSet<String>) -> Result<String, Conclusion> {
    match ids.len() {
        0 => Err(conclusion(
            ExistsObservation::Unknown,
            "VisibleInventoryMiss",
        )),
        1 => Ok(ids.into_iter().next().expect("one distinct identity")),
        _ => Err(conclusion(ExistsObservation::Ambiguous, "AmbiguousTarget")),
    }
}

pub(super) async fn probe(
    client: &Client,
    executor: &Evidence<'_>,
    request: &ExistsRequest,
    limits: ExistsLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<Conclusion, ProbeError> {
    let mut cancelled = pin!(cancelled);
    let workspace = match request.target() {
        ExistsTarget::Workspace { workspace } | ExistsTarget::Item { workspace, .. } => workspace,
    };
    let workspace_id = match workspace {
        MetadataSelector::Id(id) => id.clone(),
        MetadataSelector::Name(name) => {
            let listing = client
                .list_workspaces(executor, limits.workspaces, cancelled.as_mut())
                .await
                .map_err(ProbeError::Workspace)?;
            let mut ids = BTreeSet::new();
            for candidate in &listing.value {
                valid_id(&candidate.id)?;
                if candidate.display_name.eq_ignore_ascii_case(name)
                    && candidate.workspace_type.eq_ignore_ascii_case("Workspace")
                {
                    ids.insert(candidate.id.to_ascii_lowercase());
                }
            }
            match unique(ids) {
                Ok(id) => id,
                Err(result) => return Ok(result),
            }
        }
    };
    let metadata = MetadataHost {
        client,
        items: executor,
        workspaces: executor,
    };
    let metadata_limits = MetadataLimits {
        max_response_bytes: limits
            .items
            .max_response_bytes
            .min(limits.workspaces.max_response_bytes),
        max_result_bytes: limits.max_output_bytes,
    };
    if matches!(request.target(), ExistsTarget::Workspace { .. }) {
        let read = metadata
            .read(
                MetadataTarget::Workspace {
                    workspace_id: &workspace_id,
                },
                metadata_limits,
                cancelled.as_mut(),
            )
            .await
            .map_err(ProbeError::Metadata)?;
        validate_metadata(read.data(), &workspace_id, None, None)?;
        if let MetadataSelector::Name(name) = workspace
            && !read.data()["displayName"]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(name))
        {
            return Err(ProbeError::Changed);
        }
        let mut result = conclusion(ExistsObservation::Present, "MetadataPresent");
        result.workspace_id = Some(workspace_id);
        return Ok(result);
    }
    let ExistsTarget::Item {
        item, item_type, ..
    } = request.target()
    else {
        unreachable!("workspace handled above")
    };
    let item_id = match item {
        MetadataSelector::Id(id) => id.clone(),
        MetadataSelector::Name(name) => {
            let listing = client
                .list_items(
                    executor,
                    &workspace_id,
                    ItemListOptions {
                        query: vec![("recursive".into(), "false".into())],
                        limits: limits.items,
                    },
                    cancelled.as_mut(),
                )
                .await
                .map_err(ProbeError::Item)?;
            let mut ids = BTreeSet::new();
            for candidate in &listing.value {
                valid_id(&candidate.id)?;
                let kind = candidate.item_type().ok_or(ProbeError::InvalidResponse)?;
                let root = match candidate.additional_fields.get("folderId") {
                    None | Some(Value::Null) => true,
                    Some(Value::String(id)) => id.is_empty(),
                    _ => return Err(ProbeError::InvalidResponse),
                };
                if let Some(owner) = candidate.additional_fields.get("workspaceId")
                    && !owner
                        .as_str()
                        .is_some_and(|id| id.eq_ignore_ascii_case(&workspace_id))
                {
                    return Err(ProbeError::InvalidResponse);
                }
                if root
                    && candidate.display_name.eq_ignore_ascii_case(name)
                    && item_type.as_deref() == Some(kind)
                {
                    ids.insert(candidate.id.to_ascii_lowercase());
                }
            }
            match unique(ids) {
                Ok(id) => id,
                Err(mut result) => {
                    result.workspace_id = Some(workspace_id);
                    return Ok(result);
                }
            }
        }
    };
    let read = metadata
        .read(
            MetadataTarget::Item {
                workspace_id: &workspace_id,
                item_id: &item_id,
            },
            metadata_limits,
            cancelled.as_mut(),
        )
        .await;
    let read = match read {
        Ok(read) => read,
        Err(MetadataError::Item(error)) if matches!(&error.kind, ItemErrorKind::Http { status: 404, code, .. } if code == "ItemNotFound") =>
        {
            if matches!(item, MetadataSelector::Name(_)) {
                return Err(ProbeError::Changed);
            }
            let mut result = conclusion(ExistsObservation::Absent, "AuthoritativeItemNotFound");
            result.workspace_id = Some(workspace_id);
            result.item_id = Some(item_id);
            result.item_type = item_type.clone();
            return Ok(result);
        }
        Err(error) => return Err(ProbeError::Metadata(error)),
    };
    validate_metadata(
        read.data(),
        &item_id,
        Some(&workspace_id),
        item_type.as_deref(),
    )?;
    if let MetadataSelector::Name(name) = item {
        let root = match read.data().get("folderId") {
            None | Some(Value::Null) => true,
            Some(Value::String(id)) => id.is_empty(),
            _ => return Err(ProbeError::InvalidResponse),
        };
        if !root
            || !read.data()["displayName"]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(name))
        {
            return Err(ProbeError::Changed);
        }
    }
    let mut result = conclusion(ExistsObservation::Present, "MetadataPresent");
    result.workspace_id = Some(workspace_id);
    result.item_id = Some(item_id);
    result.item_type = read.data()["type"].as_str().map(str::to_owned);
    Ok(result)
}

fn validate_metadata(
    data: &Value,
    id: &str,
    workspace: Option<&str>,
    expected_type: Option<&str>,
) -> Result<(), ProbeError> {
    if !data["id"]
        .as_str()
        .is_some_and(|returned| returned.eq_ignore_ascii_case(id))
        || !data["displayName"].is_string()
    {
        return Err(ProbeError::InvalidResponse);
    }
    if let Some(workspace) = workspace {
        if data.get("workspaceId").is_some_and(|returned| {
            !returned
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(workspace))
        }) {
            return Err(ProbeError::InvalidResponse);
        }
        let kind = data["type"]
            .as_str()
            .filter(|kind| !kind.is_empty())
            .ok_or(ProbeError::InvalidResponse)?;
        if expected_type.is_some_and(|expected| kind != expected) {
            return Err(ProbeError::TypeMismatch);
        }
    } else if data
        .get("type")
        .is_some_and(|kind| kind.as_str() != Some("Workspace"))
    {
        return Err(ProbeError::TypeMismatch);
    }
    Ok(())
}

pub(super) fn report(
    request: ExistsRequest,
    result: Result<Conclusion, ProbeError>,
    native: Outcome,
    attempts: usize,
) -> ExistsReport {
    let (result, cause) = match result {
        Ok(result) => (result, None),
        Err(error) => {
            let (state, reason) = classify(&error);
            (conclusion(state, reason), Some(error))
        }
    };
    ExistsReport {
        request,
        observation: result.state,
        reason: result.reason,
        workspace_id: result.workspace_id,
        item_id: result.item_id,
        item_type: result.item_type,
        native,
        attempts,
        cause,
        retention_failure: None,
        json: Vec::new(),
    }
}

fn classify(error: &ProbeError) -> (ExistsObservation, &'static str) {
    use ExistsObservation::{Failed, Incomplete, Unknown};
    match error {
        ProbeError::Metadata(MetadataError::Item(error)) => classify_item(&error.kind),
        ProbeError::Metadata(MetadataError::Workspace(error)) => match &error.kind {
            WorkspaceMetadataErrorKind::Read(error) => classify_workspace(error),
            WorkspaceMetadataErrorKind::InvalidIdentifier => (Failed, "InvalidResolvedIdentity"),
        },
        ProbeError::Workspace(error) => classify_workspace(&error.kind),
        ProbeError::Item(error) => classify_item(&error.kind),
        ProbeError::Metadata(MetadataError::BudgetExceeded) => (Incomplete, "BudgetExceeded"),
        ProbeError::Metadata(MetadataError::Encoding) => (Failed, "InvalidResponse"),
        ProbeError::Metadata(MetadataError::Cancelled) | ProbeError::Cancelled => {
            (Unknown, "Cancelled")
        }
        ProbeError::TimedOut => (Unknown, "TimedOut"),
        ProbeError::InvalidResponse => (Failed, "InvalidResponse"),
        ProbeError::Changed => (Unknown, "ChangedDuringProbe"),
        ProbeError::TypeMismatch => (Failed, "TypeMismatch"),
    }
}

fn http(status: u16) -> (ExistsObservation, &'static str) {
    use ExistsObservation::{Denied, Failed, Unknown};
    match status {
        401 => (Failed, "AuthenticationRequired"),
        403 => (Denied, "AccessDenied"),
        404 => (Unknown, "UnverifiedNotFound"),
        _ => (Failed, "ServiceError"),
    }
}

fn classify_item(error: &ItemErrorKind) -> (ExistsObservation, &'static str) {
    use ExistsObservation::{Failed, Incomplete, Unknown};
    match error {
        ItemErrorKind::Http { status, .. } => http(*status),
        ItemErrorKind::Cancelled | ItemErrorKind::Execution(ItemExecutionError::Cancelled) => {
            (Unknown, "Cancelled")
        }
        ItemErrorKind::Execution(ItemExecutionError::TimedOut) => (Unknown, "TimedOut"),
        ItemErrorKind::PageLimit
        | ItemErrorKind::ItemLimit
        | ItemErrorKind::ResponseLimit
        | ItemErrorKind::Execution(ItemExecutionError::ResponseLimit) => {
            (Incomplete, "BudgetExceeded")
        }
        ItemErrorKind::Credential => (Failed, "CredentialFailed"),
        ItemErrorKind::Execution(_) => (Unknown, "ExecutionFailed"),
        ItemErrorKind::InvalidResponse => (Failed, "InvalidResponse"),
        ItemErrorKind::UnsupportedStatus(_) => (Failed, "UnsupportedStatus"),
        ItemErrorKind::InvalidContinuation | ItemErrorKind::RepeatedContinuation => {
            (Incomplete, "InvalidContinuation")
        }
        ItemErrorKind::InvalidAudience | ItemErrorKind::InvalidIdentifier => {
            (Failed, "InvalidResolvedIdentity")
        }
    }
}

fn classify_workspace(error: &WorkspaceListErrorKind) -> (ExistsObservation, &'static str) {
    use ExistsObservation::{Failed, Incomplete, Unknown};
    match error {
        WorkspaceListErrorKind::Http { status, .. } => http(*status),
        WorkspaceListErrorKind::Cancelled
        | WorkspaceListErrorKind::Execution(WorkspaceExecutionError::Cancelled) => {
            (Unknown, "Cancelled")
        }
        WorkspaceListErrorKind::Execution(WorkspaceExecutionError::TimedOut) => {
            (Unknown, "TimedOut")
        }
        WorkspaceListErrorKind::PageLimit
        | WorkspaceListErrorKind::ItemLimit
        | WorkspaceListErrorKind::ResponseLimit
        | WorkspaceListErrorKind::Execution(WorkspaceExecutionError::ResponseLimit) => {
            (Incomplete, "BudgetExceeded")
        }
        WorkspaceListErrorKind::Credential => (Failed, "CredentialFailed"),
        WorkspaceListErrorKind::Execution(_) => (Unknown, "ExecutionFailed"),
        WorkspaceListErrorKind::InvalidResponse => (Failed, "InvalidResponse"),
        WorkspaceListErrorKind::UnsupportedStatus(_) => (Failed, "UnsupportedStatus"),
        WorkspaceListErrorKind::InvalidContinuation
        | WorkspaceListErrorKind::RepeatedContinuation => (Incomplete, "InvalidContinuation"),
        WorkspaceListErrorKind::InvalidAudience => (Failed, "InvalidResolvedIdentity"),
    }
}
