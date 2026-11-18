// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Injectable CLI composition. The executable owns the runtime and signals.
#![forbid(unsafe_code)]

mod console;
mod invocation;
#[path = "process/mod.rs"]
pub mod lifecycle;
mod native;
mod output;

pub use fabric_cli_command_contracts::Host;
pub use invocation::{parser, run};
pub use native::{NativeHost, SelectionFile};
