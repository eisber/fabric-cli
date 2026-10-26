// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    CacheEffect, CreationEvidence, MutationError, MutationHost, MutationResult, NoticeKind,
    message, supported_element, workspace_id,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_cli_context::{Element, ItemType, NodeKind};
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
                return self.create_item_with_dependencies(element, params).await;
            }
            NodeKind::Tenant => unreachable!("validated above"),
        };
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

    async fn create_item_with_dependencies(
        &mut self,
        element: &Arc<Element>,
        mut params: Map<String, Value>,
    ) -> Result<MutationResult, MutationError> {
        let mut evidence = CreationEvidence::default();
        let result = async {
            let NodeKind::Item(item_type) = element.kind() else {
                unreachable!("item dispatch");
            };
            validate_required_params(item_type.as_str(), &params)?;
            self.ui.notice(
                NoticeKind::Progress,
                &format!("Creating a new {}...", item_type.as_str()),
            );
            params.remove("displayname");
            params.remove("type");
            let dependency = dependency_for(item_type.as_str(), &params);
            let mut dependency_id = None;
            if let Some((kind, warning)) = dependency {
                self.ui.notice(NoticeKind::Warning, warning);
                let parent = Arc::clone(element.parent().ok_or_else(|| {
                    MutationError::new("InvalidContext", "Missing creation parent")
                })?);
                let target = Element::child(
                    format!("{}_auto", element.short_name()),
                    None,
                    NodeKind::Item(ItemType::parse(kind).map_err(|_| {
                        MutationError::new("InvalidItemType", "Invalid dependency item type")
                    })?),
                    parent,
                )
                .map_err(|_| {
                    MutationError::new("InvalidContext", "Invalid dependency creation parent")
                })?;
                self.ui
                    .notice(NoticeKind::Progress, &format!("Creating a new {kind}..."));
                let outcome = self.dispatch_item_create(&target, &params, None).await?;
                evidence.responses.push(outcome.clone());
                record_item_creation(&target, &outcome, &mut evidence)?;
                dependency_id = completed_id(&outcome);
            }
            let outcome = self
                .dispatch_item_create(element, &params, dependency_id.as_ref())
                .await?;
            evidence.responses.push(outcome.clone());
            record_item_creation(element, &outcome, &mut evidence)?;
            let mut result = MutationResult {
                outcome: Some(outcome.clone()),
                effects: evidence.effects.clone(),
                ..MutationResult::default()
            };
            if let ResourceOutcome::Completed {
                status: 200 | 201,
                data: Some(data),
            } = outcome
            {
                let (text, data) = if dependency.is_some() {
                    (
                        batch_message(&evidence.batch_names),
                        Value::Array(evidence.batch_items.clone()),
                    )
                } else {
                    (format!("'{}' created", element.name()), data)
                };
                result.outputs.push(fabric_cli_output::OutputInput {
                    message: Some(text),
                    data: Some(data),
                    ..Default::default()
                });
                result.show_headers = true;
            }
            result.creation = Some(Box::new(evidence.clone()));
            Ok(result)
        }
        .await;
        result.map_err(|mut error: MutationError| {
            if !evidence.responses.is_empty() {
                error.creation = Some(Box::new(evidence));
            }
            error
        })
    }

    async fn dispatch_item_create(
        &mut self,
        element: &Arc<Element>,
        params: &Map<String, Value>,
        dependency: Option<&Value>,
    ) -> Result<ResourceOutcome, MutationError> {
        let NodeKind::Item(item_type) = element.kind() else {
            unreachable!("item dispatch");
        };
        let folder = element
            .parent()
            .filter(|parent| parent.kind() == NodeKind::Folder)
            .and_then(|parent| parent.id());
        let mut payload =
            json!({"displayName":element.short_name(),"type":item_type.as_str(),"folderId":folder});
        if let Some(description) = params.get("description").filter(|value| truthy(value)) {
            payload["description"] = description.clone();
        }
        add_item_payload(
            item_type.as_str(),
            workspace_id(element)?,
            params,
            dependency,
            &mut payload,
        )?;
        let collection = ItemCollection::from_item_type(item_type.as_str())
            .ok_or_else(|| MutationError::new("NotSupported", "Unmapped item collection"))?;
        Ok(
            ItemMutations::create(self.operations, workspace_id(element)?, collection, payload)
                .await?,
        )
    }
}

