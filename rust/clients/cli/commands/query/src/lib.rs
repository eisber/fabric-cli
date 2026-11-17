// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Query-specific preparation over authoritative, caller-resolved native bindings.
//!
//! No path suffix, query language, file extension, ambient identity, or provider
//! failure selects a different route. The existing CLI parser owns the grammar.
//! Inputs are bounded and resolved once before credentials or query dispatch.

mod input;
mod kql;
mod output;
mod target;

pub use input::{InputReader, InputSource, PreparedInput, QueryInput, TextSource, prepare_input};
pub use kql::{KqlPreparationError, PreparedKql};
pub use output::{QueryEncodingError, encode_single};
pub use target::{Artifact, DataAgentRoute, KqlRoute, QueryTarget, ResolvedRoute, Stage};

/// Sanitized local preparation failures, before credentials or native dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PreparationError {
    /// An authoritative requested/execution artifact or binding is inconsistent.
    #[error("The resolved query target or native binding is invalid")]
    InvalidTarget,
    /// No supported typed, read-only route is configured for the resolved target.
    #[error("No supported typed query provider is configured for this target")]
    MissingProvider,
    /// More than one configured route matches; no route is silently preferred.
    #[error("Query provider resolution is ambiguous; configure one exact binding")]
    AmbiguousProvider,
    /// An explicitly requested engine does not match an advertised route.
    #[error("The requested engine is not advertised by this resolved target")]
    UnsupportedEngine,
    /// This provider has no supported native parameter schema.
    #[error("The selected query provider does not support --parameters")]
    UnsupportedParameters,
    /// Empty, malformed, conflicting or oversized local input.
    #[error(
        "Query input must be nonblank UTF-8 within the configured byte limit; only one input may read stdin"
    )]
    InvalidInput,
    /// A caller-owned reader failed; paths, contents and query text are not echoed.
    #[error("The query input could not be read")]
    InputRead,
}
