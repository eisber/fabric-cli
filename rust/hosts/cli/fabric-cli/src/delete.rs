// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded v2 Notebook deletion with caller-owned approval, operations and output.
//!
//! The initial grammar is exactly
//! `/<workspace-guid>.Workspace/<item-guid>.Notebook`, with hexadecimal GUIDs in
//! 8-4-4-4-12 form. No context lookup, name resolution, recursive traversal,
//! credential discovery, prompting or runtime is installed here. The source
//! `rm`/`del` command is not changed.
//!
//! The current native capability requires explicit `--permanent` and Workspace
//! Admin. Default recoverable deletion is unsupported until an authoritative
//! recovery contract is available; permanence is not a physical-erasure claim.
//! A caller's `.Notebook` suffix is not proof of actual item type. Defensive
//! acceptance handling does not establish that the endpoint advertises LROs.
//! The SDK uses the type-specific `/notebooks/{id}` delete route, not a generic
//! item deletion authorized solely by the caller's suffix.
//! Resource state, observed changes and observed effects remain null without
//! authoritative observations. Declared effects and actual operation evidence
//! are separate. Private provider bodies remain on typed errors, not CLI JSON.
//!
//! Hosts first call [`preflight`], then obtain explicit approval for
//! [`PreparedDelete::assessment`], reserve output with [`prepare_output`], and
//! call [`dispatch`] once. [`write_dispatch`] retains the original dispatch on
//! projection/serialization/writer failure. The caller enforces
//! [`PreparedDelete::timeout`] across the entire command, including approval.

use crate::parser::DeleteCommand;
use fabric_sdk::items::delete::{
    DeleteApproval, DeleteAssessment, DeleteFailure, DeleteOptions, DeleteReceipt, DeleteTarget,
    OperationStatus, assess, execute,
};
use fabric_sdk_workspaces::mutations::{ResourceError, ResourceOperations};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fmt,
    fs::OpenOptions,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

/// Safe command diagnostic, retaining the underlying SDK or I/O error as data.
#[derive(Debug)]
pub struct DeleteCommandError {
    code: &'static str,
    message: &'static str,
    invalid: bool,
    /// Boxed original assessment failure, including typed metadata when available.
    pub sdk: Option<Box<DeleteFailure>>,
    /// Original local output failure; not implicitly displayed.
    pub io: Option<io::Error>,
}

impl DeleteCommandError {
    /// Stable error code, without raw arguments or local/provider diagnostics.
    pub fn code(&self) -> &str {
        self.sdk.as_deref().map_or(self.code, DeleteFailure::code)
    }

    /// Safe public diagnostic.
    pub fn message(&self) -> &str {
        self.sdk
            .as_deref()
            .map_or(self.message, DeleteFailure::message)
    }

    /// Existing executable convention: invalid input 2, execution/output failure 1.
    pub fn exit_code(&self) -> u8 {
        if self.invalid { 2 } else { 1 }
    }

    /// Versioned failure snapshot for a host to write on stderr.
    pub fn envelope(&self, command: Option<&DeleteCommand>) -> DeleteEnvelope {
        let mut meta = base_metadata(command.map(|command| command.path.as_str()));
        if let Some(error) = &self.sdk {
            bind_failure(&mut meta, error);
        }
        DeleteEnvelope {
            schema_version: "0.1",
            status: "failed".into(),
            data: Value::Null,
            meta,
            error: Some(DeleteDiagnostic {
                code: self.code().into(),
                message: self.message().into(),
                retryable: false,
                hints: Vec::new(),
            }),
        }
    }
}

impl fmt::Display for DeleteCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for DeleteCommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Some(error) = &self.sdk {
            Some(error.as_ref())
        } else {
            self.io
                .as_ref()
                .map(|error| error as &dyn std::error::Error)
        }
    }
}

fn invalid(message: &'static str) -> DeleteCommandError {
    DeleteCommandError {
        code: "InvalidInput",
        message,
        invalid: true,
        sdk: None,
        io: None,
    }
}

fn unsupported(message: &'static str) -> DeleteCommandError {
    DeleteCommandError {
        code: "UnsupportedCapability",
        message,
        invalid: false,
        sdk: None,
        io: None,
    }
}

