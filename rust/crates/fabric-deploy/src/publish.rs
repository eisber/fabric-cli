// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Repository planning and sequential native Notebook publication.

use crate::{
    deployment_config::{ConfigurationError, DeploymentConfig, valid_guid},
    notebook::{DeploymentError, DeploymentOutcome, PreparedNotebook},
    repository::NotebookRepository,
    transforms::NotebookTransforms,
};
use fabric_sdk::{
    Client,
    items::{Item, ItemExecutor, ItemListError, ItemListOptions},
};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::Value;
use std::{
    fmt,
    future::Future,
    pin::{Pin, pin},
    time::Duration,
};

/// Completed item identity retained across later failures and output delivery.
#[derive(Clone, Debug)]
pub struct PublishedItem {
    /// Exact source display name.
    pub name: String,
    /// Source logical identity.
    pub logical_id: String,
    /// Actual completed create/update and deployed ID.
    pub outcome: DeploymentOutcome,
}

/// Failure stage, including original SDK reconciliation evidence where available.
#[derive(Debug)]
pub enum PublishFailure {
    /// Preflight configuration/processor branch.
    Configuration(ConfigurationError),
    /// Complete discovery failed; no absent-item selection is safe.
    Discovery(Box<ItemListError>),
    /// Existing native preparation/transform failure.
    Notebook(DeploymentError),
    /// Shared request/operation failure with original uncertainty.
    Operation(Box<OperationError>),
}

/// Ordered partial deployment outcome; cancellation is not rollback.
#[derive(Debug)]
pub struct PublishError {
    /// Stage classification, not a string-only projection.
    pub failure: PublishFailure,
    /// Items confirmed complete before failure; never automatically deleted.
    pub completed: Vec<PublishedItem>,
    /// Current source item when mutation/preparation failed.
    pub current_item: Option<String>,
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "Deployment failed or is incomplete; retain completed items and operation evidence",
        )
    }
}

impl std::error::Error for PublishError {}

struct PlannedNotebook {
    name: String,
    logical_id: String,
    existing_id: Option<String>,
    prepared: PreparedNotebook,
}

/// Fully preflighted native plan, consumed once rather than replayed on failure.
pub struct NotebookPublishPlan {
    items: Vec<PlannedNotebook>,
}

impl NotebookPublishPlan {
    /// Select targets only after the caller has completed every bounded page.
    ///
    /// Existing folder placement requiring a move remains explicit unsupported.
    /// All local transforms validate before the first write; unresolved logical
    /// references are errors, never dummy IDs or success-shaped skipped items.
    ///
    /// # Errors
    /// Rejects ambiguous/invalid targets and unsupported source branches.
    pub fn prepare(
        config: &DeploymentConfig,
        repository: NotebookRepository,
        deployed: &[Item],
    ) -> Result<Self, PublishError> {
        let failure = |failure| PublishError {
            failure,
            completed: Vec::new(),
            current_item: None,
        };
        config
            .require_notebook_publish()
            .map_err(|error| failure(PublishFailure::Configuration(error)))?;
        let workspace = config
            .workspace
            .get("workspace_id")
            .and_then(Value::as_str)
            .filter(|id| valid_guid(id))
            .ok_or_else(|| {
                failure(PublishFailure::Configuration(ConfigurationError::Invalid(
                    vec!["Invalid workspace ID".into()],
                )))
            })?;
        let mut selected = Vec::new();
        let mut logical_ids = Vec::new();
        for notebook in &repository.notebooks {
            let mut matching = deployed.iter().filter(|item| {
                item.item_type() == Some("Notebook") && item.display_name == notebook.name()
            });
            let existing = matching.next();
            if matching.next().is_some() {
                return Err(failure(PublishFailure::Notebook(
                    DeploymentError::AmbiguousTarget,
                )));
            }
            if let Some(item) = existing {
                if !valid_guid(&item.id) {
                    return Err(failure(PublishFailure::Configuration(
                        ConfigurationError::Invalid(vec!["Invalid deployed Notebook ID".into()]),
                    )));
                }
                if item
                    .additional_fields
                    .get("folderId")
                    .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
                {
                    return Err(failure(PublishFailure::Configuration(
                        ConfigurationError::Unsupported("existing Notebook folder move".into()),
                    )));
                }
            }
            selected.push(existing.map(|item| item.id.clone()));
            logical_ids.push((
                notebook.logical_id().to_owned(),
                existing.map_or_else(String::new, |item| item.id.clone()),
            ));
        }
        let transforms = NotebookTransforms {
            logical_ids,
            literals: config
                .parameters()
                .map_or_else(Vec::new, |parameters| parameters.literals().to_vec()),
        };
        let mut items = Vec::new();
        for (notebook, existing_id) in repository.notebooks.into_iter().zip(selected) {
            let name = notebook.name().to_owned();
            let prepared =
                notebook
                    .prepare(workspace, &transforms)
                    .map_err(|error| PublishError {
                        failure: PublishFailure::Notebook(error),
                        completed: Vec::new(),
                        current_item: Some(name.clone()),
                    })?;
            items.push(PlannedNotebook {
                name,
                logical_id: notebook.logical_id().to_owned(),
                existing_id,
                prepared,
            });
        }
        Ok(Self { items })
    }

