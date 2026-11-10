// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Legacy mixed-family arguments and runtime-neutral compatibility composition.
//! Canonical handlers are reexported from the sibling capability packages.
//! Registration does not install a runtime, credential provider or HTTP backend.

mod host;
mod parser;

pub use host::{AccessHost, AccessOutput, AccessTargets, ApiClients, execute_local, render};
pub use parser::{
    AccessCommand, AccessKind, RawApiCommand, acl_subcommand, api_subcommand, find_subcommand,
    label_subcommand, register,
};
