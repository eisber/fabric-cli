// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Credential-free execution preparation and caller-approved native dispatch.
//!
//! The caller owns input, approval, time, cancellation, credentials and output.
//! A failed output publication never causes dispatch to be repeated.

use super::{ExecuteCommand, MAX_INPUT_BYTES, MAX_RESULT_BYTES, MAX_RESULT_ROWS, QueryTarget};
use fabric_sdk_dataflow::{
    ArrowVersion, ExecuteQueryCompletion, ExecuteQueryError, ExecuteQueryRequest,
    ExecuteQueryTarget, execute_query,
};
use fabric_sdk_operations::{
    OperationError, OperationErrorKind, OperationExecutor, OperationResponse, OperationSession,
};
use serde::Deserialize;
use std::{
    future::Future,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    pin::Pin,
};

/// Bounded, strictly typed structured input. Missing fields use native defaults.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QueryInput {
    custom_mashup_document: Option<String>,
    arrow_version: Option<u8>,
}

/// Preparation/approval failures contain no credential or native service work.
#[derive(Debug, thiserror::Error)]
pub enum PreparationError {
    /// Invalid target, input or finite execution budget.
    #[error("Invalid Dataflow execution input or budget.")]
    InvalidInput,
    /// The caller could not provide approval; never interpreted as consent.
    #[error("Explicit Dataflow execution approval is unavailable.")]
    ApprovalUnavailable,
    /// The caller declined the exact prepared operation.
    #[error("Dataflow execution was not approved; no query was submitted.")]
    ApprovalDenied,
}

/// A fully validated request before approval or credential acquisition.
pub struct PreparedExecution {
    command: ExecuteCommand,
    target: ExecuteQueryTarget,
    request: ExecuteQueryRequest,
    approval_input: String,
}

impl PreparedExecution {
    /// Validate exact caller-provided bytes without reading files or credentials.
    ///
    /// `None` means no input; `Some(&[])` is an invalid empty input document.
    ///
    /// # Errors
    /// Rejects invalid targets, unknown/duplicate input fields, unsupported Arrow
    /// versions, empty documents, null field values and out-of-range budgets.
    pub fn new(command: &ExecuteCommand, input: Option<&[u8]>) -> Result<Self, PreparationError> {
        let invalid = || PreparationError::InvalidInput;
        let path = format!(
            "/{}.Workspace/{}.Dataflow/queries/{}",
            command.target.workspace, command.target.dataflow, command.target.query
        );
        if QueryTarget::parse(&path).map_err(|_| invalid())? != command.target
            || command.timeout.is_zero()
            || command.timeout > std::time::Duration::from_secs(86_400)
            || !(1..=MAX_RESULT_ROWS).contains(&command.max_rows)
            || !(1..=MAX_RESULT_BYTES).contains(&command.max_bytes)
            || command.output.as_ref().is_some_and(String::is_empty)
            || (command.format == super::ResultFormat::Parquet && command.output.is_none())
        {
            return Err(invalid());
        }
        let input = match input {
            None => QueryInput {
                custom_mashup_document: None,
                arrow_version: None,
            },
            Some(bytes) => {
                if bytes.is_empty() || bytes.len() > MAX_INPUT_BYTES {
                    return Err(invalid());
                }
                // Deserialize directly, so duplicate fields are not collapsed by
                // an intermediate JSON map before the strict struct sees them.
                let parsed: QueryInput = serde_json::from_slice(bytes).map_err(|_| invalid())?;
                let object: serde_json::Value =
                    serde_json::from_slice(bytes).map_err(|_| invalid())?;
                if object
                    .as_object()
                    .ok_or_else(invalid)?
                    .values()
                    .any(serde_json::Value::is_null)
                {
                    return Err(invalid());
                }
                parsed
            }
        };
        let version = match input.arrow_version {
            None | Some(1) => ArrowVersion::V1,
            Some(2) => ArrowVersion::V2,
            _ => return Err(invalid()),
        };
        let target = ExecuteQueryTarget::new(&command.target.workspace, &command.target.dataflow)
            .map_err(|_| invalid())?;
        let mut request = ExecuteQueryRequest::new(command.target.query.clone(), version)
            .map_err(|_| invalid())?;
        let mut approval = serde_json::json!({
            "arrowVersion": input.arrow_version.unwrap_or(1),
        });
        if let Some(document) = input.custom_mashup_document {
            approval["customMashupDocument"] = serde_json::Value::String(document.clone());
            request = request
                .with_custom_mashup_document(document)
                .map_err(|_| invalid())?;
        }
        Ok(Self {
            command: command.clone(),
            target,
            request,
            approval_input: serde_json::to_string(&approval).map_err(|_| invalid())?,
        })
    }

