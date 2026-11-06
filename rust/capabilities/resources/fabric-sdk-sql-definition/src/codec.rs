// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    DefinitionError, DefinitionErrorKind as Kind, DefinitionLimitKind as Limit, DefinitionLimits,
    NativeDefinition, NativeDefinitionPart, SqlDatabaseDefinitionFormat, paths::Paths, wire,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::io::{self, Write};

/// Admit a bare native definition object, defaulting omitted format to dacpac.
///
/// # Errors
/// Rejects malformed/duplicate/unknown fields, unsupported media or payloads,
/// unsafe/colliding paths, invalid Base64 and explicit content bounds.
pub fn parse_sql_database_definition(
    json: &[u8],
    limits: DefinitionLimits,
) -> Result<NativeDefinition, DefinitionError> {
    parse(json, None, limits)
}

/// Admit the authoritative `{definition: ...}` response or LRO result body.
///
/// Omitted response format inherits the effective requested format. An explicit
/// mismatch fails. Metadata, permission-error and operation-monitor bodies are
/// not successful definitions. HTTP status and operation evidence stay outside
/// this pure codec.
///
/// # Errors
/// Retains content classifications without raw JSON, paths or payloads.
pub fn parse_sql_database_definition_response(
    json: &[u8],
    requested_format: SqlDatabaseDefinitionFormat,
    limits: DefinitionLimits,
) -> Result<NativeDefinition, DefinitionError> {
    parse(json, Some(requested_format), limits)
}

/// Admit explicit ordered native parts and encode their bytes exactly once.
///
/// No file discovery, text conversion, project interpretation or I/O occurs.
/// The caller's already-allocated input is not retroactively memory-bounded.
///
/// # Errors
/// Rejects unsupported content, unsafe/colliding paths and inclusive limits.
pub fn sql_database_definition_from_parts(
    format: SqlDatabaseDefinitionFormat,
    parts: Vec<NativeDefinitionPart>,
    limits: DefinitionLimits,
) -> Result<NativeDefinition, DefinitionError> {
    if parts.len() > limits.max_parts.get() {
        return Err(DefinitionError::limit(Limit::PartCount, None));
    }
    let mut admission = Admission::new(limits);
    let mut payloads = Vec::with_capacity(parts.len());
    let mut encoded_total = 0usize;
    for (index, part) in parts.iter().enumerate() {
        admission.path(&part.path, index)?;
        admission.bytes(part.bytes.len(), index)?;
        encoded_total = encoded_total
            .checked_add(encoded_length(part.bytes.len())?)
            .filter(|total| *total <= limits.max_wire_bytes.get())
            .ok_or_else(|| DefinitionError::limit(Limit::WireBytes, Some(index)))?;
        payloads.push(STANDARD.encode(&part.bytes));
    }
    finish(format, parts, payloads, limits)
}

fn parse(
    json: &[u8],
    requested: Option<SqlDatabaseDefinitionFormat>,
    limits: DefinitionLimits,
) -> Result<NativeDefinition, DefinitionError> {
    if json.len() > limits.max_wire_bytes.get() {
        return Err(DefinitionError::limit(Limit::WireBytes, None));
    }
    let parsed = wire::parse(json, limits, requested.is_some())?;
    let format = match parsed.format {
        Some(value) => SqlDatabaseDefinitionFormat::try_from(value.as_str())?,
        None => requested.unwrap_or_default(),
    };
    if requested.is_some_and(|expected| expected != format) {
        return Err(DefinitionError::new(Kind::FormatMismatch, None));
    }
    finish(format, parsed.parts, parsed.payloads, limits)
}

fn finish(
    format: SqlDatabaseDefinitionFormat,
    parts: Vec<NativeDefinitionPart>,
    payloads: Vec<String>,
    limits: DefinitionLimits,
) -> Result<NativeDefinition, DefinitionError> {
    let mut primary = false;
    for (index, part) in parts.iter().enumerate() {
        if part.path == ".platform" {
            continue;
        }
        let is_primary = match format {
            SqlDatabaseDefinitionFormat::Dacpac if part.path.ends_with(".dacpac") => true,
            SqlDatabaseDefinitionFormat::Sqlproj if part.path.ends_with(".sqlproj") => true,
            SqlDatabaseDefinitionFormat::Sqlproj if part.path.ends_with(".sql") => false,
            _ => return Err(DefinitionError::new(Kind::UnsupportedPart, Some(index))),
        };
        if is_primary {
            if primary {
                return Err(DefinitionError::new(
                    Kind::MultiplePrimaryParts,
                    Some(index),
                ));
            }
            if part.bytes.is_empty() {
                return Err(DefinitionError::new(Kind::InvalidShape, Some(index)));
            }
            primary = true;
        }
    }
    if !primary {
        return Err(DefinitionError::new(Kind::MissingPrimaryPart, None));
    }
    let definition = NativeDefinition::admitted(format, parts, payloads);
    let mut counter = WireCounter {
        written: 0,
        maximum: limits.max_wire_bytes.get(),
    };
    serde_json::to_writer(&mut counter, &definition)
        .map_err(|_| DefinitionError::limit(Limit::WireBytes, None))?;
    Ok(definition)
}

