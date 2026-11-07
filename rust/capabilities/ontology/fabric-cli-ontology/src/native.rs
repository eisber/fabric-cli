// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Closed native entity operations over the released injected workload SDK.

use crate::{
    AdapterHost, AdapterLimits, HostError, OntologyArguments, OntologyTarget as Representation,
    OntologyVerb,
    execution_support::{self as support, CapabilityCause, CapabilityError, CapabilityReport},
    validate_arguments,
};
use fabric_sdk_ontology::v2::{
    Availability, Binding, ContractError, Coverage, Effect, ExecutionEvidence, ExecutionReceipt,
    FailureInfo, Generation, Outcome, Precondition, Prerequisite, Registry, ResolvedTarget,
    TargetKind, Verb,
};
use fabric_sdk_ontology_workload::{
    client::{
        ByteLimits, Features, LineageTag, MemberId, NativePage, OntologyClient, OntologyTarget,
        Outcome as NativeOutcome,
    },
    models::{EntityType, MergePatch, Shape},
    workload::{
        DispatchState, WorkloadError, WorkloadErrorKind, WorkloadExecutor, WorkloadIdentity,
        WorkloadResponse,
    },
};
use serde_json::{Value, json};
use std::{
    future::{Future, poll_fn},
    pin::pin,
    sync::Arc,
    task::Poll,
};

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

/// Explicit private host binding, never discovered or constructed from a CLI URL.
#[derive(Clone)]
pub struct NativeContext {
    /// Existing caller-owned private transport/identity/capacity-routing policy.
    pub executor: Arc<dyn WorkloadExecutor>,
    /// Caller-established native feature gates, verified again by approval policy.
    pub features: Features,
    /// Explicit nonsecret identity/audience/routing binding presented for approval.
    pub binding: NativeBinding,
}

/// Nonsecret evidence binding one native executor to caller-selected context.
///
/// These fields describe existing policy, not provider configuration or public
/// endpoint overrides. Approval verifies the executor implements this binding.
#[derive(Clone)]
pub struct NativeBinding {
    account: String,
    tenant: String,
    audience: String,
    binding_id: String,
}

impl NativeBinding {
    /// Retains explicit account, tenant, native audience and capacity/routing key.
    ///
    /// No default, token, endpoint discovery or credential fallback is accepted.
    ///
    /// # Errors
    /// Rejects empty, control-bearing or unbounded context evidence.
    pub fn new(
        account: &str,
        tenant: &str,
        audience: &str,
        binding_id: &str,
    ) -> Result<Self, HostError> {
        if [account, tenant, audience, binding_id]
            .into_iter()
            .any(|value| {
                value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control)
            })
        {
            return Err(HostError(
                "Explicit nonsecret native identity/audience/binding evidence is required.".into(),
            ));
        }
        Ok(Self {
            account: account.into(),
            tenant: tenant.into(),
            audience: audience.into(),
            binding_id: binding_id.into(),
        })
    }
}

/// Host for private Digital Operations execution, separate from public HTTP.
///
/// The returned executor must use the same explicit identity/profile/deadline as
/// the resolved request. Obtaining the context must not acquire credentials or
/// dispatch remotely. Approval verifies source Build permission and exact
/// affected references; there is no default native host.
pub trait NativeHost: AdapterHost {
    /// Supply the explicitly selected native workload executor and feature gates.
    fn native_context(&mut self) -> Result<NativeContext, HostError>;
}

/// Adds native entity tuples to caller-registered descriptors transactionally.
///
/// The `ontology` owner alias must already exist. No alias, descriptor or MCP
/// tool is added here. The registered DTO is the internal V4 EntityType shape,
/// not public-model JSON or generation-1 definition parts.
///
/// # Errors
/// Rejects missing/mismatched descriptors and overlapping selector tuples.
pub fn bind_native_operations(registry: &mut Registry) -> Result<(), ContractError> {
    let mut candidate = registry.clone();
    for (facet, id, verb) in [
        ("schema.entity", "ontology.entity.get", Verb::Get),
        ("schema.entities", "ontology.entity.list", Verb::List),
        ("schema.entities", "ontology.entity.create", Verb::Create),
        ("schema.entity", "ontology.entity.patch", Verb::Update),
        ("schema.entity", "ontology.entity.delete", Verb::Delete),
        ("schema.binding", "ontology.entity.bind", Verb::Link),
        ("schema.entity", "ontology.entity.rename", Verb::Move),
    ] {
        if !candidate
            .operations()
            .any(|operation| operation.id() == id && operation.verb() == verb)
        {
            return Err(ContractError::UnknownOperation);
        }
        candidate.bind(Binding::new(
            "Ontology", facet, "native", Generation::Exact(2), id,
            Availability::Conditional(vec![Prerequisite {
                id: "ontology.native.host".into(),
                description: "Verify private Digital Operations routing/identity/gates, source Build authorization and exact reference/rename/cascade scope before approval. Ordinary V4 writes do not establish graph publication.".into(),
            }]),
        )?)?;
    }
    *registry = candidate;
    Ok(())
}

