// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Family-owned arguments and runtime-neutral composition for the single CLI.
//! Registration does not install a runtime, credential provider or HTTP backend.

mod host;
mod parser;

pub use host::{AccessHost, AccessOutput, AccessTargets, ApiClients, execute_local, render};
pub use parser::{
    AccessCommand, AccessKind, RawApiCommand, acl_subcommand, api_subcommand, find_subcommand,
    label_subcommand, register,
};
