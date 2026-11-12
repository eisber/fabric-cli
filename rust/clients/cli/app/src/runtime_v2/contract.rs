// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_sdk::runtime::NativeError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, io};

use super::{ReadFacet, TypedPath};

/// Canonical operation identity, shared by common commands and constrained facades.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ReadOperation {
    /// Read one explicitly selected representation/resource.
    #[serde(rename = "fabric.runtime.get")]
    Get,
    /// Read one explicitly selected session collection page.
    #[serde(rename = "fabric.runtime.list")]
    List,
}

impl ReadOperation {
    /// Stable registration identity; no per-artifact operation copies.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Get => "fabric.runtime.get",
            Self::List => "fabric.runtime.list",
        }
    }
}

/// The single normalized input contract, independent of the invocation spelling.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParsedInvocation {
    /// Canonical registered operation.
    #[serde(rename = "operationId")]
    pub operation: ReadOperation,
    /// Explicit owner and facet.
    pub path: TypedPath,
    /// Serialized data-byte budget, before any projection (not a wire-stream cap).
    pub max_bytes: usize,
    /// Whole-record limit for list; absent for get.
    pub limit: Option<u32>,
    /// Explicit destination; `-` means the caller's selected stdout sink.
    pub output: String,
}

impl ParsedInvocation {
    pub(super) fn validate(&self, options: ReadOptions) -> Result<(), ReadError> {
        self.path.validate()?;
        if self.max_bytes == 0
            || self.output.is_empty()
            || self.output.len() > 4096
            || self.output.chars().any(char::is_control)
        {
            return Err(ReadError::new(ReadErrorKind::InvalidInput));
        }
        if self.max_bytes > options.max_data_bytes
            || self.limit.is_some_and(|limit| limit > options.max_records)
        {
            return Err(ReadError::new(ReadErrorKind::BudgetExceeded));
        }
        match (self.operation, &self.path.facet, self.limit) {
            (_, ReadFacet::Metadata, _) => {
                Err(ReadError::new(ReadErrorKind::UnsupportedCapability))
            }
            (ReadOperation::List, ReadFacet::Sessions, Some(limit)) if limit > 0 => Ok(()),
            (
                ReadOperation::Get,
                ReadFacet::NotebookContent
                | ReadFacet::Session { .. }
                | ReadFacet::Statement { .. },
                None,
            ) => Ok(()),
            _ => Err(ReadError::new(ReadErrorKind::UnsupportedCapability)),
        }
    }
}

/// Independent finite host ceilings, not ambient process configuration.
#[derive(Clone, Copy, Debug)]
pub struct ReadOptions {
    /// Maximum bytes accepted from the single native response.
    pub max_response_bytes: usize,
    /// Maximum caller-selectable serialized data budget.
    pub max_data_bytes: usize,
    /// Maximum complete serialized receipt, including provenance.
    pub max_receipt_bytes: usize,
    /// Maximum caller-selectable collection record limit.
    pub max_records: u32,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            max_response_bytes: 4 * 1024 * 1024,
            max_data_bytes: 1024 * 1024,
            max_receipt_bytes: 2 * 1024 * 1024,
            max_records: 1000,
        }
    }
}

impl ReadOptions {
    pub(super) fn validate(self) -> Result<(), ReadError> {
        if self.max_response_bytes == 0
            || self.max_data_bytes == 0
            || self.max_receipt_bytes == 0
            || self.max_records == 0
        {
            return Err(ReadError::new(ReadErrorKind::InvalidInput));
        }
        Ok(())
    }
}

/// Local output is caller-owned; the library never opens paths or creates files.
pub trait OutputSink {
    /// Check destination support/no-clobber before remote dispatch.
    ///
    /// Implementations must also enforce no-clobber atomically when delivering,
    /// and must not create output as part of this check.
    ///
    /// # Errors
    /// Rejects unavailable, existing or unsupported destinations.
    fn check(&mut self, destination: &str) -> io::Result<()>;

    /// Deliver one complete bounded JSON receipt, with a trailing newline.
    ///
    /// # Errors
    /// May fail after partial local delivery. No automatic replay is authorized.
    fn write(&mut self, destination: &str, receipt: &[u8]) -> io::Result<()>;
}

/// Read admission/outcome, independent from remote workload state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadStatus {
    /// HTTP 202; no completed representation is implied.
    Accepted,
    /// One bounded read completed, not proof the observed workload succeeded.
    ReadComplete,
}

/// Only read effects are admitted by this head.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadEffect {
    /// Observe existing content/resources/results.
    Read,
}

/// Persisted source and runtime output are deliberately different representations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Representation {
    /// Full IPYNB JSON-value fidelity, including saved (not live) outputs.
    NotebookSource,
    /// Jupyter session metadata, not public Livy history.
    NotebookSession,
    /// Classic Livy session metadata.
    LivySession,
    /// Complete existing statement JSON, including output, status and byte cursors.
    LivyStatement,
}

