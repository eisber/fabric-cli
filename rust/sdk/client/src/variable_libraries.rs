// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Metadata-first VariableLibrary reads over existing item/representation clients.
//!
//! Source access is explicit and may disclose every default and alternate value.
//! Neither metadata nor source reads resolve consumer values, follow references,
//! change activation, or install authentication, HTTP, polling or output policy.

mod source;

pub use source::{
    SourceDefinition, SourceLimits, SourceRead, SourceShape, ValueSetShape, VariableShape,
};

use crate::{
    Client,
    items::{ItemError, ItemExecutor, ItemGetOptions, ItemMetadata},
    representations::{self, DefinitionTarget, RepresentationError, RepresentationLimits},
};
use fabric_sdk_operations::OperationExecutor;
use std::{
    fmt,
    future::{Future, poll_fn},
    io::Write,
    pin::{Pin, pin},
    task::Poll,
};

/// One already-resolved VariableLibrary, not a name, stage, path or URL.
#[derive(Clone, Eq, PartialEq)]
pub struct Target {
    workspace: String,
    item: String,
}

impl Target {
    /// Validates native UUIDs and the exact case-sensitive artifact owner.
    ///
    /// Selection does not resolve names or establish resource permissions.
    ///
    /// # Errors
    /// Rejects invalid IDs or an owner other than `VariableLibrary` before I/O.
    pub fn new(workspace: &str, item: &str, owner_type: &str) -> Result<Self, Error> {
        if !uuid(workspace) || !uuid(item) || owner_type != "VariableLibrary" {
            return Err(Error::InvalidTarget);
        }
        Ok(Self {
            workspace: workspace.into(),
            item: item.into(),
        })
    }

    /// Returns the explicitly selected workspace ID, without normalization.
    pub fn workspace_id(&self) -> &str {
        &self.workspace
    }

    /// Returns the explicitly selected item ID, without normalization.
    pub fn item_id(&self) -> &str {
        &self.item
    }
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VariableLibraryTarget(<resolved>)")
    }
}

/// Required explicit acknowledgement for source and value-set shape reads.
///
/// Even a shape-only request retrieves the entire native definition, including
/// non-active values. This is not a permission grant or a secret-vault contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceDisclosure {
    /// The caller explicitly requested potentially sensitive native source.
    ExplicitlyRequested,
}

/// Read scope facts for independent caller heads; no mutation scope is exposed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReadScope {
    /// Default read: one item metadata GET, with no definition enrichment.
    #[default]
    Metadata,
    /// Native definition or a value-set shape derived from its complete source.
    NativeSource,
}

impl ReadScope {
    /// Whether this semantic read requires item read-and-write permissions.
    ///
    /// Metadata needs item Read; source also needs sufficient encrypted-label
    /// usage rights where applicable. These are service prerequisites, not proof.
    pub const fn requires_item_write(self) -> bool {
        matches!(self, Self::NativeSource)
    }
}

/// Completed item metadata, kept separate from native source and resolved values.
pub struct Metadata {
    data: ItemMetadata,
}

impl Metadata {
    /// Borrows explicitly requested metadata, preserving unknown fields and nulls.
    pub fn data(&self) -> &ItemMetadata {
        &self.data
    }

    /// Returns the observed active-set name without default-name inference.
    ///
    /// This is workspace-local selection, not the contents used by a consumer.
    pub fn active_value_set_name(&self) -> Option<&str> {
        self.data.as_value()["properties"]["activeValueSetName"].as_str()
    }
}

impl fmt::Debug for Metadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VariableLibraryMetadata(<redacted>)")
    }
}

/// Sanitized failures; underlying service evidence is available only as data.
pub enum Error {
    /// Invalid native owner or ID, rejected before credential acquisition.
    InvalidTarget,
    /// A metadata response has the wrong identity/type or malformed known fields.
    InvalidMetadata,
    /// Native source is missing, ambiguous or structurally malformed.
    InvalidSource,
    /// Native format, part path or payload type is outside this supported binding.
    UnsupportedSource,
    /// An explicit caller byte or part bound was exceeded.
    BudgetExceeded,
    /// Cancellation won before completed metadata/source could be delivered.
    Cancelled {
        /// Preserve a source-read acceptance observed before local cancellation.
        operation_id: Option<String>,
    },
    /// The existing item reader failed, retaining its original evidence.
    Metadata(ItemError),
    /// The existing representation reader failed, retaining original evidence.
    Source(RepresentationError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidTarget => "invalid VariableLibrary target",
            Self::InvalidMetadata => "invalid VariableLibrary metadata response",
            Self::InvalidSource => "invalid VariableLibrary native source",
            Self::UnsupportedSource => "unsupported VariableLibrary native source representation",
            Self::BudgetExceeded => "VariableLibrary read exceeded its explicit budget",
            Self::Cancelled { .. } => "VariableLibrary read was cancelled",
            Self::Metadata(_) => "VariableLibrary metadata read failed",
            Self::Source(_) => "VariableLibrary native source read failed or is incomplete",
        })
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for Error {}

