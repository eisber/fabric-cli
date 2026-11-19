// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! CLI outcome evidence, independent of serialization, streams and SDK models.

use serde::Serialize;
use serde_json::Value;

/// Evidence strength, not a provider capability or downstream success claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The declared operation completed.
    Succeeded,
    /// A draft was prepared without delivery.
    Prepared,
    /// The caller must complete an action through the identified external route.
    Handoff,
    /// Work was accepted but completion has not been observed.
    Accepted,
    /// Some effects completed and others did not.
    Partial,
    /// An effect may have occurred but completion cannot be confirmed.
    Unknown,
}

/// Explicit local or provider effect, separate from presentation.
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

/// Evidence retained even when subsequent persistence or output fails.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EffectEvidence {
    /// The action whose evidence is being recorded.
    pub kind: EffectKind,
    /// The strongest observation actually available.
    pub state: EffectState,
}

impl EffectEvidence {
    /// Construct evidence without initiating an effect.
    pub const fn new(kind: EffectKind, state: EffectState) -> Self {
        Self { kind, state }
    }
}

/// Typed command data and effect observations, without a presentation choice.
///
/// Resource-specific adapters may retain their richer receipt/error types in
/// [`super::presentation::PreparedOutput`] instead of erasing them into JSON.
#[derive(Clone, Debug, Serialize)]
pub struct CommandOutcome<T> {
    /// Strength of the available outcome evidence.
    pub status: Status,
    /// Command-specific data; raw JSON is not implicitly a table.
    pub data: T,
    /// Observed effects, independent of the result's wording.
    pub effects: Vec<EffectEvidence>,
}

impl<T> CommandOutcome<T> {
    /// Return a completed declared operation.
    pub fn succeeded(data: T) -> Self {
        Self::new(Status::Succeeded, data)
    }

    /// Construct an explicitly classified result.
    pub fn new(status: Status, data: T) -> Self {
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

/// Existing local-host result shape, preserving its public API and wire schema.
pub type OperationResult = CommandOutcome<Value>;

/// Sanitized CLI failure, including effects that already happened.
///
/// This is the v2 local-host error contract, not the Python-compatible
/// [`super::CommandError`] or a replacement for resource-specific SDK errors.
#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct CliError {
    /// Stable host error classification.
    pub code: &'static str,
    /// A specific diagnostic without credentials or provider response text.
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

/// Recovery constraint retained through rendering and local delivery.
///
/// This is host metadata, not an instruction to retry or a new wire field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Recovery {
    /// No operation recovery is needed merely because presentation failed.
    None,
    /// Inspect the retained receipt/provider state before considering another operation.
    Reconcile,
    /// Do not replay an accepted, completed or possibly submitted mutation.
    DoNotReplay,
}
