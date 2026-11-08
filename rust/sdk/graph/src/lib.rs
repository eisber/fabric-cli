// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One explicit GraphModel runtime refresh with scoped approval and observation.
//!
//! Reuses job submission and the existing neutral observer. This is not universal
//! artifact refresh, a model/catalog refresh, or a graph query/readiness probe.

pub use fabric_sdk::v2;
use fabric_sdk::{
    Client,
    observe::{self, Observation, ObserveError, ObserveOptions, ReadHandle},
    v2::{
        ApprovedOperation, Availability, Binding, Effect, Generation, ItemIdentity,
        OperationDescriptor, Precondition, PreparedOperation, Prerequisite, Registry,
        ResolvedTarget, Target, TargetKind, Verb,
    },
};
use fabric_sdk_jobs::{
    JobClient, JobError,
    graph_refresh::{self, GraphTarget, RefreshError, RefreshReceipt},
};
use fabric_sdk_operations::{OperationExecutionError, OperationExecutor};
use serde_json::{Value, json};
use std::{fmt, future::Future, pin::pin, sync::Arc};

/// Stable identity shared by SDK, CLI and future consumers.
pub const OPERATION_ID: &str = "graph.runtime.refresh";

/// The complete effect set; registration never grants execution permission.
pub const EFFECTS: &[Effect] = &[
    Effect::RemoteWrite,
    Effect::ModelExecution,
    Effect::MeteredConsumption,
];

/// Pure registration of the single supported native facet and its schemas.
///
/// # Errors
/// Returns explicit registration failures rather than an incomplete registry.
pub fn registry() -> Result<Registry, crate::v2::ContractError> {
    let mut registry = Registry::default();
    registry.register(OperationDescriptor::new(
        OPERATION_ID,
        Verb::Refresh,
        json!({"type":"object","additionalProperties":false,"properties":{}}),
        json!({
            "type":"object","additionalProperties":false,
            "description":"Native graph acceptance or observed job state; no query data or generic LRO result.",
            "required":["schemaVersion","status","data","meta","error"],
            "properties":{
                "schemaVersion":{"const":"0.1"},
                "status":{"enum":["accepted","completed","failed","unknown"]},
                "data":{"type":"null"},
                "meta":{
                    "type":"object",
                    "required":["operation","target","handle","responseStatus","accepted","mutationMayHaveOccurred","effects","effectsCompleteness","remoteCancelled","remoteReplayed"],
                    "properties":{
                        "operation":{"const":OPERATION_ID},
                        "target":{"type":"string"},
                        "handle":{"type":["object","null"]},
                        "responseStatus":{"type":["integer","null"]},
                        "accepted":{"type":"boolean"},
                        "mutationMayHaveOccurred":{"type":"boolean"},
                        "effects":{"type":"array"},
                        "effectsCompleteness":{"const":"unknown"},
                        "remoteCancelled":{"const":false},
                        "remoteReplayed":{"const":false},
                        "statusRequests":{"type":"integer","minimum":0},
                        "lastState":{"type":"string"},
                        "responseBodyOmitted":{"type":"boolean"},
                        "interruption":{"enum":["TimedOut","Cancelled"]}
                    }
                },
                "error":{"type":["object","null"]}
            }
        }),
        EFFECTS.to_vec(),
    )?)?;
    registry.bind(Binding::new(
        "GraphModel", "runtime", "graph", Generation::Any, OPERATION_ID,
        Availability::Conditional(vec![Prerequisite {
            id: "graph-preview".into(),
            description: "Explicitly approve Preview graph refresh and metered execution with the selected Fabric identity; Item.Execute.All and native graph prerequisites must hold.".into(),
        }]),
    )?)?;
    Ok(registry)
}

