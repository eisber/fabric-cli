// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Metadata mutations through existing ItemMutations and shared operation execution.
//!
//! Create input is exactly `displayName: string` and optional string
//! `description`; no type/generation/definition/creationPayload is synthesized.
//! Patch permits only supplied displayName/description strings, preserving
//! omission and rejecting explicit null instead of converting it to omission.
//! Initial definition authoring remains separately owned. The resolver/policy
//! verifies destination intent; no item ID or resulting generation is fabricated.

use crate::{
    AdapterHost, AdapterLimits, OntologyArguments, OntologyTarget, OntologyVerb,
    execution_support::{self as support, CapabilityCause, CapabilityError, CapabilityReport},
    validate_arguments,
};
use fabric_sdk_items::{ItemClient, ItemCollection, mutations::ItemMutations};
use fabric_sdk_ontology::v2::{
    Availability, Binding, ContractError, Coverage, Effect, ExecutionEvidence, ExecutionReceipt,
    FailureInfo, Generation, ItemIdentity, OperationHandle, Outcome, Precondition, Prerequisite,
    Registry, TargetKind, Verb,
};
use fabric_sdk_operations::{OperationErrorKind, OperationExecutor, OperationSession};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::future::Future;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MetadataCreate {
    display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

fn metadata_body(
    verb: OntologyVerb,
    body: Option<Value>,
) -> Result<Option<Value>, CapabilityError> {
    if verb == OntologyVerb::Delete {
        return Ok(None);
    }
    let body = body.ok_or(CapabilityError::Invalid(
        "Metadata mutation requires explicit input.",
    ))?;
    let object = body.as_object().ok_or(CapabilityError::Invalid(
        "Metadata input must be an object.",
    ))?;
    if object.is_empty()
        || object
            .keys()
            .any(|key| key != "displayName" && key != "description")
        || object.get("displayName").is_some_and(|value| {
            !value
                .as_str()
                .is_some_and(|name| !name.trim().is_empty() && !name.chars().any(char::is_control))
        })
        || object
            .get("description")
            .is_some_and(|value| !value.is_string())
    {
        return Err(CapabilityError::Invalid(
            "Metadata input permits only nonempty displayName and string description; no definition, generation or read-only fields.",
        ));
    }
    if verb == OntologyVerb::Create {
        let typed: MetadataCreate = serde_json::from_value(body).map_err(|_| {
            CapabilityError::Invalid("Creation requires displayName; description is optional.")
        })?;
        return serde_json::to_value(typed)
            .map(Some)
            .map_err(|_| CapabilityError::Invalid("Metadata serialization failed."));
    }
    Ok(Some(body))
}

fn valid_metadata(value: &Value, workspace: &str, expected_item: Option<&str>) -> bool {
    let Some(id) = value.get("id").and_then(Value::as_str) else {
        return false;
    };
    ItemIdentity::new(workspace, id).is_ok()
        && expected_item.is_none_or(|expected| id.eq_ignore_ascii_case(expected))
        && value.get("type").and_then(Value::as_str) == Some("Ontology")
        && value.get("displayName").is_some_and(Value::is_string)
        && value.get("workspaceId").is_none_or(|value| {
            value
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(workspace))
        })
        && match value.get("properties") {
            None | Some(Value::Null) => true,
            Some(Value::Object(properties)) => properties
                .get("generation")
                .is_none_or(|generation| generation.is_null() || generation.as_u64().is_some()),
            Some(_) => false,
        }
}

/// Bind caller-registered metadata operations without registering another catalog.
///
/// The caller must already have installed the `ontology` owner alias (normally
/// through `bind_public_operations`). Every binding remains host-conditional.
/// Required IDs are `item.metadata.create`, `item.metadata.patch` and
/// `item.delete`. Descriptors require RemoteWrite or RemoteDelete, and must
/// declare selectable LocalRead/LocalWrite effects for input/file choices.
///
/// # Errors
/// Rejects absent/mismatched descriptors or overlapping bindings transactionally.
pub fn bind_metadata_operations(registry: &mut Registry) -> Result<(), ContractError> {
    let mut candidate = registry.clone();
    for (id, verb, kind) in [
        (
            "item.metadata.create",
            Verb::Create,
            TargetKind::WorkspaceCollection,
        ),
        ("item.metadata.patch", Verb::Update, TargetKind::Item),
        ("item.delete", Verb::Delete, TargetKind::Item),
    ] {
        if !candidate
            .operations()
            .any(|operation| operation.id() == id && operation.verb() == verb)
        {
            return Err(ContractError::UnknownOperation);
        }
        candidate.bind(Binding::new(
            "Ontology", "metadata", "json", Generation::Any, id,
            Availability::Conditional(vec![Prerequisite {
                id: "ontology.public.mutation.host".into(),
                description: "Caller verifies public item creation/update/delete readiness, exact destination intent, scopes and effects; no metadata/source fallback.".into(),
            }]),
        )?.with_target_kind(kind))?;
    }
    *registry = candidate;
    Ok(())
}