enum Request {
    Get(MemberId<EntityType>),
    List,
    Create(EntityType),
    Patch(MemberId<EntityType>, MergePatch<EntityType>),
    Delete(MemberId<EntityType>),
}

fn selection(
    args: &OntologyArguments,
    resolved: &ResolvedTarget,
) -> Result<(&'static str, Representation), CapabilityError> {
    let target = resolved.selection();
    if target.kind != TargetKind::Item || target.representation != "native" {
        return Err(CapabilityError::Invalid(
            "Native schema operations require an exact Ontology owner and native representation.",
        ));
    }
    match (args.verb, target.facet.as_str()) {
        (OntologyVerb::Get, "schema.entity") => Ok(("ontology.entity.get", Representation::Entity)),
        (OntologyVerb::List, "schema.entities") => {
            Ok(("ontology.entity.list", Representation::Entities))
        }
        (OntologyVerb::Create, "schema.entities") => {
            Ok(("ontology.entity.create", Representation::Entities))
        }
        (OntologyVerb::Update, "schema.entity") => {
            Ok(("ontology.entity.patch", Representation::Entity))
        }
        (OntologyVerb::Delete, "schema.entity") => {
            Ok(("ontology.entity.delete", Representation::Entity))
        }
        (OntologyVerb::Link, "schema.binding") => {
            Ok(("ontology.entity.bind", Representation::EntitySourceBinding))
        }
        (OntologyVerb::Move, "schema.entity") => {
            Ok(("ontology.entity.rename", Representation::Entity))
        }
        _ => Err(CapabilityError::Invalid(
            "Unsupported native verb/facet tuple.",
        )),
    }
}

fn member(
    resolved: &ResolvedTarget,
    collection: bool,
) -> Result<Option<MemberId<EntityType>>, CapabilityError> {
    if collection {
        if resolved.child().is_some() {
            return Err(CapabilityError::Invalid(
                "An entity collection cannot carry a child identity.",
            ));
        }
        return Ok(None);
    }
    let child = resolved.child().ok_or(CapabilityError::Invalid(
        "Native entity lineage identity required.",
    ))?;
    if child.kind != "entity.lineage" {
        return Err(CapabilityError::Invalid(
            "Native entity identity must be a lineage tag, not a name or generation-1 numeric selector.",
        ));
    }
    LineageTag::new(&child.id)
        .map(|id| Some(MemberId::<EntityType>::lineage(id)))
        .map_err(|_| CapabilityError::Invalid("Invalid native lineage identity."))
}

fn request(
    verb: OntologyVerb,
    id: Option<MemberId<EntityType>>,
    body: Option<&Value>,
) -> Result<Request, CapabilityError> {
    match verb {
        OntologyVerb::Get => Ok(Request::Get(
            id.ok_or(CapabilityError::Invalid("Entity identity required."))?,
        )),
        OntologyVerb::List => Ok(Request::List),
        OntologyVerb::Delete => Ok(Request::Delete(
            id.ok_or(CapabilityError::Invalid("Entity identity required."))?,
        )),
        OntologyVerb::Create => {
            let body = body.ok_or(CapabilityError::Invalid(
                "Complete native entity input required.",
            ))?;
            EntityType::check(body, false)
                .map_err(|_| CapabilityError::Invalid("Invalid complete native entity input."))?;
            serde_json::from_value(body.clone())
                .map(Request::Create)
                .map_err(|_| CapabilityError::Invalid("Invalid native entity field."))
        }
        OntologyVerb::Update | OntologyVerb::Link | OntologyVerb::Move => {
            let body = body.ok_or(CapabilityError::Invalid(
                "Native merge patch input required.",
            ))?;
            let object = body.as_object().filter(|object| !object.is_empty()).ok_or(
                CapabilityError::Invalid("Native patch must be a nonempty object."),
            )?;
            if verb == OntologyVerb::Move
                && (object.len() != 1
                    || !object.get("name").is_some_and(|value| {
                        value.as_str().is_some_and(|name| !name.trim().is_empty())
                    }))
            {
                return Err(CapabilityError::Invalid(
                    "Move accepts exactly the native name field.",
                ));
            }
            if verb == OntologyVerb::Link
                && (object
                    .keys()
                    .any(|key| !matches!(key.as_str(), "backingTable" | "additionalBackingTables"))
                    || !object.get("backingTable").is_some_and(|value| {
                        value.as_str().is_some_and(|name| !name.trim().is_empty())
                    }))
            {
                return Err(CapabilityError::Invalid(
                    "Link requires backingTable and optional additionalBackingTables only.",
                ));
            }
            let patch = MergePatch::<EntityType>::new(body.clone())
                .map_err(|_| CapabilityError::Invalid("Invalid native entity merge patch."))?;
            Ok(Request::Patch(
                id.ok_or(CapabilityError::Invalid("Entity identity required."))?,
                patch,
            ))
        }
    }
}