/// Validate an ID-only runtime/graph path without resolving names or doing I/O.
///
/// # Errors
/// Rejects every other type/facet, malformed ID, query, representation or payload.
pub fn target(path: &str) -> Result<GraphTarget, JobError> {
    let parts: Vec<_> = path.split('/').collect();
    let ["", workspace, graph, "runtime", "graph"] = parts.as_slice() else {
        return Err(JobError::InvalidIdentifier);
    };
    let workspace = workspace
        .strip_suffix(".Workspace")
        .ok_or(JobError::InvalidIdentifier)?;
    let graph = graph
        .strip_suffix(".GraphModel")
        .ok_or(JobError::InvalidIdentifier)?;
    GraphTarget::new(workspace, graph)
}

/// Prepare an exact graph request; the only native payload is absent.
///
/// # Errors
/// Fails before credentials for unsupported targets or invalid contract evidence.
pub fn prepare(path: &str) -> Result<PreparedOperation, JobError> {
    let graph = target(path)?;
    let registry = registry().map_err(|_| JobError::InvalidIdentifier)?;
    let selection = Target {
        kind: TargetKind::Item,
        owner_type: "GraphModel".into(),
        facet: "runtime".into(),
        representation: "graph".into(),
        generation: None,
    };
    let resolved = ResolvedTarget::new(
        path,
        ItemIdentity::new(graph.workspace_id(), graph.graph_id())
            .map_err(|_| JobError::InvalidIdentifier)?,
        selection.clone(),
        None,
    )
    .map_err(|_| JobError::InvalidIdentifier)?;
    let resolution = registry
        .resolve(Verb::Refresh, &selection, None)
        .map_err(|_| JobError::InvalidIdentifier)?;
    PreparedOperation::new(resolution, resolved, json!({}))
        .and_then(|prepared| prepared.with_preconditions(vec![Precondition::UnguardedConcurrency]))
        .map_err(|_| JobError::InvalidIdentifier)
}

/// Successful submission, optionally with service-observed completion.
pub struct GraphRefresh {
    /// Actual submission and, for 202, validated scoped job handle.
    pub submission: RefreshReceipt,
    /// Latest neutral observation only when explicitly requested.
    pub observation: Option<Observation>,
}

impl fmt::Debug for GraphRefresh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphRefresh")
            .field("submission", &self.submission)
            .field("observation", &self.observation)
            .finish()
    }
}

/// Failed refresh retains acceptance even when local observation later fails.
pub struct GraphRefreshError {
    /// Safe SDK/observation classification.
    pub source: GraphRefreshErrorKind,
    /// Already accepted work, never lost on timeout/cancellation.
    pub submission: Option<RefreshReceipt>,
}

/// Original failure domain, with no invented shared run foundation.
#[derive(Debug)]
pub enum GraphRefreshErrorKind {
    /// Approval, target, context or observation options invalid before dispatch.
    InvalidInput,
    /// Native submission failure and dispatch-bound uncertainty.
    Submission(Box<RefreshError>),
    /// Local first-delay interruption; the remote job was not cancelled.
    InitialWait(JobError),
    /// Native first-poll delay exceeds the caller's explicit observation bound.
    DelayLimit,
    /// Original neutral observer failure, with scoped handle and last state.
    Observation(Box<ObserveError>),
}

impl fmt::Debug for GraphRefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphRefreshError")
            .field("source", &self.source)
            .field("submission", &self.submission)
            .finish()
    }
}

impl fmt::Display for GraphRefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Graph refresh failed or remains incomplete; retain its submission evidence")
    }
}

impl std::error::Error for GraphRefreshError {}

fn failure(
    source: GraphRefreshErrorKind,
    submission: Option<RefreshReceipt>,
) -> Box<GraphRefreshError> {
    Box::new(GraphRefreshError { source, submission })
}