fn dependency_for(
    item_type: &str,
    params: &Map<String, Value>,
) -> Option<(&'static str, &'static str)> {
    let has = |key: &str| params.get(key).is_some_and(truthy);
    let text = |key: &str, default: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_lowercase()
    };
    match item_type {
        "KQLDatabase"
            if !(has("eventhouseid")
                && (text("dbtype", "readwrite") == "readwrite"
                    || (text("dbtype", "readwrite") == "shortcut"
                        && has("clusteruri")
                        && has("databasename")))) =>
        {
            Some((
                "Eventhouse",
                "EventHouse not provided in params. Creating one first",
            ))
        }
        "DigitalTwinBuilderFlow" if !has("digitaltwinbuilderid") => Some((
            "DigitalTwinBuilder",
            "DigitalTwinBuilder not provided in params. Creating one first",
        )),
        "Report" if !has("semanticmodelid") => Some((
            "SemanticModel",
            "Semantic Model not provided in params. Creating one first",
        )),
        _ => None,
    }
}

fn completed_id(outcome: &ResourceOutcome) -> Option<Value> {
    match outcome {
        ResourceOutcome::Completed {
            status: 200 | 201,
            data: Some(data),
        } => data.get("id").filter(|value| truthy(value)).cloned(),
        _ => None,
    }
}

fn record_item_creation(
    target: &Arc<Element>,
    outcome: &ResourceOutcome,
    evidence: &mut CreationEvidence,
) -> Result<(), MutationError> {
    if let ResourceOutcome::Completed {
        status: 200 | 201,
        data,
    } = outcome
    {
        let Some(data) = data.as_ref().filter(|value| value.is_object()) else {
            return Err(incomplete_create(
                outcome,
                "InvalidResponse",
                "Creation returned invalid resource metadata",
            ));
        };
        evidence.batch_names.push(target.name());
        evidence.batch_items.push(data.clone());
        evidence.effects.push(if completed_id(outcome).is_some() {
            CacheEffect::Created {
                target: Arc::clone(target),
                data: data.clone(),
            }
        } else {
            CacheEffect::Invalidate {
                target: Arc::clone(target),
            }
        });
    }
    Ok(())
}

fn batch_message(names: &[String]) -> String {
    let prefix = if names.len() == 2 {
        format!("'{}' and '{}'", names[0], names[1])
    } else {
        format!(
            "'{}' and '{}'",
            names[..names.len() - 1].join("', '"),
            names[names.len() - 1]
        )
    };
    format!("{prefix} created")
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
        service_error: None,
        raw_service_error: None,
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

fn validate_required_params(
    item_type: &str,
    params: &Map<String, Value>,
) -> Result<(), MutationError> {
    if item_type == "MountedDataFactory" {
        for key in ["subscriptionid", "resourcegroup", "factoryname"] {
            if !params.contains_key(key) || params[key].is_null() {
                return Err(MutationError::new(
                    "InvalidInput",
                    "Missing mandatory params '['subscriptionId', 'resourceGroup', 'factoryName']'. Please provide using -P/--params",
                ));
            }
        }
    }
    Ok(())
}

fn add_item_payload(
    item_type: &str,
    workspace: &str,
    params: &Map<String, Value>,
    dependency: Option<&Value>,
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
            let db_type = text("dbtype");
            if dependency_for(item_type, params).is_some() {
                payload["creationPayload"] = json!({"databaseType":"ReadWrite","parentEventhouseItemId":dependency.cloned().unwrap_or(Value::Null)});
            } else if !params.contains_key("dbtype") || db_type.eq_ignore_ascii_case("readwrite") {
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
            }
        }
        "DigitalTwinBuilderFlow" => {
            payload["creationPayload"] = json!({
                "digitalTwinBuilderItemReference": {
                    "referenceType": "ById",
                    "itemId": params.get("digitaltwinbuilderid").filter(|value|truthy(value)).cloned().or_else(||dependency.cloned()).unwrap_or(Value::Null),
                    "workspaceId": workspace,
                }
            });
        }
        "SemanticModel" => payload["definition"] = crate::templates::semantic_model(),
        "MirroredDatabase"
            if text("mirrortype").is_empty()
                || text("mirrortype").eq_ignore_ascii_case("genericmirror") =>
        {
            payload["definition"] = crate::templates::generic_mirror()
        }
        "Report" | "MirroredDatabase" => {
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
                let value = params
                    .get(key)
                    .and_then(Value::as_str)
                    .ok_or_else(|| unported("MountedDataFactory non-string creation parameters"))?;
                if !value.is_ascii()
                    || value.chars().any(char::is_control)
                    || value.contains(['"', '\\'])
                {
                    return Err(unported(
                        "MountedDataFactory Python-compatible definition serialization",
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
        "SQLDatabase" => {
            let creation = crate::sql_database::creation_payload(params)?;
            if creation.as_object().is_some_and(|value| !value.is_empty()) {
                payload["creationPayload"] = creation;
            }
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

pub(crate) fn truthy(value: &Value) -> bool {
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
