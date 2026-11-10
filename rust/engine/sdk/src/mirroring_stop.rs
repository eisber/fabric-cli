// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Context-bound host approval around the existing bounded stop orchestrator.
//!
//! Observation and the optional mutation are approved independently. The item
//! library still owns every state decision and retains submission/readback
//! evidence. Each phase uses the shared operation stack and one caller signal.

use crate::{Audience, Client, RequestContext, ServiceOrigin, mirroring, v2};
use fabric_sdk_items::stop::{self, MirroringTarget, StopError, StopOptions, StopReceipt};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
    time::Duration,
};
use v2::{
    ApprovalPolicy, ApprovedOperation, Availability, Binding, ContractError, Effect, Generation,
    OperationDescriptor, Precondition, PreparedOperation, ReceiptError, Registry, ResolvedTarget,
    Verb,
};

/// Shared identity of the preserved stop adapter and this native binding.
pub const OPERATION_ID: &str = "fabric.stop";

/// Observation is mandatory, while the write must be selected and approved.
pub const READ_EFFECTS: [Effect; 1] = [Effect::RemoteRead];

/// Exact effects of an explicitly intended stop from Running.
pub const WRITE_EFFECTS: [Effect; 2] = [Effect::RemoteRead, Effect::RemoteWrite];

/// Register only the exact MirroredDatabase runtime/mirroring stop capability.
///
/// # Errors
/// Preserves conflicting descriptor or binding errors.
pub fn register(registry: &mut Registry) -> Result<(), ContractError> {
    registry.register(
        OperationDescriptor::new(
            OPERATION_ID,
            Verb::Stop,
            json!({
                "type":"object", "additionalProperties":false,
                "required":["yes"], "properties":{"yes":{"type":"boolean"}}
            }),
            json!({
                "type":"object",
                "required":["operation","target","mutationMayHaveOccurred"],
                "properties":{
                    "operation":{"const":OPERATION_ID},
                    "stoppedConfirmed":{"type":"boolean"},
                    "outcome":{"enum":["noOp","alreadyStopping","accepted","requestCompleted","stopped"]}
                }
            }),
            READ_EFFECTS.to_vec(),
        )?
        .with_optional_effects(vec![Effect::RemoteWrite])?,
    )?;
    registry.bind(Binding::new(
        "MirroredDatabase",
        "runtime",
        "mirroring",
        Generation::Any,
        OPERATION_ID,
        Availability::Available,
    )?)
}

/// Local schema discovery without identity or transport construction.
///
/// # Errors
/// Preserves invalid contract metadata errors.
pub fn schemas() -> Result<Value, ContractError> {
    let mut registry = Registry::default();
    register(&mut registry)?;
    Ok(json!({
        "operations":registry.operations().collect::<Vec<_>>(),
        "bindings":registry.bindings()
    }))
}

/// Prepare exact observation and optional write intents, bound to one context.
///
/// # Errors
/// Rejects noncanonical targets and non-Fabric contexts before token acquisition.
pub fn prepare(
    client: &Client,
    target: ResolvedTarget,
    yes: bool,
) -> Result<PreparedStop, ReceiptError> {
    if mirroring::target(target.canonical_path())? != target {
        return Err(ReceiptError::TargetMismatch);
    }
    if client.selection().audience() != Audience::Fabric
        || client.context().endpoint().origin() != ServiceOrigin::Fabric
    {
        return Err(ReceiptError::InvalidEvidence);
    }
    let mut registry = Registry::default();
    register(&mut registry).map_err(ReceiptError::Contract)?;
    let resolution = registry
        .resolve(Verb::Stop, target.selection(), None)
        .map_err(ReceiptError::Contract)?;
    let observation = PreparedOperation::new(resolution, target, json!({"yes":yes}))?
        .with_preconditions(vec![
            Precondition::ExistingOnly,
            Precondition::UnguardedConcurrency,
        ])?;
    let mutation = yes
        .then(|| {
            observation
                .clone()
                .with_optional_effects(vec![Effect::RemoteWrite])
        })
        .transpose()?;
    Ok(PreparedStop {
        observation,
        mutation,
        context: client.context().clone(),
    })
}

/// Immutable target, effects and context presented to caller-owned host policy.
pub struct PreparedStop {
    observation: PreparedOperation,
    mutation: Option<PreparedOperation>,
    context: RequestContext,
}

impl PreparedStop {
    /// Exact read intent; even no-write observations require host policy.
    pub fn operation(&self) -> &PreparedOperation {
        &self.observation
    }

    /// Context retained for every request and the optional write.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    /// Approve observation only; `--yes` is never an authorization token.
    ///
    /// # Errors
    /// Returns the original host refusal without credentials or service work.
    pub fn authorize<P: ApprovalPolicy>(
        &self,
        policy: &mut P,
    ) -> Result<ApprovedStop<'_>, P::Error> {
        Ok(ApprovedStop {
            observation: self.observation.authorize(policy)?,
            mutation: self.mutation.as_ref(),
            context: &self.context,
        })
    }
}

