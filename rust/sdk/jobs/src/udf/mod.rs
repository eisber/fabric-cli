// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure published-function input and native-response validation.
//!
//! The caller supplies the actual selected schema validator and explicit JSON
//! budgets. These consumers do not discover a binding, inspect source, request
//! credentials, dispatch HTTP, inject defaults or create a scheduler handle.
//! They are not a production invocation capability or a general schema engine.

mod json;
mod response;

pub use response::{UdfProtocolIssue, UdfResponse, UdfResponseKind};
use serde_json::Value;
use std::{fmt, num::NonZeroUsize};

/// Largest supported container depth with the pinned decoder's recursion guard.
///
/// `serde_json` 1.0.154 starts at 128 and rejects entry when decrementing to zero,
/// so at most 127 simultaneously nested objects/arrays can be decoded safely.
/// This is a local decoder capability, not a service limit or production default.
pub const MAX_UDF_JSON_DEPTH: usize = 127;

/// Caller-selected finite JSON budgets, not public-service MB conversions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UdfJsonLimits {
    /// Maximum input or complete response body bytes.
    pub max_bytes: NonZeroUsize,
    /// Maximum simultaneously nested JSON containers, including the root.
    ///
    /// Supported values are 1 through [`MAX_UDF_JSON_DEPTH`], inclusive.
    /// Larger configurations are unsupported even for shallow input.
    pub max_depth: NonZeroUsize,
    /// Maximum JSON values plus object member names.
    pub max_nodes: NonZeroUsize,
}

impl UdfJsonLimits {
    /// Check decoder support without inspecting input or calling the schema.
    ///
    /// # Errors
    /// Returns [`UdfValidationError::UnsupportedLimits`] for an excessive depth
    /// configuration. Byte/node budgets remain caller-selected and positive.
    pub fn validate(self) -> Result<Self, UdfValidationError> {
        if self.max_depth.get() > MAX_UDF_JSON_DEPTH {
            return Err(UdfValidationError::UnsupportedLimits);
        }
        Ok(self)
    }
}

/// Bounded validation diagnostics; no argument, source or provider text is echoed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UdfValidationError {
    /// The caller's depth configuration exceeds the supported decoder capability.
    UnsupportedLimits,
    /// The selected schema cannot be validated by the supplied producer.
    UnsupportedSchema,
    /// Input does not satisfy the actual selected function schema.
    SchemaViolation,
    /// The input is not one complete valid JSON value.
    InvalidJson,
    /// Two object keys decode to the same case-sensitive name.
    DuplicateKey,
    /// An integer token cannot be represented without losing its native type.
    IntegerOutOfRange,
    /// The caller's byte, depth or node bound was exceeded.
    JsonLimit,
    /// Function arguments must be a direct JSON object.
    ArgumentsNotObject,
}

impl fmt::Display for UdfValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedLimits => {
                "JSON depth configuration exceeds the supported maximum of 127"
            }
            Self::UnsupportedSchema => "selected function schema is unsupported",
            Self::SchemaViolation => "arguments do not satisfy the selected function schema",
            Self::InvalidJson => "expected one complete JSON value",
            Self::DuplicateKey => "duplicate JSON object member",
            Self::IntegerOutOfRange => "JSON integer cannot be represented without loss",
            Self::JsonLimit => "caller-selected JSON bound exceeded",
            Self::ArgumentsNotObject => "function arguments must be a direct JSON object",
        })
    }
}

impl std::error::Error for UdfValidationError {}

/// Pure validation supplied by the owner of the actual selected function schema.
///
/// A metadata type name, example or guessed signature is not a schema. The
/// implementation must reject unsupported constructs explicitly, never fetch
/// remote references, execute source or mutate/inject defaults into arguments.
/// It must perform no credential or transport work.
pub trait UdfInputSchema {
    /// Check the entire selected schema's supported validation coverage.
    ///
    /// This runs before parsing arguments, including for omitted optional fields.
    /// Unsupported constructs cannot be ignored because input did not visit them.
    fn preflight(&self) -> Result<(), UdfValidationError>;

    /// Validate native JSON argument values without normalization or mutation.
    fn validate(&self, arguments: &Value) -> Result<(), UdfValidationError>;
}

/// Exact validated direct arguments, with no parameters/executionData wrapper.
///
/// The original bytes remain authoritative for dispatch. `value` exposes native
/// types for local consumers; omitted members, null and defaults stay distinct.
pub struct UdfArguments {
    bytes: Box<[u8]>,
    value: Value,
}

impl UdfArguments {
    /// Original complete JSON bytes, never suitable for default logging.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Validated direct argument object; no defaults have been inserted.
    pub fn value(&self) -> &Value {
        &self.value
    }
}

impl fmt::Debug for UdfArguments {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UdfArguments(<redacted>)")
    }
}

/// Validate one bounded direct function-specific JSON object without effects.
///
/// No schema dialect or keyword set is inferred here. Integer overflow,
/// duplicate keys (including escaped names), trailing content and caller budget
/// violations fail before the producer's value validator. Successful validation
/// preserves the original bytes and every native JSON argument type.
///
/// # Errors
/// Returns a sanitized schema, JSON, shape or caller-budget diagnostic. An
/// unsupported depth configuration fails before schema callbacks or parsing.
pub fn validate_arguments(
    bytes: &[u8],
    schema: &dyn UdfInputSchema,
    limits: UdfJsonLimits,
) -> Result<UdfArguments, UdfValidationError> {
    let limits = limits.validate()?;
    schema.preflight()?;
    let value = json::parse(bytes, limits)?;
    if !value.is_object() {
        return Err(UdfValidationError::ArgumentsNotObject);
    }
    schema.validate(&value)?;
    Ok(UdfArguments {
        bytes: bytes.into(),
        value,
    })
}
