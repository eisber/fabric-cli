// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private MWC management operations over the released, injected workload boundary.

use crate::{
    ValidationError,
    inputs::*,
    models::*,
    projection::Projection,
    result::{Completed, Effects, Error, ErrorKind, Outcome, status_kind},
    scope::*,
};
use fabric_sdk_ontology_workload::workload::{
    DispatchState, WorkloadError, WorkloadErrorKind, WorkloadExecutor, WorkloadIdentity,
    WorkloadMethod, WorkloadRequest, WorkloadResponse, WorkloadRoute, execute_once,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{future::Future, num::NonZeroUsize, pin::pin};

/// Caller-selected finite allocation bounds; streaming enforcement is executor-owned.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum serialized request bytes, additionally capped by the source endpoint.
    pub request_bytes: NonZeroUsize,
    /// Maximum decoded response bytes, checked again before decoding.
    pub response_bytes: NonZeroUsize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            request_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
            response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Format {
    Json,
    Empty,
    Markdown,
}

pub(crate) struct ResponseContract<T> {
    pub(crate) caller: CallerScope,
    pub(crate) resource: Resource,
    pub(crate) effects: Effects,
    pub(crate) status: u16,
    pub(crate) format: Format,
    pub(crate) selected: Option<Vec<String>>,
    check: fn(&T, &Resource, Option<&[String]>) -> bool,
}

impl<T: Projection> ResponseContract<T> {
    pub(crate) fn new(caller: CallerScope, resource: Resource, effects: Effects) -> Self {
        Self {
            caller,
            resource,
            effects,
            status: 200,
            format: Format::Json,
            selected: None,
            check: T::matches,
        }
    }
}

impl<T: DeserializeOwned> ResponseContract<T> {
    pub(crate) fn decode(&self, response: WorkloadResponse) -> Result<Outcome<T>, Error> {
        if response.status == 202 {
            return Ok(Outcome::Accepted {
                resource: Box::new(self.resource.clone()),
                caller: Box::new(self.caller.clone()),
                response,
                effects: self.effects,
            });
        }
        if !(200..300).contains(&response.status) {
            return Err(Error::observed(
                status_kind(response.status),
                response,
                self.effects,
            ));
        }
        if response.status != self.status {
            return Err(Error::observed(
                ErrorKind::InvalidResponse,
                response,
                self.effects,
            ));
        }
        let decode = || -> Option<T> {
            match self.format {
                Format::Json => {
                    if !media(&response, "application/json") {
                        return None;
                    }
                    serde_json::from_slice(&response.body).ok()
                }
                Format::Empty => {
                    if !response.body.is_empty() {
                        return None;
                    }
                    serde_json::from_value(Value::Null).ok()
                }
                Format::Markdown => {
                    if !media(&response, "text/markdown") {
                        return None;
                    }
                    let text = std::str::from_utf8(&response.body).ok()?;
                    serde_json::from_value(Value::String(text.to_owned())).ok()
                }
            }
        };
        let value = decode()
            .filter(|value| (self.check)(value, &self.resource, self.selected.as_deref()))
            .ok_or_else(|| {
                Error::observed(ErrorKind::InvalidResponse, response.clone(), self.effects)
            })?;
        let tag = response.header("etag").map_err(|_| {
            Error::observed(ErrorKind::InvalidResponse, response.clone(), self.effects)
        })?;
        let validator = tag
            .map(|value| {
                Validator::new(self.caller.clone(), self.resource.clone(), value).map_err(|_| {
                    Error::observed(ErrorKind::InvalidResponse, response.clone(), self.effects)
                })
            })
            .transpose()?;
        Ok(Outcome::Completed(Box::new(Completed {
            resource: self.resource.clone(),
            caller: self.caller.clone(),
            value,
            validator,
            response,
            effects: self.effects,
        })))
    }
}

fn media(response: &WorkloadResponse, expected: &str) -> bool {
    response
        .header("content-type")
        .ok()
        .flatten()
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
        })
}

/// Typed immutable private request with response-shape and returned-identity checks.
pub struct Operation<T> {
    pub(crate) request: WorkloadRequest,
    pub(crate) contract: ResponseContract<T>,
}

