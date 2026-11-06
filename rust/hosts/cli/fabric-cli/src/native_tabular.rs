// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Caller-injected run/query/execute frontdoors, not native service backends.
//!
//! Paths, text, file names, engine IDs and duration spellings remain opaque.
//! Providers prepare their own bindings, files/JSON, permissions and requested
//! wait/timeout behavior. This host never opens input files, reads stdin, scans
//! SQL, selects an engine, submits a job, or constructs a runtime/credential.
//! Query bindings must be proven read-only; required approval is never a CLI flag.
//!
//! Query and execute share an explicit frontend stdin rule: `--file -` or
//! `--parameters -` selects one provider-owned reader, never both. Run accepts
//! optional `--input -`; only the prepared target decides whether input may be
//! omitted. No-export invocations are parsed, but this bounded host rejects
//! configured execution before effects rather than inventing default rendering.

use crate::tabular_export::{
    self, ExecutionFailure, ExportDelivery, ExportRequest, ExportSink, TableResult,
};
use clap::{Arg, ArgAction, ArgGroup, ArgMatches, Command, error::ErrorKind};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

/// One statement source; files and stdin are resolved only by backend preparation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextSource {
    /// Exact inline text, without quote stripping, expansion or unescaping.
    Inline(String),
    /// Exact file spelling; `-` denotes provider-owned stdin.
    File(String),
}

/// Pinned run-r13 frontend options, without scheduler or service defaults.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunCommand {
    /// Optional provider-owned JSON/file input; `-` selects stdin.
    pub input: Option<String>,
    /// Explicit caller request; the provider must bind its actual semantics.
    pub wait: bool,
    /// Opaque duration spelling, validated by preparation, with no default.
    pub timeout: Option<String>,
}

/// Pinned query-r16 frontend options over a proven read-only native binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryCommand {
    /// Exactly one inline or file statement source.
    pub text: TextSource,
    /// Optional provider-owned parameters file; `-` selects stdin.
    pub parameters: Option<String>,
    /// Exact advertised engine ID; no language or engine is inferred.
    pub engine: Option<String>,
}

/// Pinned execute-r6 frontend options, with provider-owned approval and execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecuteCommand {
    /// Exactly one inline or file command source.
    pub text: TextSource,
    /// Optional provider-owned parameters file; `-` uses the query stdin rule.
    pub parameters: Option<String>,
    /// Explicit request, never a fabricated asynchronous handle or polling loop.
    pub wait: bool,
}

/// The single operation discriminator for the shared native tabular host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Invoke an executable target through a caller-injected provider.
    Run(RunCommand),
    /// Evaluate text through a proven read-only binding.
    Query(QueryCommand),
    /// Execute text through a prepared permission/approval policy.
    Execute(ExecuteCommand),
}

/// A real parsed root invocation with a shared path and export request.
#[derive(Clone, Debug)]
pub struct NativeCommand {
    /// Exact target spelling, resolved by the provider without host expansion.
    pub path: String,
    /// Typed operation and only the flags belonging to that root.
    pub operation: Operation,
    /// Explicit shared format/file pair; absence never starts an export operation.
    pub export: Option<ExportRequest>,
}

impl NativeCommand {
    /// Canonical registered root name, not a provider-supplied result label.
    pub const fn name(&self) -> &'static str {
        match &self.operation {
            Operation::Run(_) => "run",
            Operation::Query(_) => "query",
            Operation::Execute(_) => "execute",
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.path.trim().is_empty() {
            return Err("A nonblank target is required");
        }
        let valid_file = |file: Option<&str>| file.is_none_or(|file| !file.trim().is_empty());
        match &self.operation {
            Operation::Run(run) => {
                if !valid_file(run.input.as_deref()) || !valid_file(run.timeout.as_deref()) {
                    return Err("Run input and duration values must be nonblank");
                }
            }
            Operation::Query(query) => {
                validate_text(&query.text, query.parameters.as_deref())?;
                if !valid_file(query.engine.as_deref()) {
                    return Err("An advertised engine ID must be nonblank");
                }
            }
            Operation::Execute(execute) => {
                validate_text(&execute.text, execute.parameters.as_deref())?;
            }
        }
        Ok(())
    }
}

fn validate_text(text: &TextSource, parameters: Option<&str>) -> Result<(), &'static str> {
    let value = match text {
        TextSource::Inline(value) | TextSource::File(value) => value,
    };
    if value.trim().is_empty() || parameters.is_some_and(|value| value.trim().is_empty()) {
        return Err("Statement sources and parameter paths must be nonblank");
    }
    if matches!(text, TextSource::File(file) if file == "-") && parameters == Some("-") {
        return Err("Only one input may read stdin");
    }
    Ok(())
}