fn sdk_error(error: DeleteFailure) -> DeleteCommandError {
    DeleteCommandError {
        code: "DeleteFailed",
        message: "Delete assessment failed",
        invalid: error.code() == "InvalidInput",
        sdk: Some(Box::new(error)),
        io: None,
    }
}

fn output_error(error: io::Error) -> DeleteCommandError {
    DeleteCommandError {
        code: "OutputFailed",
        message: "Delete output failed; inspect retained dispatch evidence before taking further action",
        invalid: false,
        sdk: None,
        io: Some(error),
    }
}

/// Immutable validated target/options and SDK assessment, prior to any I/O.
#[derive(Debug)]
pub struct PreparedDelete {
    command: DeleteCommand,
    assessment: DeleteAssessment,
}

impl PreparedDelete {
    /// Original parsed arguments; changing another command cannot alter this plan.
    pub fn command(&self) -> &DeleteCommand {
        &self.command
    }

    /// Exact immutable assessment to show to the caller's approval adapter.
    pub fn assessment(&self) -> &DeleteAssessment {
        &self.assessment
    }

    /// Whole-command deadline budget; libraries create no clock or timer.
    pub fn timeout(&self) -> Duration {
        self.command.timeout
    }

    /// Bind caller-selected identity and endpoint metadata before approval.
    ///
    /// The SDK includes this selection in the immutable assessment and its
    /// authorization comparison. The host must execute with the same selection.
    /// This does not verify token claims or grant service permissions, acquire
    /// tokens, or perform any other I/O.
    ///
    /// # Errors
    /// Preserves the SDK's finite binding failures.
    pub fn with_selection(
        mut self,
        selection: &fabric_sdk::CredentialSelection,
        endpoint: &fabric_sdk::EndpointBinding,
    ) -> Result<Self, DeleteCommandError> {
        self.assessment = self
            .assessment
            .with_selection(selection, endpoint)
            .map_err(sdk_error)?;
        Ok(self)
    }

    /// Bind a request context's safe selection without touching credentials.
    ///
    /// Call before approval and execute with this same immutable context.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::with_selection`].
    pub fn with_context(
        self,
        context: &fabric_sdk::RequestContext,
    ) -> Result<Self, DeleteCommandError> {
        self.with_selection(context.selection(), context.endpoint())
    }

    /// Bind the assessment to a host timeout/cancellation snapshot before dispatch.
    ///
    /// # Errors
    /// Returns an explicit serialization failure instead of silently omitting metadata.
    pub fn metadata(&self) -> Result<Value, DeleteCommandError> {
        let mut meta = base_metadata(Some(&self.command.path));
        meta["assessment"] = serialized(&self.assessment)?;
        meta["operation"] = serialized(self.assessment.metadata())?;
        meta["effects"]["declared"] = serialized(&self.assessment.descriptor().effects)?;
        meta["context"] = serialized(&self.assessment.metadata().context)?;
        Ok(meta)
    }
}

/// Validate the complete bounded invocation before credentials or local writes.
///
/// `selected_profile` is the caller's actual existing profile selection, not a
/// profile to load. A supplied profile must match it exactly; no profile machinery
/// is invented. Query compilation/evaluation uses the existing query provider over
/// the pre-dispatch data shape, failing closed before mutation; data-dependent
/// evaluation errors on the actual receipt remain output errors with the receipt.
///
/// # Errors
/// Rejects unsupported capabilities, formats, profiles and unsafe paths, malformed
/// queries, timeouts outside `(0, 24h]`, and SDK assessment failures.
pub fn preflight(
    command: &DeleteCommand,
    selected_profile: Option<&str>,
) -> Result<PreparedDelete, DeleteCommandError> {
    if command.format != "json"
        || command
            .output_format
            .as_deref()
            .is_some_and(|format| format != "json")
    {
        return Err(unsupported(
            "V2 delete supports only JSON; legacy text output is incompatible",
        ));
    }
    if command
        .profile
        .as_deref()
        .is_some_and(|profile| profile.is_empty() || selected_profile != Some(profile))
    {
        return Err(unsupported(
            "The requested profile is not the caller's selected profile",
        ));
    }
    if command.timeout.is_zero() || command.timeout > Duration::from_secs(86_400) {
        return Err(invalid("Delete timeout must be positive and at most 24h"));
    }
    let (workspace, item) = target_ids(&command.path)?;
    validate_output(&command.output)?;
    if let Some(expression) = &command.project {
        if expression.trim().is_empty() {
            return Err(invalid(
                "Delete projection must be a nonempty JMESPath expression",
            ));
        }
        fabric_cli_query::search(&delete_data(Some(&command.path)), expression)
            .map_err(|_| invalid("Invalid delete data projection (JMESPath)"))?;
    }
    let target = DeleteTarget::notebook(workspace, item).map_err(sdk_error)?;
    let assessment = assess(
        target,
        DeleteOptions {
            permanent: command.permanent,
            recursive: command.recursive,
            if_match: command.if_match.clone(),
            wait: command.wait,
        },
    )
    .map_err(sdk_error)?;
    Ok(PreparedDelete {
        command: command.clone(),
        assessment,
    })
}

