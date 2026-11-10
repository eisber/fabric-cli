// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded raw API command behavior over explicitly selected SDK sessions.
//! Request input, cancellation and origins remain caller-selected.

mod api;
pub mod parser;

pub use api::{ApiInput, ApiOptions, api, build_api_request, read_api_input};
use fabric_cli_command_contracts::python_truthy;
pub use fabric_cli_command_contracts::{CommandError, CommandValue};
