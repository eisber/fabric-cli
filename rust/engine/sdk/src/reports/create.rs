// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline validation of the bounded native Report definition-create request.
//!
//! This adapter supplies schema evidence and native policy support to the shared
//! create planner, then delegates submission to existing item operations. It does
//! not resolve model references, authorize a mutation or install an execution host.

use super::ReportDefinition;
use crate::{
    items::ItemCollection,
    representations::DefinitionRead,
    v2::WorkspaceIdentity,
};
use fabric_sdk_items::create::{
    ConflictPolicy, CreateAction, CreateCapabilities, CreateError, CreateInputValidator,
    CreatePlan, CreateSupport, CreateTarget, DuplicateSupport, InputDisposition, SchemaRef,
    SkipSupport, TargetScope,
};
use fabric_sdk_items::mutations::ItemMutations;
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceOperations, ResourceOutcome,
};
use serde_json::Value;
use std::num::NonZeroUsize;

/// Schema identity for the bounded public `CreateReportRequest` definition mode.
pub const SCHEMA: &str = "fabric/report/create-definition/v1";

/// Native Report policy support, not guarantees inferred from a name preflight.
///
/// The public Create Report API rejects `ItemDisplayNameAlreadyInUse`; this
/// adapter never upserts. Exact existing Report targets may be left unchanged.
/// Neither complete identity-preserving replacement nor repeated-name creation
/// is advertised by this API, so overwrite/force and duplicate remain unsupported.
pub fn capabilities() -> CreateCapabilities {
    CreateCapabilities {
        schema: SchemaRef::new(SCHEMA).expect("constant Report schema"),
        create: CreateSupport::RejectExisting,
        replacement: None,
        skip: SkipSupport::ExactExistingUnchanged,
        duplicate: DuplicateSupport::Unsupported,
    }
}

/// Caller-bounded Report input validator, with no I/O or credential dependency.
///
/// The outer request is deliberately limited to display name, description,
/// folder selection and a complete native multipart definition. Definition and
/// part extension fields remain untouched. Label changes and the separate
/// `creationPayload` mode are not advertised by this adapter.
#[derive(Clone, Copy, Debug)]
pub struct ReportCreateValidator {
    max_body_bytes: NonZeroUsize,
}

impl ReportCreateValidator {
    /// Retain the caller's finite request-body limit.
    pub const fn new(max_body_bytes: NonZeroUsize) -> Self {
        Self { max_body_bytes }
    }

    /// Validate an exact named Report destination and its native request body.
    ///
    /// Reuses the Report representation codec and reference classification.
    /// Complete here means recognized required native structure, not full JSON
    /// Schema validation or permission to access the referenced semantic model.
    /// No source field or part is cleaned, rewritten, rebound or dropped.
    ///
    /// # Errors
    /// Rejects wrong targets, unknown outer fields, missing/null/invalid input,
    /// partial source and requests exceeding the caller's byte limit.
    pub fn inspect(
        &self,
        target: &CreateTarget,
        supplied: &Value,
    ) -> Result<ReportDefinition, CreateError> {
        if target.scope() != TargetScope::NamedLeaf
            || target.item_type() != "Report"
            || WorkspaceIdentity::new(target.parent_id().as_str()).is_err()
        {
            return Err(CreateError::InputRejected);
        }
        let body = supplied.as_object().ok_or(CreateError::InputRejected)?;
        if body.keys().any(|key| {
            !matches!(
                key.as_str(),
                "displayName" | "description" | "folderId" | "definition"
            )
        }) || body.get("displayName").and_then(Value::as_str) != Some(target.name())
        {
            return Err(CreateError::InputRejected);
        }
        if let Some(description) = body.get("description")
            && !description
                .as_str()
                .is_some_and(|description| description.chars().count() <= 256)
        {
            return Err(CreateError::InputRejected);
        }
        if let Some(folder) = body.get("folderId")
            && !folder.is_null()
            && !folder
                .as_str()
                .is_some_and(|id| WorkspaceIdentity::new(id).is_ok())
        {
            return Err(CreateError::InputRejected);
        }
        let bytes = serde_json::to_vec(supplied).map_err(|_| CreateError::InputRejected)?;
        if bytes.len() > self.max_body_bytes.get() {
            return Err(CreateError::InputRejected);
        }
        let definition = ReportDefinition::from_read(
            DefinitionRead::Completed {
                definition: supplied.clone(),
            },
            None,
        )
        .map_err(|_| CreateError::InputRejected)?;
        if !definition.complete() {
            return Err(CreateError::InputRejected);
        }
        Ok(definition)
    }

    /// Submit one validated creation action through the existing item operations.
    ///
    /// The caller must first bind and obtain immediate approval for this exact
    /// plan, body, effect and immutable execution context. Credentials, whole
    /// deadlines, cancellation, acceptance/result observation and output remain
    /// owned by that shared host. This adapter adds no polling or replay.
    ///
    /// # Errors
    /// Rejects unsupported plans, schemas or substituted inputs before execution,
    /// and returns shared mutation failures with their existing evidence intact.
    pub async fn submit(
        &self,
        executor: &mut dyn ResourceOperations,
        plan: &CreatePlan,
        completion: CompletionPolicy,
    ) -> Result<ResourceOutcome, ResourceError> {
        if !matches!(plan.policy(), ConflictPolicy::Error | ConflictPolicy::Skip) {
            return Err(ResourceError::input());
        }
        let CreateAction::Create { input } = plan.action() else {
            return Err(ResourceError::input());
        };
        self.validate(input.target(), input.schema(), input.supplied())
            .map_err(|_| ResourceError::input())?;
        let body = input.supplied().ok_or_else(ResourceError::input)?.clone();
        ItemMutations::create_with_completion(
            executor,
            input.target().parent_id().as_str(),
            ItemCollection::Report,
            body,
            completion,
        )
        .await
    }
}

impl CreateInputValidator for ReportCreateValidator {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if schema.as_str() != SCHEMA {
            return Err(CreateError::InputRejected);
        }
        self.inspect(target, supplied.ok_or(CreateError::InputRejected)?)?;
        Ok(InputDisposition::CreateOnly)
    }
}
