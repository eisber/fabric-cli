// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    CacheEffect, MutationError, MutationHost, MutationResult, NoticeKind, message,
    supported_element, workspace_id,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_cli_context::{Element, NodeKind};
use fabric_sdk_folders::mutations::FolderMutations;
use fabric_sdk_items::{ItemCollection, mutations::ItemMutations};
use fabric_sdk_workspaces::mutations::{ResourceError, ResourceOutcome, WorkspaceMutations};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// Explicit capacity resolution supplied by the main host, without ambient config.
#[derive(Clone, Debug, Default)]
pub enum CapacitySelection {
    /// Use the caller's configured default capacity ID.
    #[default]
    Default,
    /// Python's `capacityName=none`, which omits `capacityId` completely.
    Unassigned,
    /// A capacity name resolved by the existing context/catalog host.
    Resolved(String),
}

/// Parsed creation options. Parameter-string parsing remains with the CLI owner.
#[derive(Clone, Debug, Default)]
pub struct CreateOptions {
    /// Source parameters; empty means help for workspaces/items, not folders.
    pub params: Map<String, Value>,
    /// Selected/resolved capacity, without credential or tenant discovery.
    pub capacity: CapacitySelection,
    /// Explicitly injected configured default, never read from global state.
    pub default_capacity_id: Option<String>,
}

impl MutationHost<'_> {
    /// Execute `mkdir`/`create` for a workspace, folder or supported item.
    ///
    /// # Errors
    /// Preserves duplicate/required-parameter failures. Definition-template and
    /// dependency-creation branches not yet translated fail explicitly.
    pub async fn create(
        &mut self,
        element: &Arc<Element>,
        options: &CreateOptions,
    ) -> Result<MutationResult, MutationError> {
        supported_element(element)?;
        let params = lowercase_map(&options.params);
        if element.kind() != NodeKind::Folder && params.is_empty() {
            self.ui.notice(NoticeKind::Help, &parameter_help(element));
            return Ok(MutationResult::default());
        }
        if element.id().is_some() {
            return Err(MutationError::new(
                "AlreadyExists",
                match element.kind() {
                    NodeKind::Workspace(_) => {
                        "A workspace with the same name already exists. Please use a different name"
                    }
                    NodeKind::Folder => "A folder with the same name already exists",
                    _ => "An item with the same name exists",
                },
            ));
        }
        let outcome = match element.kind() {
            NodeKind::Workspace(_) => {
                let mut payload = params;
                payload.remove("displayname");
                payload.remove("capacityname");
                let capacity = match &options.capacity {
                    CapacitySelection::Unassigned => None,
                    CapacitySelection::Resolved(id) => Some(id),
                    CapacitySelection::Default => Some(
                        options
                            .default_capacity_id
                            .as_ref()
                            .ok_or_else(capacity_error)?,
                    ),
                };
                if let Some(id) = capacity {
                    if id.is_empty() {
                        return Err(capacity_error());
                    }
                    payload.insert("capacityId".into(), Value::String(id.clone()));
                }
                let mut body = json!({"displayName": element.short_name()});
                body.as_object_mut().expect("object").extend(payload);
                self.ui
                    .notice(NoticeKind::Progress, "Creating a new Workspace...");
                WorkspaceMutations::create(self.operations, body).await?
            }
            NodeKind::Folder => {
                let mut body = json!({"displayName": element.short_name()});
                if let Some(description) = params.get("description").filter(|v| truthy(v)) {
                    body["description"] = description.clone();
                }
                if let Some(parent) = element.parent().filter(|p| p.kind() == NodeKind::Folder)
                    && let Some(id) = parent.id().filter(|id| !id.is_empty())
                {
                    body["parentFolderId"] = Value::String(id.to_owned());
                }
                self.ui
                    .notice(NoticeKind::Progress, "Creating a new Folder...");
                FolderMutations::create(self.operations, workspace_id(element)?, body).await?
            }
            NodeKind::Item(item_type) => {
                check_create_support(item_type.as_str())?;
                let folder_id = element
                    .parent()
                    .filter(|parent| parent.kind() == NodeKind::Folder)
                    .and_then(|parent| parent.id());
                let mut payload = json!({
                    "displayName": element.short_name(),
                    "type": item_type.as_str(),
                    "folderId": folder_id,
                });
                if let Some(description) = params.get("description").filter(|v| truthy(v)) {
                    payload["description"] = description.clone();
                }
                add_item_payload(
                    item_type.as_str(),
                    workspace_id(element)?,
                    &params,
                    &mut payload,
                )?;
                let collection =
                    ItemCollection::from_item_type(item_type.as_str()).ok_or_else(|| {
                        MutationError::new("NotSupported", "Unmapped item collection")
                    })?;
                self.ui.notice(
                    NoticeKind::Progress,
                    &format!("Creating a new {}...", item_type.as_str()),
                );
                ItemMutations::create(self.operations, workspace_id(element)?, collection, payload)
                    .await?
            }
            NodeKind::Tenant => unreachable!("validated above"),
        };
        if matches!(element.kind(), NodeKind::Item(_))
            && matches!(outcome, ResourceOutcome::Accepted { .. })
        {
            return Err(incomplete_create(
                &outcome,
                "OperationIncomplete",
                "Item creation was accepted but has not completed",
            ));
        }
        if let ResourceOutcome::Completed {
            status: 200 | 201,
            data,
        } = &outcome
        {
            if !data.as_ref().is_some_and(Value::is_object) {
                return Err(incomplete_create(
                    &outcome,
                    "InvalidResponse",
                    "Creation returned invalid resource metadata",
                ));
            }
            if matches!(element.kind(), NodeKind::Workspace(_))
                && data
                    .as_ref()
                    .and_then(|data| data.get("id"))
                    .and_then(Value::as_str)
                    .is_none()
            {
                return Err(incomplete_create(
                    &outcome,
                    "InvalidResponse",
                    "Workspace creation returned no resource ID",
                ));
            }
        }
        let mut result = MutationResult {
            outcome: Some(outcome.clone()),
            ..MutationResult::default()
        };
        if let ResourceOutcome::Completed {
            status: 200 | 201,
            data: Some(data),
        } = outcome
        {
            let mut output = message(format!("'{}' created", element.name()));
            output.data = Some(data.clone());
            result.outputs.push(output);
            result.show_headers = true;
            result.effects.push(
                if data
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty())
                {
                    CacheEffect::Created {
                        target: Arc::clone(element),
                        data,
                    }
                } else {
                    CacheEffect::Invalidate {
                        target: Arc::clone(element),
                    }
                },
            );
        }
        Ok(result)
    }
}

