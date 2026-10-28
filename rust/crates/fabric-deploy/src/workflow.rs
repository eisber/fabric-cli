// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Serialized source deployment orchestration over the shared HTTP/LRO engine.

use crate::{
    deployment_config::{ConfigurationError, DeploymentConfig, valid_guid},
    model::invalid,
    notebook::{DeploymentAction, DeploymentOutcome},
    planner::{DeploymentPlan, EngineContext},
    processors::{Processor, mutation_body},
    publish::PublishedItem,
    repository::DeploymentRepository,
};
use fabric_sdk::Client;
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    future::{Future, poll_fn},
    pin::{Pin, pin},
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
    time::Duration,
};

mod families;
mod parameters;
mod unpublish;

/// One confirmed source side effect, retained even if a later operation fails.
#[derive(Clone, Debug)]
pub struct DeploymentEffect {
    /// Actual completed REST method.
    pub method: ResourceMethod,
    /// Validated relative API path.
    pub path: String,
    /// Actual service response status, never fabricated for acceptance.
    pub status: u16,
}

/// Final result of a command-local workflow, including nonfatal source warnings.
#[derive(Debug, Default)]
pub struct WorkflowResult {
    /// Completed create/update identities in execution order.
    pub published: Vec<PublishedItem>,
    /// Confirmed service effects in order, including folder and processor actions.
    pub effects: Vec<DeploymentEffect>,
    /// Source nonfatal failures, including failed orphan deletes.
    pub warnings: Vec<OperationError>,
    /// Source warning/notices whose presentation belongs to the CLI caller.
    pub notices: Vec<String>,
    /// All failed source item publications, including later same-type attempts.
    pub item_failures: Vec<WorkflowFailure>,
}

/// Original failure, independent of presentation.
#[derive(Clone, Debug)]
pub enum WorkflowFailure {
    /// Pure source configuration/planning/processor validation.
    Configuration(ConfigurationError),
    /// Shared operation error, retaining accepted ID and mutation uncertainty.
    Operation(Box<OperationError>),
}

impl From<ConfigurationError> for WorkflowFailure {
    fn from(value: ConfigurationError) -> Self {
        Self::Configuration(value)
    }
}

impl From<OperationError> for WorkflowFailure {
    fn from(value: OperationError) -> Self {
        Self::Operation(Box::new(value))
    }
}

/// Failed workflow retains all completed effects; cancellation is not rollback.
#[derive(Debug)]
pub struct WorkflowError {
    /// Original typed stage/failure.
    pub failure: WorkflowFailure,
    /// Confirmed effects before the failure.
    pub partial: Box<WorkflowResult>,
    /// Current source item, when known.
    pub current_item: Option<String>,
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "Deployment failed or is incomplete; retain partial effects and operation evidence",
        )
    }
}
impl std::error::Error for WorkflowError {}

struct CountedExecutor<'a, E> {
    inner: &'a E,
    requests: &'a AtomicUsize,
    maximum: usize,
}

impl<E: OperationExecutor> OperationExecutor for CountedExecutor<'_, E> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            if self.requests.fetch_add(1, Ordering::Relaxed) >= self.maximum {
                return Err(OperationExecutionError::RetriesExhausted);
            }
            self.inner.execute(request).await
        })
    }
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.inner.wait(delay)
    }
}

struct ServiceIo<'a, E> {
    client: &'a Client,
    executor: &'a E,
    limits: OperationLimits,
    requests: AtomicUsize,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    result: WorkflowResult,
    current: Option<String>,
    evidence: Option<OperationError>,
    headers: BTreeMap<String, String>,
}

