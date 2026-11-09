// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/// Explicit caller configuration. Construction does not read or persist settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Policy {
    /// No automatic network work.
    #[default]
    Off,
    /// Periodically check only; never install at startup.
    Check,
    /// A configured terminal startup may check and request installation.
    ///
    /// This is not authorization for CI, redirected/batch runs, or arbitrary
    /// library calls; those still require an explicit update invocation.
    Install,
}

/// Invocation properties supplied by the CLI before any startup network request.
#[derive(Clone, Copy, Debug)]
pub struct AutomaticContext {
    /// Both input and output are terminals.
    pub terminal: bool,
    /// Help or version has precedence over all startup checks.
    pub help_or_version: bool,
    /// Deterministic/headless/CI/batch operation.
    pub automation: bool,
}

/// Pure startup decision; the caller performs any selected work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupDecision {
    /// Preserve no-network behavior.
    Skip,
    /// Check only.
    Check,
    /// Check, then install only through an explicitly supplied install adapter.
    CheckAndInstall,
}

/// Memory-only finite 24-hour cadence using monotonic, not wall-clock, time.
///
/// A new process starts due; persistence and cross-process policy belong to the
/// caller. Attempts, including failures, consume the interval. A backwards
/// monotonic clock resets the baseline and skips instead of causing a tight loop.
#[derive(Clone, Debug, Default)]
pub struct CheckSchedule {
    last_attempt_ms: Option<u64>,
}

impl CheckSchedule {
    /// Decide whether a caller-selected startup is due without performing I/O.
    pub fn startup(
        &mut self,
        policy: Policy,
        context: AutomaticContext,
        monotonic_ms: u64,
    ) -> StartupDecision {
        if policy == Policy::Off
            || !context.terminal
            || context.help_or_version
            || context.automation
        {
            return StartupDecision::Skip;
        }
        if let Some(last) = self.last_attempt_ms {
            if monotonic_ms < last {
                self.last_attempt_ms = Some(monotonic_ms);
                return StartupDecision::Skip;
            }
            if monotonic_ms - last < 24 * 60 * 60 * 1000 {
                return StartupDecision::Skip;
            }
        }
        self.last_attempt_ms = Some(monotonic_ms);
        match policy {
            Policy::Off => StartupDecision::Skip,
            Policy::Check => StartupDecision::Check,
            Policy::Install => StartupDecision::CheckAndInstall,
        }
    }
}