fn encoded_length(bytes: usize) -> Result<usize, DefinitionError> {
    bytes
        .checked_div(3)
        .and_then(|groups| groups.checked_add(usize::from(!bytes.is_multiple_of(3))))
        .and_then(|groups| groups.checked_mul(4))
        .ok_or_else(|| DefinitionError::limit(Limit::WireBytes, None))
}

pub(crate) struct Admission {
    limits: DefinitionLimits,
    paths: Paths,
    decoded_total: usize,
}

impl Admission {
    pub(crate) fn new(limits: DefinitionLimits) -> Self {
        Self {
            limits,
            paths: Paths::default(),
            decoded_total: 0,
        }
    }

    pub(crate) fn path(&mut self, path: &str, index: usize) -> Result<(), DefinitionError> {
        self.paths.insert(path, self.limits, index)
    }

    fn bytes(&mut self, count: usize, index: usize) -> Result<(), DefinitionError> {
        if count > self.limits.max_part_bytes.get() {
            return Err(DefinitionError::limit(Limit::PartBytes, Some(index)));
        }
        self.decoded_total = self
            .decoded_total
            .checked_add(count)
            .filter(|total| *total <= self.limits.max_decoded_bytes.get())
            .ok_or_else(|| DefinitionError::limit(Limit::DecodedBytes, Some(index)))?;
        Ok(())
    }

    pub(crate) fn decode(
        &mut self,
        path: String,
        payload: &str,
        index: usize,
    ) -> Result<NativeDefinitionPart, DefinitionError> {
        self.path(&path, index)?;
        if !payload.len().is_multiple_of(4) {
            return Err(DefinitionError::new(Kind::InvalidBase64, Some(index)));
        }
        let padding = payload
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'=')
            .count();
        if padding > 2 {
            return Err(DefinitionError::new(Kind::InvalidBase64, Some(index)));
        }
        let count = payload
            .len()
            .checked_div(4)
            .and_then(|groups| groups.checked_mul(3))
            .and_then(|bytes| bytes.checked_sub(padding))
            .ok_or_else(|| DefinitionError::new(Kind::InvalidBase64, Some(index)))?;
        self.bytes(count, index)?;
        let bytes = STANDARD
            .decode(payload)
            .map_err(|_| DefinitionError::new(Kind::InvalidBase64, Some(index)))?;
        if bytes.len() != count || STANDARD.encode(&bytes) != payload {
            return Err(DefinitionError::new(Kind::InvalidBase64, Some(index)));
        }
        Ok(NativeDefinitionPart { path, bytes })
    }
}

struct WireCounter {
    written: usize,
    maximum: usize,
}

impl Write for WireCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.written = self
            .written
            .checked_add(bytes.len())
            .filter(|total| *total <= self.maximum)
            .ok_or_else(|| io::Error::other("definition wire limit"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroUsize;

    #[test]
    fn base64_length_arithmetic_rejects_overflow_without_allocating() {
        assert_eq!(encoded_length(0).unwrap(), 0);
        assert_eq!(encoded_length(1).unwrap(), 4);
        assert_eq!(encoded_length(3).unwrap(), 4);
        assert_eq!(encoded_length(4).unwrap(), 8);
        assert_eq!(
            encoded_length(usize::MAX).unwrap_err().kind,
            Kind::Limit(Limit::WireBytes)
        );
    }

    #[test]
    fn aggregate_and_serialized_byte_arithmetic_reject_overflow() {
        let maximum = NonZeroUsize::new(usize::MAX).unwrap();
        let mut admission = Admission::new(DefinitionLimits {
            max_wire_bytes: maximum,
            max_decoded_bytes: maximum,
            max_part_bytes: maximum,
            max_parts: maximum,
            max_path_bytes: maximum,
            max_depth: maximum,
        });
        admission.bytes(usize::MAX, 0).unwrap();
        assert_eq!(
            admission.bytes(1, 1).unwrap_err().kind,
            Kind::Limit(Limit::DecodedBytes)
        );
        let mut counter = WireCounter {
            written: usize::MAX,
            maximum: usize::MAX,
        };
        assert!(counter.write(b"x").is_err());
    }
}