/// A provider's explicit preparation evidence; absence is never permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Evidence {
    /// The caller-owned provider proved this property for this request.
    Proven,
    /// The selected binding does not support or permit this property.
    Denied,
    /// Preparation could not establish the property.
    Unknown,
}

/// Trusted provider approval evidence, never constructed from a CLI bypass flag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Approval {
    /// The binding's verified policy requires no approval for this request.
    NotRequired,
    /// Required approval has actually been obtained by the provider.
    Granted,
    /// Policy requires approval which has not been obtained.
    Required,
    /// Required approval was denied.
    Denied,
    /// Approval requirements or evidence are not established.
    Unknown,
}

/// Prepared capabilities for one resolved target and the exact parsed request.
#[derive(Clone, Debug)]
pub struct Capabilities {
    /// Resolved target/binding, not an assumed route from its path suffix.
    pub binding: Evidence,
    /// All input, parameters, optional-input, wait and timeout semantics are bound.
    pub input: Evidence,
    /// The caller is permitted to perform the requested operation.
    pub permission: Evidence,
    /// Query's binding is enforced read-only, independently of its opaque text.
    pub read_only: Evidence,
    /// The provider's actual policy decision for this operation.
    pub approval: Approval,
    /// A native tabular result adapter exists; metadata messages are not rows.
    pub tabular: Evidence,
    /// This binding can return completed results, not only acceptance/progress.
    pub complete: Evidence,
    /// The adapter can provide real result columns, including empty-table schemas.
    pub schema: Evidence,
    /// Known set count, or unknown until execution; multiple sets are never dropped.
    pub resultsets: Option<usize>,
    /// Actual advertised IDs; an explicit query engine must match exactly.
    pub engines: Vec<String>,
    /// Safe binding/permission evidence retained when policy/preflight rejects execution.
    pub metadata: Value,
}

/// Native execution evidence, independent of file or receipt delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionState {
    /// The provider did not invoke or submit the operation.
    NotStarted,
    /// Native execution completed; result retrieval/validation may still fail.
    Succeeded,
    /// Native execution failed.
    Failed,
    /// Submission was accepted, without completion proof.
    Accepted,
    /// Only partial results/progress are known.
    Partial,
    /// Execution/effects cannot be established.
    Unknown,
}

impl ExecutionState {
    /// Stable evidence label, without inventing a native lifecycle or handle.
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Accepted => "accepted",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }
}

/// A typed provider failure retaining known execution/effect metadata.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct BackendFailure {
    /// Provider-sanitized error code, not a supplied statement or secret.
    pub code: String,
    /// Provider-sanitized diagnostic; never copy input text or file contents here.
    pub message: String,
    /// Actual known native execution state, not inferred from an I/O failure.
    pub execution: ExecutionState,
    /// Safe real handles/effect metadata; not partial rows or statement contents.
    pub metadata: Value,
}

/// Provider outcomes; accepted/partial/unknown evidence is never a complete table.
#[derive(Clone, Debug)]
pub enum BackendResult {
    /// Complete successful native rowsets, with genuine empty schemas if needed.
    Complete(Box<TableResult>),
    /// Real acceptance/handle metadata only; this host never polls or fabricates rows.
    Accepted(Value),
    /// Real partial-progress metadata only; no partial data file is published.
    Partial(Value),
    /// Unknown execution/effect metadata, retained without a success claim.
    Unknown(Value),
}

impl BackendResult {
    /// Retain a complete result without inflating acceptance/partial outcome values.
    pub fn complete(result: TableResult) -> Self {
        Self::Complete(Box::new(result))
    }
}

/// A future on the caller's existing runtime, with no ambient host I/O.
pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One prepared, consumed-once operation supplied by a trusted caller.
pub trait PreparedOperation<'a>: Send {
    /// Evidence for this exact request, resolved before any invocation/submission.
    fn capabilities(&self) -> &Capabilities;
    /// Invoke once; return complete native data or truthful retained effects.
    fn execute(self: Box<Self>) -> BackendFuture<'a, Result<BackendResult, BackendFailure>>;
}

/// Object-safe provider boundary; libraries neither discover nor construct one.
pub trait Backend: Send + Sync {
    /// Resolve inputs, binding, permission and capabilities without invoking/submitting.
    ///
    /// This is where a provider validates target-specific duration/JSON schemas,
    /// optional run input and advertised engine/wait behavior. No CLI flag grants
    /// approval. The returned operation is consumed at most once by [`dispatch`].
    fn prepare<'a>(
        &'a self,
        command: &'a NativeCommand,
    ) -> BackendFuture<'a, Result<Box<dyn PreparedOperation<'a> + 'a>, BackendFailure>>;
}

