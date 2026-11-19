// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Catalog grammar, discovery and execution on the caller's runtime.

mod find;
mod hidden;
mod interactive_find;
pub mod parser;
pub mod service;

use fabric_cli_command_contracts::python_truthy;
pub use fabric_cli_command_contracts::{CommandError, CommandValue, Confirmation};
pub use find::{FindOptions, build_search_options, find, project_search};
pub use hidden::{
    HiddenNameKind, HiddenRead, domain_name_by_id, find_hidden_name, read_hidden_metadata,
    resolve_hidden_name,
};
pub use interactive_find::{FindOutput, find_interactive};

use fabric_sdk_api::{ApiClient, Limits};
use fabric_sdk_catalog::CatalogClient;
use std::future::Future;

/// Executes FIND using the caller-selected Fabric session and existing bounds.
pub async fn execute(
    client: &ApiClient,
    command: &FindOptions,
    limits: Limits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CommandValue, CommandError> {
    find(
        &CatalogClient::new(client.clone()),
        command,
        limits,
        cancelled,
    )
    .await
}
