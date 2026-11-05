// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native MLModel routes and operation metadata, without transport or credentials.

use super::{
    CreateMlModelRequest, ScoreDataRequest, UpdateEndpointRequest, UpdateEndpointVersionRequest,
    UpdateMlModelRequest, validate_uuid,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde::Serialize;
use serde_json::Value;

/// Caller-declared identity kind; this is metadata, not token verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MlModelIdentity {
    /// A delegated user.
    User,
    /// An explicitly selected service principal.
    ServicePrincipal,
    /// An explicitly selected managed identity.
    ManagedIdentity,
}

/// Identity kinds advertised by a native operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MlModelIdentityPolicy {
    /// Only delegated users are supported by the item APIs.
    UserOnly,
    /// Users, service principals and managed identities are supported.
    UserServicePrincipalManagedIdentity,
}

/// Validated, immutable identity of one MLModel item in one workspace.
///
/// Construction does not resolve metadata. Serving requests on known IDs never
/// perform an implicit user-only item lookup.
#[derive(Clone, Eq, PartialEq)]
pub struct MlModelTarget {
    workspace_id: String,
    item_id: String,
}

impl MlModelTarget {
    /// Validate canonical hyphenated UUID syntax and the exact `MLModel` owner.
    ///
    /// UUID hexadecimal letters may use either case; the supplied spelling is
    /// retained. No service lookup, credential inspection or URL inference occurs.
    ///
    /// # Errors
    /// Rejects invalid UUIDs and any owner other than case-sensitive `MLModel`.
    pub fn new(workspace_id: &str, item_id: &str, owner_type: &str) -> Result<Self, ResourceError> {
        validate_uuid(workspace_id)?;
        validate_uuid(item_id)?;
        if owner_type != "MLModel" {
            return Err(ResourceError::input());
        }
        Ok(Self {
            workspace_id: workspace_id.into(),
            item_id: item_id.into(),
        })
    }

    /// Validated workspace identifier, retaining the caller's spelling.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Validated MLModel item identifier, retaining the caller's spelling.
    pub fn item_id(&self) -> &str {
        &self.item_id
    }
}

/// Explicit item deletion intent; the default never requests permanent deletion.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MlModelDeleteMode {
    /// Omit `hardDelete`; do not retry an unsupported soft delete as permanent.
    #[default]
    Soft,
    /// Explicitly send `hardDelete=true`.
    Permanent,
}

/// Observable remote effect of the described operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MlModelEffect {
    /// Observe metadata without changing the model or endpoint.
    Read,
    /// Create an item, not trained weights or an active serving endpoint.
    CreateItem,
    /// Update the item's description, not its name or definition.
    UpdateItem,
    /// Delete an item with explicitly retained permanence intent.
    DeleteItem {
        /// Whether permanent deletion was explicitly selected.
        permanent: bool,
    },
    /// Change default serving assignment configuration.
    UpdateEndpoint,
    /// Change one serving version's scaling configuration.
    UpdateVersion,
    /// Provision serving for one exact version; consumes capacity.
    ActivateVersion,
    /// Disable serving for one exact version, not registry deletion or cancellation.
    DeactivateVersion,
    /// Disable serving for all versions of this model.
    DeactivateAll,
    /// Execute billable scoring through the configured default endpoint.
    ScoreDefault,
    /// Execute billable scoring through one exact version without default fallback.
    ScoreVersion,
}

/// Native result representation, distinct from acceptance or operation state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MlModelResultKind {
    /// MLModel item metadata.
    Item,
    /// Serving endpoint settings.
    Endpoint,
    /// One serving-version page, possibly with continuation.
    VersionsPage,
    /// One serving version's metadata.
    Version,
    /// Native predictions, decoded separately by the typed score outcome.
    Score,
    /// No resource result is documented.
    None,
}

