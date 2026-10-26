// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CommandError, CommandValue, list_local_labels};
use fabric_sdk_access::{
    AccessClient, AclMutation, LabelItem, LabelMutation, PrincipalType, Resource,
};
use fabric_sdk_api::{ApiError, Limits, cancellable};
use serde_json::Value;
use std::{
    future::Future,
    path::Path,
    pin::{Pin, pin},
};

/// Caller-owned asynchronous confirmation; no library reads a terminal.
pub trait Confirmation {
    /// Resolves a source question. Failure/cancellation must not mean consent.
    fn confirm<'a>(
        &'a mut self,
        question: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, CommandError>> + Send + 'a>>;
}

/// Resolved source resource plus its display name for progress messages.
#[derive(Clone, Debug)]
pub struct NamedResource {
    /// Resource-specific service route.
    pub resource: Resource,
    /// Source `.name`, including its suffix.
    pub name: String,
}

/// Source mutation flags plus explicit finite traversal/execution limits.
#[derive(Clone, Copy, Debug, Default)]
pub struct MutationOptions {
    /// Bypass source confirmations, not validation or cancellation.
    pub force: bool,
    /// Explicit shared bounds.
    pub limits: Limits,
}

/// An ACL command after source token joining.
#[derive(Clone, Debug)]
pub enum AclChange {
    /// Add/update using the source identity-detection and principal probing order.
    Set {
        /// User-supplied principal selector.
        identity: String,
        /// Role, accepted case-insensitively and sent without canonicalization.
        role: String,
    },
    /// Match UPN, application ID, display name or object ID to an assignment.
    Remove {
        /// Exact case-sensitive source identity selector.
        identity: String,
    },
}

async fn confirm(
    options: MutationOptions,
    prompt: &mut dyn Confirmation,
    question: &str,
    cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
) -> Result<bool, CommandError> {
    if options.force {
        cancellable(std::future::ready(true), cancelled)
            .await
            .map_err(CommandError::from)
    } else {
        cancellable(async { prompt.confirm(question).await }, cancelled)
            .await
            .map_err(CommandError::from)?
    }
}

fn validate_role(resource: &Resource, role: &str) -> Result<(), CommandError> {
    let roles = resource.roles().ok_or_else(|| {
        CommandError::new(
            "UnsupportedCommand",
            "Resource does not support ACL mutations",
        )
    })?;
    if role.is_empty() {
        return Err(CommandError::new("InvalidInput", "Role must be provided"));
    }
    if !roles.iter().any(|r| r.eq_ignore_ascii_case(role)) {
        let kind = match resource {
            Resource::Workspace(_) => "workspace",
            Resource::Connection(_) => "connection",
            _ => "gateway",
        };
        return Err(CommandError::new(
            "InvalidInput",
            format!(
                "Invalid role '{role}' for {kind}. Valid roles are: {}",
                roles.join(", ")
            ),
        ));
    }
    Ok(())
}