/// Render a sanitized host rejection before any SDK operation was dispatched.
///
/// The host supplies finite static diagnostics, not raw credential, OS or
/// provider text. The complete assessment and any bound host context are
/// retained. After dispatch, use the actual [`DeleteDispatch`] or
/// [`DeleteOutputFailure`] instead; this helper must not erase uncertainty.
///
/// # Errors
/// Returns an explicit assessment-metadata serialization failure.
pub fn host_failure(
    prepared: &PreparedDelete,
    code: &str,
    message: &str,
) -> Result<DeleteEnvelope, DeleteCommandError> {
    let mut meta = prepared.metadata()?;
    meta["hostFailure"] = json!({
        "dispatch": "not_dispatched",
        "mutationMayHaveOccurred": false
    });
    Ok(DeleteEnvelope {
        schema_version: "0.1",
        status: "failed".into(),
        data: Value::Null,
        meta,
        error: Some(DeleteDiagnostic {
            code: code.into(),
            message: message.into(),
            retryable: false,
            hints: Vec::new(),
        }),
    })
}

fn target_ids(path: &str) -> Result<(&str, &str), DeleteCommandError> {
    let invalid = || {
        invalid(
            "Delete requires one absolute /<workspace-guid>.Workspace/<item-guid>.Notebook target",
        )
    };
    let path = path.strip_prefix('/').ok_or_else(invalid)?;
    let (workspace, item) = path.split_once('/').ok_or_else(invalid)?;
    let workspace = workspace.strip_suffix(".Workspace").ok_or_else(invalid)?;
    let item = item.strip_suffix(".Notebook").ok_or_else(invalid)?;
    if !guid(workspace) || !guid(item) {
        return Err(invalid());
    }
    Ok((workspace, item))
}

fn guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn validate_output(value: &str) -> Result<(), DeleteCommandError> {
    if value == "-" {
        return Ok(());
    }
    if value.is_empty()
        || value.contains("://")
        || value.starts_with("//")
        || value.starts_with("\\\\")
        || value.bytes().enumerate().any(|(index, byte)| {
            byte == b':' && !(index == 1 && value.as_bytes()[0].is_ascii_alphabetic())
        })
        || value.as_bytes().get(1) == Some(&b':')
            && !value
                .as_bytes()
                .get(2)
                .is_some_and(|byte| matches!(*byte, b'/' | b'\\'))
        || value
            .chars()
            .any(|character| character.is_control() || matches!(character, '*' | '?' | '[' | ']'))
        || value.ends_with(['/', '\\'])
        || Path::new(value).file_name().is_none()
        || Path::new(value)
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        || value.split(['/', '\\']).any(|component| {
            let stem = component
                .split('.')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            component == ".."
                || component.ends_with(['.', ' '])
                || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || stem
                    .strip_prefix("COM")
                    .or_else(|| stem.strip_prefix("LPT"))
                    .is_some_and(|number| {
                        matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                    })
        })
    {
        return Err(invalid(
            "Delete output requires '-' or one exact local file path without traversal or wildcards",
        ));
    }
    Ok(())
}

/// Injected atomic no-clobber file reservation, before any provider mutation.
///
/// Implementations must create the exact file with create-new semantics, retain
/// that open handle, never truncate an existing file, and never remove any path.
/// A metadata/exists check followed by a later open does not satisfy this contract.
pub trait DeleteOutputFiles {
    /// Reserve exactly the selected output and return its already-open writer.
    ///
    /// # Errors
    /// Existing destinations, inaccessible parents and unsupported local paths fail.
    fn reserve_new(&mut self, path: &Path) -> io::Result<Box<dyn Write + Send>>;
}

