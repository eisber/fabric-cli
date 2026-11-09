// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicitly authorized, nonrecursive deletion of one exact Notebook.
//!
//! No names, lists, external targets, credentials, HTTP policy or polling are
//! resolved here. Permanent means an explicit `hardDelete=true` request, not a
//! claim of physical erasure. Recovery remains unknown and service-defined.
//! The [item deletion permissions](https://learn.microsoft.com/rest/api/fabric/core/items/delete-item#permissions)
//! require workspace Admin for hard deletion. The Notebook-specific endpoint
//! enforces the target type; a caller's type suffix alone is not trusted.
//! Its contract requires Notebook write permission and `Notebook.ReadWrite.All`
//! or `Item.ReadWrite.All`. The conservative hard-delete Admin requirement is
//! retained separately; the service determines actual permission and any denial
//! is preserved. Identity and scopes remain the injected caller's responsibility.
//! Only terminal HTTP 200 is documented; acceptance handling is defensive, not
//! a claim that Notebook deletion supports long-running operations.
//! Existing v1 mutation APIs and their compatibility contracts are unchanged.

pub use fabric_core::operation::{
    ApprovalRequirement, CapabilitySupport, CompletionEvidence, DeletionMode, DispatchEvidence,
    EffectEvidence, ObservationMode, ObservationSupport, OperationContext, OperationDescriptor,
    OperationEffect, OperationEffectKind, OperationEvidence, OperationMetadata,
    OperationPrecondition, OperationProvenance, OperationResourceType, OperationStatus,
    OperationTarget, RecoverySemantics, RequiredWorkspaceRole, TargetTypeEnforcement,
    TraversalMode,
};
use fabric_core::{Audience, CredentialSelection, EndpointBinding, ServiceOrigin};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde::Serialize;
use std::{
    fmt,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Static bounded Notebook deletion contract, not an invocation authorization.
pub const NOTEBOOK_DELETE_DESCRIPTOR: OperationDescriptor = OperationDescriptor {
    id: "fabric.items.notebook.delete.v2",
    resource_type: OperationResourceType::Notebook,
    target_type_enforcement: TargetTypeEnforcement::ServiceEndpoint,
    effects: &[
        OperationEffect {
            id: "notebook-lifecycle",
            kind: OperationEffectKind::NotebookLifecycle,
        },
        OperationEffect {
            id: "notebook-content",
            kind: OperationEffectKind::NotebookContent,
        },
    ],
    recursive_support: CapabilitySupport::Unsupported,
    conditional_support: CapabilitySupport::Unsupported,
    observation_support: ObservationSupport::TerminalResponseOnly,
    recovery: RecoverySemantics::UnknownServiceDefined,
    approval: ApprovalRequirement::ExplicitHostAuthorization,
    required_workspace_role: RequiredWorkspaceRole::Admin,
    provenance: OperationProvenance {
        service_contract: "https://learn.microsoft.com/rest/api/fabric/notebook/items/delete-notebook",
        source_revision: "5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b",
    },
};

static NEXT_INVOCATION: AtomicU64 = AtomicU64::new(1);

/// An exact, validated Notebook identity with no public unchecked constructor.
///
/// The type is selected by the caller and enforced by the Notebook-specific
/// service endpoint, not inferred from a name or trusted without enforcement.
/// GUIDs are canonicalized to lowercase. There is no deserialization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DeleteTarget(OperationTarget);

impl DeleteTarget {
    /// Validate two hyphenated GUIDs and select exactly one Notebook.
    ///
    /// # Errors
    /// Returns `InvalidInput` without echoing invalid identifiers.
    pub fn notebook(workspace: &str, item: &str) -> Result<Self, DeleteFailure> {
        if !is_guid(workspace) || !is_guid(item) {
            return Err(DeleteFailure::local(
                "InvalidInput",
                "Notebook deletion requires exact workspace and item GUIDs",
                None,
            ));
        }
        Ok(Self(OperationTarget {
            workspace_id: workspace.to_ascii_lowercase(),
            item_id: item.to_ascii_lowercase(),
            resource_type: OperationResourceType::Notebook,
        }))
    }

    /// Return the canonical workspace GUID.
    pub fn workspace_id(&self) -> &str {
        &self.0.workspace_id
    }

    /// Return the canonical item GUID.
    pub fn item_id(&self) -> &str {
        &self.0.item_id
    }

    /// Return the explicit caller-selected type.
    pub const fn resource_type(&self) -> OperationResourceType {
        self.0.resource_type
    }
}

fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Explicit deletion requirements; the default cannot authorize permanent loss.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct DeleteOptions {
    /// Request hard deletion explicitly; requires workspace Admin and is not a
    /// physical-erasure guarantee.
    pub permanent: bool,
    /// Request recursive traversal; unsupported and rejected before dispatch.
    pub recursive: bool,
    /// Request an If-Match precondition; unsupported and rejected, never dropped.
    pub if_match: Option<String>,
    /// Reconcile completion through the shared executor when advertised.
    pub wait: bool,
}

/// An immutable, single-use assessment created only by [`assess`].
///
/// Accessors return shared references. Neither public metadata construction nor
/// deserialization can forge an executable assessment. The internal execution
/// guard is consumed before awaiting the executor, even if its future is dropped.
/// Serialization includes the immutable target, options and metadata, not that
/// guard; no deserialization or unchecked assessment constructor is provided.
#[derive(Debug, Serialize)]
pub struct DeleteAssessment {
    target: DeleteTarget,
    options: DeleteOptions,
    metadata: OperationMetadata,
    #[serde(skip)]
    executed: AtomicBool,
}

impl DeleteAssessment {
    /// Bind one explicit nonsecret account, tenant and Fabric endpoint context.
    ///
    /// Binding consumes the unbound assessment without changing its invocation
    /// identity. Approvals made before binding no longer match. No credentials
    /// are acquired, stored, or inspected. The host must configure the executor
    /// from this same actual request context; an opaque `ResourceOperations`
    /// implementation cannot be checked by this method.
    ///
    /// # Errors
    /// Rejects an already consumed assessment, any rebinding, a non-Fabric
    /// selection audience, or a non-Fabric endpoint service identity. Failure
    /// metadata retains the original assessment, never the rejected context.
    pub fn with_selection(
        mut self,
        selection: &CredentialSelection,
        endpoint: &EndpointBinding,
    ) -> Result<Self, DeleteFailure> {
        if self.executed.load(Ordering::Acquire) {
            return Err(DeleteFailure::local(
                "AlreadyExecuted",
                "A consumed Notebook deletion assessment cannot be context-bound",
                Some(self.metadata),
            ));
        }
        if self.metadata.context.is_some() {
            return Err(DeleteFailure::local(
                "ContextAlreadyBound",
                "Notebook deletion execution context is already bound",
                Some(self.metadata),
            ));
        }
        if selection.audience() != Audience::Fabric || endpoint.origin() != ServiceOrigin::Fabric {
            return Err(DeleteFailure::local(
                "InvalidContext",
                "Notebook deletion requires an explicit Fabric execution context",
                Some(self.metadata),
            ));
        }
        self.metadata.context = Some(OperationContext {
            account: selection.account().to_owned(),
            tenant: selection.tenant().to_owned(),
            endpoint: endpoint.base_url().to_string(),
        });
        Ok(self)
    }

    /// Return the exact immutable target.
    pub fn target(&self) -> &DeleteTarget {
        &self.target
    }

    /// Return the exact immutable assessed options.
    pub fn options(&self) -> &DeleteOptions {
        &self.options
    }

    /// Return the full resolved metadata, unchanged by execution.
    pub fn metadata(&self) -> &OperationMetadata {
        &self.metadata
    }

    /// Return the static descriptor separately from invocation identity.
    pub fn descriptor(&self) -> &OperationDescriptor {
        &self.metadata.descriptor
    }
}

/// Trusted-host authorization bound to one exact immutable assessment.
///
/// This represents a host's explicit decision, not a Fabric permission, service
/// authorization, or permission for an agent to approve its own proposed action.
/// Hosts must apply their own human/policy authorization before calling `approve`.
#[derive(Debug)]
pub struct DeleteApproval {
    approved: OperationMetadata,
}

impl DeleteApproval {
    /// Capture the full assessed target, mode, effects and invocation identity.
    ///
    /// This does no I/O and does not check or grant service permissions.
    pub fn approve(assessment: &DeleteAssessment) -> Self {
        Self {
            approved: assessment.metadata.clone(),
        }
    }
}

/// Assess one exact Notebook without performing any I/O or service discovery.
///
/// Recovery is not guaranteed by the native API, so the default recoverable
/// request is unsupported. Conditional and recursive requests are never silently
/// weakened. Metadata on rejected options records what was requested.
/// Context initially remains absent for caller-controlled offline/custom SDK
/// hosts. Hosts are responsible for their injected executor and must use
/// [`DeleteAssessment::with_selection`] before approval when binding identity and
/// endpoint context is required; no ambient context is discovered here.
///
/// # Errors
/// Returns `UnsupportedCapability` for recoverable, recursive or conditional
/// requirements, and `InvocationLimit` if the process-local sequence is exhausted.
pub fn assess(
    target: DeleteTarget,
    options: DeleteOptions,
) -> Result<DeleteAssessment, DeleteFailure> {
    let invocation_id = NEXT_INVOCATION
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| {
            DeleteFailure::local(
                "InvocationLimit",
                "The local deletion assessment sequence is exhausted",
                None,
            )
        })?;
    let metadata = OperationMetadata {
        descriptor: NOTEBOOK_DELETE_DESCRIPTOR,
        invocation_id,
        context: None,
        target: target.0.clone(),
        deletion_mode: if options.permanent {
            DeletionMode::Permanent
        } else {
            DeletionMode::Recoverable
        },
        traversal: if options.recursive {
            TraversalMode::RecursiveRequested
        } else {
            TraversalMode::ExactTarget
        },
        precondition: if options.if_match.is_some() {
            OperationPrecondition::IfMatchRequested
        } else {
            OperationPrecondition::Unconditional
        },
        observation: if options.wait {
            ObservationMode::WaitForCompletion
        } else {
            ObservationMode::ReturnAccepted
        },
    };
    let unsupported = if !options.permanent {
        Some("Recoverable Notebook deletion is not guaranteed; explicit permanence is required")
    } else if options.recursive {
        Some("Recursive Notebook deletion is unsupported")
    } else if options.if_match.is_some() {
        Some("Conditional Notebook deletion is unsupported")
    } else {
        None
    };
    if let Some(message) = unsupported {
        return Err(DeleteFailure::local(
            "UnsupportedCapability",
            message,
            Some(metadata),
        ));
    }
    Ok(DeleteAssessment {
        target,
        options,
        metadata,
        executed: AtomicBool::new(false),
    })
}

