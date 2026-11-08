// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline DACPAC structural checking over the existing native definition codec.
//!
//! Registration uses the shared neutral registry. No schema evaluator, native
//! package parser, filesystem, credentials, downloads or service calls are added.

use crate::{
    DefinitionError, DefinitionErrorKind as Kind, DefinitionLimits, SqlDatabaseDefinitionFormat,
    parse_sql_database_definition,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;

/// Original neutral registry types, not a second validation catalogue.
pub use fabric_v2_contracts as v2;

/// Stable identity for this local structural profile, not a native operation ID.
pub const OPERATION_ID: &str = "fabric.local.sql-database.definition-dacpac.validate.v1";
/// Version of these local coverage/severity/location rules.
pub const RULE_VERSION: &str = "1.0";
/// Exact registered native representation.
pub const REPRESENTATION: &str = "definition.dacpac";
/// JSON dialect of the inert schema projection, not an installed evaluator.
pub const SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Atomically register the existing codec's first structural validation profile.
///
/// This schema projects the envelope shape only. Base64, path collisions and
/// primary-part admission are additional checks performed by the existing codec.
///
/// # Errors
/// Retains neutral duplicate/conflicting-registration errors without replacing
/// an existing operation, alias or binding.
pub fn register(registry: &mut v2::Registry) -> Result<(), v2::ContractError> {
    let mut candidate = registry.clone();
    candidate.register(v2::OperationDescriptor::new(
        OPERATION_ID,
        v2::Verb::Validate,
        input_schema(),
        json!({
            "$schema":SCHEMA_DIALECT,
            "type":"object",
            "properties":{
                "operationId":{"const":OPERATION_ID},
                "ruleVersion":{"const":RULE_VERSION},
                "mediaType":{"const":"application/json"},
                "nativeFormat":{"const":"dacpac"},
                "valid":{"type":"boolean"},
                "findings":{"type":"array","maxItems":1,"items":{
                    "type":"object",
                    "properties":{
                        "ruleId":{"type":"string"},
                        "severity":{"const":"error"},
                        "pointer":{"type":"string"},
                        "message":{"type":"string"}
                    },
                    "required":["ruleId","severity","pointer","message"],
                    "additionalProperties":false
                }},
                "coverage":{"type":"object"}
            },
            "required":["operationId","ruleVersion","mediaType","nativeFormat","valid","findings","coverage"],
            "additionalProperties":false,
            "description":"Local structural report; not native package validity or service acceptance."
        }),
        vec![v2::Effect::LocalRead],
    )?)?;
    candidate.bind(v2::Binding::new(
        "SQLDatabase",
        "src",
        REPRESENTATION,
        v2::Generation::Any,
        OPERATION_ID,
        v2::Availability::Available,
    )?)?;
    *registry = candidate;
    Ok(())
}

fn input_schema() -> Value {
    json!({
        "$schema": SCHEMA_DIALECT,
        "type": "object",
        "properties": {
            "format": {"const":"dacpac","default":"dacpac"},
            "parts": {
                "type":"array",
                "minItems":1,
                "items":{
                    "type":"object",
                    "properties":{
                        "path":{"type":"string","minLength":1},
                        "payload":{"type":"string","contentEncoding":"base64"},
                        "payloadType":{"const":"InlineBase64"}
                    },
                    "required":["path","payload","payloadType"],
                    "additionalProperties":false
                }
            }
        },
        "required":["parts"],
        "additionalProperties":false,
        "x-fabric-validation":{
            "ruleVersion":RULE_VERSION,
            "engine":"fabric-sdk-sql-definition",
            "sourceRevision":"cc183dd43140350eb1f23af0636332150d1f6d50",
            "sourceUrl":"https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/sql-database-definition",
            "sourceDocumentRevision":"51e34c7286719812e28de4d44b0adeb84e062835",
            "coverage":"structural_subset_only",
            "schemaEvaluation":"typed_codec_not_general_json_schema"
        },
        "description":"Bare SQLDatabase DACPAC definition. Additional codec checks require one nonempty lowercase .dacpac primary, optional .platform, canonical Base64 and portable collision-free paths. Native bytes are opaque."
    })
}

/// A finding's severity is separate from transport/engine failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A supplied document violates the checked structural profile.
    Error,
}

/// First failing rule, with a safe ordinal location rather than source content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// Stable local rule classification.
    pub rule_id: &'static str,
    /// Structural findings in this profile are blocking errors.
    pub severity: Severity,
    /// RFC 6901 pointer; empty means the root or an unavailable finer location.
    pub pointer: String,
    /// Static explanation, never a supplied path, payload or parser message.
    pub message: &'static str,
}

/// Coverage explicitly excludes native semantics and external acceptance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    /// Only a structural subset is checked, even with zero findings.
    pub complete: bool,
    /// Families checked by the existing codec, with fail-fast admission.
    pub checked: &'static [&'static str],
    /// No remote or native semantic inference is made.
    pub not_checked: &'static [&'static str],
    /// The existing codec returns the first finding, not all possible findings.
    pub finding_policy: &'static str,
}

