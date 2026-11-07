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

use serde::Serialize;
use serde_json::Value;

/// Evidence strength of an operation result, not a provider capability claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The declared operation completed, not an unrelated downstream action.
    Succeeded,
    /// A draft was prepared without delivery.
    Prepared,
    /// The caller must complete an action through the identified external route.
    Handoff,
    /// Work was accepted but completion has not been observed.
    Accepted,
    /// Some effects completed and others did not.
    Partial,
    /// An effect may have occurred but its completion cannot be confirmed.
    Unknown,
}

/// Explicit local or provider effect carried independently of presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    /// Writing the selected local preference.
    ConfigurationWrite,
    /// Asking the selected viewer to open a URI.
    ViewerLaunch,
    /// Acquiring the explicitly selected identity context.
    Authentication,
    /// Invalidating and signing out the selected application account.
    SignOut,
    /// Replacing the selected installation.
    Installation,
}

/// Observation of an effect, never an inferred downstream success.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    /// The operation has not initiated this effect.
    NotStarted,
    /// A launcher or provider acknowledged the request.
    Acknowledged,
    /// The declared effect completed.
    Completed,
    /// Completion is still pending.
    Pending,
    /// The effect may have happened but completion is unknown.
    Unknown,
}

/// Retained evidence even when subsequent persistence or output fails.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EffectEvidence {
    /// The action whose evidence is being recorded.
    pub kind: EffectKind,
    /// The strongest observation actually available.
    pub state: EffectState,
}

impl EffectEvidence {
    /// Construct effect evidence without initiating it.
    pub const fn new(kind: EffectKind, state: EffectState) -> Self {
        Self { kind, state }
    }
}

/// Typed result used by this local host, not a replacement SDK result schema.
#[derive(Clone, Debug, Serialize)]
pub struct OperationResult {
    /// Strength of the available outcome evidence.
    pub status: Status,
    /// Operation-specific non-secret data.
    pub data: Value,
    /// Observed effects, retained separately from the result's wording.
    pub effects: Vec<EffectEvidence>,
}

impl OperationResult {
    /// Return a completed declared operation.
    pub fn succeeded(data: Value) -> Self {
        Self::new(Status::Succeeded, data)
    }

    /// Construct an explicitly classified result.
    pub fn new(status: Status, data: Value) -> Self {
        Self {
            status,
            data,
            effects: Vec::new(),
        }
    }

    /// Retain one effect observation.
    pub fn with_effect(mut self, kind: EffectKind, state: EffectState) -> Self {
        self.effects.push(EffectEvidence::new(kind, state));
        self
    }
}

/// Sanitized operation failure, including any effects that already happened.
#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct CliError {
    /// Stable local-host error classification.
    pub code: &'static str,
    /// A specific diagnostic with no credentials or provider response text.
    pub message: String,
    /// Observed effects; cancellation and errors are not rollback.
    pub effects: Vec<EffectEvidence>,
}

impl CliError {
    /// Construct a classified failure before any effect.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            effects: Vec::new(),
        }
    }

    /// Preserve an effect observation in an otherwise failed operation.
    pub fn with_effect(mut self, kind: EffectKind, state: EffectState) -> Self {
        self.effects.push(EffectEvidence::new(kind, state));
        self
    }
}

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
