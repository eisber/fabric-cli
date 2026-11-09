// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ReportError, validate_format};
use crate::representations::DefinitionRead;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeSet, fmt};

/// Observed native format, distinguishing provider declaration from part layout.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatEvidence {
    /// Original provider format, absent when not declared.
    pub declared: Option<String>,
    /// Recognized part layout, absent for missing/unknown layout.
    pub layout: Option<String>,
}

/// Scope-specific reasons why native source must not be claimed complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum IncompleteReason {
    /// No required semantic-model binding/settings part.
    MissingDefinitionProperties,
    /// Required enhanced/legacy report structure is absent.
    MissingReportStructure,
    /// A required report/page part is not a syntactically valid JSON object.
    InvalidReportStructure,
    /// Provider format is not recognized; source remains opaque.
    UnknownFormat,
    /// Definition properties version is absent or unsupported.
    UnknownVersion,
    /// The model-reference schema generation is unsupported; reference stays inert.
    UnknownBindingSchema,
    /// A part uses a future encoding; it has not been dropped or guessed.
    UnsupportedPayloadType,
    /// Model reference absent; no related-model identity is synthesized.
    MissingModelReference,
}

/// Exact native part identity and optional provider-advertised media.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativePartEvidence {
    /// Exact native path, not normalized into a filesystem destination.
    pub path: String,
    /// Exact source payload-type spelling.
    pub payload_type: String,
    /// Provider media only; no type is guessed from the file extension.
    pub media_type: Option<String>,
}

/// Read evidence separate from the untouched native definition envelope.
#[derive(Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionEvidence {
    /// Provider declaration and recognized layout, not a converted format.
    pub format: FormatEvidence,
    /// Exact definition.pbir version when decoded and advertised.
    pub version: Option<String>,
    /// Exact enhanced definition/version.json version, distinct from definition.pbir.
    pub native_version: Option<String>,
    /// Original schema URL as data; never fetched during inspection.
    pub schema: Option<String>,
    /// Original byPath/byConnection reference, not a resolved or authorized model.
    pub model_reference: Option<Value>,
    /// Model navigation requires explicit resolution and separate authorization.
    ///
    /// This is always false: report metadata/source read is not that evidence.
    pub related_model_authorized: bool,
    /// Native part paths, encoding and media in original order.
    pub parts: Vec<NativePartEvidence>,
    /// Empty means recognized native required structure, not complete service state.
    pub incomplete: Vec<IncompleteReason>,
}

impl fmt::Debug for DefinitionEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DefinitionEvidence")
            .field("incomplete", &self.incomplete)
            .finish_non_exhaustive()
    }
}

/// Untouched Report native envelope and classified coverage.
///
/// Structural coverage does not validate every JSON schema, authorize the
/// related model, include model data/labels/ACLs or establish service backups.
#[derive(Clone, PartialEq)]
pub struct ReportDefinition {
    read: DefinitionRead,
    evidence: DefinitionEvidence,
}