/// Standard create-new local adapter using a caller-captured absolute base.
///
/// It performs no cwd/home/environment discovery, parent creation, truncation,
/// cleanup or arbitrary deletion. Like the existing selected-file helper this
/// is not a hostile-process namespace sandbox.
#[derive(Debug)]
pub struct NativeDeleteFiles {
    base: PathBuf,
}

impl NativeDeleteFiles {
    /// Bind a base already captured by the host; construction performs no I/O.
    ///
    /// # Errors
    /// Requires an absolute base rather than discovering the process directory.
    pub fn new(base: PathBuf) -> Result<Self, DeleteCommandError> {
        if !base.is_absolute() {
            return Err(invalid(
                "Delete output requires a caller-captured absolute local base",
            ));
        }
        Ok(Self { base })
    }
}

impl DeleteOutputFiles for NativeDeleteFiles {
    fn reserve_new(&mut self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        let spelling = path
            .to_str()
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        validate_output(spelling).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        if spelling == "-" {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let selected = if path.is_absolute() {
            path.to_owned()
        } else {
            self.base.join(path)
        };
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(selected)?;
        Ok(Box::new(file))
    }
}

enum OutputSink {
    Stdout,
    File(Box<dyn Write + Send>),
}

/// Open output authority bound to the exact immutable command.
///
/// Dropping this value closes its handle but intentionally leaves any newly
/// created empty/partial file in place. It never unlinks a path.
pub struct ReservedDeleteOutput {
    command: DeleteCommand,
    invocation_id: u64,
    sink: OutputSink,
}

impl fmt::Debug for ReservedDeleteOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReservedDeleteOutput")
            .finish_non_exhaustive()
    }
}

/// Reserve local output atomically, or select stdout, before calling [`dispatch`].
///
/// # Errors
/// Existing output is preserved; reservation failure prevents provider dispatch.
pub fn prepare_output(
    prepared: &PreparedDelete,
    files: &mut dyn DeleteOutputFiles,
) -> Result<ReservedDeleteOutput, DeleteCommandError> {
    let sink = if prepared.command.output == "-" {
        OutputSink::Stdout
    } else {
        OutputSink::File(
            files
                .reserve_new(Path::new(&prepared.command.output))
                .map_err(output_error)?,
        )
    };
    Ok(ReservedDeleteOutput {
        command: prepared.command.clone(),
        invocation_id: prepared.assessment.metadata().invocation_id,
        sink,
    })
}

/// Machine-readable diagnostic; private provider content stays on typed dispatches.
#[derive(Clone, Debug, Serialize)]
pub struct DeleteDiagnostic {
    /// Stable category.
    pub code: String,
    /// Safe public diagnostic.
    pub message: String,
    /// False: diagnostics never authorize automatic mutation replay.
    pub retryable: bool,
    /// Informational guidance only, never automatic remediation.
    pub hints: Vec<String>,
}

/// One versioned delete snapshot; projection can replace only [`Self::data`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteEnvelope {
    /// Explicit schema revision; not the source v1 output shape.
    pub schema_version: &'static str,
    /// One of succeeded, accepted, partial, failed or unknown.
    pub status: String,
    /// Action/state/resource/changes or its projection; null for known failures.
    pub data: Value,
    /// Target, coverage, declared effects, assessment and safe operation evidence.
    pub meta: Value,
    /// Failure diagnostic; null on success/acceptance.
    pub error: Option<DeleteDiagnostic>,
}

/// Actual SDK result plus its complete immutable snapshot and output authority.
#[derive(Debug)]
pub struct DeleteDispatch {
    /// Original receipt or failure, including provider content omitted from JSON.
    pub result: Result<DeleteReceipt, DeleteFailure>,
    envelope: DeleteEnvelope,
    project: Option<String>,
    output: Option<ReservedDeleteOutput>,
}

impl DeleteDispatch {
    /// Unprojected snapshot, safe to retain for reconciliation after output failure.
    pub fn envelope(&self) -> &DeleteEnvelope {
        &self.envelope
    }

    /// Existing success/failure/invalid convention; acceptance is not completion.
    pub fn exit_code(&self) -> u8 {
        match &self.result {
            Ok(_) if self.envelope.error.is_none() => 0,
            Ok(_) => 1,
            Err(error) if error.code() == "InvalidInput" => 2,
            Err(_) => 1,
        }
    }
}