/// A successful terminal response or explicit acceptance, with actual evidence.
///
/// `Accepted` must not be presented as completed. Even a terminal response proves
/// service-level completion only, not physical erasure or observed storage state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeleteReceipt {
    /// The complete unchanged assessed request.
    pub metadata: OperationMetadata,
    /// The actual response and effect evidence.
    pub evidence: OperationEvidence,
}

impl DeleteReceipt {
    /// Return the unchanged assessment metadata.
    pub fn metadata(&self) -> &OperationMetadata {
        &self.metadata
    }

    /// Return actual success or acceptance, never inferred completion.
    pub const fn status(&self) -> OperationStatus {
        self.evidence.status
    }
}

/// Sanitized deletion failure retaining assessment and reconciliation evidence.
///
/// The underlying resource error is available explicitly, but never serialized
/// or included in display diagnostics: provider text is not trusted output.
#[derive(Debug, Serialize)]
pub struct DeleteFailure {
    code: &'static str,
    message: &'static str,
    metadata: Option<Box<OperationMetadata>>,
    evidence: OperationEvidence,
    #[serde(skip)]
    resource_error: Option<Box<ResourceError>>,
}

impl DeleteFailure {
    /// Return a finite, sanitized diagnostic code.
    ///
    /// `Timeout` and `Cancelled` describe why observation stopped, not rollback
    /// or mutation failure; inspect [`Self::evidence`] for the actual outcome.
    pub const fn code(&self) -> &str {
        self.code
    }

