// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public SDK execution through the accepted SDK-layer neutral contract bridge.

use crate::{
    InputSource, OntologyArguments, OntologyTarget as Representation, OntologyVerb,
    OutputDestination, decode_json_input, validate_arguments,
};
use fabric_sdk_items::{ItemClient, ItemErrorKind, ItemExecutor, ItemGetOptions};
use fabric_sdk_ontology::{
    self as sdk, DefinitionLimits, DefinitionReplacement, ModelForm, ModelReplacement, Observation,
    PublicError, PublicErrorKind, PublicOutcome,
    v2::{
        ApprovalPolicy, Availability, Binding, ContractError, Coverage, Effect, ExecutionEvidence,
        ExecutionReceipt, FailureInfo, Generation, OperationHandle, Outcome, Precondition,
        PreparedOperation, ReceiptBuildError, ReceiptError, Registry, ResolvedTarget, TargetKind,
        Verb,
    },
};
use fabric_sdk_operations::{
    OperationError, OperationErrorKind, OperationExecutor, OperationLimits, OperationSession,
    OperationWarning,
};
use serde_json::{Value, json};
use std::{
    fmt,
    future::Future,
    io::{self, Write},
    num::NonZeroUsize,
    time::Duration,
};

/// Caller-selected finite bounds; no draft defaults or hidden watchdog are added.
#[derive(Clone, Copy, Debug)]
pub struct AdapterLimits {
    /// Strict local JSON input bytes.
    pub input_bytes: NonZeroUsize,
    /// Serialized returned data before projection/envelope.
    pub data_bytes: NonZeroUsize,
    /// Native part count and aggregate decoded content.
    pub definition: DefinitionLimits,
    /// Shared SDK request/response/polling policy.
    pub operation: OperationLimits,
}

/// Explicit host refusal; details must be safe to display.
#[derive(Debug)]
pub struct HostError(pub String);

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for HostError {}

/// Caller-owned path, input, approval, deadline and output boundaries.
///
/// Implementations must not acquire credentials or dispatch remote effects in
/// resolution, input loading, output reservation or approval. `reserve_output`
/// must atomically acquire a no-clobber destination (including collision/race
/// policy), never merely check existence. Its writer remains reserved through
/// dispatch and output. `authorize` must verify conditional gates and exact
/// scoped effects/preconditions, including unguarded concurrency where present.
pub trait AdapterHost: ApprovalPolicy<Error = HostError> {
    /// Resolve exactly one path without remote dispatch or credential discovery.
    fn resolve(&mut self, path: &str) -> Result<ResolvedTarget, HostError>;
    /// Read at most the explicit byte bound; stdin may be consumed only once.
    fn read_input(
        &mut self,
        source: &InputSource,
        maximum: NonZeroUsize,
    ) -> Result<Vec<u8>, HostError>;
    /// Atomically reserve a local file, or supply the caller's stdout writer.
    fn reserve_output(
        &mut self,
        destination: &OutputDestination,
    ) -> Result<Box<dyn Write>, HostError>;
    /// Write a complete failure receipt or warning on stderr, never stdout.
    fn diagnostic(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Bind one finite total deadline/cancellation policy before SDK dispatch.
    ///
    /// The caller-provided cancellation future must observe this deadline. The
    /// host must reject an unavailable deadline implementation, not ignore it.
    fn deadline(&mut self, explicit: Option<Duration>) -> Result<(), HostError>;
}

/// Original receipt and typed SDK evidence, independent of presentation success.
#[derive(Debug)]
pub struct ExecutionReport {
    /// Shared typed receipt with original target, effects and handles.
    pub receipt: ExecutionReceipt<Value>,
    /// Original provider failure; never serialized implicitly.
    pub sdk_error: Option<PublicError>,
    /// Original shared-engine warnings.
    pub warnings: Vec<OperationWarning>,
}

impl ExecutionReport {
    /// Accepted submission succeeds only as a submission, not completion.
    pub fn command_succeeded(&self) -> bool {
        matches!(
            self.receipt.outcome(),
            Outcome::Succeeded { .. } | Outcome::Accepted
        )
    }
}

/// Adapter failure before dispatch or after preserving original execution evidence.
#[derive(Debug)]
pub enum AdapterError {
    /// Sanitized local validation failure, with no credential or remote dispatch.
    Invalid(&'static str),
    /// Registry selection or registration failure.
    Contract(ContractError),
    /// Neutral preparation/effect/precondition failure.
    Preparation(ReceiptError),
    /// Host preflight/approval refusal before SDK work.
    Host(HostError),
    /// Contradictory SDK evidence retained in full, never replaced with success.
    Receipt(Box<ReceiptBuildError<Value>>),
    /// Output/diagnostic failed after execution; inspect the original report.
    Output {
        /// Original writer error, not evidence the remote operation failed.
        source: io::Error,
        /// Original outcome, handles, uncertainty and typed SDK cause; never replay.
        report: Box<ExecutionReport>,
    },
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "Ontology command failed; inspect local classification or retained execution evidence",
        )
    }
}
impl std::error::Error for AdapterError {}