/// Execute metadata Create/Patch/Delete with exact caller approval and receipts.
///
/// Creation requires a real `WorkspaceCollection` target, with no synthetic item
/// ID. Metadata-only creation does not select or claim a resulting generation.
/// Create-only is fresh-resource intent, not an atomic display-name reservation;
/// policy must refuse requirements for an unavailable namespace compare-and-swap.
/// Patch never changes source. Delete uses the existing generic items route and
/// exact `--permanent` intent, never a native definition reset or hard fallback.
///
/// # Errors
/// Retains typed shared-engine failures and output evidence without replay.
#[allow(clippy::too_many_arguments)]
pub async fn execute_metadata<E: OperationExecutor, H: AdapterHost>(
    args: &OntologyArguments,
    facade: bool,
    registry: &Registry,
    host: &mut H,
    client: &ItemClient,
    executor: E,
    limits: AdapterLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CapabilityReport, CapabilityError> {
    support::check_options(args)?;
    let id = match args.verb {
        OntologyVerb::Create => "item.metadata.create",
        OntologyVerb::Update => "item.metadata.patch",
        OntologyVerb::Delete => "item.delete",
        _ => {
            return Err(CapabilityError::Invalid(
                "Metadata mutations support Create, Patch and Delete only.",
            ));
        }
    };
    let resolved = host.resolve(&args.path).map_err(CapabilityError::Host)?;
    let selection = resolved.selection();
    let expected_kind = if args.verb == OntologyVerb::Create {
        TargetKind::WorkspaceCollection
    } else {
        TargetKind::Item
    };
    if selection.kind != expected_kind
        || resolved.child().is_some()
        || selection.facet != "metadata"
        || selection.representation != "json"
    {
        return Err(CapabilityError::Invalid(
            "Metadata mutation requires the exact owner or real workspace destination collection.",
        ));
    }
    validate_arguments(
        args,
        OntologyTarget::Item,
        &selection.owner_type,
        selection.generation.map(u64::from),
    )
    .map_err(CapabilityError::Validation)?;
    let workspace = resolved
        .identity()
        .workspace_id()
        .ok_or(CapabilityError::Invalid("Workspace identity required."))?
        .to_owned();
    let body = metadata_body(args.verb, support::input(args, host, limits.input_bytes)?)?;
    let item = resolved.identity().item_id().map(str::to_owned);
    let mut preconditions = if args.verb == OntologyVerb::Create {
        vec![Precondition::CreateOnly]
    } else {
        vec![
            Precondition::ExistingOnly,
            Precondition::UnguardedConcurrency,
        ]
    };
    if args.permanent {
        preconditions.push(Precondition::PermanentDeletion);
    }
    let effects = if args.verb == OntologyVerb::Delete {
        vec![Effect::RemoteDelete]
    } else {
        vec![Effect::RemoteWrite]
    };
    let prepared = support::prepare(
        args,
        facade,
        registry,
        resolved,
        id,
        json!({
            "sourceClass":"publicFabric",
            "body":body,
            "permanent":args.permanent,
            "context":{
                "account":client.selection().account(), "tenant":client.selection().tenant(),
                "audience":client.selection().audience().scope(), "endpoint":client.context().endpoint().host(),
            },
            "bounds":{
                "maxInputBytes":limits.input_bytes.get(), "maxDataBytes":limits.data_bytes.get(),
                "maxRequests":limits.operation.max_requests.get(),
                "maxResponseBytes":limits.operation.max_response_bytes.get(),
                "maxPollDelayNanos":limits.operation.max_poll_delay.as_nanos().to_string(),
            },
        }),
        &effects,
        preconditions,
    )?;
    host.deadline(args.timeout).map_err(CapabilityError::Host)?;
    let writer = host
        .reserve_output(&args.output)
        .map_err(CapabilityError::Host)?;
    let approved = prepared.authorize(host).map_err(CapabilityError::Host)?;
    let mut session =
        OperationSession::with_context(client.context(), executor, limits.operation, cancelled);
    let completion = if !args.wait {
        CompletionPolicy::ReturnAccepted
    } else if args.verb == OntologyVerb::Delete {
        CompletionPolicy::WaitForCompletion
    } else {
        CompletionPolicy::WaitForResult
    };
    let result = match args.verb {
        OntologyVerb::Create => {
            ItemMutations::create_with_completion(
                &mut session,
                &workspace,
                ItemCollection::Ontology,
                body.ok_or(CapabilityError::Invalid("Creation body required."))?,
                completion,
            )
            .await
        }
        OntologyVerb::Update => {
            ItemMutations::update_with_completion(
                &mut session,
                &workspace,
                ItemCollection::Ontology,
                item.as_deref()
                    .ok_or(CapabilityError::Invalid("Existing item required."))?,
                body.ok_or(CapabilityError::Invalid("Patch body required."))?,
                completion,
            )
            .await
        }
        OntologyVerb::Delete => {
            ItemMutations::delete_with_completion(
                &mut session,
                &workspace,
                item.as_deref()
                    .ok_or(CapabilityError::Invalid("Existing item required."))?,
                args.permanent,
                completion,
            )
            .await
        }
        _ => unreachable!("verb checked before preparation"),
    };
    let handle = |id: Option<String>| {
        id.into_iter()
            .map(|id| OperationHandle::FabricOperation { id })
            .collect()
    };
    let mut unpresented_data = None;
    let (outcome, evidence, cause) = match result {
        Ok(ResourceOutcome::Completed { status, data }) => {
            let data = data.unwrap_or(Value::Null);
            let expected = if args.verb == OntologyVerb::Create {
                None
            } else {
                item.as_deref()
            };
            let outcome = if args.verb != OntologyVerb::Delete
                && !valid_metadata(&data, &workspace, expected)
            {
                unpresented_data = Some(data);
                Outcome::Unknown { error: FailureInfo {
                    code:"InvalidResponse".into(),
                    message:"Mutation response did not establish the addressed Ontology metadata; the original returned data is retained without replay.".into(),
                }}
            } else {
                Outcome::Succeeded { data }
            };
            let coverage = if matches!(&outcome, Outcome::Succeeded { .. }) {
                Coverage::Complete
            } else {
                Coverage::Unknown
            };
            (
                outcome,
                ExecutionEvidence {
                    http_status: Some(status),
                    handles: handle(session.operation_id().map(str::to_owned)),
                    coverage,
                    mutation_may_have_occurred: session
                        .evidence(OperationErrorKind::InvalidResponse)
                        .mutation_may_have_occurred,
                },
                None,
            )
        }
        Ok(ResourceOutcome::Accepted { operation_id }) => {
            let handles: Vec<_> = handle(operation_id);
            let outcome = if handles.is_empty() {
                Outcome::Unknown {
                    error: FailureInfo {
                        code: "AcceptedUnobservable".into(),
                        message: "Provider accepted work without a usable observation handle."
                            .into(),
                    },
                }
            } else {
                Outcome::Accepted
            };
            (
                outcome,
                ExecutionEvidence {
                    http_status: Some(202),
                    handles,
                    coverage: Coverage::Unknown,
                    mutation_may_have_occurred: true,
                },
                None,
            )
        }
        Err(error) => {
            // Only a provider-terminal response carries raw error evidence,
            // including Some(Null) when it has no error body; local cancellation does not.
            let terminal = error.code == "LongRunningOperationFailed"
                || (error.code == "LongRunningOperationCancelled"
                    && error.raw_service_error.is_some());
            let uncertain = !terminal
                && (error.operation_id.is_some()
                    || (error.mutation_may_have_occurred && error.status.is_none()));
            let info = FailureInfo {
                code: if uncertain {
                    "CompletionUnknown"
                } else {
                    "MetadataFailed"
                }
                .into(),
                message: "Metadata request failed; original typed SDK evidence is retained.".into(),
            };
            let outcome = if uncertain {
                Outcome::Unknown { error: info }
            } else {
                Outcome::Failed { error: info }
            };
            let evidence = ExecutionEvidence {
                http_status: error.status,
                handles: handle(error.operation_id.clone()),
                coverage: Coverage::Unknown,
                mutation_may_have_occurred: error.mutation_may_have_occurred,
            };
            (
                outcome,
                evidence,
                Some(CapabilityCause::Metadata(Box::new(error))),
            )
        }
    };
    let report = CapabilityReport {
        receipt: ExecutionReceipt::new(&approved, outcome, evidence)
            .map_err(CapabilityError::Receipt)?,
        cause,
        warnings: session.warnings().to_vec(),
        unpresented_data,
    };
    support::present(host, writer, report, &approved, limits.data_bytes)
}
