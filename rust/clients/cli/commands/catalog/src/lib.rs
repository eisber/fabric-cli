// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Catalog search and hidden-resource discovery on the caller's runtime.
//! Confirmation and output are injected; no terminal or credentials are discovered.

mod find;
mod hidden;
mod interactive_find;
pub mod parser;

use fabric_cli_command_contracts::python_truthy;
pub use fabric_cli_command_contracts::{CommandError, CommandValue, Confirmation};
pub use find::{FindOptions, build_search_options, find, project_search};
pub use hidden::{
    HiddenNameKind, HiddenRead, domain_name_by_id, find_hidden_name, read_hidden_metadata,
    resolve_hidden_name,
};
pub use interactive_find::{FindOutput, find_interactive};