/// Binds accepted public selectors to caller-registered descriptors transactionally.
///
/// No descriptors or operation identities are duplicated. The caller registers
/// canonical IDs with exact schemas/effects; absence or overlap leaves `registry`
/// unchanged. Every binding remains conditional on host/provider readiness.
///
/// # Errors
/// Rejects missing/mismatched descriptors, duplicate aliases and ambiguous bindings.
pub fn bind_public_operations(registry: &mut Registry) -> Result<(), ContractError> {
    let mut candidate = registry.clone();
    candidate.alias("ontology", "Ontology")?;
    for (facet, representation, id, verb, generation) in [
        (
            "metadata",
            "json",
            "item.metadata.get",
            Verb::Get,
            Generation::Any,
        ),
        (
            "definition",
            "native",
            "item.definition.get",
            Verb::Get,
            Generation::Any,
        ),
        (
            "definition",
            "native",
            "item.definition.replace",
            Verb::Update,
            Generation::Any,
        ),
        (
            "model",
            "minimized",
            "ontology.model.get",
            Verb::Get,
            Generation::Exact(2),
        ),
        (
            "model",
            "expanded",
            "ontology.model.get",
            Verb::Get,
            Generation::Exact(2),
        ),
        (
            "model",
            "minimized",
            "ontology.model.replace",
            Verb::Update,
            Generation::Exact(2),
        ),
    ] {
        if !candidate
            .operations()
            .any(|operation| operation.id() == id && operation.verb() == verb)
        {
            return Err(ContractError::UnknownOperation);
        }
        candidate.bind(Binding::new(
            "Ontology", facet, representation, generation, id,
            Availability::Conditional(vec![sdk::v2::Prerequisite {
                id: "ontology.public.host".into(),
                description: "Host verifies public endpoint, feature gates, scoped authorization and output reservation.".into(),
            }]),
        )?)?;
    }
    *registry = candidate;
    Ok(())
}

enum Request {
    Metadata,
    Definition,
    Model(ModelForm),
    ReplaceDefinition(DefinitionReplacement),
    ReplaceModel(ModelReplacement),
}

fn representation(target: &ResolvedTarget) -> Result<Representation, AdapterError> {
    if target.selection().kind != TargetKind::Item || target.child().is_some() {
        return Err(AdapterError::Invalid(
            "This public slice requires one exact owner item.",
        ));
    }
    match (
        target.selection().facet.as_str(),
        target.selection().representation.as_str(),
    ) {
        ("metadata", "json") => Ok(Representation::Item),
        ("definition", "native") => Ok(Representation::Definition),
        ("model", "minimized") => Ok(Representation::PublicModelMinimized),
        ("model", "expanded") => Ok(Representation::PublicModelExpanded),
        _ => Err(AdapterError::Invalid(
            "Unsupported public Ontology representation.",
        )),
    }
}