impl<T> Operation<T> {
    /// Exact released request, suitable for offline wire assertions.
    pub fn request(&self) -> &WorkloadRequest {
        &self.request
    }
    /// Source-backed effects visible before dispatch.
    pub fn effects(&self) -> Effects {
        self.contract.effects
    }
}

/// Private DataAgent management planner; no host, credential backend or executor is installed.
pub struct PrivateClient {
    agent: AgentScope,
    caller: CallerScope,
    workload: WorkloadIdentity,
    limits: Limits,
}

impl PrivateClient {
    /// Binds an explicit agent, caller cache partition and caller-routed workload identity.
    ///
    /// The caller must bind its injected executor's credentials to the same caller.
    pub fn new(
        agent: AgentScope,
        caller: CallerScope,
        workload: WorkloadIdentity,
        limits: Limits,
    ) -> Self {
        Self {
            agent,
            caller,
            workload,
            limits,
        }
    }

    /// Lists attachments in exactly the requested private stage.
    pub fn list(&self, stage: Stage) -> Result<Operation<Page<OntologyAttachment>>, Error> {
        self.make(
            Resource::Attachments(self.agent.clone(), stage),
            WorkloadMethod::Get,
            &[],
            &[],
            None,
            None,
        )
    }

    /// Reads one private attachment; published member reads are not a public route.
    pub fn read(
        &self,
        stage: Stage,
        ontology: Guid,
    ) -> Result<Operation<OntologyAttachment>, Error> {
        self.make(
            self.attachment(stage, ontology),
            WorkloadMethod::Get,
            &[],
            &[],
            None,
            None,
        )
    }

    /// Adds idempotently per agent+ontology ID and auto-selects current live entities.
    ///
    /// An existing attachment validator or the staging collection validator may be supplied.
    pub fn add(
        &self,
        input: &AttachOntology,
        condition: Option<&Validator>,
    ) -> Result<Operation<OntologyAttachment>, Error> {
        let resource = self.attachment(Stage::Staging, input.reference().item_id.clone());
        let condition_value = condition
            .map(|tag| {
                tag.for_resource(&self.caller, &resource)
                    .or_else(|_| {
                        tag.for_resource(
                            &self.caller,
                            &Resource::Attachments(self.agent.clone(), Stage::Staging),
                        )
                    })
                    .map(str::to_owned)
                    .map_err(Error::local)
            })
            .transpose()?;
        let mut operation = self.make(
            Resource::Attachments(self.agent.clone(), Stage::Staging),
            WorkloadMethod::Post,
            &[],
            &[],
            Some(json(input)?),
            None,
        )?;
        operation.contract.resource = resource;
        operation.contract.effects.auto_selects_entities = true;
        operation.request = conditioned(operation.request, condition_value.as_deref())?;
        Ok(operation)
    }

    /// Recursively removes the staging attachment and its authored instructions/examples only.
    pub fn remove(
        &self,
        ontology: Guid,
        condition: Option<&Validator>,
    ) -> Result<Operation<()>, Error> {
        let mut operation = self.make(
            self.attachment(Stage::Staging, ontology),
            WorkloadMethod::Delete,
            &[],
            &[],
            None,
            condition,
        )?;
        operation.contract.format = Format::Empty;
        operation.contract.effects.removes_authored_setup = true;
        Ok(operation)
    }

    /// Reads native entity navigation with source-defined single-level paging/cache policy.
    pub fn elements(
        &self,
        stage: Stage,
        ontology: Guid,
        options: &ElementsOptions,
    ) -> Result<Operation<Page<Element>>, Error> {
        options.validate().map_err(Error::local)?;
        let resource = Resource::OntologyElements(AttachmentScope {
            agent: self.agent.clone(),
            stage,
            ontology_id: ontology,
        });
        self.make(
            resource,
            WorkloadMethod::Get,
            &[],
            &element_query(options),
            None,
            None,
        )
    }

    /// Applies native entity-only selection without a client-side read/modify/write.
    pub fn select(
        &self,
        ontology: Guid,
        input: &SelectEntities,
        condition: Option<&Validator>,
    ) -> Result<Operation<SelectionResult>, Error> {
        let mut operation = self.make(
            self.attachment(Stage::Staging, ontology),
            WorkloadMethod::Post,
            &["select"],
            &[],
            Some(json(input)?),
            condition,
        )?;
        operation.contract.selected = input.element_ids().map(<[String]>::to_vec);
        Ok(operation)
    }