/// Reads only metadata through the existing generic item GET.
///
/// The generic API is shared with other item reads; its response must match the
/// exact selected workspace/item and `VariableLibrary` type. No type-specific
/// enrichment, source, variable values, reference lookups or mutation is sent.
/// The caller owns endpoint/credential/executor binding, cancellation and output.
///
/// # Errors
/// Preserves item-reader failures; rejects mismatched metadata and result bounds.
pub async fn read_metadata(
    client: &Client,
    executor: &dyn ItemExecutor,
    target: &Target,
    limits: RepresentationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<Metadata, Error> {
    let mut cancelled = pin!(cancelled);
    let data = client
        .get_item_metadata(
            executor,
            &target.workspace,
            &target.item,
            ItemGetOptions {
                query: Vec::new(),
                max_response_bytes: limits.max_response_bytes.min(limits.max_result_bytes),
            },
            cancelled.as_mut(),
        )
        .await
        .map_err(Error::Metadata)?;
    let value = data.as_value();
    if !data.id().eq_ignore_ascii_case(&target.item)
        || value["workspaceId"]
            .as_str()
            .is_none_or(|id| !id.eq_ignore_ascii_case(&target.workspace))
        || data.item_type() != Some("VariableLibrary")
        || value.get("properties").is_some_and(|properties| {
            !properties.is_object()
                || properties
                    .get("activeValueSetName")
                    .is_some_and(|name| !name.is_string())
        })
    {
        return Err(Error::InvalidMetadata);
    }
    let mut budget = JsonBudget(limits.max_result_bytes.get());
    serde_json::to_writer(&mut budget, value).map_err(|_| Error::BudgetExceeded)?;
    check_cancelled(cancelled.as_mut(), None).await?;
    Ok(Metadata { data })
}

/// Reads native `VariableLibraryV1` source only after explicit disclosure intent.
///
/// Delegates the generic item `getDefinition` POST to the existing representation
/// reader. Read-and-write permission is required even for value-set shapes.
/// HTTP 202 remains acceptance; no polling, retry, metadata fallback, activation,
/// stage inference, extraction or source delivery occurs here.
///
/// # Errors
/// Preserves reader failures and rejects unsupported/malformed or over-budget
/// multipart source. Invalid targets cannot be constructed through this API.
pub async fn read_source(
    client: &Client,
    executor: &dyn OperationExecutor,
    target: &Target,
    _disclosure: SourceDisclosure,
    limits: SourceLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<SourceRead, Error> {
    let mut cancelled = pin!(cancelled);
    let read = representations::read_definition(
        client,
        executor,
        &DefinitionTarget {
            workspace_id: target.workspace.clone(),
            item_id: target.item.clone(),
            native_format: Some("VariableLibraryV1".into()),
        },
        limits.representation,
        cancelled.as_mut(),
    )
    .await
    .map_err(Error::Source)?;
    let read = SourceRead::decode(read, limits)?;
    let accepted = match &read {
        SourceRead::Accepted { operation_id } => Some(operation_id.as_str()),
        SourceRead::Completed(_) => None,
    };
    check_cancelled(cancelled.as_mut(), accepted).await?;
    Ok(read)
}

async fn check_cancelled(
    mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    operation_id: Option<&str>,
) -> Result<(), Error> {
    poll_fn(|cx| {
        Poll::Ready(if cancelled.as_mut().poll(cx).is_ready() {
            Err(Error::Cancelled {
                operation_id: operation_id.map(str::to_owned),
            })
        } else {
            Ok(())
        })
    })
    .await
}

fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

struct JsonBudget(usize);

impl Write for JsonBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("VariableLibrary result budget exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