fn request(
    args: &OntologyArguments,
    representation: Representation,
    body: Option<Value>,
    limits: AdapterLimits,
) -> Result<Request, AdapterError> {
    match (args.verb, representation) {
        (OntologyVerb::Get, Representation::Item) => Ok(Request::Metadata),
        (OntologyVerb::Get, Representation::Definition) => Ok(Request::Definition),
        (OntologyVerb::Get, Representation::PublicModelMinimized) => {
            Ok(Request::Model(ModelForm::Minimized))
        }
        (OntologyVerb::Get, Representation::PublicModelExpanded) => {
            Ok(Request::Model(ModelForm::Expanded))
        }
        (OntologyVerb::Update, Representation::Definition) => {
            let body = body.ok_or(AdapterError::Invalid("Definition input required."))?;
            let envelope = body
                .as_object()
                .ok_or(AdapterError::Invalid("Invalid definition envelope."))?;
            if envelope.len() != 1 || !envelope.contains_key("definition") {
                return Err(AdapterError::Invalid(
                    "Only the native definition envelope is accepted.",
                ));
            }
            DefinitionReplacement::new(
                body["definition"].clone(),
                limits.definition,
                false,
                limits.input_bytes,
            )
            .map(Request::ReplaceDefinition)
            .map_err(|_| AdapterError::Invalid("Invalid native definition input."))
        }
        (OntologyVerb::Update, Representation::PublicModelMinimized) => {
            let body = body.ok_or(AdapterError::Invalid("Model input required."))?;
            let object = body
                .as_object()
                .ok_or(AdapterError::Invalid("Invalid model envelope."))?;
            if object
                .keys()
                .any(|key| key != "model" && key != "description")
            {
                return Err(AdapterError::Invalid(
                    "Unknown public model authoring field.",
                ));
            }
            let description = match object.get("description") {
                None => None,
                Some(Value::String(value)) => Some(value.clone()),
                _ => return Err(AdapterError::Invalid("Invalid model description.")),
            };
            ModelReplacement::new(
                body.get("model")
                    .cloned()
                    .ok_or(AdapterError::Invalid("Model required."))?,
                description,
            )
            .map(Request::ReplaceModel)
            .map_err(|_| AdapterError::Invalid("Invalid public model input."))
        }
        _ => Err(AdapterError::Invalid(
            "This public operation is not enabled.",
        )),
    }
}

fn common_verb(verb: OntologyVerb) -> Result<Verb, AdapterError> {
    match verb {
        OntologyVerb::Get => Ok(Verb::Get),
        OntologyVerb::Update => Ok(Verb::Update),
        _ => Err(AdapterError::Invalid(
            "This public execution slice supports Get and replacement only.",
        )),
    }
}