/// Execute exactly once through the caller's existing resource operation policy.
///
/// Approval is injected and must match the immutable assessment. No approval is
/// synthesized for a noninteractive caller. A reservation for a different command
/// is rejected before the SDK or credentials are touched.
///
/// # Errors
/// Reservation mismatch or serialization failure is a command error; the SDK
/// result itself always remains on the returned dispatch, including uncertainty.
pub async fn dispatch(
    operations: &mut dyn ResourceOperations,
    prepared: &PreparedDelete,
    approval: Option<&DeleteApproval>,
    output: ReservedDeleteOutput,
) -> Result<DeleteDispatch, DeleteCommandError> {
    if output.command != prepared.command
        || output.invocation_id != prepared.assessment.metadata().invocation_id
    {
        return Err(invalid(
            "Delete output reservation does not match the assessed command",
        ));
    }
    let meta = prepared.metadata()?;
    let result = execute(operations, &prepared.assessment, approval).await;
    Ok(snapshot(prepared, result, meta, output))
}

fn snapshot(
    prepared: &PreparedDelete,
    result: Result<DeleteReceipt, DeleteFailure>,
    mut meta: Value,
    output: ReservedDeleteOutput,
) -> DeleteDispatch {
    let mut envelope = DeleteEnvelope {
        schema_version: "0.1",
        status: "failed".into(),
        data: delete_data(Some(&prepared.command.path)),
        meta: Value::Null,
        error: None,
    };
    match &result {
        Ok(receipt) => bind_receipt(&mut envelope, &mut meta, receipt),
        Err(error) => {
            bind_failure(&mut meta, error);
            envelope.status = if error.evidence().status == OperationStatus::Unknown {
                "unknown"
            } else {
                "failed"
            }
            .into();
            envelope.data = Value::Null;
            envelope.error = Some(DeleteDiagnostic {
                code: error.code().into(),
                message: error.message().into(),
                retryable: false,
                hints: Vec::new(),
            });
        }
    }
    envelope.meta = meta;
    DeleteDispatch {
        result,
        envelope,
        project: prepared.command.project.clone(),
        output: Some(output),
    }
}

fn base_metadata(path: Option<&str>) -> Value {
    let path = path.filter(|path| target_ids(path).is_ok());
    let target = path.and_then(|path| target_ids(path).ok());
    json!({
        "target": {
            "path": path,
            "id": target.map(|(_, item)| item.to_ascii_lowercase()),
            "type": target.map(|_| "Notebook"),
            "scope": target.map(|(workspace, _)| json!({
                "workspaceId": workspace.to_ascii_lowercase()
            }))
        },
        "context": null,
        "coverage": {"complete": null},
        "effects": {"declared": null, "observed": null},
        "operation": null,
        "assessment": null,
        "receipt": null,
        "evidence": null,
        "resourceError": null
    })
}

fn delete_data(path: Option<&str>) -> Value {
    let resource = path
        .and_then(|path| target_ids(path).ok())
        .map(|(workspace, item)| json!({"workspaceId": workspace, "id": item, "type": "Notebook"}));
    json!({"action": "delete", "state": null, "resource": resource, "changes": null})
}

fn serialized(value: &impl Serialize) -> Result<Value, DeleteCommandError> {
    serde_json::to_value(value).map_err(|_| DeleteCommandError {
        code: "SerializationFailed",
        message: "Delete metadata could not be serialized",
        invalid: false,
        sdk: None,
        io: None,
    })
}

fn bind_receipt(envelope: &mut DeleteEnvelope, meta: &mut Value, receipt: &DeleteReceipt) {
    match serialized(receipt) {
        Ok(value) => {
            envelope.status = match receipt.status() {
                OperationStatus::Accepted => "accepted",
                OperationStatus::Succeeded => "succeeded",
                OperationStatus::Failed => "failed",
                OperationStatus::Unknown => "unknown",
            }
            .into();
            meta["operation"] = json!(receipt.metadata());
            meta["evidence"] = json!(receipt.evidence);
            meta["receipt"] = value;
        }
        Err(error) => {
            envelope.data = Value::Null;
            envelope.error = Some(DeleteDiagnostic {
                code: error.code().into(),
                message: error.message().into(),
                retryable: false,
                hints: Vec::new(),
            });
        }
    }
}