/// One validated native request and its operation-specific safety metadata.
///
/// The caller's shared executor owns HTTP, credentials, cancellation and LRO
/// reconciliation. A request is not evidence that dispatch or completion occurred.
#[derive(Clone, Debug)]
pub struct MlModelRequest {
    operation: ResourceOperation,
    effect: MlModelEffect,
    result_kind: MlModelResultKind,
    identity: MlModelIdentity,
    identity_policy: MlModelIdentityPolicy,
    expected_statuses: &'static [u16],
}

impl MlModelRequest {
    /// Existing relative operation consumed by the shared executor.
    pub fn operation(&self) -> &ResourceOperation {
        &self.operation
    }

    /// Remote effect, including explicit permanent-delete or scoring intent.
    pub const fn effect(&self) -> MlModelEffect {
        self.effect
    }

    /// Native result representation; acceptance is not this result.
    pub const fn result_kind(&self) -> MlModelResultKind {
        self.result_kind
    }

    /// Caller-declared identity kind, not authenticated token claims.
    pub const fn identity(&self) -> MlModelIdentity {
        self.identity
    }

    /// Documented identity support for this operation.
    pub const fn identity_policy(&self) -> MlModelIdentityPolicy {
        self.identity_policy
    }

    /// Documented initial success statuses, not a synthesized execution outcome.
    pub const fn expected_statuses(&self) -> &'static [u16] {
        self.expected_statuses
    }

    /// Delegate exactly once to the caller's existing operation executor.
    ///
    /// No retry, result decoding, paging, identity change or second poller is
    /// installed here. Native acceptance/completion and uncertainty are preserved.
    ///
    /// # Errors
    /// Returns the executor's unchanged error and reconciliation evidence.
    pub async fn execute(
        &self,
        executor: &mut dyn ResourceOperations,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor.execute(self.operation.clone()).await
    }
}

/// Finite native MLModel bindings over a caller-declared identity kind.
///
/// Construction acquires no token and creates no runtime, transport or resolver.
/// The caller must inject an executor using the same explicitly selected identity.
pub struct MlModelRequests {
    identity: MlModelIdentity,
}

impl MlModelRequests {
    /// Select identity-kind metadata without acquiring or inspecting credentials.
    pub const fn new(identity: MlModelIdentity) -> Self {
        Self { identity }
    }

    /// Describe a user-only item metadata GET (HTTP 200), not definition download.
    ///
    /// # Errors
    /// Rejects an unsupported caller identity before dispatch.
    pub fn get_item(&self, target: &MlModelTarget) -> Result<MlModelRequest, ResourceError> {
        self.item_request(
            target,
            ResourceMethod::Get,
            None,
            MlModelEffect::Read,
            MlModelResultKind::Item,
        )
    }

    /// Describe user-only creation (HTTP 201/202), without definition or upsert.
    ///
    /// `wait` requests the actual created item result, not just operation state.
    ///
    /// # Errors
    /// Rejects unsupported identities, invalid workspace UUIDs or invalid payloads.
    pub fn create(
        &self,
        workspace_id: &str,
        payload: &CreateMlModelRequest,
        wait: bool,
    ) -> Result<MlModelRequest, ResourceError> {
        self.require_user()?;
        validate_uuid(workspace_id)?;
        payload.validate()?;
        let operation = ResourceOperation::new(
            ResourceMethod::Post,
            &["workspaces", workspace_id, "mlModels"],
            Some(body(payload)?),
            result_completion(wait),
        )?;
        Ok(self.describe(
            operation,
            MlModelEffect::CreateItem,
            MlModelResultKind::Item,
            MlModelIdentityPolicy::UserOnly,
            &[201, 202],
        ))
    }

    /// Describe a user-only description PATCH (HTTP 200), never rename/replace.
    ///
    /// # Errors
    /// Rejects unsupported identities or an invalid native payload before dispatch.
    pub fn update_item(
        &self,
        target: &MlModelTarget,
        payload: &UpdateMlModelRequest,
    ) -> Result<MlModelRequest, ResourceError> {
        self.require_user()?;
        payload.validate()?;
        self.item_request(
            target,
            ResourceMethod::Patch,
            Some(body(payload)?),
            MlModelEffect::UpdateItem,
            MlModelResultKind::Item,
        )
    }

