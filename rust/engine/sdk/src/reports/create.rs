// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline validation of the bounded native Report definition-create request.
//!
//! This adapter supplies schema evidence to the shared create planner. It does
//! not advertise conflict guarantees, resolve model references, authorize a
//! mutation, or install an execution host.

use super::ReportDefinition;
use crate::{representations::DefinitionRead, v2::WorkspaceIdentity};
use fabric_sdk_items::create::{
    CreateError, CreateInputValidator, CreateTarget, InputDisposition, SchemaRef, TargetScope,
};
use serde_json::Value;
use std::num::NonZeroUsize;

/// Schema identity for the bounded public `CreateReportRequest` definition mode.
pub const SCHEMA: &str = "fabric/report/create-definition/v1";

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