fn incomplete_create(outcome: &ResourceOutcome, code: &str, message: &str) -> MutationError {
    let (status, operation_id) = match outcome {
        ResourceOutcome::Accepted { operation_id } => (Some(202), operation_id.clone()),
        ResourceOutcome::Completed { status, .. } => (Some(*status), None),
    };
    ResourceError {
        code: code.into(),
        message: message.into(),
        status,
        operation_id,
        mutation_may_have_occurred: true,
    }
    .into()
}

fn capacity_error() -> MutationError {
    MutationError::new(
        "InvalidInput",
        "The specified capacity was not found or is invalid. Please use 'config set default_capacity <capacity_name>' or '-P capacityName=<capacity_name>' to specify a valid capacity",
    )
}

fn check_create_support(item_type: &str) -> Result<(), MutationError> {
    if matches!(
        item_type,
        "Dashboard"
            | "PaginatedReport"
            | "SQLEndpoint"
            | "Datamart"
            | "MirroredWarehouse"
            | "OrgApp"
            | "MetricSet"
            | "ApacheAirflowJob"
            | "Exploration"
            | "RetailDataManager"
            | "Healthcaredatasolution"
            | "SustainabilityDataSolution"
            | "AISkill"
    ) {
        return Err(MutationError::new(
            "UnsupportedCommand",
            format!("mkdir is not supported for {item_type}"),
        ));
    }
    Ok(())
}

