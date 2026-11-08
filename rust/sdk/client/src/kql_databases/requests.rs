// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    KqlDatabase, KqlDatabasePage, KqlError, KqlErrorKind, Observed,
    binding::{checked_identity, validate_context},
};
use crate::{
    RequestContext,
    v2::{ItemIdentity, ResolvedTarget, TargetKind, WorkspaceIdentity},
};
use fabric_sdk_operations::{
    FabricOperation, OperationExecutor, OperationLimits, OperationSession,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceMethod, ResourceOperation, ResourceOutcome,
};
use serde::Deserialize;
use serde_json::Value;
use std::{fmt, future::Future};

/// Explicit native list options; no CLI/default pagination policy is installed.
pub struct KqlListOptions<'a> {
    /// Native recursive choice, always sent explicitly.
    pub recursive: bool,
    /// Optional concrete root folder ID, never inferred from a display name.
    pub root_folder_id: Option<&'a str>,
    /// Actual decoded token or None for the first page.
    pub continuation_token: Option<&'a str>,
}

/// Fixed Fabric metadata request with exact endpoint/context and typed decoder.
///
/// Execution binds the shared policy to `context()`. The descriptor
/// does not create a transport, acquire credentials or poll unexpected 202s.
pub struct KqlRequest<T> {
    operation: ResourceOperation,
    context: RequestContext,
    expected: Expected,
    decoder: fn(Value, &Expected, &RequestContext) -> Result<T, KqlError>,
}

struct Expected {
    workspace: WorkspaceIdentity,
    item: Option<ItemIdentity>,
    query: Vec<(String, String)>,
}

impl<T> KqlRequest<T> {
    /// Fixed GET descriptor, with no user URI or executable body.
    pub fn operation(&self) -> &ResourceOperation {
        &self.operation
    }
    /// Exact Fabric context that must also bind the shared execution policy.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }
    /// Decode an observed shared outcome without follow-up requests.
    ///
    /// # Errors
    /// Rejects undocumented statuses, missing bodies, mismatched owner identity
    /// and malformed fields. Acceptance is retained as evidence, not success.
    pub fn decode(&self, outcome: ResourceOutcome) -> Result<T, KqlError> {
        let (status, data) = match outcome {
            ResourceOutcome::Accepted { operation_id } => {
                return Err(KqlError::Contract {
                    kind: KqlErrorKind::UnexpectedAcceptance,
                    status: Some(202),
                    operation_id,
                });
            }
            ResourceOutcome::Completed { status, data } => (status, data),
        };
        let reject = |kind| KqlError::Contract {
            kind,
            status: Some(status),
            operation_id: None,
        };
        if status != 200 {
            return Err(reject(KqlErrorKind::UnexpectedStatus));
        }
        let data = data.ok_or_else(|| reject(KqlErrorKind::InvalidResponse))?;
        (self.decoder)(data, &self.expected, &self.context).map_err(|error| match error {
            KqlError::Contract {
                kind, operation_id, ..
            } => KqlError::Contract {
                kind,
                status: Some(status),
                operation_id,
            },
            error => error,
        })
    }
    /// Delegate one GET through the existing operation engine and its executor.
    ///
    /// # Errors
    /// Retains original shared failures; never maps denied/network errors to
    /// absence, polls acceptance or selects a fallback endpoint.
    pub async fn execute(
        self,
        executor: impl OperationExecutor,
        limits: OperationLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<T, KqlError> {
        let operation = FabricOperation::from_resource(&self.operation)
            .map_err(|_| KqlError::new(KqlErrorKind::InvalidInput))?;
        let mut session =
            OperationSession::with_context(&self.context, executor, limits, cancelled);
        let response = session
            .execute(operation)
            .await
            .map_err(KqlError::Operation)?;
        if response.headers.iter().any(|(key, media)| {
            key.eq_ignore_ascii_case("content-type")
                && !media
                    .split(';')
                    .next()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        }) {
            return Err(KqlError::new(KqlErrorKind::UnsupportedCapability));
        }
        let data = if response.body.is_empty() {
            None
        } else {
            Some(
                serde_json::from_slice(&response.body)
                    .map_err(|_| KqlError::new(KqlErrorKind::InvalidResponse))?,
            )
        };
        let outcome = ResourceOutcome::Completed {
            status: response.status,
            data,
        };
        self.decode(outcome)
    }
}

impl<T> fmt::Debug for KqlRequest<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KqlRequest(<GET metadata; explicit context>)")
    }
}

/// Native KQLDatabase GET and one-page LIST, never query/control/definition reads.
pub struct KqlDatabaseRequests;

impl KqlDatabaseRequests {
    /// Prepare an exact KQLDatabase item metadata GET.
    ///
    /// # Errors
    /// Rejects wrong owners, children, representations, IDs or Fabric contexts
    /// before credentials, execution or network access.
    pub fn get(
        target: &ResolvedTarget,
        context: &RequestContext,
    ) -> Result<KqlRequest<KqlDatabase>, KqlError> {
        validate_context(context)?;
        let selection = target.selection();
        if selection.kind != TargetKind::Item || selection.owner_type != "KQLDatabase" {
            return Err(KqlError::new(KqlErrorKind::TargetMismatch));
        }
        if selection.facet != "item"
            || selection.representation != "metadata"
            || target.child().is_some()
        {
            return Err(KqlError::new(KqlErrorKind::UnsupportedCapability));
        }
        let identity = checked_identity(
            target
                .identity()
                .workspace_id()
                .ok_or_else(|| KqlError::new(KqlErrorKind::TargetMismatch))?,
            target
                .identity()
                .item_id()
                .ok_or_else(|| KqlError::new(KqlErrorKind::TargetMismatch))?,
        )?;
        let operation = ResourceOperation::new(
            ResourceMethod::Get,
            &[
                "workspaces",
                identity.workspace_id(),
                "kqlDatabases",
                identity.item_id(),
            ],
            None,
            CompletionPolicy::ReturnAccepted,
        )
        .map_err(|_| KqlError::new(KqlErrorKind::InvalidInput))?;
        Ok(KqlRequest {
            operation,
            context: context.clone(),
            expected: Expected {
                workspace: WorkspaceIdentity::new(identity.workspace_id())
                    .map_err(|_| KqlError::new(KqlErrorKind::InvalidInput))?,
                item: Some(identity),
                query: Vec::new(),
            },
            decoder: |data, expected, context| {
                let item = expected
                    .item
                    .as_ref()
                    .ok_or_else(|| KqlError::new(KqlErrorKind::TargetMismatch))?;
                KqlDatabase::decode(data, item, context)
            },
        })
    }