/// Consumed observation approval tied to one immutable context.
pub struct ApprovedStop<'a> {
    observation: ApprovedOperation<'a>,
    mutation: Option<&'a PreparedOperation>,
    context: &'a RequestContext,
}

/// Original SDK result and any independent, unmodified mutation policy refusal.
pub struct StopExecution<E> {
    /// Preserved state, acceptance, uncertainty and completed mutation receipt.
    pub result: Result<StopReceipt, StopError>,
    /// Host refusal retained independently of the SDK's sanitized resource error.
    pub policy_error: Option<E>,
    /// Local interruption observed, even when an actual received response survives.
    pub interrupted: bool,
}

struct ApprovedOperations<'a, E, P: ApprovalPolicy> {
    approved: ApprovedStop<'a>,
    executor: &'a E,
    policy: &'a mut P,
    policy_error: Option<P::Error>,
    limits: OperationLimits,
    remaining: usize,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    interrupted: bool,
    submitted: bool,
}

impl<E: OperationExecutor, P: ApprovalPolicy + Send> ResourceOperations
    for ApprovedOperations<'_, E, P>
where
    P::Error: Send,
{
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        Box::pin(async move {
            let identity = self.approved.observation.prepared().target().identity();
            let v2::TargetIdentity::Item { identity } = identity else {
                return Err(ResourceError::input());
            };
            let prefix = format!(
                "/v1/workspaces/{}/mirroredDatabases/{}/",
                identity.workspace_id(),
                identity.item_id()
            );
            let action = operation.path().strip_prefix(&prefix);
            if operation.method() != ResourceMethod::Post
                || operation.body().is_some()
                || operation.completion() != CompletionPolicy::ReturnAccepted
                || !matches!(action, Some("getMirroringStatus" | "stopMirroring"))
            {
                return Err(ResourceError::input());
            }
            let write = action == Some("stopMirroring");
            if write {
                let Some(prepared) = self.approved.mutation else {
                    return Err(ResourceError::input());
                };
                if self.submitted {
                    return Err(ResourceError::input());
                }
                match prepared.authorize(self.policy) {
                    Ok(approved) if approved.prepared().effects() == WRITE_EFFECTS => (),
                    Ok(_) => return Err(ResourceError::input()),
                    Err(error) => {
                        self.policy_error = Some(error);
                        return Err(ResourceError {
                            code: "HostPolicyDenied".into(),
                            message: "Host policy refused this exact stop mutation".into(),
                            ..ResourceError::input()
                        });
                    }
                }
                self.submitted = true;
            }
            if self.remaining == 0 {
                return Err(ResourceError {
                    code: "Limit".into(),
                    ..ResourceError::input()
                });
            }
            self.remaining -= 1;
            let response_received = AtomicBool::new(false);
            let signal = poll_fn(|cx| {
                if !self.interrupted {
                    self.interrupted = self.cancelled.as_mut().poll(cx).is_ready();
                }
                if self.interrupted && !response_received.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            });
            let mut session = OperationSession::with_context(
                self.approved.context,
                ResponseBoundary {
                    inner: self.executor,
                    received: &response_received,
                },
                self.limits,
                signal,
            );
            ResourceOperations::execute(&mut session, operation).await
        })
    }
}

struct ResponseBoundary<'a, E> {
    inner: &'a E,
    received: &'a AtomicBool,
}

impl<E: OperationExecutor> OperationExecutor for ResponseBoundary<'_, E> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let result = self.inner.execute(request).await;
            // Retain a received response through the shared post-dispatch check;
            // the latched interruption still stops the next phase.
            if result.is_ok() {
                self.received.store(true, Ordering::SeqCst);
            }
            result
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.inner.wait(delay)
    }
}

/// Run the existing stop orchestrator through exact-context approval and HTTP.
///
/// The caller supplies one latched cancellation/deadline signal covering all
/// phases. No outer race drops completed submission evidence or restarts time.
pub async fn execute<E: OperationExecutor, P: ApprovalPolicy + Send>(
    approved: ApprovedStop<'_>,
    executor: &E,
    policy: &mut P,
    limits: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> StopExecution<P::Error>
where
    P::Error: Send,
{
    let v2::TargetIdentity::Item { identity } = approved.observation.prepared().target().identity()
    else {
        unreachable!("prepared stop requires an item identity")
    };
    let target = MirroringTarget::new(identity.workspace_id(), identity.item_id())
        .expect("prepared stop validates exact UUIDs");
    let yes = approved.mutation.is_some();
    let mut operations = ApprovedOperations {
        approved,
        executor,
        policy,
        policy_error: None,
        limits,
        remaining: limits.max_requests.get(),
        cancelled: Box::pin(cancelled),
        interrupted: false,
        submitted: false,
    };
    let result = stop::stop(&mut operations, &target, StopOptions { approved: yes }).await;
    StopExecution {
        result,
        policy_error: operations.policy_error,
        interrupted: operations.interrupted,
    }
}