    /// Exact validated target and result/output budgets being approved.
    pub fn command(&self) -> &ExecuteCommand {
        &self.command
    }

    /// Complete escaped target/input approval description, never an SQL guess.
    pub fn approval_question(&self) -> String {
        format!(
            "Execute Dataflow query in workspace {}, item {}, named query {:?}, input {}? This can consume compute and access external sources; local cancellation does not stop remote execution.",
            self.command.target.workspace,
            self.command.target.dataflow,
            self.command.target.query,
            self.approval_input,
        )
    }

    /// Obtain caller-owned approval before constructing an authorized dispatch.
    ///
    /// # Errors
    /// Denial or unavailable approval never permits credential or service work.
    pub async fn approve(
        self,
        approval: &mut impl ExecutionApproval,
    ) -> Result<ApprovedExecution, PreparationError> {
        match approval.approve(&self).await {
            Ok(true) => Ok(ApprovedExecution(self)),
            Ok(false) => Err(PreparationError::ApprovalDenied),
            Err(_) => Err(PreparationError::ApprovalUnavailable),
        }
    }
}

/// Approval capability injected by an executable or embedding application.
pub trait ExecutionApproval {
    /// Approve this immutable target and exact structured input, or fail closed.
    fn approve<'a>(
        &'a mut self,
        prepared: &'a PreparedExecution,
    ) -> Pin<Box<dyn Future<Output = io::Result<bool>> + Send + 'a>>;
}

/// An immutable request with explicit approval; cannot be constructed unchecked.
pub struct ApprovedExecution(PreparedExecution);

impl ApprovedExecution {
    /// The approved target, input selection and bounded output contract.
    pub fn command(&self) -> &ExecuteCommand {
        &self.0.command
    }

    /// Submit once over the existing shared operation engine.
    ///
    /// The session's caller supplies one total timeout/cancellation future.
    /// Application-level Arrow failures and all encoding errors remain failures,
    /// with the native response and original operation evidence retained.
    ///
    /// # Errors
    /// Returns native service/operation or typed tabular conversion failures.
    pub async fn execute<E: OperationExecutor>(
        &self,
        session: &mut OperationSession<'_, E>,
    ) -> Result<ExecutionResult, ExecutionFailure> {
        let response = execute_query(
            session,
            &self.0.target,
            &self.0.request,
            ExecuteQueryCompletion::WaitForResult,
        )
        .await
        .map_err(ExecutionFailure::Query)?;
        encode_response(self.command(), response, session)
    }
}

/// Complete encoded data plus original request reconciliation evidence.
pub struct ExecutionResult {
    /// Bytes in the requested format; diagnostics must use a separate stream.
    pub bytes: Vec<u8>,
    /// Original completion response, retained for application diagnostics.
    pub response: OperationResponse,
    /// Session evidence, including actual accepted ID and uncertain effects.
    pub evidence: ExecutionEvidence,
}

/// Reconciliation facts without inventing an error classification on success.
#[derive(Clone, Debug)]
pub struct ExecutionEvidence {
    /// Actual native accepted operation ID, when one was returned.
    pub operation_id: Option<String>,
    /// Submission may have reached the service; local failure is not rollback.
    pub mutation_may_have_occurred: bool,
    /// Actual failed HTTP status, including failures without a response body.
    pub http_status: Option<u16>,
    /// Exact structured service failure, when the shared engine retained one.
    pub raw_service_error: Option<Box<serde_json::Value>>,
}

impl From<&OperationError> for ExecutionEvidence {
    fn from(error: &OperationError) -> Self {
        Self {
            operation_id: error.operation_id.clone(),
            mutation_may_have_occurred: error.mutation_may_have_occurred,
            http_status: match &error.kind {
                OperationErrorKind::Http { status, .. } => Some(*status),
                _ => None,
            },
            raw_service_error: error.raw_service_error.clone(),
        }
    }
}

