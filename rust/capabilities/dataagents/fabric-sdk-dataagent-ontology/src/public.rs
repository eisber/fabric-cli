// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public published-only reads over the existing authenticated Fabric operation policy.
//!
//! Staging/member/source-guidance/contextmarkdown/publish are not public routes.

use crate::{
    ValidationError, models::{Element, OntologyAttachment, Page},
    private::{Limits, ResponseContract, segments},
    projection::Projection,
    result::{Effects, Error, ErrorKind, Outcome, status_kind},
    scope::*,
};
use fabric_core::RequestContext;
use fabric_sdk_ontology_workload::workload::{DispatchState, WorkloadResponse, WorkloadRoute};
use fabric_sdk_operations::{
    FabricOperation, OperationErrorKind, OperationExecutor, OperationLimits, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde::de::DeserializeOwned;
use std::{future::Future, num::NonZeroUsize};

/// Typed public read; cannot be executed through a private MWC executor.
pub struct PublicRead<T> {
    operation: FabricOperation,
    contract: ResponseContract<T>,
}

impl<T> PublicRead<T> {
    /// Validated operation descriptor for offline method/path/body inspection.
    pub fn operation(&self) -> &FabricOperation { &self.operation }
    /// Declared cache/migration effects, even though the method is GET.
    pub fn effects(&self) -> Effects { self.contract.effects }
}

/// Public published ontology projection with explicitly injected credentials/context.
pub struct PublicClient {
    context: RequestContext,
    agent: AgentScope,
    caller: CallerScope,
    limits: Limits,
}

impl PublicClient {
    /// Stores caller inputs without token acquisition or endpoint discovery.
    pub fn new(context: RequestContext, agent: AgentScope, limits: Limits) -> Result<Self, Error> {
        let caller = CallerScope::new(context.selection().account(), context.selection().tenant())
            .map_err(Error::local)?;
        Ok(Self { context, agent, caller, limits })
    }

    /// Lists published attachments. It does not invent a public member GET route.
    pub fn list_published(&self) -> Result<PublicRead<Page<OntologyAttachment>>, Error> {
        self.make(Resource::Attachments(self.agent.clone(), Stage::Published), &[], &[])
    }

    /// Reads published ontology navigation; public source supports only `rootId`.
    ///
    /// No staging, live-refresh, page-size or continuation option is silently forwarded.
    pub fn published_elements(&self, ontology_id: Guid, root_id: Option<&str>) -> Result<PublicRead<Page<Element>>, Error> {
        let mut query = Vec::new();
        if let Some(value) = root_id {
            crate::selector(value, 8192).map_err(Error::local)?;
            query.push(("rootId", value));
        }
        let resource = Resource::Attachment(AttachmentScope {
            agent: self.agent.clone(), stage: Stage::Published, ontology_id,
        });
        self.make(resource, &["elements"], &query)
    }

    fn make<T: Projection>(&self, resource: Resource, tail: &[&str], query: &[(&str, &str)]) -> Result<PublicRead<T>, Error> {
        let mut parts = segments(&resource);
        parts.extend(tail.iter().map(|value| (*value).to_owned()));
        let parts: Vec<_> = parts.iter().map(String::as_str).collect();
        let route = WorkloadRoute::new(&parts, query)
            .map_err(|_| Error::local(ValidationError::InvalidSelector))?;
        let operation = FabricOperation::new(
            ResourceMethod::Get, route.as_str(), Vec::new(), CompletionPolicy::ReturnAccepted,
        ).map_err(|_| Error::local(ValidationError::InvalidSelector))?;
        Ok(PublicRead { operation, contract: ResponseContract::new(self.caller.clone(), resource, Effects::read()) })
    }

    /// Uses the existing shared authenticated single-read policy, with no new transport or poller.
    ///
    /// Unsupported/failed public reads never fall back to private MWC, responses,
    /// example queries, model answering, or an Ontology definition/query endpoint.
    pub async fn execute<T: DeserializeOwned, E: OperationExecutor>(
        &self, read: PublicRead<T>, executor: E, cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Outcome<T>, Error> {
        if read.contract.caller != self.caller || read.contract.resource.agent_stage().0 != &self.agent {
            return Err(Error::local(ValidationError::ValidatorScope));
        }
        let limits = OperationLimits {
            max_requests: NonZeroUsize::new(1).unwrap(),
            max_response_bytes: self.limits.response_bytes,
            ..OperationLimits::default()
        };
        let mut session = OperationSession::with_context(&self.context, executor, limits, cancelled);
        let response = session.execute(read.operation).await.map_err(|error| {
            let kind = match &error.kind {
                OperationErrorKind::Http { status, .. } => status_kind(*status),
                OperationErrorKind::Cancelled => ErrorKind::Cancelled,
                OperationErrorKind::Limit => ErrorKind::Limit,
                OperationErrorKind::InvalidResponse | OperationErrorKind::InvalidLink => ErrorKind::InvalidResponse,
                _ => ErrorKind::Execution,
            };
            Error {
                kind,
                dispatch: match &error.kind {
                    OperationErrorKind::InvalidInput | OperationErrorKind::InvalidAudience
                    | OperationErrorKind::Credential => DispatchState::NotSent,
                    OperationErrorKind::Http { status: 202, .. } => DispatchState::Accepted,
                    OperationErrorKind::Http { .. } => DispatchState::Completed,
                    _ => DispatchState::Unknown,
                },
                response: None, response_truncated: false, workload_error: None,
                public_error: Some(Box::new(error)), effects: read.contract.effects,
            }
        })?;
        read.contract.decode(WorkloadResponse {
            status: response.status, headers: response.headers, body: response.body,
        })
    }
}
