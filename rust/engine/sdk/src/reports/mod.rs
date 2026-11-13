// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed Report reads over the existing item, representation and observer clients.
//!
//! Report source is not PBIX, model data or served documentation. This module
//! neither creates a model nor resolves/authorizes a referenced model. Credentials,
//! cancellation, total deadlines and observation remain caller-owned.

pub mod create;
mod definition;
mod registry;

pub use definition::{
    DefinitionEvidence, FormatEvidence, IncompleteReason, NativePartEvidence, ReportDefinition,
};
pub use registry::register;

use crate::{
    Client,
    items::{ItemCollection, ItemError, ItemExecutor, ItemGetOptions, ItemMetadata},
    metadata::MetadataLimits,
    representations::{
        DefinitionRead, DefinitionTarget, RepresentationError, RepresentationLimits,
    },
    v2::ItemIdentity,
};
use fabric_sdk_operations::OperationExecutor;
use serde_json::Value;
use std::{fmt, future::Future};

/// Exact Report owner coordinates, not a name, URL or proof of permission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportTarget(ItemIdentity);

impl ReportTarget {
    /// Validate canonical workspace/item UUIDs before credentials or HTTP.
    ///
    /// # Errors
    /// Rejects malformed identities; no resource discovery occurs.
    pub fn new(workspace_id: &str, report_id: &str) -> Result<Self, ReportError> {
        ItemIdentity::new(workspace_id, report_id)
            .map(Self)
            .map_err(|_| ReportError::InvalidInput)
    }

    /// Owning workspace, never inferred from a semantic-model reference.
    pub fn workspace_id(&self) -> &str {
        self.0.workspace_id()
    }

    /// Exact report item ID.
    pub fn report_id(&self) -> &str {
        self.0.item_id()
    }
}

/// Validated Report metadata with unknown provider fields preserved.
///
/// Validation establishes identity/type consistency, not live authorization,
/// freshness, a model binding or definition completeness.
#[derive(Clone, PartialEq)]
pub struct ReportMetadata {
    target: ReportTarget,
    metadata: ItemMetadata,
}

impl ReportMetadata {
    /// Admit already acquired metadata from a shared item reader.
    ///
    /// # Errors
    /// Rejects wrong/missing owner type, a mismatched report ID or an explicitly
    /// mismatched workspace. Missing workspace metadata is not synthesized.
    pub fn from_metadata(
        target: ReportTarget,
        metadata: ItemMetadata,
    ) -> Result<Self, ReportError> {
        if metadata.item_type() != Some("Report") {
            return Err(ReportError::WrongOwner);
        }
        if !metadata.id().eq_ignore_ascii_case(target.report_id())
            || metadata
                .as_value()
                .get("workspaceId")
                .is_some_and(|workspace| {
                    !workspace
                        .as_str()
                        .is_some_and(|id| id.eq_ignore_ascii_case(target.workspace_id()))
                })
        {
            return Err(ReportError::InvalidResponse);
        }
        Ok(Self { target, metadata })
    }

    /// Actual validated report coordinates; model coordinates remain separate.
    pub fn target(&self) -> &ReportTarget {
        &self.target
    }

    /// Exact parsed metadata, not enriched with model or definition reads.
    pub fn data(&self) -> &Value {
        self.metadata.as_value()
    }

    /// Consume the original metadata without rebuilding known fields.
    pub fn into_value(self) -> Value {
        self.metadata.into_value()
    }
}

impl fmt::Debug for ReportMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReportMetadata(<metadata>)")
    }
}

/// Immediate Report source or an actual shared Fabric acceptance handle.
#[derive(Debug)]
pub enum ReportDefinitionRead {
    /// Complete transport envelope with separately classified native coverage.
    Completed(Box<ReportDefinition>),
    /// Existing shared acceptance evidence, not completed source or replay intent.
    Accepted(DefinitionRead),
}