    /// Reads selected-entity generated context, never authored instruction source.
    pub fn context_markdown(&self, ontology: Guid) -> Result<Operation<ContextMarkdown>, Error> {
        let resource = Resource::ContextMarkdown(AttachmentScope {
            agent: self.agent.clone(),
            stage: Stage::Staging,
            ontology_id: ontology,
        });
        let mut operation = self.make(resource, WorkloadMethod::Get, &[], &[], None, None)?;
        operation.contract.format = Format::Markdown;
        Ok(operation)
    }

    /// Reads selected-source projections; this does not select or mutate physical items.
    pub fn sources(
        &self,
        stage: Stage,
        ontology: Guid,
        response_source: Option<ResponseSource>,
    ) -> Result<Operation<Page<OntologySource>>, Error> {
        let resource = Resource::Sources(AttachmentScope {
            agent: self.agent.clone(),
            stage,
            ontology_id: ontology,
        });
        self.make(
            resource,
            WorkloadMethod::Get,
            &[],
            &source_query(response_source),
            None,
            None,
        )
    }

    /// Reads selected-source schema; inaccessible/unselected sources remain provider 404s.
    pub fn source_schema(
        &self,
        scope: SourceScope,
        response_source: Option<ResponseSource>,
    ) -> Result<Operation<SourceSchema>, Error> {
        self.make(
            Resource::SourceSchema(scope),
            WorkloadMethod::Get,
            &[],
            &source_query(response_source),
            None,
            None,
        )
    }

    /// Reads selected-source element navigation, not an editable schema.
    pub fn source_elements(
        &self,
        scope: SourceScope,
        options: &ElementsOptions,
    ) -> Result<Operation<Page<Element>>, Error> {
        options.validate().map_err(Error::local)?;
        self.make(
            Resource::SourceElements(scope),
            WorkloadMethod::Get,
            &[],
            &element_query(options),
            None,
            None,
        )
    }

    /// Reads authored staging source; native JSON null means no retained user metadata.
    ///
    /// A malformed object or transient provider failure is not treated as absence.
    pub fn instructions(
        &self,
        scope: SourceScope,
    ) -> Result<Operation<Option<SourceInstructions>>, Error> {
        require_staging(&scope)?;
        self.make(
            Resource::Instructions(scope),
            WorkloadMethod::Get,
            &[],
            &[],
            None,
            None,
        )
    }

    /// Patches only the supplied authored source fields; never projects markdown back.
    pub fn patch_instructions(
        &self,
        scope: SourceScope,
        input: &InstructionsPatch,
        condition: Option<&Validator>,
    ) -> Result<Operation<SourceInstructions>, Error> {
        require_staging(&scope)?;
        self.make(
            Resource::Instructions(scope),
            WorkloadMethod::Patch,
            &[],
            &[],
            Some(json(input)?),
            condition,
        )
    }

    /// Explicit replacement clears omitted authored fields.
    pub fn replace_instructions(
        &self,
        scope: SourceScope,
        input: &InstructionsReplace,
        condition: Option<&Validator>,
    ) -> Result<Operation<SourceInstructions>, Error> {
        require_staging(&scope)?;
        self.make(
            Resource::Instructions(scope),
            WorkloadMethod::Put,
            &[],
            &[],
            Some(json(input)?),
            condition,
        )
    }

    /// Deletes instructions while the server preserves description and intact few shots.
    pub fn delete_instructions(
        &self,
        scope: SourceScope,
        condition: Option<&Validator>,
    ) -> Result<Operation<()>, Error> {
        require_staging(&scope)?;
        let mut operation = self.make(
            Resource::Instructions(scope),
            WorkloadMethod::Delete,
            &[],
            &[],
            None,
            condition,
        )?;
        operation.contract.format = Format::Empty;
        Ok(operation)
    }

    /// Lists authored examples in the explicitly selected private stage.
    pub fn few_shots(&self, scope: SourceScope) -> Result<Operation<Page<FewShot>>, Error> {
        self.make(
            Resource::FewShots(scope),
            WorkloadMethod::Get,
            &[],
            &[],
            None,
            None,
        )
    }