/// Execute one typed generation-2 entity operation with injected native policy.
///
/// Create is strict native collection creation. Update is JSON Merge Patch:
/// omitted/null/arrays retain native semantics. Link is a backingTable-only
/// binding patch (plus optional additionalBackingTables); Move is a name-only
/// patch of one stable lineage identity, never transfer or copy/delete. Delete
/// requires explicit permanent intent and host approval of all affected refs.
///
/// Lists inspect one bounded native page, not an unbounded traversal. Remaining
/// pages or locally cut records produce truthful Partial coverage; a cut service
/// page has no fabricated resume token. Accepted private work without a supported
/// scoped observation handle is Unknown with the original response retained.
///
/// # Errors
/// Rejects unsupported options/gates/input/identity before credential or native
/// execution. Approval and no-clobber reservation precede dispatch. Output failure
/// preserves the original CapabilityReport; uncertain work is never replayed.
pub async fn execute_native<H: NativeHost>(
    args: &OntologyArguments,
    facade: bool,
    registry: &Registry,
    host: &mut H,
    limits: AdapterLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CapabilityReport, CapabilityError> {
    support::check_options(args)?;
    let resolved = host.resolve(&args.path).map_err(CapabilityError::Host)?;
    let (expected, representation) = selection(args, &resolved)?;
    let selected = resolved.selection();
    validate_arguments(
        args,
        representation,
        &selected.owner_type,
        selected.generation.map(u64::from),
    )
    .map_err(CapabilityError::Validation)?;
    // Resolve before consuming stdin or consulting a private-host binding.
    let resolution = registry
        .resolve(
            support::verb(args.verb),
            selected,
            facade.then_some("ontology"),
        )
        .map_err(CapabilityError::Contract)?;
    if resolution.operation.id() != expected {
        return Err(CapabilityError::Invalid(
            "Registered native operation identity mismatch.",
        ));
    }
    let id = member(&resolved, representation == Representation::Entities)?;
    let workspace = resolved
        .identity()
        .workspace_id()
        .ok_or(CapabilityError::Invalid("Native owner workspace required."))?;
    let body = support::input(args, host, limits.input_bytes)?;
    let request = request(args.verb, id, body.as_ref())?;
    let context = host.native_context().map_err(CapabilityError::Host)?;
    let target = OntologyTarget::new(
        WorkloadIdentity::digital_operations(),
        workspace,
        resolved
            .identity()
            .item_id()
            .ok_or(CapabilityError::Invalid("Native owner item required."))?,
        &selected.owner_type,
        selected.generation.unwrap_or(0),
        context.features,
    )
    .map_err(|_| CapabilityError::Invalid("Native owner/generation/gates are unavailable."))?;
    let client = OntologyClient::new(
        target,
        context.executor.as_ref(),
        ByteLimits::new(
            limits.input_bytes.get(),
            limits.operation.max_response_bytes.get(),
        )
        .map_err(|_| CapabilityError::Invalid("Native request/response bounds must be finite."))?,
    )
    .map_err(|_| CapabilityError::Invalid("Invalid native host binding."))?;
    // Run native DTO/gate/route preflight through the SDK, without an executor call.
    match &request {
        Request::Get(id) => client.get_request(id, false),
        Request::List => {
            client.page_request::<EntityType>(args.continuation.as_deref(), false, &[])
        }
        Request::Create(body) => client.create_request(body),
        Request::Patch(id, patch) => client.patch_request(id, patch),
        Request::Delete(id) => client.delete_request(id),
    }
    .map_err(|_| CapabilityError::Invalid("Native SDK request preflight failed."))?;
    let mutation = !matches!(args.verb, OntologyVerb::Get | OntologyVerb::List);
    let effects = if args.verb == OntologyVerb::Delete {
        vec![Effect::RemoteWrite, Effect::RemoteDelete]
    } else if mutation {
        vec![Effect::RemoteWrite]
    } else {
        vec![Effect::RemoteRead, Effect::MeteredConsumption]
    };
    let mut conditions = if args.verb == OntologyVerb::Create {
        vec![Precondition::CreateOnly]
    } else if mutation {
        vec![
            Precondition::ExistingOnly,
            Precondition::UnguardedConcurrency,
        ]
    } else {
        vec![]
    };
    if args.permanent {
        conditions.push(Precondition::PermanentDeletion);
    }
    let prepared = support::prepare(
        args,
        facade,
        registry,
        resolved,
        expected,
        json!({
            "sourceClass":"privateWorkload",
            "workload":"DigitalOperations", "body":body,
            "context":{"account":context.binding.account,"tenant":context.binding.tenant,
                "audience":context.binding.audience,"nativeBinding":context.binding.binding_id},
            "features":{"tmsl":context.features.tmsl, "inheritance":context.features.inheritance,
                "reusableProperties":context.features.reusable_properties, "autoProjection":context.features.auto_projection},
            "continuation":args.continuation, "limit":args.limit.map(|value| value.get()),
            "bounds":{"maxInputBytes":limits.input_bytes.get(), "maxResponseBytes":limits.operation.max_response_bytes.get(),
                "maxDataBytes":args.max_bytes.unwrap_or(limits.data_bytes).get()},
        }),
        &effects,
        conditions,
    )?;
    host.deadline(args.timeout).map_err(CapabilityError::Host)?;
    let writer = host
        .reserve_output(&args.output)
        .map_err(CapabilityError::Host)?;
    let approved = prepared.authorize(host).map_err(CapabilityError::Host)?;
    let dispatch = async {
        match request {
            Request::Get(id) => client
                .get(&id, false)
                .await
                .map(|result| map_native(result, |value| value.raw, false)),
            Request::Create(body) => client
                .create(&body)
                .await
                .map(|result| map_native(result, |value| value.raw, true)),
            Request::Patch(id, patch) => client
                .patch(&id, &patch)
                .await
                .map(|result| map_native(result, |value| value.raw, true)),
            Request::Delete(id) => client
                .delete(&id)
                .await
                .map(|result| map_native(result, |()| Value::Null, true)),
            Request::List => client
                .page::<EntityType>(args.continuation.as_deref(), false, &[])
                .await
                .map(|result| map_page(result, args.limit.map(|value| value.get()))),
        }
    };
    let result = observe(dispatch, cancelled).await;
    let (outcome, evidence, cause) = match result {
        Ok(result) => result,
        Err(error) => failure(error, mutation),
    };
    let report = CapabilityReport {
        receipt: ExecutionReceipt::new(&approved, outcome, evidence)
            .map_err(CapabilityError::Receipt)?,
        cause,
        warnings: vec![],
        unpresented_data: None,
    };
    support::present(
        host,
        writer,
        report,
        &approved,
        args.max_bytes.unwrap_or(limits.data_bytes),
    )
}

async fn observe<T>(
    request: impl Future<Output = Result<T, WorkloadError>>,
    cancelled: impl Future<Output = ()>,
) -> Result<T, WorkloadError> {
    let mut request = pin!(request);
    let mut cancelled = pin!(cancelled);
    let mut polled = false;
    poll_fn(|context| {
        if cancelled.as_mut().poll(context).is_ready() {
            return Poll::Ready(Err(WorkloadError::execution(
                if polled { DispatchState::Unknown } else { DispatchState::NotSent },
                "Local observation cancelled or deadline elapsed; no remote cancellation or replay was requested.".into(),
            )));
        }
        polled = true;
        request.as_mut().poll(context)
    }).await
}

type NativeResult = (Outcome<Value>, ExecutionEvidence, Option<CapabilityCause>);

fn info(code: &str, message: &str) -> FailureInfo {
    FailureInfo {
        code: code.into(),
        message: message.into(),
    }
}

fn map_native<T>(
    result: NativeOutcome<T>,
    map: impl FnOnce(T) -> Value,
    mutation: bool,
) -> NativeResult {
    match result {
        NativeOutcome::Completed { value, response } => (
            Outcome::Succeeded { data: map(value) },
            ExecutionEvidence {
                http_status: Some(response.status),
                handles: vec![],
                coverage: Coverage::Complete,
                mutation_may_have_occurred: mutation,
            },
            None,
        ),
        NativeOutcome::Accepted { response } => accepted(response, mutation),
    }
}

fn map_page(result: NativeOutcome<NativePage<EntityType>>, maximum: Option<usize>) -> NativeResult {
    match result {
        NativeOutcome::Completed {
            value: page,
            response,
        } => {
            let mut data: Vec<_> = page.value.into_iter().map(|value| value.raw).collect();
            let cut = maximum.is_some_and(|maximum| data.len() > maximum);
            if let Some(maximum) = maximum {
                data.truncate(maximum);
            }
            let incomplete = cut || page.continuation_token.is_some();
            let coverage = if incomplete {
                Coverage::Incomplete {
                    reasons: vec![if cut {
                        "Page exceeds requested record limit; cut page is nonresumable.".into()
                    } else {
                        "Only one bounded native page was observed; native continuation remains."
                            .into()
                    }],
                    continuation: if cut { None } else { page.continuation_token },
                }
            } else {
                Coverage::Complete
            };
            let outcome = if incomplete {
                Outcome::Partial {
                    data: Value::Array(data),
                    error: info("PartialResult", "Only a bounded native page was observed."),
                }
            } else {
                Outcome::Succeeded {
                    data: Value::Array(data),
                }
            };
            (
                outcome,
                ExecutionEvidence {
                    http_status: Some(response.status),
                    handles: vec![],
                    coverage,
                    mutation_may_have_occurred: false,
                },
                None,
            )
        }
        NativeOutcome::Accepted { response } => accepted(response, false),
    }
}

fn accepted(response: WorkloadResponse, mutation: bool) -> NativeResult {
    // Private Location is not a public Fabric operation contract. Keep its exact
    // response instead of minting a cross-transport observation handle.
    let error = WorkloadError::with_response(
        WorkloadErrorKind::CapabilityGap("native accepted observation handle is not advertised"),
        response.clone(),
        DispatchState::Accepted,
    );
    (
        Outcome::Unknown {
            error: info(
                "OutcomeUnknown",
                "Native work was accepted without a supported scoped observation handle.",
            ),
        },
        ExecutionEvidence {
            http_status: Some(response.status),
            handles: vec![],
            coverage: Coverage::Unknown,
            mutation_may_have_occurred: mutation,
        },
        Some(CapabilityCause::Native(Box::new(error))),
    )
}

fn failure(error: WorkloadError, mutation: bool) -> NativeResult {
    let possible_mutation = mutation && error.dispatch != DispatchState::NotSent;
    let uncertain = mutation
        && match error.dispatch {
            DispatchState::NotSent => false,
            DispatchState::Accepted | DispatchState::Unknown => true,
            DispatchState::Completed => !matches!(error.kind, WorkloadErrorKind::Http),
        };
    let code = match error.kind {
        WorkloadErrorKind::Limit => "BudgetExceeded",
        WorkloadErrorKind::CapabilityGap(_) => "UnsupportedCapability",
        WorkloadErrorKind::InvalidInput(_) => "InvalidInput",
        WorkloadErrorKind::Http => match error.response.as_ref().map(|response| response.status) {
            Some(401) => "Unauthenticated",
            Some(403) => "AccessDenied",
            Some(404) => "NotFound",
            Some(409) => "AlreadyExists",
            _ => "OperationFailed",
        },
        _ => "OperationFailed",
    };
    let info = info(
        code,
        "Native operation failed; original dispatch/response evidence is retained.",
    );
    let evidence = ExecutionEvidence {
        http_status: error.response.as_ref().map(|response| response.status),
        handles: vec![],
        coverage: Coverage::Unknown,
        mutation_may_have_occurred: possible_mutation,
    };
    (
        if uncertain {
            Outcome::Unknown { error: info }
        } else {
            Outcome::Failed { error: info }
        },
        evidence,
        Some(CapabilityCause::Native(Box::new(error))),
    )
}
