// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::ExistsError;
use crate::{
    items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    workspaces::{WorkspaceExecutionError, WorkspaceExecutor, WorkspaceRequest, WorkspaceResponse},
};
use fabric_sdk_api::v2_outcome::{
    Coverage, CoverageScope, Effect, NativeResponse, Outcome, ResponseRole, RetentionError,
    RetentionLimits,
};
use std::{future::Future, pin::Pin, sync::Mutex};

pub(super) struct Evidence<'a> {
    items: &'a dyn ItemExecutor,
    workspaces: &'a dyn WorkspaceExecutor,
    ledger: Mutex<Ledger>,
}

struct Ledger {
    outcome: Outcome,
    attempts: usize,
    failure: Option<RetentionError>,
}

impl<'a> Evidence<'a> {
    pub(super) fn new(
        items: &'a dyn ItemExecutor,
        workspaces: &'a dyn WorkspaceExecutor,
        limits: RetentionLimits,
    ) -> Result<Self, ExistsError> {
        Ok(Self {
            items,
            workspaces,
            ledger: Mutex::new(Ledger {
                outcome: Outcome::new(
                    Effect::ReadOnly,
                    Coverage::new(CoverageScope::Response, None, 0)?,
                    limits,
                ),
                attempts: 0,
                failure: None,
            }),
        })
    }

    fn before(&self) -> Result<(), ()> {
        let mut ledger = self.ledger.lock().map_err(|_| ())?;
        if ledger.failure.is_some()
            || ledger.attempts >= ledger.outcome.limits().max_responses()
            || ledger.outcome.retained_bytes() >= ledger.outcome.limits().max_retained_bytes()
        {
            return Err(());
        }
        ledger.attempts += 1;
        Ok(())
    }

    fn capture(
        &self,
        url: &str,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<(), ()> {
        let mut ledger = self.ledger.lock().map_err(|_| ())?;
        let role = if url
            .split('?')
            .next()
            .is_some_and(|path| path.ends_with("/workspaces") || path.ends_with("/items"))
        {
            ResponseRole::Page
        } else {
            ResponseRole::Initial
        };
        let result = NativeResponse::new(role, status, headers, body, ledger.outcome.limits())
            .and_then(|response| ledger.outcome.append(response));
        if let Err(error) = result {
            ledger.failure = Some(error);
            return Err(());
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<(Outcome, usize, Option<RetentionError>), ExistsError> {
        let ledger = self
            .ledger
            .into_inner()
            .map_err(|_| ExistsError::EvidenceLock)?;
        Ok((ledger.outcome, ledger.attempts, ledger.failure))
    }
}

impl ItemExecutor for Evidence<'_> {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            self.before()
                .map_err(|()| ItemExecutionError::ResponseLimit)?;
            let url = request.url().as_str().to_owned();
            let response = self.items.execute(request).await?;
            self.capture(
                &url,
                response.status,
                response
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                response.body.clone(),
            )
            .map_err(|()| ItemExecutionError::ResponseLimit)?;
            Ok(response)
        })
    }
}

impl WorkspaceExecutor for Evidence<'_> {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.before()
                .map_err(|()| WorkspaceExecutionError::ResponseLimit)?;
            let url = request.url().as_str().to_owned();
            let response = self.workspaces.execute(request).await?;
            self.capture(
                &url,
                response.status,
                response
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                response.body.clone(),
            )
            .map_err(|()| WorkspaceExecutionError::ResponseLimit)?;
            Ok(response)
        })
    }
}
