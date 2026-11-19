// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Definition-free Environment creation over the shared create host.
//!
//! The native request uses the documented CreateEnvironmentRequest metadata
//! subset. An optional input `type` is a local consistency assertion, not an
//! undocumented provider property. V1's existing payload remains unchanged.

use crate::{
    create_command::{CreateBinding, MAX_BYTES},
    get_target::is_id,
};
use fabric_sdk::items::{
    ItemCollection, ItemMutations,
    create::{
        CreateAction, CreateCapabilities, CreateError, CreateInputValidator, CreatePlan,
        CreateSupport, CreateTarget, DuplicateSupport, InputDisposition, SchemaRef, SkipSupport,
        TargetScope,
    },
};
use fabric_sdk_operations::FabricOperation;
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

const SCHEMA: &str = "fabric.environment.create.metadata/v1";

/// Root-level Environment metadata only; no definition, publish or compute work.
pub struct EnvironmentCreate;

impl CreateInputValidator for EnvironmentCreate {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        input: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if target.scope() != TargetScope::NamedLeaf
            || target.item_type() != "Environment"
            || schema.as_str() != SCHEMA
            || !is_id(target.parent_id().as_str())
            || target.name().trim().is_empty()
            || target.name().len() > 256
            || target.name().chars().any(char::is_control)
        {
            return Err(CreateError::InputRejected);
        }
        if let Some(input) = input {
            let object = input.as_object().ok_or(CreateError::InputRejected)?;
            if object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "displayName" | "description" | "type" | "folderId"
                )
            }) || object.get("displayName").and_then(Value::as_str) != Some(target.name())
                || object
                    .get("type")
                    .is_some_and(|value| value.as_str() != Some("Environment"))
                || object.get("folderId").is_some_and(|value| !value.is_null())
                || object.get("description").is_some_and(|value| {
                    !value
                        .as_str()
                        .is_some_and(|text| text.chars().count() <= 256)
                })
            {
                return Err(CreateError::InputRejected);
            }
        }
        Ok(InputDisposition::CreateOnly)
    }
}

impl CreateBinding for EnvironmentCreate {
    fn item_type(&self) -> &'static str {
        "Environment"
    }

    fn capabilities(&self) -> CreateCapabilities {
        CreateCapabilities {
            schema: SchemaRef::new(SCHEMA).expect("fixed Environment schema"),
            create: CreateSupport::RejectExisting,
            replacement: None,
            skip: SkipSupport::ExactExistingUnchanged,
            duplicate: DuplicateSupport::Unsupported,
        }
    }

    fn operation(
        &self,
        plan: &CreatePlan,
        raw: Option<&[u8]>,
        wait: bool,
    ) -> Result<FabricOperation, CreateError> {
        let CreateAction::Create { input } = plan.action() else {
            return Err(CreateError::UnsupportedPolicy);
        };
        let decoded = raw
            .map(|bytes| fabric_cli_output::input::strict_json_bounded(bytes, MAX_BYTES))
            .transpose()
            .map_err(|_| CreateError::InputRejected)?;
        if decoded.as_ref() != input.supplied() {
            return Err(CreateError::InputRejected);
        }
        self.validate(plan.target(), input.schema(), input.supplied())?;
        let mut body = json!({"displayName":plan.target().name(),"folderId":null});
        if let Some(description) = input.supplied().and_then(|value| value.get("description")) {
            body["description"] = description.clone();
        }
        let mut capture = RequestCapture(None);
        let completion = if wait {
            CompletionPolicy::WaitForResult
        } else {
            CompletionPolicy::ReturnAccepted
        };
        // Capture the existing SDK's request, never fabricate an execution result.
        let result = ItemMutations::create_with_completion(
            &mut capture,
            plan.target().parent_id().as_str(),
            ItemCollection::Environment,
            body,
            completion,
        )
        .now_or_never();
        if result.is_some() {
            return Err(CreateError::InputRejected);
        }
        let operation = capture.0.ok_or(CreateError::InputRejected)?;
        FabricOperation::from_resource(&operation).map_err(|_| CreateError::InputRejected)
    }
}

struct RequestCapture(Option<ResourceOperation>);

impl ResourceOperations for RequestCapture {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.0 = Some(operation);
        Box::pin(std::future::pending())
    }
}