impl ReportDefinition {
    /// Inspect an immediate or observed completed shared definition result.
    ///
    /// # Errors
    /// Rejects acceptance, malformed binding JSON, duplicate paths, corrupt
    /// InlineBase64, conflicting layouts and a mismatched requested format.
    /// Missing required parts/unknown format or version retain partial evidence.
    pub fn from_read(
        read: DefinitionRead,
        requested_format: Option<&str>,
    ) -> Result<Self, ReportError> {
        validate_format(requested_format)?;
        let DefinitionRead::Completed { definition } = &read else {
            return Err(ReportError::Representation(
                crate::representations::RepresentationError::NotCompleted,
            ));
        };
        let native = definition
            .get("definition")
            .and_then(Value::as_object)
            .ok_or(ReportError::InvalidResponse)?;
        let parts = native
            .get("parts")
            .and_then(Value::as_array)
            .ok_or(ReportError::InvalidResponse)?;
        let mut evidence = DefinitionEvidence {
            format: FormatEvidence {
                declared: None,
                layout: None,
            },
            version: None,
            native_version: None,
            schema: None,
            model_reference: None,
            related_model_authorized: false,
            parts: Vec::new(),
            incomplete: Vec::new(),
        };
        let mut paths = BTreeSet::new();
        for part in parts {
            let string = |field| {
                part.get(field)
                    .and_then(Value::as_str)
                    .ok_or(ReportError::InvalidResponse)
            };
            let path = string("path")?;
            if path.is_empty() || !paths.insert(path) {
                return Err(ReportError::AmbiguousSource);
            }
            let payload_type = string("payloadType")?;
            string("payload")?;
            let media_type = optional_string(part.get("mediaType"))?;
            if payload_type == "InlineBase64" {
                STANDARD.decode(string("payload")?).map_err(|_| {
                    ReportError::Representation(
                        crate::representations::RepresentationError::InvalidBase64,
                    )
                })?;
            } else {
                mark(&mut evidence, IncompleteReason::UnsupportedPayloadType);
            }
            evidence.parts.push(NativePartEvidence {
                path: path.into(),
                payload_type: payload_type.into(),
                media_type,
            });
        }
        let legacy = paths.contains("report.json");
        let enhanced = paths.iter().any(|path| path.starts_with("definition/"));
        if legacy && enhanced {
            return Err(ReportError::AmbiguousSource);
        }
        evidence.format.layout = if legacy {
            Some("PBIR-Legacy".into())
        } else if enhanced {
            Some("PBIR".into())
        } else {
            None
        };
        evidence.format.declared = optional_string(native.get("format"))?;
        if evidence
            .format
            .declared
            .as_deref()
            .is_some_and(|format| !matches!(format, "PBIR" | "PBIR-Legacy"))
        {
            mark(&mut evidence, IncompleteReason::UnknownFormat);
        } else if let (Some(declared), Some(layout)) =
            (&evidence.format.declared, &evidence.format.layout)
            && declared != layout
        {
            return Err(ReportError::AmbiguousSource);
        }
        let observed = evidence
            .format
            .declared
            .as_deref()
            .or(evidence.format.layout.as_deref());
        if requested_format.is_some_and(|requested| observed != Some(requested)) {
            return Err(ReportError::UnsupportedFormat);
        }
        if !legacy
            && (!paths.contains("definition/report.json")
                || !paths.contains("definition/version.json")
                || !paths.iter().any(|path| {
                    path.starts_with("definition/pages/") && path.ends_with("/page.json")
                }))
        {
            mark(&mut evidence, IncompleteReason::MissingReportStructure);
        }
        if enhanced
            && let Some(part) = parts
                .iter()
                .find(|part| part["path"] == "definition/version.json")
            && part["payloadType"] == "InlineBase64"
        {
            let bytes = read
                .part_bytes("definition/version.json")
                .map_err(ReportError::Representation)?;
            let version: Value =
                serde_json::from_slice(&bytes).map_err(|_| ReportError::InvalidResponse)?;
            if !version.is_object() {
                return Err(ReportError::InvalidResponse);
            }
            evidence.native_version = optional_string(version.get("version"))?;
        }
        if enhanced && evidence.native_version.as_deref() != Some("2.0.0") {
            mark(&mut evidence, IncompleteReason::UnknownVersion);
        }
        for part in parts {
            let path = part["path"].as_str().expect("validated native part path");
            let required = path == "report.json"
                || path == "definition/report.json"
                || (path.starts_with("definition/pages/") && path.ends_with("/page.json"));
            if required && part["payloadType"] == "InlineBase64" {
                let bytes = read.part_bytes(path).map_err(ReportError::Representation)?;
                if !matches!(
                    serde_json::from_slice::<Value>(&bytes),
                    Ok(Value::Object(_))
                ) {
                    mark(&mut evidence, IncompleteReason::InvalidReportStructure);
                }
            }
        }
        if !paths.contains("definition.pbir") {
            mark(&mut evidence, IncompleteReason::MissingDefinitionProperties);
        } else if let Some(part) = parts.iter().find(|part| part["path"] == "definition.pbir")
            && part["payloadType"] == "InlineBase64"
        {
            let bytes = read
                .part_bytes("definition.pbir")
                .map_err(ReportError::Representation)?;
            let properties: Value =
                serde_json::from_slice(&bytes).map_err(|_| ReportError::InvalidResponse)?;
            if !properties.is_object() {
                return Err(ReportError::InvalidResponse);
            }
            evidence.version = optional_string(properties.get("version"))?;
            evidence.schema = optional_string(properties.get("$schema"))?;
            if let Some(reference) = properties.get("datasetReference") {
                if matches!(evidence.version.as_deref(), Some("1.0" | "4.0"))
                    && validate_reference(reference, evidence.schema.as_deref())?
                {
                    mark(&mut evidence, IncompleteReason::UnknownBindingSchema);
                }
                evidence.model_reference = Some(reference.clone());
            }
        }
        if !matches!(evidence.version.as_deref(), Some("1.0" | "4.0")) {
            mark(&mut evidence, IncompleteReason::UnknownVersion);
        } else if enhanced && evidence.version.as_deref() == Some("1.0") {
            return Err(ReportError::InvalidResponse);
        }
        if evidence.model_reference.is_none() {
            mark(&mut evidence, IncompleteReason::MissingModelReference);
        }
        Ok(Self { read, evidence })
    }