    /// Return a sanitized diagnostic with no supplied ID or provider text.
    pub const fn message(&self) -> &str {
        self.message
    }

    /// Return the unchanged assessed request, when a valid target was available.
    pub fn metadata(&self) -> Option<&OperationMetadata> {
        self.metadata.as_deref()
    }

    /// Return dispatch, failure and uncertainty evidence.
    pub fn evidence(&self) -> &OperationEvidence {
        &self.evidence
    }

    /// Return the original shared error without changing any of its fields.
    pub fn resource_error(&self) -> Option<&ResourceError> {
        self.resource_error.as_deref()
    }

    fn local(
        code: &'static str,
        message: &'static str,
        metadata: Option<OperationMetadata>,
    ) -> Self {
        let evidence = evidence(
            metadata.as_ref(),
            OperationStatus::Failed,
            None,
            None,
            DispatchEvidence::NotDispatched,
            false,
        );
        Self {
            code,
            message,
            metadata: metadata.map(Box::new),
            evidence,
            resource_error: None,
        }
    }

    fn resource(metadata: &OperationMetadata, error: ResourceError) -> Self {
        // A reconciliation denial cannot disprove an already accepted mutation.
        let status = if !error.mutation_may_have_occurred
            || (error.operation_id.is_none()
                && matches!(error.status, Some(400 | 401 | 403 | 404 | 409 | 412)))
        {
            OperationStatus::Failed
        } else {
            OperationStatus::Unknown
        };
        let (code, message) = match (error.code.as_str(), error.status) {
            ("Timeout" | "TimedOut", _) => (
                "Timeout",
                "Notebook deletion observation timed out; inspect outcome evidence before retrying",
            ),
            ("Cancelled" | "LongRunningOperationCancelled", _) => (
                "Cancelled",
                "Notebook deletion observation was cancelled; cancellation does not roll back deletion",
            ),
            (_, Some(401)) => (
                "Unauthenticated",
                "Notebook deletion or operation observation requires service authentication",
            ),
            (_, Some(403)) if status == OperationStatus::Unknown => (
                "AccessDenied",
                "Notebook deletion operation observation was denied; deletion outcome is unknown",
            ),
            (_, Some(403)) => (
                "AccessDenied",
                "Notebook deletion was denied by the service",
            ),
            (_, Some(404)) if status == OperationStatus::Unknown => (
                "NotFound",
                "Notebook deletion operation observation was not found; deletion outcome is unknown",
            ),
            (_, Some(404)) => ("NotFound", "The Notebook deletion target was not found"),
            (_, Some(412)) => (
                "PreconditionFailed",
                "A Notebook deletion or operation observation precondition was rejected",
            ),
            (_, Some(429)) => (
                "Throttled",
                "Notebook deletion was throttled by the service",
            ),
            _ if status == OperationStatus::Unknown => (
                "OutcomeUnknown",
                "Notebook deletion may have occurred; do not replay automatically",
            ),
            _ => ("ResourceOperationFailed", "Notebook deletion failed"),
        };
        Self {
            code,
            message,
            metadata: Some(Box::new(metadata.clone())),
            evidence: evidence(
                Some(metadata),
                status,
                error.operation_id.clone(),
                error.status,
                if error.status.is_some() {
                    DispatchEvidence::ResponseObserved
                } else if error.mutation_may_have_occurred {
                    DispatchEvidence::Uncertain
                } else {
                    DispatchEvidence::NotDispatched
                },
                error.mutation_may_have_occurred,
            ),
            resource_error: Some(Box::new(error)),
        }
    }
}

