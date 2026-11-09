// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Warehouse native multipart content; no build, extraction or deployment.

use crate::{
    DefinitionError, DefinitionErrorKind, DefinitionLimitKind, DefinitionLimits, NativeDefinition,
    NativeDefinitionPart, SqlDatabaseDefinitionFormat, codec, wire,
};
use std::fmt;

/// Warehouse representations; intentionally not the SQLDatabase default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WarehouseDefinitionFormat {
    /// One SQL project, SQL objects and optional shared queries; service default.
    #[default]
    Sqlproj,
    /// Opaque database model package, not rows or a backup.
    Dacpac,
}

impl WarehouseDefinitionFormat {
    /// Exact native query spelling, not an output serializer or MIME type.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sqlproj => "sqlproj",
            Self::Dacpac => "dacpac",
        }
    }

    const fn shared(self) -> SqlDatabaseDefinitionFormat {
        match self {
            Self::Sqlproj => SqlDatabaseDefinitionFormat::Sqlproj,
            Self::Dacpac => SqlDatabaseDefinitionFormat::Dacpac,
        }
    }
}

impl TryFrom<&str> for WarehouseDefinitionFormat {
    type Error = DefinitionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "sqlproj" => Ok(Self::Sqlproj),
            "dacpac" => Ok(Self::Dacpac),
            _ => Err(DefinitionError::new(
                DefinitionErrorKind::UnsupportedFormat,
                None,
            )),
        }
    }
}

/// Provider media declaration, never guessed from a path or decoded bytes.
#[derive(Clone, Eq, PartialEq)]
pub enum NativeMediaType {
    /// No mediaType member was received.
    Missing,
    /// The provider explicitly sent null.
    Null,
    /// Exact advertised string, including empty or future media types.
    Value(String),
}

impl fmt::Debug for NativeMediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "NativeMediaType::Missing",
            Self::Null => "NativeMediaType::Null",
            Self::Value(_) => "NativeMediaType::Value(<native media>)",
        })
    }
}

/// Immutable complete native response and admitted opaque multipart content.
///
/// The original envelope is the fidelity surface: it includes all future fields,
/// declared/omitted format, native media, Base64 strings and original JSON bytes.
/// Admission establishes neither SQL/project/package validity nor round-trip
/// deployability. There is no mutable view, unchecked constructor or Deserialize.
pub struct WarehouseDefinition {
    native: NativeDefinition,
    format: WarehouseDefinitionFormat,
    declared_format: Option<WarehouseDefinitionFormat>,
    media_types: Vec<NativeMediaType>,
    unknown_fields: bool,
    response: Box<[u8]>,
}

impl WarehouseDefinition {
    /// Effective requested format, even when the response omits it.
    pub const fn format(&self) -> WarehouseDefinitionFormat {
        self.format
    }

    /// Exact known provider declaration; None means omitted, never null.
    pub const fn declared_format(&self) -> Option<WarehouseDefinitionFormat> {
        self.declared_format
    }

    /// Original ordered native paths and bytes, without text conversion.
    pub fn parts(&self) -> &[NativeDefinitionPart] {
        self.native.parts()
    }

    /// Original media declarations in the same order as parts.
    pub fn media_types(&self) -> &[NativeMediaType] {
        &self.media_types
    }

    /// Whether unrecognized envelope, definition or part fields were retained.
    ///
    /// Future fields remain data, not additional validated semantic coverage.
    pub const fn has_unknown_fields(&self) -> bool {
        self.unknown_fields
    }

    /// Complete original response bytes; never reconstructed or truncated.
    pub fn response_bytes(&self) -> &[u8] {
        &self.response
    }
}

impl fmt::Debug for WarehouseDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WarehouseDefinition")
            .field("format", &self.format)
            .field("part_count", &self.parts().len())
            .field("has_unknown_fields", &self.unknown_fields)
            .finish_non_exhaustive()
    }
}

/// Admit a completed native Warehouse definition response or authoritative result.
///
/// Reuses SQL multipart/Base64/path/primary admission. Warehouse-only optional
/// media and future fields are preserved in the complete original envelope;
/// SQLDatabase's strict profile and dacpac default remain unchanged.
///
/// # Errors
/// Rejects malformed/duplicate structural fields, null/unknown/mismatched format,
/// unsafe/colliding paths, unsupported parts/encoding, malformed Base64 and all
/// six explicit inclusive bounds. Unknown fields never repair missing content.
pub fn parse_warehouse_definition_response(
    json: &[u8],
    requested: WarehouseDefinitionFormat,
    limits: DefinitionLimits,
) -> Result<WarehouseDefinition, DefinitionError> {
    if json.len() > limits.max_wire_bytes.get() {
        return Err(DefinitionError::limit(DefinitionLimitKind::WireBytes, None));
    }
    let parsed = wire::parse_profile(json, limits, true, true)?;
    let declared_format = parsed
        .format
        .as_deref()
        .map(WarehouseDefinitionFormat::try_from)
        .transpose()?;
    if declared_format.is_some_and(|format| format != requested) {
        return Err(DefinitionError::new(
            DefinitionErrorKind::FormatMismatch,
            None,
        ));
    }
    codec::validate_primary(requested.shared(), &parsed.parts)?;
    let native = NativeDefinition::admitted(requested.shared(), parsed.parts, parsed.payloads);
    Ok(WarehouseDefinition {
        native,
        format: requested,
        declared_format,
        media_types: parsed.media_types,
        unknown_fields: parsed.unknown_fields,
        response: json.into(),
    })
}