    /// Describe a user-only typed item DELETE (HTTP 200), soft by default intent.
    ///
    /// Use `MlModelDeleteMode::default()` for soft deletion. Permanent deletion is
    /// only requested by `Permanent`; there is no fallback or invented LRO.
    ///
    /// # Errors
    /// Rejects unsupported identities before dispatch.
    pub fn delete_item(
        &self,
        target: &MlModelTarget,
        mode: MlModelDeleteMode,
    ) -> Result<MlModelRequest, ResourceError> {
        let permanent = mode == MlModelDeleteMode::Permanent;
        let mut request = self.item_request(
            target,
            ResourceMethod::Delete,
            None,
            MlModelEffect::DeleteItem { permanent },
            MlModelResultKind::None,
        )?;
        if permanent {
            request.operation = request.operation.with_hard_delete()?;
        }
        Ok(request)
    }

    /// Describe an endpoint settings GET (HTTP 200), with no item metadata lookup.
    ///
    /// # Errors
    /// Propagates relative-operation validation failures before dispatch.
    pub fn get_endpoint(&self, target: &MlModelTarget) -> Result<MlModelRequest, ResourceError> {
        self.serving_request(
            target,
            &[],
            ResourceMethod::Get,
            None,
            MlModelEffect::Read,
            MlModelResultKind::Endpoint,
        )
    }

    /// Describe an endpoint settings PATCH (HTTP 200), not automatic activation.
    ///
    /// # Errors
    /// Rejects an invalid native payload before dispatch.
    pub fn update_endpoint(
        &self,
        target: &MlModelTarget,
        payload: &UpdateEndpointRequest,
    ) -> Result<MlModelRequest, ResourceError> {
        payload.validate()?;
        self.serving_request(
            target,
            &[],
            ResourceMethod::Patch,
            Some(body(payload)?),
            MlModelEffect::UpdateEndpoint,
            MlModelResultKind::Endpoint,
        )
    }

    /// Describe exactly one serving-version page GET (HTTP 200), not a collector.
    ///
    /// An optional supplied continuation token is opaque and safely query-encoded,
    /// including an explicitly supplied empty token. The shared pager/host owns
    /// bounded traversal, continuation validation and exhaustive-list claims.
    ///
    /// # Errors
    /// Rejects control-bearing tokens before dispatch.
    pub fn list_versions_page(
        &self,
        target: &MlModelTarget,
        continuation_token: Option<&str>,
    ) -> Result<MlModelRequest, ResourceError> {
        if continuation_token.is_some_and(|token| token.chars().any(char::is_control)) {
            return Err(ResourceError::input());
        }
        let mut request = self.serving_request(
            target,
            &["versions"],
            ResourceMethod::Get,
            None,
            MlModelEffect::Read,
            MlModelResultKind::VersionsPage,
        )?;
        if let Some(token) = continuation_token {
            request.operation = request
                .operation
                .with_query(&[("continuationToken", token)])?;
        }
        Ok(request)
    }

    /// Describe a GET of one exact opaque serving version (HTTP 200).
    ///
    /// # Errors
    /// Rejects empty, dot, separator or control-bearing version names.
    pub fn get_version(
        &self,
        target: &MlModelTarget,
        version: &str,
    ) -> Result<MlModelRequest, ResourceError> {
        validate_version(version)?;
        self.serving_request(
            target,
            &["versions", version],
            ResourceMethod::Get,
            None,
            MlModelEffect::Read,
            MlModelResultKind::Version,
        )
    }