impl<E: OperationExecutor> ServiceIo<'_, E> {
    fn error(&self, kind: OperationErrorKind) -> OperationError {
        self.evidence
            .as_ref()
            .map(|error| OperationError {
                kind: kind.clone(),
                ..error.clone()
            })
            .unwrap_or(OperationError {
                kind,
                operation_id: None,
                mutation_may_have_occurred: false,
                raw_service_error: None,
            })
    }

    async fn request(
        &mut self,
        method: ResourceMethod,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, WorkflowFailure> {
        self.request_policy(method, path, body, CompletionPolicy::PythonSynchronous)
            .await
    }

    async fn request_policy(
        &mut self,
        method: ResourceMethod,
        path: &str,
        body: Option<Value>,
        policy: CompletionPolicy,
    ) -> Result<Value, WorkflowFailure> {
        let body = body
            .map(|body| fabric_core_json::json_dumps(&body).into_bytes())
            .unwrap_or_default();
        let operation =
            FabricOperation::new(method, path, body, policy).map_err(|kind| self.error(kind))?;
        let mut session = OperationSession::with_context(
            self.client.context(),
            CountedExecutor {
                inner: self.executor,
                requests: &self.requests,
                maximum: self.limits.max_requests.get(),
            },
            self.limits,
            self.cancelled.as_mut(),
        );
        let response = session.execute(operation).await?;
        self.evidence = Some(session.evidence(OperationErrorKind::InvalidResponse));
        self.headers = response.headers.clone();
        if response.status == 202 && policy != CompletionPolicy::ReturnAccepted {
            return Err(session.evidence(OperationErrorKind::InvalidResponse).into());
        }
        let value = if response.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&response.body)
                .map_err(|_| session.evidence(OperationErrorKind::InvalidResponse))?
        };
        if method != ResourceMethod::Get {
            self.result.effects.push(DeploymentEffect {
                method,
                path: path.into(),
                status: response.status,
            });
        }
        Ok(value)
    }

    async fn list(&mut self, path: &str) -> Result<Vec<Value>, WorkflowFailure> {
        let mut next = path.to_owned();
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        loop {
            if !seen.insert(next.clone()) {
                return Err(self.error(OperationErrorKind::InvalidLink).into());
            }
            let value = self.request(ResourceMethod::Get, &next, None).await?;
            let values = value
                .get("value")
                .and_then(Value::as_array)
                .ok_or_else(|| self.error(OperationErrorKind::InvalidResponse))?;
            if result.len().saturating_add(values.len()) > 100_000 {
                return Err(self.error(OperationErrorKind::Limit).into());
            }
            result.extend(values.iter().cloned());
            let Some(link) = value
                .get("continuationUri")
                .and_then(Value::as_str)
                .or_else(|| {
                    self.headers
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case("continuationUri"))
                        .map(|(_, value)| value.as_str())
                })
            else {
                break;
            };
            let url = self
                .client
                .context()
                .endpoint()
                .base_url()
                .join(link)
                .map_err(|_| self.error(OperationErrorKind::InvalidLink))?;
            self.client
                .context()
                .endpoint()
                .validate_url(&url)
                .map_err(|_| self.error(OperationErrorKind::InvalidLink))?;
            if url.path() != path
                || url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(self.error(OperationErrorKind::InvalidLink).into());
            }
            next = format!(
                "{}{}",
                url.path(),
                url.query()
                    .map(|query| format!("?{query}"))
                    .unwrap_or_default()
            );
        }
        Ok(result)
    }

    async fn wait(&mut self, seconds: u64) -> Result<(), WorkflowFailure> {
        let delay = Duration::from_secs(seconds).min(self.limits.max_poll_delay);
        let mut work = pin!(self.executor.wait(delay));
        let result = poll_fn(|cx| {
            if self.cancelled.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(OperationErrorKind::Cancelled));
            }
            work.as_mut()
                .poll(cx)
                .map(|result| result.map_err(OperationErrorKind::Execution))
        })
        .await;
        result.map_err(|kind| self.error(kind).into())
    }
}

#[derive(Clone)]
struct DeployedItem {
    id: String,
    name: String,
    item_type: String,
    folder_id: String,
}

fn identity_key(item: &crate::model::RepositoryItem) -> String {
    if item.logical_id == crate::transforms::DEFAULT_GUID {
        format!("{}\0{}", item.item_type, item.name)
    } else {
        item.logical_id.clone()
    }
}

fn deployed_items(values: Vec<Value>) -> Result<Vec<DeployedItem>, ConfigurationError> {
    values
        .into_iter()
        .map(|value| {
            Ok(DeployedItem {
                id: value
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| valid_guid(id))
                    .ok_or_else(|| invalid("Invalid deployed item ID"))?
                    .into(),
                name: value
                    .get("displayName")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("Invalid deployed displayName"))?
                    .into(),
                item_type: value
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("Invalid deployed item type"))?
                    .into(),
                folder_id: value
                    .get("folderId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
            })
        })
        .collect()
}

async fn workspace_id<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    config: &DeploymentConfig,
) -> Result<String, WorkflowFailure> {
    if let Some(id) = config
        .workspace
        .get("workspace_id")
        .and_then(Value::as_str)
        .filter(|id| valid_guid(id))
    {
        return Ok(id.into());
    }
    let name = config
        .workspace
        .get("workspace_name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Missing workspace identifier"))?;
    let values = io.list("/v1/workspaces").await?;
    values
        .iter()
        .find(|value| value.get("displayName").and_then(Value::as_str) == Some(name))
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .filter(|id| valid_guid(id))
        .map(str::to_owned)
        .ok_or_else(|| invalid("Workspace display name could not be resolved").into())
}

fn folder_hierarchy(folders: &[Value]) -> Result<HashMap<String, String>, ConfigurationError> {
    let mut result = HashMap::new();
    for folder in folders {
        let id = folder
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_guid(id))
            .ok_or_else(|| invalid("Invalid folder ID"))?;
        let mut current = folder;
        let mut components = Vec::new();
        let mut visited = HashSet::new();
        loop {
            let current_id = current
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Invalid folder ID"))?;
            if !visited.insert(current_id) {
                return Err(invalid("Folder hierarchy contains a cycle"));
            }
            let name = current
                .get("displayName")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Invalid folder name"))?;
            if name.contains(['/', '\\', '\0']) {
                return Err(invalid("Unsafe folder name"));
            }
            components.push(name);
            let Some(parent) = current
                .get("parentFolderId")
                .and_then(Value::as_str)
                .filter(|parent| !parent.is_empty())
            else {
                break;
            };
            let Some(parent) = folders
                .iter()
                .find(|folder| folder.get("id").and_then(Value::as_str) == Some(parent))
            else {
                break;
            };
            current = parent;
        }
        components.reverse();
        if result
            .insert(format!("/{}", components.join("/")), id.into())
            .is_some()
        {
            return Err(invalid("Ambiguous workspace folder path"));
        }
    }
    Ok(result)
}