/// Completed local check; valid is not permission or native package acceptance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    /// Stable local operation registration.
    pub operation_id: &'static str,
    /// Exact rule/profile version, not the Fabric REST API version.
    pub rule_version: &'static str,
    /// The supplied input media.
    pub media_type: &'static str,
    /// Effective codec format; omission in a document uses the native default.
    pub native_format: &'static str,
    /// Whether the checked structural rules admitted these exact input bytes.
    pub valid: bool,
    /// Complete empty list or the first structural failure.
    pub findings: Vec<Finding>,
    /// Explicit nonclaims retained on both valid and invalid results.
    pub coverage: Coverage,
}

/// A check could not establish a verdict under the requested profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    /// A rule set/version, media or native format is not supported.
    Unsupported,
    /// An explicit wire/decoded/part/path/depth limit prevented complete checking.
    Budget(DefinitionError),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unsupported => "The exact local media, format or rule version is unsupported.",
            Self::Budget(_) => {
                "An explicit validation budget was exceeded; no verdict was established."
            }
        })
    }
}

impl std::error::Error for ValidationError {}

/// Check supplied bytes using the already released strict native codec.
///
/// The JSON body is bare `{format?, parts}`; response envelopes, archives and
/// directory acquisition are not silently converted. No input is normalized,
/// written or executed. SDK callers may reuse this before explicit create/update
/// but must still bind approval and native service evidence independently.
///
/// # Errors
/// Unknown media/format/version and exhausted budgets produce no valid/invalid
/// report. Malformed/duplicate JSON and other structural errors yield an invalid
/// report with a safe location. Decoded native payloads remain opaque.
pub fn validate(
    bytes: &[u8],
    media_type: &str,
    native_format: &str,
    rule_version: &str,
    limits: DefinitionLimits,
) -> Result<ValidationReport, ValidationError> {
    if media_type != "application/json" || native_format != "dacpac" || rule_version != RULE_VERSION
    {
        return Err(ValidationError::Unsupported);
    }
    if bytes.len() > limits.max_wire_bytes.get() {
        return Err(ValidationError::Budget(DefinitionError::limit(
            crate::DefinitionLimitKind::WireBytes,
            None,
        )));
    }
    // Select the profile independently of part admission. Ignored fields are
    // still checked by the original strict codec, including duplicate keys.
    if serde_json::from_slice::<DocumentFormat>(bytes)
        .is_ok_and(|document| document.format.is_some_and(|format| format != "dacpac"))
    {
        return Err(ValidationError::Unsupported);
    }
    let findings = match parse_sql_database_definition(bytes, limits) {
        Ok(definition) if definition.format() == SqlDatabaseDefinitionFormat::Dacpac => Vec::new(),
        Ok(_) => return Err(ValidationError::Unsupported),
        Err(error) if matches!(error.kind, Kind::Limit(_)) => {
            return Err(ValidationError::Budget(error));
        }
        Err(error)
            if matches!(
                error.kind,
                Kind::UnsupportedFormat | Kind::UnsupportedPayloadType | Kind::UnsupportedPart
            ) =>
        {
            return Err(ValidationError::Unsupported);
        }
        Err(error) => vec![finding(error)],
    };
    Ok(ValidationReport {
        operation_id: OPERATION_ID,
        rule_version: RULE_VERSION,
        media_type: "application/json",
        native_format: "dacpac",
        valid: findings.is_empty(),
        findings,
        coverage: Coverage {
            complete: false,
            checked: &[
                "json_envelope",
                "part_shape",
                "portable_paths",
                "canonical_base64",
                "primary_part",
            ],
            not_checked: &[
                "dacpac_package_semantics",
                "decoded_platform_json",
                "permissions",
                "service_acceptance",
                "reference_existence",
                "roundtrip",
                "runtime_correctness",
            ],
            finding_policy: "first_failure",
        },
    })
}

#[derive(Deserialize)]
struct DocumentFormat {
    format: Option<String>,
}

fn finding(error: DefinitionError) -> Finding {
    let (rule_id, member, message) = match error.kind {
        Kind::InvalidJson => (
            "invalid_json",
            "",
            "Expected one strict UTF-8 JSON document.",
        ),
        Kind::InvalidShape | Kind::MissingDefinition => (
            "invalid_envelope",
            "",
            "Missing, duplicate, unknown or incorrectly typed envelope/part fields.",
        ),
        Kind::InvalidBase64 => (
            "invalid_base64",
            "/payload",
            "Expected canonical standard Base64.",
        ),
        Kind::UnsafePath => (
            "unsafe_path",
            "/path",
            "Expected a portable relative part path.",
        ),
        Kind::DuplicatePath | Kind::CaseCollision | Kind::FileDirectoryCollision => (
            "path_collision",
            "/path",
            "Part paths or parent components collide.",
        ),
        Kind::MissingPrimaryPart => (
            "missing_primary_part",
            "",
            "One nonempty DACPAC primary part is required.",
        ),
        Kind::MultiplePrimaryParts => (
            "multiple_primary_parts",
            "/path",
            "Only one DACPAC primary part is admitted.",
        ),
        Kind::FormatMismatch
        | Kind::UnsupportedFormat
        | Kind::UnsupportedPayloadType
        | Kind::UnsupportedPart
        | Kind::Limit(_) => (
            "unsupported",
            "",
            "The requested profile cannot establish a verdict.",
        ),
    };
    Finding {
        rule_id,
        severity: Severity::Error,
        pointer: error
            .part_index
            .map_or_else(String::new, |index| format!("/parts/{index}{member}")),
        message,
    }
}