fn failure(command: &NativeCommand, stage: &str, error: BackendFailure) -> ExportDelivery {
    let mut delivery = if let Some(request) = &command.export {
        ExportDelivery::host_failed(command.name(), request, &error.code, &error.message, 1)
    } else {
        ExportDelivery {
            receipt: json!({
                "source": command.name(),
                "execution": {"status": error.execution.name()},
                "delivery": {"status": "failed", "error": {"code": error.code, "message": error.message}}
            }),
            exit_code: 1,
        }
    };
    delivery.receipt["execution"]["status"] = json!(error.execution.name());
    delivery.receipt["execution"]["metadata"] = error.metadata;
    delivery.receipt["delivery"]["stage"] = json!(stage);
    delivery
}

fn rejected(command: &NativeCommand, code: &str, message: &str, metadata: Value) -> ExportDelivery {
    failure(
        command,
        "preparation",
        BackendFailure {
            code: code.into(),
            message: message.into(),
            execution: ExecutionState::NotStarted,
            metadata,
        },
    )
}

/// Truthful binary/default-host result, without reading files/configuration or stdin.
pub fn not_configured(command: &NativeCommand) -> ExportDelivery {
    rejected(
        command,
        "NotConfigured",
        "This command requires a caller-injected native tabular backend.",
        Value::Null,
    )
}

fn capability_error(
    command: &NativeCommand,
    capabilities: &Capabilities,
) -> Option<(&'static str, &'static str)> {
    for (evidence, code, message) in [
        (
            capabilities.binding,
            "binding_not_proven",
            "A resolved native binding is required.",
        ),
        (
            capabilities.input,
            "input_not_prepared",
            "The provider must bind all requested input and execution options.",
        ),
        (
            capabilities.permission,
            "permission_not_proven",
            "Permission for this operation is not established.",
        ),
        (
            capabilities.tabular,
            "tabular_not_supported",
            "The binding does not prove a native tabular result adapter.",
        ),
        (
            capabilities.complete,
            "complete_results_not_supported",
            "The binding does not prove completed native results.",
        ),
        (
            capabilities.schema,
            "schema_unavailable",
            "The binding must expose real result columns.",
        ),
    ] {
        if evidence != Evidence::Proven {
            return Some((code, message));
        }
    }
    if matches!(command.operation, Operation::Query(_))
        && capabilities.read_only != Evidence::Proven
    {
        return Some((
            "read_only_binding_required",
            "Query requires a proven read-only native binding.",
        ));
    }
    match capabilities.approval {
        Approval::Required => {
            return Some((
                "approval_required",
                "Required provider approval has not been obtained.",
            ));
        }
        Approval::Denied => {
            return Some(("approval_denied", "Required provider approval was denied."));
        }
        Approval::Unknown => {
            return Some((
                "approval_not_proven",
                "Provider approval policy is not established.",
            ));
        }
        Approval::NotRequired | Approval::Granted => {}
    }
    if let Operation::Query(query) = &command.operation
        && let Some(engine) = &query.engine
        && !capabilities
            .engines
            .iter()
            .any(|advertised| advertised == engine)
    {
        return Some((
            "unsupported_engine",
            "The requested engine is not advertised by the selected binding.",
        ));
    }
    None
}

