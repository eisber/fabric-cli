// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{DefinitionError, DefinitionErrorKind};
use serde::{
    Serialize, Serializer,
    ser::{SerializeSeq, SerializeStruct},
};
use std::{fmt, num::NonZeroUsize};

/// Supported native SQLDatabase representations, not MIME types or backup modes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SqlDatabaseDefinitionFormat {
    /// Native schema/model package; the documented service default.
    #[default]
    Dacpac,
    /// One project and its ordered SQL/shared-query parts.
    Sqlproj,
}

impl SqlDatabaseDefinitionFormat {
    /// Exact service format spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dacpac => "dacpac",
            Self::Sqlproj => "sqlproj",
        }
    }
}

impl TryFrom<&str> for SqlDatabaseDefinitionFormat {
    type Error = DefinitionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "dacpac" => Ok(Self::Dacpac),
            "sqlproj" => Ok(Self::Sqlproj),
            _ => Err(DefinitionError::new(
                DefinitionErrorKind::UnsupportedFormat,
                None,
            )),
        }
    }
}

impl Serialize for SqlDatabaseDefinitionFormat {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Explicit inclusive content bounds; no numeric defaults are selected here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DefinitionLimits {
    /// Maximum input JSON and compact serialized bare-definition bytes.
    ///
    /// Callers separately bound enclosing requests, responses and pretty output.
    pub max_wire_bytes: NonZeroUsize,
    /// Maximum aggregate decoded native bytes.
    pub max_decoded_bytes: NonZeroUsize,
    /// Maximum decoded bytes in each part.
    pub max_part_bytes: NonZeroUsize,
    /// Maximum number of parts.
    pub max_parts: NonZeroUsize,
    /// Maximum UTF-8 bytes in each native path.
    pub max_path_bytes: NonZeroUsize,
    /// Maximum slash-separated components in each native path.
    pub max_depth: NonZeroUsize,
}

/// An owned native path and opaque bytes, not a local file or write capability.
///
/// Standalone parts are unvalidated inputs. Only the constructors/parsers of
/// [`NativeDefinition`] admit them as a complete definition.
#[derive(Eq, PartialEq)]
pub struct NativeDefinitionPart {
    /// Exact slash-separated service-relative path, without normalization.
    pub path: String,
    /// Exact native bytes; never converted through text.
    pub bytes: Vec<u8>,
}

impl fmt::Debug for NativeDefinitionPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeDefinitionPart")
            .field("path", &"<native path>")
            .field("bytes", &"<native content>")
            .finish()
    }
}

/// An immutable admitted definition usable directly by SDK consumers.
///
/// Serialization writes the bare native definition with an explicit effective
/// format and the original validated Base64 strings. There is no unchecked
/// public constructor, mutable part view or `Deserialize` implementation.
/// Admission does not certify package validity or safe local filesystem access.
///
/// ```compile_fail
/// use fabric_sdk_sql_definition::NativeDefinition;
/// let unchecked = serde_json::from_str::<NativeDefinition>("{}");
/// ```
pub struct NativeDefinition {
    format: SqlDatabaseDefinitionFormat,
    parts: Vec<NativeDefinitionPart>,
    payloads: Vec<String>,
}

impl NativeDefinition {
    pub(crate) fn admitted(
        format: SqlDatabaseDefinitionFormat,
        parts: Vec<NativeDefinitionPart>,
        payloads: Vec<String>,
    ) -> Self {
        Self {
            format,
            parts,
            payloads,
        }
    }

    /// Effective native media/format, including resolved service defaults.
    pub const fn format(&self) -> SqlDatabaseDefinitionFormat {
        self.format
    }

    /// Immutable native paths and bytes in caller/service order.
    pub fn parts(&self) -> &[NativeDefinitionPart] {
        &self.parts
    }
}

impl fmt::Debug for NativeDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeDefinition")
            .field("format", &self.format)
            .field("part_count", &self.parts.len())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct EncodedPart<'a> {
    path: &'a str,
    payload: &'a str,
    #[serde(rename = "payloadType")]
    payload_type: &'static str,
}

struct EncodedParts<'a>(&'a NativeDefinition);

impl Serialize for EncodedParts<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.parts.len()))?;
        for (part, payload) in self.0.parts.iter().zip(&self.0.payloads) {
            sequence.serialize_element(&EncodedPart {
                path: &part.path,
                payload,
                payload_type: "InlineBase64",
            })?;
        }
        sequence.end()
    }
}

impl Serialize for NativeDefinition {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_struct("NativeDefinition", 2)?;
        object.serialize_field("format", &self.format)?;
        object.serialize_field("parts", &EncodedParts(self))?;
        object.end()
    }
}