    /// Native envelope with original paths, order, encodings and unknown fields.
    pub fn data(&self) -> &Value {
        match &self.read {
            DefinitionRead::Completed { definition } => definition,
            DefinitionRead::Accepted { .. } => unreachable!("validated completed Report source"),
        }
    }

    /// Source-format/coverage evidence, never inferred model authorization.
    pub fn evidence(&self) -> &DefinitionEvidence {
        &self.evidence
    }

    /// Whether recognized required native structure is present.
    pub fn complete(&self) -> bool {
        self.evidence.incomplete.is_empty()
    }

    /// Exact bytes of one part; media is available separately in evidence.
    ///
    /// # Errors
    /// Retains the shared exact-selection, encoding and ambiguity failures.
    pub fn part_bytes(&self, path: &str) -> Result<Vec<u8>, ReportError> {
        self.read
            .part_bytes(path)
            .map_err(ReportError::Representation)
    }
}

impl fmt::Debug for ReportDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReportDefinition")
            .field("evidence", &self.evidence)
            .finish_non_exhaustive()
    }
}

fn mark(evidence: &mut DefinitionEvidence, reason: IncompleteReason) {
    if !evidence.incomplete.contains(&reason) {
        evidence.incomplete.push(reason);
    }
}

fn optional_string(value: Option<&Value>) -> Result<Option<String>, ReportError> {
    value
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or(ReportError::InvalidResponse)
        })
        .transpose()
}

fn validate_reference(reference: &Value, schema: Option<&str>) -> Result<bool, ReportError> {
    let legacy = match schema {
        Some(
            "https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/1.0.0/schema.json",
        ) => true,
        None
        | Some(
            "https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/2.0.0/schema.json",
        ) => false,
        Some(_) => return Ok(true),
    };
    let reference = reference.as_object().ok_or(ReportError::InvalidResponse)?;
    let path = reference.get("byPath").filter(|value| !value.is_null());
    let connection = reference
        .get("byConnection")
        .filter(|value| !value.is_null());
    match (path, connection) {
        (Some(path), None) if path.is_object() => {
            optional_string(path.get("path"))?.ok_or(ReportError::InvalidResponse)?;
        }
        (None, Some(connection)) if connection.is_object() => {
            if legacy {
                validate_legacy_connection(connection)?;
            } else {
                optional_string(connection.get("connectionString"))?
                    .ok_or(ReportError::InvalidResponse)?;
            }
        }
        _ => return Err(ReportError::InvalidResponse),
    }
    Ok(false)
}

fn validate_legacy_connection(connection: &Value) -> Result<(), ReportError> {
    for field in [
        "connectionString",
        "pbiModelVirtualServerName",
        "pbiModelDatabaseName",
        "name",
        "connectionType",
    ] {
        if !connection
            .get(field)
            .is_some_and(|value| value.is_string() || value.is_null())
        {
            return Err(ReportError::InvalidResponse);
        }
    }
    if !connection
        .get("pbiServiceModelId")
        .is_some_and(|value| value.is_i64() || value.is_u64() || value.is_null())
    {
        return Err(ReportError::InvalidResponse);
    }
    Ok(())
}
