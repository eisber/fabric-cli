// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use azure_core::credentials::AccessToken;
use fabric_sdk_api::{ExecutionError, Executor, Origin, Request, Response};
use fabric_sdk_http::{ExecutionError as HttpError, ExecutionFuture, HttpExecutor, PublicOrigin};
use std::{num::NonZeroUsize, sync::Arc};

/// Thin composition adapter. SDK service crates retain transport-free traits,
/// avoiding a future facade -> service -> HTTP -> facade dependency cycle.
pub struct ResourceHttpExecutor(pub Arc<HttpExecutor>);

impl Executor for ResourceHttpExecutor {
    fn supports(&self, _: &Request) -> bool {
        true
    }

    fn wait(&self, delay: std::time::Duration) -> ExecutionFuture<'_, Result<(), ExecutionError>> {
        Box::pin(async move { self.0.wait_for(delay).await.map_err(map_error) })
    }

    fn execute<'a>(
        &'a self,
        request: &'a Request,
        token: &'a AccessToken,
        limit: NonZeroUsize,
    ) -> ExecutionFuture<'a, Result<Response, ExecutionError>> {
        Box::pin(async move {
            self.0
                .execute_bound(
                    match request.origin() {
                        Origin::Fabric => PublicOrigin::Fabric,
                        Origin::OneLake => PublicOrigin::OneLake,
                        Origin::Arm => PublicOrigin::Arm,
                        Origin::PowerBi => PublicOrigin::PowerBi,
                    },
                    request.method(),
                    request.url(),
                    token,
                    request.body(),
                    request.headers(),
                    limit,
                )
                .await
                .map_err(map_error)
        })
    }
}

fn map_error(error: HttpError) -> ExecutionError {
    match error {
        HttpError::TimedOut => ExecutionError::TimedOut,
        HttpError::ResponseLimit => ExecutionError::ResponseLimit,
        HttpError::RetriesExhausted => ExecutionError::RetriesExhausted,
        _ => ExecutionError::Failed,
    }
}