/// A separate workload observation; failure does not turn a successful GET into a write failure.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadObservation {
    /// Queued/startup.
    Pending,
    /// Ready session, not successful application execution.
    Ready,
    /// Work is running.
    Running,
    /// Native model has explicit workload-success evidence.
    Succeeded,
    /// Explicit remote failure evidence.
    Failed,
    /// Explicit remote cancellation evidence.
    Cancelled,
    /// Unknown/missing state or unsupported state classification.
    Unknown,
}

/// Coverage of this single observation, never universal history completeness.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadCoverage {
    /// The exact addressed representation/resource.
    Resource,
    /// The returned live Jupyter collection without advertised continuation.
    ReturnedCollection,
    /// One provider page; no invented continuation or collect-all claim.
    ProviderPage,
    /// Accepted only; no completed read coverage.
    Accepted,
}

/// Actual read provenance, not a planned request or deployed qualification claim.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadMeta {
    /// Exact provider representation.
    pub representation: Representation,
    /// Actual HTTP response status.
    pub http_status: u16,
    /// Native source implementation revision.
    pub source_revision: String,
    /// Always source-modeled/deployment-unverified; fake HTTP is not live proof.
    pub maturity: String,
    /// Finite observed scope.
    pub coverage: ReadCoverage,
    /// No resume token is manufactured in H1.
    pub continuation: Option<String>,
    /// Provider total if supplied, not a total-history guarantee.
    pub provider_total: Option<u64>,
    /// Raw provider state without normalization.
    pub workload_state: Option<Value>,
    /// Independent workload classification, absent for source/collection reads.
    pub workload_observation: Option<WorkloadObservation>,
    /// Actual bounded response body size.
    pub response_bytes: usize,
}

/// Resolved read target, retained after output failure for explicit reconciliation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadTarget {
    /// Human selection and typed facet.
    pub path: TypedPath,
    /// Actual authoritative SDK route observed by the executor.
    pub native_url: String,
}

/// Public version-0.1 JSON receipt. Source content/provider fields are untrusted data.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadReceipt {
    /// Literal `0.1`.
    pub schema_version: String,
    /// Shared canonical/facade operation ID.
    pub operation_id: ReadOperation,
    /// Actual owner/representation and resolved route.
    pub target: ReadTarget,
    /// Declared read-only effect.
    pub effect: ReadEffect,
    /// Read completion versus acceptance, independent of workload success.
    pub status: ReadStatus,
    /// Bounded native JSON; an empty accepted acknowledgement has null data.
    pub data: Value,
    /// Provider/provenance/coverage and separate workload evidence.
    pub meta: ReadMeta,
}

/// Sanitized error category; provider bodies and output paths are never diagnostics.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadErrorKind {
    /// Bad path, arguments or typed input.
    InvalidInput,
    /// Unadvertised representation, option or host capability.
    UnsupportedCapability,
    /// Resolver's authoritative type disagrees with the selected owner.
    WrongOwner,
    /// Data/records/wire/receipt ceiling exceeded; no clipped success.
    BudgetExceeded,
    /// Caller stopped local observation.
    Cancelled,
    /// Caller executor's actual deadline expired; no remote cancel.
    Timeout,
    /// Native HTTP status is retained separately.
    Http,
    /// Invalid native representation or inconsistent owner IDs.
    InvalidResponse,
    /// Executor could not provide an authoritative read outcome.
    Execution,
    /// Output preflight failed; no HTTP was dispatched.
    OutputUnavailable,
    /// Output failed after remote read; retained receipt does not prove delivery.
    Delivery,
}

/// A local/remote read error, optionally retaining a completed observation after sink failure.
pub struct ReadError {
    /// Sanitized category.
    pub kind: ReadErrorKind,
    /// Actual HTTP status when observed, never synthesized.
    pub http_status: Option<u16>,
    /// Remote observation retained only when local delivery failed.
    pub receipt: Option<Box<ReadReceipt>>,
}

impl ReadError {
    pub(super) fn new(kind: ReadErrorKind) -> Self {
        Self {
            kind,
            http_status: None,
            receipt: None,
        }
    }
}

impl From<NativeError> for ReadError {
    fn from(error: NativeError) -> Self {
        let mut result = Self::new(match error {
            NativeError::InvalidInput => ReadErrorKind::InvalidInput,
            NativeError::UnsupportedCapability => ReadErrorKind::UnsupportedCapability,
            NativeError::Limit => ReadErrorKind::BudgetExceeded,
            NativeError::Cancelled => ReadErrorKind::Cancelled,
            NativeError::TimedOut => ReadErrorKind::Timeout,
            NativeError::Http(_) => ReadErrorKind::Http,
            NativeError::InvalidResponse => ReadErrorKind::InvalidResponse,
            NativeError::OutcomeUnknown | NativeError::Transport => ReadErrorKind::Execution,
        });
        if let NativeError::Http(status) = error {
            result.http_status = Some(status);
        }
        result
    }
}

impl fmt::Debug for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadError")
            .field("kind", &self.kind)
            .field("http_status", &self.http_status)
            .field("receipt_retained", &self.receipt.is_some())
            .finish()
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runtime read failed: {:?}", self.kind)
    }
}

impl std::error::Error for ReadError {}