/// Prepare one parsed request, enforce trusted policy, and export one complete table.
///
/// Known multi-set output is rejected by the shared policy before execution.
/// Missing providers and unsupported no-export presentation fail before preparation.
/// File preflight, codec/schema failures and receipt I/O use the existing shared
/// export entry points; neither this function nor its writers replay an operation.
pub async fn dispatch(
    command: &NativeCommand,
    backend: Option<&dyn Backend>,
    sink: &mut impl ExportSink,
) -> ExportDelivery {
    let Some(backend) = backend else {
        return not_configured(command);
    };
    if let Err(message) = command.validate() {
        return rejected(command, "invalid_arguments", message, Value::Null);
    }
    let Some(request) = &command.export else {
        return rejected(
            command,
            "explicit_export_required",
            "This configured host requires explicit --format and --output before execution.",
            Value::Null,
        );
    };
    let prepared = match backend.prepare(command).await {
        Ok(prepared) => prepared,
        Err(error) => return failure(command, "preparation", error),
    };
    let capabilities = prepared.capabilities();
    if let Some((code, message)) = capability_error(command, capabilities) {
        return rejected(command, code, message, capabilities.metadata.clone());
    }
    let resultsets = capabilities.resultsets;
    let preparation_metadata = capabilities.metadata.clone();
    let mut retained = None;
    let mut missing_schema = false;
    let retained_ref = &mut retained;
    let missing_schema_ref = &mut missing_schema;
    let source = command.name();
    let mut delivery = tabular_export::deliver(request, source, resultsets, sink, || async move {
        let error = match prepared.execute().await {
            Ok(BackendResult::Complete(mut result)) => {
                result.source = source.into();
                if result.resultsets.len() == 1
                    && result.resultsets[0].is_empty()
                    && result.empty_schema.is_none()
                    && result.declared_table.is_none()
                {
                    *missing_schema_ref = true;
                    result.validation_error =
                        Some("The native empty result did not supply its real columns".into());
                }
                return Ok(*result);
            }
            Ok(BackendResult::Accepted(metadata)) => BackendFailure {
                code: "incomplete_result".into(),
                message: "Native acceptance is not a completed table.".into(),
                execution: ExecutionState::Accepted,
                metadata,
            },
            Ok(BackendResult::Partial(metadata)) => BackendFailure {
                code: "incomplete_result".into(),
                message: "Partial native results are not a completed table.".into(),
                execution: ExecutionState::Partial,
                metadata,
            },
            Ok(BackendResult::Unknown(metadata)) => BackendFailure {
                code: "incomplete_result".into(),
                message: "Native completion is unknown.".into(),
                execution: ExecutionState::Unknown,
                metadata,
            },
            Err(error) => error,
        };
        let shared = ExecutionFailure {
            code: error.code.clone(),
            message: error.message.clone(),
        };
        *retained_ref = Some(error);
        Err(shared)
    })
    .await;
    if let Some(error) = retained {
        delivery.receipt["execution"]["status"] = json!(error.execution.name());
        delivery.receipt["execution"]["metadata"] = error.metadata;
    } else if delivery.receipt["execution"]["status"] == "not_started" {
        delivery.receipt["execution"]["metadata"] = preparation_metadata;
    }
    if missing_schema {
        delivery.receipt["delivery"]["error"]["code"] = json!("schema_unavailable");
    }
    delivery
}

fn text_arguments(command: Command) -> Command {
    command
        .arg(Arg::new("text").long("text"))
        .arg(Arg::new("file").long("file"))
        .arg(Arg::new("parameters").long("parameters"))
        .group(
            ArgGroup::new("statement")
                .args(["text", "file"])
                .required(true)
                .multiple(false),
        )
}

pub(crate) fn register(mut root: Command) -> Command {
    let path = || Arg::new("path").required(true).value_name("PATH");
    let run = Command::new("run")
        .about("Run through a caller-injected native tabular backend")
        .arg(path())
        .arg(Arg::new("input").long("input"))
        .arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue))
        .arg(Arg::new("timeout").long("timeout").value_name("DURATION"));
    let query = text_arguments(
        Command::new("query")
            .about("Query a proven read-only native binding")
            .arg(path()),
    )
    .arg(Arg::new("engine").long("engine"));
    let execute = text_arguments(
        Command::new("execute")
            .about("Execute through a prepared native policy")
            .arg(path()),
    )
    .arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue));
    for command in [run, query, execute] {
        root = root.subcommand(
            tabular_export::arguments(command, Some("output_format"))
                .expect("native tabular roots do not have input/output flag collisions"),
        );
    }
    root
}

pub(crate) fn from_matches(name: &str, values: &ArgMatches) -> Result<NativeCommand, clap::Error> {
    let string = |id: &str| values.get_one::<String>(id).cloned();
    let text = || match (string("text"), string("file")) {
        (Some(text), None) => Some(TextSource::Inline(text)),
        (None, Some(file)) => Some(TextSource::File(file)),
        _ => None,
    };
    let invalid = || clap::Error::raw(ErrorKind::InvalidValue, crate::parser::INVALID_ARGUMENTS);
    let operation = match name {
        "run" => Operation::Run(RunCommand {
            input: string("input"),
            wait: values.get_flag("wait"),
            timeout: string("timeout"),
        }),
        "query" => Operation::Query(QueryCommand {
            text: text().ok_or_else(invalid)?,
            parameters: string("parameters"),
            engine: string("engine"),
        }),
        "execute" => Operation::Execute(ExecuteCommand {
            text: text().ok_or_else(invalid)?,
            parameters: string("parameters"),
            wait: values.get_flag("wait"),
        }),
        _ => return Err(invalid()),
    };
    let command = NativeCommand {
        path: string("path").ok_or_else(invalid)?,
        operation,
        export: tabular_export::request(values),
    };
    command.validate().map_err(|_| invalid())?;
    Ok(command)
}
