// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Item-definition import command ownership and Environment publication.

#![cfg_attr(test, deny(unnameable_test_items))]

mod environment;
mod host;
pub mod parser;

use fabric_cli_context::NodeKind;
pub use fabric_cli_import_export_contracts::{
    DefinitionCommandContext, TransferCommand, TransferContext, TransferError, TransferEvent,
    TransferFailure, TransferKind, TransferResult,
};
use fabric_cli_import_export_contracts::{
    ImportCacheEffect,
    execution::{
        BorrowedExecutor, Cancellation, item_parts, local_error, message, phase, require_id,
    },
    parser::{command_error, invalid},
    process_tokens,
};
use fabric_deploy::definitions::{self, DefinitionError, ImportTarget};
use fabric_sdk_operations::{OperationExecutor, OperationSession};
use std::future::Future;

/// Execute import using only caller-injected capabilities and bounded policy.
pub async fn execute<E: OperationExecutor + ?Sized>(
    host: &mut DefinitionCommandContext<'_, E>,
    command: &TransferCommand,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<TransferResult, TransferError> {
    if command.kind != TransferKind::Import {
        return Err(invalid("An import command is required").into());
    }
    let mut cancelled: Cancellation<'_> = Box::pin(cancelled);
    let context = host.prepare(command, &mut cancelled).await?;
    host::import(host, command, context, &mut cancelled).await
}
