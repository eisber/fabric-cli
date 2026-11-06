// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure, opt-in v2 operation discovery, not a mutation executor.
//!
//! A single typed registration drives descriptors, help, local input validation
//! and JSON Schema/MCP projections. Lexical selectors establish neither resource
//! identity nor authorization. This crate has no identity, transport, runtime,
//! CLI, file or environment dependency.

#![cfg_attr(test, deny(unnameable_test_items))]

mod constraints;
mod registry;
mod target;

use serde::Serialize;
use std::str::FromStr;

pub use registry::{CONTRACT_VERSION, INPUT_SCHEMA_DIALECT, registrations};
pub use registry::{OperationContract, OperationRegistration, discover, discover_for_facade};
pub use target::{MAX_TARGET_BYTES, TargetKind, TargetSelector};

/// A v2 operation whose contract can be inspected without executing it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// Inspect a prospective resource's creation contract.
    Create,
    /// Inspect a resource's update contract.
    Update,
    /// Inspect an execution contract, without submitting a job.
    Run,
    /// Inspect a deletion contract, without deleting anything.
    Delete,
}

impl Operation {
    /// Stable verb vocabulary for front-end generation.
    ///
    /// Enumeration does not imply support for every target kind; exact pairs
    /// still require [`discover`] or the narrower [`registrations`] catalogue.
    pub const ALL: [Self; 4] = [Self::Create, Self::Update, Self::Run, Self::Delete];

    /// Stable, case-sensitive command spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Run => "run",
            Self::Delete => "delete",
        }
    }
}

impl FromStr for Operation {
    type Err = DiscoveryError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|operation| operation.as_str() == value)
            .ok_or(DiscoveryError::InvalidOperation)
    }
}

/// Local discovery failures. Messages never include user-supplied values.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DiscoveryError {
    /// No registered operation has this exact spelling.
    #[error("Unknown operation. Expected create, update, run or delete.")]
    InvalidOperation,
    /// The lexical selector is malformed or exceeds the local safety bound.
    #[error("Invalid target selector: {reason}.")]
    InvalidTarget {
        /// A static explanation, not the rejected selector.
        reason: &'static str,
    },
    /// An exact type, operation, facet or parameter contract is unavailable.
    #[error("Unsupported capability: {reason}.")]
    UnsupportedCapability {
        /// A static capability gap.
        reason: &'static str,
    },
    /// A body violates the registered local structural constraints.
    #[error("Invalid input at {field}: {reason}.")]
    InvalidInput {
        /// A registered field name, or `$` for the whole body.
        field: &'static str,
        /// A static constraint explanation.
        reason: &'static str,
    },
}

impl DiscoveryError {
    /// Stable machine classification, independent of display wording.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidOperation => "invalid_operation",
            Self::InvalidTarget { .. } => "invalid_target",
            Self::UnsupportedCapability { .. } => "unsupported_capability",
            Self::InvalidInput { .. } => "invalid_input",
        }
    }
}