/// Typed native execution or result-format failure; never a successful receipt.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionFailure {
    /// Native service errors preserve their original operation and response.
    #[error("Native Dataflow query failed or its completion is unconfirmed.")]
    Query(ExecuteQueryError),
    /// Native Arrow decoding or requested serialization failed.
    #[error("The complete Dataflow result could not be decoded or serialized.")]
    Result {
        /// Shared typed tabular error, including native metadata failures.
        error: fabric_sdk_tabular::TabularError,
        /// Unmodified native response, not a fabricated job or result.
        response: OperationResponse,
        /// Original operation reconciliation evidence.
        evidence: Box<OperationError>,
    },
}

fn encode_response<E: OperationExecutor>(
    command: &ExecuteCommand,
    response: OperationResponse,
    session: &OperationSession<'_, E>,
) -> Result<ExecutionResult, ExecutionFailure> {
    use fabric_sdk_tabular::{OutputLimits, ResultFormat, decode_arrow_stream, encode};
    let evidence = session.evidence(OperationErrorKind::InvalidResponse);
    let encoded = (|| {
        let limits = OutputLimits::new(command.max_rows, command.max_bytes, 4096)?;
        let result = decode_arrow_stream(&response.body, limits)?;
        // The native REST contract reports in-stream failure evidence in this
        // reserved column, independently of Arrow custom/schema metadata.
        result.check_diagnostic_column("PQ Arrow Metadata")?;
        let format = match command.format {
            super::ResultFormat::Json => ResultFormat::Json,
            super::ResultFormat::Csv => ResultFormat::Csv,
            super::ResultFormat::Parquet => ResultFormat::Parquet,
        };
        encode(&result, format, limits)
    })();
    match encoded {
        Ok(bytes) => Ok(ExecutionResult {
            bytes,
            response,
            evidence: ExecutionEvidence::from(&evidence),
        }),
        Err(error) => Err(ExecutionFailure::Result {
            error,
            response,
            evidence: Box::new(evidence),
        }),
    }
}

/// Read a finite structured-input document, without truncating oversized input.
///
/// # Errors
/// Returns I/O or explicit input-limit errors; an empty document is not default.
pub fn read_input(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    input
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > MAX_INPUT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid input size",
        ));
    }
    Ok(bytes)
}

/// A no-clobber local destination preflighted before submission.
pub struct FileDestination {
    path: PathBuf,
}

impl FileDestination {
    /// Reject existing entries and probe creation/write access in the parent.
    ///
    /// This does not create the destination; publication repeats the race-safe
    /// `create_new` check after the one remote execution completes.
    ///
    /// # Errors
    /// Rejects existing destinations and unavailable/non-directory parents.
    pub fn preflight(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return Err(io::Error::from(io::ErrorKind::AlreadyExists)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if path.file_name().is_none() {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !parent.metadata()?.is_dir() {
            return Err(io::Error::from(io::ErrorKind::NotADirectory));
        }
        Self::probe_parent(parent)?;
        Ok(Self { path })
    }

    fn probe_parent(parent: &Path) -> io::Result<()> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let probe = parent.join(format!(
                ".fab-execute-preflight-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&probe)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            let written = file.write_all(&[0]);
            drop(file);
            let removed = std::fs::remove_file(probe);
            // A failed cleanup is also a failed preflight, not authority to submit.
            return removed.and(written);
        }
        Err(io::Error::from(io::ErrorKind::AlreadyExists))
    }

    /// Publish once without overwriting any racing file.
    ///
    /// A failed create/write/sync is never retried. Partial files are explicitly
    /// retained as incomplete evidence rather than deleted by a racy path lookup.
    ///
    /// # Errors
    /// Returns whether an incomplete file may exist; no result is reported saved.
    pub fn publish(&self, bytes: &[u8]) -> Result<(), PublicationFailure> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
            .map_err(|error| PublicationFailure {
                error,
                partial: false,
            })?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| PublicationFailure {
                error,
                partial: true,
            })
    }
}

/// Output failure never authorizes a second remote query.
#[derive(Debug, thiserror::Error)]
#[error("Result publication failed; partial file may exist: {partial}.")]
pub struct PublicationFailure {
    /// Original local I/O error.
    pub error: io::Error,
    /// The destination was created and may contain incomplete bytes.
    pub partial: bool,
}