/// Report-specific failures retain shared transport/operation evidence.
#[derive(Debug)]
pub enum ReportError {
    /// Invalid canonical target, format selector or caller input.
    InvalidInput,
    /// The authoritative metadata does not describe a Report.
    WrongOwner,
    /// Native report metadata/source is malformed or identity-inconsistent.
    InvalidResponse,
    /// Conflicting native formats or duplicate exact part paths.
    AmbiguousSource,
    /// Caller-requested format is not an advertised Report representation.
    UnsupportedFormat,
    /// A shared metadata request failed; denial is not absence.
    Metadata(ItemError),
    /// Shared native representation failure, including operation evidence.
    Representation(RepresentationError),
    /// Complete unprojected metadata exceeds its caller-selected result bound.
    BudgetExceeded,
}

impl fmt::Display for ReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInput => "invalid Report read input",
            Self::WrongOwner => "Report read requires authoritative Report metadata",
            Self::InvalidResponse => "invalid Report metadata or native definition",
            Self::AmbiguousSource => "ambiguous Report native definition",
            Self::UnsupportedFormat => "unsupported Report native format",
            Self::Metadata(_) => "Report metadata read failed",
            Self::Representation(_) => "Report native source read failed or is incomplete",
            Self::BudgetExceeded => "Report metadata exceeds the result byte budget",
        })
    }
}

impl std::error::Error for ReportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Metadata(error) => Some(error),
            Self::Representation(error) => Some(error),
            _ => None,
        }
    }
}

/// Read only typed Report metadata via the public reports collection (v1).
///
/// Requires report read permission; no definition, related-model query or hidden
/// dependency creation runs. Unknown fields remain original parsed JSON.
///
/// # Errors
/// Retains item errors, owner/identity mismatch, cancellation and byte limits.
pub async fn read_metadata(
    client: &Client,
    executor: &dyn ItemExecutor,
    target: &ReportTarget,
    limits: MetadataLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<ReportMetadata, ReportError> {
    let metadata = client
        .get_item_metadata_from_collection(
            executor,
            target.workspace_id(),
            ItemCollection::Report,
            target.report_id(),
            ItemGetOptions {
                query: Vec::new(),
                max_response_bytes: limits.max_response_bytes,
            },
            cancelled,
        )
        .await
        .map_err(ReportError::Metadata)?;
    let read = ReportMetadata::from_metadata(target.clone(), metadata)?;
    let bytes = serde_json::to_vec(read.data()).map_err(|_| ReportError::InvalidResponse)?;
    if bytes.len() > limits.max_result_bytes.get() {
        return Err(ReportError::BudgetExceeded);
    }
    Ok(read)
}

/// Read exact native parts from a Report whose metadata the caller acquired.
///
/// Uses public POST `reports/{id}/getDefinition`, not an authoring request.
/// The service requires read/write rights and may restrict encrypted labels.
/// Only PBIR/PBIR-Legacy selectors are advertised; observed unknown versions
/// remain evidence, never a conversion, downgrade or PBIX claim.
///
/// # Errors
/// Rejects unsupported selectors before HTTP and preserves native/operation
/// failures. Partial native coverage is returned explicitly, not lost parts.
pub async fn read_definition(
    client: &Client,
    executor: &dyn OperationExecutor,
    metadata: &ReportMetadata,
    native_format: Option<&str>,
    limits: RepresentationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<ReportDefinitionRead, ReportError> {
    validate_format(native_format)?;
    let read = crate::representations::read_definition_from_collection(
        client,
        executor,
        &DefinitionTarget {
            workspace_id: metadata.target.workspace_id().into(),
            item_id: metadata.target.report_id().into(),
            native_format: native_format.map(str::to_owned),
        },
        Some(ItemCollection::Report),
        limits,
        cancelled,
    )
    .await
    .map_err(ReportError::Representation)?;
    match read {
        DefinitionRead::Accepted { .. } => Ok(ReportDefinitionRead::Accepted(read)),
        DefinitionRead::Completed { .. } => ReportDefinition::from_read(read, native_format)
            .map(Box::new)
            .map(ReportDefinitionRead::Completed),
    }
}

pub(super) fn validate_format(format: Option<&str>) -> Result<(), ReportError> {
    if format.is_some_and(|format| !matches!(format, "PBIR" | "PBIR-Legacy")) {
        return Err(ReportError::UnsupportedFormat);
    }
    Ok(())
}
