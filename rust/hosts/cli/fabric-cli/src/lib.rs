// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded read-command composition over the real SDK, pure context and output.
//!
//! The host owns identity, transport, async runtime, immutable lookup metadata,
//! configuration, time and streams. The opt-in `native-read` executable host
//! supplies these explicitly; default builds never discover credentials.

#[cfg(feature = "windows-broker")]
pub mod auth_commands;
#[cfg(feature = "v2-cancel")]
pub mod cancel;
pub mod console;
pub mod delete;
pub mod deploy;
mod errors;
pub mod execute;
#[cfg(feature = "native-host")]
pub mod families;
mod folder_reads;
pub mod get_backend;
mod get_budget;
pub mod get_command;
pub mod get_metadata;
pub mod get_resolution;
pub mod get_target;
pub mod jobs;
pub mod local_commands;
pub mod native_tabular;
pub mod parser;
mod projection;
mod reads;
#[cfg(feature = "runtime-v2")]
pub mod runtime_v2;
pub mod session;
#[cfg(feature = "native-host")]
pub mod tables;
pub mod tabular_export;
/// Opt-in operation-contract discovery and caller-injected data-schema reads.
#[cfg(feature = "v2-discovery")]
pub mod v2_discovery;
mod workspace_get;

pub use folder_reads::FolderReadHost;
pub use projection::{NoQuery, Projection, PythonQuery};
pub use reads::{ItemSort, ReadConfig, ReadHost, WriteError};

/// Why the executable cannot run service reads without a configured host.
#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub enum HostError {
    /// No production identity/transport/configuration host has been selected.
    #[error(
        "Read execution requires an explicitly configured credential, transport, context, and clock host."
    )]
    NotConfigured,
}
