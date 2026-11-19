// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit local operations for the additive Fabric CLI v2 host.
//!
//! Planning does not discover credentials, read settings, or execute host effects.
//! The Python CLI and Rust v1 compatibility executable remain separate.

pub mod auth;
pub mod configure;
pub mod output;
pub mod parser;
pub mod report;
pub mod show;
pub mod upgrade;
pub mod validate;

pub use fabric_cli_output::outcome::{
    CliError, EffectEvidence, EffectKind, EffectState, OperationResult, Status,
};

/// Caller-owned cancellation/deadline policy for synchronous local effects.
pub trait Control {
    /// Whether the caller has requested cancellation.
    fn is_cancelled(&self) -> bool;

    /// Whether the caller's total operation deadline has elapsed.
    fn deadline_elapsed(&self) -> bool;
}

/// Check the caller's policy before starting another effect.
pub fn check_control(control: &impl Control) -> Result<(), CliError> {
    if control.is_cancelled() {
        Err(CliError::new("Cancelled", "The operation was cancelled."))
    } else if control.deadline_elapsed() {
        Err(CliError::new("Timeout", "The operation deadline elapsed."))
    } else {
        Ok(())
    }
}