    /// Reads one authored example without executing or retrieving it through runtime APIs.
    pub fn few_shot(&self, scope: SourceScope, id: Guid) -> Result<Operation<FewShot>, Error> {
        self.make(
            Resource::FewShot(scope, id),
            WorkloadMethod::Get,
            &[],
            &[],
            None,
            None,
        )
    }

    /// Creates an authored example with a server-assigned identity.
    ///
    /// Source-side validation/embedding maintenance may occur; this is not runtime answering.
    pub fn create_few_shot(
        &self,
        scope: SourceScope,
        input: &FewShotCreate,
        condition: Option<&Validator>,
    ) -> Result<Operation<FewShot>, Error> {
        require_staging(&scope)?;
        let mut operation = self.make(
            Resource::FewShots(scope),
            WorkloadMethod::Post,
            &[],
            &[],
            Some(json(input)?),
            condition,
        )?;
        operation.contract.status = 201;
        operation.contract.effects.may_validate_examples = true;
        Ok(operation)
    }

    /// Patches authored example source without changing omitted fields.
    pub fn patch_few_shot(
        &self,
        scope: SourceScope,
        id: Guid,
        input: &FewShotPatch,
        condition: Option<&Validator>,
    ) -> Result<Operation<FewShot>, Error> {
        require_staging(&scope)?;
        let mut operation = self.make(
            Resource::FewShot(scope, id),
            WorkloadMethod::Patch,
            &[],
            &[],
            Some(json(input)?),
            condition,
        )?;
        operation.contract.effects.may_validate_examples = true;
        Ok(operation)
    }

    /// Deletes exactly one authored example, preserving other source setup.
    pub fn delete_few_shot(
        &self,
        scope: SourceScope,
        id: Guid,
        condition: Option<&Validator>,
    ) -> Result<Operation<()>, Error> {
        require_staging(&scope)?;
        let mut operation = self.make(
            Resource::FewShot(scope, id),
            WorkloadMethod::Delete,
            &[],
            &[],
            None,
            condition,
        )?;
        operation.contract.format = Format::Empty;
        Ok(operation)
    }

    /// Clears all retained custom setup only when the source's native live check permits it.
    ///
    /// Still-available or empty-setup conflicts remain HTTP 409, never client-suppressed.
    pub fn cleanup_unavailable_source(
        &self,
        scope: SourceScope,
        condition: Option<&Validator>,
    ) -> Result<Operation<()>, Error> {
        require_staging(&scope)?;
        let mut operation = self.make(
            Resource::CustomSetup(scope),
            WorkloadMethod::Delete,
            &[],
            &[],
            None,
            condition,
        )?;
        operation.contract.format = Format::Empty;
        operation.contract.effects.removes_authored_setup = true;
        Ok(operation)
    }

    /// Explicitly publishes the entire agent staging tree, not just ontology edits.
    ///
    /// Source does not consume If-Match here. This API has no misleading condition argument.
    pub fn publish(&self, input: &Publish) -> Result<Operation<PublishResult>, Error> {
        let mut operation = self.make(
            Resource::Agent(self.agent.clone(), Stage::Staging),
            WorkloadMethod::Post,
            &["publish"],
            &[],
            Some(json(input)?),
            None,
        )?;
        operation.contract.effects.publishes_whole_agent = true;
        operation.contract.effects.may_validate_examples = true;
        Ok(operation)
    }

    fn attachment(&self, stage: Stage, ontology_id: Guid) -> Resource {
        Resource::Attachment(AttachmentScope {
            agent: self.agent.clone(),
            stage,
            ontology_id,
        })
    }