fn bind_failure(meta: &mut Value, error: &DeleteFailure) {
    meta["operation"] = json!(error.metadata());
    if let Some(metadata) = error.metadata() {
        meta["effects"]["declared"] = json!(metadata.descriptor.effects);
    }
    meta["evidence"] = json!(error.evidence());
    if let Some(error) = error.resource_error() {
        meta["resourceError"] = resource_error(error);
    }
}

fn resource_error(error: &ResourceError) -> Value {
    json!({
        "status": error.status,
        "operationId": error.operation_id,
        "mutationMayHaveOccurred": error.mutation_may_have_occurred
    })
}

/// Projection or output failure that still owns the original dispatch.
#[derive(Debug)]
pub struct DeleteOutputFailure {
    /// Complete original receipt/failure and its metadata; never repeat deletion.
    pub dispatch: Box<DeleteDispatch>,
    /// Safe local diagnostic and original I/O error, where applicable.
    pub error: DeleteCommandError,
}

impl DeleteOutputFailure {
    /// Error snapshot for stderr, preserving the original snapshot under metadata.
    pub fn envelope(&self) -> DeleteEnvelope {
        let mut envelope = self.dispatch.envelope.clone();
        envelope.meta["dispatch"] = json!(self.dispatch.envelope);
        envelope.status = "failed".into();
        envelope.data = Value::Null;
        envelope.error = Some(DeleteDiagnostic {
            code: self.error.code().into(),
            message: self.error.message().into(),
            retryable: false,
            hints: Vec::new(),
        });
        envelope
    }

    /// Output failure is exit 1, regardless of provider acceptance/completion.
    pub fn exit_code(&self) -> u8 {
        1
    }
}

impl fmt::Display for DeleteOutputFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for DeleteOutputFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Render one JSON snapshot; only `data` is projected, including scalar results.
///
/// # Errors
/// Data-dependent query and serialization errors are explicit, not empty output.
pub fn render(dispatch: &DeleteDispatch) -> Result<Vec<u8>, DeleteCommandError> {
    if dispatch.result.is_ok() && dispatch.envelope.error.is_some() {
        return Err(DeleteCommandError {
            code: "SerializationFailed",
            message: "Delete snapshot could not be serialized; original dispatch evidence is retained",
            invalid: false,
            sdk: None,
            io: None,
        });
    }
    let mut envelope = dispatch.envelope.clone();
    if dispatch.result.is_ok()
        && let Some(expression) = &dispatch.project
    {
        envelope.data = fabric_cli_query::search(&envelope.data, expression)
            .map_err(|_| DeleteCommandError {
                code: "ProjectionFailed",
                message: "Delete data projection failed; the original dispatch evidence is retained",
                invalid: false,
                sdk: None,
                io: None,
            })?;
    }
    let mut bytes = serde_json::to_vec(&envelope).map_err(|_| DeleteCommandError {
        code: "SerializationFailed",
        message: "Delete snapshot could not be serialized; original dispatch evidence is retained",
        invalid: false,
        sdk: None,
        io: None,
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Write once to the pre-reserved file or injected stdout, with no service replay.
///
/// The caller writes SDK failure snapshots to stderr instead of stdout; this
/// function is for successful/accepted dispatch output. On failure it returns the
/// original dispatch; call [`DeleteOutputFailure::envelope`] for a stderr snapshot.
/// A consumed writer cannot be reopened or retried through this function.
///
/// # Errors
/// Projection, serialization, write and flush failures retain the dispatch.
pub fn write_dispatch(
    mut dispatch: DeleteDispatch,
    stdout: &mut dyn Write,
) -> Result<DeleteDispatch, DeleteOutputFailure> {
    let output = dispatch.output.take();
    let result = (|| {
        let bytes = render(&dispatch)?;
        let mut output =
            output.ok_or_else(|| output_error(io::Error::other("Output already consumed")))?;
        let writer: &mut dyn Write = match &mut output.sink {
            OutputSink::Stdout => stdout,
            OutputSink::File(writer) => writer.as_mut(),
        };
        writer.write_all(&bytes).map_err(output_error)?;
        writer.flush().map_err(output_error)
    })();
    match result {
        Ok(()) => Ok(dispatch),
        Err(error) => Err(DeleteOutputFailure {
            dispatch: Box::new(dispatch),
            error,
        }),
    }
}