    /// Describe a scaling PATCH of one exact serving version (HTTP 200).
    ///
    /// # Errors
    /// Rejects unsafe version names or invalid native payloads before dispatch.
    pub fn update_version(
        &self,
        target: &MlModelTarget,
        version: &str,
        payload: &UpdateEndpointVersionRequest,
    ) -> Result<MlModelRequest, ResourceError> {
        validate_version(version)?;
        payload.validate()?;
        self.serving_request(
            target,
            &["versions", version],
            ResourceMethod::Patch,
            Some(body(payload)?),
            MlModelEffect::UpdateVersion,
            MlModelResultKind::Version,
        )
    }

    /// Describe activation of one exact version (HTTP 200/202), with no body.
    ///
    /// `wait` reconciles completion without requesting a nonexistent result.
    ///
    /// # Errors
    /// Rejects unsafe version names before dispatch.
    pub fn activate(
        &self,
        target: &MlModelTarget,
        version: &str,
        wait: bool,
    ) -> Result<MlModelRequest, ResourceError> {
        validate_version(version)?;
        self.lifecycle(
            target,
            &["versions", version, "activate"],
            MlModelEffect::ActivateVersion,
            wait,
        )
    }

    /// Describe deactivation of one exact version (HTTP 200/202), with no body.
    ///
    /// This does not cancel in-flight scoring or delete registry versions.
    ///
    /// # Errors
    /// Rejects unsafe version names before dispatch.
    pub fn deactivate(
        &self,
        target: &MlModelTarget,
        version: &str,
        wait: bool,
    ) -> Result<MlModelRequest, ResourceError> {
        validate_version(version)?;
        self.lifecycle(
            target,
            &["versions", version, "deactivate"],
            MlModelEffect::DeactivateVersion,
            wait,
        )
    }

    /// Describe explicit model-scoped deactivation of all versions (HTTP 200/202).
    ///
    /// No request body or result is documented. There is no sentinel-version
    /// coercion from an exact version operation to this action.
    ///
    /// # Errors
    /// Propagates relative-operation validation failures before dispatch.
    pub fn deactivate_all(
        &self,
        target: &MlModelTarget,
        wait: bool,
    ) -> Result<MlModelRequest, ResourceError> {
        self.lifecycle(
            target,
            &["versions", "deactivateAll"],
            MlModelEffect::DeactivateAll,
            wait,
        )
    }

    /// Describe native default-endpoint scoring (HTTP 200/202).
    ///
    /// Uses the documented mixed-case `mlModels` route. A missing/inactive default
    /// is not replaced with the latest/first version. `wait` requests predictions.
    ///
    /// # Errors
    /// Rejects invalid native scoring input before dispatch.
    pub fn score_default(
        &self,
        target: &MlModelTarget,
        payload: &ScoreDataRequest,
        wait: bool,
    ) -> Result<MlModelRequest, ResourceError> {
        payload.validate()?;
        let operation = ResourceOperation::new(
            ResourceMethod::Post,
            &[
                "workspaces",
                target.workspace_id(),
                "mlModels",
                target.item_id(),
                "endpoint",
                "score",
            ],
            Some(body(payload)?),
            result_completion(wait),
        )?;
        Ok(self.describe(
            operation,
            MlModelEffect::ScoreDefault,
            MlModelResultKind::Score,
            MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            &[200, 202],
        ))
    }

    /// Describe native scoring of one exact opaque version (HTTP 200/202).
    ///
    /// Uses lowercase `mlmodels`, without numeric parsing or default fallback.
    /// `wait` retrieves predictions, not just an LRO state document.
    ///
    /// # Errors
    /// Rejects unsafe version names or invalid native input before dispatch.
    pub fn score_version(
        &self,
        target: &MlModelTarget,
        version: &str,
        payload: &ScoreDataRequest,
        wait: bool,
    ) -> Result<MlModelRequest, ResourceError> {
        validate_version(version)?;
        payload.validate()?;
        let operation = ResourceOperation::encoded(
            ResourceMethod::Post,
            &[
                "workspaces",
                target.workspace_id(),
                "mlmodels",
                target.item_id(),
                "endpoint",
                "versions",
                version,
                "score",
            ],
            Some(body(payload)?),
            result_completion(wait),
        )?;
        Ok(self.describe(
            operation,
            MlModelEffect::ScoreVersion,
            MlModelResultKind::Score,
            MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            &[200, 202],
        ))
    }

