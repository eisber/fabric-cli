// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded read-command composition over the real SDK, pure context and output.
//!
//! The host owns identity, transport, async runtime, immutable lookup metadata,
//! configuration, time and streams. The opt-in `native-read` executable host
//! supplies these explicitly; default builds never discover credentials.

mod errors;
mod folder_reads;
pub mod jobs;
pub mod parser;
mod projection;
mod reads;
pub mod session;
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
