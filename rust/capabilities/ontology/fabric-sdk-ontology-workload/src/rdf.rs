// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Server RDF conversion with explicit format, byte bounds and fidelity gaps.

use crate::{client::*, workload::*};
use serde::{Deserialize, Serialize};

/// Source's maximum decoded export bytes (10 MiB), independent of caller budget.
pub const MAX_EXPORT_BYTES: usize = 10 * 1024 * 1024;
/// Source import file ceiling (30 MiB); import remains disabled due to concurrency semantics.
pub const MAX_IMPORT_BYTES: usize = 30 * 1024 * 1024;

/// Only formats in the source's actual writable whitelist.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RdfFormat {
    /// Turtle (.ttl).
    Turtle,
    /// RDF/XML (.rdf or .owl), not arbitrary OWL syntaxes.
    RdfXml,
}

impl RdfFormat {
    /// Source MIME type, not guessed from a file name.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Turtle => "text/turtle",
            Self::RdfXml => "application/rdf+xml",
        }
    }
    /// Exact format spelling. JSON-LD enum existence is not parser/serializer support.
    pub fn parse(value: &str) -> Result<Self, WorkloadError> {
        match value {
            "Turtle" => Ok(Self::Turtle),
            "RdfXml" => Ok(Self::RdfXml),
            _ => Err(WorkloadError::gap(
                "unsupported RDF format; only Turtle and RdfXml are writable",
            )),
        }
    }
    /// Whitelisted extensions only; `.owl` explicitly means RDF/XML.
    pub fn from_extension(extension: &str) -> Result<Self, WorkloadError> {
        match extension.to_ascii_lowercase().as_str() {
            ".ttl" => Ok(Self::Turtle),
            ".rdf" | ".owl" => Ok(Self::RdfXml),
            _ => Err(WorkloadError::gap("unsupported RDF file extension")),
        }
    }
}

/// Explicit RDF namespace/base IRI, never used for request routing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct BaseIri(String);
impl BaseIri {
    /// Require an absolute HTTP(S) or URN namespace, matching the native controller.
    pub fn new(value: impl Into<String>) -> Result<Self, WorkloadError> {
        let value = value.into();
        let lower = value.to_ascii_lowercase();
        let valid = if lower.starts_with("https://") || lower.starts_with("http://") {
            value.split_once("://").is_some_and(|(_, rest)| {
                let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
                !authority.is_empty() && !authority.ends_with(':')
            })
        } else if lower.starts_with("urn:") {
            !value[4..].is_empty()
        } else {
            false
        };
        if !valid
            || value
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "<>\"{}|\\^`".contains(c))
        {
            return Err(WorkloadError::invalid("absolute RDF namespace IRI"));
        }
        let bytes = value.as_bytes();
        for (i, byte) in bytes.iter().enumerate() {
            if *byte == b'%'
                && (i + 2 >= bytes.len()
                    || !bytes[i + 1].is_ascii_hexdigit()
                    || !bytes[i + 2].is_ascii_hexdigit())
            {
                return Err(WorkloadError::invalid("RDF IRI percent encoding"));
            }
        }
        Ok(Self(value))
    }
}

/// Closed native server export body.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RdfExportRequest {
    /// Exact source format.
    pub format: RdfFormat,
    /// Optional explicit base; absent delegates to source mapping policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_iri: Option<BaseIri>,
}

/// RDF is a schema projection, not a full native definition backup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RdfFidelity {
    /// Export returns bytes only, without exhaustive per-construct loss diagnostics.
    ///
    /// Backing tables/expressions, rules, metrics and other native metadata are
    /// not promised to round-trip. Inheritance/reusable-property mapping and
    /// source auto-projection have independent server feature gates.
    LossyProjectionWithoutCompleteDiagnostics,
}

/// Bounded binary output. Never write provider file-name text to disk implicitly.
#[derive(Clone, Debug)]
pub struct RdfExport {
    /// Selected whitelisted format.
    pub format: RdfFormat,
    /// Exact decoded binary bytes, bounded by both service and caller limits.
    pub bytes: Vec<u8>,
    /// Untrusted provider Content-Disposition, retained as data only.
    pub content_disposition: Option<String>,
    /// Explicit non-backup/loss classification.
    pub fidelity: RdfFidelity,
}

/// Why native RDF import is not enabled by this deterministic slice.
pub const RDF_IMPORT_GAP: &str = "native RDF import checks empty target then createOrReplace in separate calls; no exposed atomic create-only precondition or verified concurrency/effect guard";

impl OntologyClient<'_> {
    /// Generate server RDF export against the private DO workload, not Fabric public URL.
    pub fn rdf_export_request(
        &self,
        request: &RdfExportRequest,
    ) -> Result<WorkloadRequest, WorkloadError> {
        self.request(
            &["rdf", "export"],
            &[],
            WorkloadMethod::Post,
            Some("application/json"),
            to_bytes(request, self.limits.request)?,
            self.limits.response.min(MAX_EXPORT_BYTES),
        )
    }

    /// Export using the server mapper and serializer, preserving errors/acceptance as received.
    pub async fn export_rdf(
        &self,
        export: &RdfExportRequest,
    ) -> Result<Outcome<RdfExport>, WorkloadError> {
        let request = self.rdf_export_request(export)?;
        let response = execute_once(self.executor, &request).await?;
        if response.status == 202 {
            return Ok(Outcome::Accepted { response });
        }
        expect_status(&response, 200, request.method())?;
        let media = response
            .header("Content-Type")?
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if !media.eq_ignore_ascii_case(export.format.content_type()) {
            return Err(protocol(
                &request,
                response,
                "RDF response format/content type mismatch",
            ));
        }
        let value = RdfExport {
            format: export.format,
            bytes: response.body.clone(),
            content_disposition: response.header("Content-Disposition")?.map(str::to_owned),
            fidelity: RdfFidelity::LossyProjectionWithoutCompleteDiagnostics,
        };
        Ok(Outcome::Completed { value, response })
    }

    /// Fail explicitly before execution; do not turn an unsafe replacement into a create.
    pub fn rdf_import_capability(&self) -> Result<(), WorkloadError> {
        Err(WorkloadError::gap(RDF_IMPORT_GAP))
    }
}