    fn make<T: Projection>(
        &self,
        resource: Resource,
        method: WorkloadMethod,
        tail: &[&str],
        query: &[(String, String)],
        body: Option<Value>,
        condition: Option<&Validator>,
    ) -> Result<Operation<T>, Error> {
        if resource.agent_stage().0 != &self.agent {
            return Err(Error::local(ValidationError::ValidatorScope));
        }
        let condition = condition
            .map(|tag| {
                tag.for_resource(&self.caller, &resource)
                    .map(str::to_owned)
                    .map_err(Error::local)
            })
            .transpose()?;
        let mut segments = segments(&resource);
        segments.extend(tail.iter().map(|value| (*value).to_owned()));
        let parts: Vec<_> = segments.iter().map(String::as_str).collect();
        let query: Vec<_> = query
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let route = WorkloadRoute::new(&parts, &query).map_err(local_workload)?;
        let source_bound = match &resource {
            Resource::Attachments(..) => 512 * 1024,
            Resource::FewShots(_) | Resource::FewShot(..) => 1024 * 1024,
            Resource::Instructions(_) => (515_000 * 4) + (64 * 1024),
            _ => (500 * 8192) + (64 * 1024),
        };
        let body = body
            .map(|value| serde_json::to_vec(&value))
            .transpose()
            .map_err(|_| Error::local(ValidationError::InputLimit))?;
        let request = WorkloadRequest::new(
            self.workload.clone(),
            route,
            method,
            body.as_ref().map(|_| "application/json"),
            body.unwrap_or_default(),
            self.limits.request_bytes.get().min(source_bound),
            self.limits.response_bytes.get(),
        )
        .map_err(local_workload)?;
        let request = conditioned(request, condition.as_deref())?;
        let effects = if method == WorkloadMethod::Get {
            Effects::read()
        } else {
            Effects::write()
        };
        Ok(Operation {
            request,
            contract: ResponseContract::new(self.caller.clone(), resource, effects),
        })
    }

