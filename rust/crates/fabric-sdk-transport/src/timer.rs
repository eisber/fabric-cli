// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_sdk_http::{ExecutionFuture, ExecutionTimer};
use std::time::Duration;

/// An asynchronous timer on the caller's Tokio runtime.
///
/// Poll its futures on a Tokio runtime with time enabled. This type does not
/// create a runtime or spawn work. Dropping a future cancels its sleep.
#[derive(Clone, Copy, Debug)]
pub struct TokioTimer;

impl ExecutionTimer for TokioTimer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(async move {
            tokio::time::sleep(delay).await;
        })
    }
}
