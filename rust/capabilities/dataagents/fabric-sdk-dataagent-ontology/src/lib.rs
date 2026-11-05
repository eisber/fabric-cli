// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! DataAgent-owned ontology attachment, selection and source authoring.
//!
//! This package never queries an ontology, invokes a model, mutates an Ontology
//! item, acquires credentials, or publishes implicitly.

pub mod inputs;
pub mod models;
pub mod paging;
mod projection;
pub mod private;
pub mod public;
pub mod result;
pub mod scope;

use thiserror::Error;

/// Local validation failures, before an executor may be invoked.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ValidationError {
    /// An identifier is not a nonzero, canonicalizable GUID.
    #[error("a nonzero GUID is required")]
    InvalidGuid,
    /// An opaque selector is empty, oversized, or contains control characters.
    #[error("the opaque selector is invalid")]
    InvalidSelector,
    /// Authored input exceeds a source-defined UTF-16 length or byte bound.
    #[error("authored input exceeds its supported bound")]
    InputLimit,
    /// The display name is not a safe single storage path segment.
    #[error("a safe nonempty display name is required")]
    InvalidDisplayName,
    /// Explicit selection must contain between one and 500 returned entity IDs.
    #[error("explicit entity selection requires one to 500 identifiers")]
    InvalidSelection,
    /// The ETag value cannot be sent as an exact If-Match header.
    #[error("the exact ETag is invalid")]
    InvalidETag,
    /// An ETag belongs to a different caller, agent, stage, ontology or resource.
    #[error("the validator does not belong to this caller and resource")]
    ValidatorScope,
    /// A route or option is unsupported on the explicitly selected surface.
    #[error("the operation is not available on the selected API surface")]
    UnsupportedSurface,
}

pub(crate) fn text_bound(text: &str, maximum: usize) -> Result<(), ValidationError> {
    if text.encode_utf16().count() > maximum {
        Err(ValidationError::InputLimit)
    } else {
        Ok(())
    }
}

pub(crate) fn selector(text: &str, maximum: usize) -> Result<(), ValidationError> {
    if text.is_empty()
        || text.chars().any(char::is_control)
        || text.encode_utf16().count() > maximum
    {
        Err(ValidationError::InvalidSelector)
    } else {
        Ok(())
    }
}