/// Executes one public SDK request after complete local preflight and approval.
///
/// Generic and facade entry points share this method and the same resolved
/// registry descriptor; `facade` selects only the owner restriction. The host
/// provides the original runtime/credential/executor/cancellation future. No
/// request is replayed to repair a receipt or failed output write.
///
/// # Errors
/// Preserves local/host refusal before SDK access and original execution evidence
/// on receipt/output failure. Unsupported projection is rejected, never ignored.
#[allow(clippy::too_many_arguments)]
pub async fn execute<E, H>(
    args: &OntologyArguments,
    facade: bool,
    registry: &Registry,
    host: &mut H,
    client: &ItemClient,
    executor: E,
    limits: AdapterLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<ExecutionReport, AdapterError>
where
    E: OperationExecutor + ItemExecutor,
    H: AdapterHost,
{
    let verb = common_verb(args.verb)?;
    if args.project.is_some() {
        return Err(AdapterError::Invalid(
            "Data projection is not enabled in this execution slice.",
        ));
    }
    let resolved = host.resolve(&args.path).map_err(AdapterError::Host)?;
    let selection = resolved.selection();
    let representation = representation(&resolved)?;
    validate_arguments(
        args,
        representation,
        &selection.owner_type,
        selection.generation.map(u64::from),
    )
    .map_err(|_| AdapterError::Invalid("Arguments do not match the resolved representation."))?;
    let resolution = registry
        .resolve(verb, selection, facade.then_some("ontology"))
        .map_err(AdapterError::Contract)?;
    let expected_id = match (verb, representation) {
        (Verb::Get, Representation::Item) => "item.metadata.get",
        (Verb::Get, Representation::Definition) => "item.definition.get",
        (Verb::Get, Representation::PublicModelMinimized | Representation::PublicModelExpanded) => {
            "ontology.model.get"
        }
        (Verb::Update, Representation::Definition) => "item.definition.replace",
        (Verb::Update, Representation::PublicModelMinimized) => "ontology.model.replace",
        _ => return Err(AdapterError::Invalid("Unsupported operation descriptor.")),
    };
    if resolution.operation.id() != expected_id {
        return Err(AdapterError::Invalid(
            "The registered descriptor does not identify this SDK operation.",
        ));
    }
    let mut body = None;
    if let Some(source) = &args.input {
        let bytes = host
            .read_input(source, limits.input_bytes)
            .map_err(AdapterError::Host)?;
        body = Some(
            decode_json_input(Some(&bytes), "application/json", limits.input_bytes)
                .map_err(|_| AdapterError::Invalid("Invalid bounded JSON input."))?,
        );
    }
    let request = request(args, representation, body.clone(), limits)?;
    let sdk_target = sdk::OntologyTarget::new(
        resolved.identity().workspace_id(),
        resolved
            .identity()
            .item_id()
            .ok_or(AdapterError::Invalid("Item identity required."))?,
    )
    .map_err(|_| AdapterError::Invalid("Invalid SDK target identity."))?;
    let output = match &args.output {
        OutputDestination::Stdout => json!({"kind":"stdout"}),
        OutputDestination::File(path) => {
            json!({"kind":"file","path":path.to_str().ok_or(AdapterError::Invalid("Output path is not representable in approval evidence."))?})
        }
    };
    let mut prepared = PreparedOperation::new(
        resolution,
        resolved,
        json!({
            "body":body, "output":output, "wait":args.wait, "requestedPath":args.path,
            "timeoutNanos":args.timeout.map(|value|value.as_nanos().to_string()),
            "maxDataBytes":args.max_bytes.unwrap_or(limits.data_bytes).get(),
            "maxInputBytes":limits.input_bytes.get(),
            "profile":args.profile,
            "context":{
                "account":client.selection().account(),
                "tenant":client.selection().tenant(),
                "audience":client.selection().audience().scope(),
                "endpoint":client.context().endpoint().host(),
            },
            "bounds":{
                "maxRequests":limits.operation.max_requests.get(),
                "maxResponseBytes":limits.operation.max_response_bytes.get(),
                "maxPollDelayNanos":limits.operation.max_poll_delay.as_nanos().to_string(),
                "maxDefinitionParts":limits.definition.max_parts.get(),
                "maxDecodedDefinitionBytes":limits.definition.max_decoded_bytes.get(),
            },
        }),
    )
    .map_err(AdapterError::Preparation)?;
    let mut required = vec![Effect::RemoteRead];
    if args.verb == OntologyVerb::Update {
        required.push(Effect::RemoteWrite);
    }
    if representation == Representation::PublicModelMinimized
        || representation == Representation::PublicModelExpanded
    {
        required.push(Effect::MeteredConsumption);
    }
    if required
        .iter()
        .any(|effect| !prepared.effects().contains(effect))
    {
        return Err(AdapterError::Invalid(
            "Registered descriptor omits required effects.",
        ));
    }
    if matches!(args.output, OutputDestination::File(_)) {
        prepared = prepared
            .with_optional_effects(vec![Effect::LocalWrite])
            .map_err(AdapterError::Preparation)?;
    }
    if args.verb == OntologyVerb::Update {
        prepared = prepared
            .with_preconditions(vec![
                Precondition::ExistingOnly,
                Precondition::UnguardedConcurrency,
            ])
            .map_err(AdapterError::Preparation)?;
    }
    host.deadline(args.timeout).map_err(AdapterError::Host)?;
    let mut writer = host
        .reserve_output(&args.output)
        .map_err(AdapterError::Host)?;
    let approved = prepared.authorize(host).map_err(AdapterError::Host)?;
    let observation = if args.wait {
        Observation::WaitForResult
    } else {
        Observation::ReturnAccepted
    };
    let result = match request {
        Request::Metadata => {
            let result = sdk::get_metadata(
                client,
                &executor,
                &sdk_target,
                ItemGetOptions {
                    query: vec![],
                    max_response_bytes: limits.operation.max_response_bytes,
                },
                cancelled,
            )
            .await;
            result.map(|value| value.metadata().as_value().clone())
        }
        request => {
            let mut session = OperationSession::with_context(
                client.context(),
                executor,
                limits.operation,
                cancelled,
            );
            let result = match request {
                Request::Definition => {
                    sdk::get_definition(&mut session, &sdk_target, limits.definition, observation)
                        .await
                        .map(|result| map_public(result, |value| value.as_value().clone()))
                }
                Request::Model(form) => {
                    sdk::get_model(&mut session, &sdk_target, form, observation)
                        .await
                        .map(|result| map_public(result, |value| value.as_value().clone()))
                }
                Request::ReplaceModel(input) => {
                    sdk::replace_model(&mut session, &sdk_target, &input, observation)
                        .await
                        .map(|result| map_public(result, |value| value.as_value().clone()))
                }
                Request::ReplaceDefinition(input) => {
                    sdk::replace_definition(&mut session, &sdk_target, &input, observation)
                        .await
                        .map(|result| map_public(result, |_| Value::Null))
                }
                Request::Metadata => unreachable!("metadata handled before operation session"),
            };
            let (outcome, evidence, warnings, error) = match result {
                Ok(result) => result,
                Err(error) => failure(error),
            };
            let mut report = report(&approved, outcome, evidence, warnings, error)?;
            enforce_data_bound(
                &approved,
                &mut report,
                args.max_bytes.unwrap_or(limits.data_bytes),
            )?;
            return present(host, &mut writer, report);
        }
    };
    let (outcome, evidence, warnings, error) = match result {
        Ok(data) => (
            Outcome::Succeeded { data },
            ExecutionEvidence {
                http_status: None,
                handles: vec![],
                coverage: Coverage::Complete,
                mutation_may_have_occurred: false,
            },
            vec![],
            None,
        ),
        Err(error) => failure(error),
    };
    let mut report = report(&approved, outcome, evidence, warnings, error)?;
    enforce_data_bound(
        &approved,
        &mut report,
        args.max_bytes.unwrap_or(limits.data_bytes),
    )?;
    present(host, &mut writer, report)
}

type DispatchResult = (
    Outcome<Value>,
    ExecutionEvidence,
    Vec<OperationWarning>,
    Option<PublicError>,
);

fn map_public<T>(result: PublicOutcome<T>, map: impl FnOnce(T) -> Value) -> DispatchResult {
    match result {
        PublicOutcome::Completed {
            data,
            response,
            operation_id,
            mutation_may_have_occurred,
            warnings,
            ..
        } => (
            Outcome::Succeeded { data: map(data) },
            ExecutionEvidence {
                http_status: Some(response.status),
                handles: handles(operation_id),
                coverage: Coverage::Complete,
                mutation_may_have_occurred,
            },
            warnings,
            None,
        ),
        PublicOutcome::Accepted {
            operation_id,
            response,
            mutation_may_have_occurred,
            warnings,
            ..
        } => (
            Outcome::Accepted,
            ExecutionEvidence {
                http_status: Some(response.status),
                handles: handles(Some(operation_id)),
                coverage: Coverage::Unknown,
                mutation_may_have_occurred,
            },
            warnings,
            None,
        ),
    }
}

fn handles(id: Option<String>) -> Vec<OperationHandle> {
    id.into_iter()
        .map(|id| OperationHandle::FabricOperation { id })
        .collect()
}

fn operation_evidence(error: &OperationError) -> ExecutionEvidence {
    ExecutionEvidence {
        http_status: match &error.kind {
            OperationErrorKind::Http { status, .. } => Some(*status),
            _ => None,
        },
        handles: handles(error.operation_id.clone()),
        coverage: Coverage::Unknown,
        mutation_may_have_occurred: error.mutation_may_have_occurred,
    }
}

fn failure(error: PublicError) -> DispatchResult {
    let mut evidence = ExecutionEvidence {
        http_status: None,
        handles: vec![],
        coverage: Coverage::Unknown,
        mutation_may_have_occurred: false,
    };
    let mut uncertain = false;
    let code = match &error.kind {
        PublicErrorKind::Operation(cause) => {
            evidence = operation_evidence(cause);
            uncertain = match cause.kind {
                OperationErrorKind::OperationFailed { .. } => false,
                OperationErrorKind::Http { .. } => !evidence.handles.is_empty(),
                _ => evidence.mutation_may_have_occurred || !evidence.handles.is_empty(),
            };
            match cause.kind {
                OperationErrorKind::Http { status: 401, .. } => "Unauthenticated",
                OperationErrorKind::Http { status: 403, .. } => "AccessDenied",
                OperationErrorKind::Http { status: 404, .. } => "NotFound",
                OperationErrorKind::Http { status: 409, .. } => "Conflict",
                OperationErrorKind::Execution(
                    fabric_sdk_operations::OperationExecutionError::TimedOut,
                ) => "Timeout",
                _ => "OperationFailed",
            }
        }
        PublicErrorKind::InvalidResponse {
            response,
            evidence: original,
            ..
        } => {
            evidence = operation_evidence(original);
            evidence.http_status = Some(response.status);
            uncertain = evidence.mutation_may_have_occurred || !evidence.handles.is_empty();
            "InvalidResponse"
        }
        PublicErrorKind::BudgetExceeded {
            response,
            evidence: original,
            ..
        } => {
            evidence = operation_evidence(original);
            evidence.http_status = Some(response.status);
            "BudgetExceeded"
        }
        PublicErrorKind::Metadata(cause) => match &cause.kind {
            ItemErrorKind::Http { status, .. } => {
                evidence.http_status = Some(*status);
                if *status == 403 {
                    "AccessDenied"
                } else if *status == 401 {
                    "Unauthenticated"
                } else {
                    "OperationFailed"
                }
            }
            _ => "OperationFailed",
        },
        PublicErrorKind::InvalidInput => "InvalidInput",
        PublicErrorKind::InvalidMetadata(_) => "InvalidResponse",
    };
    let info = FailureInfo {
        code: code.into(),
        message: "Ontology provider request failed; original typed evidence is retained.".into(),
    };
    let outcome = if uncertain {
        Outcome::Unknown { error: info }
    } else {
        Outcome::Failed { error: info }
    };
    (outcome, evidence, vec![], Some(error))
}

fn report(
    approved: &sdk::v2::ApprovedOperation<'_>,
    outcome: Outcome<Value>,
    evidence: ExecutionEvidence,
    warnings: Vec<OperationWarning>,
    sdk_error: Option<PublicError>,
) -> Result<ExecutionReport, AdapterError> {
    Ok(ExecutionReport {
        receipt: ExecutionReceipt::new(approved, outcome, evidence)
            .map_err(AdapterError::Receipt)?,
        warnings,
        sdk_error,
    })
}

fn enforce_data_bound(
    approved: &sdk::v2::ApprovedOperation<'_>,
    report: &mut ExecutionReport,
    maximum: NonZeroUsize,
) -> Result<(), AdapterError> {
    if let Outcome::Succeeded { data } | Outcome::Partial { data, .. } = report.receipt.outcome()
        && serde_json::to_vec(data)
            .map_err(|_| AdapterError::Invalid("Result serialization failed."))?
            .len()
            > maximum.get()
    {
        report.receipt = ExecutionReceipt::new(approved, Outcome::Failed { error:FailureInfo {
            code:"BudgetExceeded".into(), message:"Returned data exceeds the explicit serialized-data bound; no clipped source was emitted.".into(),
        } }, report.receipt.evidence().clone()).map_err(AdapterError::Receipt)?;
    }
    Ok(())
}

fn present<H: AdapterHost>(
    host: &mut H,
    writer: &mut Box<dyn Write>,
    report: ExecutionReport,
) -> Result<ExecutionReport, AdapterError> {
    let mut bytes = serde_json::to_vec(&report.receipt)
        .map_err(|_| AdapterError::Invalid("Receipt serialization failed."))?;
    bytes.push(b'\n');
    let result = if report.command_succeeded() {
        writer.write_all(&bytes).and_then(|_| writer.flush())
    } else {
        host.diagnostic(&bytes)
    };
    if let Err(source) = result {
        return Err(AdapterError::Output {
            source,
            report: Box::new(report),
        });
    }
    if !report.warnings.is_empty()
        && let Err(source) = host.diagnostic(b"Ontology operation returned observation-policy warnings; inspect retained warnings.\n")
    { return Err(AdapterError::Output { source, report:Box::new(report) }); }
    Ok(report)
}