    /// Dispatches once through the caller's released executor; no HTTP/auth/retry/poller exists here.
    ///
    /// Cancellation before dispatch is NotSent. Cancellation after starting dispatch
    /// is Unknown, never rollback or permission to replay. Accepted responses remain accepted.
    pub async fn execute<T: DeserializeOwned>(
        &self,
        executor: &dyn WorkloadExecutor,
        operation: Operation<T>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Outcome<T>, Error> {
        if operation.contract.caller != self.caller
            || operation.request.workload() != &self.workload
            || operation.contract.resource.agent_stage().0 != &self.agent
        {
            return Err(Error::local(ValidationError::ValidatorScope));
        }
        if operation.request.max_response_bytes() > self.limits.response_bytes.get()
            || operation.request.body().len() > self.limits.request_bytes.get()
        {
            return Err(Error::local(ValidationError::InputLimit));
        }
        let resource = operation.contract.resource.clone();
        self.dispatch(executor, operation, cancelled)
            .await
            .map_err(|error| error.with_scope(self.caller.clone(), resource))
    }

    async fn dispatch<T: DeserializeOwned>(
        &self,
        executor: &dyn WorkloadExecutor,
        operation: Operation<T>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Outcome<T>, Error> {
        let mut started = false;
        let mut cancelled = pin!(cancelled);
        let effects = operation.contract.effects;
        let request = operation.request;
        let response = fabric_core::cancel_with(
            async {
                started = true;
                execute_once(executor, &request).await
            },
            cancelled.as_mut(),
        )
        .await
        .map_err(|_| Error {
            resource: None,
            caller: None,
            kind: ErrorKind::Cancelled,
            dispatch: if started {
                DispatchState::Unknown
            } else {
                DispatchState::NotSent
            },
            response: None,
            response_truncated: false,
            workload_error: None,
            public_error: None,
            effects,
        })?
        .map_err(|error| workload_failure(error, effects, request.max_response_bytes()))?;
        operation.contract.decode(response)
    }
}

fn conditioned(
    request: WorkloadRequest,
    condition: Option<&str>,
) -> Result<WorkloadRequest, Error> {
    match condition {
        Some(token) => request.with_if_match(token).map_err(local_workload),
        None => Ok(request),
    }
}

fn workload_failure(mut error: WorkloadError, effects: Effects, bound: usize) -> Error {
    let mut truncated = matches!(error.kind, WorkloadErrorKind::Limit) && error.response.is_some();
    if let Some(response) = &mut error.response
        && response.body.len() > bound
    {
        response.body.truncate(bound);
        truncated = true;
    }
    let kind = if truncated {
        ErrorKind::Limit
    } else {
        match error.kind {
            WorkloadErrorKind::Limit => ErrorKind::Limit,
            WorkloadErrorKind::Protocol(_) => ErrorKind::InvalidResponse,
            WorkloadErrorKind::Http => error
                .response
                .as_ref()
                .map_or(ErrorKind::Provider, |response| status_kind(response.status)),
            _ => ErrorKind::Execution,
        }
    };
    Error {
        resource: None,
        caller: None,
        kind,
        dispatch: error.dispatch,
        response: error.response.clone(),
        response_truncated: truncated,
        workload_error: Some(Box::new(error)),
        public_error: None,
        effects,
    }
}

fn require_staging(scope: &SourceScope) -> Result<(), Error> {
    if scope.attachment.stage != Stage::Staging {
        Err(Error::local(ValidationError::UnsupportedSurface))
    } else {
        Ok(())
    }
}

fn json(value: &impl Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|_| Error::local(ValidationError::InputLimit))
}

fn local_workload(error: WorkloadError) -> Error {
    Error {
        resource: None,
        caller: None,
        kind: ErrorKind::Execution,
        dispatch: error.dispatch,
        response: error.response.clone(),
        response_truncated: false,
        workload_error: Some(Box::new(error)),
        public_error: None,
        effects: Effects::default(),
    }
}

pub(crate) fn segments(resource: &Resource) -> Vec<String> {
    let (agent, stage) = resource.agent_stage();
    let mut parts = vec![
        "v1".into(),
        "workspaces".into(),
        agent.workspace_id.to_string(),
        "dataagents".into(),
        agent.agent_id.to_string(),
    ];
    if stage == Stage::Staging {
        parts.push("staging".into());
    }
    if matches!(resource, Resource::Agent(..)) {
        return parts;
    }
    parts.push("ontologies".into());
    let attachment = match resource {
        Resource::Attachments(..) => return parts,
        Resource::Attachment(scope)
        | Resource::OntologyElements(scope)
        | Resource::ContextMarkdown(scope)
        | Resource::Sources(scope) => scope,
        Resource::SourceSchema(scope)
        | Resource::SourceElements(scope)
        | Resource::Instructions(scope)
        | Resource::FewShots(scope)
        | Resource::FewShot(scope, _)
        | Resource::CustomSetup(scope) => &scope.attachment,
        Resource::Agent(..) => unreachable!(),
    };
    parts.push(attachment.ontology_id.to_string());
    if matches!(resource, Resource::ContextMarkdown(_)) {
        parts.push("contextmarkdown".into());
        return parts;
    }
    if matches!(resource, Resource::OntologyElements(_)) {
        parts.push("elements".into());
        return parts;
    }
    if matches!(resource, Resource::Attachment(_)) {
        return parts;
    }
    parts.push("ontologydatasources".into());
    let source = match resource {
        Resource::SourceSchema(scope)
        | Resource::SourceElements(scope)
        | Resource::Instructions(scope)
        | Resource::FewShots(scope)
        | Resource::FewShot(scope, _)
        | Resource::CustomSetup(scope) => scope,
        _ => return parts,
    };
    parts.push(source.source_id.to_string());
    match resource {
        Resource::SourceSchema(_) => parts.push("schema".into()),
        Resource::SourceElements(_) => parts.push("elements".into()),
        Resource::Instructions(_) => parts.push("instructions".into()),
        Resource::FewShots(_) => parts.push("fewshots".into()),
        Resource::FewShot(_, id) => {
            parts.push("fewshots".into());
            parts.push(id.to_string());
        }
        _ => {}
    }
    parts
}

pub(crate) fn element_query(options: &ElementsOptions) -> Vec<(String, String)> {
    let mut query = Vec::new();
    if let Some(value) = &options.root_id {
        query.push(("rootId".into(), value.clone()));
    }
    if let Some(value) = &options.continuation_token {
        query.push(("continuationToken".into(), value.clone()));
    }
    if let Some(value) = options.page_size {
        query.push(("pageSize".into(), value.to_string()));
    }
    query.extend(source_query(options.response_source));
    query
}

fn source_query(source: Option<ResponseSource>) -> Vec<(String, String)> {
    source
        .map(|source| vec![("responseSource".into(), source.as_str().into())])
        .unwrap_or_default()
}