    /// Prepare one native list page with explicit folder, recursion and token.
    ///
    /// # Errors
    /// Rejects invalid coordinates/context/token; returned URI metadata is
    /// checked against the original collection, never used as a new destination.
    pub fn list_page(
        workspace: &WorkspaceIdentity,
        context: &RequestContext,
        options: KqlListOptions<'_>,
    ) -> Result<KqlRequest<KqlDatabasePage>, KqlError> {
        validate_context(context)?;
        if workspace.workspace_id() == "00000000-0000-0000-0000-000000000000" {
            return Err(KqlError::new(KqlErrorKind::InvalidInput));
        }
        let mut operation = ResourceOperation::new(
            ResourceMethod::Get,
            &["workspaces", workspace.workspace_id(), "kqlDatabases"],
            None,
            CompletionPolicy::ReturnAccepted,
        )
        .map_err(|_| KqlError::new(KqlErrorKind::InvalidInput))?;
        let mut query = vec![(
            "recursive",
            if options.recursive { "true" } else { "false" },
        )];
        if let Some(folder) = options.root_folder_id {
            checked_identity(workspace.workspace_id(), folder)?;
            query.push(("rootFolderId", folder));
        }
        if let Some(token) = options.continuation_token {
            if token.is_empty() || token.chars().any(char::is_control) {
                return Err(KqlError::new(KqlErrorKind::InvalidInput));
            }
            query.push(("continuationToken", token));
        }
        operation = operation
            .with_query(&query)
            .map_err(|_| KqlError::new(KqlErrorKind::InvalidInput))?;
        Ok(KqlRequest {
            operation,
            context: context.clone(),
            expected: Expected {
                workspace: workspace.clone(),
                item: None,
                query: query
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect(),
            },
            decoder: decode_page,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageFields {
    value: Vec<Value>,
    #[serde(default)]
    continuation_token: Observed<String>,
    #[serde(default)]
    continuation_uri: Observed<String>,
}

fn decode_page(
    data: Value,
    expected: &Expected,
    context: &RequestContext,
) -> Result<KqlDatabasePage, KqlError> {
    if !data.is_object() {
        return Err(KqlError::new(KqlErrorKind::InvalidResponse));
    }
    let fields: PageFields = serde_json::from_value(data.clone())
        .map_err(|_| KqlError::new(KqlErrorKind::InvalidResponse))?;
    if let Observed::Value(uri) = &fields.continuation_uri
        && !uri.is_empty()
    {
        let path = format!(
            "/v1/workspaces/{}/kqlDatabases",
            expected.workspace.workspace_id()
        );
        let token = match &fields.continuation_token {
            Observed::Value(token) if !token.is_empty() => token,
            _ => return Err(KqlError::new(KqlErrorKind::InvalidContinuation)),
        };
        if uri.chars().any(char::is_control) || token.chars().any(char::is_control) {
            return Err(KqlError::new(KqlErrorKind::InvalidContinuation));
        }
        let base = uri.split_once('?').map_or(uri.as_str(), |(base, _)| base);
        context
            .endpoint()
            .validate_link(base, &path)
            .map_err(|_| KqlError::new(KqlErrorKind::InvalidContinuation))?;
        let parsed = azure_core::http::Url::parse(uri)
            .map_err(|_| KqlError::new(KqlErrorKind::InvalidContinuation))?;
        context
            .endpoint()
            .validate_url(&parsed)
            .map_err(|_| KqlError::new(KqlErrorKind::InvalidContinuation))?;
        let pairs = parsed.query_pairs().collect::<Vec<_>>();
        if pairs
            .iter()
            .filter(|(key, _)| key == "continuationToken")
            .count()
            != 1
            || !pairs
                .iter()
                .any(|(key, value)| key == "continuationToken" && value == token)
            || pairs.iter().any(|(key, value)| {
                key != "continuationToken"
                    && !expected.query.iter().any(|(expected_key, expected_value)| {
                        key == expected_key && value == expected_value
                    })
            })
        {
            return Err(KqlError::new(KqlErrorKind::InvalidContinuation));
        }
    }
    if let Observed::Value(token) = &fields.continuation_token
        && (token.chars().any(char::is_control)
            || expected
                .query
                .iter()
                .any(|(key, value)| key == "continuationToken" && value == token))
    {
        return Err(KqlError::new(KqlErrorKind::InvalidContinuation));
    }
    let value = fields
        .value
        .into_iter()
        .map(|row| {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| KqlError::new(KqlErrorKind::InvalidResponse))?;
            let identity = checked_identity(expected.workspace.workspace_id(), id)?;
            KqlDatabase::decode(row, &identity, context)
        })
        .collect::<Result<_, _>>()?;
    Ok(KqlDatabasePage {
        value,
        continuation_token: fields.continuation_token,
        continuation_uri: fields.continuation_uri,
        data,
    })
}
