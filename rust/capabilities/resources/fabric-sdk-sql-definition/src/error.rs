// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::fmt;

/// The explicit inclusive bound that rejected content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefinitionLimitKind {
    /// Input JSON or compact serialized definition bytes.
    WireBytes,
    /// Aggregate decoded native bytes.
    DecodedBytes,
    /// Decoded bytes in one part.
    PartBytes,
    /// Number of definition parts.
    PartCount,
    /// UTF-8 bytes in a native relative path.
    PathBytes,
    /// Number of native path components.
    Depth,
}

/// Content admission failures, independent of HTTP and filesystem errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefinitionErrorKind {
    /// Malformed JSON, invalid JSON UTF-8 or trailing input.
    InvalidJson,
    /// Missing, duplicate, unknown or incorrectly typed structural fields.
    InvalidShape,
    /// The response has no definition member.
    MissingDefinition,
    /// The native format is not exactly dacpac or sqlproj.
    UnsupportedFormat,
    /// A response declares a format different from the requested format.
    FormatMismatch,
    /// The payload type is not exactly InlineBase64.
    UnsupportedPayloadType,
    /// The payload is not canonical standard Base64.
    InvalidBase64,
    /// A part is outside the bounded lowercase extension profile.
    UnsupportedPart,
    /// The definition has no primary DACPAC or project.
    MissingPrimaryPart,
    /// The definition has more than one primary DACPAC or project.
    MultiplePrimaryParts,
    /// A path violates portable relative-path admission.
    UnsafePath,
    /// An exact native path occurs more than once.
    DuplicatePath,
    /// Paths or parent components have differing case aliases.
    CaseCollision,
    /// A path is used as both a file and a parent directory.
    FileDirectoryCollision,
    /// An explicit caller bound was exceeded.
    Limit(DefinitionLimitKind),
}

/// A redacted content error; never contains payloads, paths or provider text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DefinitionError {
    /// Stable content or limit classification.
    pub kind: DefinitionErrorKind,
    /// Zero-based part ordinal when the failure belongs to a particular part.
    pub part_index: Option<usize>,
}

impl DefinitionError {
    pub(crate) const fn new(kind: DefinitionErrorKind, part_index: Option<usize>) -> Self {
        Self { kind, part_index }
    }

    pub(crate) const fn limit(kind: DefinitionLimitKind, part_index: Option<usize>) -> Self {
        Self::new(DefinitionErrorKind::Limit(kind), part_index)
    }
}

impl fmt::Display for DefinitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SQLDatabase definition rejected: {:?}", self.kind)
    }
}

impl std::error::Error for DefinitionError {}
