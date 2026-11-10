// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Item-definition export and bulk-export command ownership.

#![cfg_attr(test, deny(unnameable_test_items))]

mod bulk_export;
mod files;
mod host;
pub mod parser;

use fabric_cli_context::{Element, NodeKind};
pub use fabric_cli_import_export_contracts::{
    DefinitionCommandContext, TransferCommand, TransferContext, TransferError, TransferEvent,
    TransferFailure, TransferKind, TransferResult,
};
use fabric_cli_import_export_contracts::{
    execution::{
        BorrowedExecutor, Cancellation, item_parts, local_error, message, phase, require_id,
    },
    parser::{command_error, invalid},
    process_tokens,
};
use fabric_cli_output::OutputInput;
use fabric_deploy::definitions::{
    self, BulkExportSnapshot, BulkItem, BulkScope, CommandSupport, DefinitionErrorKind,
    DefinitionTarget,
};
use fabric_sdk_operations::{OperationExecutor, OperationSession};
use serde_json::{Map, Value, json};
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Execute export or bulk-export using only caller-injected capabilities.
///
/// The legacy deploy leaf retains its explicit pre-discovery rejection.
pub async fn execute<E: OperationExecutor + ?Sized>(
    host: &mut DefinitionCommandContext<'_, E>,
    command: &TransferCommand,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<TransferResult, TransferError> {
    if command.kind == TransferKind::Import {
        return Err(invalid("An export command is required").into());
    }
    let mut cancelled: Cancellation<'_> = Box::pin(cancelled);
    let context = host.prepare(command, &mut cancelled).await?;
    match command.kind {
        TransferKind::Export => host::export(host, command, context, &mut cancelled).await,
        TransferKind::BulkExport => bulk_export::bulk(host, command, context, &mut cancelled).await,
        TransferKind::Import | TransferKind::Deploy => unreachable!(),
    }
}

const SINGLE_CONFIRM: &str =
    "Item definition is exported without its sensitivity label. Are you sure?";

async fn export_directory<E: OperationExecutor + ?Sized>(
    host: &mut DefinitionCommandContext<'_, E>,
    input: &str,
) -> Result<PathBuf, TransferError> {
    if input.is_empty() {
        return Err(command_error("InvalidPath", "No such file or directory").into());
    }
    host.files
        .resolve_export_directory_async(input)
        .await
        .map_err(local_error)
}