impl fmt::Display for DeleteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for DeleteFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.resource_error
            .as_deref()
            .map(|error| error as &dyn std::error::Error)
    }
}

/// Execute the assessed DELETE at most once through the caller's shared policy.
///
/// Missing/mismatched authorization is rejected before consuming the assessment
/// or calling the executor. The exact request is
/// `DELETE /v1/workspaces/{workspace}/notebooks/{item}?hardDelete=true`, without a
/// body. The backend enforces the Notebook type; no generic item DELETE fallback
/// is permitted. Hard deletion requires workspace Admin service permission,
/// independently of host approval. This function implements no replay,
/// cancellation, rollback, transport or polling. A dropped in-flight future
/// still consumes the assessment. Completion policy is forwarded to the shared
/// executor, but no Notebook LRO support is promised.
///
/// # Errors
/// Returns sanitized authorization/reuse errors before dispatch, or preserves
/// the original `ResourceError` and assessment after failure/uncertainty.
/// Only documented HTTP 200 terminal success and explicit acceptance yield a
/// receipt; unexpected responses never fabricate terminal success.
pub async fn execute(
    executor: &mut dyn ResourceOperations,
    assessment: &DeleteAssessment,
    approval: Option<&DeleteApproval>,
) -> Result<DeleteReceipt, DeleteFailure> {
    let Some(approval) = approval else {
        return Err(DeleteFailure::local(
            "ApprovalRequired",
            "Explicit trusted-host approval is required for Notebook deletion",
            Some(assessment.metadata.clone()),
        ));
    };
    if approval.approved != assessment.metadata {
        return Err(DeleteFailure::local(
            "ApprovalMismatch",
            "Notebook deletion approval does not match the exact assessment",
            Some(assessment.metadata.clone()),
        ));
    }
    if assessment
        .executed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(DeleteFailure::local(
            "AlreadyExecuted",
            "This Notebook deletion assessment has already been consumed",
            Some(assessment.metadata.clone()),
        ));
    }
    let operation = ResourceOperation::new(
        ResourceMethod::Delete,
        &[
            "workspaces",
            assessment.target.workspace_id(),
            "notebooks",
            assessment.target.item_id(),
        ],
        None,
        if assessment.options.wait {
            CompletionPolicy::WaitForCompletion
        } else {
            CompletionPolicy::ReturnAccepted
        },
    )
    .and_then(ResourceOperation::with_hard_delete)
    .map_err(|error| DeleteFailure::resource(&assessment.metadata, error))?;
    let outcome = executor
        .execute(operation)
        .await
        .map_err(|error| DeleteFailure::resource(&assessment.metadata, error))?;
    let (status, operation_id, http_status) = match outcome {
        ResourceOutcome::Completed { status: 200, .. } => {
            (OperationStatus::Succeeded, None, Some(200))
        }
        ResourceOutcome::Accepted { operation_id } => {
            (OperationStatus::Accepted, operation_id, Some(202))
        }
        ResourceOutcome::Completed { status: 202, .. } => {
            (OperationStatus::Accepted, None, Some(202))
        }
        ResourceOutcome::Completed { status, .. } => {
            return Err(DeleteFailure::resource(
                &assessment.metadata,
                ResourceError {
                    code: "UnexpectedResponse".into(),
                    message: "Unexpected Notebook deletion response".into(),
                    status: Some(status),
                    service_error: None,
                    raw_service_error: None,
                    operation_id: None,
                    mutation_may_have_occurred: true,
                },
            ));
        }
    };
    Ok(DeleteReceipt {
        metadata: assessment.metadata.clone(),
        evidence: evidence(
            Some(&assessment.metadata),
            status,
            operation_id,
            http_status,
            DispatchEvidence::ResponseObserved,
            true,
        ),
    })
}

fn evidence(
    metadata: Option<&OperationMetadata>,
    status: OperationStatus,
    operation_id: Option<String>,
    http_status: Option<u16>,
    dispatch: DispatchEvidence,
    mutation_may_have_occurred: bool,
) -> OperationEvidence {
    OperationEvidence {
        status,
        completion: if status == OperationStatus::Succeeded {
            CompletionEvidence::Completed
        } else {
            CompletionEvidence::Unknown
        },
        operation_id,
        http_status,
        dispatch,
        mutation_may_have_occurred,
        effects: metadata
            .into_iter()
            .flat_map(|metadata| metadata.descriptor.effects)
            .map(|effect| EffectEvidence {
                effect_id: effect.id,
                status,
            })
            .collect(),
    }
}