    /// Execute sequentially through caller-supplied shared operation sessions.
    ///
    /// # Errors
    /// Retains actual completed IDs, current item, accepted operation ID and
    /// mutation uncertainty. No automatic replay, rollback or deletion occurs.
    pub async fn execute<E: OperationExecutor>(
        self,
        client: &Client,
        executor: &E,
        limits: OperationLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Vec<PublishedItem>, PublishError> {
        let mut cancelled = pin!(cancelled);
        let mut completed = Vec::new();
        for item in self.items {
            let mut session = OperationSession::new(
                client.credential().as_ref(),
                client.selection(),
                BorrowedExecutor(executor),
                limits,
                cancelled.as_mut(),
            );
            match item
                .prepared
                .publish_with_session(&mut session, item.existing_id.as_deref())
                .await
            {
                Ok(outcome) => completed.push(PublishedItem {
                    name: item.name,
                    logical_id: item.logical_id,
                    outcome,
                }),
                Err(error) => {
                    return Err(PublishError {
                        failure: PublishFailure::Operation(Box::new(error)),
                        completed,
                        current_item: Some(item.name),
                    });
                }
            }
        }
        Ok(completed)
    }
}

struct BorrowedExecutor<'a, E>(&'a E);

impl<E: OperationExecutor> OperationExecutor for BorrowedExecutor<'_, E> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.0.execute(request)
    }
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.0.wait(delay)
    }
}

/// Check source capacity policy, complete discovery, preflight, then publish.
///
/// The executor is the caller's existing HTTP adapter for both operations and
/// paging. This installs no HTTP/retry/polling/runtime/credential implementation.
///
/// # Errors
/// Fails explicitly before writes for unsupported processors and local plans;
/// service failures preserve original evidence and completed item outcomes.
pub async fn publish_repository<E: OperationExecutor + ItemExecutor>(
    client: &Client,
    executor: &E,
    config: &DeploymentConfig,
    repository: NotebookRepository,
    limits: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<Vec<PublishedItem>, PublishError> {
    let failure = |failure| PublishError {
        failure,
        completed: Vec::new(),
        current_item: None,
    };
    config
        .require_notebook_publish()
        .map_err(|error| failure(PublishFailure::Configuration(error)))?;
    if config.skip_publish() {
        return Ok(Vec::new());
    }
    let workspace = config
        .workspace
        .get("workspace_id")
        .and_then(Value::as_str)
        .filter(|id| valid_guid(id))
        .ok_or_else(|| {
            failure(PublishFailure::Configuration(ConfigurationError::Invalid(
                vec!["Invalid workspace ID".into()],
            )))
        })?;
    let mut cancelled = pin!(cancelled);
    {
        let mut session = OperationSession::new(
            client.credential().as_ref(),
            client.selection(),
            BorrowedExecutor(executor),
            limits,
            cancelled.as_mut(),
        );
        let operation = FabricOperation::new(
            ResourceMethod::Get,
            &format!("/v1/workspaces/{workspace}"),
            Vec::new(),
            CompletionPolicy::PythonSynchronous,
        )
        .map_err(|kind| failure(PublishFailure::Operation(Box::new(session.evidence(kind)))))?;
        let response = session
            .execute(operation)
            .await
            .map_err(|error| failure(PublishFailure::Operation(Box::new(error))))?;
        let metadata: Value = serde_json::from_slice(&response.body).map_err(|_| {
            failure(PublishFailure::Operation(Box::new(
                session.evidence(OperationErrorKind::InvalidResponse),
            )))
        })?;
        if !metadata
            .get("capacityId")
            .is_some_and(|value| value.as_str().is_some_and(|id| !id.is_empty()))
        {
            return Err(failure(PublishFailure::Configuration(
                ConfigurationError::Invalid(vec![
                    "Workspace does not have an assigned capacity".into(),
                ]),
            )));
        }
    }
    let deployed = client
        .list_items(
            executor,
            workspace,
            ItemListOptions::default(),
            cancelled.as_mut(),
        )
        .await
        .map_err(|error| failure(PublishFailure::Discovery(Box::new(error))))?;
    let plan = NotebookPublishPlan::prepare(config, repository, &deployed.value)?;
    plan.execute(client, executor, limits, cancelled.as_mut())
        .await
}