/// Runs a complete source ACL write consumer, with explicitly injected prompts.
/// Only a fully received HTTP 400 rejection advances principal probing; transport,
/// auth, deadline, cancellation, 5xx and accepted writes are never hidden/replayed.
pub async fn mutate_acl(
    client: &AccessClient,
    target: &NamedResource,
    change: &AclChange,
    options: MutationOptions,
    prompt: &mut dyn Confirmation,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CommandValue, CommandError> {
    if target.resource.roles().is_none() {
        return Err(CommandError::new(
            "UnsupportedCommand",
            "Resource does not support ACL mutations",
        ));
    }
    if let AclChange::Set { role, .. } = change {
        validate_role(&target.resource, role)?;
    }
    let mut cancelled = pin!(cancelled);
    if !confirm(options, prompt, "Are you sure?", cancelled.as_mut()).await? {
        return Ok(CommandValue::default());
    }
    let listed = client
        .list(&target.resource, options.limits, cancelled.as_mut())
        .await
        .map_err(|e| CommandError::from(e.error))?;
    let mut value = CommandValue::default();
    match change {
        AclChange::Set { identity, role } => {
            if listed.source_text.contains(identity) {
                value.warnings.push(
                    "The provided principal already has a role assigned in the connection".into(),
                );
                if confirm(options, prompt, "Overwrite?", cancelled.as_mut())
                    .await
                    .map_err(|e| e.with_context(&value))?
                {
                    let response = client
                        .mutate_acl(
                            &target.resource,
                            AclMutation::Update { identity, role },
                            options.limits.max_response_bytes,
                            cancelled.as_mut(),
                        )
                        .await
                        .map_err(|e| CommandError::from(e).with_context(&value))?;
                    if response.status == 202 {
                        return Err(CommandError::from(ApiError::UnsupportedStatus(202))
                            .with_context(&value));
                    }
                    if matches!(response.status, 200 | 201) {
                        value.message = Some("ACL updated".into());
                    }
                }
                return Ok(value);
            }
            value
                .diagnostics
                .push(format!("Adding ACL to '{}'...", target.name));
            for principal_type in PrincipalType::SOURCE_ORDER {
                match client
                    .mutate_acl(
                        &target.resource,
                        AclMutation::Add {
                            identity,
                            principal_type,
                            role,
                        },
                        options.limits.max_response_bytes,
                        cancelled.as_mut(),
                    )
                    .await
                {
                    Ok(response) if matches!(response.status, 200 | 201) => {
                        value.message = Some("ACL set".into());
                        return Ok(value);
                    }
                    Ok(response) => {
                        return Err(CommandError::from(ApiError::UnsupportedStatus(
                            response.status,
                        ))
                        .with_context(&value));
                    }
                    Err(ApiError::Http { status: 400, .. }) => {}
                    Err(ApiError::Service { status: 400, .. }) => {}
                    Err(error) => return Err(CommandError::from(error).with_context(&value)),
                }
            }
            Err(
                CommandError::new("NotFound", format!("'{identity}' identity not found"))
                    .with_context(&value),
            )
        }
        AclChange::Remove { identity } => {
            let mut assignment = None;
            for row in &listed.rows {
                let p = row
                    .get("principal")
                    .filter(|p| p.is_object())
                    .ok_or_else(|| {
                        CommandError::new("InvalidEntriesFormat", "Invalid ACL entries format")
                    })?;
                for field in ["userDetails", "servicePrincipalDetails"] {
                    if p.get(field).is_some_and(|v| !v.is_object()) {
                        return Err(CommandError::new(
                            "InvalidEntriesFormat",
                            "Invalid ACL entries format",
                        ));
                    }
                }
                if [
                    p["userDetails"].get("userPrincipalName"),
                    p["servicePrincipalDetails"].get("aadAppId"),
                    p.get("displayName"),
                    p.get("id"),
                ]
                .iter()
                .any(|v| v.and_then(Value::as_str) == Some(identity.as_str()))
                {
                    assignment = Some(row.get("id").and_then(Value::as_str).ok_or_else(|| {
                        CommandError::new("InvalidEntriesFormat", "Invalid ACL entries format")
                    })?);
                    break;
                }
            }
            let assignment = assignment.ok_or_else(|| {
                CommandError::new("NotFound", format!("'{identity}' identity not found"))
            })?;
            value
                .diagnostics
                .push(format!("Deleting ACL from '{}'...", target.name));
            let response = client
                .mutate_acl(
                    &target.resource,
                    AclMutation::Remove { assignment },
                    options.limits.max_response_bytes,
                    cancelled.as_mut(),
                )
                .await
                .map_err(|e| CommandError::from(e).with_context(&value))?;
            if response.status == 202 {
                return Err(
                    CommandError::from(ApiError::UnsupportedStatus(202)).with_context(&value)
                );
            }
            if response.status == 200 {
                value.message = Some("ACL removed".into());
            }
            Ok(value)
        }
    }
}

/// Sensitivity-label command after source token joining.
#[derive(Clone, Debug)]
pub enum LabelChange {
    /// Exact case-sensitive local label name.
    Set(String),
    /// Source still requires a configured definitions path, but does not open it.
    Remove,
}

/// Executes the source label consumer, including config/role restrictions and
/// local-name lookup. No local definition or identity is discovered implicitly.
pub async fn mutate_label(
    client: &AccessClient,
    item: &LabelItem,
    change: &LabelChange,
    definitions: Option<&Path>,
    options: MutationOptions,
    prompt: &mut dyn Confirmation,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CommandValue, CommandError> {
    let kind = fabric_cli_context::ItemType::parse(&item.item_type)
        .map_err(|e| CommandError::new(&e.code, e.message))?;
    if matches!(kind.as_str(), "Warehouse" | "SQLEndpoint") {
        return Err(CommandError::new(
            "UnsupportedCommand",
            "Item type does not support labels",
        ));
    }
    if definitions.is_none_or(|p| p.as_os_str().is_empty()) {
        return list_local_labels(None);
    }
    let mut cancelled = pin!(cancelled);
    if !confirm(options, prompt, "Are you sure?", cancelled.as_mut()).await? {
        return Ok(CommandValue::default());
    }
    let mut value = CommandValue::default();
    let mutation = match change {
        LabelChange::Remove => {
            value.diagnostics.push("Removing label...".into());
            LabelMutation::Remove
        }
        LabelChange::Set(name) => {
            let definition = crate::read_labels_value(
                definitions.expect("configured path checked before confirmation"),
            )?;
            let object = definition.as_object().ok_or_else(|| {
                CommandError::new("InvalidEntriesFormat", "Invalid entries format")
            })?;
            let labels = match object.get("labels") {
                None => &[][..],
                Some(value) => value.as_array().map(Vec::as_slice).ok_or_else(|| {
                    CommandError::new("InvalidEntriesFormat", "Invalid entries format")
                })?,
            };
            let label = labels
                .iter()
                .find(|label| label.get("name").and_then(Value::as_str) == Some(name))
                .and_then(|label| label.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CommandError::new("InvalidInput", format!("Id not found for label '{name}'"))
                })?;
            value.diagnostics.push(format!("Setting '{name}' label..."));
            let response = client
                .mutate_label(
                    item,
                    LabelMutation::Set(label),
                    options.limits.max_response_bytes,
                    cancelled.as_mut(),
                )
                .await
                .map_err(|e| CommandError::from(e).with_context(&value))?;
            if response.status == 202 {
                return Err(
                    CommandError::from(ApiError::UnsupportedStatus(202)).with_context(&value)
                );
            }
            if response.status == 200 {
                value.message = Some("Label set".into());
            }
            return Ok(value);
        }
    };
    let response = client
        .mutate_label(
            item,
            mutation,
            options.limits.max_response_bytes,
            cancelled.as_mut(),
        )
        .await
        .map_err(|e| CommandError::from(e).with_context(&value))?;
    if response.status == 202 {
        return Err(CommandError::from(ApiError::UnsupportedStatus(202)).with_context(&value));
    }
    if response.status == 200 {
        value.message = Some("Label removed".into());
    }
    Ok(value)
}