async fn publish_folders<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    base: &str,
    plan: &DeploymentPlan,
) -> Result<HashMap<String, String>, WorkflowFailure> {
    let mut folders = folder_hierarchy(&io.list(&format!("{base}/folders")).await?)?;
    for path in &plan.folders {
        if folders.contains_key(path) {
            continue;
        }
        let (parent, name) = path
            .rsplit_once('/')
            .ok_or_else(|| invalid("Invalid repository folder path"))?;
        if name.contains([
            '~', '"', '#', '.', '%', '&', '*', ':', '<', '>', '?', '/', '\\', '{', '|', '}',
        ]) {
            return Err(invalid("Folder name contains invalid characters").into());
        }
        let mut body = json!({"displayName": name});
        if let Some(parent) = folders.get(parent) {
            body["parentFolderId"] = parent.clone().into();
        }
        let response = io
            .request(ResourceMethod::Post, &format!("{base}/folders"), Some(body))
            .await?;
        let id = response
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_guid(id))
            .ok_or_else(|| io.error(OperationErrorKind::InvalidResponse))?;
        folders.insert(path.clone(), id.into());
    }
    Ok(folders)
}

/// Execute the complete source-config deployment workflow with injected capabilities.
///
/// Every snapshot and policy is invocation-local. Writes are serialized, attempted
/// once and reconciled through the existing SDK operation engine. No live service,
/// credential discovery, owned runtime, rollback or mutation retry is introduced.
///
/// # Errors
/// Retains typed original failures, actual completed identities and partial effects.
pub async fn deploy_repository<E: OperationExecutor>(
    client: &Client,
    executor: &E,
    config: &DeploymentConfig,
    repository: DeploymentRepository,
    context: EngineContext,
    limits: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<WorkflowResult, WorkflowError> {
    let mut io = ServiceIo {
        client,
        executor,
        limits,
        requests: AtomicUsize::new(0),
        cancelled: Box::pin(cancelled),
        result: WorkflowResult::default(),
        current: None,
        evidence: None,
        headers: BTreeMap::new(),
    };
    match run(&mut io, config, repository, &context).await {
        Ok(()) => Ok(io.result),
        Err(failure) => Err(WorkflowError {
            failure,
            partial: Box::new(io.result),
            current_item: io.current,
        }),
    }
}

async fn run<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    config: &DeploymentConfig,
    repository: DeploymentRepository,
    context: &EngineContext,
) -> Result<(), WorkflowFailure> {
    let plan = DeploymentPlan::prepare(config, context, repository)?;
    parameters::validate(config, context)?;
    let workspace = workspace_id(io, config).await?;
    let base = format!("/v1/workspaces/{workspace}");
    let mut ids = HashMap::new();
    if !config.skip_publish() {
        let metadata = io.request(ResourceMethod::Get, &base, None).await?;
        if !metadata
            .get("capacityId")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
            && !config.item_types.iter().all(|item_type| {
                ["SemanticModel", "Report", "PaginatedReport"].contains(&item_type.as_str())
            })
        {
            return Err(invalid("Workspace does not have an assigned capacity").into());
        }
        let bulk = context.bulk_publish
            && !config
                .item_types
                .iter()
                .any(|item_type| ["Warehouse", "DataBuildToolJob"].contains(&item_type.as_str()))
            && !parameters::has_unfiltered_item_variable(context);
        let folders = if context.enabled("disable_workspace_folder_publish") {
            HashMap::new()
        } else if bulk {
            folder_hierarchy(&io.list(&format!("{base}/folders")).await?)?
        } else {
            publish_folders(io, &base, &plan).await?
        };
        let deployed = deployed_items(io.list(&format!("{base}/items")).await?)?;
        for item in &plan.repository.items {
            let matches: Vec<_> = deployed
                .iter()
                .filter(|target| target.item_type == item.item_type && target.name == item.name)
                .collect();
            if matches.len() > 1 {
                return Err(invalid("Ambiguous deployed item displayName/type").into());
            }
            if let Some(target) = matches.first() {
                ids.insert(identity_key(item), target.id.clone());
            }
        }
        families::publish(
            io,
            families::Publication {
                config,
                context,
                plan: &plan,
                base: &base,
                workspace: &workspace,
                folders: &folders,
                deployed: &deployed,
            },
            &mut ids,
        )
        .await?;
    }
    if config.unpublish.get("skip").and_then(Value::as_bool) != Some(true) {
        unpublish::run(io, config, context, &plan, &base).await?;
    }
    Ok(())
}
