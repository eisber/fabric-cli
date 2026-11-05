// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private element traversal through the existing shared token pager.

use crate::{
    ValidationError, models::{Element, Page},
    private::{Operation, PrivateClient, ResponseContract},
    result::{Effects, Error, ErrorKind, Outcome},
    scope::{CallerScope, Resource},
};
use azure_core::http::Url;
use fabric_sdk_ontology_workload::workload::{
    DispatchState, WorkloadExecutor, WorkloadMethod, WorkloadRequest, WorkloadRoute,
};
pub use fabric_sdk_paging::{PagedValues, PagingError, PagingErrorKind, PagingLimits};
use fabric_sdk_paging::{PageResponse, TokenPager};
use std::{fmt, future::{Future, pending}, sync::{Arc, Mutex}};

/// Complete bounded traversal; never silently returned for an incomplete collection.
#[derive(Clone, Debug)]
pub struct Elements {
    /// Exact source/attachment stage traversed.
    pub resource: Resource,
    /// Caller partition whose source projection was traversed.
    pub caller: CallerScope,
    /// Ordered elements, preserving duplicates.
    pub value: Vec<Element>,
    /// Fully committed pages.
    pub pages_received: usize,
    /// Declared cache/migration effects across this traversal.
    pub effects: Effects,
}

/// Shared pager failure with committed raw rows and declared source effects.
#[derive(Debug)]
pub struct ElementsError {
    /// Exact source/attachment stage whose traversal is incomplete.
    pub resource: Box<Resource>,
    /// Original caller partition.
    pub caller: Box<CallerScope>,
    /// Exact shared paging classification and partial progress.
    pub error: Box<PagingError<Error>>,
    /// Source effects, including caller-cache refresh/migration on GET.
    pub effects: Effects,
}

impl fmt::Display for ElementsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DataAgent element traversal is incomplete; inspect committed progress")
    }
}
impl std::error::Error for ElementsError {}

impl PrivateClient {
    /// Collects private attachment/source elements with shared paging, bounds and cancellation.
    ///
    /// `continuation_origin` is an explicit caller-approved HTTPS root used only to
    /// validate advertised continuation metadata. It is never an HTTP destination:
    /// all dispatch remains through the same caller-routed workload executor.
    /// Public published elements have no source-supported cursor argument and
    /// deliberately do not expose this traversal method.
    pub async fn collect_elements(
        &self, executor: &impl WorkloadExecutor, operation: Operation<Page<Element>>,
        continuation_origin: Url, limits: PagingLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Elements, ElementsError> {
        let effects = operation.contract.effects;
        let resource = operation.contract.resource.clone();
        let caller = operation.contract.caller.clone();
        let failure = |kind| ElementsError {
            resource: Box::new(resource.clone()), caller: Box::new(caller.clone()),
            error: Box::new(PagingError { kind, partial: PagedValues::default() }), effects,
        };
        if !matches!(operation.contract.resource, Resource::Attachment(_) | Resource::Source(_))
            || operation.request.method() != WorkloadMethod::Get
            || continuation_origin.path() != "/"
            || continuation_origin.query().is_some()
            || continuation_origin.fragment().is_some()
            || continuation_origin.scheme() != "https"
            || continuation_origin.host_str().is_none()
            || !continuation_origin.username().is_empty()
            || continuation_origin.password().is_some()
        {
            return Err(failure(PagingErrorKind::Fetch(Error::local(ValidationError::UnsupportedSurface))));
        }
        let base = continuation_origin.join(operation.request.route().as_str())
            .map_err(|_| failure(PagingErrorKind::InvalidContinuation))?;
        let pager = TokenPager::new(base, limits)
            .map_err(|_| failure(PagingErrorKind::InvalidContinuation))?;
        let tags = Arc::new(Mutex::new(None::<Option<String>>));
        let collected = pager.collect(|page| {
            let tags = tags.clone();
            let workload = operation.request.workload().clone();
            let resource = operation.contract.resource.clone();
            let caller = operation.contract.caller.clone();
            let path: Vec<_> = page.url().path().split('/').filter(|part| !part.is_empty()).collect();
            let query: Vec<_> = page.url().query_pairs().into_owned().collect();
            let query_refs: Vec<_> = query.iter().map(|(key, value)| (key.as_str(), value.as_str())).collect();
            let request = WorkloadRoute::new(&path, &query_refs).and_then(|route| {
                WorkloadRequest::new(
                    workload, route, WorkloadMethod::Get, None, Vec::new(), 1,
                    page.max_response_bytes().get().min(operation.request.max_response_bytes()),
                )
            });
            async move {
                let request = request.map_err(|error| Error {
                    resource: Some(Box::new(resource.clone())), caller: Some(Box::new(caller.clone())),
                    kind: ErrorKind::Execution, dispatch: error.dispatch, response: error.response.clone(),
                    response_truncated: false, workload_error: Some(Box::new(error)), public_error: None, effects,
                })?;
                let operation: Operation<Page<Element>> = Operation {
                    request, contract: ResponseContract::new(caller, resource, effects),
                };
                let completed = match self.execute(executor, operation, pending()).await? {
                    Outcome::Completed(completed) => completed,
                    Outcome::Accepted { response, .. } => {
                        let mut error = Error::observed(ErrorKind::InvalidResponse, response, effects);
                        error.dispatch = DispatchState::Accepted;
                        return Err(error);
                    }
                };
                let current_tag = completed.response.header("etag")
                    .map_err(|_| Error::observed(ErrorKind::InvalidResponse, completed.response.clone(), effects))?
                    .map(str::to_owned);
                {
                    let mut previous = tags.lock()
                        .map_err(|_| Error::observed(ErrorKind::InvalidResponse, completed.response.clone(), effects))?;
                    if previous.as_ref().is_some_and(|tag| tag != &current_tag) {
                        return Err(Error::observed(ErrorKind::SnapshotChanged, completed.response, effects));
                    }
                    *previous = Some(current_tag);
                }
                Ok(PageResponse {
                    status: completed.response.status,
                    body: completed.response.body,
                    headers: completed.response.headers,
                })
            }
        }, cancelled).await.map_err(|error| ElementsError {
            resource: Box::new(resource.clone()), caller: Box::new(caller.clone()), error: Box::new(error), effects,
        })?;
        let mut value = Vec::with_capacity(collected.value.len());
        for row in &collected.value {
            let element = serde_json::from_value(row.clone()).map_err(|_| ElementsError {
                resource: Box::new(resource.clone()), caller: Box::new(caller.clone()),
                error: Box::new(PagingError { kind: PagingErrorKind::InvalidResponse, partial: collected.clone() }), effects,
            })?;
            value.push(element);
        }
        Ok(Elements { resource, caller, value, pages_received: collected.pages_received, effects })
    }
}