    fn require_user(&self) -> Result<(), ResourceError> {
        if self.identity != MlModelIdentity::User {
            return Err(ResourceError {
                code: "UnsupportedIdentity".into(),
                message: "MLModel item operations require a user identity".into(),
                ..ResourceError::input()
            });
        }
        Ok(())
    }

    fn describe(
        &self,
        operation: ResourceOperation,
        effect: MlModelEffect,
        result_kind: MlModelResultKind,
        identity_policy: MlModelIdentityPolicy,
        expected_statuses: &'static [u16],
    ) -> MlModelRequest {
        MlModelRequest {
            operation,
            effect,
            result_kind,
            identity: self.identity,
            identity_policy,
            expected_statuses,
        }
    }

    fn item_request(
        &self,
        target: &MlModelTarget,
        method: ResourceMethod,
        body: Option<Value>,
        effect: MlModelEffect,
        result: MlModelResultKind,
    ) -> Result<MlModelRequest, ResourceError> {
        self.require_user()?;
        let operation = ResourceOperation::new(
            method,
            &[
                "workspaces",
                target.workspace_id(),
                "mlModels",
                target.item_id(),
            ],
            body,
            CompletionPolicy::ReturnAccepted,
        )?;
        Ok(self.describe(
            operation,
            effect,
            result,
            MlModelIdentityPolicy::UserOnly,
            &[200],
        ))
    }

    fn serving_request(
        &self,
        target: &MlModelTarget,
        suffix: &[&str],
        method: ResourceMethod,
        body: Option<Value>,
        effect: MlModelEffect,
        result: MlModelResultKind,
    ) -> Result<MlModelRequest, ResourceError> {
        let mut segments = vec![
            "workspaces",
            target.workspace_id(),
            "mlmodels",
            target.item_id(),
            "endpoint",
        ];
        segments.extend_from_slice(suffix);
        let operation =
            ResourceOperation::encoded(method, &segments, body, CompletionPolicy::ReturnAccepted)?;
        Ok(self.describe(
            operation,
            effect,
            result,
            MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            &[200],
        ))
    }

    fn lifecycle(
        &self,
        target: &MlModelTarget,
        suffix: &[&str],
        effect: MlModelEffect,
        wait: bool,
    ) -> Result<MlModelRequest, ResourceError> {
        let mut segments = vec![
            "workspaces",
            target.workspace_id(),
            "mlmodels",
            target.item_id(),
            "endpoint",
        ];
        segments.extend_from_slice(suffix);
        let operation = ResourceOperation::encoded(
            ResourceMethod::Post,
            &segments,
            None,
            if wait {
                CompletionPolicy::WaitForCompletion
            } else {
                CompletionPolicy::ReturnAccepted
            },
        )?;
        Ok(self.describe(
            operation,
            effect,
            MlModelResultKind::None,
            MlModelIdentityPolicy::UserServicePrincipalManagedIdentity,
            &[200, 202],
        ))
    }
}

fn result_completion(wait: bool) -> CompletionPolicy {
    if wait {
        CompletionPolicy::WaitForResult
    } else {
        CompletionPolicy::ReturnAccepted
    }
}

fn body(payload: &impl Serialize) -> Result<Value, ResourceError> {
    serde_json::to_value(payload).map_err(|_| ResourceError::input())
}

fn validate_version(version: &str) -> Result<(), ResourceError> {
    if version.is_empty()
        || matches!(version, "." | "..")
        || version.contains(['/', '\\'])
        || version.chars().any(char::is_control)
    {
        return Err(ResourceError::input());
    }
    Ok(())
}
