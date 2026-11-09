// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One invocation-wide response/request ledger over existing executor policies.

use fabric_sdk::{
    items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    workspaces::{WorkspaceExecutionError, WorkspaceExecutor, WorkspaceRequest, WorkspaceResponse},
};
use fabric_sdk_onelake::{OneLakeExecutionError, OneLakeExecutor, OneLakeRequest, OneLakeResponse};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use std::{future::Future, pin::Pin, sync::Mutex, time::Duration};

struct Budget {
    bytes: usize,
    requests: usize,
}

/// Shared response and dispatch allowance; the caller owns the total deadline.
pub(crate) struct GetReadBudget<'a> {
    pub(crate) items: &'a dyn ItemExecutor,
    pub(crate) workspaces: &'a dyn WorkspaceExecutor,
    pub(crate) operations: &'a dyn OperationExecutor,
    pub(crate) storage: Option<&'a dyn OneLakeExecutor>,
    remaining: Mutex<Budget>,
}

impl<'a> GetReadBudget<'a> {
    pub(crate) fn new(
        items: &'a dyn ItemExecutor,
        workspaces: &'a dyn WorkspaceExecutor,
        operations: &'a dyn OperationExecutor,
        storage: Option<&'a dyn OneLakeExecutor>,
        bytes: usize,
        requests: usize,
    ) -> Self {
        Self {
            items,
            workspaces,
            operations,
            storage,
            remaining: Mutex::new(Budget { bytes, requests }),
        }
    }

    fn before(&self) -> bool {
        let Ok(mut budget) = self.remaining.lock() else {
            return false;
        };
        if budget.bytes == 0 || budget.requests == 0 {
            return false;
        }
        budget.requests -= 1;
        true
    }

    fn charge(&self, bytes: usize) -> bool {
        let Ok(mut budget) = self.remaining.lock() else {
            return false;
        };
        match budget.bytes.checked_sub(bytes) {
            Some(left) => {
                budget.bytes = left;
                true
            }
            None => {
                budget.bytes = 0;
                false
            }
        }
    }

    pub(crate) fn remaining_bytes(&self) -> usize {
        self.remaining.lock().map_or(0, |budget| budget.bytes)
    }
}

impl ItemExecutor for GetReadBudget<'_> {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            if !self.before() {
                return Err(ItemExecutionError::ResponseLimit);
            }
            let response = self.items.execute(request).await?;
            if !self.charge(response.body.len()) {
                return Err(ItemExecutionError::ResponseLimit);
            }
            Ok(response)
        })
    }
}

impl WorkspaceExecutor for GetReadBudget<'_> {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            if !self.before() {
                return Err(WorkspaceExecutionError::ResponseLimit);
            }
            let response = self.workspaces.execute(request).await?;
            if !self.charge(response.body.len()) {
                return Err(WorkspaceExecutionError::ResponseLimit);
            }
            Ok(response)
        })
    }
}

impl OperationExecutor for GetReadBudget<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            if !self.before() {
                return Err(OperationExecutionError::ResponseLimit);
            }
            let response = self.operations.execute(request).await?;
            if !self.charge(response.body.len()) {
                return Err(OperationExecutionError::ResponseLimit);
            }
            Ok(response)
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.operations.wait(delay)
    }
}

impl OneLakeExecutor for GetReadBudget<'_> {
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OneLakeResponse, OneLakeExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            if !self.before() {
                return Err(OneLakeExecutionError::ResponseLimit);
            }
            let storage = self.storage.ok_or(OneLakeExecutionError::Failed)?;
            let response = storage.execute(request).await?;
            if !self.charge(response.body.len()) {
                return Err(OneLakeExecutionError::ResponseLimit);
            }
            Ok(response)
        })
    }
}
