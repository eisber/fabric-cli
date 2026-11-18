// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::CommandError;
use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use fabric_core::{
    Binding, Clock, HttpTransport, OperationContext,
    diagnostics::{DiagnosticPhase, DiagnosticStage},
};
use std::{sync::Arc, time::Duration};

/// Host-owned effects; fake implementations prove parser and output behavior offline.
#[async_trait]
pub trait Host: Send + Sync {
    /// Read one explicitly named inherited hint; injected hosts default to no hints.
    fn environment(&self, _name: &str) -> Option<std::ffi::OsString> {
        None
    }

    /// Explicit clock used for timestamps, deadlines, and polling.
    fn clock(&self) -> Arc<dyn Clock>;
    /// Read only this Rust CLI's non-secret selection.
    fn selection(&self) -> Result<Option<Binding>, CommandError>;
    /// Atomically persist tenant/principal only.
    fn save_selection(&self, binding: &Binding) -> Result<(), CommandError>;
    /// Clear only Rust selection; absent selection is success.
    fn clear_selection(&self) -> Result<(), CommandError>;
    /// Validate the current Azure CLI context, without signing in or mutating it.
    async fn login(&self, context: &OperationContext) -> Result<Binding, CommandError>;
    /// Bind an explicit existing selection to an injected credential source.
    fn credential(&self, binding: Binding) -> Result<Arc<dyn TokenCredential>, CommandError>;
    /// Construct transport only after request and selection validation.
    fn transport(&self) -> Result<Arc<dyn HttpTransport>, CommandError>;
    /// Drain safe static diagnostics for the host to write to stderr, never JSON stdout.
    fn take_diagnostics(&self) -> Vec<&'static str> {
        Vec::new()
    }
    /// Whether progress can use ANSI color. Injected/nonterminal streams default to plain text.
    fn progress_color(&self) -> bool {
        false
    }
    /// Whether stderr is an interactive terminal, independently of color.
    fn progress_terminal(&self) -> bool {
        false
    }
}

/// Read a required selection without initializing credentials or transport.
pub fn selected(host: &dyn Host) -> Result<Binding, CommandError> {
    host.selection()?.ok_or_else(|| {
        CommandError::new(
            "Unauthorized",
            "No Rust identity selected. Run fab-rs auth login.",
        )
    })
}

/// Report command stages without owning or directly writing a console stream.
pub trait CommandDiagnostics: Send + Sync {
    /// Elapsed time since logging started, or zero with logging disabled.
    fn elapsed(&self) -> Duration;

    /// Report a typed stage transition.
    fn stage(
        &self,
        stage: DiagnosticStage,
        phase: DiagnosticPhase,
        duration: Duration,
    ) -> impl Future<Output = Result<(), CommandError>> + Send;

    /// Report the start of a stage and return its start time.
    fn start(
        &self,
        stage: DiagnosticStage,
    ) -> impl Future<Output = Result<Duration, CommandError>> + Send {
        async move {
            let started = self.elapsed();
            self.stage(stage, DiagnosticPhase::Started, Duration::ZERO)
                .await?;
            Ok(started)
        }
    }

    /// Report the result of a stage and propagate its output or reporting error.
    fn finish<T: Send>(
        &self,
        stage: DiagnosticStage,
        started: Duration,
        result: Result<T, CommandError>,
    ) -> impl Future<Output = Result<T, CommandError>> + Send {
        async move {
            let phase = result.as_ref().map_or_else(
                |error| DiagnosticPhase::Failed(error.kind()),
                |_| DiagnosticPhase::Completed,
            );
            self.stage(stage, phase, self.elapsed().saturating_sub(started))
                .await?;
            result
        }
    }
}