/// Submit an approved graph refresh once and optionally observe its exact job.
///
/// The caller owns one cancellation/deadline future across submission, first
/// Retry-After wait and neutral observation. Timeout means local interruption,
/// not remote cancellation, rollback, replay or completed refresh. Options and
/// matching credential/endpoint contexts are validated before the POST.
///
/// # Errors
/// Retains accepted work, native dispatch uncertainty and raw unsuccessful,
/// deduped or unknown job states. Job results and metadata/query fallbacks are
/// unsupported. The supplied executor must be the caller's same bounded HTTP
/// stack, including GET retry and timer policy.
pub async fn execute(
    approved: &ApprovedOperation<'_>,
    client: &Client,
    jobs: &JobClient,
    executor: &dyn OperationExecutor,
    options: Option<ObserveOptions>,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<GraphRefresh, Box<GraphRefreshError>> {
    let prepared = approved.prepared();
    let path = prepared.target().canonical_path();
    let graph = target(path).map_err(|_| failure(GraphRefreshErrorKind::InvalidInput, None))?;
    let expected = prepare(path).map_err(|_| failure(GraphRefreshErrorKind::InvalidInput, None))?;
    if prepared.operation_id() != OPERATION_ID
        || prepared.target() != expected.target()
        || prepared.input() != &json!({})
        || prepared.effects() != EFFECTS
        || client.context().endpoint() != jobs.context().endpoint()
        || client.selection() != jobs.context().selection()
        || !Arc::ptr_eq(client.credential(), jobs.context().credential())
        || options
            .is_some_and(|options| options.result || !options.wait || options.max_delay.is_zero())
    {
        return Err(failure(GraphRefreshErrorKind::InvalidInput, None));
    }
    let mut cancelled = pin!(cancelled);
    let submission = graph_refresh::submit(jobs, &graph, cancelled.as_mut())
        .await
        .map_err(|error| failure(GraphRefreshErrorKind::Submission(error), None))?;
    let Some(options) = options else {
        return Ok(GraphRefresh {
            submission,
            observation: None,
        });
    };
    let Some(run) = submission.run() else {
        return Ok(GraphRefresh {
            submission,
            observation: None,
        });
    };
    let handle = ReadHandle::Job {
        workspace_id: graph.workspace_id().into(),
        item_id: graph.graph_id().into(),
        job_id: run.id().into(),
    };
    let delay = match graph_refresh::retry_after(submission.response()) {
        Ok(delay) => delay,
        Err(error) => {
            return Err(failure(
                GraphRefreshErrorKind::InitialWait(error),
                Some(submission),
            ));
        }
    };
    if delay > options.max_delay {
        return Err(failure(GraphRefreshErrorKind::DelayLimit, Some(submission)));
    }
    let waited = fabric_sdk_jobs::cancellable(cancelled.as_mut(), async {
        executor.wait(delay).await.map_err(|error| {
            JobError::Execution(match error {
                OperationExecutionError::Failed => fabric_sdk_jobs::JobExecutionError::Failed,
                OperationExecutionError::TimedOut => fabric_sdk_jobs::JobExecutionError::TimedOut,
                OperationExecutionError::Cancelled => fabric_sdk_jobs::JobExecutionError::Cancelled,
                OperationExecutionError::ResponseLimit => {
                    fabric_sdk_jobs::JobExecutionError::ResponseLimit
                }
                OperationExecutionError::RetriesExhausted => {
                    fabric_sdk_jobs::JobExecutionError::RetriesExhausted
                }
            })
        })
    })
    .await;
    if let Err(error) = waited {
        return Err(failure(
            GraphRefreshErrorKind::InitialWait(error),
            Some(submission),
        ));
    }
    match observe::observe(client, executor, &handle, options, cancelled.as_mut()).await {
        Ok(observation) => Ok(GraphRefresh {
            submission,
            observation: Some(observation),
        }),
        Err(error) => Err(failure(
            GraphRefreshErrorKind::Observation(Box::new(error)),
            Some(submission),
        )),
    }
}

/// Stable schema/effect discovery without credential or native execution.
pub fn schemas() -> Result<Value, crate::v2::ContractError> {
    let registry = registry()?;
    let operation = registry
        .operations()
        .next()
        .ok_or(crate::v2::ContractError::UnknownOperation)?;
    Ok(json!({"operation":operation,"bindings":registry.bindings()}))
}