fn add_item_payload(
    item_type: &str,
    workspace: &str,
    params: &Map<String, Value>,
    payload: &mut Value,
) -> Result<(), MutationError> {
    let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("");
    match item_type {
        "Lakehouse" if text("enableschemas").eq_ignore_ascii_case("true") => {
            payload["creationPayload"] = json!({"enableSchemas": true});
        }
        "Warehouse" => {
            payload["creationPayload"] = json!({
                "defaultCollation": if text("enablecaseinsensitive").eq_ignore_ascii_case("true") {
                    "Latin1_General_100_CI_AS_KS_WS_SC_UTF8"
                } else {
                    "Latin1_General_100_BIN2_UTF8"
                }
            });
        }
        "KQLDatabase" => {
            let eventhouse = text("eventhouseid");
            if eventhouse.is_empty() {
                return Err(unported("KQLDatabase dependency creation"));
            }
            let db_type = text("dbtype");
            if db_type.is_empty() || db_type.eq_ignore_ascii_case("readwrite") {
                payload["creationPayload"] = json!({
                    "databaseType": "ReadWrite", "parentEventhouseItemId": eventhouse,
                });
            } else if db_type.eq_ignore_ascii_case("shortcut")
                && !text("clusteruri").is_empty()
                && !text("databasename").is_empty()
            {
                payload["creationPayload"] = json!({
                    "databaseType": "Shortcut", "parentEventhouseItemId": eventhouse,
                    "sourceClusterUri": text("clusteruri"),
                    "sourceDatabaseName": text("databasename"),
                });
            } else {
                return Err(unported("KQLDatabase fallback dependency creation"));
            }
        }
        "DigitalTwinBuilderFlow" => {
            if text("digitaltwinbuilderid").is_empty() {
                return Err(unported("DigitalTwinBuilderFlow dependency creation"));
            }
            payload["creationPayload"] = json!({
                "digitalTwinBuilderItemReference": {
                    "referenceType": "ById",
                    "itemId": text("digitaltwinbuilderid"),
                    "workspaceId": workspace,
                }
            });
        }
        "Report" | "SemanticModel" | "MirroredDatabase" => {
            return Err(unported("item-specific definition template creation"));
        }
        "Notebook" => {
            payload["definition"] = json!({"parts": [{
                "path": "notebook-content.py",
                "payload": "IyBGYWJyaWMgbm90ZWJvb2sgc291cmNlCgojIE1FVEFEQVRBICoqKioqKioqKioqKioqKioqKioqCgojIE1FVEEgewojIE1FVEEgICAia2VybmVsX2luZm8iOiB7CiMgTUVUQSAgICAgIm5hbWUiOiAic3luYXBzZV9weXNwYXJrIgojIE1FVEEgICB9LAojIE1FVEEgICAiZGVwZW5kZW5jaWVzIjoge30KIyBNRVRBIH0KCiMgQ0VMTCAqKioqKioqKioqKioqKioqKioqKgoKIyBXZWxjb21lIHRvIHlvdXIgbmV3IG5vdGVib29rCiMgVHlwZSBoZXJlIGluIHRoZSBjZWxsIGVkaXRvciB0byBhZGQgY29kZSEKCgojIE1FVEFEQVRBICoqKioqKioqKioqKioqKioqKioqCgojIE1FVEEgewojIE1FVEEgICAibGFuZ3VhZ2UiOiAicHl0aG9uIiwKIyBNRVRBICAgImxhbmd1YWdlX2dyb3VwIjogInN5bmFwc2VfcHlzcGFyayIKIyBNRVRBIH0K",
                "payloadType": "InlineBase64",
            }]});
        }
        "MountedDataFactory" => {
            for key in ["subscriptionid", "resourcegroup", "factoryname"] {
                if !params.contains_key(key) || params[key].is_null() {
                    return Err(MutationError::new(
                        "InvalidInput",
                        "Missing mandatory params '['subscriptionId', 'resourceGroup', 'factoryName']'. Please provide using -P/--params",
                    ));
                }
            }
            let content = format!(
                "{{\"dataFactoryResourceId\": \"/subscriptions/{}/resourceGroups/{}/providers/Microsoft.DataFactory/factories/{}\"}}",
                text("subscriptionid"),
                text("resourcegroup"),
                text("factoryname"),
            );
            payload["definition"] = json!({"parts": [{
                "path": "mountedDataFactory-content.json",
                "payload": STANDARD.encode(content.as_bytes()),
                "payloadType": "InlineBase64",
            }]});
        }
        "SQLDatabase"
            if params
                .keys()
                .any(|key| key != "description" && key != "run") =>
        {
            return Err(unported("SQLDatabase creation options"));
        }
        _ => {}
    }
    Ok(())
}

fn unported(branch: &str) -> MutationError {
    MutationError::new(
        "NotSupported",
        format!("This Rust creation branch is not yet translated: {branch}"),
    )
}

pub(crate) fn lowercase_map(params: &Map<String, Value>) -> Map<String, Value> {
    params
        .iter()
        .map(|(key, value)| {
            (
                key.to_lowercase(),
                match value {
                    Value::Object(map) => Value::Object(lowercase_map(map)),
                    _ => value.clone(),
                },
            )
        })
        .collect()
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64() != Some(0.0),
    }
}

fn parameter_help(element: &Element) -> String {
    let (kind, required, mut optional): (&str, &[&str], Vec<&str>) = match element.kind() {
        NodeKind::Workspace(_) => ("Workspace", &[], vec!["capacityName"]),
        NodeKind::Item(item_type) => {
            let (required, optional): (&[&str], &[&str]) = match item_type.as_str() {
                "Lakehouse" => (&[], &["enableSchemas"]),
                "Warehouse" => (&[], &["enableCaseInsensitive"]),
                "KQLDatabase" => (
                    &[],
                    &["dbType", "eventhouseId", "clusterUri", "databaseName"],
                ),
                "DigitalTwinBuilderFlow" => (&[], &["digitalTwinBuilderId"]),
                "MirroredDatabase" => (
                    &[],
                    &[
                        "mirrorType",
                        "connectionId",
                        "database",
                        "defaultSchema",
                        "mountedTables",
                    ],
                ),
                "Report" => (&[], &["semanticModelId"]),
                "MountedDataFactory" => (&["subscriptionId", "resourceGroup", "factoryName"], &[]),
                "SQLDatabase" => (
                    &[],
                    &[
                        "creationMode",
                        "backupRetentionDays",
                        "collation",
                        "restorePointInTime",
                        "itemId",
                        "workspaceId",
                        "referenceType",
                        "restorableDeletedDatabaseName",
                    ],
                ),
                _ => (&[], &[]),
            };
            (item_type.as_str(), required, optional.to_vec())
        }
        _ => unreachable!("only workspace/item help"),
    };
    let mut text = format!("Params for '.{kind}'. Use key=value separated by commas.");
    if !required.is_empty() {
        let mut required = required.to_vec();
        required.sort_unstable();
        text.push_str(&format!(
            "\n\nRequired params:\n  {}",
            required.join("\n  ")
        ));
    }
    if !optional.is_empty() {
        optional.sort_unstable();
        text.push_str(&format!(
            "\n\nOptional params:\n  {}",
            optional.join("\n  ")
        ));
    }
    text.push('\n');
    text
}
