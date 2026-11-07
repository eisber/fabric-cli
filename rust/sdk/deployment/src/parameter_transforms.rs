// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded source-compatible parameter primitives, separate from CLI projection.

mod context;
mod input;
mod jsonpath;
mod paths;
mod program;
mod regex;
mod schema;

pub use context::{
    DynamicVariable, DynamicVariableKind, ParameterItem, ParameterResolution, ParameterResolver,
    encode_dynamic_name, matches_parameter_filters,
};
pub use input::{
    ParameterFileHost, PreparedParameters, expand_parameter_environment, merge_parameter_template,
    prepare_parameter_document,
};
pub use jsonpath::JsonPathReplacement;
pub use paths::{PathFilterOptions, PathFilterResolution, resolve_parameter_paths};
pub use program::{ParameterProgram, SkippedParameter};
pub use regex::{FindReplacement, SourceRegex};

use std::{
    fmt,
    time::{Duration, Instant},
};

/// Explicit parameter failure; unsupported or exhausted work is never no-match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransformError {
    /// Invalid source pattern or input, retaining its diagnostic.
    Invalid(String),
    /// Recognized source semantics outside this adapter's proven subset.
    Unsupported(&'static str),
    /// Input, output, syntax, backtracking, visit or match limit reached.
    Limit,
    /// Caller cancellation observed before completion.
    Cancelled,
    /// Finite elapsed preparation budget reached.
    TimedOut,
}

impl fmt::Display for TransformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Unsupported(branch) => {
                write!(formatter, "Unported parameter transform: {branch}")
            }
            Self::Limit => formatter.write_str("Parameter transform exceeded its finite bound"),
            Self::Cancelled => formatter.write_str("Parameter transform cancelled"),
            Self::TimedOut => {
                formatter.write_str("Parameter transform preparation deadline elapsed")
            }
        }
    }
}

impl std::error::Error for TransformError {}

/// Caller-selected finite bounds shared across one parameter preparation.
#[derive(Clone, Copy, Debug)]
pub struct TransformLimits {
    /// Maximum UTF-8 input or resulting content bytes.
    pub max_bytes: usize,
    /// Maximum pattern/expression UTF-8 bytes.
    pub max_pattern_bytes: usize,
    /// Maximum syntax or JSON nesting.
    pub max_depth: usize,
    /// Maximum aggregate visited nodes/steps, including sequential rules.
    pub max_visits: usize,
    /// Maximum selected matches across sequential rules.
    pub max_matches: usize,
    /// Maximum fancy-regex VM backtracking steps per search.
    pub max_backtracks: usize,
    /// Maximum compiled delegated regex allocation.
    pub max_regex_bytes: usize,
    /// Finite overall elapsed budget, checked between bounded primitive calls.
    pub max_duration: Duration,
}

impl Default for TransformLimits {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024,
            max_pattern_bytes: 4096,
            max_depth: 64,
            max_visits: 100_000,
            max_matches: 10_000,
            max_backtracks: 100_000,
            max_regex_bytes: 1024 * 1024,
            max_duration: Duration::from_secs(5),
        }
    }
}

/// Caller cancellation capability; no runtime or global flag is installed.
pub trait TransformCancellation: Send + Sync {
    /// Return true when synchronous preparation must stop.
    fn is_cancelled(&self) -> bool;
}

impl<F: Fn() -> bool + Send + Sync> TransformCancellation for F {
    fn is_cancelled(&self) -> bool {
        self()
    }
}

/// One preparation budget shared by syntax, selection, mutation and emission.
///
/// Elapsed time is checked between primitive calls. A single regex search is
/// separately bounded by input/compiled size and finite VM backtracking; this is
/// not preemptive interruption or a hard real-time guarantee.
pub struct TransformBudget<'a> {
    pub(super) limits: TransformLimits,
    cancellation: &'a dyn TransformCancellation,
    started: Instant,
    visits: usize,
    matches: usize,
}

impl<'a> TransformBudget<'a> {
    /// Start a command-local finite preparation with caller cancellation.
    pub fn new(limits: TransformLimits, cancellation: &'a dyn TransformCancellation) -> Self {
        Self {
            limits,
            cancellation,
            started: Instant::now(),
            visits: 0,
            matches: 0,
        }
    }

    /// Observe cancellation, elapsed time and aggregate work bounds.
    ///
    /// # Errors
    /// Returns typed cancellation, time or visit exhaustion, never partial success.
    pub fn checkpoint(&mut self) -> Result<(), TransformError> {
        if self.cancellation.is_cancelled() {
            return Err(TransformError::Cancelled);
        }
        if self.started.elapsed() >= self.limits.max_duration {
            return Err(TransformError::TimedOut);
        }
        self.visits = self
            .visits
            .checked_add(1)
            .filter(|count| *count <= self.limits.max_visits)
            .ok_or(TransformError::Limit)?;
        Ok(())
    }

    pub(super) fn text(&mut self, text: &str) -> Result<(), TransformError> {
        self.checkpoint()?;
        if text.len() > self.limits.max_bytes {
            return Err(TransformError::Limit);
        }
        Ok(())
    }

    pub(super) fn depth(&mut self, depth: usize) -> Result<(), TransformError> {
        self.checkpoint()?;
        if depth > self.limits.max_depth {
            return Err(TransformError::Limit);
        }
        Ok(())
    }

    pub(super) fn matched(&mut self) -> Result<(), TransformError> {
        self.checkpoint()?;
        self.matches = self
            .matches
            .checked_add(1)
            .filter(|count| *count <= self.limits.max_matches)
            .ok_or(TransformError::Limit)?;
        Ok(())
    }

    pub(super) fn push(&mut self, output: &mut String, text: &str) -> Result<(), TransformError> {
        self.checkpoint()?;
        output
            .len()
            .checked_add(text.len())
            .filter(|length| *length <= self.limits.max_bytes)
            .ok_or(TransformError::Limit)?;
        output.push_str(text);
        Ok(())
    }
}
