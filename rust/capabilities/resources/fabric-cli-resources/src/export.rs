// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CommandError, CommandValue, NamedResource, read_targets};
use fabric_sdk_access::AccessClient;
use fabric_sdk_api::{Limits, cancellable};
use serde_json::Value;
use std::{
    future::Future,
    pin::{Pin, pin},
};

/// Explicit source `acl get -o` storage boundary. The host resolves the requested
/// local/OneLake destination and owns its real writer, naming, time and cleanup.
pub trait AclExportSink {
    /// Exports even falsey projected data. No default successful no-op is supplied.
    /// Dropping the future cancels local work, not a completed remote write.
    fn export<'a>(
        &'a mut self,
        name: &'a str,
        data: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<CommandValue, CommandError>> + Send + 'a>>;
}

/// Composes actual ACL SDK reads/query/export intent without applying stdout
/// truthiness suppression. The separately owned storage host performs the I/O.
pub async fn export_acls(
    client: &AccessClient,
    target: &NamedResource,
    query: Option<&str>,
    limits: Limits,
    sink: &mut dyn AclExportSink,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CommandValue, CommandError> {
    let context = read_targets::acl_context(&target.resource);
    let mut cancelled = pin!(cancelled);
    let rows = client
        .list(&target.resource, limits, cancelled.as_mut())
        .await
        .map_err(|e| CommandError::from(e.error).with_context(&context))?
        .rows;
    let data = read_targets::project_get(rows, query).map_err(|e| e.with_context(&context))?;
    let result = cancellable(
        async { sink.export(&target.name, &data).await },
        cancelled.as_mut(),
    )
    .await
    .map_err(|e| CommandError::from(e).with_context(&context))?;
    match result {
        Ok(mut value) => {
            value.warnings.extend(context.warnings);
            Ok(value)
        }
        Err(mut error) => {
            error.warnings.extend(context.warnings);
            Err(error)
        }
    }
}
